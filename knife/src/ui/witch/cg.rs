//! Witch on the Holy Night's CG mode (図録, the title's Extra menu): jp_45850 sets it up (objects 245..264, the
//! extra / CG atlases, the slots' seen pictures), jp_42760 runs it (jd_B2D550 states 1..9) and jp_440E0 draws it
//! (task 23 through jp_424D0, with jp_424D0's black on top); state 3 tears it down (jp_425A0) and the title comes
//! back as the Extra menu left it (sub_6C170).
//!
//! The grid: 21 pages of up to 20 slots, 5 x 4 cards, the pad moving the cursor (wrapping into the pages before
//! and after at the edges), L / R or a flick turning the page. A slot with a seen picture opens the viewer: its
//! seen pictures in turn (A / R / a tap / a left flick on, L / a right flick back), zoom with the right stick or
//! X / Y (to 2, from the fit), Minus the fill zoom, R3 back to the fit, the left stick / d-pad / the pointer at an
//! edge / a drag moving it. jp_AA340's preloads (the cursor resting 30 frames, the next picture on opening) only
//! warm the texture cache.

use std::sync::Arc;

use knife_render::{Quad, RenderTarget, Renderer, TexRef};

use super::super::{H, Screen, Ui, W};
use super::cgtab::{self, CgPic, PICS};
use super::{DOWN, LEFT, MASK, Pen, RIGHT, UP, WScr, objtab};
use crate::assets::Image;
use crate::game::Game;

/// The page order (dword_25FBF8: one row, the pages in turn).
const PAGES: usize = 21;

/// The viewer's geometry (jp_45C40): zoom (jd_58FE7C) from the fit (jd_58FE78) to 2, the position the picture
/// takes at zoom 2 (jd_B2D57C / jd_B2D580) down to its limits (jd_B2D59C / jd_58FE74), and the fit's centring
/// offset (jd_B2D5A4 / jd_B2D5A0), 640x448 units.
#[derive(Clone, Copy, Default, Debug)]
struct View {
    zoom: f32,
    fit: f32,
    pan: [f32; 2],
    min: [f32; 2],
    off: [f32; 2],
}

impl View {
    /// jp_440E0: the picture's top left at this zoom, the position clamped to the screen (the stored one
    /// follows the clamp).
    fn place(&mut self) -> [f32; 2] {
        let t = if self.fit < 2.0 { (self.zoom - self.fit) / (2.0 - self.fit) } else { 0.0 };
        let mut p = [0.0; 2];
        for (i, size) in [640.0f32, 448.0].into_iter().enumerate() {
            let o = (1.0 - t) * self.off[i];
            let d = self.zoom * ((size - (self.off[i] + self.off[i])) / self.fit);
            p[i] = if d >= size {
                let v = t * self.pan[i] + o;
                (if v >= size - d { v } else { size - d }).min(0.0)
            } else {
                (size - d) * 0.5
            };
        }
        if t != 0.0 {
            for (i, v) in p.iter().enumerate() {
                self.pan[i] = (v - (1.0 - t) * self.off[i]) / t;
            }
        }
        p
    }

    /// jp_45A90's place for the old view of a variant change: no clamp.
    fn place_frozen(&self) -> [f32; 2] {
        let t = if self.fit < 2.0 { (self.zoom - self.fit) / (2.0 - self.fit) } else { 0.0 };
        [t * self.pan[0] + (1.0 - t) * self.off[0], t * self.pan[1] + (1.0 - t) * self.off[1]]
    }
}

/// The view a variant change fades out (jp_45A90 renders it into the capture jp_A46F0 draws).
#[derive(Clone)]
struct Old {
    view: View,
    pic: Option<Arc<Image>>,
    /// the old variant's entry: its backdrop and (page 0 slot 18) its rect
    var: usize,
}

#[derive(Default)]
pub struct WCg {
    /// jd_B2D550
    st: u32,
    /// qword_677124: the grid's alpha (lo) and the cards' entry (hi, 1 settled)
    fade: f32,
    cards: f32,
    /// qword_67712C: the black under the viewer (lo) and the picture's alpha (hi)
    back: f32,
    pic_a: f32,
    /// LODWORD(qword_677134): the old view over the new one
    xf: f32,
    /// jd_B2D550 + 4: jp_424D0's black over everything
    black: f32,
    /// jd_B2D598 / dword_677178: the page and its place in the order (kept from visit to visit)
    page: usize,
    page_ix: usize,
    /// dword_67713C and qword_677140: the cursor's slot, column and row
    cur: usize,
    col: i32,
    row: i32,
    /// dword_677180 / qword_677184: the page (and column, row) a page turn goes to
    to_page: usize,
    to: (i32, i32),
    /// dword_677174: the cards' entry direction (0 opening, 1 closing, 2 / 4 / 6 / 8 a page turn leaving to
    /// the left / right / down / up, one more entering)
    dir: usize,
    /// byte_67710C: the viewer is up
    viewing: bool,
    /// dword_67718C / jd_B2D594: the picture on view (of the slot's seen ones) and the one before (-1 none)
    var: i32,
    prev_var: i32,
    view: View,
    /// dword_677198 / dword_67719C: the position a drag started from
    drag_from: [f32; 2],
    /// textures 285 / 286 and the one on view (HIDWORD(qword_677134))
    tex: [Option<Arc<Image>>; 2],
    buf: usize,
    old: Option<Old>,
    /// dword_649894: each slot's seen pictures (index into its PICS list)
    seen: Vec<Vec<usize>>,
    /// the capture surface (jp_A4560)
    rt: Option<TexRef>,
}

impl WCg {
    fn count(&self) -> usize {
        cgtab::COUNT[self.page]
    }

    fn slot_seen(&self, slot: usize) -> &[usize] {
        self.seen.get(slot).map_or(&[], |v| v.as_slice())
    }

    /// The slot's entry `k` (the unpacked list: jd_B0EFA0, which the exe indexes by the seen picture's number).
    fn entry(&self, k: i32) -> Option<&'static CgPic> {
        PICS.get(self.page)?.get(self.cur)?.get(usize::try_from(k).ok()?)
    }
}

/// jp_455C0: a CG counts as seen past the ending (jp_8D1C0), for the pictures of its switch by their RDST
/// bits, else by the seen bitmap (jp_75900 -> sub_57460).
fn unlocked(game: &Game, name: &str) -> bool {
    if super::title::complete(game) {
        return true;
    }
    let num = name.get(3..7).filter(|_| name.len() >= 7 && name[..3].eq_ignore_ascii_case("img")).and_then(|d| d.parse::<u16>().ok());
    if let Some(bits) = num.and_then(|n| cgtab::UNLOCK.iter().find(|e| e.0 == n)).map(|e| e.1) {
        return bits.iter().all(|&b| super::title::rdst(game, usize::from(b)));
    }
    game.is_seen(name)
}

impl Ui {
    /// jp_45850 (jp_48AC0 mode 0, result 5): the objects, the slots' seen pictures, the textures; state 1.
    pub(in crate::ui) fn wcg_init(&mut self, game: &mut Game, r: &Renderer) {
        let c = &mut self.w.cg;
        c.viewing = false;
        c.row = 0;
        c.fade = 0.0;
        c.cards = 0.0;
        c.back = 0.0;
        c.pic_a = 0.0;
        c.xf = 0.0;
        c.buf = 0;
        c.cur = 0;
        c.col = 0;
        c.black = 0.0;
        c.old = None;
        c.tex = [None, None];
        self.w.objs.install(objtab::CG);
        // each slot's pictures that exist (jp_A90C0), those seen in order
        c.seen = (0..PAGES * 20)
            .map(|i| {
                PICS[i / 20][i % 20]
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| game.assets.has_image_hd(p.name) && unlocked(game, p.name))
                    .map(|(k, _)| k)
                    .collect()
            })
            .collect();
        c.st = 1;
        self.w.tex.load(game, r, objtab::RES_EXTRA);
        self.w.tex.load(game, r, objtab::RES_CG);
        if self.w.tex.get(76).is_none() {
            self.w.tex.load(game, r, objtab::RES_DISP);
        }
        self.w.guide.on = true;
        self.w.scr = WScr::Cg;
        self.screen = Screen::Witch;
        self.stack.clear();
    }

    /// One frame of jp_42760; true once state 3 has handed back to the title.
    pub(in crate::ui) fn wcg_step(&mut self, game: &mut Game, r: &Renderer) -> bool {
        let f = 0.7;
        match self.w.cg.st {
            1 => {
                let c = &mut self.w.cg;
                c.dir = 0;
                c.fade += f * 0.024;
                if c.fade >= 1.0 {
                    c.fade = 1.0;
                    self.w.objs.group_flags(19, 1, 0);
                    self.w.objs.group_flags(20, 1, 0);
                    c.st = 2;
                }
                c.cards = c.fade;
            }
            2 => self.wcg_grid(game, r),
            3 => {
                let c = &mut self.w.cg;
                c.dir = 1;
                c.fade += f * -0.024;
                if c.fade <= 0.0 {
                    c.fade = 0.0;
                    c.black = 1.0;
                    c.cards = 0.0;
                    c.st = 0;
                    c.tex = [None, None];
                    c.old = None;
                    // jp_9C960(1); jp_9C470(jp_8E1C0(), 255, 0): the title's BGM
                    let bgm = if super::title::complete(game) { 42 } else { 65 };
                    if game.sound.bgm_state().is_none_or(|(id, _)| id != bgm) {
                        game.play_bgm(bgm, 1.0, 0.0);
                    }
                    self.w.objs.remove_group(20);
                    self.w.objs.remove_group(19);
                    self.w.scr = WScr::None;
                    self.wt_from_extra(game, r);
                    return true;
                }
                c.black = 1.0 - c.fade;
                c.cards = c.fade;
            }
            4 => {
                let c = &mut self.w.cg;
                c.pic_a += f * 0.03;
                if c.pic_a > 1.0 {
                    c.pic_a = 1.0;
                    c.xf = 0.0;
                    c.tex[c.buf ^ 1] = None;
                    c.st = 5;
                }
                c.back = c.pic_a;
            }
            5 => self.wcg_viewer(game, r),
            6 => {
                let c = &mut self.w.cg;
                c.xf += f * -0.05;
                if c.xf >= 0.0 {
                    return false;
                }
                c.xf = 0.0;
                c.tex[c.buf ^ 1] = None;
                c.old = None;
                c.st = 5;
            }
            7 => {
                let c = &mut self.w.cg;
                c.pic_a += f * -0.03;
                if c.pic_a < 0.0 {
                    c.pic_a = 0.0;
                    c.xf = 0.0;
                    c.tex = [None, None];
                    c.old = None;
                    c.st = 2;
                    c.viewing = false;
                }
                c.back = c.pic_a;
            }
            8 => {
                let c = &mut self.w.cg;
                c.cards += f * 0.05;
                if c.cards >= 1.0 {
                    c.cards = 1.0;
                    self.w.objs.group_flags(20, 1, 0);
                    c.st = 2;
                }
            }
            9 => {
                let c = &mut self.w.cg;
                c.cards += f * -0.05;
                if c.cards > 0.0 {
                    return false;
                }
                c.cards = 0.0;
                c.page = c.to_page;
                c.page_ix = c.to_page;
                (c.col, c.row) = c.to;
                c.cur = (5 * c.row + c.col) as usize;
                let n = c.count();
                if c.cur >= n {
                    c.row = ((n - 1) / 5) as i32;
                    c.col = ((n - 1) % 5) as i32;
                    c.cur = n - 1;
                    self.w.objs.focus = (n + 244) as i32;
                    game.play_sys_sound(2);
                }
                let c = &mut self.w.cg;
                c.dir += 1;
                c.st = 8;
            }
            _ => {}
        }
        false
    }

    /// A page turn (state 9): to `page`, the cursor to (col, row), the cards leaving towards `dir`.
    fn wcg_turn(&mut self, game: &mut Game, page: usize, to: (i32, i32), dir: usize) {
        let c = &mut self.w.cg;
        c.to_page = page;
        c.to = to;
        c.dir = dir;
        c.st = 9;
        game.play_sys_sound(0);
    }

    /// L / R and the flicks step through the page order (dword_677178, wrapping).
    fn wcg_order(&mut self, next: bool) -> usize {
        let c = &mut self.w.cg;
        c.page_ix = if next {
            if c.page_ix >= 20 { 0 } else { c.page_ix + 1 }
        } else if c.page_ix < 1 {
            20
        } else {
            c.page_ix - 1
        };
        c.page_ix
    }

    /// State 2: the grid.
    fn wcg_grid(&mut self, game: &mut Game, r: &Renderer) {
        let inp = &self.w.inp;
        let (pressed, repeat) = (inp.pressed, inp.repeat);
        let tap = inp.touch & 2 != 0;
        let hit = self.w.objs.hit;
        let card = usize::try_from(hit - 245).ok().filter(|&v| v < 20);
        let n = self.w.cg.count();
        let mut moved = false;
        match card {
            None => self.w.objs.focus = (self.w.cg.cur + 245) as i32,
            // a tap on another card moves the cursor there; one on the cursor's opens it
            Some(v) if tap && v < n && self.w.cg.cur != v => {
                self.w.cg.cur = v;
                game.play_sys_sound(2);
                moved = true;
            }
            Some(_) => {}
        }
        let c = &mut self.w.cg;
        c.col = (c.cur % 5) as i32;
        c.row = (c.cur / 5) as i32;
        if pressed & MASK[1] != 0 || self.w.guide.tapped {
            c.st = 3;
            game.play_sys_sound(1);
            self.w.objs.group_flags(19, 0, 1);
            self.w.objs.group_flags(20, 0, 1);
            self.w.guide.on = false;
            return;
        }
        let open = pressed & MASK[0] != 0 || (!moved && tap && hit == (c.cur + 245) as i32);
        if open {
            if !self.w.cg.slot_seen(self.w.cg.page * 20 + self.w.cg.cur).is_empty() {
                self.wcg_open(game, r);
            }
            return;
        }
        let (col, row, cur) = (c.col, c.row, c.cur);
        if repeat & UP != 0 {
            self.w.inp.shown = 0;
            if row < 1 {
                if pressed & UP != 0 {
                    let p = if c.page < 1 { 20 } else { c.page - 1 };
                    self.wcg_turn(game, p, (col, 3), 6);
                }
                return;
            }
            c.row -= 1;
            c.cur = (col + 5 * c.row) as usize;
            self.w.objs.focus = (c.cur + 245) as i32;
            game.play_sys_sound(2);
        } else if repeat & DOWN != 0 {
            self.w.inp.shown = 0;
            let v = (col + 5 * (row + 1)) as usize;
            if v < n {
                c.row += 1;
                c.cur = v;
                self.w.objs.focus = (v + 245) as i32;
                game.play_sys_sound(2);
            } else if pressed & DOWN != 0 {
                let p = if c.page <= 19 { c.page + 1 } else { 0 };
                self.wcg_turn(game, p, (col, 0), 8);
            }
        } else if repeat & LEFT != 0 {
            self.w.inp.shown = 0;
            if cur < 1 {
                if pressed & LEFT != 0 {
                    let p = if c.page < 1 { 20 } else { c.page - 1 };
                    self.wcg_turn(game, p, (4, 3), 4);
                }
                return;
            }
            if col < 1 {
                c.col = 4;
                c.row -= 1;
            } else {
                c.col -= 1;
            }
            c.cur = (5 * c.row + c.col) as usize;
            game.play_sys_sound(2);
        } else if repeat & RIGHT != 0 {
            self.w.inp.shown = 0;
            if cur + 1 < n {
                if col >= 4 {
                    c.col = 0;
                    c.row += 1;
                } else {
                    c.col += 1;
                }
                c.cur = (5 * c.row + c.col) as usize;
                game.play_sys_sound(2);
            } else if pressed & RIGHT != 0 {
                let p = if c.page <= 19 { c.page + 1 } else { 0 };
                self.wcg_turn(game, p, (0, 0), 2);
            }
        } else {
            let flick = self.w.inp.flick;
            if pressed & MASK[9] != 0 || flick & 5 != 0 {
                let p = self.wcg_order(true);
                self.wcg_turn(game, p, (col, row), if flick & 4 != 0 { 8 } else { 2 });
            } else if pressed & MASK[8] != 0 || flick & 0xA != 0 {
                let p = self.wcg_order(false);
                self.wcg_turn(game, p, (col, row), if flick & 8 != 0 { 6 } else { 4 });
            }
        }
    }

    /// The slot's seen picture `k` (packed) into texture 285 + buf (jp_B6A20).
    fn wcg_load(&mut self, game: &mut Game, r: &Renderer, k: i32) -> Option<Arc<Image>> {
        let c = &self.w.cg;
        let slot = c.page * 20 + c.cur;
        let pic = *c.slot_seen(slot).get(usize::try_from(k).ok()?)?;
        game.assets.image_hd(r, PICS[c.page][c.cur][pic].name)
    }

    /// jp_42760 state 2 on an open: the slot's first seen picture fades in (state 4).
    fn wcg_open(&mut self, game: &mut Game, r: &Renderer) {
        let c = &mut self.w.cg;
        c.var = 0;
        c.prev_var = -1;
        self.wcg_fit(true);
        let img = self.wcg_load(game, r, 0);
        let c = &mut self.w.cg;
        c.tex[c.buf] = img;
        c.viewing = true;
        c.back = 0.0;
        c.pic_a = 0.0;
        c.xf = 0.0;
        self.w.objs.group_flags(19, 0, 1);
        self.w.objs.group_flags(20, 0, 1);
        c.st = 4;
        game.play_sys_sound(0);
        self.w.guide.on = false;
    }

    /// jp_45C40: the picture's limits and fit; `reset` centres it at the fit, else a picture of another size
    /// than the one before keeps the position and zoom inside its own limits.
    fn wcg_fit(&mut self, reset: bool) {
        let c = &mut self.w.cg;
        let Some(e) = c.entry(c.var) else { return };
        let (w, h) = (f32::from(e.w), f32::from(e.h));
        let other = c.prev_var >= 0 && c.entry(c.prev_var).is_some_and(|p| p.w != e.w || p.h != e.h);
        let v = &mut c.view;
        v.min = [(w + w - 1920.0) * -0.33333, (h + h - 1080.0) * -0.41481];
        if reset {
            v.pan = [v.min[0] * 0.5, v.min[1] * 0.5];
        } else if other {
            for i in 0..2 {
                v.pan[i] = if v.pan[i] >= v.min[i] { v.pan[i].min(0.0) } else { v.min[i] };
            }
        }
        let fit = (1920.0 / w).min(1.0).min((1080.0 / h).min(1.0));
        v.fit = fit;
        if reset || other {
            v.zoom = if reset || v.zoom < 1.0 { fit } else { v.zoom.min(2.0).max(fit) };
            v.off = [((fit * w - 1920.0) * 0.5) * -0.33333, ((fit * h - 1080.0) * 0.5) * -0.41481];
        }
        v.off = v.off.map(|o| o.max(0.0));
    }

    /// State 5: the viewer.
    fn wcg_viewer(&mut self, game: &mut Game, r: &Renderer) {
        let f = 0.7;
        let inp = &self.w.inp;
        let (pressed, held) = (inp.pressed, inp.held);
        let (touch, flick) = (inp.touch, inp.flick);
        if pressed & 0x4000 != 0 || touch & 8 != 0 {
            self.wcg_close(game, 1);
        }
        // A / R / a tap / a flick: on (L or a right flick: back, the first picture closing), L alone back
        let c = &mut self.w.cg;
        let l = pressed & 0x400 != 0;
        if pressed & (MASK[0] | 0x800) != 0 || touch & 2 != 0 || (l && c.var >= 1) || flick & 3 != 0 {
            c.prev_var = c.var;
            if !l && flick & 2 == 0 {
                self.wcg_variant(game, r, 1);
            } else if c.var >= 1 {
                self.wcg_variant(game, r, -1);
            } else {
                self.wcg_close(game, 0);
            }
        }
        // zoom: the right stick past 40, X / Y as 80 / -80 with it centred
        let c = &mut self.w.cg;
        let mut ry = self.rstick;
        if held & 0x1000 != 0 {
            if ry == 0 {
                ry = 80;
            }
        } else if ry == 0 && held & 0x8000 != 0 {
            ry = -80;
        }
        if !(-40..=40).contains(&ry) {
            let d = if ry <= -41 { ry + 40 } else { ry - 40 };
            let z = c.view.zoom + f * 0.0005 * d as f32;
            c.view.zoom = if z < c.view.fit { c.view.fit } else { z.min(2.0) };
        }
        if pressed & 1 != 0 {
            // Minus: the zoom that fills the other side
            if let Some(e) = c.entry(c.var) {
                let (w, h) = (f32::from(e.w), f32::from(e.h));
                let (a, b) = if e.w >= e.h { (1080.0 / h, 1920.0 / w) } else { (1920.0 / w, 1080.0 / h) };
                let z = if (c.view.fit - a).abs() < 0.00000011921 { b } else { a };
                game.play_sys_sound(2);
                self.w.cg.view.zoom = z.min(2.0);
            }
        } else if pressed & 4 != 0 {
            game.play_sys_sound(2);
            self.wcg_fit(true);
        }
        let inp = &self.w.inp;
        let c = &mut self.w.cg;
        if let Some(d) = inp.held_delta() {
            // one finger drags it (the touch's own position, 640x448, over a 1920 px screen)
            if inp.began {
                c.drag_from = c.view.pan;
            }
            for (i, di) in d.iter().enumerate() {
                let v = c.drag_from[i] + di / c.view.zoom;
                c.view.pan[i] = if v >= c.view.min[i] { v.min(0.0) } else { c.view.min[i] };
            }
        }
        let [lx, ly] = self.stick;
        let (x, y) = (inp.pos[0] as i32, inp.pos[1] as i32);
        let edge = inp.shown >= 1 && !((16..=624).contains(&x) && (16..433).contains(&y));
        if (-40..=40).contains(&lx) && (-40..=40).contains(&ly) && held & 0xF0 == 0 && !edge {
            return;
        }
        let v = &mut c.view;
        if held & 0x10 != 0 {
            v.pan[1] += 8.0;
        }
        if held & 0x40 != 0 {
            v.pan[1] -= 8.0;
        }
        if held & 0x80 != 0 {
            v.pan[0] += 8.0;
        }
        if held & 0x20 != 0 {
            v.pan[0] -= 8.0;
        }
        if !(-40..=40).contains(&lx) {
            v.pan[0] += (lx + if lx < -40 { 40 } else { -40 }) as f32 * -0.2;
        }
        if !(-40..=40).contains(&ly) {
            v.pan[1] += (ly + if ly < -40 { 40 } else { -40 }) as f32 * 0.2;
        }
        if inp.shown >= 1 {
            if x < 16 {
                v.pan[0] += 8.0;
            } else if x >= 625 {
                v.pan[0] -= 8.0;
            }
            if y <= 15 {
                v.pan[1] += 8.0;
            } else if y >= 433 {
                v.pan[1] -= 8.0;
            }
        }
        for i in 0..2 {
            v.pan[i] = v.pan[i].min(0.0).max(v.min[i]);
        }
    }

    /// The viewer closes (B: sound 1; past the slot's pictures: 0): state 7, the grid's input back.
    fn wcg_close(&mut self, game: &mut Game, snd: usize) {
        self.w.objs.group_flags(19, 1, 0);
        self.w.objs.group_flags(20, 1, 0);
        game.play_sys_sound(snd);
        self.w.cg.st = 7;
        self.w.guide.on = true;
    }

    /// The next / previous seen picture (LABEL_68): the old view kept for the crossfade (state 6), the new one
    /// in the other texture; past the last, the viewer closes.
    fn wcg_variant(&mut self, game: &mut Game, r: &Renderer, d: i32) {
        let c = &self.w.cg;
        let k = c.var + d;
        let slot = c.page * 20 + c.cur;
        if usize::try_from(k).ok().is_none_or(|k| k >= c.slot_seen(slot).len()) {
            self.wcg_close(game, 0);
            return;
        }
        let c = &mut self.w.cg;
        c.var = k;
        c.xf = 1.0;
        c.old = Some(Old { view: c.view, pic: c.tex[c.buf].clone(), var: c.prev_var.max(0) as usize });
        self.wcg_fit(false);
        let img = self.wcg_load(game, r, k);
        let c = &mut self.w.cg;
        c.buf ^= 1;
        c.tex[c.buf] = img;
        game.play_sys_sound(0);
        c.st = 6;
    }

    /// jp_424D0: jp_440E0, then the black.
    pub(in crate::ui) fn wcg_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
        self.wcg_grid_draw(out, (game.assets.language(), game.assets.languages()));
        self.wcg_view_draw(game, r, out);
        let b = self.w.cg.black;
        if b > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, b.min(1.0)]));
        }
    }

    /// jp_440E0's grid: the backdrop, the decorations and the button guide, the page number, the cards (black
    /// behind a seen slot's thumbnail, the locked card otherwise) entering by their delays, and the cursor.
    fn wcg_grid_draw(&mut self, out: &mut Vec<Quad>, langs: (usize, usize)) {
        let c = &self.w.cg;
        if c.fade <= 0.0 || c.back >= 1.0 {
            return;
        }
        let tex = |id| self.w.tex.get(id);
        let mut pen = Pen::default();
        pen.color(c.fade, 1.0, 1.0, 1.0);
        pen.scale(0.33333, 0.41481);
        if let Some(t) = tex(3624) {
            pen.spr(out, &t, 16, None, 0.0, 0.0);
        }
        pen.color(c.fade * (1.0 - c.back), 1.0, 1.0, 1.0);
        if let Some(t) = tex(3625) {
            pen.spr(out, &t, 16, None, 450.6667, -47.288887);
        }
        if let Some(t) = tex(3626) {
            pen.spr(out, &t, 16, None, 546.6667, 69.68889);
        }
        if let Some(t) = tex(3633) {
            pen.spr(out, &t, 16, None, 0.0, 0.0);
        }
        self.wg_draw(out, &pen, 9, 0.0, 0.0);
        pen.color(c.fade, 1.0, 1.0, 1.0);
        if let Some(t) = tex(3631) {
            pen.spr(out, &t, 16, Some([1184.0, 24.0, 1296.0, 128.0]), 34.667, 396.56);
            pen.spr(out, &t, 16, Some([1168.0, 160.0, 1248.0, 208.0]), 48.0, 419.79);
            pen.spr(out, &t, 16, Some(cgtab::DIGITS[c.page_ix.min(20)].map(f32::from)), 32.0, 396.56);
        }
        for row in 0..4 {
            for col in 0..5 {
                let i = row * 5 + col;
                if i >= c.count() {
                    break;
                }
                let v = i + 20 * c.page;
                // the card's centre (x + 56, y + 39.2) moves, the corner is drawn (x truncated)
                let (mut x, mut y) = (col as f32 * 122.67 + 18.667 + 56.0, row as f32 * 87.111 + 47.289 + 39.2);
                let a = if c.cards >= 1.0 {
                    1.0
                } else {
                    let d = cgtab::DELAY[c.dir.min(9)][i] + 1.0 + c.cards * -3.0 + 1.0;
                    let a = (2.0 - d).clamp(0.0, 1.0);
                    if c.dir > 1 {
                        let s = d.max(1.0) - 1.0;
                        let o = s * s * 20.0;
                        if (2..=5).contains(&c.dir) {
                            x += if matches!(c.dir, 2 | 5) { -o } else { o };
                        } else {
                            y += if matches!(c.dir, 6 | 9) { o } else { -o };
                        }
                    }
                    a
                };
                let (x, y) = (((x + -56.0) as i32) as f32, y + -39.2);
                let seen = !c.slot_seen(v).is_empty();
                let k = if seen { 0.0 } else { 1.0 };
                pen.color(a, k, k, k);
                if let Some(t) = tex(3630) {
                    pen.spr(out, &t, 16, Some([8.0, 8.0, 352.0, 205.0]), x, y);
                }
                pen.color(a, 1.0, 1.0, 1.0);
                if seen && let Some(t) = tex(cgtab::THUMB[v]) {
                    // byte_260878: sub_18410(-10): the language's strip of the thumbnail
                    let src = cgtab::THUMB_STRIP.contains(&v).then(|| {
                        let (lang, n) = langs;
                        let h = t.height as f32 / n.max(1) as f32;
                        [0.0, h * lang as f32, t.width as f32, h * lang as f32 + h]
                    });
                    pen.spr(out, &t, 16, src, x, y);
                }
                if col as i32 == c.col
                    && row as i32 == c.row
                    && let Some(t) = tex(3629)
                {
                    pen.spr(out, &t, 16, None, x, y);
                }
            }
        }
    }

    /// The viewer (byte_67710C): the black under it, the picture's backdrop (white for the entries with +52),
    /// the picture at its zoom and position, and a variant change's old view fading out over it.
    fn wcg_view_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        if !self.w.cg.viewing {
            return;
        }
        let white = r.white.clone();
        let mut pen = Pen::default();
        let c = &mut self.w.cg;
        if c.back > 0.0 {
            pen.color(c.back, 0.0, 0.0, 0.0);
            pen.fill(out, &white, 0.0, 0.0, 640.0, 448.0);
        }
        if c.pic_a > 0.0 {
            let var = usize::try_from(c.var).unwrap_or(0);
            let pic = c.tex[c.buf].clone();
            let a = c.pic_a;
            let mut view = c.view;
            Self::wcg_view_quads(c, &white, out, &mut view, pic.as_ref(), var, a, true);
            c.view = view;
        }
        if c.xf <= 0.0 {
            return;
        }
        let Some(old) = c.old.clone() else { return };
        // jp_45A90 into the capture: black, the old backdrop, the old picture where it was
        let mut cap = Vec::new();
        pen.color(1.0, 0.0, 0.0, 0.0);
        pen.fill(&mut cap, &white, 0.0, 0.0, 640.0, 448.0);
        let mut view = old.view;
        Self::wcg_view_quads(c, &white, &mut cap, &mut view, old.pic.as_ref(), old.var, c.pic_a, false);
        let rt = c.rt.get_or_insert_with(|| r.create_target(W as u32, H as u32, "cg capture").tex).clone();
        game.ui_passes.push((RenderTarget { tex: rt.clone() }, Some([0.0, 0.0, 0.0, 1.0]), cap));
        out.push(Quad::rect(rt, [0.0, 0.0, W, H], [1.0, 1.0, 1.0, c.xf]));
    }

    /// The backdrop and the picture of entry `var` at `view` (`clamp`: jp_440E0's place, else jp_45A90's).
    #[allow(clippy::too_many_arguments)]
    fn wcg_view_quads(c: &WCg, white: &TexRef, out: &mut Vec<Quad>, view: &mut View, pic: Option<&Arc<Image>>, var: usize, a: f32, clamp: bool) {
        let mut pen = Pen::default();
        let e = PICS[c.page][c.cur].get(var);
        let bg = if e.is_some_and(|e| e.white) { 1.0 } else { 0.0 };
        pen.color(a, bg, bg, bg);
        pen.fill(out, white, 0.0, 0.0, 640.0, 448.0);
        let (w, h) = e.map_or((1920.0, 1080.0), |e| (f32::from(e.w), f32::from(e.h)));
        let [x, y] = if clamp { view.place() } else { view.place_frozen() };
        let z = view.zoom;
        pen.color(a, 1.0, 1.0, 1.0);
        if c.page == 0 && c.cur == 18 && var <= 35 {
            pen.fill(out, white, x, y, z * w * 0.33333, z * h * 0.41481);
        }
        if let Some(img) = pic {
            // the texels at zoom x (1/3, 0.41481)
            let (lw, lh) = (img.width, img.height);
            let [kx, ky] = pen.k();
            let uv = img.tex.uv_max();
            let q = Quad::rect(img.tex.clone(), [x * kx, y * ky, lw * z * 0.33333 * kx, lh * z * 0.41481 * ky], [1.0, 1.0, 1.0, a])
                .with_uv_rect(0.0, 0.0, uv[0], uv[1]);
            out.push(q);
        }
    }
}
