//! Audio playback on top of kira: decoded PCM sounds with volume ramps, looping and
//! stop/fade control from the game thread.

pub mod bfsar;
pub mod codec;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Result;
use kira::{
    AudioManager, AudioManagerSettings, DefaultBackend, Frame,
    info::Info,
    sound::{Sound, SoundData},
};
use parking_lot::Mutex;

pub use codec::Pcm;

#[derive(Debug, Clone, Copy)]
struct Ramp {
    from: f32,
    to: f32,
    /// total length in seconds (0 = immediate)
    len: f64,
    t: f64,
}

impl Ramp {
    fn value(&self) -> f32 {
        if self.len <= 0.0 || self.t >= self.len {
            self.to
        } else {
            self.from + (self.to - self.from) * (self.t / self.len) as f32
        }
    }
    fn done(&self) -> bool {
        self.len <= 0.0 || self.t >= self.len
    }
}

#[derive(Debug)]
struct Control {
    volume: Ramp,
    pan: f32,
    /// Stop once the current volume ramp completes.
    stop_after_ramp: bool,
    stop_now: bool,
    paused: bool,
}

/// Handle to a playing sound.
#[derive(Clone)]
pub struct Voice {
    ctl: Arc<Mutex<Control>>,
    finished: Arc<AtomicBool>,
}

impl Voice {
    /// Change volume (0..1) over `secs`.
    pub fn set_volume(&self, volume: f32, secs: f64) {
        let mut c = self.ctl.lock();
        let cur = c.volume.value();
        c.volume = Ramp {
            from: cur,
            to: volume,
            len: secs,
            t: 0.0,
        };
    }

    pub fn set_pan(&self, pan: f32) {
        self.ctl.lock().pan = pan.clamp(-1.0, 1.0);
    }

    /// Fade out over `secs` and stop.
    pub fn stop(&self, secs: f64) {
        let mut c = self.ctl.lock();
        if secs <= 0.0 {
            c.stop_now = true;
        } else {
            let cur = c.volume.value();
            c.volume = Ramp {
                from: cur,
                to: 0.0,
                len: secs,
                t: 0.0,
            };
            c.stop_after_ramp = true;
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.ctl.lock().paused = paused;
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    pub fn volume(&self) -> f32 {
        self.ctl.lock().volume.value()
    }
}

struct PcmSound {
    pcm: Arc<Pcm>,
    pos: f64,
    looping: bool,
    ctl: Arc<Mutex<Control>>,
    finished: Arc<AtomicBool>,
    done: bool,
}

impl Sound for PcmSound {
    fn process(&mut self, out: &mut [Frame], dt: f64, _info: &Info) {
        let mut c = self.ctl.lock();
        let rate = self.pcm.sample_rate as f64 * dt;
        let n = self.pcm.frames.len();
        for f in out.iter_mut() {
            if self.done || c.stop_now {
                self.done = true;
                *f = Frame::ZERO;
                continue;
            }
            if c.paused {
                *f = Frame::ZERO;
                continue;
            }
            c.volume.t += dt;
            if c.stop_after_ramp && c.volume.done() {
                self.done = true;
                *f = Frame::ZERO;
                continue;
            }
            let i = self.pos as usize;
            if i >= n {
                if let Some((ls, le)) = self.pcm.loop_range.filter(|_| self.looping) {
                    self.pos = ls as f64 + (self.pos - le.min(n) as f64).max(0.0);
                } else if self.looping && n > 0 {
                    self.pos = 0.0;
                } else {
                    self.done = true;
                    *f = Frame::ZERO;
                    continue;
                }
            }
            let i = (self.pos as usize).min(n.saturating_sub(1));
            let s = self.pcm.frames.get(i).copied().unwrap_or([0.0, 0.0]);
            let v = c.volume.value();
            let (pl, pr) = if c.pan < 0.0 { (1.0, 1.0 + c.pan) } else { (1.0 - c.pan, 1.0) };
            *f = Frame {
                left: s[0] * v * pl,
                right: s[1] * v * pr,
            };
            self.pos += rate;
            if let Some((_, le)) = self.pcm.loop_range.filter(|_| self.looping)
                && self.pos as usize >= le
            {
                let (ls, le) = self.pcm.loop_range.unwrap();
                self.pos = ls as f64 + (self.pos - le as f64);
            }
        }
        if self.done {
            self.finished.store(true, Ordering::Relaxed);
        }
    }

    fn finished(&self) -> bool {
        self.done
    }
}

struct PcmSoundData {
    sound: PcmSound,
}

impl SoundData for PcmSoundData {
    type Error = ();
    type Handle = ();
    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        Ok((Box::new(self.sound), ()))
    }
}

pub struct Audio {
    manager: Option<AudioManager<DefaultBackend>>,
}

impl Audio {
    pub fn new() -> Self {
        let manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::error!("audio disabled: {e}");
                None
            }
        };
        Self { manager }
    }

    /// Start playing. Returns a handle even when audio is disabled.
    pub fn play(&mut self, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool) -> Voice {
        let ctl = Arc::new(Mutex::new(Control {
            volume: Ramp {
                from: if fade_in > 0.0 { 0.0 } else { volume },
                to: volume,
                len: fade_in,
                t: 0.0,
            },
            pan: 0.0,
            stop_after_ramp: false,
            stop_now: false,
            paused: false,
        }));
        let finished = Arc::new(AtomicBool::new(false));
        let voice = Voice {
            ctl: ctl.clone(),
            finished: finished.clone(),
        };
        match &mut self.manager {
            Some(m) => {
                let data = PcmSoundData {
                    sound: PcmSound {
                        pcm,
                        pos: 0.0,
                        looping,
                        ctl,
                        finished,
                        done: false,
                    },
                };
                if m.play(data).is_err() {
                    voice.finished.store(true, Ordering::Relaxed);
                }
            }
            None => voice.finished.store(true, Ordering::Relaxed),
        }
        voice
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}
