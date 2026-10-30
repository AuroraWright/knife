//! FADE(40, p, q) screen effect: screen mode 4 of qword_AFEBFC. Each colour channel of the frame is shifted
//! along a slowly swinging direction and the three are overlaid back onto the frame (sub_2FC50), with the
//! shift amplitude and angle animated every 60 Hz frame (sub_2F910). The parameters live in F words:
//! F1100 = screen mode (4 while on), F1120 = p + 127 (brightness), F1121 = q (preset). FADE(41) sets mode 5,
//! which fades the effect out.

use std::f32::consts::PI;

use knife_render::{Blend, PsOp, Quad, RenderTarget, Renderer};

/// The per-frame state of sub_2E280 / sub_2F910.
#[derive(Clone, Debug, Default)]
pub struct Ghost {
    /// qword_AFEBFC low word: 4 on (alpha rises 1/60 a frame), 5 fading out, anything else off
    pub mode: i32,
    /// qword_AFEBFC high word
    pub alpha: f32,
    /// AFEBE4: brightness offset (p, or the q = 16 ramp)
    bright: f32,
    /// AFEC68: the brightness latched while no transition runs
    latched: f32,
    /// aFff: shift amplitude in 960-wide pixels and the extra wobble (percent)
    base: f32,
    wobble: f32,
    /// qword_AFEBE8: frames since start, and the 6-frame step counter
    frame: i32,
    sub: i32,
    /// qword_AFEBF0: phase (0..3333 in steps of 100) and the curve value
    phase: i32,
    curve: f32,
    /// word_AFEB94: the phase wrapped once (the wobble stops), angle direction (+3 / -3)
    wrapped: bool,
    up: bool,
    /// AFEBF8: current shift amplitude (960-wide pixels)
    amp: f32,
    /// dword_572B5C: shift direction in degrees, swinging between -30 and 30
    angle: f32,
    /// unspent time for the 60 Hz steps
    acc: f32,
}

/// The q presets (sub_2E280): (amplitude, wobble, starting amplitude x 100).
fn preset(q: i32) -> (f32, f32, f32) {
    match q {
        6 | 7 | 12 => (2.5, 200.0, 50.0),
        8 => (3.5, 200.0, 70.0),
        9 => (3.1, 105.0, 62.0),
        10 => (3.1, 150.0, 62.0),
        13 => (1.5, 105.0, 30.0),
        _ => (3.1, 200.0, 62.0),
    }
}

/// q = 16: the brightness climbs from p to 0 by 90 / 8 per second.
fn ramp(p: f32, frame: i32) -> f32 {
    (p + ((frame as f32 * 90.0 * 0.125 / 60.0) as i32) as f32).min(0.0)
}

impl Ghost {
    fn reset_phase(&mut self, init: f32) {
        self.frame = 0;
        self.sub = 0;
        self.phase = 0;
        self.curve = 0.0;
        self.wrapped = false;
        self.up = false;
        self.amp = init / 100.0;
        self.angle = 30.0;
    }

    /// FADE(40) (sub_2E3E0(4, reset) -> sub_2E280): `reset` when the screen mode was 0 before, so a repeated
    /// FADE(40) only changes the brightness and preset of the running effect. A non-zero p shows it at once.
    pub fn start(&mut self, p: i32, q: i32, reset: bool) {
        self.mode = 4;
        let (base, wobble, init) = preset(q);
        (self.base, self.wobble) = (base, wobble);
        self.bright = if q == 16 && !reset { ramp(p as f32, self.frame) } else { p as f32 };
        if reset {
            self.reset_phase(init);
        }
        if p != 0 {
            self.alpha = 1.0;
        }
    }

    /// sub_2E430(mode): FADE(41) (mode 5) and the replay restoring F1100. Mode 4 always restarts the phase.
    pub fn set_mode(&mut self, mode: i32, p: i32, q: i32) {
        self.mode = mode;
        if mode == 4 {
            let (base, wobble, init) = preset(q);
            (self.base, self.wobble) = (base, wobble);
            self.bright = p as f32;
            self.reset_phase(init);
            if p != 0 {
                self.alpha = 1.0;
            }
        }
    }

    pub fn active(&self) -> bool {
        matches!(self.mode, 4 | 5)
    }

    /// Run the 60 Hz steps of sub_2F910 covered by `dt` ms.
    pub fn advance(&mut self, dt: f32, p: i32, q: i32) {
        if !self.active() {
            self.acc = 0.0;
            return;
        }
        self.acc += dt;
        while self.acc >= super::FRAME_MS && self.active() {
            self.acc -= super::FRAME_MS;
            self.step(p, q);
        }
    }

    fn step(&mut self, p: i32, q: i32) {
        if self.mode == 4 {
            self.alpha = (self.alpha + 0.016667).min(1.0);
        } else {
            self.alpha -= 0.016667;
            if self.alpha <= 0.0 {
                self.mode = 0;
                self.alpha = 0.0;
            }
        }
        self.frame += 1;
        let (base, wobble, _) = preset(q);
        (self.base, self.wobble) = (base, wobble);
        self.bright = if q == 16 { ramp(p as f32, self.frame) } else { p as f32 };
        let tick = self.sub >= 5;
        self.sub += 1;
        if !tick {
            return;
        }
        self.sub = 0;
        self.phase += 100;
        if self.phase >= 3334 {
            self.phase -= 3333;
            self.wrapped = true;
        }
        let folded = if self.phase > 1666 { 3333 - self.phase } else { self.phase };
        self.curve = 0.5 - (folded as f32 * PI / 1666.0).cos() * 0.5;
        if !self.wrapped {
            self.curve += self.wobble / 100.0 * (self.phase as f32 * PI / 3333.0).sin();
        }
        self.amp = self.base * ((self.curve * 80.0 + 20.0) / 100.0);
        // the angle swings by 3 degrees a step and reflects off +-30
        if self.up {
            self.angle += 3.0;
            if self.angle > 30.0 {
                self.angle = 30.0 - (self.angle - 30.0);
                self.up = false;
            }
        } else {
            self.angle -= 3.0;
            if self.angle < -30.0 {
                self.angle = -30.0 - (self.angle + 30.0);
                self.up = true;
            }
        }
    }

    /// sub_2FC50 over `canvas`. The scratch targets are the exe's render targets 2 (a copy of the frame), 1 (one
    /// channel) and 3 (the three channels). `trans` is the progress of a running transition, which eases a
    /// brightness change in.
    pub fn render(
        &mut self,
        r: &mut Renderer,
        enc: &mut wgpu::CommandEncoder,
        canvas: &RenderTarget,
        [src, chan, out]: [&RenderTarget; 3],
        trans: Option<f32>,
    ) {
        if self.alpha <= 0.0 || !self.active() {
            return;
        }
        let bright = match trans {
            Some(t) => self.latched + t * (self.bright - self.latched),
            None => {
                self.latched = self.bright;
                self.bright
            }
        };
        let [w, h] = canvas.units;
        let k = self.amp * w / 960.0;
        let th = self.angle.to_radians();
        let opp = th + PI;
        // red along the angle, green opposite, blue twice as far opposite (y down, as sub_A5690 adds it)
        let offs = [
            [th.cos() * k, -(th.sin() * k)],
            [opp.cos() * k, -(opp.sin() * k)],
            [2.0 * k * opp.cos(), -(2.0 * k * opp.sin())],
        ];
        // vertex colours 0xAARRGGBB: ((int)bright + 255) in one channel, built with the exe's wrapping maths
        let v = bright as i32 + 255;
        let words = [(v << 16).wrapping_sub(0x0100_0000), (v << 8).wrapping_sub(0x0100_0000), v.wrapping_sub(0x0100_0000)];
        r.copy_target(enc, canvas, src);
        let full = [0.0, 0.0, w, h];
        for (c, (word, [ox, oy])) in words.into_iter().zip(offs).enumerate() {
            let b = word as u32;
            let byte = |s: u32| ((b >> s) & 0xff) as f32 / 255.0;
            let col = [byte(16), byte(8), byte(0), byte(24)];
            // shader 2 (noalpha) without blending: the unshifted frame, then the shifted one over it
            let base = Quad { blend: Blend::Opaque, ..Quad::rect(src.tex.clone(), full, col) };
            let shifted = Quad { blend: Blend::Opaque, ..Quad::rect(src.tex.clone(), [ox, oy, w, h], col) };
            r.draw(enc, chan, None, &[base, shifted]);
            // red replaces the result (blend 8), green and blue add (blend 30: SrcAlpha / One)
            let blend = if c == 0 { Blend::Opaque } else { Blend::Add };
            r.draw(enc, out, None, &[Quad { blend, ..Quad::rect(chan.tex.clone(), full, col) }]);
        }
        // blend 22 (blend state 7, shader 31 = overlay) onto the frame at the effect's alpha
        let overlay = Blend::Ps(PsOp::Overlay);
        r.draw(enc, canvas, None, &[Quad { blend: overlay, ..Quad::rect(out.tex.clone(), full, [1.0, 1.0, 1.0, self.alpha]) }]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amplitude_and_angle_follow_sub_2f910() {
        let mut g = Ghost::default();
        g.start(0, 0, true);
        assert_eq!((g.alpha, g.amp, g.angle), (0.0, 0.62, 30.0));
        for _ in 0..6 {
            g.step(0, 0);
        }
        // first 6-frame step: phase 100, angle 27, amplitude from the curve plus the wobble
        assert_eq!((g.phase, g.angle), (100, 27.0));
        let curve = 0.5 - (100.0f32 * PI / 1666.0).cos() * 0.5 + 2.0 * (100.0f32 * PI / 3333.0).sin();
        assert!((g.amp - 3.1 * (curve * 80.0 + 20.0) / 100.0).abs() < 1e-5);
        assert!((g.alpha - 0.1).abs() < 1e-3);
        // the angle reflects at -30
        for _ in 0..6 * 20 {
            g.step(0, 0);
        }
        assert!(g.angle >= -30.0 && g.angle <= 30.0);
        g.set_mode(5, 0, 0);
        for _ in 0..200 {
            g.step(0, 0);
        }
        assert_eq!(g.mode, 0);
    }
}
