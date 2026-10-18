//! Movie playback through an external `ffmpeg` process (H.264 MP4 -> raw RGBA frames).

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

const W: u32 = 1920;
const H: u32 = 1080;

pub struct Movie {
    child: Option<Child>,
    rx: mpsc::Receiver<Vec<u8>>,
    pub tex: TexRef,
    fps: f32,
    elapsed: f32,
    shown: u64,
    pub finished: bool,
    pub looping: bool,
    has_frame: bool,
    path: std::path::PathBuf,
}

fn probe_fps(path: &Path) -> f32 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=r_frame_rate", "-of", "csv=p=0"])
        .arg(path)
        .output();
    let Ok(out) = out else { return 30.0 };
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    match s.split_once('/') {
        Some((a, b)) => {
            let (a, b) = (a.parse::<f32>().unwrap_or(30.0), b.parse::<f32>().unwrap_or(1.0));
            if b > 0.0 { a / b } else { 30.0 }
        }
        None => s.parse().unwrap_or(30.0),
    }
}

fn spawn_video(path: &Path) -> Result<(Child, mpsc::Receiver<Vec<u8>>)> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{W}x{H}"), "-"])
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
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.stdout.len() < 8 {
        return None;
    }
    let frames = out
        .stdout
        .chunks_exact(8)
        .map(|c| {
            [
                f32::from_le_bytes(c[0..4].try_into().unwrap()),
                f32::from_le_bytes(c[4..8].try_into().unwrap()),
            ]
        })
        .collect();
    Some(Pcm {
        sample_rate: 48000,
        frames,
        loop_range: None,
    })
}

impl Movie {
    pub fn open(r: &Renderer, path: &Path, looping: bool) -> Result<Self> {
        let fps = probe_fps(path);
        let (child, rx) = spawn_video(path)?;
        Ok(Self {
            child: Some(child),
            rx,
            tex: r.create_writable(W, H, false, "movie"),
            fps,
            elapsed: 0.0,
            shown: 0,
            finished: false,
            looping,
            has_frame: false,
            path: path.to_path_buf(),
        })
    }

    pub fn has_frame(&self) -> bool {
        self.has_frame
    }

    pub fn update(&mut self, r: &Renderer, dt: f32) {
        if self.finished {
            return;
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
                        if let Ok((child, rx)) = spawn_video(&self.path) {
                            self.kill();
                            self.child = Some(child);
                            self.rx = rx;
                            self.elapsed = 0.0;
                            self.shown = 0;
                        }
                    } else {
                        self.finished = true;
                    }
                    break;
                }
            }
        }
        if let Some(f) = latest {
            r.write_region(&self.tex, 0, 0, W, H, 4, &f);
            self.has_frame = true;
        }
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn stop(&mut self) {
        self.kill();
        self.finished = true;
    }
}

impl Drop for Movie {
    fn drop(&mut self) {
        self.kill();
    }
}
