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

/// Taps on each side of the interpolation point and sub-sample phases of the resampling kernel.
const HALF_TAPS: usize = 12;
const PHASES: usize = 1024;

/// Windowed-sinc kernel for one source / output rate ratio: band-limited to the lower Nyquist rate, so a
/// device rate other than the source's (44.1 kHz output of 48 kHz audio) neither drops nor repeats samples.
struct Kernel {
    step: f64,
    /// `PHASES + 1` rows of `2 * HALF_TAPS` weights for the samples at offsets `1 - HALF_TAPS ..= HALF_TAPS`
    table: Vec<f32>,
}

impl Kernel {
    fn new(step: f64) -> Self {
        let cutoff = (1.0 / step).min(1.0);
        let w = 2 * HALF_TAPS;
        let mut table = vec![0f32; (PHASES + 1) * w];
        for ph in 0..=PHASES {
            let frac = ph as f64 / PHASES as f64;
            let row = &mut table[ph * w..(ph + 1) * w];
            let mut sum = 0.0;
            for (k, t) in row.iter_mut().enumerate() {
                let x = (k as f64 + 1.0 - HALF_TAPS as f64) - frac;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    let a = std::f64::consts::PI * cutoff * x;
                    a.sin() / a
                };
                // Blackman window over |x| < HALF_TAPS
                let u = (x / HALF_TAPS as f64 + 1.0) * 0.5;
                let win = if (0.0..=1.0).contains(&u) {
                    let a = 2.0 * std::f64::consts::PI * u;
                    0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
                } else {
                    0.0
                };
                let v = sinc * win;
                *t = v as f32;
                sum += v;
            }
            // unity gain at DC
            for t in row.iter_mut() {
                *t = (*t as f64 / sum) as f32;
            }
        }
        Self { step, table }
    }
}

struct PcmSound {
    pcm: Arc<Pcm>,
    pos: f64,
    looping: bool,
    ctl: Arc<Mutex<Control>>,
    finished: Arc<AtomicBool>,
    done: bool,
    kernel: Option<Kernel>,
}

impl PcmSound {
    /// The source frame at `i`, following the loop (silence before the start and after a one-shot's end).
    fn at(&self, i: i64) -> [f32; 2] {
        let n = self.pcm.frames.len() as i64;
        if i < 0 || n == 0 {
            return [0.0, 0.0];
        }
        let i = if i < n && !(self.looping && self.pcm.loop_range.is_some_and(|(_, le)| i >= le as i64)) {
            i
        } else if !self.looping {
            return [0.0, 0.0];
        } else if let Some((ls, le)) = self.pcm.loop_range {
            let (ls, le) = (ls as i64, (le as i64).min(n));
            if le <= ls {
                return [0.0, 0.0];
            }
            ls + (i - le).rem_euclid(le - ls)
        } else {
            i.rem_euclid(n)
        };
        self.pcm.frames[i as usize]
    }

    fn sample(&self, rate: f64) -> [f32; 2] {
        let base = self.pos.floor();
        let frac = self.pos - base;
        let base = base as i64;
        if rate == 1.0 && frac == 0.0 {
            return self.at(base);
        }
        let Some(k) = &self.kernel else { return self.at(base) };
        let w = 2 * HALF_TAPS;
        let ph = ((frac * PHASES as f64).round() as usize).min(PHASES);
        let row = &k.table[ph * w..(ph + 1) * w];
        let mut acc = [0f32; 2];
        for (j, &t) in row.iter().enumerate() {
            let s = self.at(base + j as i64 + 1 - HALF_TAPS as i64);
            acc[0] += s[0] * t;
            acc[1] += s[1] * t;
        }
        acc
    }
}

impl Sound for PcmSound {
    fn process(&mut self, out: &mut [Frame], dt: f64, _info: &Info) {
        let mut c = self.ctl.lock();
        let rate = self.pcm.sample_rate as f64 * dt;
        if rate != 1.0 && self.kernel.as_ref().is_none_or(|k| k.step != rate) {
            self.kernel = Some(Kernel::new(rate));
        }
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
            let s = self.sample(rate);
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
        self.play_at(pcm, volume, fade_in, looping, 0)
    }

    /// `play` from source frame `start` (nn::atk StartInfo start offset, type 2 = samples).
    pub fn play_at(&mut self, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool, start: usize) -> Voice {
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
                        pos: start as f64,
                        looping,
                        ctl,
                        finished,
                        done: false,
                        kernel: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 48 kHz audio at a 44.1 kHz device: a 10 kHz tone comes out clean
    #[test]
    fn resamples_band_limited() {
        let f = 10_000.0f64;
        let frames = (0..48_000).map(|i| {
            let v = (2.0 * std::f64::consts::PI * f * i as f64 / 48_000.0).sin() as f32;
            [v, v]
        });
        let pcm = Arc::new(Pcm { sample_rate: 48_000, frames: frames.collect(), loop_range: None });
        let rate = 48_000.0 / 44_100.0;
        let mut s = PcmSound {
            pcm,
            pos: 0.0,
            looping: false,
            ctl: Arc::new(Mutex::new(Control {
                volume: Ramp { from: 1.0, to: 1.0, len: 0.0, t: 0.0 },
                pan: 0.0,
                stop_after_ramp: false,
                stop_now: false,
                paused: false,
            })),
            finished: Arc::new(AtomicBool::new(false)),
            done: false,
            kernel: Some(Kernel::new(rate)),
        };
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for _ in 0..40_000 {
            if s.pos > 100.0 {
                let want = (2.0 * std::f64::consts::PI * f * s.pos / 48_000.0).sin();
                let got = s.sample(rate)[0] as f64;
                err += (got - want).powi(2);
                sig += want * want;
            }
            s.pos += rate;
        }
        let snr = 10.0 * (sig / err).log10();
        assert!(snr > 60.0, "SNR {snr:.1} dB");
    }
}
