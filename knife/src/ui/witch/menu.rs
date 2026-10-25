//! The system menu over the story (Y): sub_2D7D0 installs objects 170..174 (group 1: Save, Load, Config,
//! Title, Back) from the table at 0x3CDE70, sub_2D940 runs it (state qword_67A5F8, cursor in its high half,
//! alpha qword_67A600 at 1/12 a frame) and jp_4C370 draws it: the blurred scene (sub_7FCA0 / sub_7FDB0 at
//! alpha a^2), black at 0.8 a, the window (sub_2E220), the SYSTEM title, the entries and the cursor sparkle.
//! The save screen (states 4 / 5) and the config (7 / 8) run inside it.

use knife_render::{Blend, Quad, RenderTarget, Renderer, Shade};

use super::super::{H, Ui, W};
use super::{DOWN, LEFT, MASK, RIGHT, UP, objtab};
use crate::game::Game;

/// unk_2617E0 / unk_2617F4: the cursor sparkle per entry (640x448)
const CUR_X: [f32; 5] = [500.0, 500.0, 500.0, 500.0, 497.6667];
const CUR_Y: [f32; 5] = [71.348145, 116.97778, 162.6074, 208.23703, 382.45923];
/// the entries' rows (jp_4C370)
const ROW_Y: [f32; 5] = [66.37, 112.0, 157.63, 203.26, 371.67];
const ENTRY_X: f32 = 501.33;
const SX: f32 = 0.33333;
const SY: f32 = 0.41481;

#[derive(Default)]
pub struct WMenu {
    /// LODWORD(qword_67A5F8)
    pub st: u32,
    /// HIDWORD(qword_67A5F8)
    pub cur: i32,
    /// LODWORD(qword_67A600)
    pub a: f32,
    /// HIDWORD(qword_67A600): the cursor sparkle's frame (0..40, half a frame per frame)
    anim: f32,
    installed: bool,
    /// the window's offscreen surfaces (jp_A5C10's "backbuffer surface HH" 0 / 1)
    rt: Option<[knife_render::TexRef; 2]>,
}

impl Ui {
    /// sub_40ED0's Y (mask [10]): the menu opens with sound 10 (the menu task's state is 2 at alpha 0).
    pub(in crate::ui) fn wm_open(&mut self, game: &mut Game, r: &Renderer) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        if !self.w.menu.installed {
            // sub_2D7D0
            self.w.menu.installed = true;
            self.w.tex.load(game, r, objtab::RES_MENU);
            self.w.objs.install(objtab::MENU);
            for id in 170..=174 {
                self.w.objs.scale(id, SX, SY);
            }
            self.w.menu.st = 2;
            self.w.menu.a = 0.0;
        }
        game.play_sys_sound(10);
        self.push(super::super::Screen::Witch);
        self.w.scr = super::WScr::Menu;
    }

    /// sub_2D8A0: Save / Load take flag 2 while saving is off (F025); Title takes its other frames and help
    /// (144 instead of 142) while F024 is set.
    fn wm_flags(&mut self, game: &Game) {
        let off = game.var("F025") != 0;
        let (set, clear) = if off { (2, 0) } else { (0, 2) };
        self.w.objs.flags(170, set, clear);
        self.w.objs.flags(171, set, clear);
        let f24 = game.var("F024") != 0;
        self.w.objs.set_states(173, if f24 { &objtab::MENU_TITLE_NG } else { &objtab::MENU_TITLE_OK });
        if let Some(o) = self.w.objs.get(173) {
            o.help = if f24 { 144 } else { 142 };
        }
    }

    /// sub_2D940, one 60 Hz frame. Returns true when the story ends for the title (state 15).
    pub(in crate::ui) fn wm_step(&mut self, game: &mut Game, r: &Renderer) -> bool {
        let f = 1.0;
        let off = game.var("F025") != 0;
        let m = &mut self.w.menu;
        match m.st {
            0..=2 => {
                if m.a == 0.0 {
                    self.wm_flags(game);
                    self.w.objs.reset_hit();
                }
                let m = &mut self.w.menu;
                m.a += f * 0.083333;
                if m.a < 1.0 {
                    return false;
                }
                m.a = 1.0;
                m.st = 3;
                self.w.objs.group_flags(1, 1, 0);
                self.wm_flags(game);
            }
            3 => self.wm_input(game, r, off),
            4 => {
                m.a -= f * 0.083333;
                if m.a > 0.0 {
                    return false;
                }
                m.a = 0.0;
                m.st = 5;
                self.w.objs.group_flags(1, 0, 1);
                let save = self.w.menu.cur == 0;
                self.ws_open(game, r, save);
            }
            5 => match self.ws_step(game, r) {
                2 => {
                    // a record was loaded: the menu is gone with the story it was over
                    let m = &mut self.w.menu;
                    m.a = 0.0;
                    m.st = 2;
                    self.w.objs.group_flags(1, 0, 1);
                    self.w.scr = super::WScr::None;
                }
                3 => self.w.menu.st = 6,
                _ => {}
            },
            6 | 9 | 12 => {
                m.a += f * 0.083333;
                if m.a < 1.0 {
                    return false;
                }
                m.a = 1.0;
                m.st = 3;
                self.w.objs.group_flags(1, 1, 0);
            }
            7 => {
                m.a -= f * 0.083333;
                if m.a > 0.0 {
                    return false;
                }
                m.a = 0.0;
                m.st = 8;
                self.wc_open(game, r);
                self.w.objs.group_flags(1, 0, 1);
            }
            8 => {
                if self.wc_step(game, r) == 1 {
                    self.w.menu.st = 9;
                }
            }
            13 => {
                // jp_AD440(320, 224, 256): the pointer goes back to the middle (also the story's last touch, so
                // a tap on 戻る low down does not leave the window hidden)
                self.w.inp.pos = [320.0, 224.0];
                self.touch.at = [320.0, 224.0];
                m.a -= f * 0.083333;
                if m.a > 0.0 {
                    return false;
                }
                m.a = 0.0;
                m.st = 2;
                self.w.objs.group_flags(1, 0, 1);
                self.w.scr = super::WScr::None;
            }
            14 => {
                if self.w.dlg.busy() {
                    return false;
                }
                // No: back to the entries; Yes: the system data write (jp_87BA0 / jp_87C10), then the fade
                if self.w.dlg.answer != 0 {
                    self.w.menu.st = 3;
                } else {
                    self.store_global(game);
                    self.w.menu.st = 15;
                }
            }
            15 => {
                m.a -= f * 0.083333;
                if m.a > 0.0 {
                    return false;
                }
                m.a = 0.0;
                m.st = 2;
                self.w.objs.group_flags(1, 0, 1);
                self.w.scr = super::WScr::None;
                return true;
            }
            19 => {
                // the Saved banner: A, a tap or a mouse button sends it away (jp_3B480); then the fade
                if self.w.inp.pressed & MASK[0] != 0 || self.w.inp.touch & 2 != 0 {
                    self.skip_banner();
                }
                if self.banner.is_none() {
                    self.w.menu.st = 15;
                }
            }
            _ => {}
        }
        false
    }

    /// sub_2D940 state 3.
    fn wm_input(&mut self, game: &mut Game, r: &Renderer, off: bool) {
        let hit = self.w.objs.hit;
        let cur = self.w.menu.cur;
        let mut moved = false;
        if !(170..=177).contains(&hit) {
            self.w.objs.focus = cur + 170;
        } else if (self.w.inp.touch & 2 != 0 || self.w.inp.hovering()) && cur != hit - 170 {
            self.w.menu.cur = hit - 170;
            game.play_sys_sound(2);
            moved = true;
        }
        let p = self.w.inp.pressed;
        let rep = self.w.inp.repeat;
        if p & MASK[11] != 0 || self.w.inp.flick & 6 != 0 {
            self.w.zl_block = true;
            game.play_sys_sound(1);
            self.w.menu.st = 13;
            self.w.inp.pos[1] = 398.0;
            self.touch.at[1] = 398.0;
            return;
        }
        let cur = self.w.menu.cur;
        if p & MASK[0] != 0 || (!moved && self.w.inp.touch & 2 != 0 && hit == cur + 170) {
            match cur {
                0 | 1 if off => game.play_sys_sound(1),
                0 | 1 => {
                    game.play_sys_sound(0);
                    self.w.menu.st = 4;
                }
                2 => {
                    game.play_sys_sound(0);
                    self.w.menu.st = 7;
                }
                3 => {
                    let f24 = game.var("F024") != 0;
                    if off || f24 {
                        self.wd_open(game, r, if f24 { 12 } else { 8 }, !off, false);
                        self.w.menu.st = 14;
                    } else {
                        game.play_sys_sound(0);
                        // jp_52A20: nothing changed since the resume record was written -> straight out
                        let same = game
                            .save_data()
                            .zip(self.slots.get(crate::save::auto_first()).and_then(Option::as_ref))
                            .is_some_and(|(a, b)| a.scene == b.scene && a.pc == b.pc);
                        if same {
                            self.store_global(game);
                            self.w.menu.st = 15;
                        } else {
                            // sub_1D4A0(1) the Saved banner, sub_35790(1) the resume record, sound 13
                            self.show_banner(1);
                            if game.can_save() {
                                self.write_quiet(game, r, crate::save::auto_first());
                            } else {
                                self.store_global(game);
                            }
                            self.save_cool = 180;
                            game.play_sys_sound(13);
                            self.w.menu.st = 19;
                        }
                    }
                }
                4 => {
                    game.play_sys_sound(1);
                    self.w.menu.st = 13;
                }
                _ => {}
            }
            self.w.objs.press(cur + 170, 10);
            return;
        }
        let step = |ui: &mut Self, game: &mut Game, c: i32| {
            ui.w.menu.cur = c;
            ui.w.objs.focus = c + 170;
            game.play_sys_sound(2);
        };
        if rep & UP != 0 || p & MASK[6] != 0 {
            if cur >= 1 {
                step(self, game, cur - 1);
            } else if p & UP != 0 || p & MASK[6] != 0 {
                step(self, game, 4);
            }
        } else if rep & DOWN != 0 || p & MASK[7] != 0 {
            if cur <= 3 {
                step(self, game, cur + 1);
            } else if p & DOWN != 0 || p & MASK[7] != 0 {
                step(self, game, 0);
            }
        } else if p & LEFT != 0 || p & MASK[8] != 0 {
            if cur >= 1 {
                step(self, game, 0);
            }
        } else if (p & RIGHT != 0 || p & MASK[9] != 0) && cur <= 3 {
            step(self, game, 4);
        }
    }

    /// The blurred backdrop's alpha this frame (jp_4C370: a^2, the alpha held at 1 under the save screen and the
    /// config).
    pub(in crate::ui) fn wm_blur(&self) -> f32 {
        let m = &self.w.menu;
        let v1 = if (4..10).contains(&m.st) { 1.0 } else { m.a };
        v1 * v1
    }

    /// jp_4C370.
    pub(in crate::ui) fn wm_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let m = &self.w.menu;
        let (st, a) = (m.st, m.a);
        let v1 = if (4..10).contains(&st) { 1.0 } else { a };
        out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, v1 * 0.8]));
        // sub_302A0 under the save screen's states
        if st == 5 {
            self.ws_draw(game, r, out);
        }
        // sub_BD90 (layer 21) while the config is up
        self.wc_draw(game, r, out);
        if a <= 0.0 {
            return;
        }
        let closing = st <= 15 && (1 << st) & 0xA490 != 0;
        let opening = st <= 15 && (1 << st) & 0x1244 != 0;
        let v9 = if closing || opening { a } else { 1.0 };
        let v4 = if opening { 1.0 - a } else { -1.0 };
        self.wm_window(game, r, out, v9, v4);
        let mut pen = super::Pen::default();
        pen.scale(SX, SY);
        let tx = if opening { 453.33334 + (1.0 - a) * 10.0 } else { 453.33334 };
        pen.alpha(if closing || opening { a } else { 1.0 });
        if let Some(t) = self.w.tex.get(398) {
            pen.spr(out, &t, 16, None, tx, 13.274074);
        }
        let objs = &mut self.w.objs;
        if closing {
            for (i, y) in ROW_Y.iter().enumerate() {
                objs.place(170 + i as i32, ENTRY_X, y + (1.0 - a) * 4.0, a);
            }
            objs.help(-1, false);
        } else if opening {
            for i in 0..4 {
                let v = (2.0 * a - i as f32 / 3.0).clamp(0.0, 1.0);
                objs.place(170 + i, ENTRY_X, (1.0 - v) * 19.0 * SY + ROW_Y[i as usize], v);
                objs.scale(170 + i, SX, (v + 1.0) * 0.5 * SY);
            }
            objs.place(174, ENTRY_X, ROW_Y[4], a);
            objs.scale(174, SX, SY);
        } else {
            for i in 0..5 {
                objs.place(170 + i, ENTRY_X, ROW_Y[i as usize], 1.0);
                objs.scale(170 + i, SX, SY);
            }
        }
        let tex = &self.w.tex;
        self.w.objs.draw(&super::Pen::default(), 1, out, &|id| tex.get(id));
        let m = &mut self.w.menu;
        if m.cur > 4 {
            return;
        }
        m.anim += 0.5;
        if m.anim >= 40.0 {
            m.anim -= 40.0;
        }
        let k = m.anim as i32;
        let src = [((k % 20) * 64) as f32, ((k / 20) * 64) as f32, ((k % 20) * 64 + 64) as f32, ((k / 20) * 64 + 64) as f32];
        let c = m.cur as usize;
        let (ca, y) = if closing {
            (a, CUR_Y[c] + (1.0 - a) * 4.0)
        } else if opening {
            (if c != 4 { (2.0 * a - c as f32 / 3.0).clamp(0.0, 1.0) } else { a }, CUR_Y[c])
        } else {
            (1.0, CUR_Y[c])
        };
        pen.alpha(ca);
        if let Some(t) = self.w.tex.get(399) {
            pen.spr(out, &t, 16, Some(src), CUR_X[c], y);
        }
        // jp_AE590: the help balloon of the focused entry
        self.wm_help(game, r, out);
    }

    /// sub_2E220(v): menu_window_o (397, shader noalpha) into surface 1; kusa1 / kusa2 (400 / 401, the first
    /// narrowed and the second shortened by v while opening) into surface 0; surface 0 added onto surface 1
    /// (sub_81C20(1)); menu_window (396) into surface 0 for its alpha, and surface 1 drawn through it
    /// (sub_8A4E0(0) / jp_B2900). All at the menu's alpha.
    fn wm_window(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, a: f32, v: f32) {
        let [t0, t1] = self.w.menu.rt.get_or_insert_with(|| {
            let mk = |l| r.create_target(W as u32, H as u32, l).tex;
            [mk("menu surface 0"), mk("menu surface 1")]
        });
        let (t0, t1) = (t0.clone(), t1.clone());
        let tex = |id| self.w.tex.get(id);
        let mut pen = super::Pen::default();
        pen.scale(SX, SY);
        pen.alpha(a);
        let mut s1 = Vec::new();
        if let Some(t) = tex(397) {
            pen.spr(&mut s1, &t, 16, None, 406.66666, 0.0);
            if let Some(q) = s1.last_mut() {
                q.blend = Blend::Opaque;
                q.shade = Shade::NoAlpha;
            }
        }
        let mut s0 = Vec::new();
        if let Some(t) = tex(400) {
            let mut p = pen;
            if v >= 0.0 {
                p.scale((v * -100.0 + 640.0) / 1920.0, SY);
            }
            p.spr(&mut s0, &t, 18, None, 640.0, -47.289);
        }
        if let Some(t) = tex(401) {
            let mut p = pen;
            if v >= 0.0 {
                p.scale(SX, (v * -50.0 + 448.0) / 1080.0);
            }
            p.spr(&mut s0, &t, 24, None, 640.0, 448.0);
        }
        for q in &mut s0 {
            q.blend = Blend::Over;
        }
        let mut add = Quad::rect(t0.clone(), [0.0, 0.0, W, H], [1.0; 4]);
        add.blend = Blend::Add;
        s1.push(add);
        if let Some(t) = tex(396) {
            pen.spr(&mut s1, &t, 16, None, 406.66666, 0.0);
            if let Some(q) = s1.last_mut() {
                q.blend = Blend::MaskAlpha;
            }
        }
        game.ui_passes.push((RenderTarget { tex: t0 }, Some([0.0; 4]), s0));
        game.ui_passes.push((RenderTarget { tex: t1.clone() }, Some([0.0; 4]), s1));
        out.push(Quad::rect(t1, [0.0, 0.0, W, H], [1.0, 1.0, 1.0, a]));
    }

    /// jp_AE590 for the menu's objects.
    pub(in crate::ui) fn wm_help(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let o = &self.w.objs;
        let Some(obj) = usize::try_from(o.help_obj).ok().and_then(|i| o.o[i].as_ref()) else { return };
        if o.help_id < 1 || obj.flags & 1 == 0 {
            return;
        }
        let a = obj.alpha * o.help_a;
        let rc = obj.rect.map(f32::from);
        let (id, pos, flags) = (o.help_id as usize, obj.pos, obj.flags);
        self.draw_help_at(game, r, out, id, pos, rc, flags, a);
    }
}
