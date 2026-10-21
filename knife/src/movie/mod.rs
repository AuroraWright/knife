//! Movie playback: the MP4 is demuxed here and its H.264 video decoded by the platform (VideoToolbox, Media
//! Foundation or GStreamer, see `decoder`), its AAC audio by symphonia. An `ffmpeg` executable is the fallback
//! when the platform decoder is unavailable.

mod audio;
mod bitstream;
mod decoder;
mod mp4;

use std::{
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
};

use anyhow::{Context, Result};
use knife_audio::Pcm;
use knife_render::{Renderer, TexRef};

pub const MOVIES: &[&str] = &[
    "titlebg1",
    "titlebg2",
    "titlebg3",
    "titlebg4",
    "prologue",
    "op1_arc",
    "op2_ciel",
    "phaseout",
    "phasetitle_01_00",
    "phasetitle_02_00",
    "phasetitle_03_00",
    "phasetitle_04_00",
    "phasetitle_05_00",
    "phasetitle_05_01",
    "phasetitle_06_00",
    "phasetitle_06_01",
    "phasetitle_07_00",
    "phasetitle_07_01",
    "phasetitle_08_00",
    "phasetitle_08_01",
    "phasetitle_09_00",
    "phasetitle_09_01",
    "phasetitle_10_00",
    "phasetitle_10_01",
    "phasetitle_10_02",
    "phasetitle_11_00",
    "phasetitle_11_01",
    "phasetitle_12_00",
    "phasetitle_12_01",
    "phasetitle_13_00",
    "phasetitle_13_01",
    "phasetitle_14_00",
    "phasetitle_14_01",
    "phasetitle_15_01a",
    "phasetitle_15_01b",
    "ed01_arc",
    "ed02_ciel",
    "ed03_ciel",
    "TM_CI",
    "credit00",
    "credit01",
    "credit02",
    "credit03",
    "credit04",
    "credit05",
    "credit06",
];

pub(crate) const W: u32 = 1920;
pub(crate) const H: u32 = 1080;

pub struct Movie {
    child: Option<Child>,
    rx: mpsc::Receiver<Vec<u8>>,
    pub tex: TexRef,
    fps: f32,
    elapsed: f32,
    shown: u64,
    pub finished: bool,
    pub looping: bool,
    /// paused: the clock stops and the decoder waits (the title movie under a submenu)
    pub hold: bool,
    has_frame: bool,
    path: std::path::PathBuf,
    /// sprite alpha (off_AE01D0 +640): sub_1EC0 raises it by 0.05 a frame from 0 while the movie plays, and
    /// once playback stops lowers it by 1/30 a frame
    pub alpha: f32,
    /// the fade-in runs (off_AE0228; sub_1A80 clears it)
    fading_in: bool,
    /// updates since the fade-out reached 0; the player closes 3 later (sub_1EC0 states 1..3)
    close_steps: u8,
    /// the player has closed (off_AE0218 == 0): sub_19E0 no longer reports it playing
    pub closed: bool,
    /// a looping movie restarts here, in seconds (sub_1C20; the title movies loop from 240 / 30 s)
    pub loop_start: f32,
    /// sub_1A30 countdown (dword_AE582C): 4 while the player is not running
    ready_wait: u8,
}

/// Frame rate from the video track's sample tables (30 when unknown).
fn track_fps(path: &Path) -> f32 {
    let fps = mp4::Mp4::open(path)
        .map(|m| m.video.info(|t| t.frame_rate()))
        .unwrap_or(0.0);
    if fps > 0.0 { fps as f32 } else { 30.0 }
}

/// RGBA frames from `start` seconds on: the platform decoder, else an ffmpeg process.
fn spawn_video(
    path: &Path,
    start: f32,
    fps: f32,
) -> Result<(Option<Child>, mpsc::Receiver<Vec<u8>>)> {
    if !cfg!(feature = "ffmpeg") {
        match decoder::spawn(path, start, fps) {
            Ok(rx) => return Ok((None, rx)),
            Err(e) => tracing::warn!("platform movie decoder unavailable, trying ffmpeg: {e:#}"),
        }
    }
    let (child, rx) = spawn_ffmpeg(path, start)?;
    Ok((Some(child), rx))
}

fn spawn_ffmpeg(path: &Path, start: f32) -> Result<(Child, mpsc::Receiver<Vec<u8>>)> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &format!("{start:.3}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{W}x{H}"),
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting ffmpeg (is it installed?)")?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(6);
    std::thread::spawn(move || {
        let size = (W * H * 4) as usize;
        loop {
            let mut buf = vec![0u8; size];
            if stdout.read_exact(&mut buf).is_err() {
                break;
            }
            if tx.send(buf).is_err() {
                break;
            }
        }
    });
    Ok((child, rx))
}

/// Decode a movie's audio track (None when it has none).
pub fn decode_audio(path: &Path) -> Option<Pcm> {
    let track = mp4::Mp4::open(path)
        .map_err(|e| tracing::warn!("{}: {e:#}", path.display()))
        .ok()?
        .audio?;
    let (sample_rate, frames) = audio::decode(track)
        .map_err(|e| tracing::warn!("{}: {e:#}", path.display()))
        .ok()?;
    (!frames.is_empty()).then_some(Pcm {
        sample_rate,
        frames,
        loop_range: None,
    })
}

impl Movie {
    pub fn open(r: &Renderer, path: &Path, looping: bool) -> Result<Self> {
        let fps = track_fps(path);
        let (child, rx) = spawn_video(path, 0.0, fps)?;
        Ok(Self {
            child,
            rx,
            tex: r.create_writable(W, H, false, "movie"),
            fps,
            elapsed: 0.0,
            shown: 0,
            finished: false,
            looping,
            hold: false,
            has_frame: false,
            path: path.to_path_buf(),
            alpha: 0.0,
            fading_in: true,
            close_steps: 0,
            closed: false,
            loop_start: 0.0,
            ready_wait: 4,
        })
    }

    /// Playback position in seconds (sub_1AE0).
    pub fn position(&self) -> f32 {
        self.elapsed / 1000.0
    }

    /// sub_1A30: the player has been running for 4 updates.
    pub fn ready(&self) -> bool {
        self.ready_wait == 0 && !self.finished
    }

    pub fn has_frame(&self) -> bool {
        self.has_frame
    }

    pub fn update(&mut self, r: &Renderer, dt: f32) {
        let frames = dt * 0.06;
        if self.finished {
            if self.alpha > 0.0 {
                self.alpha = (self.alpha - frames / 30.0).max(0.0);
            } else if !self.closed {
                self.close_steps += 1;
                self.closed = self.close_steps >= 3;
            }
            return;
        }
        if self.hold {
            return;
        }
        self.ready_wait = if self.has_frame {
            self.ready_wait.saturating_sub(1)
        } else {
            4
        };
        if self.has_frame && self.fading_in {
            self.alpha = (self.alpha + 0.05 * frames).min(1.0);
        }
        self.elapsed += dt;
        let due = (self.elapsed / 1000.0 * self.fps) as u64 + 1;
        let mut latest = None;
        while self.shown < due {
            match self.rx.try_recv() {
                Ok(f) => {
                    latest = Some(f);
                    self.shown += 1;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // decoder is behind; don't let the clock run away
                    if self.shown + 2 < due {
                        self.elapsed = self.shown as f32 * 1000.0 / self.fps;
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.looping {
                        if let Ok((child, rx)) = spawn_video(&self.path, self.loop_start, self.fps)
                        {
                            self.kill();
                            self.child = child;
                            self.rx = rx;
                            self.elapsed = self.loop_start * 1000.0;
                            self.shown = (self.loop_start * self.fps).round() as u64;
                        }
                    } else {
                        self.finished = true;
                    }
                    break;
                }
            }
        }
        if let Some(f) = latest {
            r.write_region(&self.tex, [0, 0, W, H], 4, &f);
            self.has_frame = true;
        }
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // a platform decoder thread ends once its frame channel is gone
        self.rx = mpsc::sync_channel(0).1;
    }

    /// sub_1A80: stops playback; the last frame then fades out.
    pub fn stop(&mut self) {
        self.kill();
        self.finished = true;
        self.fading_in = false;
    }
}

impl Drop for Movie {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The platform decoder's frames match ffmpeg's from the start and after a seek (needs switch_romfs).
    #[test]
    fn platform_frames_match_ffmpeg() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../switch_romfs/movie/titlebg1.mp4");
        if !path.exists() {
            return;
        }
        for start in [0.0, 30.0] {
            let fps = super::track_fps(&path);
            let ours = super::decoder::spawn(&path, start, fps).unwrap().recv().unwrap();
            let (mut child, rx) = super::spawn_ffmpeg(&path, start).unwrap();
            let theirs = rx.recv().unwrap();
            let _ = child.kill();
            let diff = ours.iter().zip(&theirs).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>() as f64
                / ours.len() as f64;
            eprintln!("start {start}: mean abs diff {diff:.3}");
            assert!(diff < 2.0, "start {start}: mean abs diff {diff}");
        }
    }

    /// symphonia's AAC decode matches ffmpeg's (needs switch_romfs).
    #[test]
    fn audio_matches_ffmpeg() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../switch_romfs/movie/prologue.mp4");
        if !path.exists() {
            return;
        }
        let ours = super::decode_audio(&path).unwrap();
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&path)
            .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
            .output()
            .unwrap();
        let theirs: Vec<f32> = out.stdout.as_chunks::<4>().0.iter().map(|&c| f32::from_le_bytes(c)).collect();
        eprintln!("frames {} vs {}", ours.frames.len(), theirs.len() / 2);
        let n = ours.frames.len().min(theirs.len() / 2);
        let err = (0..n).map(|i| (ours.frames[i][0] - theirs[2 * i]).abs()).fold(0.0f32, f32::max);
        eprintln!("max abs diff {err}");
        assert_eq!(ours.sample_rate, 48000);
        assert!(ours.frames.len() == theirs.len() / 2 && err < 1e-3);
    }
}
