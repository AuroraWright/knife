//! The screen shake (qword_AFEC04..AFEC30, stepped by sub_2EF30 once per 60 Hz frame). SHOK starts its sine
//! type (sub_5F8F0 -> sub_2EE70 type 1), SHAF stops or fades it (sub_2E560 / sub_2E9D0). The offset
//! (off_AE0580 / off_AE0588) is in the 640x448 draw space; sub_34440 adds it twice to each layer whose image
//! is not exempt, and sub_32050 shifts the clip scissors the same way. The other types (SHAK, FADE 0 / 1 / 2 /
//! 7 presets) are not reachable from the scripts.

/// sub_2EE70 / sub_2EF30's 2 pi
#[allow(clippy::approx_constant)]
const EXE_TAU: f32 = 6.2832;

#[derive(Clone, Copy, Debug, Default)]
struct Start {
    ax: i32,
    ay: i32,
    period_ms: i32,
    count: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ScreenShake {
    /// dword_AFEC04: 1 = the SHOK sine, -1 / 0 = none here
    kind: i32,
    /// HIDWORD(qword_AFEC18): frames left; 0 = still
    frames: i32,
    /// qword_AFEC20: frames per cycle and cycle count
    period: f32,
    count: f32,
    /// qword_AFEC28: amplitudes
    amp: (f32, f32),
    /// qword_AFEC30: phase and the amplitude factor falling to 0 over the cycles
    phase: f32,
    fade: f32,
    /// dword_AFEC58 / dword_AFECB0..: a delayed start (ms left) and its parameters
    delay: i32,
    pending: Start,
    /// off_AE0580 / off_AE0588
    pub offset: (f32, f32),
    acc: f32,
}

impl ScreenShake {
    /// sub_2EE70(1, ax, ay, period_ms, count, delay_ms): start now, or after the delay (the running shake keeps
    /// going until then).
    pub fn start(&mut self, ax: i32, ay: i32, period_ms: i32, count: i32, delay_ms: i32) {
        let s = Start { ax, ay, period_ms, count };
        self.delay = delay_ms;
        if delay_ms < 1 {
            self.begin(s);
        } else {
            self.pending = s;
        }
    }

    fn begin(&mut self, s: Start) {
        self.kind = 1;
        self.frames = 3 * s.period_ms * s.count / 50;
        let p = s.period_ms as f32 * 3.0 / 50.0;
        self.period = if p == 0.0 { 1.0 } else { p };
        self.count = s.count.max(1) as f32;
        self.amp = (s.ax as f32, s.ay as f32);
        (self.phase, self.fade) = (0.0, 1.0);
    }

    /// sub_2E560 / sub_2EA70(-1): no shake (a delayed SHOK still starts later).
    pub fn stop(&mut self) {
        self.kind = -1;
        self.frames = 0;
    }

    /// SHAF(ms) (sub_2E9D0): the shake runs for 60 ms / 1000 more frames with qword_AFEC30 set to minus the
    /// amplitudes over them (for the sine that zeroes the factor); 0 frames stops it.
    pub fn fade_out(&mut self, ms: i32) {
        let f = 60 * ms / 1000;
        self.frames = f;
        if f > 0 {
            self.phase = -(self.amp.0 / f as f32);
            self.fade = -(self.amp.1 / f as f32);
        } else {
            self.stop();
        }
    }

    pub fn advance(&mut self, dt: f32) {
        self.acc += dt;
        while self.acc >= super::FRAME_MS {
            self.acc -= super::FRAME_MS;
            self.step();
        }
    }

    fn step(&mut self) {
        if self.delay >= 1 {
            self.delay = (self.delay - 1000 / 60).max(0);
            if self.delay == 0 {
                self.begin(self.pending);
            }
        }
        if self.frames == 0 {
            self.offset = (0.0, 0.0);
            return;
        }
        if self.kind != 1 {
            return;
        }
        if self.frames >= 1 {
            self.frames = (self.frames - 1).max(0);
            if self.frames == 0 {
                self.kind = 0;
                (self.period, self.amp, self.phase, self.fade) = (0.0, (0.0, 0.0), 0.0, 0.0);
                self.offset = (0.0, 0.0);
                return;
            }
        }
        self.phase += EXE_TAU / self.period;
        self.fade = (self.fade - 1.0 / self.period / self.count).max(0.0);
        let s = self.phase.sin();
        self.offset = (s * self.amp.0 * self.fade * 640.0 / 1280.0, s * self.amp.1 * self.fade * 480.0 / 720.0);
    }
}

/// MSQK message-text quake (sub_39110, stepped in sub_3C970): a fresh uniform offset within the amplitudes
/// (640x448 units) every frame until the duration (ms) has passed.
#[derive(Clone, Debug, Default)]
pub struct MsgQuake {
    ms: f32,
    elapsed: f32,
    amp: (f32, f32),
    pub offset: (f32, f32),
    acc: f32,
}

impl MsgQuake {
    /// sub_39110(ms, ay, ax): over 10000 ms becomes 1000.
    pub fn start(&mut self, ms: i32, ax: i32, ay: i32) {
        self.ms = if (0..=10000).contains(&ms) { ms as f32 } else { 1000.0 };
        self.elapsed = 0.0;
        self.amp = (ax as f32 * 640.0 / 1280.0, ay as f32 * 448.0 / 720.0);
        self.offset = (0.0, 0.0);
    }

    pub fn stop(&mut self) {
        *self = Self::default();
    }

    pub fn advance(&mut self, dt: f32, rng: &mut crate::fx::Rng) {
        self.acc += dt;
        while self.acc >= super::FRAME_MS {
            self.acc -= super::FRAME_MS;
            if self.ms <= 0.0 {
                continue;
            }
            self.elapsed += 1000.0 / 60.0;
            if self.elapsed >= self.ms {
                self.ms = 0.0;
                self.offset = (0.0, 0.0);
            } else {
                let mut u = |a: f32| -a + 2.0 * a * rng.f();
                self.offset = (u(self.amp.0), u(self.amp.1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shok_sine_fades_over_its_cycles() {
        // SHOK(0, 3, 100, 10): 6 frames a cycle, 60 frames
        let mut s = ScreenShake::default();
        s.start(0, 3, 100, 10, 0);
        assert_eq!(s.frames, 60);
        s.step();
        let want = (EXE_TAU / 6.0).sin() * 3.0 * (1.0 - 1.0 / 60.0) * 480.0 / 720.0;
        assert!((s.offset.1 - want).abs() < 1e-4 && s.offset.0 == 0.0);
        for _ in 0..59 {
            s.step();
        }
        assert_eq!(s.offset, (0.0, 0.0));
    }

    #[test]
    fn delayed_shok_waits() {
        let mut s = ScreenShake::default();
        s.start(0, 3, 100, 10, 50);
        s.step();
        s.step();
        assert_eq!(s.offset, (0.0, 0.0));
        s.step();
        s.step();
        assert_ne!(s.offset, (0.0, 0.0));
    }
}
