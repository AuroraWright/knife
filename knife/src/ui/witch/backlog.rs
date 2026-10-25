//! The backlog over the story (X, a right flick from the left edge, the mode panel's button): jp_40ED0 lists
//! the ring's messages (sub_A180) and installs objects 38..49 (group 16, sub_A0E0: the scrollbar's knob and
//! rails, the eight entry panels), sub_A700 runs it (state HIDWORD(qword_638658), alpha in its low half,
//! 1/20 a frame) and sub_9270 draws it: the blurred scene (jp_2E000), the backdrop, eight panels of 116 units
//! scrolled from the newest at the bottom, the quick-jump tab with the entry's thumbnail, the guide and the
//! scrollbar. A quick jump restores the entry after dialog 23.

use knife_render::{Quad, Renderer};

use super::super::{H, Ui, W};
use super::{DOWN, LEFT, MASK, RIGHT, UP, objtab};
use crate::game::{Game, LogEntry};

const SX: f32 = 0.33333;
const SY: f32 = 0.41481;
/// unk_2EB078: the entry panel (tex 69), focused below it
const PANEL: [[f32; 4]; 2] = [[32.0, 8.0, 1754.0, 270.0], [32.0, 288.0, 1754.0, 550.0]];
/// unk_2EB088 / aH: the scrollbar and its knob (tex 73)
const BAR: [f32; 4] = [0.0, 0.0, 44.0, 944.0];
const KNOB: [f32; 4] = [44.0, 8.0, 72.0, 160.0];
/// unk_2EB098: the voice icon (tex 72), focused
const VOICE: [[f32; 4]; 2] = [[16.0, 32.0, 87.0, 95.0], [112.0, 32.0, 183.0, 95.0]];

#[derive(Default)]
pub struct WLog {
    /// HIDWORD(qword_638658)
    st: u32,
    /// LODWORD(qword_638658)
    a: f32,
    /// LODWORD(qword_638674): the quick-jump tab
    ja: f32,
    /// HIDWORD(qword_638674): the knob, 0..322 (330 dragged)
    knob: i32,
    /// dword_6379D0 / dword_638660
    list: Vec<LogEntry>,
    /// dword_638664: -1, 38 the knob, 42.. the list
    drag: i32,
    /// dword_638668
    cur: i32,
    /// dword_63866C / dword_638670 / dword_638680
    target: i32,
    scroll: i32,
    drag_start: i32,
    /// dword_63867C: the panels state 3 has filled
    fill: i32,
    /// word_638650 +52 / +84 / +116: per panel its entry, quick-jumpable, with a voice
    slots: [i32; 8],
    jump: [bool; 8],
    voice: [bool; 8],
    /// LOBYTE(word_638650): the thumbnail is drawn; HIBYTE: a voice was replayed
    thumb: bool,
    playing: bool,
    /// the entry jp_89590 took the thumbnail of
    thumb_serial: u64,
    /// jd_AFEBB0: the scene's blur (jp_3BBA0, +1/12 a frame from 0.001; sub_105E0(0) ends it)
    pub blur: f32,
}

impl WLog {
    fn n(&self) -> i32 {
        self.list.len() as i32
    }

    /// An entry's panel top (sub_9270).
    fn y(&self, idx: i32) -> f32 {
        440.0 - (self.scroll + 116 * idx + 116) as f32
    }

    /// (int)((n - 3.8621) * 116): how far the list scrolls
    fn span(&self) -> i32 {
        ((self.n() as f32 - 3.8621) * 116.0) as i32
    }

    /// sub_A2A0's flags of the panel showing entry `idx` (none past the list: an empty panel).
    fn set_slot(&mut self, idx: i32) {
        let s = (idx & 7) as usize;
        self.slots[s] = idx;
        let e = usize::try_from(idx).ok().and_then(|i| self.list.get(i));
        self.jump[s] = e.is_some_and(|e| e.jump);
        self.voice[s] = e.is_some_and(|e| e.voice.is_some());
    }

    fn entry(&self, idx: i32) -> Option<&LogEntry> {
        usize::try_from(idx).ok().and_then(|i| self.list.get(i))
    }
}

impl Ui {
    /// Whether the story may open the backlog (jp_40ED0: no story movie or staff roll (off_AE0650), a message
    /// in the ring (sub_A180 >= 1)).
    pub(in crate::ui) fn wl_ok(&self, game: &Game) -> bool {
        !game.mode_task() && game.witch_log_len() > 0
    }

    /// jp_40ED0 -> sub_A180 / sub_A0E0: the list, objects 38..49, the slots' ruby lowered (sub_861A0), state 1;
    /// a pointer below y 408 goes up to 398.
    pub(in crate::ui) fn wl_open(&mut self, game: &mut Game, r: &Renderer) {
        self.w.tex.load(game, r, objtab::RES_DISP);
        self.w.objs.install(objtab::BACKLOG);
        self.w.inp.clear_rows();
        let l = &mut self.w.log;
        l.list = game.witch_log();
        l.a = 0.0;
        l.st = 1;
        if self.w.inp.pos[1] >= 409.0 {
            self.w.inp.pos[1] = 398.0;
        }
        if self.touch.at[1] >= 409.0 {
            self.touch.at[1] = 398.0;
        }
        self.push(super::super::Screen::Witch);
        self.w.scr = super::WScr::Backlog;
    }

    /// sub_A700, one 60 Hz frame. True once it is gone (state 11's fade, or the quick jump made).
    pub(in crate::ui) fn wl_step(&mut self, game: &mut Game, r: &Renderer) -> bool {
        let l = &mut self.w.log;
        if l.blur > 0.0 {
            l.blur = (l.blur + 1.0 / 12.0).min(1.0);
        }
        let mut ease = true;
        match l.st {
            1 => {
                // jp_2E000: the blur starts
                l.blur = 0.001;
                l.st = 2;
            }
            2 => {
                game.play_sys_sound(10);
                l.drag = -1;
                l.cur = 0;
                (l.target, l.scroll) = (8, 8);
                // sub_88480(38, 3, ..): the knob's touch rect is as tall as the share of the list on screen
                let size = ((1244.8 / l.n() as f32) as i32).clamp(8, 322);
                (l.ja, l.knob, l.a) = (0.0, 322, 0.0);
                l.thumb = false;
                l.playing = false;
                l.st = 3;
                l.fill = 0;
                if let Some(o) = self.w.objs.get(38) {
                    o.rect[3] = size as i16;
                }
                self.w.guide.on = true;
            }
            3 => {
                let k = l.fill;
                l.set_slot(k);
                l.fill += 1;
                if k >= 7 {
                    l.st = 4;
                }
            }
            4 => {
                l.a += 0.05;
                if l.a >= 1.0 {
                    (l.a, l.st) = (1.0, 5);
                    self.w.objs.group_flags(16, 1, 0);
                }
            }
            5 => ease = self.wl_input(game),
            6 => {
                l.ja += 0.05;
                if l.ja >= 1.0 {
                    l.ja = 1.0;
                    let v = 116 * l.cur + l.scroll + 116;
                    if v > 432 {
                        l.target = 328 - 116 * l.cur;
                    } else if v <= 115 {
                        l.target = -116 * l.cur;
                    }
                    l.st = 7;
                }
            }
            7 => {
                let p = self.w.inp.pressed;
                let inp = &self.w.inp;
                if p & (MASK[1] | RIGHT) != 0 || (inp.pos[0] as i32 >= 121 && inp.active) || inp.touch & 8 != 0 {
                    self.w.zl_block = true;
                    game.play_sys_sound(1);
                    l.st = 10;
                }
                let hit = self.w.objs.hit;
                if (42..=49).contains(&hit) && self.w.inp.active && l.cur != l.slots[(hit - 42) as usize] {
                    l.st = 10;
                }
                let inp = &self.w.inp;
                if p & MASK[0] != 0 || (inp.pos[0] as i32 <= 120 && inp.touch & 2 != 0) {
                    self.wd_open(game, r, 23, true, false);
                    self.w.log.st = 8;
                }
            }
            8 => {
                if !self.w.dlg.busy() {
                    if self.w.dlg.answer != 0 {
                        l.st = 7;
                    } else {
                        self.w.guide.on = false;
                        l.st = 9;
                    }
                }
            }
            9 => {
                l.a -= 0.05;
                if l.a <= 0.0 {
                    (l.a, l.st) = (0.0, 0);
                    l.blur = 0.0;
                    self.w.objs.group_flags(16, 0, 1);
                    game.stop_voice();
                    // the ring back to the entry (jp_5BAF0(entry, 0), sub_3E570, sub_6A7B0)
                    if let Some(e) = l.entry(l.cur) {
                        game.witch_log_jump(r, e.serial);
                    }
                    return true;
                }
            }
            10 => {
                l.ja -= 0.05;
                if l.ja <= 0.0 {
                    (l.ja, l.st) = (0.0, 5);
                }
            }
            11 => {
                // jp_AD440(320, 224, 256): the pointer back to the middle
                self.w.inp.pos = [320.0, 224.0];
                self.touch.at = [320.0, 224.0];
                l.a -= 0.05;
                if l.a <= 0.0 {
                    (l.a, l.st) = (0.0, 0);
                    l.blur = 0.0;
                    self.w.objs.group_flags(16, 0, 1);
                    return true;
                }
            }
            _ => {}
        }
        self.wl_tail(ease);
        false
    }

    /// sub_A700 state 5; false when the knob was dragged (the scroll is set, no easing or knob update).
    fn wl_input(&mut self, game: &mut Game) -> bool {
        let row = self.w.inp.take_row();
        let l = &mut self.w.log;
        let inp = &self.w.inp;
        let p = inp.pressed;
        // MASK[18]: B / X / ZL; a left flick; the back button
        if p & MASK[18] != 0 || inp.flick & 1 != 0 || self.w.guide.tapped {
            self.w.zl_block = true;
            game.play_sys_sound(1);
            l.st = 11;
            self.w.guide.on = false;
            if l.playing {
                game.stop_voice();
            }
            return true;
        }
        let down = inp.finger_down();
        if !down {
            l.drag = -1;
        }
        let hit = self.w.objs.hit;
        if inp.shown <= 0 {
            self.w.objs.focus = -1;
        } else if (42..=49).contains(&hit) && inp.active && l.slots[(hit - 42) as usize] != l.cur {
            l.cur = l.slots[(hit - 42) as usize];
            game.play_sys_sound(2);
        }
        let ([sx, sy], y) = (inp.start.map(|v| v as i32), inp.pos[1] as i32);
        let drag = if !down || l.drag >= 0 {
            l.drag
        } else if (622..=640).contains(&sx) && (15..=435).contains(&sy) {
            l.drag = 38;
            38
        } else if (14..=621).contains(&sx) {
            l.drag = 42;
            l.drag_start = l.scroll;
            42
        } else {
            -1
        };
        if drag == 38 {
            // the knob follows the finger; the list to match, the cursor kept on screen
            let k = ((y as f32 - 39.822) as i32).clamp(0, 330);
            l.knob = k;
            let v93 = l.span().max(0);
            let s = (v93 * (k - 322) / 322).min(8).max(-v93);
            (l.target, l.scroll) = (s, s);
            let top = (116 - s) / 116;
            let bottom = (332 - s) / 116;
            if l.cur < top {
                l.cur = top.max(0).min(l.n() - 1);
            } else if l.cur > bottom {
                l.cur = bottom.max(0).min(l.n() - 1);
            }
            return false;
        }
        if drag >= 42 {
            let s = (sy - y + l.drag_start).min(8).max((-24 - l.span()).min(8));
            (l.target, l.scroll) = (s, s);
            return true;
        }
        let (rep, wheel) = (inp.repeat, row);
        let x = inp.pos[0] as i32;
        let n = l.n();
        if p & MASK[0] == 0 && (!(14..=538).contains(&x) || inp.touch & 2 == 0) {
            if rep & UP != 0 || wheel >= 1 {
                if l.cur >= n - 1 {
                    if p & UP != 0 {
                        game.play_sys_sound(2);
                        l.cur = 0;
                        if l.scroll < 0 {
                            l.target = 8;
                        }
                    }
                } else {
                    game.play_sys_sound(2);
                    let old = l.cur;
                    l.cur += 1;
                    if 116 * old + l.scroll + 232 >= 433 {
                        l.target = 190 - 116 * old;
                    }
                }
            } else if rep & DOWN != 0 || wheel < 0 {
                if l.cur < 1 {
                    if p & DOWN != 0 {
                        game.play_sys_sound(2);
                        l.cur = n - 1;
                        if l.scroll + 116 * n >= 433 {
                            l.target = 422 - 116 * n;
                        }
                    }
                } else {
                    game.play_sys_sound(2);
                    let old = l.cur;
                    l.cur -= 1;
                    if l.scroll + 116 * old <= 115 {
                        l.target = 124 - 116 * old;
                    }
                }
            } else if p & LEFT != 0 || (x <= 13 && inp.active) {
                // the quick jump: an entry the ring can go back to (F823 == 1, not taken under a story movie or
                // the staff roll, not type 8 / 11) gets its thumbnail (jp_89590) and the tab
                if let Some(serial) = l.entry(l.cur).filter(|e| e.jump).map(|e| e.serial) {
                    l.thumb_serial = serial;
                    l.thumb = false;
                    game.witch_log_preview(serial, true);
                    l.st = 6;
                    game.play_sys_sound(0);
                }
            }
            return true;
        }
        // A or a tap on the list: the entry's voice again (jp_AA9F0, jp_AA970) while the voices are on (config
        // 0x36), and a finger on a panel drags the list
        if let Some(v) = l.entry(l.cur).and_then(|e| e.voice.clone())
            && game.settings.voice_on
        {
            game.stop_voice();
            game.witch_log_voice(&v);
            l.playing = true;
        }
        if (42..=49).contains(&hit) {
            l.drag = hit;
        }
        true
    }

    /// sub_A700's LABEL_46 / LABEL_68: the scroll eases to its target (a sixth of the way, at least 2), the
    /// knob follows it, and the panels from two above the top take the entries now in view.
    fn wl_tail(&mut self, ease: bool) {
        let l = &mut self.w.log;
        if ease {
            let d = l.target - l.scroll;
            if d != 0 {
                let step = if d >= 1 { (if d < 12 { 2 } else { d / 6 }).min(d) } else { (if d <= -12 { d / 6 } else { -2 }).max(d) };
                l.scroll += step;
            }
            let v44 = l.span();
            l.knob = if v44 <= 0 { 0 } else { (322 + 322 * l.scroll / v44).clamp(0, 322) };
        }
        let first = ((-l.scroll) / 116).max(2) - 2;
        for idx in (first..l.n()).take(8) {
            if l.slots[(idx & 7) as usize] != idx {
                l.set_slot(idx);
            }
        }
    }

    /// sub_9270.
    pub(in crate::ui) fn wl_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let a = self.w.log.a;
        if a <= 0.0 {
            return;
        }
        let v88 = (1.0 - a) * (1.0 - a);
        let v91 = v88 * -300.0;
        let mut pen = super::Pen::default();
        pen.alpha(a * 0.9);
        pen.scale(SX, SY);
        let tex = |ui: &Self, id| ui.w.tex.get(id);
        if let Some(t) = tex(self, 70) {
            pen.spr(out, &t, 16, None, 0.0, 0.0);
        }
        let l = &self.w.log;
        let (slots, cur, ja) = (l.slots, l.cur, l.ja);
        let ys = slots.map(|i| l.y(i));
        let shown = |y: f32| y > -116.0 && y < 448.0;
        pen.alpha(a);
        pen.scale(0.29688, 0.49778);
        for i in 0..8 {
            self.w.objs.place(42 + i as i32, v91, ys[i], 1.0);
            if shown(ys[i])
                && let Some(t) = tex(self, 69)
            {
                pen.spr(out, &t, 16, Some(PANEL[usize::from(slots[i] == cur)]), v91 + 37.0, ys[i] - 1.0);
            }
            // (the name slot 40 + i at (v91 + 119, y + 23) is empty: nothing writes the record's name)
        }
        for i in 0..8 {
            if !shown(ys[i]) {
                continue;
            }
            if let Some(e) = self.w.log.entry(slots[i]).cloned() {
                out.extend(game.witch_log_quads(r, &e, i, v91 + 55.0, ys[i] + 18.0, a));
            }
        }
        pen.scale(SX, SY);
        for i in 0..8 {
            if shown(ys[i])
                && self.w.log.voice[i]
                && let Some(t) = tex(self, 72)
            {
                pen.spr(out, &t, 16, Some(VOICE[usize::from(slots[i] == cur)]), v91 + 1550.0 / 3.0, ys[i] + 220.0 * 448.0 / 1080.0);
            }
        }
        let mut thumb_at = None;
        for i in 0..8 {
            if !shown(ys[i]) {
                continue;
            }
            if slots[i] == cur && ja > 0.0 {
                let v55 = v91 + (1.0 - ja) * -120.0;
                pen.alpha(ja * a);
                pen.scale(0.40323, 0.38199);
                if let Some(t) = tex(self, 71) {
                    pen.spr(out, &t, 16, None, v55 + 1.6129, ys[i] + 23.683);
                }
                thumb_at = Some((v55 + 25.0, ys[i] + 40.109));
            } else if self.w.log.jump[i] {
                pen.alpha(a);
                pen.scale(SX, SY);
                if let Some(t) = tex(self, 65) {
                    pen.spr(out, &t, 16, None, v91 + 13.333, ys[i] + 49.778);
                }
            }
        }
        // jp_89AD0 draws the thumbnail once; jp_A62C0 shows it 100 x 0.82667 by 53 x 1.0859 units of 1920 x 1080
        let thumb = game.witch_log_preview(self.w.log.thumb_serial, false);
        self.w.log.thumb = thumb.is_some();
        if let (Some((x, y)), Some(t)) = (thumb_at, thumb) {
            let (kx, ky) = (W / 640.0, H / 448.0);
            let (w, h) = (100.0 * 0.82667 * kx, 53.0 * 1.0859 * ky);
            out.push(Quad::rect(t, [x * kx, y * ky, w, h], [1.0, 1.0, 1.0, ja * a]));
        }
        pen.scale(SX, SY);
        pen.alpha(a);
        if let Some(t) = tex(self, 68) {
            pen.spr(out, &t, 16, None, 0.0, 0.0);
        }
        self.wg_draw(out, &pen, 5, 0.0, 0.0);
        let x = v88 * 48.0 * 640.0 / 960.0 + 620.0;
        pen.scale(SX, 0.35556);
        if let Some(t) = tex(self, 73) {
            pen.spr(out, &t, 16, Some(BAR), x, 39.822);
        }
        for id in 39..=41 {
            self.w.objs.place(id, x, 0.0, 1.0);
        }
        let ky = self.w.log.knob as f32 * 384.0 / 448.0 + 39.822;
        self.w.objs.place(38, x, ky, 1.0);
        if let Some(t) = tex(self, 73) {
            pen.spr(out, &t, 16, Some(KNOB), x + 2.6667, ky + 3.3185);
        }
    }
}
