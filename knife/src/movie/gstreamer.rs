//! Decodes H.264 with Decoder, using whichever H.264 decoder plugin the
//! system provides (libav, OpenH264, VA-API, ...).

use std::sync::Once;

use anyhow::{Context, Result, anyhow, bail, ensure};
use gst_video::prelude::*;
use tracing::{debug, warn};

use super::{FrameDecoder, Nv12Frame, Output, Packet, StreamInfo, copy_plane};

pub struct Decoder {
    pipeline: gst::Pipeline,
    source: gst_app::AppSrc,
    sink: gst_app::AppSink,
    timescale: u64,
}

fn init() -> Result<()> {
    static INIT: Once = Once::new();
    let mut result = Ok(());
    INIT.call_once(|| result = gst::init());
    result.context("Initializing Decoder")
}

/// Whether any installed plugin can decode H.264.
fn h264_decoder_available() -> bool {
    let caps = gst::Caps::builder("video/x-h264").build();
    gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO,
        gst::Rank::MARGINAL,
    )
    .iter()
    .any(|factory| factory.can_sink_any_caps(&caps))
}

impl Decoder {
    fn time(&self, value: i64) -> gst::ClockTime {
        let value = u64::try_from(value).unwrap_or(0);
        gst::ClockTime::from_nseconds(value * 1_000_000_000 / self.timescale)
    }

    /// Fail with the first error the pipeline reported.
    fn check_bus(&self) -> Result<()> {
        let Some(bus) = self.pipeline.bus() else {
            return Ok(());
        };
        while let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
            if let gst::MessageView::Error(error) = message.view() {
                bail!(
                    "Decoder error from {}: {} ({:?})",
                    error
                        .src()
                        .map(|source| source.path_string().to_string())
                        .unwrap_or_default(),
                    error.error(),
                    error.debug()
                );
            }
        }
        Ok(())
    }

    fn emit(&self, sample: gst::Sample, output: &mut Output) -> Result<()> {
        let caps = sample.caps().context("Decoded frame has no caps")?;
        let info = gst_video::VideoInfo::from_caps(caps).context("Reading decoded frame format")?;
        ensure!(
            info.format() == gst_video::VideoFormat::Nv12,
            "Decoder returned {:?} instead of NV12",
            info.format()
        );
        let buffer = sample.buffer().context("Decoded frame has no buffer")?;
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .map_err(|_| anyhow!("Mapping a decoded frame"))?;
        let (width, height) = (info.width() as usize, info.height() as usize);
        let mut planes = [0, 1].into_iter().map(|plane| -> Result<Vec<u8>> {
            let data = frame.plane_data(plane).context("Reading a decoded plane")?;
            let stride = usize::try_from(frame.info().stride()[plane as usize])
                .context("Bottom-up Decoder frames are not supported")?;
            let rows = if plane == 0 {
                height
            } else {
                height.div_ceil(2)
            };
            ensure!(
                stride >= width && data.len() >= stride * (rows - 1) + width,
                "Decoded plane is too small"
            );
            // SAFETY: checked above that the plane holds `rows` rows of `width` bytes.
            Ok(unsafe { copy_plane(data.as_ptr(), stride, width, rows) })
        });
        let y_plane = planes.next().unwrap()?;
        let uv_plane = planes.next().unwrap()?;
        output.emit(Nv12Frame {
            y_plane,
            uv_plane,
            width: width as u32,
            height: height as u32,
        });
        Ok(())
    }
}

impl FrameDecoder for Decoder {
    const NAME: &'static str = "GStreamer";

    fn new(info: &StreamInfo) -> Result<Self> {
        init()?;
        ensure!(
            h264_decoder_available(),
            "No Decoder H.264 decoder is installed (for example gst-libav or gst-plugins-bad's openh264)"
        );
        let (major, minor, micro, _) = gst::version();
        debug!("Decoding movie with Decoder {major}.{minor}.{micro}");

        let source = gst_app::AppSrc::builder()
            .caps(
                &gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            )
            .format(gst::Format::Time)
            .build();
        let decode = gst::ElementFactory::make("decodebin")
            .build()
            .context("Creating decodebin")?;
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .context("Creating videoconvert")?;
        let sink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "NV12")
                    .build(),
            )
            .sync(false)
            .build();
        let pipeline = gst::Pipeline::default();
        pipeline
            .add_many([source.upcast_ref(), &decode, &convert, sink.upcast_ref()])
            .context("Building the Decoder pipeline")?;
        source
            .link(&decode)
            .context("Linking the Decoder decoder")?;
        convert
            .link(&sink)
            .context("Linking the Decoder frame output")?;
        // decodebin creates its output pad once it knows the stream.
        let convert_weak = convert.downgrade();
        decode.connect_pad_added(move |_, pad| {
            let Some(convert) = convert_weak.upgrade() else {
                return;
            };
            let sink_pad = convert
                .static_pad("sink")
                .expect("videoconvert has a sink pad");
            if !sink_pad.is_linked()
                && let Err(error) = pad.link(&sink_pad)
            {
                warn!("Linking the Decoder decoder output: {error:?}");
            }
        });
        pipeline
            .set_state(gst::State::Playing)
            .context("Starting the Decoder pipeline")?;
        Ok(Self {
            pipeline,
            source,
            sink,
            timescale: u64::from(info.timescale).max(1),
        })
    }

    fn decode(&mut self, packet: &Packet, output: &mut Output) -> Result<()> {
        let mut buffer = gst::Buffer::from_slice(packet.annex_b.to_vec());
        {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts(self.time(packet.presentation_time));
            buffer.set_dts(self.time(packet.decode_time as i64));
            buffer.set_duration(self.time(i64::from(packet.duration)));
        }
        if let Err(flow) = self.source.push_buffer(buffer) {
            self.check_bus()?;
            bail!("Decoder refused a movie frame: {flow:?}");
        }
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            self.emit(sample, output)?;
        }
        self.check_bus()
    }

    fn finish(&mut self, output: &mut Output) -> Result<()> {
        self.source
            .end_of_stream()
            .map_err(|flow| anyhow!("Ending the Decoder stream: {flow:?}"))?;
        // Blocks until the next frame, or returns nothing at end of stream or on error.
        while let Ok(sample) = self.sink.pull_sample() {
            self.emit(sample, output)?;
        }
        self.check_bus()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
