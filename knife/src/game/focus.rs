//! FCLN screen effects (sub_86470 start / stop, sub_86850 step, sub_87A90 draw from the start of the text
//! pass sub_391A0 unless F963 = 1). Effects 4..=33 are manga focus lines: up to 400 strips of the FocusLine
//! texture (EFFECT_PARTS, texture 24) radiating from a centre and re-rolled every interval, keyed by a table of
//! up to 4 timed keyframes per effect (dword_35153C). Effects 1..=3 are falling musical notes (FallNote0..2,
//! textures 21..23), 50 sprites with their own drift. The state lives at qword_AED480 + 0x5CAC: mode (0 off,
//! 1 fading in, 2 on, 3 fading out), effect, level and its per-frame rate; the particles at word_14715DC.
//!
//! Positions are kept in the exe's 640x448 draw space; lines and notes are sized in native pixels of the
//! 1920x1080 frame (the draws scale by 640 / 1920 and 448 / 1080 before mapping back to the frame).

use knife_render::{CANVAS_H, CANVAS_W, Quad, TexRef};

use crate::fx::Rng;

/// Line keyframes per effect - 4 (dword_35153C): time ms (-1 = unused), centre x / y (1280x720), radius min /
/// max, line count min / max, alpha (0..255), re-roll interval ms.
#[rustfmt::skip]
const LINES: [[[i32; 9]; 4]; 30] = {
    const N: [i32; 9] = [-1; 9];
    [
        [[0, 640, 360, 600, 200, 300, 400, 255, 32], N, N, N],
        [[0, 640, 360, 184, 400, 70, 90, 169, 32], N, N, N],
        [[0, 806, 360, 288, 479, 40, 80, 255, 50], [1100, 734, 417, 323, 407, 40, 80, 255, 50], [1200, 734, 417, 323, 407, 40, 80, 255, 50], N],
        [[0, 817, 956, 800, 300, 60, 100, 255, 50], N, N, N],
        [[0, 800, 327, 300, 400, 50, 60, 255, 32], N, N, N],
        [[0, 420, 360, 300, 100, 25, 50, 80, 50], N, N, N],
        [[0, 850, 360, 400, 100, 25, 50, 80, 50], N, N, N],
        [[0, 600, 360, 400, 100, 25, 50, 80, 50], N, N, N],
        [[0, 700, 360, 400, 100, 25, 50, 80, 50], N, N, N],
        [[0, 600, 360, 400, 100, 25, 50, 150, 50], N, N, N],
        [[0, 600, 360, 400, 100, 25, 50, 120, 50], N, N, N],
        [[0, 600, 360, 2000, 100, 25, 400, 255, 30], N, N, N],
        [[0, 650, 360, 2000, 100, 100, 200, 255, 30], N, N, N],
        [[0, 650, 360, 2000, 100, 70, 120, 255, 30], N, N, N],
        [[0, 878, 279, 242, 329, 10, 15, 0, 50], [200, 878, 279, 242, 329, 10, 15, 128, 50], N, N],
        [[0, 540, 453, 119, 324, 25, 50, 0, 20], [200, 540, 453, 119, 324, 25, 50, 255, 20], N, N],
        [[0, 428, 376, 175, 395, 25, 50, 0, 20], [200, 428, 376, 175, 395, 25, 50, 255, 20], N, N],
        [[0, 540, 453, 119, 324, 25, 50, 0, 20], [0, 540, 453, 119, 324, 125, 150, 255, 20], N, N],
        [[0, 540, 453, 119, 324, 25, 50, 255, 20], [0, 540, 453, 119, 324, 25, 50, 255, 20], N, N],
        [[0, 540, 453, 119, 324, 40, 60, 0, 20], [200, 540, 453, 119, 324, 40, 60, 255, 20], N, N],
        [[0, 587, 463, 256, 423, 25, 50, 255, 50], N, N, N],
        [[0, 640, 487, 189, 384, 25, 50, 255, 50], N, N, N],
        [[0, 256, 313, 50, 100, 25, 50, 255, 50], [300, 256, 313, 131, 296, 10, 20, 255, 50], N, N],
        [[0, 256, 313, 131, 296, 25, 50, 255, 50], [400, 386, 390, 155, 413, 50, 60, 255, 32], N, N],
        [[0, 540, 453, 119, 324, 25, 50, 0, 20], N, N, N],
        [[0, 540, 453, 119, 324, 25, 50, 0, 20], [200, 626, 382, 228, 467, 10, 20, 255, 20], N, N],
        [[0, 626, 382, 228, 467, 10, 20, 255, 20], [100, -361, 809, 228, 467, 10, 20, 0, 20], N, N],
        [[0, 441, 510, 228, 467, 50, 75, 255, 20], N, N, N],
        [[0, 441, 510, 228, 467, 50, 75, 255, 20], [150, 811, 395, 228, 467, 50, 75, 255, 20], N, N],
        [[0, 640, 360, 100, 200, 10, 20, 0, 1000], N, N, N],
    ]
};

/// Effects (- 4) that stay on their first keyframe (bit set in 0x29303FFB).
const SINGLE: u32 = 0x2930_3FFB;

/// FocusLine texel origin per effect - 4 (unk_3527C0): u, v. Each line is 960 x 2 texels.
fn line_uv(idx: usize) -> (f32, f32) {
    match idx {
        2 => (0.0, 10.0),
        3 => (0.0, 40.0),
        6 => (0.0, 30.0),
        14 => (850.0, 120.0),
        _ => (0.0, 0.0),
    }
}

/// An i16 angle (a half turn per 32768) in radians.
fn rad(a: i16) -> f32 {
    f32::from(a) * std::f32::consts::PI / 32768.0
}

/// FallNote sprites (off_AE0AA8, 88 bytes each): frames, frame size, ticks per frame, base / jitter velocity,
/// jitter acceleration (y), y past which a note is gone.
struct Note {
    frames: i16,
    size: (f32, f32),
    step: i16,
    accel: f32,
}

const NOTES: [Note; 3] = [
    Note { frames: 24, size: (563.0, 187.0), step: 4, accel: 0.001 },
    Note { frames: 20, size: (225.0, 112.0), step: 4, accel: 0.0002 },
    Note { frames: 16, size: (225.0, 150.0), step: 4, accel: 0.0015 },
];
const NOTE_V: (f32, f32) = (-0.4, 4.0);
const NOTE_JITTER: f32 = 0.005;
const NOTE_FLOOR: f32 = 450.0;

/// One particle (56 bytes at word_14715DC).
#[derive(Clone, Copy, Debug)]
struct Slot {
    /// word 0: effect (lines) or note kind; -1 free
    kind: i16,
    /// notes: words 1..4 = animation tick, ticks per loop, ticks per frame, frame
    tick: i16,
    total: i16,
    step: i16,
    frame: i16,
    /// lines: word 2 = the keyframe it was rolled from; word 5 = angle (i16 of a half turn)
    key: usize,
    angle: i16,
    x: f32,
    y: f32,
    alpha: f32,
    vx: f32,
    vy: f32,
    ax: f32,
    ay: f32,
}

const FREE: Slot =
    Slot { kind: -1, tick: 0, total: 0, step: 0, frame: 0, key: 0, angle: 0, x: 0.0, y: 0.0, alpha: 0.0, vx: 0.0, vy: 0.0, ax: 0.0, ay: 0.0 };

pub struct Focus {
    /// +0x5CAC: 0 off, 1 fading in, 2 on, 3 fading out; +0x5CAD: the effect
    mode: u8,
    effect: i32,
    /// +0x5CB0 / +0x5CB4
    level: f32,
    rate: f32,
    slots: Vec<Slot>,
    /// lines: frames since the start (slot 0 word 1) and since the last re-roll (slot 1 word 1)
    clock: i16,
    since: i16,
    /// off_AE0AB0: the chance a frame drops a note
    chance: f32,
    rng: Rng,
    acc: f32,
}

impl Default for Focus {
    fn default() -> Self {
        Self {
            mode: 0,
            effect: 0,
            level: 0.0,
            rate: 1.0,
            slots: vec![FREE; 400],
            clock: 0,
            since: 0,
            chance: 0.05,
            rng: Rng::new(0x005C_AC1D),
            acc: 0.0,
        }
    }
}

impl Focus {
    pub fn active(&self) -> bool {
        self.mode != 0
    }

    /// sub_86470(effect, frames): FCLN starts `effect` (frames 0), FCSP stops it, fading out over `frames`.
    /// The effect already on keeps running.
    pub fn start(&mut self, effect: i32, frames: i32) {
        let rate = if frames > 0 { 1.0 / frames as f32 } else { 1.0 };
        match effect {
            0 if frames <= 0 => {
                self.level = 0.0;
                self.mode = 0;
                self.rate = 1.0;
            }
            0 => {
                self.mode = 3;
                self.rate = rate;
            }
            4..=33 => {
                if self.mode == 2 && self.effect == effect {
                    return;
                }
                (self.mode, self.effect, self.level, self.rate) = (1, effect, 0.0, rate);
                // the qword stores clear words 1..3 too: the line clocks in slots 0 and 1
                self.slots.fill(FREE);
                (self.clock, self.since) = (0, 0);
            }
            1..=3 => {
                (self.mode, self.effect, self.level, self.rate) = (1, effect, 0.0, rate);
                for s in &mut self.slots[..50] {
                    s.kind = -1;
                }
                self.chance = if effect == 3 { 0.1 } else { 0.05 };
            }
            _ => {}
        }
    }

    pub fn advance(&mut self, dt: f32) {
        if !self.active() {
            self.acc = 0.0;
            return;
        }
        self.acc += dt;
        while self.acc >= super::FRAME_MS && self.active() {
            self.acc -= super::FRAME_MS;
            self.step(1);
        }
    }

    /// sub_86850 for `n` frames (sub_B0190: 1, or 2 after a dropped frame).
    fn step(&mut self, n: i16) {
        let nf = f32::from(n);
        match self.mode {
            3 => {
                self.level -= self.rate * nf;
                if self.level < 0.0 {
                    self.level = 0.0;
                    self.mode = 0;
                    return;
                }
            }
            1 => {
                self.level += self.rate * nf;
                if self.level > 1.0 {
                    self.level = 1.0;
                    self.mode = 2;
                }
            }
            0 => return,
            _ => {}
        }
        match self.effect {
            4..=33 => self.step_lines(n),
            1..=3 => self.step_notes(n),
            _ => {}
        }
    }

    fn step_lines(&mut self, n: i16) {
        let idx = (self.effect - 4) as usize;
        let table = &LINES[idx];
        self.clock = self.clock.wrapping_add(n);
        if self.clock < 0 {
            self.clock = 180;
        }
        self.since = self.since.wrapping_add(n);
        let t = f32::from(self.clock) * 1000.0 / 60.0;
        // the keyframe in force and the time of the next (-1: none to blend towards)
        let t1 = table[1][0] as f32;
        let (k, next) = if t < table[0][0] as f32 || (SINGLE >> idx & 1 == 0 && t >= t1) {
            let t2 = table[2][0] as f32;
            if t >= t1 && (idx != 2 || t < t2) {
                (1, table[2][0])
            } else if t >= t2 {
                (2, table[3][0])
            } else {
                (0, table[1][0])
            }
        } else {
            (0, table[1][0])
        };
        let kf = &table[k];
        let mut alpha = kf[7] as f32;
        if next >= 0 {
            alpha += (t - kf[0] as f32) * (table[k + 1][7] - kf[7]) as f32 / (next - kf[0]) as f32;
        }
        if f32::from(self.since) * 1000.0 / 60.0 < kf[8] as f32 {
            return;
        }
        self.since = 0;
        let span = (kf[6] - kf[5]).abs().max(1);
        let mut count = (self.rng.rand() % span + kf[5]).min(400);
        if self.effect <= 8 {
            count /= 2;
        }
        let count = count.max(0) as usize;
        for i in 0..count {
            self.slots[i].key = k;
            self.roll_line(i, idx);
            self.slots[i].alpha = alpha / 255.0;
        }
        for s in &mut self.slots[count..] {
            *s = FREE;
        }
    }

    /// sub_862E0: a line from a random point of the ring around the keyframe's centre, pointing away from it.
    fn roll_line(&mut self, i: usize, idx: usize) {
        let kf = LINES[idx][self.slots[i].key];
        let angle = (self.rng.rand() % 65536 - 32768) as i16;
        let (lo, span) = (kf[3].min(kf[4]), (kf[4] - kf[3]).abs().max(1));
        let r = (self.rng.rand() % span + lo) as f32 * 0.7;
        let th = rad(angle);
        self.slots[i] = Slot {
            kind: (idx + 4) as i16,
            angle,
            x: (r * th.cos() + kf[1] as f32) * 640.0 / 1280.0,
            y: (r * th.sin() + kf[2] as f32) * 448.0 / 720.0,
            ..self.slots[i]
        };
    }

    fn step_notes(&mut self, n: i16) {
        if (self.rng.rand() as f32 * 4.6566e-10) < self.chance
            && let Some(i) = self.slots[..50].iter().position(|s| s.kind < 0)
        {
            let kind = self.rng.rand() % 3;
            self.drop_note(i, kind as usize);
        }
        let nf = f32::from(n);
        for s in self.slots[..50].iter_mut().filter(|s| s.kind >= 0) {
            if s.total >= 1 {
                let mut t = s.tick.wrapping_add(n);
                while t >= s.total {
                    t -= s.total;
                }
                s.tick = t;
                s.frame = t / s.step;
            }
            s.vy += s.ay * nf;
            s.vx += s.ax * nf;
            s.x += s.vx * nf;
            s.y += s.vy * nf;
            if s.y > NOTE_FLOOR {
                s.kind = -1;
            }
        }
    }

    /// sub_86120: a note above the top edge at a random phase of its animation.
    fn drop_note(&mut self, i: usize, kind: usize) {
        let d = &NOTES[kind];
        let total = d.frames * d.step;
        let tick = (self.rng.rand() % i32::from(total)) as i16;
        let mut jitter = || (self.rng.rand() % 16 - 8) as f32;
        let (jx, jy, jay) = (jitter(), jitter(), jitter());
        let px = (self.rng.rand() % 801 - 80) as f32;
        let py = (self.rng.rand() % 101 - 130) as f32;
        self.slots[i] = Slot {
            kind: kind as i16,
            tick,
            total,
            step: d.step,
            frame: tick / d.step,
            key: 0,
            angle: 0,
            x: px,
            y: py,
            alpha: 1.0,
            vx: NOTE_V.0 + NOTE_JITTER * jx,
            vy: NOTE_V.1 + NOTE_JITTER * jy,
            // the x acceleration's jitter scale is 0
            ax: 0.0,
            ay: d.accel * jay,
        };
    }

    /// sub_87A90(2): the lines as one mesh (sub_87150, shader default_mesh, normal blending) or the notes as
    /// sprites (sub_86FF0, blend 27). `tex`: EFFECT_PARTS FallNote0..2 and FocusLine.
    pub fn quads(&self, tex: &[TexRef; 4]) -> Vec<Quad> {
        if !self.active() {
            return Vec::new();
        }
        let (kx, ky) = (CANVAS_W as f32 / 640.0, CANVAS_H as f32 / 448.0);
        let mut out = Vec::new();
        match self.effect {
            4..=33 => {
                let t = &tex[3];
                let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
                // the scale (qword_AC0D68) starts at the native-pixel one and is only changed by the kinds
                // and angles below, so a line can inherit the previous line's
                let native = (640.0 / 1920.0, 448.0 / 1080.0);
                let (mut sx, mut sy) = native;
                for s in self.slots.iter().filter(|s| s.alpha > 0.0 && s.kind >= 0) {
                    let u = s.angle as u16;
                    let near = u.wrapping_add(7999) <= 0x3E7E;
                    let scale = match s.kind {
                        7 => Some((2.0 / 3.0, native.1 / 2.0)),
                        8 if near => Some((1.0 / 6.0, native.1)),
                        8 if u.wrapping_add(27000) >= 0xD2F1 => Some((0.4, native.1)),
                        9 | 10 => Some((0.5, native.1)),
                        13 | 19 => Some((0.4, native.1)),
                        15..=17 => Some((0.6, native.1)),
                        18 if u.wrapping_add(21000) >= 0xA411 => Some((0.433_333_34, native.1)),
                        20 if near => Some((0.4, native.1)),
                        21..=23 if u >= 0xE0C1 => Some((0.4, native.1)),
                        26 | 27 if near => Some((0.6, native.1)),
                        28..=31 => Some((0.533_333_3, native.1)),
                        32 if u.wrapping_add(27000) >= 0xD2F1 => Some((0.533_333_3, native.1)),
                        _ => None,
                    };
                    if let Some(v) = scale {
                        (sx, sy) = v;
                    }
                    let a = s.alpha * self.level;
                    let th_ = rad(s.angle);
                    let (c, n) = (th_.cos(), th_.sin());
                    let (len, wid) = (960.0 * sx, 2.0 * sy);
                    let p = |dl: f32, dw: f32| [(s.x + c * dl - n * dw) * kx, (s.y + n * dl + c * dw) * ky];
                    let (u0, v0) = line_uv(s.kind as usize - 4);
                    out.push(Quad {
                        pos: [p(0.0, 0.0), p(len, 0.0), p(len, wid), p(0.0, wid)],
                        uv: [
                            [u0 / tw, v0 / th],
                            [(u0 + 960.0) / tw, v0 / th],
                            [(u0 + 960.0) / tw, (v0 + 2.0) / th],
                            [u0 / tw, (v0 + 2.0) / th],
                        ],
                        ..Quad::rect(t.clone(), [0.0; 4], [1.0, 1.0, 1.0, a])
                    });
                }
            }
            1..=3 => {
                for s in self.slots[..50].iter().filter(|s| s.alpha > 0.0 && s.kind >= 0) {
                    let d = &NOTES[s.kind as usize];
                    let t = &tex[s.kind as usize];
                    let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
                    let (w, h) = d.size;
                    let f = f32::from(s.frame);
                    let rect = [s.x * kx, s.y * ky, w * CANVAS_W as f32 / 1920.0, h * CANVAS_H as f32 / 1080.0];
                    let a = (s.alpha * self.level).max(0.0);
                    out.push(Quad::rect(t.clone(), rect, [1.0, 1.0, 1.0, a]).with_uv_rect(f * w / tw, 0.0, (f + 1.0) * w / tw, h / th));
                }
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_follow_sub_86850() {
        let mut f = Focus::default();
        f.start(18, 0);
        // level 1 after the first frame, on after the second; the first roll after 50 ms (3 frames)
        f.step(1);
        assert_eq!((f.mode, f.level), (1, 1.0));
        f.step(1);
        assert_eq!(f.mode, 2);
        assert!(f.slots.iter().all(|s| s.kind < 0));
        f.step(1);
        let live: Vec<_> = f.slots.iter().filter(|s| s.kind >= 0).collect();
        assert!((10..15).contains(&live.len()));
        // effect 18 fades its alpha from 0 to 128 over the first 200 ms
        assert!((live[0].alpha - 50.0 / 200.0 * 128.0 / 255.0).abs() < 1e-4);
        assert!(live.iter().all(|s| s.kind == 18 && s.key == 0));
        // restarting the running effect changes nothing; FCSP(2000) fades out over 120 frames
        f.start(18, 0);
        assert_eq!(f.mode, 2);
        f.start(0, 120);
        for _ in 0..119 {
            f.step(1);
        }
        assert_eq!(f.mode, 3);
        f.step(1);
        f.step(1);
        assert_eq!(f.mode, 0);
    }

    #[test]
    fn effect_6_switches_keyframes() {
        let mut f = Focus::default();
        f.start(6, 0);
        for _ in 0..66 {
            f.step(1);
        }
        // 1100 ms: keyframe 1; the count is halved for effects up to 8
        let live: Vec<_> = f.slots.iter().filter(|s| s.kind >= 0).collect();
        assert!(live.iter().all(|s| s.key == 1));
        assert!((20..40).contains(&live.len()));
        for _ in 0..6 {
            f.step(1);
        }
        assert!(f.slots.iter().filter(|s| s.kind >= 0).all(|s| s.key == 2));
    }
}
