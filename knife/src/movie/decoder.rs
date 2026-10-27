//! H.264 decoding with the platform decoder (after shin-video's `h264_decoder`): VideoToolbox on macOS, Media
//! Foundation on Windows, MediaCodec on Android, GStreamer elsewhere. Each runs on its own thread, gets the MP4
//! samples in decode order and hands NV12 frames back in presentation order, converted to RGBA for the movie
//! texture.

use std::{path::Path, sync::mpsc};

use anyhow::{Context, Result, anyhow};
use tracing::debug;

use super::{
    bitstream::Mp4BitstreamConverter,
    mp4::{Mp4, presentation_times, sync_samples},
};

#[cfg(not(any(target_os = "macos", windows, target_os = "android")))]
#[path = "gstreamer.rs"]
mod platform;
#[cfg(target_os = "android")]
#[path = "mediacodec.rs"]
mod platform;
#[cfg(windows)]
#[path = "media_foundation.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "videotoolbox.rs"]
mod platform;

/// A decoded frame: tightly packed luma and interleaved CbCr planes.
pub struct Nv12Frame {
    pub y_plane: Vec<u8>,
    pub uv_plane: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Stream parameters available before the first sample.
pub struct StreamInfo {
    /// MP4 time units per second
    pub timescale: u32,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub width: u32,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub height: u32,
    /// size of the NAL length prefixes in [`Packet::avcc`]
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub length_size: u8,
    /// sequence parameter sets, then picture parameter sets
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub parameter_sets: Vec<Vec<u8>>,
    /// presentation timestamps of the samples that will be decoded, in decode order
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub presentation_times: Vec<i64>,
}

/// One MP4 video sample in both bitstream formats.
pub struct Packet<'a> {
    /// length-prefixed NAL units, as stored in the MP4
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub avcc: &'a [u8],
    /// Annex B with start codes, parameter sets before IDR frames
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub annex_b: &'a [u8],
    #[cfg_attr(any(windows, target_os = "android"), allow(dead_code))]
    pub decode_time: u64,
    pub presentation_time: i64,
    pub duration: u32,
}

/// Where a decoder delivers its frames, in presentation order.
pub struct Output {
    frames: mpsc::SyncSender<Vec<u8>>,
    /// frames still to drop before the start position
    skip: u32,
    closed: bool,
}

impl Output {
    pub fn emit(&mut self, frame: Nv12Frame) {
        if self.closed {
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        if (frame.width, frame.height) != (super::W, super::H) {
            tracing::warn!(
                "movie frame is {}x{}, expected {}x{}",
                frame.width,
                frame.height,
                super::W,
                super::H
            );
            return;
        }
        if self.frames.send(nv12_to_rgba(&frame)).is_err() {
            debug!("movie consumer closed the frame channel");
            self.closed = true;
        }
    }
}

/// A synchronous H.264 decoder, created and used on the worker thread.
pub trait FrameDecoder: Sized {
    const NAME: &'static str;
    fn new(info: &StreamInfo) -> Result<Self>;
    fn decode(&mut self, packet: &Packet, output: &mut Output) -> Result<()>;
    /// Emit every remaining frame after the last packet.
    fn finish(&mut self, output: &mut Output) -> Result<()>;
}

/// Copy a plane of `rows` rows of `width` bytes from rows `stride` bytes apart.
///
/// # Safety
/// `source` must be readable for `stride * (rows - 1) + width` bytes.
pub unsafe fn copy_plane(source: *const u8, stride: usize, width: usize, rows: usize) -> Vec<u8> {
    let mut plane = Vec::with_capacity(width * rows);
    for row in 0..rows {
        // SAFETY: guaranteed by the caller.
        plane.extend_from_slice(unsafe {
            std::slice::from_raw_parts(source.add(row * stride), width)
        });
    }
    plane
}

/// NV12 to RGBA as the Switch shows its movies: BT.601 video range in 8-bit integer coefficients, (298 Y' +
/// 409 Cr, 298 Y' - 100 Cb - 208 Cr, 298 Y' + 516 Cb) / 256 rounded down, whatever the stream says (the
/// movies are tagged BT.709; an Astris capture of the titlebg4 title fits this at 0.78 mean abs, float BT.601
/// rounded at 0.98, BT.709 at 1.5 and more). The chroma plane is sampled bilinearly with centred siting, as a
/// linear texture lookup of the half-size plane would.
pub fn nv12_to_rgba(f: &Nv12Frame) -> Vec<u8> {
    let (w, h) = (f.width as usize, f.height as usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut out = vec![0u8; w * h * 4];
    // per 256, with chroma in sixteenths
    const KY: i32 = 298 * 16;
    const KRV: i32 = 409;
    const KGU: i32 = -100;
    const KGV: i32 = -208;
    const KBU: i32 = 516;
    // the two chroma taps of output coordinate `i` (weights 3 / 1, in quarters)
    let taps = |i: usize, n: usize| -> (usize, usize) {
        let c = i / 2;
        let o = if i.is_multiple_of(2) {
            c.saturating_sub(1)
        } else {
            (c + 1).min(n - 1)
        };
        (c, o)
    };
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8);
    let band = h.div_ceil(threads).next_multiple_of(2);
    std::thread::scope(|s| {
        for (bi, chunk) in out.chunks_mut(band * w * 4).enumerate() {
            s.spawn(move || {
                let y0 = bi * band;
                for (ry, row) in chunk.chunks_mut(w * 4).enumerate() {
                    let y = y0 + ry;
                    let (cy, oy) = taps(y, ch);
                    let (r0, r1) = (
                        &f.uv_plane[cy * w..cy * w + w],
                        &f.uv_plane[oy * w..oy * w + w],
                    );
                    let luma = &f.y_plane[y * w..y * w + w];
                    for x in 0..w {
                        let (cx, ox) = taps(x, cw);
                        // 9 / 3 / 3 / 1 sixteenths
                        let sample = |k: usize| {
                            9 * i32::from(r0[2 * cx + k])
                                + 3 * i32::from(r0[2 * ox + k])
                                + 3 * i32::from(r1[2 * cx + k])
                                + i32::from(r1[2 * ox + k])
                        };
                        // chroma scaled by 16, centred
                        let u = sample(0) - 128 * 16;
                        let v = sample(1) - 128 * 16;
                        let yy = (i32::from(luma[x]) - 16) * KY;
                        let px = &mut row[x * 4..x * 4 + 4];
                        px[0] = ((yy + KRV * v) >> 12).clamp(0, 255) as u8;
                        px[1] = ((yy + KGU * u + KGV * v) >> 12).clamp(0, 255) as u8;
                        px[2] = ((yy + KBU * u) >> 12).clamp(0, 255) as u8;
                        px[3] = 255;
                    }
                }
            });
        }
    });
    out
}

/// Start decoding `path` from `start` seconds on a worker thread with the platform decoder: RGBA frames from
/// that position on, in presentation order. Decoding begins at the last sync sample at or before it.
pub fn spawn(path: &Path, start: f32, fps: f32) -> Result<mpsc::Receiver<Vec<u8>>> {
    let mut mp4 = Mp4::open(path)?;
    let track = &mut mp4.video;
    let converter = track
        .info(Mp4BitstreamConverter::for_mp4_track)
        .context("reading the AVC configuration")?;
    let pts = track.info(presentation_times);
    // presentation rank of every sample
    let mut order: Vec<usize> = (0..pts.len()).collect();
    order.sort_by_key(|&i| pts[i]);
    let mut rank = vec![0u32; pts.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r as u32;
    }
    let target = ((start * fps).round().max(0.0) as u32).min(pts.len().saturating_sub(1) as u32);
    let first = track
        .info(sync_samples)
        .into_iter()
        .filter(|&s| rank.get(s as usize).is_some_and(|&r| r <= target))
        .max()
        .unwrap_or(0);
    track.seek(first);
    let info = StreamInfo {
        timescale: track.info(|t| t.timescale()),
        width: track.info(|t| u32::from(t.width())),
        height: track.info(|t| u32::from(t.height())),
        length_size: converter.length_size(),
        parameter_sets: converter.parameter_sets().map(<[u8]>::to_vec).collect(),
        presentation_times: pts[first as usize..].to_vec(),
    };
    let skip = target - rank[first as usize];
    let (ready_tx, ready) = mpsc::sync_channel::<Result<()>>(1);
    let (frames_tx, frames) = mpsc::sync_channel(6);
    let mut video = mp4.video;
    let mut converter = converter;
    std::thread::Builder::new()
        .name(format!("movie decoder ({})", platform::Decoder::NAME))
        .spawn(move || {
            let mut decoder = match platform::Decoder::new(&info) {
                Ok(d) => {
                    let _ = ready_tx.send(Ok(()));
                    d
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let mut output = Output {
                frames: frames_tx,
                skip,
                closed: false,
            };
            let mut annex_b = Vec::new();
            let result = (|| -> Result<()> {
                while let Some(sample) = video.next_sample()? {
                    converter.convert_packet(&sample.bytes, &mut annex_b)?;
                    let packet = Packet {
                        avcc: &sample.bytes,
                        annex_b: &annex_b,
                        decode_time: sample.start_time,
                        presentation_time: sample.start_time as i64
                            + i64::from(sample.rendering_offset),
                        duration: sample.duration,
                    };
                    decoder.decode(&packet, &mut output)?;
                    if output.closed {
                        return Ok(());
                    }
                }
                decoder.finish(&mut output)
            })();
            if let Err(e) = result {
                tracing::warn!("{}: {e:#}", platform::Decoder::NAME);
            }
        })
        .context("starting the movie decoder thread")?;
    ready
        .recv()
        .map_err(|_| anyhow!("{} failed to start", platform::Decoder::NAME))??;
    Ok(frames)
}
