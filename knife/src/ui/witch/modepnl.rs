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

impl Ui {
    /// jp_4DCC0's resources (jp_9A940(&off_3CDF90)): the window and button atlas.
    pub(in crate::ui) fn wmp_load(&mut self, game: &Game, r: &Renderer) {
        if self.w.tex.get(526).is_none() {
            self.w.tex.load(game, r, objtab::RES_MODEPNL);
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
