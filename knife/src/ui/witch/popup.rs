//! The message popup (jp_4ABE0 opens it, jp_49750 runs it, jp_48540 draws it): dialog_win with a SYSMES line
//! and no buttons, in two slots that take turns like the dialog's. A button (A / B) or a tap closes it
//! unless the caller holds it (jp_4ABD0(1)) and closes it itself (jp_4ACD0).

use knife_render::{Quad, Renderer};

use super::super::{H, Ui};
use super::MASK;
use crate::game::Game;

#[derive(Default, Clone)]
struct Slot {
    /// byte_67A548[128 + 4 slot]: 0 off, 1 opening, 2 open, 3 closing
    st: u8,
    /// [136 + 4 slot]: the alpha (0.07 a frame)
    a: f32,
    /// [144 + 4 slot]: frames left (100000 and up: no limit)
    frames: i32,
    msg: String,
}

#[derive(Default)]
pub struct WPopup {
    slots: [Slot; 2],
    /// jd_B34B40
    cur: usize,
    /// jd_B34B44 (jp_4AD30): 1 opening, 2 open, 0 closed
    pub busy: u8,
    /// jd_B34B3C (jp_4ABD0)
    held: bool,
}

impl Ui {
    /// jp_4ABE0(id, frames): SYSMES `id` through dword_286908 (an id it has no line for shows an empty window).
    pub(in crate::ui) fn wp_open(&mut self, game: &Game, id: i32, frames: i32) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let msg = usize::try_from(id - 159)
            .ok()
            .and_then(|k| POPUP_SYSMES.get(k))
            .and_then(|&m| usize::try_from(m).ok())
            .and_then(|m| self.sysmes.get(m).cloned())
            .unwrap_or_default();
        let p = &mut self.w.popup;
        if !matches!(p.slots[p.cur].st, 0 | 3) {
            p.slots[p.cur].st = 3;
            p.held = false;
        }
        p.busy = 1;
        self.w.objs.range = (-1, -1);
        p.cur ^= 1;
        p.slots[p.cur] = Slot { st: 1, a: 0.0, frames, msg };
        self.w.objs.help(-1, false);
    }

    /// jp_4ABD0(1)
    pub(in crate::ui) fn wp_hold(&mut self) {
        self.w.popup.held = true;
    }

    /// jp_4ACD0
    pub(in crate::ui) fn wp_close(&mut self) {
        let p = &mut self.w.popup;
        let o = p.cur ^ 1;
        if !matches!(p.slots[o].st, 0 | 3) {
            p.slots[o].st = 3;
        }
        p.slots[p.cur].st = 3;
        self.w.objs.range = (-1, 373);
        p.busy = 0;
        p.held = false;
    }

    /// jp_49750, one 60 Hz frame.
    pub(in crate::ui) fn wp_step(&mut self) {
        for k in 0..2 {
            let p = &mut self.w.popup;
            let s = &mut p.slots[k];
            match s.st {
                3 => {
                    s.a -= 0.07;
                    if s.a <= 0.0 {
                        *s = Slot::default();
                    }
                }
                2 => {
                    p.busy = 2;
                    if s.frames < 100000 {
                        s.frames -= 1;
                    }
                    if !p.held
                        && (s.frames < 1 || self.w.inp.pressed & (MASK[0] | MASK[1]) != 0 || self.w.inp.touch & 0x2008 != 0)
                    {
                        p.busy = 0;
                        p.held = false;
                        s.st = 3;
                        self.w.objs.range = (-1, 373);
                    }
                    self.w.objs.help(-1, false);
                }
                1 => {
                    s.a += 0.07;
                    if s.a >= 1.0 {
                        s.a = 1.0;
                        s.st = 2;
                    }
                }
                _ => {}
            }
        }
    }

    /// jp_48540: the current slot, then the other, each dialog_win (485) from (320, 139.79) at scale
    /// (1 - 0.2 (1 - a))^2 and alpha a, with its line (text slot 10 + slot, 0.6 x 0.7, centred) from y 190.79
    /// once fully open. Nothing while the current slot is off.
    pub(in crate::ui) fn wp_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let cur = self.w.popup.cur;
        if self.w.popup.slots[cur].st == 0 {
            return;
        }
        for k in [cur, cur ^ 1] {
            let s = self.w.popup.slots[k].clone();
            if s.st == 0 {
                continue;
            }
            let sc = 1.0 - (1.0 - s.a) * 0.2;
            let mut pen = super::Pen::default();
            pen.scale(sc * sc * 0.33333, sc * sc * 0.41481);
            pen.alpha(s.a);
            if let Some(t) = self.w.tex.get(485) {
                pen.spr(out, &t, 0x11, None, 320.0, 139.79259);
            }
            if s.a == 1.0 {
                self.draw_dialog_text(game, r, out, &s.msg, 190.79 * H / 448.0, 4.0);
            }
        }
    }
}

/// dword_286908: the popup's SYSMES line per id 159.. (-1: none)
const POPUP_SYSMES: [i32; 33] = [
    159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, -1, -1, -1, -1, -1, -1, -1, 177, 178, 179, 180, 181, 182,
    183, 184, 185, 186, -1, -1, 189, 190, 191,
];
