//! Witch on the Holy Night's title (re/NOTES_WOH.md "Title"): init jp_8D2E0, the state machine jp_8E200 (one
//! step per 60 Hz frame), the draw jp_8D5D0 with the launch splashes sub_6E550 / sub_6E700, and the menu
//! objects 205..211 (group 10) and 214..218 (Extra, group 11) of the object table at 0x45A500.
//!
//! The launch task (jp_B5650) only loads: its logo rows (parts.mrg logo_fix / caution2) sit behind a branch the
//! game never takes. Each launch's first title visit plays movie/logo.mp4 over white, shows typemoon and the
//! caution (states 61..68); a fresh profile (G010 = 0) instead shows the title1st splash and starts the story
//! (states 55..60, then 14).

use knife_render::{Blend, Quad, Renderer, TexRef};

use super::super::{Screen, Ui, H, W};
use crate::game::Game;

/// title_parts entries (jp_9A940 of the resource lists 0x45A2F0 / 0x45A3F8): texture slot -> atlas index
/// before and after the ending (jp_8D1C0).
const BG: [usize; 2] = [0, 1];
const MENU: usize = 2;
const LOGO: [usize; 2] = [3, 4];
const CURSOR: [usize; 2] = [5, 6];
const SINCE: usize = 7;
const EXTRA: usize = 8;
const FIRST_W: usize = 9;
const FIRST_B: usize = 10;
const TYPEMOON: usize = 11;
const CAUTION: usize = 12;
/// RDST bits (G130.., jp_8D180) jp_8D1C0 needs for the second title (bg2, logo_b, cursor_b, BGM 42).
const COMPLETE: [usize; 11] = [128, 65, 73, 72, 206, 129, 130, 131, 132, 133, 134];
/// Menu rows by entry count (unk_262E44, 640x448 units, indexed from (n - 2)(n - 3) / 2 + 2n - 3).
const ROWS4: [[f32; 2]; 4] = [[221.33333, 195.79259], [221.33333, 239.76297], [221.33333, 283.73334], [221.33333, 327.7037]];
const ROWS5: [[f32; 2]; 5] =
    [[221.33333, 195.79259], [221.33333, 239.76297], [221.33333, 283.73334], [221.33333, 327.7037], [221.33333, 371.67407]];
/// Object touch rects (i16 x0, y0, x1, y1 from the object position, 640x448 units): group 10 / group 11.
const RECT_MAIN: [i32; 4] = [10, 0, 187, 39];
const RECT_EXTRA: [i32; 4] = [33, 0, 164, 39];
/// Atlas row (y of the 104 px cell) per object 205..211 and 214..218; columns: normal x 8, focus x 616, pressed
/// row + 552 (the object frame tables at 0x459E68).
const MAIN_ROW: [f32; 7] = [220.0, 8.0, 114.0, 326.0, 326.0, 432.0, 432.0];
const EXTRA_ROW: [f32; 5] = [8.0, 114.0, 220.0, 326.0, 326.0];
/// Cursor rows (dword_262AE4) and per language (xl, xr, y without Continue, y with) in 960x544 units
/// (unk_262B00).
const CUR_ROW: [i32; 7] = [0, 1, 2, 3, -1, 4, -1];
const CUR_POS: [[[f32; 4]; 5]; 4] = [
    [[356.0, 604.0, -250.0, 238.0], [356.0, 604.0, 238.0, 291.0], [371.0, 589.0, 291.0, 344.0], [386.0, 574.0, 344.0, 397.0], [411.0, 549.0, 397.0, 450.0]],
    [[371.0, 589.0, -250.0, 238.0], [406.0, 554.0, 238.0, 291.0], [316.0, 643.0, 291.0, 344.0], [391.0, 569.0, 344.0, 397.0], [406.0, 554.0, 397.0, 450.0]],
    [[380.5, 579.5, -250.0, 238.0], [344.0, 616.5, 238.0, 291.0], [391.0, 569.5, 291.0, 344.0], [380.5, 580.0, 344.0, 397.0], [418.0, 542.5, 397.0, 450.0]],
    [[375.0, 585.5, -250.0, 238.0], [375.0, 585.5, 238.0, 291.0], [388.0, 572.0, 291.0, 344.0], [414.5, 546.0, 344.0, 397.0], [414.5, 546.0, 397.0, 450.0]],
];
/// Extra cursor rows (dword_262D40) and per language (xl, xr, y) (unk_262D54).
const EXT_ROW: [i32; 5] = [0, 1, 2, -1, 3];
const EXT_POS: [[[f32; 3]; 4]; 4] = [
    [[406.0, 554.0, 238.0], [406.0, 554.0, 291.0], [406.0, 554.0, 344.0], [406.0, 554.0, 397.0]],
    [[381.0, 579.0, 238.0], [376.0, 584.0, 291.0], [346.0, 614.0, 344.0], [406.0, 554.0, 397.0]],
    [[399.5, 561.0, 238.0], [399.5, 561.0, 291.0], [399.5, 561.0, 344.0], [381.0, 579.0, 397.0]],
    [[401.0, 559.0, 238.0], [401.0, 559.0, 291.0], [401.0, 559.0, 344.0], [387.0, 573.5, 397.0]],
];
/// Splash backdrop per state 61..68 (dword_262F48): white, then black.
const SPLASH_BG: [f32; 8] = [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
/// System sounds (jp_ACEF0): 0 decide, 1 cancel / not available, 2 cursor.
const SND_OK: usize = 0;
const SND_NG: usize = 1;
const SND_MOVE: usize = 2;

/// What the title hands to the mode manager (jp_48AC0 mode 0 after jp_8E200's result).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WtOut {
    /// 2: START_SCRIPT (scene id 110)
    Start,
    /// 3: the record read in state 21 / by the load screen
    Loaded(usize),
    /// 5 / 6 / 7: CG mode, sound mode, the archive (states 38 / 39 / 40)
    Cg,
    Sound,
    Archive,
}

/// A menu object (jd_18C0C08, 136 bytes): its frame state crossfades (flag 0x20) at 0.2 a frame.
#[derive(Clone, Copy, Default)]
struct Obj {
    /// +4: 0 normal, 1 focus, 4 pressed
    state: u8,
    /// +80: the state before, drawn over the new one at `w`
    prev: u8,
    /// +64
    w: f32,
    /// +16: frames left in the pressed state (jp_AE040)
    press: i32,
    /// +44 / +48 (640x448 units) and +60
    pos: [f32; 2],
    alpha: f32,
}

#[derive(Default)]
pub struct WitchTitle {
    /// HIDWORD(qword_F475A0)
    pub st: u32,
    /// LODWORD(qword_F475A0): the Extra menu shown
    ext: f32,
    /// LODWORD(qword_F475A8): the black over the title
    black: f32,
    /// off_632930: the title fade-in (state 2)
    fade: f32,
    /// off_632938: the menu's alpha
    menu_a: f32,
    /// qword_F47598: the cursors' alphas
    cur_a: f32,
    ext_cur_a: f32,
    /// dword_F475C0: the cursor's animation frame (0..40, half a frame per frame)
    anim: f32,
    /// dword_45A2EC / dword_F475BC
    pub cursor: i32,
    ext_cursor: i32,
    /// qword_F475B4: title1st_b / caution (lo), title1st_w / typemoon (hi)
    splash: [f32; 2],
    /// off_632980
    timer: i32,
    /// dword_45A2E8: frames before the first state runs
    wait: i32,
    /// byte_F47590: the title draws
    active: bool,
    /// byte_F47591: this launch has shown the logo movie and caution
    visited: bool,
    /// the entries offered (+76: Continue, Start, Load, Config, -, Extra, -) and their rows (off_6328F8)
    on: [bool; 7],
    order: [i32; 7],
    count: i32,
    /// the Extra entries (+56: Archive, CG, Sound, -, Back) and rows (off_632920)
    ext_on: [bool; 5],
    ext_order: [i32; 5],
    ext_count: i32,
    /// input on for object groups 10 / 11 (jp_ADEB0 bit 0)
    groups: [bool; 2],
    objs: [Obj; 12],
    /// the focus object (LODWORD(jd_19150EC)) as an index into `objs`
    focus: Option<usize>,
    /// the second title (jp_8D1C0)
    complete: bool,
    pub tex: Vec<TexRef>,
    /// the frames since the last 60 Hz step
    ms: f32,
    /// the logo movie runs (state 63)
    movie: bool,
    pub out: Option<WtOut>,
}

/// jp_8D180: RDST bit `n` (bit n % 16 of G130 + n / 16).
pub(super) fn rdst(game: &Game, n: usize) -> bool {
    n <= 255 && game.var(&format!("G{:03}", 130 + n / 16)) >> (n % 16) & 1 != 0
}

/// jp_8D1C0: the bits of the second title.
pub(super) fn complete(game: &Game) -> bool {
    COMPLETE.iter().all(|&n| rdst(game, n))
}

impl WitchTitle {
    fn tex(&self, i: usize) -> Option<&TexRef> {
        self.tex.get(i)
    }

    /// Object index of a menu entry (205 + i) or an Extra entry (214 + i).
    fn main_obj(i: usize) -> usize {
        i
    }

    fn ext_obj(i: usize) -> usize {
        7 + i
    }

    /// (n - 2)(n - 3) / 2 + 2n - 3 rows into unk_262E44, as 4 / 5 entry tables.
    fn row_pos(count: i32, row: i32) -> [f32; 2] {
        let r = row.max(0) as usize;
        if count >= 5 { ROWS5[r.min(4)] } else { ROWS4[r.min(3)] }
    }

    /// jp_AE840's state step for every object: pressed while its counter runs, focus on the focus object, else
    /// normal; a change restarts the crossfade (+64 = 1, then -0.2 a frame).
    fn step_objs(&mut self) {
        for (i, o) in self.objs.iter_mut().enumerate() {
            let s = if o.press > 0 {
                o.press -= 1;
                4
            } else if self.focus == Some(i) {
                1
            } else {
                0
            };
            if s != o.state {
                o.w = 1.0;
                o.prev = o.state;
                o.state = s;
            } else if o.prev != o.state {
                o.w -= 0.2;
                if o.w <= 0.0 {
                    o.w = 0.0;
                    o.prev = o.state;
                }
            }
        }
    }
}

impl Ui {
    /// TITLE_PARTS in the current language, unless it is loaded.
    pub(in crate::ui) fn wt_load(&mut self, game: &Game, r: &Renderer) {
        if self.wt.tex.is_empty() {
            let atlas = game.assets.ui_atlas("TITLE_PARTS");
            match game.assets.ui_textures(r, &atlas) {
                Ok(list) => self.wt.tex = list.into_iter().map(|(_, t)| t).collect(),
                Err(e) => tracing::warn!("ui textures {atlas}: {e:#}"),
            }
        }
    }

    /// jp_8D2E0: the title starts over (state 0, black), the menu laid out for the records there are.
    pub(in crate::ui) fn wt_init(&mut self, game: &mut Game, r: &Renderer) {
        self.wt_load(game, r);
        // jp_47BF0 loads dialog_win at boot: the title's popups (ロード中です, セーブデータがありません) draw it
        self.wd_install(game, r);
        let wt = &mut self.wt;
        wt.active = false;
        wt.complete = complete(game);
        wt.black = 1.0;
        wt.wait = 3;
        wt.objs = [Obj::default(); 12];
        wt.groups = [false; 2];
        wt.focus = None;
        wt.ext_on = [true, true, true, false, true];
        // the resume record (122) in use: Continue first
        let resume = self.file.continue_slot().is_some();
        let k = i32::from(resume);
        wt.on = [resume, true, true, true, false, true, false];
        wt.order = [if resume { 0 } else { -1 }, k, k + 1, k | 2, -1, k + 3, -1];
        wt.count = k | 4;
        wt.ext_order = [0, 1, 2, -1, 3];
        wt.ext_count = 4;
        wt.menu_a = 0.0;
        wt.fade = 0.0;
        wt.ext = 0.0;
        wt.cur_a = 0.0;
        wt.ext_cur_a = 0.0;
        wt.st = 0;
        wt.out = None;
        wt.movie = false;
        self.screen = Screen::Title;
        self.stack.clear();
    }

    /// sub_6C170 (jp_48AC0 mode 0, 8): back from an extra mode, the title as its Extra menu left it: the fade-in
    /// done, the menu and the Extra entries up, state 46 lifting the black.
    pub(in crate::ui) fn wt_from_extra(&mut self, game: &mut Game, r: &Renderer) {
        self.wt_load(game, r);
        let wt = &mut self.wt;
        wt.fade = 1.0;
        wt.menu_a = 1.0;
        wt.ext = 1.0;
        wt.black = 1.0;
        wt.st = 46;
        wt.active = true;
        wt.out = None;
        self.screen = Screen::Title;
        self.stack.clear();
    }

    /// Back from the story (jp_48AC0 back to mode 0): the title starts over, past its first-visit logos (which a
    /// story reached here always had shown, short of a debug start).
    pub(in crate::ui) fn wt_reenter(&mut self, game: &mut Game, r: &Renderer) {
        self.wt.visited = true;
        self.wt_init(game, r);
    }

    /// Launch: the title's first visit (logo movie, caution) or, with `skip`, straight to its fade-in.
    pub(in crate::ui) fn wt_boot(&mut self, game: &mut Game, r: &Renderer, skip: bool) {
        self.wt.visited = skip;
        self.wt_init(game, r);
    }

    /// Per frame: the 60 Hz steps of jp_8E200 and the object states (jp_AE840).
    pub(in crate::ui) fn wt_update(&mut self, game: &mut Game, r: &Renderer, dt: f32) {
        self.wt.ms += dt;
        self.w.objs.help_on = game.settings.help_text;
        while self.wt.ms >= 1000.0 / 60.0 {
            self.wt.ms -= 1000.0 / 60.0;
            // jp_AF9F0's frame: the touch state, the objects, the dialog and popup, the button guide, the mode
            self.w.inp.begin();
            // the menu's states make the cursor's entry (objects 205..218) the focus object; knife keeps those
            // in `wt.objs`, so the focus a screen left (the archive's book, the system menu's tab, disabled but
            // still there) must not take the touch's release bits away (jp_AE840)
            if matches!(self.wt.st, 9 | 11 | 33..=36 | 46) {
                self.w.objs.focus = -1;
            }
            self.w.objs.update(&mut self.w.inp);
            self.wd_step(game);
            self.wp_step();
            self.wg_step();
            self.wt_step(game, r);
            self.w.inp.end();
            self.wt.step_objs();
            if self.wt.out.is_some() {
                break;
            }
        }
        if let Some(o) = self.wt.out.take() {
            self.wt_leave(game, r, o);
        }
    }

    /// The title's result (jp_48AC0 mode 0).
    pub(in crate::ui) fn wt_leave(&mut self, game: &mut Game, r: &Renderer, o: WtOut) {
        game.movie_by_ui = false;
        match o {
            WtOut::Start => {
                game.reset();
                game.start_scene(0, None);
                self.screen = Screen::Game;
                self.stack.clear();
            }
            WtOut::Loaded(slot) => {
                if self.restore_slot(game, r, slot) {
                    self.enter_loaded();
                } else {
                    self.wt_init(game, r);
                }
            }
            // jp_45850 / jp_46660 / jp_47AE0
            WtOut::Cg => self.wcg_init(game, r),
            WtOut::Sound => self.wsnd_init(game, r),
            WtOut::Archive => self.warc_init(game, false),
        }
    }

    /// One frame of jp_8E200.
    pub(in crate::ui) fn wt_step(&mut self, game: &mut Game, r: &Renderer) {
        let f = 1.0;
        match self.wt.st {
            0 => {
                if self.wt.wait >= 1 {
                    self.wt.wait -= 1;
                    return;
                }
                if game.var("G010") == 0 {
                    // a fresh profile: the title1st splash, then the story
                    self.wt.fade = 0.0;
                    self.wt.visited = true;
                    self.wt.splash = [1.0, 1.0];
                    self.wt.st = 55;
                    self.wt.black = 1.0;
                    return;
                }
                if !self.wt.visited {
                    self.wt.splash = [0.0, 0.0];
                    self.wt.fade = 0.0;
                    self.wt.visited = true;
                    self.wt.st = 61;
                    self.wt.black = 1.0;
                    return;
                }
                self.wt.active = true;
                self.wt.st = 2;
            }
            1 => {
                self.wt.active = true;
                self.wt.st = 2;
            }
            2 => {
                let v = self.wt.fade + 0.012 * f;
                self.wt.fade = v;
                if v >= 1.0 {
                    self.wt.fade = 1.0;
                    // jp_64F60 marks the BGM heard; jp_9C470 plays it at full volume at once
                    let bgm = if self.wt.complete { 42 } else { 65 };
                    crate::game::set_bit(&mut game.heard, bgm);
                    if game.sound.bgm_state().is_none_or(|(id, _)| id != bgm as i32) {
                        game.play_bgm(bgm as i32, 1.0, 0.0);
                    }
                    self.wt.st = 4;
                }
                self.wt.black = 1.0 - self.wt.fade;
            }
            4 => self.wt.st = 5,
            // loc_77C00: the free space check passes
            5 => self.wt.st = 8,
            8 => {
                self.wt.menu_a = 1.0;
                let manual = self.file.manual_count() > 0;
                self.wt.cursor = if self.wt.on[0] {
                    0
                } else if manual {
                    2
                } else {
                    1
                };
                self.wt.groups[0] = true;
                self.wt.st = 9;
            }
            9 => self.wt.st = 11,
            11 => {
                // with no touch the focus object is the cursor's
                self.wt.focus = Some(WitchTitle::main_obj(self.wt.cursor.max(0) as usize));
                self.wt_touch(game, r);
            }
            12 => {
                self.wt.st = 4;
                self.wt.menu_a = 0.0;
            }
            14 | 23 => {
                self.wt.black += 0.01 * f;
                if self.wt.black > 1.2 {
                    self.wt.black = 1.0;
                    self.wt.active = false;
                    let st = self.wt.st;
                    self.wt.st = 0;
                    self.wt.out = Some(match st {
                        14 => WtOut::Start,
                        _ => WtOut::Loaded(self.wt_loaded.take().unwrap_or(crate::save::auto_first())),
                    });
                }
            }
            20 => {
                if self.w.popup.busy == 0 {
                    self.wt.st = 9;
                }
            }
            21 => {
                // the "ロード中です" popup up (jp_4AD30 past 1): the resume record is read, the popup closes,
                // the BGM fades
                if self.w.popup.busy == 1 {
                    return;
                }
                self.wp_close();
                match self.file.continue_slot() {
                    Some(i) => {
                        self.wt_loaded = Some(i);
                        game.sound.stop_bgm_rate(1.5);
                        self.wt.st = 23;
                    }
                    None => self.wt.st = 9,
                }
            }
            25 => match self.ws_step(game, r) {
                2 => {
                    let slot = self.w.save.loaded.take().unwrap_or(crate::save::auto_first());
                    self.wt_loaded_from_list(game, slot);
                }
                3 => self.wt_back(),
                _ => {}
            },
            26 => {
                if self.wc_step(game, r) == 1 {
                    self.wt_back();
                }
            }
            33 | 34 => {
                self.wt.ext = 1.0;
                self.wt.groups[1] = true;
                self.wt.st = 35;
            }
            35 => {
                self.wt.focus = Some(WitchTitle::ext_obj(self.wt.ext_cursor.max(0) as usize));
                self.wt_touch(game, r);
            }
            36 => {
                self.wt.ext -= 0.02 * f;
                if self.wt.ext <= 0.0 {
                    self.wt.ext = 0.0;
                    self.wt.groups[0] = true;
                    self.wt.st = 9;
                }
            }
            38..=40 => {
                self.wt.black += 0.02 * f;
                if self.wt.black > 1.2 {
                    self.wt.black = 1.0;
                    self.wt.active = false;
                    let st = self.wt.st;
                    self.wt.st = 0;
                    self.wt.out = Some(match st {
                        38 => WtOut::Cg,
                        39 => WtOut::Sound,
                        _ => WtOut::Archive,
                    });
                }
            }
            46 => {
                self.wt.black -= 0.014 * f;
                if self.wt.black < 0.0 {
                    self.wt.st = 35;
                    self.wt.groups[1] = true;
                }
            }
            55 => {
                self.wt.active = true;
                self.wt.splash = [1.0, 1.0];
                self.wt.st = 56;
            }
            56 => {
                self.wt.black -= f / 180.0;
                if self.wt.black < 0.0 {
                    self.wt.timer = 180;
                    self.wt.st = 57;
                }
            }
            57 | 59 | 67 => {
                let t = self.wt.timer;
                self.wt.timer -= 1;
                // the caution holds 60 frames before a button skips it
                if (self.wt.st == 57 && t > 180) || (self.wt.st == 67 && t > 300) || (self.wt.st == 59 && t > 0) {
                    return;
                }
                if std::mem::take(&mut self.wt_any) {
                    self.wt.timer = 0;
                } else if self.wt.timer > 0 {
                    return;
                }
                self.wt.st += 1;
            }
            58 => {
                self.wt.splash[1] -= f / 60.0;
                if self.wt.splash[1] < 0.0 {
                    self.wt.splash[1] = 0.0;
                    self.wt.st = 59;
                    self.wt.timer = 0;
                }
            }
            60 => {
                self.wt.splash[0] -= f / 60.0;
                if self.wt.splash[0] < 0.0 {
                    self.wt.splash[0] = 0.0;
                    self.wt.st = 14;
                }
            }
            61 => {
                self.wt.splash = [0.0, 0.0];
                self.wt.st = 62;
                self.wt.black = 1.0;
            }
            62 => {
                self.wt.black -= f / 60.0;
                if self.wt.black < 0.0 {
                    self.wt.black = 0.0;
                    // jp_17C0(4): movie/logo.mp4
                    game.gallery_movie = true;
                    game.play_movie(r, "logo");
                    game.movie_by_ui = true;
                    self.wt.movie = true;
                    self.wt_any = false;
                    self.wt.st = 63;
                }
            }
            63 => {
                if !game.movie_active() {
                    self.wt.splash[1] = 0.0;
                    self.wt.st = 64;
                } else if game.movie_ready() {
                    self.wt.splash[1] = 1.0;
                    if std::mem::take(&mut self.wt_any) {
                        game.stop_movie();
                        self.wt.active = true;
                        self.wt.st = 64;
                    }
                }
            }
            64 => {
                if game.movie_active() {
                    return;
                }
                self.wt.movie = false;
                game.movie_by_ui = false;
                game.gallery_movie = false;
                self.wt.black += f / 60.0;
                if self.wt.black > 1.0 {
                    self.wt.st = 65;
                    self.wt.black = 1.0;
                }
            }
            65 => {
                self.wt.splash = [1.0, 0.0];
                self.wt.active = true;
                self.wt.st = 66;
                self.wt.black = 1.0;
            }
            66 => {
                self.wt.black -= f / 60.0;
                if self.wt.black < 0.0 {
                    self.wt.timer = 360;
                    self.wt_any = false;
                    self.wt.st = 67;
                }
            }
            68 => {
                self.wt.black += f / 60.0;
                if self.wt.black > 1.0 {
                    self.wt.cursor = -1;
                    self.wt.fade = 0.0;
                    self.wt.st = 1;
                    self.wt.black = 1.0;
                }
            }
            _ => {}
        }
    }

    /// A button or a touch during a splash (states 57 / 59 / 63 / 67: any pad button, a new touch).
    fn wt_any_input(&mut self) -> bool {
        if matches!(self.wt.st, 55..=68) {
            self.wt_any = true;
            return true;
        }
        false
    }

    /// The first enabled entry from `i` going by `d` (wrapping past the ends when `wrap`), else `i`.
    fn wt_move(on: &[bool], i: i32, d: i32, wrap: bool) -> i32 {
        let n = on.len() as i32;
        let mut c = i;
        for _ in 0..n {
            let next = c + d;
            c = if next < 0 || next >= n {
                if !wrap {
                    return if on[i.clamp(0, n - 1) as usize] { i } else { c };
                }
                next.rem_euclid(n)
            } else {
                next
            };
            if on[c as usize] {
                return c;
            }
        }
        i
    }

    /// Controller input on the title (states 11 / 35; the splashes take any button).
    pub(in crate::ui) fn wt_pad(&mut self, game: &mut Game, r: &Renderer, b: super::super::Pad, repeat: bool) {
        use super::super::Pad;
        if self.wt_any_input() || self.w.popup.busy != 0 {
            return;
        }
        let ext = self.wt.st == 35;
        if !(self.wt.st == 11 || ext) {
            return;
        }
        let fresh = !repeat;
        // masks [6] ZR up, [7] R down (both wrap), [8] L to the first entry, left / right to the first / last
        let (d, wrap) = match b {
            Pad::Up | Pad::LUp => (-1, fresh),
            Pad::Down | Pad::LDown => (1, fresh),
            Pad::ZR if fresh => (-1, true),
            Pad::R if fresh => (1, true),
            _ => (0, false),
        };
        let (on, cur): (Vec<bool>, i32) =
            if ext { (self.wt.ext_on.to_vec(), self.wt.ext_cursor) } else { (self.wt.on.to_vec(), self.wt.cursor) };
        let next = if d != 0 {
            Some(Self::wt_move(&on, cur.max(0), d, wrap))
        } else if fresh && matches!(b, Pad::Left | Pad::LLeft | Pad::L) {
            on.iter().position(|&e| e).map(|i| i as i32)
        } else if fresh && matches!(b, Pad::Right | Pad::LRight) {
            on.iter().rposition(|&e| e).map(|i| i as i32)
        } else {
            None
        };
        if let Some(n) = next {
            if n != cur {
                if ext {
                    self.wt.ext_cursor = n;
                } else {
                    self.wt.cursor = n;
                }
                self.snd(game, SND_MOVE);
            }
            return;
        }
        if !fresh {
            return;
        }
        match b {
            Pad::A => self.wt_activate(game, r),
            Pad::B if ext => {
                self.snd(game, SND_NG);
                self.wt.groups[1] = false;
                self.wt.st = 36;
            }
            Pad::B => {
                // state 11 cancel: the menu starts over from state 4 (cursor back to its default)
                self.wt.groups[0] = false;
                let bgm = if self.wt.complete { 42 } else { 65 };
                if game.sound.bgm_state().is_none_or(|(id, _)| id != bgm) {
                    game.play_bgm(bgm, 1.0, 0.0);
                }
                self.wt.st = 12;
            }
            _ => {}
        }
    }

    /// States 11 / 35's touch (jp_8E200): a release after 4..24 frames (touch flag 2) on another entry moves
    /// the cursor there (sound 2) and nothing is activated that frame; otherwise a release after 7..199 frames
    /// (0x4000, the byte test at +0x35) on the cursor's entry activates it. knife: the mouse hovering onto an
    /// entry moves the cursor as the pad does.
    fn wt_touch(&mut self, game: &mut Game, r: &Renderer) {
        let t = self.w.inp.touch;
        let hover = self.w.inp.hovering();
        if (t & 0x4002 == 0 && !hover) || self.w.popup.busy != 0 {
            return;
        }
        let ext = self.wt.st == 35;
        let [ux, uy] = self.w.inp.pos;
        let Some(hit) = self.wt_hit(ux * W / 640.0, uy * H / 448.0, ext) else { return };
        let cur = if ext { &mut self.wt.ext_cursor } else { &mut self.wt.cursor };
        if (t & 2 != 0 || hover) && *cur != hit as i32 {
            *cur = hit as i32;
            self.snd(game, SND_MOVE);
        } else if t & 0x4000 != 0 && *cur == hit as i32 {
            self.wt_activate(game, r);
        }
    }

    /// A click: during a splash it counts as the new touch that moves it on.
    pub(in crate::ui) fn wt_click(&mut self) {
        self.wt_any_input();
    }

    /// The entry under (x, y) canvas px (jp_AE840 over the input-enabled group).
    fn wt_hit(&self, x: f32, y: f32, ext: bool) -> Option<usize> {
        let (n, base, rect, group) = if ext { (5, 7, RECT_EXTRA, 1) } else { (7, 0, RECT_MAIN, 0) };
        if !self.wt.groups[group] {
            return None;
        }
        (0..n).find(|&i| {
            let o = &self.wt.objs[base + i];
            let on = if ext { self.wt.ext_on[i] } else { self.wt.on[i] };
            on && super::super::rect_hit(Self::object_rect(o.pos, rect), (x, y))
        })
    }

    /// A on the cursor (LABEL_148 / state 35's confirm).
    fn wt_activate(&mut self, game: &mut Game, r: &Renderer) {
        if self.wt.st == 35 {
            let i = self.wt.ext_cursor;
            match i {
                0..=2 => {
                    self.snd(game, SND_OK);
                    self.wt.groups[1] = false;
                    if i == 0 {
                        // sub_745D0(0): G009 = 0
                        game.set_var_pub("G009", 0);
                    }
                    self.wt.st = [40, 38, 39][i as usize];
                }
                4 => {
                    self.snd(game, SND_NG);
                    self.wt.groups[1] = false;
                    self.wt.st = 36;
                }
                _ => {}
            }
            return;
        }
        let c = self.wt.cursor;
        if c != -1 && c != 5 {
            self.wt.objs[WitchTitle::main_obj(c.max(0) as usize)].press = 10;
        }
        self.wt.groups[0] = false;
        match c {
            -1 | 1 => {
                if c == -1 {
                    self.wt.cursor = 1;
                }
                self.snd(game, SND_OK);
                game.sound.stop_bgm_rate(1.5);
                self.wt.st = 14;
            }
            0 => {
                if self.wt.on[0] {
                    // "ロード中です" (SYSMES 160) held until the record is read (states 21 / 23)
                    self.snd(game, SND_OK);
                    self.wp_open(game, 160, 480);
                    self.wp_hold();
                    self.wt.st = 21;
                } else {
                    // "セーブデータがありません" (169)
                    self.snd(game, SND_NG);
                    self.wp_open(game, 169, 480);
                    self.wt.st = 20;
                }
            }
            2 => {
                // jp_4EA10(0), sub_31A70, sub_323A0(0): the load screen over the title
                self.wt.st = 25;
                self.ws_open(game, r, false);
                self.w.save.title = true;
            }
            3 => {
                // loc_D350: the config over the title
                self.wt.st = 26;
                self.wc_open(game, r);
            }
            4 => {
                self.snd(game, SND_NG);
                self.wt.groups[0] = true;
            }
            5 => {
                self.snd(game, SND_OK);
                self.wt.st = 33;
            }
            _ => self.wt.st = 14,
        }
    }

    /// Back on the title from the load screen (state 25 result 3) or the config (state 26).
    pub(in crate::ui) fn wt_back(&mut self) {
        if matches!(self.wt.st, 25 | 26) {
            self.wt.st = 9;
            self.wt.groups[0] = true;
        }
    }

    /// The load screen loaded `slot` over the title (state 25 result 2): the BGM fades, then state 23.
    pub(in crate::ui) fn wt_loaded_from_list(&mut self, game: &mut Game, slot: usize) {
        self.wt_loaded = Some(slot);
        game.sound.stop_bgm_rate(1.5);
        self.wt.active = true;
        self.wt.st = 23;
    }

    /// jp_8D5D0 (and the splashes sub_6E550 / sub_6E700).
    pub(in crate::ui) fn wt_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let full = [0.0, 0.0, W, H];
        out.push(Quad::rect(r.white.clone(), full, [0.0, 0.0, 0.0, 1.0]));
        let wt = &self.wt;
        let st = wt.st;
        let f = 1.0;
        if matches!(st, 55..=60) {
            if wt.black < 1.0 {
                out.push(Quad::rect(r.white.clone(), full, [0.0, 0.0, 0.0, 1.0]));
                self.wt_quad(out, FIRST_B, full, None, wt.splash[0]);
                self.wt_quad(out, FIRST_W, full, None, wt.splash[1]);
            }
            Self::wt_black(r, out, wt.black);
            return;
        }
        if matches!(st, 61..=68) {
            if wt.black < 1.0 {
                let c = SPLASH_BG[(st - 61) as usize];
                out.push(Quad::rect(r.white.clone(), full, [c, c, c, 1.0]));
                if wt.movie {
                    out.extend(game.movie_quad());
                }
                if wt.splash[0] > 0.0 {
                    self.wt_quad(out, CAUTION, full, None, wt.splash[0]);
                }
                if wt.splash[1] > 0.0 && st == 64 {
                    self.wt_quad(out, TYPEMOON, full, None, 1.0);
                }
            }
            Self::wt_black(r, out, wt.black);
            return;
        }
        if !wt.active || wt.wait > 0 || st == 0 || game.var("G010") == 0 {
            return;
        }
        let k = usize::from(wt.complete);
        // the menu follows the fade-in until it is shown
        let lift = (1.0 - wt.black).min(1.0);
        if self.wt.menu_a < lift {
            self.wt.menu_a = lift;
        }
        let wt = &self.wt;
        let menu_a = wt.menu_a;
        self.wt_quad(out, BG[k], full, None, 1.0);
        // texture 215 at (242, ((1 - a)^2 * 50 + 152) * 544 / 1080) in 960x544 units
        if let Some(t) = self.wt.tex(LOGO[k]) {
            let (w, h) = (t.width as f32, t.height as f32);
            let y = (1.0 - menu_a) * (1.0 - menu_a) * 50.0 + 152.0;
            self.wt_quad(out, LOGO[k], [484.0, y, w, h], None, 1.0);
        }
        let lang = game.assets.language().min(3);
        if menu_a > 0.0 {
            if menu_a >= 1.0 {
                self.wt.cur_a = (self.wt.cur_a + f / 60.0).min(1.0);
                let mut a = self.wt.anim + f * 0.5;
                if a >= 40.0 {
                    a -= 40.0;
                }
                self.wt.anim = a;
                let wt = &self.wt;
                let c = wt.cursor;
                let shown = (c == 0 && wt.on[0]) || matches!(c, 1 | 2 | 3 | 5);
                if shown && let Some(row) = usize::try_from(CUR_ROW[c as usize]).ok() {
                    let p = CUR_POS[lang][row];
                    let y = if wt.on[0] { p[3] } else { p[2] };
                    self.wt_cursor(out, CURSOR[k], [p[0], p[1]], y, wt.cur_a * (menu_a - wt.ext));
                }
            }
            // objects 205..211
            let wt = &self.wt;
            let count = wt.count as f32;
            let mut place = Vec::new();
            for i in 0..7 {
                let v = wt.order[i];
                if v < 0 {
                    place.push((i, [0.0, -100.0], 0.0));
                    continue;
                }
                let pos = WitchTitle::row_pos(wt.count, v);
                let shown = (2.0 * (1.0 - wt.ext) - v as f32 / 7.0).clamp(0.0, 1.0);
                let (mut x, mut y) = if shown == 0.0 { (-500.0, 0.0) } else { (pos[0], pos[1]) };
                if wt.ext <= 0.0 {
                    (x, y) = (pos[0], pos[1]);
                }
                let a = match st {
                    36 => 1.0 - 1.2 * wt.ext,
                    34 => {
                        y += wt.ext * 30.0;
                        1.0 - 2.0 * wt.ext
                    }
                    _ => menu_a,
                };
                place.push((i, [x, y], (2.0 * a - (v + 1) as f32 / count).clamp(0.0, 1.0)));
            }
            for (i, pos, a) in place {
                let o = &mut self.wt.objs[WitchTitle::main_obj(i)];
                o.pos = pos;
                o.alpha = a;
            }
            for (i, &row) in MAIN_ROW.iter().enumerate() {
                self.wt_obj(out, WitchTitle::main_obj(i), MENU, row);
            }
        }
        if self.wt.ext > 0.0 {
            let wt = &self.wt;
            self.wt.ext_cur_a = (wt.ext * wt.ext).min(1.0);
            let wt = &self.wt;
            if wt.ext_cursor != 3
                && let Some(row) = usize::try_from(EXT_ROW[wt.ext_cursor.clamp(0, 4) as usize]).ok()
            {
                let p = EXT_POS[lang][row];
                self.wt_cursor(out, CURSOR[k], [p[0], p[1]], p[2], wt.ext_cur_a);
            }
            let wt = &self.wt;
            let mut place = Vec::new();
            for i in 0..5 {
                let v = wt.ext_order[i];
                if v < 0 {
                    place.push((i, [0.0, -100.0], 0.0));
                    continue;
                }
                let pos = WitchTitle::row_pos(wt.ext_count, v);
                let (x, mut y) = (pos[0], pos[1]);
                let a = match st {
                    36 => {
                        y += (1.0 - wt.ext) * 30.0;
                        let b = 1.0 - 2.0 * (1.0 - wt.ext);
                        2.0 * b - v as f32 / wt.count as f32
                    }
                    34 => {
                        let b = 1.0 - 1.2 * (1.0 - wt.ext);
                        2.0 * b - v as f32 / wt.count as f32
                    }
                    _ => 1.0,
                };
                place.push((i, [x, y], a.clamp(0.0, 1.0)));
            }
            for (i, pos, a) in place {
                let o = &mut self.wt.objs[WitchTitle::ext_obj(i)];
                o.pos = pos;
                o.alpha = a;
            }
            for (i, &row) in EXTRA_ROW.iter().enumerate() {
                self.wt_obj(out, WitchTitle::ext_obj(i), EXTRA, row);
            }
        } else {
            for i in 0..5 {
                self.wt.objs[WitchTitle::ext_obj(i)].alpha = 0.0;
            }
        }
        // texture 217 bottom-centred at (480, 516.8) in 960x544 units
        if let Some(t) = self.wt.tex(SINCE) {
            let (w, h) = (t.width as f32, t.height as f32);
            let y = 516.8 * H / 544.0;
            self.wt_quad(out, SINCE, [960.0 - w / 2.0, y - h, w, h], None, 1.0);
        }
        Self::wt_black(r, out, self.wt.black);
    }

    fn wt_black(r: &Renderer, out: &mut Vec<Quad>, a: f32) {
        if a > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, a.clamp(0.0, 1.0)]));
        }
    }

    fn wt_quad(&self, out: &mut Vec<Quad>, i: usize, dst: [f32; 4], src: Option<[f32; 4]>, a: f32) {
        let Some(t) = self.wt.tex(i) else { return };
        let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
        let [x, y, w, h] = src.unwrap_or([0.0, 0.0, t.width as f32, t.height as f32]);
        let mut q = Quad::rect(t.clone(), dst, [1.0, 1.0, 1.0, a]).with_uv_rect(x / tw, y / th, (x + w) / tw, (y + h) / th);
        q.blend = Blend::Alpha;
        out.push(q);
    }

    /// The titleCursor pair (texture 216: 20 x 2 frames of 60 x 75 in 60 x 80 cells, frame = the animation
    /// counter) at xl and, mirrored, at xr (960x544 units, scale 0.5 / -0.5).
    fn wt_cursor(&self, out: &mut Vec<Quad>, tex: usize, x: [f32; 2], y: f32, a: f32) {
        if a <= 0.0 {
            return;
        }
        let k = self.wt.anim as i32;
        let src = [(60 * (k % 20)) as f32, (80 * (k / 20)) as f32, 60.0, 75.0];
        let y = y * H / 544.0;
        self.wt_quad(out, tex, [x[0] * 2.0, y, 60.0, 75.0], Some(src), a);
        let n = out.len();
        self.wt_quad(out, tex, [x[1] * 2.0, y, -60.0, 75.0], Some(src), a);
        let _ = n;
    }

    /// A menu object (jp_AEEC0): its state's frame, then the state before at the crossfade weight.
    fn wt_obj(&self, out: &mut Vec<Quad>, i: usize, tex: usize, row: f32) {
        let o = &self.wt.objs[i];
        if o.alpha <= 0.0 {
            return;
        }
        let x = o.pos[0] * W / 640.0;
        let y = o.pos[1] * H / 448.0;
        let frame = |s: u8| match s {
            1 | 3 => [616.0, row, 592.0, 104.0],
            4 => [8.0, row + 552.0, 592.0, 104.0],
            _ => [8.0, row, 592.0, 104.0],
        };
        let f = frame(o.state);
        self.wt_quad(out, tex, [x, y, f[2], f[3]], Some(f), o.alpha);
        if o.w > 0.0 && o.prev != o.state {
            let f = frame(o.prev);
            self.wt_quad(out, tex, [x, y, f[2], f[3]], Some(f), o.alpha * o.w);
        }
    }
}
