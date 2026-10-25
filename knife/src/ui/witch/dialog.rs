//! The Yes / No dialog (jp_4AD40 opens it, jp_49F00 runs it, jp_4AE90 draws it): two slots that take turns, so a
//! dialog can open while the last one still fades. Objects 0 (Yes) / 1 (No), group 15 (jp_47BF0 installs them
//! scaled 1/3 x 0.41481); the message is SYSMES[dword_2868A0[id]] in text slot 12 + slot.

use knife_render::{Quad, Renderer};

use super::super::{Confirm, H, Screen, Ui, W};
use super::{MASK, WScr, objtab};
use crate::game::Game;

#[derive(Default, Clone)]
struct Slot {
    /// byte_67A548[160 + 4 slot]: 0 off, 1 opening, 2 open, 3 answered, 4 closing
    st: u8,
    /// [168 + 4 slot]: the alpha (0.07 a frame)
    a: f32,
    /// [152 + 4 slot]: the quick record's thumbnail under the message (jp_4B4A0)
    thumb: bool,
    msg: String,
}

#[derive(Default)]
pub struct WDlg {
    slots: [Slot; 2],
    /// jd_B34B50: the slot the last dialog took
    cur: usize,
    /// jd_B34B54: 1 opening, 2 open, 0 answered (jp_496F0)
    pub active: u8,
    /// LODWORD(jd_B34B58): the button (0 Yes, 1 No), the answer once answered (jp_49700)
    pub answer: i32,
    /// HIDWORD(jd_B34B58): frames until an answered dialog lets go
    hold: i32,
    installed: bool,
}

impl WDlg {
    /// jp_496F0
    pub fn busy(&self) -> bool {
        self.active != 0
    }

    /// Whether a slot is up (for the screens that stop drawing their input meanwhile)
    pub fn shown(&self) -> bool {
        self.slots.iter().any(|s| s.st != 0)
    }
}

impl Ui {
    /// jp_49710(id, no): dialog `id` (the dword_2868A0 row), the cursor on No when `no`.
    pub(in crate::ui) fn wd_open(&mut self, game: &mut Game, r: &Renderer, id: usize, no: bool, thumb: bool) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        self.wd_install(game, r);
        let msg = super::super::WITCH_DIALOG_SYSMES.get(id).and_then(|&m| self.sysmes.get(m).cloned()).unwrap_or_default();
        let d = &mut self.w.dlg;
        if d.slots[d.cur].st & !4 != 0 {
            d.slots[d.cur].st = 4;
        }
        d.active = 1;
        self.w.objs.range = (0, 1);
        d.cur ^= 1;
        d.slots[d.cur] = Slot { st: 1, a: 0.0, thumb, msg };
        d.answer = i32::from(no);
        game.play_sys_sound(7);
        self.w.objs.focus = d.answer;
        self.w.objs.help(-1, false);
    }

    /// A story dialog (jp_40ED0's dialogs 10 / 11 for the mode panel's next / previous scene, 9 for its quick
    /// load and Minus, with the quick record's thumbnail: jp_89590 + jp_4B4A0(1)): the story waits on
    /// jp_496F0 with the mode panel still drawn under the dialog, then acts on Yes (jp_49700() == 0).
    pub(in crate::ui) fn wd_ask(&mut self, game: &mut Game, r: &Renderer, c: Confirm) {
        let Some((id, no)) = c.dialog() else { return };
        self.wd_open(game, r, id, no, c == Confirm::QuickLoad);
        self.w.scr = WScr::Ask(c);
        self.push(Screen::Witch);
    }

    /// jp_47BF0's part: the buttons (objects 0 / 1) and their texture.
    pub(in crate::ui) fn wd_install(&mut self, game: &Game, r: &Renderer) {
        if self.w.dlg.installed {
            return;
        }
        self.w.dlg.installed = true;
        self.w.tex.load(game, r, objtab::RES_DIALOG);
        self.w.objs.install(objtab::DIALOG);
        for id in 0..2 {
            self.w.objs.scale(id, 0.33333, 0.41481);
        }
    }

    /// jp_49F00, one 60 Hz frame of both slots.
    pub(in crate::ui) fn wd_step(&mut self, game: &mut Game) {
        for k in 0..2 {
            let d = &mut self.w.dlg;
            match d.slots[k].st {
                1 => {
                    d.slots[k].a += 0.07;
                    if d.slots[k].a >= 1.0 {
                        d.slots[k].a = 1.0;
                        d.slots[k].st = 2;
                        self.w.objs.group_flags(15, 1, 0);
                    }
                }
                2 => {
                    d.active = 2;
                    let hit = self.w.objs.hit;
                    let mut moved = false;
                    if !(0..=1).contains(&hit) {
                        self.w.objs.focus = d.answer;
                        moved = true;
                    } else if (self.w.inp.touch & 2 != 0 || self.w.inp.hovering()) && d.answer != hit {
                        // (a tap on the other button only moves the answer there, the next tap answers;
                        // knife: the mouse hovering onto a button moves it as the pad does)
                        game.play_sys_sound(2);
                        d.answer = hit;
                        moved = true;
                    }
                    let p = self.w.inp.pressed;
                    let t = self.w.inp.touch;
                    if p & MASK[0] != 0 || (!moved && t & 2 != 0) {
                        game.play_sys_sound(0);
                        d.hold = 15;
                        self.w.objs.press(d.answer, 10);
                        d.slots[k].st = 3;
                    } else if p & MASK[1] != 0 || t & 8 != 0 {
                        self.w.zl_block = true;
                        game.play_sys_sound(1);
                        d.answer = 1;
                        d.hold = 1;
                        d.slots[k].st = 3;
                    } else if self.w.inp.wheel != 0 || p & 0xF0_00F0 != 0 {
                        game.play_sys_sound(2);
                        d.answer = i32::from(d.answer == 0);
                        self.w.objs.focus = d.answer;
                    }
                }
                3 => {
                    d.hold -= 1;
                    if d.hold < 1 {
                        d.active = 0;
                        self.w.objs.range = (-1, 373);
                        d.slots[k].st = 4;
                        self.w.objs.group_flags(15, 0, 1);
                    }
                }
                4 => {
                    d.slots[k].a -= 0.07;
                    if d.slots[k].a <= 0.0 {
                        d.slots[k] = Slot::default();
                    }
                }
                _ => {}
            }
        }
    }

    /// jp_4AE90: each shown slot (the last opened first) as dialog_win (texture 485, centred at x 320 from y
    /// 139.79, scale s^2 / 3 x 0.41481 s^2 with s = 1 + 0.2 (1 - a) opening, then shrinking after Yes and growing
    /// after No) at alpha a; once open the message from y 177.79 (153.79 over a thumbnail) and the buttons at
    /// (330 / 252, 229.93).
    pub(in crate::ui) fn wd_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let mut k = self.w.dlg.cur;
        for _ in 0..2 {
            let s = self.w.dlg.slots[k].clone();
            if s.st != 0 {
                let a = s.a;
                let d = (1.0 - a) * 0.2;
                let sc = if s.st == 1 { 1.0 + d } else if self.w.dlg.answer == 0 { 1.0 - d } else { 1.0 + d };
                let mut pen = super::Pen::default();
                pen.scale(sc * sc * 0.33333, sc * sc * 0.41481);
                pen.alpha(a);
                if let Some(t) = self.w.tex.get(485) {
                    pen.spr(out, &t, 0x11, None, 320.0, 139.79259);
                }
                if a == 1.0 {
                    let y = if s.thumb { 153.79259 } else { 177.79259 };
                    self.draw_dialog_text(game, r, out, &s.msg, y * H / 448.0, 0.0);
                    if s.thumb {
                        self.draw_dialog_thumb(game, r, out);
                    }
                }
                if s.st & !1 == 2 {
                    self.w.objs.place(0, 330.0, 229.92941, 1.0);
                    self.w.objs.place(1, 252.0, 229.92941, 1.0);
                    let tex = &self.w.tex;
                    self.w.objs.draw(&super::Pen::default(), 15, out, &|id| tex.get(id));
                }
            }
            k ^= 1;
        }
        let _ = W;
    }
}
