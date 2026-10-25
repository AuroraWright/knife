//! Witch on the Holy Night's mode panel looks: jp_4DCC0 installs objects 180..187 (group 14, table 0x3cdfd8)
//! scaled 1/3 x 0.41481, and jp_4DE10 draws MODEPNL_PARTS #0 (texture 526) at the slide's y, then the buttons
//! at (25.333 + 73.667 i, y + 6.637) through jp_AEEC0(14). The panel's state machine is Tsukihime's
//! (`ui::touch`).

use knife_render::{Quad, Renderer};

use super::super::Ui;
use super::{Pen, objtab};
use crate::game::Game;

/// The help balloons (the objects' field [32]: SYSMES 145..152).
pub const HELP: [usize; 8] = [145, 147, 149, 148, 146, 150, 151, 152];

/// A button's frame states as jp_AE840 steps them: the state drawn, the one before and its fade-out weight
/// (flag 0x20: -0.2 a frame).
#[derive(Clone, Copy, Default)]
pub struct WBtn {
    last: u8,
    prev: u8,
    w: f32,
}

impl WBtn {
    /// One 60 Hz step (`f` frames) towards `st`.
    pub fn step(&mut self, st: u8, f: f32) {
        if self.last != st {
            self.w = 1.0;
            self.prev = self.last;
            self.last = st;
        } else if self.prev == st {
            return;
        }
        self.w -= 0.2 * f;
        if self.w <= 0.0 {
            self.w = 0.0;
            self.prev = st;
        }
    }
}

/// The mode display (sub_1D670, drawn by sub_1E770 after the Saved banner) between frames: the spinner's alpha
/// (dword_648570), its rotation (dword_64856C, 65536 a turn), the frames counted while it runs (dword_648564:
/// past 120 in auto mode everything it draws fades over 60 frames) and the auto flag it last saw (dword_648560,
/// sub_1D5D0 restarts the count when auto mode comes on).
#[derive(Clone, Copy, Default)]
pub struct WSpin {
    a: f32,
    rot: f32,
    n: f32,
    auto: bool,
    /// v8: the colour alpha of the frame's draws
    fade: f32,
    rewinding: bool,
}

impl WSpin {
    /// One step of `f` 60 Hz frames: `speed` is the frame's speed word (off_632500), `rewound` whether the last
    /// rewind step went back (AE05F8, jp_814C0), `off` a choice (AE04B8 0x10) or a story movie / the staff roll
    /// (AE0650). Tsukihime's drag arrow (dword_64987C, the stretch the spinner would also follow) is never set.
    pub fn step(&mut self, f: f32, speed: f32, auto: bool, rewinding: bool, rewound: bool, off: bool) {
        if auto && !self.auto {
            self.n = 0.0;
        }
        self.auto = auto;
        self.rewinding = rewinding;
        let faded = |n: f32| if n > 120.0 { 1.0 - (n - 120.0) / 60.0 } else { 1.0 };
        let on = if off || (!auto && speed < 2.0) {
            self.fade = faded(self.n);
            false
        } else {
            let d = 60.0 * f * speed;
            let on = if rewinding {
                self.rot -= d;
                rewound
            } else {
                self.rot += d;
                true
            };
            self.rot = self.rot.rem_euclid(65536.0);
            let n = self.n + speed * f;
            if speed > 1.0 || rewinding {
                self.n = n.min(120.0);
                self.fade = 1.0;
            } else {
                self.n = n.min(180.0);
                self.fade = faded(self.n);
            }
            on
        };
        let rate = 0.05 * f * if rewinding { 5.0 } else { 1.0 };
        self.a = if on { (self.a + rate).min(1.0) } else { (self.a - rate).max(0.0) };
    }

    /// The backlog runs (AE05A0): sub_1D670 only zeroes the alpha.
    pub fn hide(&mut self) {
        self.a = 0.0;
    }
}

impl Ui {
    /// sub_1D670's draw in the 640x448 space: the spinner (DISP_PARTS 32, 33 while rewinding: image slots 82 / 83)
    /// at (600, 27.77), centred, turned by its rotation and growing in with its alpha ((1 - (1 - a)^2) x 0.4225 of
    /// its 1280x720 size); in auto mode mode_disp's play cell (DISP_PARTS 4, slot 54) at (597.75, 27.772); the mode
    /// icon's cell (unk_3AF4B2) at flt_25E8A4[icon] (at 0.6 in skip mode, 5), both at 0.21667 x 0.26963.
    #[allow(clippy::approx_constant)]
    pub(in crate::ui) fn wmode_draw(&self, out: &mut Vec<Quad>, s: &WSpin, icon: usize) {
        const CELL: [[f32; 4]; 6] =
            [[176.0, 0.0, 254.0, 78.0], [0.0, 0.0, 78.0, 78.0], [88.0, 0.0, 166.0, 78.0], [264.0, 0.0, 342.0, 78.0], [352.0, 0.0, 430.0, 78.0], [264.0, 0.0, 342.0, 78.0]];
        const X: [f32; 6] = [597.75, 570.45, 570.45, 625.05, 625.05, 625.05];
        let mut pen = Pen::default();
        if s.a > 0.0
            && let Some(t) = self.w.tex.get(if s.rewinding { 83 } else { 82 })
        {
            pen.alpha(s.fade * s.a);
            let k = (1.0 - (1.0 - s.a) * (1.0 - s.a)) * 0.65 * 0.65;
            pen.scale(640.0 * k / 1280.0, 448.0 * k / 720.0);
            let angle = (s.rot as u16 as i16) as f32 * 3.1416 * 0.000030518;
            pen.spr_rot(out, &t, 0x34, None, 597.75, 27.771_85, angle);
        }
        let Some(t) = self.w.tex.get(54) else { return };
        pen.scale(0.21667, 0.26963);
        pen.alpha(s.fade);
        if s.auto {
            pen.spr(out, &t, 20, Some(CELL[0]), X[0], 27.772);
        }
        if (1..=5).contains(&icon) {
            if icon == 5 {
                pen.alpha(0.6);
            }
            pen.spr(out, &t, 20, Some(CELL[icon]), X[icon], 27.772);
        }
    }

    /// jp_4DCC0's resources (jp_9A940(&off_3CDF90)): the window and button atlas.
    pub(in crate::ui) fn wmp_load(&mut self, game: &Game, r: &Renderer) {
        if self.w.tex.get(526).is_none() {
            self.w.tex.load(game, r, objtab::RES_MODEPNL);
        }
        // the mode display's DISP_PARTS (installed at boot with the message windows, jp_3B9A0)
        if self.w.tex.get(82).is_none() {
            self.w.tex.load(game, r, objtab::RES_DISP);
        }
    }

    /// jp_4DE10 at the slide's y (640x448 units): the window, then each button's state with the state before
    /// fading out on top.
    pub(in crate::ui) fn wmp_draw(&self, out: &mut Vec<Quad>, y: f32, btn: &[WBtn; 8]) {
        let mut pen = Pen::default();
        pen.scale(0.33333, 0.41481);
        if let Some(t) = self.w.tex.get(526) {
            pen.spr(out, &t, 16, None, 0.0, y);
        }
        for (i, o) in objtab::MODEPNL.iter().enumerate().take(8) {
            let b = btn[i];
            let pos = [25.333 + 73.667 * i as f32, y + 6.637];
            let mut draw = |st: u8, a: f32| {
                for f in o.states[usize::from(st)] {
                    let Some(t) = self.w.tex.get(f.tex) else { continue };
                    pen.color(a * (f.color >> 24) as f32 / 255.0, 1.0, 1.0, 1.0);
                    pen.spr(out, &t, f.flags, Some(f.src.map(f32::from)), pos[0] + f.d[0], pos[1] + f.d[1]);
                }
            };
            draw(b.last, 1.0);
            if b.w > 0.0 && b.prev != b.last {
                draw(b.prev, b.w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WSpin;

    fn run(s: &mut WSpin, frames: usize, speed: f32, auto: bool, rewinding: bool, rewound: bool) {
        for _ in 0..frames {
            s.step(1.0, speed, auto, rewinding, rewound, false);
        }
    }

    #[test]
    fn spinner_follows_the_speed_auto_and_rewind() {
        // ZR held, still at speed 1: nothing
        let mut s = WSpin::default();
        run(&mut s, 10, 1.0, false, false, false);
        assert_eq!(s.a, 0.0);
        // a skip: in over 20 frames, turning 60 x speed a frame, never faded
        run(&mut s, 20, 4.0, false, false, false);
        assert!((s.a - 1.0).abs() < 1e-4 && s.fade == 1.0);
        assert_eq!(s.rot, 20.0 * 240.0);
        // a rewind going back: 5 x faster, the other way; the ring run out: out again
        let mut s = WSpin::default();
        run(&mut s, 4, 4.0, false, true, true);
        assert!((s.a - 1.0).abs() < 1e-4);
        assert_eq!(s.rot, 65536.0 - 4.0 * 240.0);
        run(&mut s, 4, 4.0, false, true, false);
        assert_eq!(s.a, 0.0);
        // auto mode: the count restarts when it comes on; past 120 frames everything fades over 60
        let mut s = WSpin { n: 170.0, ..WSpin::default() };
        run(&mut s, 120, 1.0, true, false, false);
        assert_eq!(s.fade, 1.0);
        run(&mut s, 30, 1.0, true, false, false);
        assert!((s.fade - 0.5).abs() < 1e-4);
        run(&mut s, 60, 1.0, true, false, false);
        assert_eq!(s.fade, 0.0);
    }
}
