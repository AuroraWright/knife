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

/// sub_CA0's volume curve (flt_3355F0, 100 points over 0..1, linear in between; 1.0 from 0.99).
const VOLUME_CURVE: [f32; 100] = [
    0.0, 0.001442, 0.002885, 0.004327, 0.005769, 0.007212, 0.008654, 0.010096, 0.011538, 0.016667, 0.021795,
    0.026923, 0.030769, 0.034615, 0.038462, 0.042308, 0.046154, 0.05, 0.055769, 0.061538, 0.069231, 0.076923,
    0.082692, 0.088462, 0.094231, 0.1, 0.105769, 0.111538, 0.119231, 0.126923, 0.132692, 0.138462, 0.15, 0.155769,
    0.161538, 0.176923, 0.182692, 0.188462, 0.2, 0.205769, 0.211538, 0.226923, 0.238462, 0.244231, 0.25, 0.261538,
    0.276923, 0.288462, 0.294231, 0.3, 0.311538, 0.326923, 0.338462, 0.35, 0.361538, 0.376923, 0.382692, 0.388462,
    0.4, 0.411538, 0.426923, 0.438462, 0.45, 0.461538, 0.476923, 0.488462, 0.5, 0.511538, 0.526923, 0.538462, 0.55,
    0.576923, 0.588462, 0.6, 0.611538, 0.625, 0.638462, 0.65, 0.661538, 0.676923, 0.688462, 0.711538, 0.725,
    0.738462, 0.75, 0.776923, 0.788462, 0.8, 0.811538, 0.826923, 0.85, 0.863462, 0.876923, 0.9, 0.913462, 0.926923,
    0.938462, 0.961538, 0.976923, 0.988462,
];

/// sub_CA0: below 0.0014423 as is; else 100v < 1 -> 0, the table interpolated up to 98, 1.0 above.
pub fn volume_curve(v: f32) -> f32 {
    if v < 0.0014423 {
        return v;
    }
    let x = v * 100.0;
    let i = x as usize;
    match i {
        0 => 0.0,
        1..=98 => VOLUME_CURVE[i] + (x - i as f32) * (VOLUME_CURVE[i + 1] - VOLUME_CURVE[i]),
        _ => 1.0,
    }
}

/// How a sound player turns its volume (volume x category, 0..1) into gain (sub_9B7F0 / sub_9B5D0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    /// scene voices (players 2 / 3, sub_EEA34) and the system sounds of sub_ACEF0
    Linear,
    /// SE channels (players 11..30): sub_CA0
    Se,
    /// BGM: sub_E1BFC easing 12 (quadratic Bezier 0 -> 1 through 0.77, sub_FF4B8) inside 0..1, then sub_CA0
    Bgm,
}

impl Curve {
    pub fn apply(self, v: f32) -> f32 {
        match self {
            Curve::Linear => v,
            Curve::Se => volume_curve(v),
            Curve::Bgm => {
                let b = if v > 0.0 && v < 1.0 { v * v + 2.0 * v * (1.0 - v) * 0.77 } else { v };
                volume_curve(b)
            }
        }
    }
}

struct Control {
    /// volume before the curve; fades step it linearly, as the exe steps its 0..100 volume words
    volume: Ramp,
    /// category gain, multiplied in before the curve (the exe's player volume x category byte)
    gain: f32,
    curve: Curve,
    pan: f32,
    /// Fading out: stop once the volume has come down to 0 (sub_9B7F0, state 2: a later volume change can
    /// retarget the ramp, and the sound only stops at 0).
    stop_after_ramp: bool,
    stop_now: bool,
    paused: bool,
    /// the whole source for a voice started on a prefix (`Audio::play_prefix`), taken by the next `process`
    swap: Option<Arc<Pcm>>,
}

impl Control {
    fn ramp_to(&mut self, volume: f32, per_sec: f32) {
        let cur = self.volume.value();
        let len = if per_sec > 0.0 { f64::from((volume - cur).abs() / per_sec) } else { 0.0 };
        self.volume = Ramp {
            from: cur,
            to: volume,
            len,
            t: 0.0,
        };
    }
}

/// A change made through a `Voice`, held until `Audio::flush`.
type Change = Box<dyn FnOnce(&mut Control) + Send>;

/// The voices changed since the last `Audio::flush`, with their changes in order.
type Staged = Arc<Mutex<Vec<(Arc<Mutex<Control>>, Change)>>>;

/// Handle to a playing sound.
#[derive(Clone)]
///
/// Changes take effect at the next `Audio::flush`, together with the sounds started in the same frame, as
/// nn::atk hands a frame's commands to the renderer at once; the volume a change starts from is the one at
/// the flush.
pub struct Voice {
    ctl: Arc<Mutex<Control>>,
    finished: Arc<AtomicBool>,
    staged: Staged,
}

impl Voice {
    fn change(&self, f: impl FnOnce(&mut Control) + Send + 'static) {
        self.staged.lock().push((self.ctl.clone(), Box::new(f)));
    }

    /// Change volume (0..1) over `secs`.
    pub fn set_volume(&self, volume: f32, secs: f64) {
        self.change(move |c| {
            let cur = c.volume.value();
            c.volume = Ramp {
                from: cur,
                to: volume,
                len: secs,
                t: 0.0,
            };
        });
    }

    /// Move the volume to `volume` at `per_sec` (volume units a second; <= 0 jumps), as the exe's players step
    /// their volume by a rate each frame (sub_9B7F0).
    pub fn ramp_to(&self, volume: f32, per_sec: f32) {
        self.change(move |c| c.ramp_to(volume, per_sec));
    }

    /// Fade out at `per_sec` and stop at 0 (sub_9D020 / sub_9CFE0: state 2).
    pub fn fade_out(&self, per_sec: f32) {
        self.change(move |c| {
            c.ramp_to(0.0, per_sec);
            c.stop_after_ramp = true;
        });
    }

    /// The category gain (applied at once, outside the volume ramp).
    pub fn set_gain(&self, gain: f32) {
        self.change(move |c| c.gain = gain);
    }

    /// nn::atk pan, -1 (left) .. 1 (right).
    pub fn set_pan(&self, pan: f32) {
        self.change(move |c| c.pan = pan.clamp(-1.0, 1.0));
    }

    /// Fade out over `secs` and stop.
    pub fn stop(&self, secs: f64) {
        self.change(move |c| {
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
        });
    }

    pub fn set_paused(&self, paused: bool) {
        self.change(move |c| c.paused = paused);
    }

    /// Go on from the same position in `pcm`, the whole source of a voice started on a prefix of it.
    pub fn replace_source(&self, pcm: Arc<Pcm>) {
        self.change(move |c| c.swap = Some(pcm));
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
    /// a prefix of the source still decoding: `pcm` holds source frames `base..`, silence elsewhere, and playback
    /// waits at its end
    partial: bool,
    base: usize,
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
        if self.partial {
            let j = i - self.base as i64;
            return usize::try_from(j).ok().and_then(|j| self.pcm.frames.get(j).copied()).unwrap_or([0.0; 2]);
        }
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
        if let Some(p) = c.swap.take() {
            self.pcm = p;
            self.partial = false;
            self.base = 0;
        }
        let rate = self.pcm.sample_rate as f64 * dt;
        if rate != 1.0 && self.kernel.as_ref().is_none_or(|k| k.step != rate) {
            self.kernel = Some(Kernel::new(rate));
        }
        let n = self.pcm.frames.len();
        // nn::atk's pan (sub_12A1A8 with curve 0, the 257-entry table sqrt(1 - i / 256) at 0x3cf91c): a channel
        // at position x gets curve(x) on the left and curve(-x) on the right. A mono source's one channel sits at
        // the pan (its frames hold 1 / sqrt 2 = curve(0) already); wave sounds pan in dual mode, the left channel
        // at pan - 1 and the right at pan + 1 (stereo keeps full level centred)
        let curve = |x: f32| {
            let i = if x < -1.0 { 0 } else { ((x.min(1.0) + 1.0) * 0.5 * 256.0 + 0.5) as usize };
            (1.0 - i.min(256) as f32 / 256.0).sqrt()
        };
        let pan = if c.pan == 0.0 {
            [[1.0, 0.0], [0.0, 1.0]]
        } else if self.pcm.mono {
            let k = std::f32::consts::SQRT_2;
            [[curve(c.pan) * k, 0.0], [0.0, curve(-c.pan) * k]]
        } else {
            let (l, r) = (c.pan - 1.0, c.pan + 1.0);
            [[curve(l), curve(-l)], [curve(r), curve(-r)]]
        };
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
            if c.stop_after_ramp && c.volume.done() && c.volume.value() <= 0.0 {
                self.done = true;
                *f = Frame::ZERO;
                continue;
            }
            let i = self.pos as usize;
            if self.partial {
                // the rest is still decoding
                if i + HALF_TAPS >= self.base + n {
                    *f = Frame::ZERO;
                    continue;
                }
            } else if i >= n {
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
            let v = c.curve.apply(c.volume.value() * c.gain);
            *f = Frame {
                left: (s[0] * pan[0][0] + s[1] * pan[1][0]) * v,
                right: (s[0] * pan[0][1] + s[1] * pan[1][1]) * v,
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
    /// sounds started since the last `flush`
    queued: Vec<PcmSoundData>,
    staged: Staged,
}

impl Audio {
    pub fn new() -> Self {
        let settings = AudioManagerSettings { backend_settings: backend_settings(), ..Default::default() };
        let manager = match AudioManager::<DefaultBackend>::new(settings) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::error!("audio disabled: {e}");
                None
            }
        };
        Self {
            manager,
            queued: Vec::new(),
            staged: Staged::default(),
        }
    }

    /// Silence everything (the mixer's volume) while the app is in the background, or bring the sound back.
    pub fn set_muted(&mut self, muted: bool) {
        if let Some(manager) = &mut self.manager {
            let volume = if muted { kira::Decibels::SILENCE } else { kira::Decibels::IDENTITY };
            manager.main_track().set_volume(volume, kira::Tween::default());
        }
    }

    /// Once a frame, after the game's and the UI's update: hand the frame's changes and new sounds to the
    /// mixer together (nn::atk's update, sub_106734, flushes a frame's commands to the renderer).
    pub fn flush(&mut self) {
        let staged = std::mem::take(&mut *self.staged.lock());
        for (ctl, f) in staged {
            f(&mut ctl.lock());
        }
        for data in self.queued.drain(..) {
            let finished = data.sound.finished.clone();
            if self.manager.as_mut().is_none_or(|m| m.play(data).is_err()) {
                finished.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Start playing. Returns a handle even when audio is disabled.
    pub fn play(&mut self, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool) -> Voice {
        self.play_at(pcm, volume, fade_in, looping, 0, Curve::Linear)
    }

    /// `play` from source frame `start` (nn::atk StartInfo start offset, type 2 = samples), the volume through
    /// `curve`.
    pub fn play_at(&mut self, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool, start: usize, curve: Curve) -> Voice {
        self.start(pcm, None, (volume, 1.0, 0.0), fade_in, looping, start, curve)
    }

    /// `play_at` from the start with a category gain on top of `volume` (`Voice::set_gain`) and a pan
    /// (`Voice::set_pan`).
    pub fn play_gain(&mut self, pcm: Arc<Pcm>, (volume, gain, pan): (f32, f32, f32), fade_in: f64, looping: bool, curve: Curve) -> Voice {
        self.start(pcm, None, (volume, gain, pan.clamp(-1.0, 1.0)), fade_in, looping, 0, curve)
    }

    /// `play_at` on `prefix`, source frames `base..` of a sound still decoding; `Voice::replace_source` hands
    /// over the whole sound.
    #[allow(clippy::too_many_arguments)]
    pub fn play_prefix(
        &mut self,
        prefix: Arc<Pcm>,
        base: usize,
        volume: f32,
        fade_in: f64,
        looping: bool,
        start: usize,
        curve: Curve,
    ) -> Voice {
        self.start(prefix, Some(base), (volume, 1.0, 0.0), fade_in, looping, start, curve)
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        pcm: Arc<Pcm>,
        prefix_base: Option<usize>,
        (volume, gain, pan): (f32, f32, f32),
        fade_in: f64,
        looping: bool,
        start: usize,
        curve: Curve,
    ) -> Voice {
        let ctl = Arc::new(Mutex::new(Control {
            volume: Ramp {
                from: if fade_in > 0.0 { 0.0 } else { volume },
                to: volume,
                len: fade_in,
                t: 0.0,
            },
            gain,
            curve,
            pan,
            stop_after_ramp: false,
            stop_now: false,
            paused: false,
            swap: None,
        }));
        let finished = Arc::new(AtomicBool::new(false));
        let voice = Voice {
            ctl: ctl.clone(),
            finished: finished.clone(),
            staged: self.staged.clone(),
        };
        if self.manager.is_some() {
            self.queued.push(PcmSoundData {
                sound: PcmSound {
                    pcm,
                    partial: prefix_base.is_some(),
                    base: prefix_base.unwrap_or(0),
                    pos: start as f64,
                    looping,
                    ctl,
                    finished,
                    done: false,
                    kernel: None,
                },
            });
        } else {
            voice.finished.store(true, Ordering::Relaxed);
        }
        voice
    }
}

/// The output stream in 5 ms buffers: nn::audio's renderer runs 5 ms audio frames (240 samples at 48 kHz) and
/// applies the commands of each game frame (nn::atk's update, `RequestUpdateAudioRenderer`) at its next one, so
/// sounds start and stop on a 5 ms grid. kira takes new commands once a device buffer, which is 512 frames
/// (10.7 ms) by default on macOS. (Not on Android, where a phone's own buffer size is left alone: 5 ms buffers
/// underrun there.)
fn backend_settings() -> kira::backend::cpal::CpalBackendSettings {
    use kira::backend::cpal::cpal::{
        BufferSize, SupportedBufferSize,
        traits::{DeviceTrait, HostTrait},
    };
    let config = kira::backend::cpal::cpal::default_host().default_output_device().and_then(|d| d.default_output_config().ok()).map(|c| {
        let frames = c.sample_rate() / 200;
        let mut config = c.config();
        if let SupportedBufferSize::Range { min, max } = *c.buffer_size()
            && (min..=max).contains(&frames)
            && !cfg!(target_os = "android")
        {
            config.buffer_size = BufferSize::Fixed(frames);
        }
        config
    });
    kira::backend::cpal::CpalBackendSettings { device: None, config }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sub_CA0 and the BGM easing at the config defaults: an SE at 30 (x SE 40) and a BGM at 100 (x BGM 40)
    #[test]
    fn volume_curves() {
        assert!((Curve::Se.apply(0.12) - 0.030769).abs() < 1e-5);
        // 0.4 eases to 0.5296, between table points 52 and 53
        assert!((Curve::Bgm.apply(0.4) - 0.349538).abs() < 1e-5);
        assert_eq!(Curve::Bgm.apply(1.0), 1.0);
        assert_eq!(Curve::Se.apply(0.0), 0.0);
        assert_eq!(Curve::Linear.apply(0.35), 0.35);
    }

    /// 48 kHz audio at a 44.1 kHz device: a 10 kHz tone comes out clean
    #[test]
    fn resamples_band_limited() {
        let f = 10_000.0f64;
        let frames = (0..48_000).map(|i| {
            let v = (2.0 * std::f64::consts::PI * f * i as f64 / 48_000.0).sin() as f32;
            [v, v]
        });
        let pcm = Arc::new(Pcm { sample_rate: 48_000, frames: frames.collect(), loop_range: None, mono: false });
        let rate = 48_000.0 / 44_100.0;
        let mut s = PcmSound {
            pcm,
            partial: false,
            base: 0,
            pos: 0.0,
            looping: false,
            ctl: Arc::new(Mutex::new(Control {
                volume: Ramp { from: 1.0, to: 1.0, len: 0.0, t: 0.0 },
                gain: 1.0,
                curve: Curve::Linear,
                pan: 0.0,
                stop_after_ramp: false,
                stop_now: false,
                paused: false,
                swap: None,
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
