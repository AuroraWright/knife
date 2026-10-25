//! Witch on the Holy Night's Archive (書庫, the title's Extra menu): sub_6390 runs it (jd_B2D550 states 15 / 16,
//! its own states in HIDWORD(qword_6379C0)), sub_2DE0 sets it up, sub_4610 takes the shelf's input, sub_5D80
//! the scene panel's and sub_3100 draws it (task 23 through jp_424D0).
//!
//! A bookshelf of the 20 chapters (objects 326..345, the back label 325): a chapter's book shows once one of
//! its scenes is open (sub_2CD0: the flags of its row, or the ending), the pad steps through the books
//! (sub_4610's order tables) and the focused book glows. A or a tap on it opens its panel: the chapter's
//! scenes four at a time (title, the English title under it, a read / unread icon), a scrollbar, the picked
//! scene's thumbnail, 読む and 戻す. 読む fades to black and plays the scene through 528_ARCHIVE (id 3000, G008 =
//! the scene id, F024 / F026 set by the script); its end comes back here with the panel open on it (state 16,
//! sub_7320 by F026). Leaving goes back to the title's Extra menu, or with G009 set (the TeaTime menu's
//! archive button) replays that TeaTime (jp_48AC0 mode 5 state 11).

use knife_render::{Blend, Quad, RenderTarget, Renderer, Shade, TexRef};

use super::super::{H, Screen, Ui, W};
use super::{DOWN, LEFT, MASK, Pen, RIGHT, UP, WScr, objtab};
use crate::game::Game;

/// dword_25A248 (10 ints a scene, 9 a chapter): the scene id (G008, 528_ARCHIVE jumps by it), its thumbnail
/// (ARCHTHUMB_PARTS), its title (SYSMES), its icon cell (texture 4248), its read flag (an RDST bit) and the flags
/// that open it (to the first negative).
const SCENES: [&[[i16; 10]]; 20] = [
    &[
        [200, 0, 214, 8, 0, 0, -1, -1, -1, -1],
        [201, 1, 215, 9, 1, 1, -1, -1, -1, -1],
        [202, 2, 216, 9, 3, 3, -1, -1, -1, -1],
        [204, 4, 217, 9, 4, 4, -1, -1, -1, -1],
        [205, 5, 218, 4, 5, 5, -1, -1, -1, -1],
    ],
    &[
        [206, 6, 219, 5, 6, 5, -1, -1, -1, -1],
        [207, 7, 220, 5, 7, 5, -1, -1, -1, -1],
        [208, 8, 221, 5, 8, 5, -1, -1, -1, -1],
        [209, 9, 222, 5, 9, 5, -1, -1, -1, -1],
    ],
    &[
        [210, 10, 223, 0, 11, 11, -1, -1, -1, -1],
        [212, 12, 224, 0, 13, 13, -1, -1, -1, -1],
        [214, 14, 225, 0, 14, 14, -1, -1, -1, -1],
    ],
    &[
        [215, 15, 226, 1, 15, 15, -1, -1, -1, -1],
    ],
    &[
        [216, 16, 227, 5, 16, 16, -1, -1, -1, -1],
        [217, 17, 228, 4, 17, 17, -1, -1, -1, -1],
        [218, 18, 229, 6, 18, 18, -1, -1, -1, -1],
        [219, 19, 230, 6, 19, 19, -1, -1, -1, -1],
    ],
    &[
        [220, 20, 231, 2, 21, 21, -1, -1, -1, -1],
        [225, 22, 232, 2, 23, 23, -1, -1, -1, -1],
        [227, 24, 233, 2, 26, 26, -1, -1, -1, -1],
        [230, 27, 234, 2, 28, 28, -1, -1, -1, -1],
        [221, 29, 235, 2, 31, 31, -1, -1, -1, -1],
    ],
    &[
        [232, 32, 236, 2, 32, 32, -1, -1, -1, -1],
        [240, 33, 237, 2, 34, 34, -1, -1, -1, -1],
        [242, 35, 238, 2, 35, 35, -1, -1, -1, -1],
        [243, 36, 239, 2, 38, 38, -1, -1, -1, -1],
        [246, 39, 240, 2, 39, 39, -1, -1, -1, -1],
        [247, 40, 241, 2, 42, 42, -1, -1, -1, -1],
        [235, 43, 242, 2, 43, 43, -1, -1, -1, -1],
        [236, 44, 243, 2, 45, 45, -1, -1, -1, -1],
        [238, 46, 244, 2, 47, 47, -1, -1, -1, -1],
    ],
    &[
        [248, 48, 245, 7, 48, 48, -1, -1, -1, -1],
        [249, 49, 246, 5, 50, 50, -1, -1, -1, -1],
        [251, 51, 247, 0, 51, 51, -1, -1, -1, -1],
        [252, 52, 248, 1, 53, 53, -1, -1, -1, -1],
        [254, 54, 249, 4, 55, 55, -1, -1, -1, -1],
        [256, 56, 250, 7, 56, 56, -1, -1, -1, -1],
    ],
    &[
        [257, 57, 251, 3, 57, 57, -1, -1, -1, -1],
        [259, 58, 252, 1, 60, 60, -1, -1, -1, -1],
        [261, 60, 253, 1, 64, 64, -1, -1, -1, -1],
        [265, 64, 254, 7, 65, 64, -1, -1, -1, -1],
    ],
    &[
        [266, 65, 255, 6, 66, 66, -1, -1, -1, -1],
        [267, 66, 256, 3, 68, 68, -1, -1, -1, -1],
        [269, 68, 257, 5, 69, 69, -1, -1, -1, -1],
        [270, 69, 258, 1, 71, 71, -1, -1, -1, -1],
        [272, 71, 259, 4, 72, 71, -1, -1, -1, -1],
    ],
    &[
        [273, 72, 260, 2, 73, 71, -1, -1, -1, -1],
    ],
    &[
        [274, 73, 261, 4, 74, 74, -1, -1, -1, -1],
        [275, 74, 262, 4, 76, 76, -1, -1, -1, -1],
        [277, 76, 263, 7, 78, 78, -1, -1, -1, -1],
        [279, 78, 264, 1, 81, 81, -1, -1, -1, -1],
        [282, 81, 265, 0, 83, 83, -1, -1, -1, -1],
    ],
    &[
        [329, 128, 266, 4, 129, 72, -1, -1, -1, -1],
        [330, 129, 267, 4, 130, 72, -1, -1, -1, -1],
        [331, 130, 268, 4, 131, 72, 128, -1, -1, -1],
        [332, 131, 269, 4, 132, 72, 128, -1, -1, -1],
        [333, 132, 270, 4, 133, 72, 206, -1, -1, -1],
        [334, 133, 271, 4, 134, 72, 206, -1, -1, -1],
    ],
    &[
        [284, 83, 272, 4, 84, 84, -1, -1, -1, -1],
        [285, 84, 273, 7, 87, 87, -1, -1, -1, -1],
        [288, 87, 274, 7, 88, 88, -1, -1, -1, -1],
        [289, 88, 275, 4, 90, 90, -1, -1, -1, -1],
        [291, 90, 276, 3, 92, 92, -1, -1, -1, -1],
    ],
    &[
        [293, 92, 277, 1, 95, 95, -1, -1, -1, -1],
        [296, 95, 278, 1, 98, 98, -1, -1, -1, -1],
        [299, 98, 279, 9, 99, 99, -1, -1, -1, -1],
        [300, 99, 280, 9, 101, 101, -1, -1, -1, -1],
    ],
    &[
        [302, 101, 281, 2, 110, 110, -1, -1, -1, -1],
        [317, 110, 282, 2, 119, 119, -1, -1, -1, -1],
    ],
    &[
        [320, 119, 283, 3, 127, 127, -1, -1, -1, -1],
    ],
    &[
        [336, 134, 284, 7, 136, 128, -1, -1, -1, -1],
    ],
    &[
        [1058, 135, 285, 7, 206, 6, 7, 8, 9, 136],
    ],
    &[
        [328, 127, 286, 8, 128, 128, -1, -1, -1, -1],
    ],
];

/// sub_7320: by F026 - 1 (528_ARCHIVE sets it to the replayed scene, 1..73): the chapter (dword_25BEEC), the
/// panel cursor (unk_25C010), the pick (unk_25C134) and the scroll (unk_25C258).
const RESTORE: [[i8; 4]; 73] = [
    [1, 0, 0, 0], [1, 1, 1, 0], [1, 2, 2, 0], [1, 3, 3, 0], [1, 3, 4, 1], [2, 0, 0, 0],
    [2, 1, 1, 0], [2, 2, 2, 0], [2, 3, 3, 0], [3, 0, 0, 0], [3, 1, 1, 0], [3, 2, 2, 0],
    [4, 0, 0, 0], [5, 0, 0, 0], [5, 1, 1, 0], [5, 2, 2, 0], [5, 3, 3, 0], [6, 0, 0, 0],
    [6, 1, 1, 0], [6, 2, 2, 0], [6, 3, 3, 0], [6, 3, 4, 1], [7, 0, 0, 0], [7, 1, 1, 0],
    [7, 2, 2, 0], [7, 3, 3, 0], [7, 3, 4, 1], [7, 3, 5, 2], [7, 3, 6, 3], [7, 3, 7, 4],
    [7, 3, 8, 5], [8, 0, 0, 0], [8, 1, 1, 0], [8, 2, 2, 0], [8, 3, 3, 0], [8, 3, 4, 1],
    [8, 3, 5, 2], [9, 0, 0, 0], [9, 1, 1, 0], [9, 2, 2, 0], [9, 3, 3, 0], [10, 0, 0, 0],
    [10, 1, 1, 0], [10, 2, 2, 0], [10, 3, 3, 0], [10, 3, 4, 1], [11, 0, 0, 0], [12, 0, 0, 0],
    [12, 1, 1, 0], [12, 2, 2, 0], [12, 3, 3, 0], [12, 3, 4, 1], [13, 0, 0, 0], [13, 1, 1, 0],
    [13, 2, 2, 0], [13, 3, 3, 0], [13, 3, 4, 1], [13, 3, 5, 2], [14, 0, 0, 0], [14, 1, 1, 0],
    [14, 2, 2, 0], [14, 3, 3, 0], [14, 3, 4, 1], [15, 0, 0, 0], [15, 1, 1, 0], [15, 2, 2, 0],
    [15, 3, 3, 0], [16, 0, 0, 0], [16, 1, 1, 0], [17, 0, 0, 0], [18, 0, 0, 0], [19, 0, 0, 0],
    [20, 0, 0, 0],
];

/// dword_25A1F4: the shelf cursor's object (0 the back label, 1..20 the chapters' books).
const CH_OBJ: [i32; 21] = [325, 333, 331, 329, 327, 326, 328, 330, 332, 334, 342, 340, 338, 336, 335, 337, 339, 341, 343, 344, 345];
/// dword_25BE80: an object's (325..345) shelf cursor.
const OBJ_CH: [i32; 21] = [0, 5, 4, 6, 3, 7, 2, 8, 1, 9, 14, 13, 15, 12, 16, 11, 17, 10, 18, 19, 20];
/// off_632308: the panel's chapter title.
const TITLE_TEX: [i32; 20] = [4254, 4255, 4256, 4257, 4258, 4259, 4260, 4261, 4262, 4263, 4264, 4265, 4253, 4266, 4267, 4268, 4269, 4253, 4253, 4269];
/// sub_4610's orders: UP from the back label, DOWN from it, UP from the lower shelf, DOWN from the upper.
const UP_BACK: [i32; 20] = [14, 15, 16, 17, 18, 19, 20, 13, 12, 11, 10, 5, 6, 7, 8, 9, 4, 3, 2, 1];
const DOWN_BACK: [i32; 20] = [5, 6, 7, 8, 9, 4, 3, 2, 1, 14, 15, 16, 17, 18, 19, 20, 13, 12, 11, 10];
const UP_LOW: [i32; 9] = [5, 6, 7, 8, 9, 4, 3, 2, 1];
const DOWN_TOP: [i32; 11] = [14, 15, 16, 17, 18, 19, 20, 13, 12, 11, 10];
/// sub_3100: the patch behind chapter a + 1's place while chapter a has a book and it has none (by a, 0-based).
const GAPS: [(usize, i32, f32, f32); 18] = [
    (7, 4233, 514.3334, 73.837), (6, 4232, 410.6667, 73.837), (5, 4231, 378.6667, 73.837), (4, 4230, 345.6667, 73.837),
    (3, 4229, 288.6667, 65.9556), (2, 4228, 267.0, 73.837), (1, 4227, 225.0, 73.837), (0, 4226, 154.0, 84.6222),
    (18, 4243, 564.6667, 265.4815), (17, 4242, 553.0, 264.6519), (16, 4241, 529.3334, 257.6), (15, 4240, 507.6667, 255.5259),
    (14, 4239, 439.0, 254.2815), (13, 4238, 376.6667, 254.2815), (12, 4237, 288.3333, 253.8667), (11, 4236, 274.0, 252.2074),
    (10, 4235, 182.3333, 242.2518), (9, 4234, 157.3333, 251.7926),
];
/// unk_2EA200 / unk_2EA208: texture 4250's cursor and pick bars.
const BAR_CUR: [f32; 4] = [16.0, 16.0, 780.0, 75.0];
const BAR_PICK: [f32; 4] = [16.0, 88.0, 780.0, 147.0];
/// 0x2EA218: the scrollbar knob's texels (normal, dragged, -, the whole bar).
const KNOB: [[f32; 4]; 4] = [[1.0, 1.0, 27.0, 2.0], [33.0, 1.0, 59.0, 2.0], [65.0, 1.0, 91.0, 2.0], [97.0, 1.0, 123.0, 2.0]];

/// The scene's row (`ch` 1..20, `k` 0..8) when the table has one.
fn row(ch: i32, k: i32) -> Option<&'static [i16; 10]> {
    let ch = usize::try_from(ch - 1).ok()?;
    SCENES.get(ch)?.get(usize::try_from(k).ok()?)
}

/// jp_8D180(k): RDST bit k (the words G130..G145).
fn read(game: &Game, k: i32) -> bool {
    (0..256).contains(&k) && (game.var(&format!("G{:03}", 130 + k / 16)) >> (k % 16)) & 1 != 0
}

/// sub_2CD0: the scene is open after the ending (jp_8D1C0) or with all its flags set.
fn open(game: &Game, complete: bool, r: &[i16; 10]) -> bool {
    complete || r[5..].iter().take_while(|&&f| f >= 0).all(|&f| read(game, f.into()))
}

/// The Archive's state (sub_6390's globals; the cursor, the pick and the scroll stay from visit to visit).
pub struct WArc {
    /// HIDWORD(qword_6379C0)
    st: i32,
    /// sub_6390's argument: 2 back from a replay (jd_B2D550 16), else 1
    arg: i32,
    /// LODWORD(qword_6379C0): the black over it
    black: f32,
    /// dword_6379AC: the panel's alpha
    pa: f32,
    /// dword_6379A8: the shelf cursor (0 the back label, 1..20 the chapters)
    ch: i32,
    /// LODWORD / HIDWORD(qword_6379B0): the panel cursor (0..3 the rows, 4 読む, 5 戻す) and its first row
    cur: i32,
    scroll: i32,
    /// dword_2EA1F0: the picked scene (-1 none)
    sel: i32,
    /// dword_2EA1F4: the scrollbar's drag (0..1, -1 none)
    drag: f32,
    /// qword_6379B8: the book's and the panel's pulse
    ph: [f32; 2],
    /// byte_6379A4: drawn
    active: bool,
    /// off_6322E0 / off_6322F0 / off_6322E8: each chapter's open scenes, its rows and which are open
    count: [i32; 20],
    rows: [i32; 20],
    avail: [[bool; 9]; 20],
    /// the titles in the game's language and in English (the subtitles)
    titles: Vec<String>,
    titles_en: Vec<String>,
    /// the glow's surface (jo_AE0C40 + 856)
    rt: Option<TexRef>,
}

impl Default for WArc {
    fn default() -> Self {
        Self {
            st: 0,
            arg: 1,
            black: 0.0,
            pa: 0.0,
            ch: 0,
            cur: 0,
            scroll: 0,
            sel: -1,
            drag: -1.0,
            ph: [0.0; 2],
            active: false,
            count: [0; 20],
            rows: [0; 20],
            avail: [[false; 9]; 20],
            titles: Vec::new(),
            titles_en: Vec::new(),
            rt: None,
        }
    }
}

impl WArc {
    /// A chapter (1..20) with an open scene.
    fn has(&self, ch: i32) -> bool {
        (1..=20).contains(&ch) && self.count[(ch - 1) as usize] > 0
    }

    fn avail(&self, ch: i32, k: i32) -> bool {
        (1..=20).contains(&ch) && (0..9).contains(&k) && self.avail[(ch - 1) as usize][k as usize]
    }

    /// The open scenes of the cursor's chapter.
    fn n(&self) -> i32 {
        if (1..=20).contains(&self.ch) { self.count[(self.ch - 1) as usize] } else { 0 }
    }

    /// The panel's offset: up for the lower shelf.
    fn off(&self) -> f32 {
        if self.ch > 9 { -171.73 } else { 0.0 }
    }
}

/// What the Archive hands back (sub_6390's result through jp_42680).
enum Out {
    /// 3: the title's Extra menu, or the TeaTime replay with G009 set
    Leave,
    /// scene + 4: 528_ARCHIVE plays the scene
    Play(i32),
}

impl Ui {
    /// jp_47AE0 (jd_B2D550 15, `back` false) or jp_47A90 (16, back from a replay: the panel opens on the scene
    /// sub_7320 took from F026 at the story's end).
    pub(in crate::ui) fn warc_init(&mut self, game: &mut Game, back: bool) {
        let a = &mut self.w.arc;
        a.st = 0;
        a.arg = if back { 2 } else { 1 };
        a.titles = game.assets.sysmes();
        a.titles_en = game.assets.sysmes_in(1);
        self.w.guide.on = true;
        self.w.scr = WScr::Archive;
        self.screen = Screen::Witch;
        self.stack.clear();
    }

    /// sub_7320 (at the replay's end): the shelf and panel cursors for F026's scene.
    pub(in crate::ui) fn warc_restore(&mut self, f026: i32) {
        let a = &mut self.w.arc;
        let [ch, cur, sel, scroll] = usize::try_from(f026 - 1).ok().and_then(|i| RESTORE.get(i)).map_or([1, 0, 0, 0], |v| v.map(i32::from));
        a.ch = ch;
        a.sel = sel;
        a.cur = cur;
        a.scroll = scroll;
    }

    /// sub_2DE0: the resources, which scenes are open, the objects (sub_28F0), the pulses, the black.
    fn warc_setup(&mut self, game: &mut Game, r: &Renderer) {
        self.w.tex.load(game, r, objtab::RES_ARCHIVE);
        self.w.tex.load(game, r, objtab::RES_ARCHTHUMB);
        // the button guide's help_key / help_txt (textures 76 / 77, loaded with the story's display parts)
        if self.w.tex.get(76).is_none() {
            self.w.tex.load(game, r, objtab::RES_DISP);
        }
        let complete = super::title::complete(game);
        let a = &mut self.w.arc;
        for (c, rows) in SCENES.iter().enumerate() {
            a.rows[c] = rows.len() as i32;
            a.avail[c] = [false; 9];
            for (k, r) in rows.iter().enumerate() {
                a.avail[c][k] = open(game, complete, r);
            }
            a.count[c] = a.avail[c].iter().filter(|&&v| v).count() as i32;
        }
        self.warc_objects();
        let a = &mut self.w.arc;
        a.ph = [0.0; 2];
        a.active = true;
        a.black = 1.0;
    }

    /// sub_28F0: the ARCHIVE objects; a book whose neighbour has one too takes a narrower hit width, all at the
    /// screen scale; the focus on the book, or with the panel up on its cursor (a button's cursor goes back to
    /// the picked row).
    fn warc_objects(&mut self) {
        self.w.objs.install(objtab::ARCHIVE);
        let c = self.w.arc.count;
        let both = |a: usize| c[a] >= 1 && c[a + 1] >= 1;
        for (a, id, w) in [(0, 333, 99), (1, 331, 81), (2, 329, 48), (3, 327, 24), (9, 342, 96), (10, 340, 31), (11, 338, 106), (12, 336, 15)] {
            if both(a)
                && let Some(o) = self.w.objs.get(id)
            {
                o.rect[2] = w;
            }
        }
        for id in 325..=351 {
            self.w.objs.scale(id, 0.33333, 0.41481);
        }
        let a = &mut self.w.arc;
        if (0..=20).contains(&a.ch) {
            self.w.objs.focus = CH_OBJ[a.ch as usize];
        }
        if a.pa > 0.0 {
            if (a.cur == 4 || a.cur == 5) && a.sel >= 0 && row(a.ch, a.sel).is_some_and(|r| r[0] >= 1) {
                a.cur = a.sel - a.scroll;
            }
            if (0..=5).contains(&a.cur) {
                self.w.objs.focus = 346 + a.cur;
            }
        }
    }

    /// One frame of sub_6390; true once it handed over (to the title, a replay or a TeaTime).
    #[allow(clippy::approx_constant)]
    pub(in crate::ui) fn warc_step(&mut self, game: &mut Game, r: &Renderer) -> bool {
        let a = &mut self.w.arc;
        for (p, d) in a.ph.iter_mut().zip([0.05236, 0.069813]) {
            *p += d;
            if *p >= 6.2832 {
                *p -= 6.2832;
            }
        }
        let mut out = None;
        match a.st {
            0 => a.st = if a.arg == 2 { 2 } else { 1 },
            1 => {
                self.warc_setup(game, r);
                let a = &mut self.w.arc;
                a.pa = 0.0;
                a.st = 4;
            }
            2 => {
                self.warc_setup(game, r);
                let a = &mut self.w.arc;
                let id = CH_OBJ.get(a.ch as usize).copied().unwrap_or(-1);
                self.w.objs.flags(id, 2, 0);
                self.w.objs.group_flags(24, 0, 1);
                let a = &mut self.w.arc;
                a.pa = 1.0;
                a.st = 4;
            }
            3 => {
                self.warc_teardown();
                self.w.arc.st = 0;
                out = Some(Out::Leave);
            }
            4 => {
                a.black -= 0.02;
                if a.black <= 0.0 {
                    a.black = 0.0;
                    if a.pa <= 0.0 {
                        self.w.objs.group_flags(24, 1, 0);
                        self.w.objs.group_flags(25, 0, 1);
                        self.w.objs.group_flags(26, 0, 1);
                        self.w.arc.st = 6;
                    } else {
                        self.w.objs.group_flags(24, 0, 1);
                        self.w.objs.group_flags(25, 1, 0);
                        self.w.objs.group_flags(26, 1, 0);
                        let cur = self.w.arc.cur;
                        if (0..=5).contains(&cur) {
                            self.w.objs.focus = 346 + cur;
                        }
                        self.w.arc.st = 8;
                    }
                }
            }
            5 => {
                a.black += 0.02;
                if a.black >= 1.0 {
                    a.black = 1.0;
                    a.st = 3;
                }
            }
            6 => {
                let st = self.warc_shelf(game);
                self.w.arc.st = st;
                if st == 7 {
                    let id = CH_OBJ.get(self.w.arc.ch as usize).copied().unwrap_or(-1);
                    self.w.objs.flags(id, 2, 0);
                    self.w.objs.group_flags(24, 0, 1);
                } else if st == 5 {
                    for g in 24..=26 {
                        self.w.objs.group_flags(g, 0, 1);
                    }
                }
            }
            7 => {
                a.pa += 0.2;
                if a.pa >= 1.0 {
                    a.pa = 1.0;
                    a.st = 8;
                    let cur = a.cur;
                    self.w.objs.group_flags(25, 1, 0);
                    self.w.objs.group_flags(26, 1, 0);
                    self.w.objs.focus = 346 + cur;
                }
            }
            8 => {
                let st = self.warc_panel(game);
                self.w.arc.st = st;
                if st == 9 {
                    self.w.objs.group_flags(25, 0, 1);
                    self.w.objs.group_flags(26, 0, 1);
                }
            }
            9 => {
                a.pa -= 0.2;
                if a.pa <= 0.0 {
                    a.pa = 0.0;
                    a.st = 6;
                    let id = CH_OBJ.get(a.ch as usize).copied().unwrap_or(-1);
                    self.w.objs.group_flags(24, 1, 0);
                    self.w.objs.flags(id, 0, 2);
                    self.w.objs.focus = id;
                }
            }
            10 => {
                let scene = row(a.ch, a.sel).filter(|r| r[0] >= 1).map(|r| i32::from(r[0]));
                if !(1..=20).contains(&a.ch) {
                    a.st = 6;
                } else if a.sel < 0 || scene.is_none() || !a.avail(a.ch, a.sel) {
                    a.st = 8;
                } else {
                    a.black += 0.02;
                    if a.black >= 1.0 {
                        self.warc_teardown();
                        // jp_9C610: the BGM stops at once
                        game.sound.stop_bgm(0.0);
                        self.w.arc.st = 0;
                        out = scene.map(Out::Play);
                    }
                }
            }
            _ => {}
        }
        match out {
            None => false,
            Some(o) => {
                self.w.scr = WScr::None;
                self.warc_leave(game, r, o);
                true
            }
        }
    }

    /// Groups 24..26 off, not drawn, the black full (states 3 / 10).
    fn warc_teardown(&mut self) {
        for g in 24..=26 {
            self.w.objs.group_flags(g, 0, 1);
        }
        let a = &mut self.w.arc;
        a.active = false;
        a.black = 1.0;
        a.rt = None;
    }

    /// jp_48AC0 mode 4 on the Archive's result: a scene plays through 528_ARCHIVE (G008); leaving goes to the
    /// TeaTime that sent it here (mode 5 state 11: G009's scene, id 499 + G009, with the BGM fading at 1.5 a
    /// frame, G009 cleared and the TeaTime replay flag off_632648 set), else to the title's Extra menu (sub_6C170).
    fn warc_leave(&mut self, game: &mut Game, r: &Renderer, o: Out) {
        match o {
            Out::Play(scene) => self.start_replay(game, scene),
            Out::Leave => {
                let n = game.var("G009");
                let idx = (1..=13).contains(&n).then(|| game.assets.scene_by_id(n + 499)).flatten();
                match idx {
                    Some(idx) => {
                        game.sound.stop_bgm_rate(1.5);
                        game.reset();
                        game.set_var_pub("G009", 0);
                        game.teatime_replay = true;
                        game.start_scene(idx, None);
                        self.screen = Screen::Game;
                        self.stack.clear();
                    }
                    None => self.wt_from_extra(game, r),
                }
            }
        }
    }

    /// sub_4610: the shelf's input; 5 to leave, 7 to open the cursor's chapter, else 6.
    fn warc_shelf(&mut self, game: &mut Game) -> i32 {
        let inp = &self.w.inp;
        let (pressed, repeat, touch, shown) = (inp.pressed, inp.repeat, inp.touch, inp.shown);
        if pressed & MASK[1] != 0 || touch & 8 != 0 {
            game.play_sys_sound(1);
            return 5;
        }
        let hit = self.w.objs.hit;
        let on_book = (325..=345).contains(&hit);
        let mut activate = false;
        let a = &mut self.w.arc;
        if on_book && OBJ_CH[(hit - 325) as usize] != a.ch && (hit == 325 || a.has(OBJ_CH[(hit - 325) as usize])) {
            // the pointer's book becomes the cursor
            a.ch = OBJ_CH[(hit - 325) as usize];
            game.play_sys_sound(if hit == 325 { 2 } else { 9 });
            self.w.objs.focus = CH_OBJ[a.ch as usize];
            activate = pressed & MASK[0] != 0;
        } else if pressed & MASK[0] != 0 {
            if hit <= 324 && shown >= 1 && touch & 2 != 0 {
                return 6;
            }
            activate = true;
        } else if hit >= 325 && touch & 2 != 0 && OBJ_CH.get((hit - 325) as usize) == Some(&a.ch) {
            activate = true;
        }
        let a = &mut self.w.arc;
        if activate {
            return match a.ch {
                1..=20 => {
                    a.sel = -1;
                    a.cur = 0;
                    a.scroll = 0;
                    game.play_sys_sound(0);
                    7
                }
                0 => {
                    game.play_sys_sound(1);
                    5
                }
                _ => 6,
            };
        }
        let cur = a.ch;
        let first = |l: &[i32]| l.iter().copied().find(|&c| a.has(c));
        let to = if repeat & LEFT != 0 || repeat & RIGHT != 0 {
            // the next chapter with a book; a fresh press wraps
            let (step, fresh) = if repeat & LEFT != 0 { (-1, pressed & LEFT != 0) } else { (1, pressed & RIGHT != 0) };
            if cur == 0 {
                return 6;
            }
            let mut c = cur;
            let mut to = None;
            for _ in 0..20 {
                c += step;
                if !(1..=20).contains(&c) {
                    if !fresh {
                        break;
                    }
                    c = if c > 20 { 1 } else { 20 };
                }
                if c == cur {
                    break;
                }
                if a.has(c) {
                    to = Some(c);
                    break;
                }
            }
            to
        } else if repeat & UP != 0 {
            let fresh = pressed & UP != 0;
            match cur {
                0 => first(&UP_BACK),
                1..=9 => fresh.then_some(0),
                _ => first(&UP_LOW).or(fresh.then_some(0)),
            }
        } else if repeat & DOWN != 0 {
            let fresh = pressed & DOWN != 0;
            match cur {
                0 => first(&DOWN_BACK).filter(|_| fresh),
                1..=9 => first(&DOWN_TOP).or(Some(0)),
                _ => Some(0),
            }
        } else {
            return 6;
        };
        if let Some(to) = to.filter(|&t| t != cur) {
            a.ch = to;
            game.play_sys_sound(if to == 0 { 2 } else { 9 });
        }
        self.w.objs.focus = CH_OBJ[self.w.arc.ch as usize];
        6
    }

    /// sub_5D80: the panel's input; 9 to close it, 10 to play the picked scene, else 8.
    fn warc_panel(&mut self, game: &mut Game) -> i32 {
        let inp = &self.w.inp;
        let (pressed, repeat, touch, shown, pos) = (inp.pressed, inp.repeat, inp.touch, inp.shown, inp.pos);
        let a = &mut self.w.arc;
        if pressed & MASK[1] != 0 || touch & 8 != 0 {
            game.play_sys_sound(1);
            // B on a button goes back to the picked row first
            if (a.cur == 4 || a.cur == 5) && a.sel >= 0 {
                a.cur = a.sel - a.scroll;
                self.w.objs.focus = a.cur + 346;
                return 8;
            }
            return 9;
        }
        a.drag = -1.0;
        let hit = self.w.objs.hit;
        let mut moved = false;
        if (346..=351).contains(&hit) {
            if a.cur != hit - 346 && (hit > 349 || a.avail(a.ch, a.scroll + hit - 346)) {
                a.cur = hit - 346;
                game.play_sys_sound(2);
                self.w.objs.focus = hit;
                moved = true;
            }
        } else if shown >= 1 && a.n() >= 5 {
            // the scrollbar (x 467..475): where the pointer is along it
            let off = a.off();
            let (x, y) = (pos[0] as i32, pos[1] as i32 as f32);
            if (467..=475).contains(&x) && off + 285.81 <= y && y < off + 381.21 {
                a.drag = (y - (off + 285.81)) / 95.407;
            }
        }
        let a = &mut self.w.arc;
        if a.drag >= 0.0 && touch & 1 != 0 {
            let n = a.n();
            if n >= 5 {
                let last = n - 4;
                let k = (0..last).find(|&k| a.drag <= (k + 1) as f32 / (n - 3) as f32).unwrap_or(last);
                a.scroll = k.clamp(0, last.max(0));
            }
            return 8;
        }
        if pressed & MASK[0] != 0 || (!moved && touch & 2 != 0 && hit == a.cur + 346) {
            if hit >= 0 || shown < 1 || touch & 2 == 0 {
                match a.cur {
                    4 => {
                        if a.sel >= 0 && row(a.ch, a.sel).is_some_and(|r| r[0] >= 1) {
                            game.play_sys_sound(0);
                            return 10;
                        }
                    }
                    5 => {
                        game.play_sys_sound(1);
                        return 9;
                    }
                    0..4 => {
                        let k = a.scroll + a.cur;
                        if (0..=8).contains(&k) && row(a.ch, k).is_some_and(|r| r[0] >= 1) {
                            a.sel = k;
                            game.play_sys_sound(0);
                            a.cur = 4;
                            self.w.objs.focus = 350;
                        }
                    }
                    _ => {}
                }
            }
            return 8;
        }
        // the picked row when it shows, else the first
        let pick_row = |a: &WArc| if a.sel >= a.scroll + 4 || a.sel < a.scroll { 0 } else { a.sel - a.scroll };
        let n = a.n();
        let rows = (0..=3).contains(&a.cur);
        let changed = if repeat & LEFT != 0 {
            match a.cur {
                5 => {
                    a.cur = 4;
                    true
                }
                4 => {
                    a.cur = pick_row(a);
                    true
                }
                _ if rows && pressed & LEFT == 0 => false,
                _ => {
                    a.cur = 5;
                    true
                }
            }
        } else if repeat & RIGHT != 0 {
            match a.cur {
                4 => {
                    a.cur = 5;
                    true
                }
                5 if pressed & RIGHT == 0 => false,
                5 => {
                    a.cur = pick_row(a);
                    true
                }
                _ => {
                    a.cur = if rows { 4 } else { 5 };
                    true
                }
            }
        } else if repeat & UP != 0 {
            match a.cur {
                4 | 5 => false,
                1..=3 => {
                    a.cur -= 1;
                    true
                }
                0 if a.scroll >= 1 => {
                    a.scroll -= 1;
                    true
                }
                // a fresh press at the top goes round to the last row
                0 if pressed & UP == 0 => false,
                0 if n < 2 => false,
                0 if n < 5 => {
                    a.cur = n - 1;
                    true
                }
                0 => {
                    a.cur = 3;
                    a.scroll = n - 4;
                    true
                }
                _ => {
                    a.cur = 5;
                    true
                }
            }
        } else if repeat & DOWN != 0 {
            match a.cur {
                4 | 5 => false,
                0..=3 if a.scroll + a.cur >= n - 1 => {
                    if pressed & DOWN == 0 || n < 2 {
                        false
                    } else {
                        a.cur = 0;
                        a.scroll = 0;
                        true
                    }
                }
                3 => {
                    a.scroll += 1;
                    true
                }
                0..=2 => {
                    a.cur += 1;
                    true
                }
                _ => {
                    a.cur = 5;
                    true
                }
            }
        } else {
            return 8;
        };
        if changed {
            game.play_sys_sound(2);
        }
        self.w.objs.focus = self.w.arc.cur + 346;
        8
    }
}

impl Ui {
    /// jp_424D0 for the Archive: sub_3100 and the black.
    pub(in crate::ui) fn warc_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
        let a = &self.w.arc;
        if a.active && a.black < 1.0 {
            self.warc_draw_shelf(game, r, out);
            if self.w.arc.pa > 0.0 {
                self.warc_draw_panel(game, r, out);
            }
        }
        let b = self.w.arc.black;
        if b > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, b.min(1.0)]));
        }
    }

    /// sub_3100's shelf: the backdrop, the patches where a book is missing next to one, the books (those not
    /// open out of the way), the unread marks (sub_6BC0), the frame, the heading, the guide, and the cursor
    /// book's pulse: every book's shape into a surface, the cursor's white and the rest black over it (so
    /// those in front cover it), added at (sin + 1) / 2, a quarter of it on a chapter.
    fn warc_draw_shelf(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let c = self.w.arc.count;
        let tex = |id| self.w.tex.get(id);
        let mut pen = Pen::default();
        pen.scale(0.33333, 0.41481);
        if let Some(t) = tex(4201) {
            pen.spr(out, &t, 16, None, 0.0, 0.0);
        }
        for &(k, id, x, y) in &GAPS {
            if c[k] >= 1
                && c[k + 1] == 0
                && let Some(t) = tex(id)
            {
                pen.spr(out, &t, 16, None, x, y);
            }
        }
        if c[19] > 0
            && let Some(t) = tex(4244)
        {
            pen.spr(out, &t, 16, None, 538.6667, 237.6889);
        }
        for (k, &n) in c.iter().enumerate() {
            if n <= 0 {
                self.w.objs.place(CH_OBJ[k + 1], -500.0, -500.0, 0.0);
            }
        }
        let tex = |id| self.w.tex.get(id);
        self.w.objs.draw(&Pen::default(), 24, out, &tex);
        self.warc_draw_marks(game, out, &pen);
        if let Some(t) = tex(4202) {
            pen.spr(out, &t, 16, None, 0.0, 0.0);
        }
        if let Some(t) = tex(4203) {
            pen.spr(out, &t, 16, None, 253.3333, 13.2741);
        }
        self.wg_draw(out, &pen, 7, 0.0, 0.0);
        // the pulse (surface 1, cleared): the books from the last to the back label at their table places
        let a = &self.w.arc;
        let focus = CH_OBJ.get(a.ch as usize).map_or(-1, |&id| id - 325);
        let mut glow = Vec::new();
        for k in (0..=20).rev() {
            let id = 325 + k;
            let shown = self.w.objs.o.get(id as usize).and_then(|o| o.as_ref()).is_some_and(|o| o.pos[0] >= 0.0 && o.pos[1] >= 0.0);
            let def = &objtab::ARCHIVE[k as usize];
            let Some(f) = def.states[0].first() else { continue };
            if !shown {
                continue;
            }
            let Some(t) = tex(f.tex) else { continue };
            let w = if k == focus { 1.0 } else { 0.0 };
            let mut p = pen;
            p.color(1.0, w, w, w);
            let n = glow.len();
            p.spr(&mut glow, &t, f.flags, Some(f.src.map(f32::from)), def.pos[0] + f.d[0], def.pos[1] + f.d[1]);
            for q in &mut glow[n..] {
                q.shade = Shade::Fill;
                q.blend = Blend::Over;
            }
        }
        let k = if a.ch == 0 { 1.0 } else { 0.25 };
        let alpha = (a.ph[0].sin() + 1.0) * 0.5 * k;
        if !glow.is_empty() && alpha > 0.0 {
            let rt = self.w.arc.rt.get_or_insert_with(|| r.create_target(W as u32, H as u32, "archive glow").tex).clone();
            game.ui_passes.push((RenderTarget { tex: rt.clone() }, Some([0.0; 4]), glow));
            let mut q = Quad::rect(rt, [0.0, 0.0, W, H], [1.0, 1.0, 1.0, alpha]);
            q.shade = Shade::ForceAlpha;
            q.blend = Blend::Add;
            out.push(q);
        }
    }

    /// sub_6BC0: texture 4205 by the chapters with scenes not yet read.
    fn warc_draw_marks(&self, game: &Game, out: &mut Vec<Quad>, pen: &Pen) {
        let Some(t) = self.w.tex.get(4205) else { return };
        let c = self.w.arc.count;
        let rd = |k| read(game, k);
        let mut mark = |x: f32, y: f32| pen.spr(out, &t, 16, None, x, y);
        if c[1] >= 1 {
            for (k, x) in [(6, 131.3333), (7, 147.0), (8, 165.6667), (9, 191.3333)] {
                if !rd(k) {
                    mark(x, 185.837);
                }
            }
        }
        if c[8] >= 4 && !rd(65) {
            mark(590.3334, 185.837);
        }
        if c[9] > 4 && !rd(72) {
            mark(116.6667, 377.8963);
        }
        if c[10] >= 1 && !rd(73) {
            mark(138.6667, 377.8963);
        }
        if c[12] >= 1
            && (!rd(129) || !rd(130) || (!rd(131) && rd(128)) || (!rd(132) && rd(128)) || (!rd(133) && rd(206)) || (!rd(134) && rd(206)))
        {
            mark(269.3333, 377.8963);
        }
        if c[17] >= 1 && !rd(136) {
            mark(544.3334, 377.8963);
        }
        if c[18] >= 1 && !rd(206) {
            mark(560.0, 377.8963);
        }
    }

    /// sub_3100's panel at its alpha (up by 171.73 for the lower shelf): the sheet and its four row lines, the
    /// rows' objects, the picked and the cursor bars, the cursor's pulse (its bar or button shape, added in white,
    /// red on 読む with nothing picked), the titles with their English under them (sub_6A10), the read icons
    /// (sub_6F20), the frame, the scrollbar knob, the chapter's title, the picked scene's thumbnail and the buttons.
    fn warc_draw_panel(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let a = &self.w.arc;
        let (pa, off, ch, cur, scroll, sel) = (a.pa, a.off(), a.ch, a.cur, a.scroll, a.sel);
        let mut pen = Pen::default();
        pen.scale(0.33333, 0.41481);
        pen.color(pa, 1.0, 1.0, 1.0);
        let tex = |id| self.w.tex.get(id);
        if let Some(t) = tex(4245) {
            pen.spr(out, &t, 16, None, 183.3333, off + 211.56);
        }
        if let Some(t) = tex(4249) {
            for y in [276.68, 301.57, 326.46, 351.35] {
                pen.spr(out, &t, 16, None, 201.0, off + y);
            }
        }
        for (i, y) in [284.15, 309.04, 333.93, 358.81].into_iter().enumerate() {
            let k = scroll + i as i32;
            if self.w.arc.avail(ch, k) {
                self.w.objs.place(346 + i as i32, 206.0, off + y, pa);
            } else {
                self.w.objs.place(346 + i as i32, -500.0, -500.0, 0.0);
            }
        }
        let tex = |id| self.w.tex.get(id);
        let bar_y = |i: i32| off + i as f32 * 24.889 + 284.15;
        if let Some(t) = tex(4250) {
            if (0..4).contains(&(sel - scroll)) {
                pen.spr(out, &t, 16, Some(BAR_PICK), 206.0, bar_y(sel - scroll));
            }
            if (0..=3).contains(&cur) {
                pen.spr(out, &t, 16, Some(BAR_CUR), 206.0, bar_y(cur));
            }
        }
        // the cursor's pulse: its shape (surface 1) filled with the colour (surface 2) and added
        let shape = match cur {
            0..=3 => tex(4250).map(|t| (t, 16, BAR_CUR, 206.0, bar_y(cur), 0.5)),
            4 | 5 => self.w.objs.o.get((346 + cur) as usize).and_then(|o| o.as_ref()).and_then(|o| {
                let f = objtab::ARCHIVE.iter().find(|d| i32::from(d.id) == 346 + cur)?.states[0].first()?;
                Some((tex(f.tex)?, f.flags, f.src.map(f32::from), o.pos[0], o.pos[1], 1.0))
            }),
            _ => None,
        };
        if let Some((t, flags, src, x, y, k)) = shape {
            let red = cur == 4 && sel < 0;
            let g = if red { 0.0 } else { 1.0 };
            let mut p = pen;
            p.color(pa * ((self.w.arc.ph[1].sin() + 1.0) * 0.5) * k, 1.0, g, g);
            let n = out.len();
            p.spr(out, &t, flags, Some(src), x, y);
            for q in &mut out[n..] {
                q.shade = Shade::Fill;
                q.blend = Blend::Add;
            }
        }
        for i in 0..4 {
            let k = scroll + i;
            let id = if self.w.arc.avail(ch, k) { row(ch, k).map_or(0, |r| r[2]) } else { 0 };
            self.warc_title(game, r, out, i, id.into(), pa, off);
        }
        let tex = |id| self.w.tex.get(id);
        for (i, y) in [286.22, 311.11, 336.0, 360.89].into_iter().enumerate() {
            let k = scroll + i as i32;
            if k > 8 {
                break;
            }
            let Some(rw) = row(ch, k).filter(|_| self.w.arc.avail(ch, k)) else { continue };
            if read(game, rw[4].into()) {
                let x0 = 8.0 + 56.0 * f32::from(rw[3]);
                if let Some(t) = tex(4248) {
                    pen.spr(out, &t, 16, Some([x0, 16.0, x0 + 45.0, 61.0]), 209.67, off + y);
                }
            } else if let Some(t) = tex(4205) {
                pen.spr(out, &t, 16, None, 209.67, off + y);
            }
        }
        if let Some(t) = tex(4246) {
            pen.spr(out, &t, 16, None, 200.6667, off + 277.51);
        }
        let a = &self.w.arc;
        let n = a.n();
        let (h, y, knob) = if n <= 4 {
            (95.407, 285.81, 3)
        } else {
            let rows = a.rows[(ch - 1) as usize];
            let h = (4.0 / rows as f32).min(1.0) * 95.407;
            (h, (95.407 - h) * scroll as f32 / (n - 4) as f32 + 285.81, usize::from(a.drag >= 0.0))
        };
        if let Some(t) = tex(4247) {
            let mut p = pen;
            p.scale(0.33333, h);
            p.spr(out, &t, 16, Some(KNOB[knob]), 467.0, off + y);
        }
        if let Some(t) = TITLE_TEX.get((ch - 1) as usize).and_then(|&id| tex(id)) {
            pen.spr(out, &t, 16, None, 196.0, off + 218.61);
        }
        if let Some(rw) = row(ch, sel)
            && let Some(t) = tex(4281 + i32::from(rw[1]))
        {
            // sub_18410(-10) for the thumbnails 128..133: the language's strip
            let src = (128..134).contains(&rw[1]).then(|| {
                let (lang, n) = (game.assets.language(), game.assets.languages());
                let h = t.height as f32 / n.max(1) as f32;
                [0.0, h * lang as f32, t.width as f32, h * lang as f32 + h]
            });
            pen.spr(out, &t, 16, src, 483.0, off + 265.07);
        }
        self.w.objs.place(350, 483.67, off + 361.72, pa);
        self.w.objs.place(351, 548.67, off + 361.72, pa);
        let tex = |id| self.w.tex.get(id);
        self.w.objs.draw(&Pen::default(), 26, out, &tex);
    }

    /// sub_6A10(i, text): row i's title (text slot 50 + i, 0.5 x 0.5, character space 0 + 4, black, the system font) and, but in
    /// English, the English title under it (54 + i, 0.35 x 0.3); in English the title sits 4.1481 lower.
    #[allow(clippy::too_many_arguments)]
    fn warc_title(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, i: i32, id: i32, pa: f32, off: f32) {
        if id < 1 {
            return;
        }
        let en = crate::text::english();
        let a = &self.w.arc;
        let title = a.titles.get(id as usize).cloned().unwrap_or_default();
        let sub = if en { None } else { a.titles_en.get(id as usize).cloned() };
        let y = off + if en { 4.1481 } else { 0.0 } + i as f32 * 24.889;
        let black = [0.0, 0.0, 0.0, pa];
        self.wtext(game, r, out, &title, 0x987040, [0.5, 0.5], 4.0, 226.33, y + 282.9, 0, black);
        if let Some(s) = sub {
            self.wtext(game, r, out, &s, 0x987040, [0.35, 0.3], 4.0, 226.33, y + 297.01, 0, black);
        }
    }
}
