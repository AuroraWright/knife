//! Decodes H.264 with Android's MediaCodec (through the NDK's AMediaCodec), which uses the phone's hardware
//! decoder where there is one.
//!
//! MediaCodec returns its frames in presentation order, so unlike VideoToolbox's they need no reordering here.
//! The decoder is asked for NV12 (YUV420SemiPlanar) in ordinary memory; what it really returns (the layout of
//! its buffers varies with the decoder: a row stride and a slice height past the picture's own) is read from the
//! output format it announces.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use ndk::media::{
    media_codec::{DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection},
    media_format::MediaFormat,
};

use super::{FrameDecoder, Nv12Frame, Output, Packet, StreamInfo};

/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Planar`: I420
const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
/// `COLOR_FormatYUV420SemiPlanar`: NV12
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
/// `AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM`
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// How long to wait for the codec at a time
const WAIT: Duration = Duration::from_millis(10);
/// How many `WAIT`s to wait for an input buffer, or for the last frames, before giving up on the codec
const PATIENCE: u32 = 500;

/// The layout of the codec's output buffers.
#[derive(Clone, Copy)]
struct Layout {
    /// bytes from a row to the next
    stride: usize,
    /// rows of the luma plane, with the padding the codec adds below the picture
    slice_height: usize,
    /// I420 (separate U and V planes) rather than NV12
    planar: bool,
}

pub struct Decoder {
    codec: MediaCodec,
    width: usize,
    height: usize,
    layout: Layout,
    timescale: u64,
}

/// `annex_b` parameter sets of `kind` (NAL unit type 7: sequence, 8: picture), each behind a start code: the
/// codec-specific data MediaCodec wants in its format.
fn parameter_sets(info: &StreamInfo, kind: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for set in info.parameter_sets.iter().filter(|set| set.first().is_some_and(|b| b & 0x1f == kind)) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(set);
    }
    out
}

impl Decoder {
    /// The buffers' layout as the codec says it (only once it has announced its output format), else the
    /// picture's own size in NV12.
    fn read_layout(&self) -> Layout {
        let format = self.codec.output_format();
        let size = |key: &str, least: usize| {
            format.i32(key).and_then(|v| usize::try_from(v).ok()).filter(|&v| v >= least)
        };
        let color = format.i32("color-format").unwrap_or(COLOR_FORMAT_YUV420_SEMI_PLANAR);
        if color != COLOR_FORMAT_YUV420_SEMI_PLANAR && color != COLOR_FORMAT_YUV420_PLANAR {
            tracing::warn!("MediaCodec returns color format {color:#x}, taken for NV12");
        }
        Layout {
            stride: size("stride", self.width).unwrap_or(self.width),
            slice_height: size("slice-height", self.height).unwrap_or(self.height),
            planar: color == COLOR_FORMAT_YUV420_PLANAR,
        }
    }

    /// Tightly packed NV12 planes from an output buffer's bytes (the picture starts at `data`'s first byte).
    fn frame(&self, data: &[u8]) -> Result<Nv12Frame> {
        let Layout { stride, slice_height, planar } = self.layout;
        let (w, h) = (self.width, self.height);
        let chroma_rows = h.div_ceil(2);
        let luma = stride * slice_height;
        let mut y_plane = Vec::with_capacity(w * h);
        let mut uv_plane = Vec::with_capacity(w * chroma_rows);
        if planar {
            // I420: U, then V, each a half-width plane (rounded up) of half the rows
            let (cs, cw) = (stride / 2, w.div_ceil(2));
            let u = luma;
            let v = luma + cs * (slice_height / 2);
            ensure!(data.len() >= v + cs * (chroma_rows - 1) + cw, "MediaCodec returned a short frame");
            for row in 0..chroma_rows {
                for x in 0..cw {
                    uv_plane.push(data[u + row * cs + x]);
                    uv_plane.push(data[v + row * cs + x]);
                }
            }
        } else {
            ensure!(data.len() >= luma + stride * (chroma_rows - 1) + w, "MediaCodec returned a short frame");
            for row in 0..chroma_rows {
                uv_plane.extend_from_slice(&data[luma + row * stride..][..w]);
            }
        }
        ensure!(data.len() >= stride * (h - 1) + w, "MediaCodec returned a short frame");
        for row in 0..h {
            y_plane.extend_from_slice(&data[row * stride..][..w]);
        }
        Ok(Nv12Frame { y_plane, uv_plane, width: w as u32, height: h as u32 })
    }

    /// Hand over the frames the codec has ready, waiting up to `timeout` for the next one. True once the
    /// end-of-stream marker has come through.
    fn drain(&mut self, output: &mut Output, timeout: Duration) -> Result<bool> {
        loop {
            match self
                .codec
                .dequeue_output_buffer(timeout)
                .map_err(|e| anyhow!("MediaCodec output: {e}"))?
            {
                DequeuedOutputBufferInfoResult::Buffer(buffer) => {
                    let info = *buffer.info();
                    let end = info.flags() & BUFFER_FLAG_END_OF_STREAM != 0;
                    if info.size() > 0 {
                        let offset = usize::try_from(info.offset()).unwrap_or(0);
                        let frame = buffer
                            .buffer()
                            .get(offset..)
                            .context("MediaCodec returned a frame past its buffer")
                            .and_then(|data| self.frame(data));
                        // the buffer goes back to the codec either way
                        self.codec
                            .release_output_buffer(buffer, false)
                            .map_err(|e| anyhow!("MediaCodec release: {e}"))?;
                        output.emit(frame?);
                    } else {
                        self.codec
                            .release_output_buffer(buffer, false)
                            .map_err(|e| anyhow!("MediaCodec release: {e}"))?;
                    }
                    if end {
                        return Ok(true);
                    }
                }
                DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(false),
                DequeuedOutputBufferInfoResult::OutputFormatChanged => self.layout = self.read_layout(),
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
            }
        }
    }

    /// Give the codec `data` as one input buffer; when it has none free, take out the frames it is holding.
    fn queue(&mut self, data: &[u8], time_us: u64, flags: u32, output: &mut Output) -> Result<()> {
        for _ in 0..PATIENCE {
            match self
                .codec
                .dequeue_input_buffer(WAIT)
                .map_err(|e| anyhow!("MediaCodec input: {e}"))?
            {
                DequeuedInputBufferResult::Buffer(mut buffer) => {
                    let room = buffer.buffer_mut();
                    ensure!(data.len() <= room.len(), "a movie frame does not fit the codec's input buffer");
                    for (to, from) in room.iter_mut().zip(data) {
                        to.write(*from);
                    }
                    return self
                        .codec
                        .queue_input_buffer(buffer, 0, data.len(), time_us, flags)
                        .map_err(|e| anyhow!("MediaCodec queue: {e}"));
                }
                DequeuedInputBufferResult::TryAgainLater => {
                    self.drain(output, Duration::ZERO)?;
                    if output.closed {
                        return Ok(());
                    }
                }
            }
        }
        bail!("MediaCodec took no more input")
    }
}

impl FrameDecoder for Decoder {
    const NAME: &'static str = "MediaCodec";

    fn new(info: &StreamInfo) -> Result<Self> {
        let codec = MediaCodec::from_decoder_type("video/avc").context("this device has no H.264 decoder")?;
        let mut format = MediaFormat::new();
        format.set_str("mime", "video/avc");
        format.set_i32("width", info.width as i32);
        format.set_i32("height", info.height as i32);
        format.set_i32("color-format", COLOR_FORMAT_YUV420_SEMI_PLANAR);
        let (sps, pps) = (parameter_sets(info, 7), parameter_sets(info, 8));
        if !sps.is_empty() {
            format.set_buffer("csd-0", &sps);
        }
        if !pps.is_empty() {
            format.set_buffer("csd-1", &pps);
        }
        codec
            .configure(&format, None, MediaCodecDirection::Decoder)
            .map_err(|e| anyhow!("configuring MediaCodec: {e}"))?;
        codec.start().map_err(|e| anyhow!("starting MediaCodec: {e}"))?;
        let (width, height) = (info.width as usize, info.height as usize);
        // (the codec announces its real layout before its first frame, `drain`)
        Ok(Self {
            codec,
            width,
            height,
            layout: Layout { stride: width, slice_height: height, planar: false },
            timescale: u64::from(info.timescale).max(1),
        })
    }

    fn decode(&mut self, packet: &Packet, output: &mut Output) -> Result<()> {
        let time_us = u64::try_from(packet.presentation_time).unwrap_or(0) * 1_000_000 / self.timescale;
        self.queue(packet.annex_b, time_us, 0, output)?;
        self.drain(output, Duration::ZERO)?;
        Ok(())
    }

    fn finish(&mut self, output: &mut Output) -> Result<()> {
        self.queue(&[], 0, BUFFER_FLAG_END_OF_STREAM, output)?;
        for _ in 0..PATIENCE {
            if output.closed || self.drain(output, WAIT)? {
                return Ok(());
            }
        }
        bail!("MediaCodec did not end the stream")
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}
