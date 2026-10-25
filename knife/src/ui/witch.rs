//! Witch on the Holy Night's system screens. They run as the exe's per-frame state machines (60 Hz steps)
//! on the shared engine pieces in `objs` (draw state, objects) with the object tables of `objtab`, the
//! textures of the resource lists (jp_9A940) and the input word the exe polls (`WIn`).

use std::collections::HashMap;

use knife_render::{Quad, Renderer, TexRef};

use super::{Pad, Ui};
use crate::game::Game;

mod archive;
mod backlog;
mod cg;
mod cgtab;
mod conf;
mod dialog;
mod guide;
mod menu;
mod modepnl;
mod objs;
mod objtab;
mod popup;
mod save;
mod sound;
mod title;

pub use modepnl::{HELP as PANEL_HELP, WBtn, WSpin};
pub use objs::{Pen, WObjs};
pub use title::WitchTitle;

/// jp_8D1C0: the ending was reached (the title's BGM 42, else 65).
pub fn title_complete(game: &Game) -> bool {
    title::complete(game)
}

/// Game button bits (sub_F3EC4): see `pad.rs`.
pub fn pad_bit(b: Pad) -> u32 {
    match b {
        Pad::Minus => 0x1,
        Pad::L3 => 0x2,
        Pad::R3 => 0x4,
        Pad::Plus => 0x8,
        Pad::Up => 0x10,
        Pad::Right => 0x20,
        Pad::Down => 0x40,
        Pad::Left => 0x80,
        Pad::ZL => 0x100,
        Pad::ZR => 0x200,
        Pad::L => 0x400,
        Pad::R => 0x800,
        Pad::X => 0x1000,
        Pad::A => 0x2000,
        Pad::B => 0x4000,
        Pad::Y => 0x8000,
        Pad::LUp => 0x10000,
        Pad::LRight => 0x20000,
        Pad::LDown => 0x40000,
        Pad::LLeft => 0x80000,
    }
}

/// The action masks (0x612D68): [0] A, [1] B, [6] ZR, [7] R, [8] L | ZR, [9] R, [11] Y | B | ZL, ...
pub const MASK: [u32; 32] = [
    0x2000, 0x4000, 0x0, 0x200, 0x200, 0x0, 0x200, 0x800, 0x600, 0x800, 0x8000, 0xC100, 0x800, 0x400, 0xA, 0x1, 0x0,
    0x1000, 0x5100, 0x200, 0x100, 0x100010, 0x404140, 0x0, 0x0, 0x4000, 0x2002, 0x4100, 0x0, 0x200, 0x200, 0x0,
];
/// D-pad and right stick directions (the right stick comes in as the d-pad).
pub const UP: u32 = 0x100010;
pub const DOWN: u32 = 0x400040;
pub const LEFT: u32 = 0x800080;
pub const RIGHT: u32 = 0x200020;

/// The input the screens poll each frame: the pad state (*off_632310: +8 held, +12 repeat, +16 pressed)
/// and the touch state (off_632320, jp_AD4E0) in 640x448 units. The left mouse button is the finger; a mouse
/// that moves without it is the exe's mouse pointer (off_632B90), which makes the pointer active (+8) for
/// that frame only, so the objects under it take the focus but nothing is tapped.
#[derive(Default, Debug)]
pub struct WIn {
    pub held: u32,
    pub repeat: u32,
    pub pressed: u32,
    /// +0x34: 0x8001 while a finger is down; on its release 2 after 4..24 frames, 0x2000 after 7..199 (both
    /// only without a drag of 13 units or a flick), and 0x4000 / 0x1000 copies of 0x2000 / 2
    pub touch: u32,
    /// +0x54: the release's flick, along its stronger axis: 1 left, 2 right, 4 up, 8 down (7 units over the
    /// last two frames), 16 / 32 / 64 / 128 the same at 13
    pub flick: u32,
    /// +8: a finger down or the mouse moved this frame
    pub active: bool,
    /// +0x48: a finger came down this frame
    pub began: bool,
    /// +0x2C / +0x30: where the pointer is (it stays after the release)
    pub pos: [f32; 2],
    /// +0x3C (and +0x38): the mouse wheel's notches this frame, + up (jp_AD4E0 copies off_632B90 +0x10, the
    /// exe's mouse, which the Switch never fills): dialogs take any as left / right, choices and the backlog
    /// >= 1 as up and < 0 as down
    pub wheel: i32,
    /// the finger slot's start (fields 5 / 6), kept after the release for the tap's checks
    pub start: [f32; 2],
    /// +12: 256 while the pointer is in use (a finger down or the mouse moved), then a frame less each frame;
    /// the backlog drops its focus once it runs out
    pub shown: i32,
    finger: Option<Finger>,
    released: bool,
    hovered: bool,
    wheel_acc: f32,
    /// knife: the backlog's wheel in entries (a notch, or the trackpad's travel over an entry's height),
    /// taken one entry a frame
    rows_acc: f32,
}

#[derive(Debug, Clone, Copy)]
struct Finger {
    start: [f32; 2],
    /// the pointer each frame with the finger down (slot fields 9, 7, 3; knife's desktop policy in `Trail`)
    trail: super::touch::Trail,
    /// moved 13 units from the start (slot field 11 -> +0x78)
    moved: bool,
    frames: u32,
}

impl WIn {
    pub fn pad(&mut self, b: Pad, pressed: bool, repeat: bool) {
        let bit = pad_bit(b);
        if !pressed {
            self.held &= !bit;
        } else if repeat {
            self.repeat |= bit;
        } else {
            self.held |= bit;
            self.pressed |= bit;
            self.repeat |= bit;
        }
    }

    pub fn touch_down(&mut self, x: f32, y: f32) {
        self.pos = [x, y];
        self.start = [x, y];
        let mut trail = super::touch::Trail::default();
        trail.reset([x, y]);
        self.finger = Some(Finger { start: [x, y], trail, moved: false, frames: 0 });
        self.released = false;
    }

    pub fn touch_move(&mut self, x: f32, y: f32) {
        self.pos = [x, y];
        match &mut self.finger {
            Some(f) => {
                if (x - f.start[0]).abs() as i32 + (y - f.start[1]).abs() as i32 >= 13 {
                    f.moved = true;
                }
            }
            None => self.hovered = true,
        }
    }

    pub fn touch_up(&mut self) {
        if self.finger.is_some() {
            self.released = true;
        }
    }

    /// Mouse wheel / trackpad notches (+ up); a trackpad's fractions add up to whole notches.
    pub fn wheel(&mut self, lines: f32) {
        if self.wheel_acc != 0.0 && self.wheel_acc.signum() != lines.signum() {
            self.wheel_acc = 0.0;
        }
        self.wheel_acc += lines;
    }

    /// The backlog's wheel travel in entries (+ up); a change of direction drops what was left.
    pub fn wheel_rows(&mut self, rows: f32) {
        if self.rows_acc != 0.0 && self.rows_acc.signum() != rows.signum() {
            self.rows_acc = 0.0;
        }
        self.rows_acc += rows;
    }

    /// One whole entry of the backlog's wheel travel (+1 up, -1 down), or 0.
    pub fn take_row(&mut self) -> i32 {
        if self.rows_acc >= 1.0 {
            self.rows_acc -= 1.0;
            1
        } else if self.rows_acc <= -1.0 {
            self.rows_acc += 1.0;
            -1
        } else {
            0
        }
    }

    pub fn clear_rows(&mut self) {
        self.rows_acc = 0.0;
    }

    /// The whole notches since the last call.
    pub fn take_wheel(&mut self) -> i32 {
        let n = self.wheel_acc.trunc();
        self.wheel_acc -= n;
        n as i32
    }

    pub fn finger_down(&self) -> bool {
        self.finger.is_some() && !self.released
    }

    /// The mouse moved this frame with its button up (no Switch input does this): knife lets it move the menus'
    /// cursors as the pad does.
    pub fn hovering(&self) -> bool {
        self.active && self.finger.is_none()
    }

    /// A finger down: how far it is from where it started.
    pub fn held_delta(&self) -> Option<[f32; 2]> {
        self.finger.filter(|_| !self.released).map(|f| [self.pos[0] - f.start[0], self.pos[1] - f.start[1]])
    }

    /// The start of a 60 Hz frame (jp_AD4E0): this frame's touch flags.
    pub fn begin(&mut self) {
        self.touch = 0;
        self.flick = 0;
        self.active = std::mem::take(&mut self.hovered);
        self.began = false;
        self.wheel = self.take_wheel();
        if let Some(f) = &mut self.finger {
            if std::mem::take(&mut self.released) {
                let [dx, dy] = f.trail.flick(f.start, self.pos).map(|d| d as i32);
                let mut v = 0;
                v |= if dx < -6 { 1 } else if dx >= 7 { 2 } else { 0 };
                v |= if dy < -6 { 4 } else if dy >= 7 { 8 } else { 0 };
                v |= if dx < -12 { 16 } else if dx >= 13 { 32 } else { 0 };
                v |= if dy < -12 { 64 } else if dy >= 13 { 128 } else { 0 };
                if v != 0 {
                    v &= if dx.abs() <= dy.abs() { !3 } else { !12 };
                }
                self.flick = v;
                // the frames since the last frame without a finger
                let d = f.frames + 1;
                if !f.moved && v == 0 {
                    if (4..=24).contains(&d) {
                        self.touch |= 2;
                    }
                    if (7..=199).contains(&d) {
                        self.touch |= 0x2000;
                    }
                }
                self.finger = None;
            } else {
                self.began = f.frames == 0;
                f.frames += 1;
                f.trail.push(self.pos);
                self.active = true;
                self.touch |= 0x8001;
            }
        }
        if self.active {
            self.shown = 256;
        } else if self.shown > 0 {
            self.shown -= 1;
        }
        if self.touch & 0x2002 != 0 {
            let v = self.touch;
            self.touch = (v << 1) & 0x4000 | (v << 11) & 0x1000 | v;
        }
    }

    /// The end of a frame: the pressed and repeat words are used up.
    pub fn end(&mut self) {
        self.pressed = 0;
        self.repeat = 0;
    }
}

/// Textures by id (jp_9A940 loads a resource list: id -> atlas image).
#[derive(Default)]
pub struct WTex {
    map: HashMap<i32, TexRef>,
    atlases: HashMap<&'static str, Vec<TexRef>>,
    /// each id's atlas and image, for a language change
    ids: HashMap<i32, (&'static str, usize)>,
}

impl WTex {
    pub fn load(&mut self, game: &Game, r: &Renderer, list: &[(i32, &'static str, usize)]) {
        for &(id, stem, idx) in list {
            self.ids.insert(id, (stem, idx));
            let imgs = self.atlases.entry(stem).or_insert_with(|| {
                // jp_9A940 adds the language suffix; the atlases without one (CG_PARTS, ARCHTHUMB_PARTS) have none
                let lang = game.assets.ui_atlas(stem);
                let name = if game.assets.allui.find_stem(&lang).is_some() { lang } else { stem.to_string() };
                match game.assets.ui_textures(r, &name) {
                    Ok(v) => v.into_iter().map(|(_, t)| t).collect(),
                    Err(e) => {
                        tracing::warn!("ui textures {name}: {e:#}");
                        Vec::new()
                    }
                }
            });
            if let Some(t) = imgs.get(idx) {
                self.map.insert(id, t.clone());
            }
        }
    }

    pub fn get(&self, id: i32) -> Option<TexRef> {
        self.map.get(&id).cloned()
    }

    /// jp_9ACB0(1) after a language change: every loaded resource again (jp_9A940 picks each atlas's
    /// language file anew).
    pub fn relang(&mut self, game: &Game, r: &Renderer) {
        self.atlases.clear();
        self.map.clear();
        let list: Vec<(i32, &'static str, usize)> = self.ids.iter().map(|(&id, &(stem, idx))| (id, stem, idx)).collect();
        self.load(game, r, &list);
    }
}

/// The WoH task up while `Screen::Witch` shows.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub enum WScr {
    #[default]
    None,
    /// the system menu over the story (and the save screen / config it opens)
    Menu,
    /// the backlog over the story
    Backlog,
    /// the save screen on its own over the story: the TeaTime menu's save button (sub_74190: jp_4EA10(1),
    /// sub_323A0(1); sub_745E0 state 3 sub_31A70, state 4 steps sub_324D0 until it returns 3)
    Save,
    /// a story dialog (`wd_ask`) over the story and its mode panel
    Ask(super::Confirm),
    /// CG mode (the title's Extra menu)
    Cg,
    /// Sound mode (the title's Extra menu)
    Sound,
    /// the Archive (the title's Extra menu)
    Archive,
}

/// Everything the WoH screens keep.
#[derive(Default)]
pub struct WUi {
    pub inp: WIn,
    pub objs: WObjs,
    pub tex: WTex,
    pub scr: WScr,
    pub menu: menu::WMenu,
    pub dlg: dialog::WDlg,
    pub popup: popup::WPopup,
    pub guide: guide::WGuide,
    pub save: save::WSave,
    pub conf: conf::WConf,
    pub log: backlog::WLog,
    pub cg: cg::WCg,
    pub snd: sound::WSnd,
    pub arc: archive::WArc,
    /// off_632388: a screen closed with B / X / ZL (MASK[18] or a menu's back); ZL does not rewind the story
    /// until it is let go (jp_40ED0 clears it while ZL is up)
    pub zl_block: bool,
    /// the frames since the last 60 Hz step
    ms: f32,
}

impl Ui {
    /// Per frame: the 60 Hz steps of the WoH screens, in the frame order of jp_AF9F0: the touch state
    /// (jp_AD4E0), the objects (jp_AE840), the mode manager (jp_48AC0: the dialog, then the screen's task).
    pub(super) fn witch_update(&mut self, game: &mut Game, r: &Renderer, dt: f32) {
        self.w.ms += dt;
        // jp_ADC40: the help balloons follow config 0x34 (sub_10130 at load, the config's row at once)
        self.w.objs.help_on = game.settings.help_text;
        while self.w.ms >= 1000.0 / 60.0 {
            self.w.ms -= 1000.0 / 60.0;
            self.w.inp.begin();
            self.w.objs.update(&mut self.w.inp);
            self.wd_step(game);
            self.wp_step();
            self.wg_step();
            let mut yes = None;
            let to_title = match self.w.scr {
                WScr::Menu => self.wm_step(game, r),
                WScr::Backlog => {
                    if self.wl_step(game, r) {
                        self.w.scr = WScr::None;
                    }
                    false
                }
                WScr::Save => {
                    if self.ws_step(game, r) != 0 {
                        self.w.scr = WScr::None;
                    }
                    false
                }
                WScr::Ask(c) => {
                    if !self.w.dlg.busy() {
                        self.w.scr = WScr::None;
                        yes = (self.w.dlg.answer == 0).then_some(c);
                    }
                    false
                }
                WScr::Cg => {
                    if self.wcg_step(game, r) {
                        self.w.inp.end();
                        return;
                    }
                    false
                }
                WScr::Sound => {
                    if self.wsnd_step(game, r) {
                        self.w.inp.end();
                        return;
                    }
                    false
                }
                WScr::Archive => {
                    if self.warc_step(game, r) {
                        self.w.inp.end();
                        return;
                    }
                    false
                }
                WScr::None => false,
            };
            self.w.inp.end();
            if to_title {
                // sub_2D940 state 15 ends with jp_5F200 (every sound out, the BGM at 416.67 a frame) and returns 1:
                // the story mode fades to black over the story (jp_48AC0 mode 2 state 2), then the title, or the
                // Archive in a replay
                self.pop();
                game.sound.stop_bgm_rate(416.67);
                self.end_story(game);
                return;
            }
            if self.w.scr == WScr::None && self.screen == super::Screen::Witch {
                self.pop();
            }
            if let Some(c) = yes {
                self.confirm_yes(game, r, c);
            }
        }
        if self.screen == super::Screen::Witch && self.w.scr == WScr::Menu {
            game.menu_blur = Some(self.wm_blur()).filter(|&a| a > 0.0);
        }
        if self.screen == super::Screen::Witch && self.w.scr == WScr::Backlog {
            game.menu_blur = Some(self.w.log.blur).filter(|&a| a > 0.0);
        }
    }

    /// `Screen::Witch`'s draw.
    pub(super) fn witch_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        match self.w.scr {
            WScr::Menu => self.wm_draw(game, r, out),
            WScr::Backlog => self.wl_draw(game, r, out),
            WScr::Save => self.ws_draw(game, r, out),
            // jp_4DE10 keeps drawing the mode panel under the dialog
            WScr::Ask(_) => self.draw_touch_game(r, out),
            WScr::Cg => self.wcg_draw(game, r, out),
            WScr::Sound => self.wsnd_draw(game, r, out),
            WScr::Archive => self.warc_draw(game, r, out),
            WScr::None => {}
        }
        self.wd_draw(game, r, out);
        self.wp_draw(game, r, out);
    }

    /// Pad input while a WoH screen is up.
    pub(super) fn witch_pad(&mut self, b: Pad, pressed: bool, repeat: bool) {
        self.w.inp.pad(b, pressed, repeat);
    }

    /// The pointer in 640x448 units.
    fn witch_units(&self) -> (f32, f32) {
        (self.mouse.0 * 640.0 / super::W, self.mouse.1 * 448.0 / super::H)
    }

    pub(super) fn witch_mouse_down(&mut self) {
        let (x, y) = self.witch_units();
        self.w.inp.touch_down(x, y);
    }

    pub(super) fn witch_mouse_move(&mut self) {
        let (x, y) = self.witch_units();
        self.w.inp.touch_move(x, y);
    }

    pub(super) fn witch_mouse_up(&mut self) {
        self.w.inp.touch_up();
    }
}
