//! H.264 decoding with the platform decoder (after shin-video's `h264_decoder`): VideoToolbox on macOS, Media
//! Foundation on Windows, GStreamer elsewhere. Each runs on its own thread, gets the MP4 samples in decode order
//! and hands NV12 frames back in presentation order, converted to RGBA for the movie texture.

use std::{path::Path, sync::mpsc};

use anyhow::{Context, Result, anyhow};
use tracing::debug;

use super::{
    bitstream::Mp4BitstreamConverter,
    mp4::{Mp4, TrackReader, presentation_times, sync_samples},
};

#[cfg(not(any(target_os = "macos", windows)))]
#[path = "gstreamer.rs"]
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
    #[cfg_attr(windows, allow(dead_code))]
    pub decode_time: u64,
    pub presentation_time: i64,
    pub duration: u32,
}

/// RGBA frame buffers the player hands back once uploaded, for the decoder to convert the next frames into: a
/// 1920x1080 frame is 8 MB, which a fresh allocation would fault in page by page 30 times a second.
#[derive(Clone, Default)]
pub struct FramePool(std::sync::Arc<parking_lot::Mutex<Vec<Vec<u8>>>>);

impl FramePool {
    /// Buffers kept: the frame channel's 6 and a couple in flight
    const KEEP: usize = 8;

    pub fn take(&self) -> Vec<u8> {
        self.0.lock().pop().unwrap_or_default()
    }

    pub fn give(&self, buffer: Vec<u8>) {
        let mut pool = self.0.lock();
        if pool.len() < Self::KEEP {
            pool.push(buffer);
        }
    }
}

/// Where a decoder delivers its frames, in presentation order.
pub struct Output {
    frames: mpsc::SyncSender<Vec<u8>>,
    pool: FramePool,
    /// the planes of the frame emitted last, for the decoder to copy the next one into (`spare_planes`)
    spare: Option<Nv12Frame>,
    /// frames still to drop before the start position
    skip: u32,
    /// frames delivered past the start position (a software decoder taking over skips them again)
    delivered: u32,
    closed: bool,
}

impl Output {
    /// The luma and chroma buffers of the frame emitted last (empty before the first), to fill with the next.
    pub fn spare_planes(&mut self) -> (Vec<u8>, Vec<u8>) {
        self.spare.take().map(|f| (f.y_plane, f.uv_plane)).unwrap_or_default()
    }

    pub fn emit(&mut self, frame: Nv12Frame) {
        if self.closed {
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            self.spare = Some(frame);
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
        self.delivered += 1;
        let mut rgba = self.pool.take();
        nv12_to_rgba_into(&frame, &mut rgba);
        self.spare = Some(frame);
        if self.frames.send(rgba).is_err() {
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
    /// The decoder runs on the GPU and has a software one (`software`) to fall back to when it fails.
    fn hardware(&self) -> bool {
        false
    }
    /// A decoder that does not use the GPU.
    fn software(info: &StreamInfo) -> Result<Self> {
        Self::new(info)
    }
}

/// Decode `video` from where it stands to its end into `output`.
fn run<D: FrameDecoder>(
    decoder: &mut D,
    video: &mut TrackReader,
    converter: &mut Mp4BitstreamConverter,
    output: &mut Output,
) -> Result<()> {
    let mut annex_b = Vec::new();
    while let Some(sample) = video.next_sample()? {
        converter.convert_packet(&sample.bytes, &mut annex_b)?;
        let packet = Packet {
            avcc: &sample.bytes,
            annex_b: &annex_b,
            decode_time: sample.start_time,
            presentation_time: sample.start_time as i64 + i64::from(sample.rendering_offset),
            duration: sample.duration,
        };
        decoder.decode(&packet, output)?;
        if output.closed {
            return Ok(());
        }
    }
    decoder.finish(output)
}

/// Copy a plane of `rows` rows of `width` bytes from rows `stride` bytes apart into `plane`, replacing its
/// contents and keeping its allocation (the buffers of an earlier frame, `Output::spare_planes`).
///
/// # Safety
/// `source` must be readable for `stride * (rows - 1) + width` bytes.
pub unsafe fn copy_plane(plane: &mut Vec<u8>, source: *const u8, stride: usize, width: usize, rows: usize) {
    plane.clear();
    plane.reserve(width * rows);
    for row in 0..rows {
        // SAFETY: guaranteed by the caller.
        plane.extend_from_slice(unsafe {
            std::slice::from_raw_parts(source.add(row * stride), width)
        });
    }
}

/// NV12 to RGBA as the Switch shows its movies: BT.601 video range in 8-bit integer coefficients, (298 Y' +
/// 409 Cr, 298 Y' - 100 Cb - 208 Cr, 298 Y' + 516 Cb) / 256 rounded down, whatever the stream says (the
/// movies are tagged BT.709; an Astris capture of the titlebg4 title fits this at 0.78 mean abs, float BT.601
/// rounded at 0.98, BT.709 at 1.5 and more). The chroma plane is sampled bilinearly with centred siting, as a
/// linear texture lookup of the half-size plane would.
///
/// The RGBA goes into `out`, resized to fit: a buffer of an earlier frame is reused without being faulted in
/// again page by page.
pub fn nv12_to_rgba_into(f: &Nv12Frame, out: &mut Vec<u8>) {
    // bands of rows on up to 8 threads
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let (w, h) = (f.width as usize, f.height as usize);
    let ch = h / 2;
    assert!(w >= 2 && w.is_multiple_of(2) && h.is_multiple_of(2), "odd movie frame size {w}x{h}");
    out.resize(w * h * 4, 0);
    // per 256, with chroma in sixteenths
    const KY: i32 = 298 * 16;
    const KRV: i32 = 409;
    const KGU: i32 = -100;
    const KGV: i32 = -208;
    const KBU: i32 = 516;
    let band = h.div_ceil(threads).next_multiple_of(2);
    std::thread::scope(|s| {
        for (bi, chunk) in out.chunks_mut(band * w * 4).enumerate() {
            s.spawn(move || {
                // The bilinear tap's 9 / 3 / 3 / 1 sixteenths, taken in two passes: the chroma rows blended
                // vertically, 3 * nearer + farther (Cb, Cr interleaved, with a column of padding each side that
                // repeats the edge column), then 3 * nearer + farther column of that. An even output coordinate's
                // farther row or column is the one before, an odd one's the one after, clamped to the plane.
                let mut blend = vec![0i32; w + 4];
                // the chroma terms of each output pixel's red, green and blue
                let (mut rc, mut gc, mut bc) = (vec![0i32; w], vec![0i32; w], vec![0i32; w]);
                let y0 = bi * band;
                for (ry, row) in chunk.chunks_exact_mut(w * 4).enumerate() {
                    let y = y0 + ry;
                    let cy = y / 2;
                    let oy = if y.is_multiple_of(2) { cy.saturating_sub(1) } else { (cy + 1).min(ch - 1) };
                    let r0 = &f.uv_plane[cy * w..cy * w + w];
                    let r1 = &f.uv_plane[oy * w..oy * w + w];
                    for ((b, &n), &o) in blend[2..w + 2].iter_mut().zip(r0).zip(r1) {
                        *b = 3 * i32::from(n) + i32::from(o);
                    }
                    blend.copy_within(2..4, 0);
                    blend.copy_within(w..w + 2, w + 2);
                    let columns = blend[0..w]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .zip(blend[2..w + 2].as_chunks::<2>().0)
                        .zip(blend[4..w + 4].as_chunks::<2>().0);
                    let terms = rc.as_chunks_mut::<2>().0.iter_mut()
                        .zip(gc.as_chunks_mut::<2>().0)
                        .zip(bc.as_chunks_mut::<2>().0);
                    for (((before, here), after), ((r, g), b)) in columns.zip(terms) {
                        // chroma scaled by 16, centred; [0] the even pixel's, [1] the odd one's
                        let u = [3 * here[0] + before[0] - 128 * 16, 3 * here[0] + after[0] - 128 * 16];
                        let v = [3 * here[1] + before[1] - 128 * 16, 3 * here[1] + after[1] - 128 * 16];
                        for i in 0..2 {
                            r[i] = KRV * v[i];
                            g[i] = KGU * u[i] + KGV * v[i];
                            b[i] = KBU * u[i];
                        }
                    }
                    let luma = &f.y_plane[y * w..y * w + w];
                    let pixels = row.as_chunks_mut::<4>().0.iter_mut().zip(luma).zip(rc.iter().zip(&gc).zip(&bc));
                    for ((px, &l), ((&r, &g), &b)) in pixels {
                        let yy = (i32::from(l) - 16) * KY;
                        let channel = |c: i32| ((yy + c) >> 12).clamp(0, 255) as u32;
                        // one store per pixel, which LLVM vectorizes (four byte stores it does not)
                        let rgba = channel(r) | channel(g) << 8 | channel(b) << 16 | 0xFF00_0000;
                        *px = rgba.to_le_bytes();
                    }
                }
            });
        }
    });
}

#[cfg(test)]
fn nv12_to_rgba_reference(f: &Nv12Frame) -> Vec<u8> {
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
pub fn spawn(path: &Path, start: f32, fps: f32, pool: FramePool) -> Result<mpsc::Receiver<Vec<u8>>> {
    spawn_with(path, start, fps, pool, platform::Decoder::new)
}

/// `spawn` with the decoder `create` makes.
fn spawn_with(
    path: &Path,
    start: f32,
    fps: f32,
    pool: FramePool,
    create: fn(&StreamInfo) -> Result<platform::Decoder>,
) -> Result<mpsc::Receiver<Vec<u8>>> {
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
            let mut decoder = match create(&info) {
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
                pool,
                spare: None,
                skip,
                delivered: 0,
                closed: false,
            };
            let mut result = run(&mut decoder, &mut video, &mut converter, &mut output);
            // a GPU decoder that fails hands over to the software one, which decodes again from the same sync
            // sample and passes over the frames already delivered
            if let Err(e) = &result
                && decoder.hardware()
            {
                tracing::warn!("{} hardware decoding failed, decoding in software: {e:#}", platform::Decoder::NAME);
                // the failed decoder's device and surfaces go before the software one starts
                drop(decoder);
                result = platform::Decoder::software(&info).and_then(|mut software| {
                    video.seek(first);
                    output.skip = skip + output.delivered;
                    output.delivered = 0;
                    run(&mut software, &mut video, &mut converter, &mut output)
                });
            }
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

#[cfg(test)]
mod tests {
    use super::{FrameDecoder, FramePool, platform};

    /// The hardware decoder's frames are the software decoder's, from the start and after a seek (needs
    /// switch_romfs, or a movie in KNIFE_TEST_MOVIE).
    #[test]
    fn hardware_frames_match_software() {
        let path = std::env::var_os("KNIFE_TEST_MOVIE").map_or_else(
            || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../switch_romfs/movie/titlebg1.mp4"),
            std::path::PathBuf::from,
        );
        if !path.exists() {
            return;
        }
        let fps = 30.0;
        for start in [0.0, 30.0] {
            let frames = |create, label| {
                let rx = super::spawn_with(&path, start, fps, FramePool::default(), create).unwrap();
                let t = std::time::Instant::now();
                let frames: Vec<Vec<u8>> = rx.iter().take(90).collect();
                eprintln!("start {start}: {label} {} frames in {:.2} s", frames.len(), t.elapsed().as_secs_f64());
                frames
            };
            let hardware = frames(
                |info| {
                    let decoder = platform::Decoder::new(info)?;
                    eprintln!("default decoder on the GPU: {}", decoder.hardware());
                    Ok(decoder)
                },
                "default",
            );
            let software = frames(platform::Decoder::software, "software");
            assert_eq!(hardware.len(), software.len());
            for (i, (h, s)) in hardware.iter().zip(&software).enumerate() {
                assert!(h == s, "start {start}: frame {i} differs");
            }
        }
    }

    /// The row-blended conversion gives the per-pixel bilinear tap's bytes exactly.
    #[test]
    fn nv12_to_rgba_matches_reference() {
        let (w, h) = (1920usize, 1080usize);
        // a fixed pseudo-random frame, so every channel and clamp is exercised
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        };
        let y_plane = (0..w * h).map(|_| next()).collect();
        let uv_plane = (0..w * h / 2).map(|_| next()).collect();
        let frame = super::Nv12Frame { y_plane, uv_plane, width: w as u32, height: h as u32 };
        let mut out = Vec::new();
        super::nv12_to_rgba_into(&frame, &mut out);
        assert!(out == super::nv12_to_rgba_reference(&frame));
    }
}
