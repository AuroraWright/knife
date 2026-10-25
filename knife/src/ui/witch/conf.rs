//! The config: sub_D2C0 installs it (at boot from jp_47BF0: RES_CONF and the objects of the table at
//! 0x2EB1F0), loc_D350 opens it (the config block's backup for the change check), sub_D700 runs it (state /
//! page in qword_638AE8) and sub_BD90 draws it (layer 21). Five pages (音響 / 音声 / 文章 / 動作 設定 and
//! 操作説明) of rows from the page table dword_2ED170: sliders (kind 2: knob, bar, down, up), radio pairs
//! (kind 1), buttons (kind 0: a page's reset, the language arrows) and the voice grid (kind 5: a voice's
//! toggle and volume knob, three a row). Each change goes into the config block at once and is applied;
//! closing writes the system data when the block differs from the backup.

use knife_render::{Quad, Renderer};

use super::super::{Ui, W};
use super::{DOWN, LEFT, MASK, RIGHT, UP, objtab};
use crate::game::Game;
use crate::savedata::{get_i16, get_i32, put_i16, put_i32, wcfg};

const SX: f32 = 0.33333;
const SY: f32 = 0.41481;

/// A page of dword_2ED170 (255 ints): its object range, rows, each row's column count, kind and objects, the
/// objects' positions (floats), and the rows' radio choice / slider values / slider steps (the table's own
/// initial values; sub_BB00 fills them from the config).
struct Page {
    objs: (i32, i32),
    rows: usize,
    cols: [i32; 9],
    kind: [i32; 9],
    ids: [[i32; 7]; 9],
    pos: [[[f32; 2]; 7]; 9],
    sel: [i32; 9],
    val: [[i32; 3]; 9],
    step: [i32; 9],
}

#[rustfmt::skip]
const PAGES: [Page; 5] = [
    Page {
        objs: (73, 97),
        rows: 7,
        cols: [4, 4, 4, 4, 2, 2, 1, 0, 0],
        kind: [2, 2, 2, 2, 1, 1, 0, 0, 0],
        ids: [
            [73, 74, 75, 76, 0, 0, 0],
            [77, 78, 79, 80, 0, 0, 0],
            [81, 82, 83, 84, 0, 0, 0],
            [85, 86, 87, 88, 0, 0, 0],
            [93, 94, 0, 0, 0, 0, 0],
            [95, 96, 0, 0, 0, 0, 0],
            [97, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ],
        pos: [
            [[250.66667, 84.62222], [250.66667, 84.62222], [-166.66667, 84.62222], [-166.66667, 84.62222], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[250.66667, 121.12593], [250.66667, 121.12593], [-166.66667, 121.12593], [-166.66667, 121.12593], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[250.66667, 157.62964], [250.66667, 157.62964], [-166.66667, 157.62964], [-166.66667, 157.62964], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[250.66667, 194.13333], [250.66667, 194.13333], [-166.66667, 194.13333], [-166.66667, 194.13333], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[318.6667, 219.02222], [404.0, 219.02222], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[318.6667, 255.52594], [404.0, 255.52594], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[232.33334, 385.36295], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
        ],
        sel: [0, 0, 0, 0, 0, 1, 0, 0, 0],
        val: [[80, 0, 0], [255, 0, 0], [80, 0, 0], [80, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0]],
        step: [16, 16, 16, 16, 0, 0, 0, 0, 0],
    },
    Page {
        objs: (130, 160),
        rows: 6,
        cols: [6, 6, 6, 6, 6, 1, 0, 0, 0],
        kind: [5, 5, 5, 5, 5, 0, 0, 0, 0],
        ids: [
            [130, 145, 131, 146, 132, 147, 0],
            [133, 148, 134, 149, 135, 150, 0],
            [136, 151, 137, 152, 138, 153, 0],
            [139, 154, 140, 155, 141, 156, 0],
            [142, 157, 143, 158, 144, 159, 0],
            [160, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ],
        pos: [
            [[38.666668, 76.32592], [122.33334, 104.533325], [229.33334, 76.32592], [313.0, 104.533325], [4.2e+02, 76.32592], [503.6667, 104.533325], [0.0, 0.0]],
            [[38.666668, 129.42221], [122.33334, 157.62962], [229.33334, 129.42221], [313.0, 157.62962], [4.2e+02, 129.42221], [503.6667, 157.62962], [0.0, 0.0]],
            [[38.666668, 182.51851], [122.33334, 210.72592], [229.33334, 182.51851], [313.0, 210.72592], [4.2e+02, 182.51851], [503.6667, 210.72592], [0.0, 0.0]],
            [[38.666668, 235.6148], [122.33334, 263.8222], [229.33334, 235.6148], [313.0, 263.8222], [4.2e+02, 235.6148], [503.6667, 263.8222], [0.0, 0.0]],
            [[38.666668, 288.7111], [122.33334, 316.9185], [229.33334, 288.7111], [313.0, 316.9185], [4.2e+02, 288.7111], [503.6667, 316.9185], [0.0, 0.0]],
            [[232.33334, 385.36295], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
        ],
        sel: [0, 0, 0, 0, 0, 0, 0, 0, 0],
        val: [
            [100, 0, 0],
            [100, 0, 0],
            [100, 0, 0],
            [100, 0, 0],
            [100, 0, 0],
            [100, 0, 0],
            [0, 0, 0],
            [0, 0, 0],
            [0, 0, 0],
        ],
        step: [10, 10, 10, 10, 10, 10, 10, 0, 0],
    },
    Page {
        objs: (100, 118),
        rows: 7,
        cols: [4, 4, 4, 2, 2, 2, 1, 0, 0],
        kind: [2, 2, 2, 1, 0, 1, 0, 0, 0],
        ids: [
            [100, 101, 102, 103, 0, 0, 0],
            [104, 105, 106, 107, 0, 0, 0],
            [108, 109, 110, 111, 0, 0, 0],
            [112, 113, 0, 0, 0, 0, 0],
            [114, 115, 0, 0, 0, 0, 0],
            [116, 117, 0, 0, 0, 0, 0],
            [118, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ],
        pos: [
            [[250.66667, 84.62222], [250.66667, 84.62222], [-166.66667, 84.62222], [-166.66667, 84.62222], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[250.66667, 121.12593], [250.66667, 121.12593], [-166.66667, 121.12593], [-166.66667, 121.12593], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[250.66667, 157.62964], [250.66667, 157.62964], [-166.66667, 157.62964], [-166.66667, 157.62964], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[318.6667, 182.51852], [404.0, 182.51852], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[285.33334, 222.34073], [490.6667, 222.34073], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[318.6667, 255.52594], [404.0, 255.52594], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[232.33334, 385.36295], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
        ],
        sel: [0, 0, 0, 1, 0, 0, 0, 0, 0],
        val: [[127, 0, 0], [127, 0, 0], [127, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0]],
        step: [8, 8, 8, 0, 0, 0, 0, 0, 0],
    },
    Page {
        objs: (119, 127),
        rows: 4,
        cols: [2, 2, 2, 1, 0, 0, 0, 0, 0],
        kind: [1, 1, 1, 0, 0, 0, 0, 0, 0],
        ids: [
            [119, 120, 0, 0, 0, 0, 0],
            [121, 122, 0, 0, 0, 0, 0],
            [125, 126, 0, 0, 0, 0, 0],
            [127, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ],
        pos: [
            [[318.6667, 73.00741], [404.0, 73.00741], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[318.6667, 109.51111], [404.0, 109.51111], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[318.6667, 146.01482], [404.0, 146.01482], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[232.33334, 385.36295], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
        ],
        sel: [1, 0, 1, 0, 0, 0, 0, 0, 0],
        val: [[0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0]],
        step: [0, 0, 0, 0, 0, 0, 0, 0, 0],
    },
    Page {
        objs: (161, 161),
        rows: 0,
        cols: [0, 0, 0, 0, 0, 0, 0, 0, 0],
        kind: [0, 0, 0, 0, 0, 0, 0, 0, 0],
        ids: [
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ],
        pos: [
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
        ],
        sel: [1, 0, 0, 0, 0, 0, 0, 0, 0],
        val: [[0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0]],
        step: [0, 0, 0, 0, 0, 0, 0, 0, 0],
    },];

/// dword_25DEE8: the voice (config bit / volume word) of each voice page column, 7 a row
const VOICE_OF: [[usize; 7]; 5] =
    [[0, 0, 1, 1, 2, 2, 0], [3, 3, 4, 4, 5, 5, 0], [6, 6, 7, 7, 8, 8, 0], [9, 9, 10, 10, 11, 11, 0], [12, 12, 13, 13, 14, 14, 0]];
/// aJnrvz: the slider bar that lights (flag 2) for a page's focused row
const ROW_BAR: [[i32; 8]; 5] = [
    [74, 78, 82, 86, 90, -1, -1, -1],
    [-1; 8],
    [101, 105, 109, -1, -1, -1, -1, -1],
    [-1; 8],
    [-1; 8],
];
/// sub_BD90: the page tabs' x (objects 50..54, y 31.525925)
const TAB_X: [f32; 5] = [49.0, 157.67, 266.33, 375.0, 483.67];
/// dword_25E00C: each page's row captions (CONF_PARTS), 88 px apart
const LABELS: [i32; 5] = [501, -1, 512, 514, -1];
/// unk_25E048: a slider row's down / up captions (cells of the 0x2F0D08 table: odd 小 side, even 大 side)
const ENDS: [[(usize, usize); 5]; 5] = [
    [(3, 4), (3, 4), (3, 4), (3, 4), (3, 4)],
    [(0, 0); 5],
    [(1, 2), (1, 2), (5, 6), (0, 0), (0, 0)],
    [(0, 0); 5],
    [(0, 0); 5],
];

/// The config's state (qword_638AE8 and the other words sub_D2C0 / loc_D350 set).
#[derive(Default)]
pub struct WConf {
    installed: bool,
    /// layer 21 draws (jp_B8070(21, 0, 256) at states 0 / 1, off when state 8 ends)
    shown: bool,
    /// LODWORD / HIDWORD(qword_638AE8)
    st: i32,
    page: usize,
    /// dword_638AF0: the tab the pad is on, and the page a slide goes to
    tab: i32,
    /// LODWORD(qword_638AF4): the page slide's direction; HIDWORD: the page's alpha / slide (0..1)
    dir: f32,
    p: f32,
    /// dword_638AFC: the screen's alpha (0.024 a frame)
    a: f32,
    /// LODWORD(qword_638B00): a horizontal drag's pull on the page (x 0.025 units)
    band: i32,
    /// dword_638B10: frames the page stays still after a slider drag
    still: i32,
    /// LODWORD(qword_638B08): the knob the pointer drags; HIDWORD: the language set last frame
    knob: i32,
    lang: i32,
    /// dword_638B20 / dword_638B24: the pointer x and the value where the knob drag started
    knob_x: i32,
    knob_v: i32,
    /// dword_638B18: the value under the finger on a slider bar
    under: i32,
    /// dword_638B1C: the voice slider A adjusts (byte_638AE4 while A is held on it)
    vsel: i32,
    adjust: bool,
    /// dword_638B28: the row whose bar lights
    lit: i32,
    /// dword_2F07A0[0]: the slider the finger drags (a row, or a voice 0..14 on the voice page)
    slide: i32,
    /// dword_2F07A0[14 + page]: each page's focused object (page 1's is dword_2F07DC, the dragged knob)
    focus: [i32; 5],
    /// sub_D700's v1: the column of the focused object in its row
    col: i32,
    /// the pages' radio choices and slider values (dword_2ED170 + 210 / + 219)
    sel: [[i32; 9]; 5],
    val: [[[i32; 3]; 9]; 5],
    /// unk_638B2C: the config block as the screen opened
    backup: Vec<u8>,
}

impl Ui {
    /// sub_D2C0 (jp_47BF0 at boot): RES_CONF, the objects (jp_AEEC0(group, 1) draws them at the pen's
    /// 1/3 x 0.41481), page 0, state 1.
    fn wc_install(&mut self, game: &Game, r: &Renderer) {
        if self.w.conf.installed {
            return;
        }
        self.w.tex.load(game, r, objtab::RES_CONF);
        // the backdrop (51) and the guide's help_key / help_txt come with the game screen's parts (jp_3B9A0)
        if self.w.tex.get(76).is_none() {
            self.w.tex.load(game, r, objtab::RES_DISP);
        }
        self.w.objs.install(objtab::CONF);
        for d in objtab::CONF {
            self.w.objs.scale(i32::from(d.id), SX, SY);
        }
        let c = &mut self.w.conf;
        c.installed = true;
        c.st = 1;
        c.page = 0;
        c.tab = 0;
        c.dir = 1.0;
        c.p = 0.0;
        c.a = 0.0;
        c.slide = -1;
        // dword_2F07A0[14..19] as the exe starts
        c.focus = [0, 0, 0, 0, 54];
        for (k, pg) in PAGES.iter().enumerate() {
            c.sel[k] = pg.sel;
            c.val[k] = pg.val;
        }
    }

    /// loc_D350 (sub_2D940 state 7 / jp_8E200 state 26).
    pub(in crate::ui) fn wc_open(&mut self, game: &mut Game, r: &Renderer) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        self.wc_install(game, r);
        let backup = self.file.config().to_vec();
        let c = &mut self.w.conf;
        c.backup = backup;
        c.band = 0;
        if c.st >= 2 {
            c.st = 1;
        }
        c.knob = -1;
        c.lang = -1;
    }

    /// sub_BB00(page): a page's values from the config block, then its objects' flags: a radio's choice on and
    /// the rest off, sliders and buttons off (a slider's bar and ends 0x80).
    fn wc_load(&mut self, page: usize) {
        let cf = self.file.config();
        let w = |o: usize| i32::from(get_i16(cf, o));
        let c = &mut self.w.conf;
        let objs = &mut self.w.objs;
        match page {
            0 => {
                c.val[0][0][0] = w(wcfg::BGM);
                c.val[0][1][0] = w(wcfg::VOICE);
                c.val[0][2][0] = w(wcfg::SE);
                c.val[0][3][0] = w(wcfg::SYS_SE);
                c.sel[0][4] = w(wcfg::VOICE_STOP);
                c.sel[0][5] = w(wcfg::BGM_AUTO);
            }
            1 => {
                let bits = get_i32(cf, wcfg::VOICE_BITS);
                for i in 0..15 {
                    // flag 2 (the greyed frames) on a voice that is off
                    let on = bits >> i & 1 != 0;
                    objs.flags(130 + i as i32, if on { 0 } else { 2 }, if on { 2 } else { 0 });
                    c.val[1][i / 3][i % 3] = w(wcfg::CHAR_VOICE + 2 * i);
                }
            }
            2 => {
                c.val[2][0][0] = w(wcfg::TEXT_SPEED);
                c.val[2][1][0] = w(wcfg::AUTO_WAIT);
                c.val[2][2][0] = 255 - w(wcfg::WINDOW_ALPHA);
                c.sel[2][3] = w(wcfg::TEXT_EDGE);
                c.sel[2][5] = w(wcfg::TEXT_COLOUR);
            }
            3 => {
                c.sel[3][0] = w(wcfg::SKIP_READ_ONLY);
                c.sel[3][1] = w(wcfg::CHOICE_SKIP_STOP);
                c.sel[3][2] = w(wcfg::HELP);
            }
            _ => {}
        }
        let pg = &PAGES[page];
        for row in 0..pg.rows {
            let k = pg.kind[row];
            for col in 0..pg.cols[row] {
                let id = pg.ids[row][col as usize];
                if k == 1 && col == c.sel[page][row] {
                    objs.flags(id, 2, 0);
                    continue;
                }
                if k != 1 && (3..6).contains(&k) {
                    continue;
                }
                objs.flags(id, 0, 2);
                if col != 0 && k == 2 {
                    objs.flags(id, 0x80, 0);
                }
            }
        }
    }

    /// sub_B7E0: a voice whose character the story has met (G011..G024) shows its face and name in the help;
    /// the last (others) always.
    fn wc_voices(&mut self, game: &Game) {
        for k in 0..15 {
            if k < 14 && game.var(&format!("G{:03}", 11 + k)) == 0 {
                continue;
            }
            let id = 130 + k as i32;
            self.w.objs.set_states(id, &objtab::CONF_VOICES[k]);
            let help = if k < 14 { 43 + 4 * k as i32 } else { 39 };
            if let Some(o) = self.w.objs.get(id) {
                o.help = help;
            }
            if let Some(o) = self.w.objs.get(id + 15) {
                o.help = help + 1;
            }
        }
    }

    /// The row and column of `obj` on the page (sub_D700's scan; a button row remembers the column).
    fn wc_find(&mut self, page: usize, obj: i32) -> Option<(i32, i32)> {
        let pg = &PAGES[page];
        let mut found = None;
        for row in 0..pg.rows {
            for col in 0..pg.cols[row].max(0) {
                if pg.ids[row][col as usize] == obj {
                    if pg.kind[row] == 0 {
                        self.w.conf.sel[page][row] = col;
                    }
                    found = Some((row as i32, col));
                }
            }
        }
        found
    }

    /// A row's kind (dword_2ED170 + 12 + row; past the nine kinds the table's object ids, never a slider).
    fn wc_kind(page: usize, row: i32) -> i32 {
        usize::try_from(row).ok().and_then(|r| PAGES[page].kind.get(r).copied()).unwrap_or(-1)
    }

    /// sub_D700, one 60 Hz frame. Returns 1 once the screen is gone.
    pub(in crate::ui) fn wc_step(&mut self, game: &mut Game, r: &Renderer) -> i32 {
        // a language set last frame: the help of the arrow (dword_2EDCC0: the language row's choice) again
        if self.w.conf.lang >= 0 {
            let arrow = if self.w.conf.sel[2][4] != 0 { 115 } else { 114 };
            self.w.objs.help(arrow, true);
            self.w.conf.lang = -1;
        }
        let inp = &self.w.inp;
        let c = &mut self.w.conf;
        if inp.finger_down() {
            // qword_638B00: the drag's stronger axis less 4 units
            let (dx, dy) = ((inp.pos[0] - inp.start[0]) as i32, (inp.pos[1] - inp.start[1]) as i32);
            c.band = if dx.abs() <= dy.abs() {
                0
            } else if dx > -5 {
                (dx - 4).max(0)
            } else {
                dx + 4
            };
        } else if c.band >= 1 {
            c.band = (c.band - 60).max(0);
        } else if c.band < 0 {
            c.band = (c.band + 60).min(0);
        }
        if c.still >= 1 {
            c.still -= 1;
        }
        let mut done = 0;
        match c.st {
            0 | 1 => {
                c.adjust = false;
                for k in 0..5 {
                    self.wc_load(k);
                }
                self.wc_voices(game);
                self.w.conf.shown = true;
                self.w.guide.on = true;
                self.w.conf.st = 2;
                self.wc_fade_in();
            }
            2 => self.wc_fade_in(),
            3 => done = self.wc_input(game, r),
            4 => {
                let c = &mut self.w.conf;
                c.p -= 0.05;
                if c.p <= 0.0 {
                    c.p = 0.0;
                    c.st = 5;
                    c.page = c.tab as usize;
                    for g in 4..9 {
                        self.w.objs.group_flags(g, 0, 1);
                    }
                    let page = self.w.conf.page;
                    self.w.objs.group_flags(page as u8 + 4, 1, 0);
                    let f = self.w.conf.focus[page];
                    self.w.objs.focus = f;
                    let row = if (50..56).contains(&f) {
                        self.w.conf.tab = f - 50;
                        -1
                    } else if let Some((row, _)) = self.wc_find(page, f) {
                        row
                    } else {
                        let first = PAGES[page].ids[0][0];
                        self.w.conf.focus[page] = first;
                        self.w.objs.focus = first;
                        0
                    };
                    self.w.conf.lit = if page == 4 { -1 } else { row };
                    self.w.objs.lock = 0;
                }
            }
            5 => {
                c.p += 0.05;
                if c.p >= 1.0 {
                    c.p = 1.0;
                    c.st = 3;
                }
            }
            8 => {
                let f = 0.024;
                if c.a > 0.0 {
                    c.a = (c.a - f).max(0.0);
                }
                c.p -= f;
                if c.p <= 0.0 {
                    c.p = 0.0;
                    c.st = 2;
                    c.shown = false;
                    // the closing fade ending resets the per-message colour words
                    game.reset_colour_words();
                    self.w.objs.group_flags(3, 0, 3);
                    for g in 4..9 {
                        self.w.objs.group_flags(g, 0, 1);
                    }
                    done = 1;
                }
            }
            // jp_87C10: the system data write ends
            11 => c.st = 8,
            _ => {}
        }
        // LABEL_125: the focused slider row's bar lights
        for id in [74, 78, 82, 86, 90, 101, 105, 109] {
            self.w.objs.flags(id, 0, 2);
        }
        let c = &self.w.conf;
        if (0..5).contains(&c.lit) && ROW_BAR[c.page][c.lit as usize] >= 0 {
            self.w.objs.flags(ROW_BAR[c.page][c.lit as usize], 2, 0);
        }
        done
    }

    /// States 1 / 2: the screen and the page fade in at 0.024 a frame; then the tabs and the page take input.
    fn wc_fade_in(&mut self) {
        let c = &mut self.w.conf;
        let f = 0.024;
        if c.a < 1.0 {
            c.a = (c.a + f).min(1.0);
        }
        c.p += f;
        if c.p >= 1.0 {
            c.p = 1.0;
            c.st = 3;
            let page = c.page;
            self.w.objs.group_flags(3, 1, 0);
            self.w.objs.flags(page as i32 + 50, 2, 0);
            for g in 4..9 {
                self.w.objs.group_flags(g, 0, 1);
            }
            self.w.objs.group_flags(page as u8 + 4, 1, 0);
        }
    }

    /// sub_D700 state 3.
    fn wc_input(&mut self, game: &mut Game, r: &Renderer) -> i32 {
        let p = self.w.inp.pressed;
        let rep = self.w.inp.repeat;
        let page = self.w.conf.page;
        let pg = &PAGES[page];
        // B, the back button or a two-finger tap: close; a changed config is written first (state 11)
        if p & MASK[1] != 0 || self.w.guide.tapped || self.w.inp.touch & 8 != 0 {
            self.w.objs.focus = -1;
            let changed = self.file.config()[..320] != self.w.conf.backup[..320.min(self.w.conf.backup.len())];
            self.w.conf.st = 8;
            if changed {
                self.store_global(game);
                self.w.conf.st = 11;
            }
            game.play_sys_sound(1);
            self.w.guide.on = false;
            return 0;
        }
        if self.w.inp.held & MASK[0] == 0 {
            self.w.conf.adjust = false;
        }
        // the finger on a slider: page 1's fifteen voice bars (95 units, 0..100), the others' rows 0..4 from
        // y 81 (39 units a row, 291 units from x 254 for 0..255)
        let (sx, sy) = (self.w.inp.start[0] as i32, self.w.inp.start[1] as i32);
        let (fx, px) = (self.w.inp.pos[0] as i32, self.w.inp.pos[0] as i32);
        let mut v3 = -1;
        if !self.w.inp.finger_down() {
            self.w.conf.slide = -1;
        } else if page == 1 {
            let c = &mut self.w.conf;
            if c.slide < 0 {
                let hit = (0..15).find(|&i| {
                    let (row, k) = (i / 3, i % 3);
                    let (x0, x1) = (190 * k + 125, 190 * k + 220);
                    sy < 50 * row + 131 && sy > 50 * row + 100 && sx > x0 && sx < x1
                });
                if let Some(i) = hit {
                    let (row, k) = (i as usize / 3, i as usize % 3);
                    c.under = ((sx - (190 * k as i32 + 125)) as f32 * 100.0 / 95.0) as i32;
                    c.adjust = true;
                    c.slide = i;
                    v3 = PAGES[1].ids[row][2 * k + 1];
                    c.focus[1] = v3;
                    c.vsel = i;
                    c.knob_x = px;
                    c.knob_v = c.val[1][row][k];
                    game.play_sys_sound(2);
                }
            }
            if c.slide >= 0 {
                c.under = (((fx - 190 * (c.slide % 3) - 125) as f32 * 100.0 / 95.0) as i32).clamp(0, 100);
            }
        } else {
            let c = &mut self.w.conf;
            if page != 3 && c.slide < 0 && (sx - 121) as u32 <= 0x1E8 && (sy - 81) as u32 <= 0xBE {
                let row = ((sy - 80) / 39).clamp(0, 4);
                c.slide = if Self::wc_kind(page, row) == 2 { row } else { -1 };
            }
            if c.slide >= 0 {
                c.under = ((fx - 254) as f32 * 255.0 / 291.0) as i32;
            }
        }
        // LABEL_158
        if self.w.inp.touch & 1 == 0 {
            self.w.conf.knob = -1;
        }
        let hit = if self.w.conf.slide >= 0 {
            if self.w.objs.lock == 0 {
                if page == 1 {
                    self.w.objs.focus = self.w.conf.focus[1];
                } else if page != 3 {
                    // the drag starts on a slider's bar: its knob takes the focus (jp_AE840 runs again)
                    self.w.objs.update(&mut self.w.inp);
                    let h = self.w.objs.hit;
                    if h < 1 {
                        self.w.conf.slide = -1;
                    } else {
                        v3 = h;
                        if let Some(row) = (0..pg.rows).find(|&row| pg.kind[row] == 2 && pg.ids[row][1] == h) {
                            v3 = pg.ids[row][0];
                            self.w.conf.slide = row as i32;
                        }
                        self.w.conf.focus[page] = v3;
                        match Self::wc_kind(page, self.w.conf.slide) {
                            1 => game.play_sys_sound(2),
                            2 => game.play_sys_sound(0),
                            _ => {}
                        }
                    }
                }
            }
            if self.w.conf.slide >= 0 {
                self.w.objs.lock = 1;
            }
            v3
        } else {
            self.w.objs.lock = 0;
            self.w.objs.hit
        };
        // LABEL_167: the pointer's object on the page takes its focus
        let c = &mut self.w.conf;
        if !self.w.inp.active {
            self.w.objs.focus = c.focus[page];
        } else if c.knob < 0 {
            if (50..=55).contains(&hit) || (hit >= pg.objs.0 && hit <= pg.objs.1) {
                if c.focus[page] != hit {
                    c.focus[page] = hit;
                    game.play_sys_sound(2);
                }
            } else {
                self.w.objs.focus = c.focus[page];
            }
        } else {
            c.focus[page] = c.knob;
        }
        let f = self.w.conf.focus[page];
        let mut row = if (50..56).contains(&f) {
            self.w.conf.tab = f - 50;
            -1
        } else if let Some((row, col)) = self.wc_find(page, f) {
            self.w.conf.col = col;
            row
        } else {
            let first = pg.ids[0][0];
            self.w.conf.focus[page] = first;
            self.w.objs.focus = first;
            self.w.conf.col = 0;
            0
        };
        if page == 4 {
            row = -1;
        }
        self.w.conf.lit = row;
        // the finger's slider takes the value under it
        let mut dragged = false;
        let c = &mut self.w.conf;
        if c.slide >= 0 {
            if Self::wc_kind(page, c.slide) == 2 {
                c.lit = c.slide;
                row = c.slide;
                c.still = 4;
                c.val[page][row as usize][0] = c.under.clamp(0, 255);
                dragged = true;
            } else {
                let r3 = c.slide / 3;
                if Self::wc_kind(page, r3) == 5 {
                    c.lit = r3;
                    let col = c.col.max(0) as usize;
                    c.val[page][r3 as usize][col >> 1] = c.under.clamp(0, 100);
                    c.knob = VOICE_OF[r3 as usize][col] as i32 + 145;
                    dragged = true;
                    c.still = 4;
                    row = r3;
                }
            }
        }
        // LABEL_222: the knob follows the pointer from where the drag started
        if c.knob >= 0 {
            let d = (px - c.knob_x) as f32;
            if row >= 0 {
                let rw = row as usize;
                if page == 1 {
                    let v = ((d * 100.0 / 82.667 + c.knob_v as f32) as i32).clamp(0, 100);
                    c.val[1][rw][(c.col.max(0) >> 1) as usize] = v;
                } else {
                    c.val[page][rw][0] = ((d * 255.0 / 278.0 + c.knob_v as f32) as i32).clamp(0, 255);
                }
                self.wc_apply(game, r, row, false);
            }
            return 0;
        }
        let tapped = self.w.inp.touch & 2 != 0 && hit == self.w.conf.focus[page];
        if p & MASK[0] == 0 && !tapped {
            let flick = self.w.inp.flick;
            let still = self.w.conf.still == 0;
            if p & MASK[9] != 0 || (flick & 1 != 0 && still) {
                if !self.w.conf.adjust {
                    self.wc_slide(game, (page as i32 + 1) % 5, -1.0);
                }
            } else if p & MASK[8] != 0 || (flick & 2 != 0 && still) {
                if !self.w.conf.adjust {
                    self.wc_slide(game, (page as i32 + 4) % 5, 1.0);
                }
            } else if rep & UP != 0 {
                if page != 4 && !self.w.conf.adjust {
                    let from = if row >= 0 { self.wc_mark(page, row) } else { self.w.conf.still };
                    let to = if row >= 0 { row - 1 } else if p & UP != 0 { pg.rows as i32 - 1 } else { -1 };
                    self.wc_row(game, page, row, to, from);
                }
            } else if rep & DOWN != 0 {
                if page != 4 && !self.w.conf.adjust {
                    let from = if row >= 0 { self.wc_mark(page, row) } else { self.w.conf.still };
                    let to = row + i32::from(row < pg.rows as i32 - 1 || p & DOWN != 0);
                    let to = if to < pg.rows as i32 { to } else { -1 };
                    self.wc_row(game, page, row, to, from);
                }
            } else if rep & RIGHT != 0 {
                if self.wc_side(game, page, row, true) {
                    self.wc_apply(game, r, row, true);
                    return 0;
                }
            } else if rep & LEFT != 0 && self.wc_side(game, page, row, false) {
                self.wc_apply(game, r, row, true);
                return 0;
            }
            if dragged {
                self.wc_apply(game, r, row, false);
            }
            return 0;
        }
        // A or a tap on the focused object
        if row < 0 {
            let c = &self.w.conf;
            if c.tab >= 0 && c.tab != page as i32 {
                let tab = c.tab;
                self.w.objs.flags(page as i32 + 50, 0, 2);
                self.w.objs.flags(tab + 50, 2, 0);
                self.w.objs.press(tab + 50, 10);
                let c = &mut self.w.conf;
                c.st = 4;
                c.dir = if (page as i32) < tab { -1.0 } else { 1.0 };
                c.focus[tab as usize] = tab + 50;
                c.focus[page] = tab + 50;
                game.play_sys_sound(0);
            }
            return 0;
        }
        let rw = row as usize;
        let col = self.w.conf.col;
        let mut press = true;
        match pg.kind[rw] {
            1 => {
                for k in 0..pg.cols[rw] {
                    self.w.objs.flags(pg.ids[rw][k as usize], 0, 2);
                }
                self.w.objs.flags(pg.ids[rw][col as usize], 2, 0);
                self.w.conf.sel[page][rw] = col;
                game.play_sys_sound(0);
            }
            2 => {
                if p & MASK[0] == 0 {
                    let c = &mut self.w.conf;
                    let step = pg.step[rw];
                    let v = &mut c.val[page][rw][0];
                    match col {
                        0 => {
                            c.knob = pg.ids[rw][0];
                            c.knob_x = px;
                            c.knob_v = *v;
                        }
                        1 => *v = (px - pg.pos[rw][1][0] as i32).clamp(16, 271) - 16,
                        2 if *v >= 1 => *v = (*v - step).max(0),
                        3 if *v <= 254 => *v = (*v + step).min(255),
                        _ => {}
                    }
                } else {
                    press = false;
                }
            }
            5 if col & 1 != 0 => {
                let c = &mut self.w.conf;
                if p & MASK[0] != 0 || self.w.inp.touch & 2 != 0 {
                    c.adjust = true;
                    c.vsel = 3 * row + col / 2;
                }
                c.knob = VOICE_OF[rw][col as usize] as i32 + 145;
                c.knob_x = px;
                c.knob_v = c.val[1][rw][(col >> 1) as usize];
            }
            5 => {
                // LABEL_299: the voice's bit (flag 2 greys it while off)
                let bit = VOICE_OF[rw][col as usize];
                let cf = self.file.config_mut();
                let bits = get_i32(cf, wcfg::VOICE_BITS) ^ 1 << bit;
                put_i32(cf, wcfg::VOICE_BITS, bits);
                let on = bits >> bit & 1 != 0;
                self.w.objs.flags(bit as i32 + 130, if on { 0 } else { 2 }, if on { 2 } else { 0 });
                game.play_sys_sound(0);
            }
            _ => game.play_sys_sound(0),
        }
        if press {
            let f = self.w.conf.focus[page];
            self.w.objs.press(f, 10);
        }
        self.wc_apply(game, r, row, false);
        0
    }

    /// The row's kind for the next row's column (the radio's choice is kept when it was its own kind).
    fn wc_mark(&mut self, page: usize, row: i32) -> i32 {
        let k = Self::wc_kind(page, row);
        if k == 3 {
            self.w.conf.sel[page][row as usize] = self.w.conf.col;
        }
        k
    }

    /// Up / down onto row `to` (-1 the tabs): a slider's knob, a radio's choice, a button row's last column,
    /// else the column kept from the row before.
    fn wc_row(&mut self, game: &mut Game, page: usize, row: i32, to: i32, from: i32) {
        let pg = &PAGES[page];
        let c = &mut self.w.conf;
        let obj = if to < 0 {
            c.tab + 50
        } else {
            let t = to as usize;
            let keep = (c.col).min(pg.cols[t] - 1);
            let col = match pg.kind[t] {
                2 => 0,
                3 if from == 3 => keep,
                4 => keep,
                5 if from == 5 => keep,
                _ => c.sel[page][t],
            };
            c.col = col;
            pg.ids[t][col.max(0) as usize]
        };
        c.focus[page] = obj;
        self.w.objs.focus = obj;
        if row != to {
            game.play_sys_sound(2);
        }
    }

    /// Right / left: a slider's step (from 0 the remainder first), A held on a voice slider steps it, else
    /// the column wraps along the row (the tabs along the five). Returns whether a value changed.
    fn wc_side(&mut self, game: &mut Game, page: usize, row: i32, right: bool) -> bool {
        let pg = &PAGES[page];
        let p = self.w.inp.pressed;
        let held = self.w.inp.held;
        let c = &mut self.w.conf;
        if row < 0 {
            let old = c.tab;
            let dir = if right { RIGHT } else { LEFT };
            if right {
                if c.tab > 3 {
                    if p & dir != 0 {
                        c.tab = 0;
                    }
                } else {
                    c.tab += 1;
                }
            } else if c.tab < 1 {
                if p & dir != 0 || c.tab < 0 {
                    c.tab = 4;
                }
            } else {
                c.tab -= 1;
            }
            let obj = c.tab + 50;
            c.focus[page] = obj;
            self.w.objs.focus = obj;
            if old != c.tab {
                game.play_sys_sound(2);
            }
            return false;
        }
        let rw = row as usize;
        let col = c.col;
        let step = pg.step[rw];
        let voice = col & 1 != 0 && pg.kind[rw] == 5 && c.adjust && held & 0x2000 != 0;
        if pg.kind[rw] == 2 || voice {
            let (max, v) = if voice { (100, &mut c.val[page][rw][(col >> 1) as usize]) } else { (255, &mut c.val[page][rw][0]) };
            if right {
                if *v >= max {
                    return false;
                }
                *v = if *v == 0 && max % step != 0 { max % step } else { (*v + step).min(max) };
            } else {
                if *v < 1 {
                    return false;
                }
                *v = (*v - step).max(0);
            }
            return true;
        }
        let n = pg.cols[rw];
        let nc = if right {
            let k = col + i32::from(col < n - 1 || p & RIGHT != 0);
            if k >= n { 0 } else { k }
        } else if col < 1 {
            if p & LEFT != 0 || col != 0 { n - 1 } else { 0 }
        } else {
            col - 1
        };
        // LABEL_533 keeps the object for the page; the objects take it next frame (LABEL_176)
        c.focus[page] = pg.ids[rw][nc.max(0) as usize];
        if col != nc {
            game.play_sys_sound(2);
            c.col = nc;
        }
        false
    }

    /// L / R (or a flick): the tabs swap their look, the page slides out towards `dir`, focus and help off.
    fn wc_slide(&mut self, game: &mut Game, to: i32, dir: f32) {
        let page = self.w.conf.page as i32;
        self.w.conf.tab = to;
        self.w.objs.flags(page + 50, 0, 2);
        self.w.objs.flags(to + 50, 2, 0);
        self.w.objs.press(to + 50, 10);
        self.w.objs.focus = -1;
        self.w.objs.help(-1, false);
        self.w.objs.lock = 2;
        let c = &mut self.w.conf;
        c.st = 4;
        c.dir = dir;
        if (50..=55).contains(&c.focus[to as usize]) {
            c.focus[to as usize] = to + 50;
        }
        game.play_sys_sound(0);
    }

    /// LABEL_336: row `row`'s value into the config block (the change applies at once), then sound 2 for a
    /// stepped value.
    fn wc_apply(&mut self, game: &mut Game, r: &Renderer, row: i32, sound: bool) {
        let c = &self.w.conf;
        let page = c.page;
        let col = c.col.max(0) as usize;
        let Ok(rw) = usize::try_from(row) else { return };
        let sel = c.sel[page].get(rw).copied().unwrap_or(0) as i16;
        let v = c.val[page].get(rw).map_or(0, |v| v[0]) as i16;
        let mut reload = None;
        let mut language = None;
        let cf = self.file.config_mut();
        match (page, rw) {
            (1, 5) => {
                // 初期設定に戻す: every voice on at 100, the voices' master switch back on
                put_i32(cf, wcfg::VOICE_BITS, 0x3FFF_FFFF);
                put_i16(cf, wcfg::VOICE_ALL, 1);
                reload = Some(1);
                for i in 0..15 {
                    put_i16(cf, wcfg::CHAR_VOICE + 2 * i, 100);
                }
            }
            (1, 0..5) if col & 1 != 0 => {
                put_i16(cf, wcfg::CHAR_VOICE + 2 * VOICE_OF[rw][col], c.val[1][rw][col >> 1] as i16);
            }
            (3, 0) => put_i16(cf, wcfg::SKIP_READ_ONLY, sel),
            (3, 1) => put_i16(cf, wcfg::CHOICE_SKIP_STOP, sel),
            (3, 2) => {
                put_i16(cf, wcfg::HELP, sel);
                self.w.objs.help_on = sel != 0;
                self.w.objs.help(if sel == 1 { 126 } else { -1 }, false);
            }
            (3, 3) => {
                put_i16(cf, wcfg::CHOICE_SKIP_STOP, 0);
                put_i16(cf, wcfg::SKIP_READ_ONLY, 1);
                put_i16(cf, wcfg::HELP, 1);
                self.w.objs.help_on = true;
                reload = Some(3);
            }
            (2, 0) => put_i16(cf, wcfg::TEXT_SPEED, v),
            (2, 1) => put_i16(cf, wcfg::AUTO_WAIT, v),
            (2, 2) => put_i16(cf, wcfg::WINDOW_ALPHA, 255 - v),
            (2, 3) => put_i16(cf, wcfg::TEXT_EDGE, sel),
            (2, 4) => {
                // the language arrows: the next / previous of jp_9A700's languages (from the system's -1)
                self.w.objs.help(-1, false);
                let n = game.assets.languages() as i16;
                let old = get_i16(cf, wcfg::LANGUAGE);
                let mut l = if sel != 0 { old + 1 } else { old - 1 };
                if sel != 0 && n <= l {
                    l = 0;
                } else if sel == 0 && l < 0 {
                    l = n - 1;
                }
                put_i16(cf, wcfg::LANGUAGE, l);
                language = Some(l);
            }
            (2, 5) => put_i16(cf, wcfg::TEXT_COLOUR, sel),
            (2, 6) => {
                // 初期設定に戻す: speeds 127, opaque window, edge on, the system's language
                let l = crate::assets::system_language(game.assets.build);
                put_i16(cf, wcfg::TEXT_SPEED, 127);
                put_i16(cf, wcfg::AUTO_WAIT, 127);
                put_i16(cf, wcfg::TEXT_COLOUR, 0);
                put_i32(cf, wcfg::TEXT_EDGE, 1);
                put_i16(cf, wcfg::LANGUAGE, l);
                put_i16(cf, 0x46, 0);
                put_i32(cf, wcfg::WINDOW_ALPHA, 255);
                reload = Some(2);
                language = Some(l);
            }
            (0, 0) => put_i16(cf, wcfg::BGM, v),
            (0, 1) => put_i16(cf, wcfg::VOICE, v),
            (0, 2) => put_i16(cf, wcfg::SE, v),
            (0, 3) => put_i16(cf, wcfg::SYS_SE, v),
            (0, 4) => put_i16(cf, wcfg::VOICE_STOP, sel),
            (0, 5) => put_i16(cf, wcfg::BGM_AUTO, sel),
            (0, 6) => {
                // 初期設定に戻す: the four levels and the movie's 127, voice cut on, BGM auto adjust off
                for o in [wcfg::BGM, wcfg::VOICE, wcfg::SE, wcfg::SYS_SE, wcfg::MOVIE] {
                    put_i16(cf, o, 127);
                }
                put_i16(cf, wcfg::VOICE_STOP, 1);
                put_i16(cf, wcfg::BGM_AUTO, 0);
                reload = Some(0);
            }
            _ => {}
        }
        if let Some(k) = reload {
            self.wc_load(k);
            if k == 1 {
                for v in self.w.conf.val[1].iter_mut().take(5) {
                    *v = [100; 3];
                }
            }
        }
        game.witch_config(self.file.config());
        if let Some(l) = language {
            self.wc_language(game, r, l);
            if rw == 6 {
                self.w.objs.help(118, false);
            } else {
                // sub_D700 case 4 resets the per-message colour words (a bubble's colour follows the language)
                game.reset_colour_words();
            }
        }
        if sound {
            game.play_sys_sound(2);
        }
    }

    /// A new language (sub_85A90, jp_9A940 / jp_9ACB0): the script text, fonts and SYSMES, and every WoH
    /// atlas in it; next frame the arrow's help shows again.
    fn wc_language(&mut self, game: &mut Game, r: &Renderer, word: i16) {
        if game.change_language(r, word) {
            self.sysmes.clear();
            self.font_sys = None;
            self.w.tex.relang(game, r);
            // the title's TITLE_PARTS is one of the loaded atlases jp_9ACB0 reloads too
            if !self.wt.tex.is_empty() {
                self.wt.tex.clear();
                self.wt_load(game, r);
            }
            self.w.conf.lang = i32::from(word);
        }
    }

    /// sub_BD90 (layer 21): the backdrop and tabs at the screen's alpha; the page (panel, row highlights and
    /// separators, captions, the language plate, the voice bars, the knobs at their values and the page's
    /// objects) into draw list 77, replayed at the page's alpha and slide; the title and the guide on top.
    pub(in crate::ui) fn wc_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let c = &self.w.conf;
        if !c.shown {
            return;
        }
        let (a, p, st, page) = (c.a, c.p, c.st, c.page);
        let mut x = if c.still <= 0 { c.band as f32 * 0.025 } else { 0.0 };
        let mut pen = super::Pen::default();
        pen.scale(SX, SY);
        if a > 0.0 {
            pen.alpha(a);
            if let Some(t) = self.w.tex.get(51) {
                pen.spr(out, &t, 16, None, 0.0, 0.0);
            }
            for (k, tx) in TAB_X.iter().enumerate() {
                self.w.objs.place(50 + k as i32, *tx, 31.525925, a);
            }
            let tex = &self.w.tex;
            self.w.objs.draw(&super::Pen::default(), 3, out, &|id| tex.get(id));
        }
        if p > 0.0 {
            let mut alpha = 1.0;
            if st == 4 || st == 5 {
                let d = if st == 5 { c.dir } else { -c.dir };
                let v = 1.0 - p.min(1.0);
                x = v * v * -300.0 * d;
                alpha = p;
            } else if st == 8 || st == 2 {
                let v = (0.7 - p) * 1.4286;
                alpha = if v >= 0.0 { 1.0 - v.min(1.0) } else { 1.0 };
                x = 0.0;
            }
            // jp_9FB80: list 77's draws again at the offset, each at its alpha x the page's
            let dx = x * W / 640.0;
            for mut q in self.wc_page(game, page) {
                for v in &mut q.pos {
                    v[0] += dx;
                }
                q.color[3] *= alpha;
                out.push(q);
            }
        }
        if a > 0.0 {
            let v = if a > 0.7 { 0.0 } else { (0.7 - a) * 1.4286 };
            if v < 1.0 {
                pen.alpha(1.0 - v);
                if let Some(t) = self.w.tex.get(500) {
                    pen.spr(out, &t, 16, None, 0.0, 0.0);
                }
                self.wg_draw(out, &pen, if page == 4 { 1 } else { 2 }, 0.0, 0.0);
            }
        }
        // jp_AE590: the focused object's help balloon
        self.wm_help(game, r, out);
    }

    /// sub_BD90's page: its draws go into draw list 77 (jp_B04B0), which jp_B06C0 / jp_9FAD0 replay.
    fn wc_page(&mut self, game: &Game, page: usize) -> Vec<Quad> {
        let mut s = Vec::new();
        let mut pen = super::Pen::default();
        pen.scale(SX, SY);
        let tex = |id| self.w.tex.get(id);
        let pg = &PAGES[page];
        let c = &self.w.conf;
        if let Some(t) = tex(495) {
            pen.spr(&mut s, &t, 16, None, 17.0, 53.096294);
        }
        // the focused row: -1 on a tab, -2 when the object is on no row
        let f = c.focus[page];
        let lit = if (50..56).contains(&f) {
            -1
        } else {
            (0..pg.rows).rfind(|&row| pg.ids[row][..pg.cols[row].max(0) as usize].contains(&f)).map_or(-2, |r| r as i32)
        };
        let rows: &[i32] = match page {
            0 | 2 => &[0, 1, 2, 3, 4, 5],
            3 => &[0, 1, 2],
            _ => &[],
        };
        for &row in rows {
            let y = 88.0 * row as f32;
            if let Some(t) = tex(494) {
                // unk_2F07A8: nothing, or the highlight band
                let src = if row == lit { [0.0, 14.0, 1752.0, 104.0] } else { [0.0; 4] };
                pen.spr(&mut s, &t, 16, Some(src), 37.0, (y + 174.0) * 0.41481);
            }
            if let Some(t) = tex(496) {
                pen.spr(&mut s, &t, 16, None, 30.666668, (y + 263.0) * 0.41481);
            }
        }
        if let Some(t) = tex(LABELS[page]) {
            for &row in rows {
                // page 0 has no caption for its fifth cell, page 3 none for its third
                let j = row + i32::from((row > 3 && page == 0) || (row > 1 && page == 3));
                let y0 = 88.0 * j as f32;
                pen.spr(&mut s, &t, 16, Some([0.0, y0, 416.0, y0 + 56.0]), 112.0 * 0.33333, (88.0 * row as f32 + 192.0) * 0.41481);
            }
        }
        match page {
            1 => {
                for i in 0..15 {
                    let [x, y] = pg.pos[i / 3][2 * (i % 3) + 1];
                    let held = c.adjust && c.vsel == i as i32;
                    if let Some(t) = tex(502) {
                        let src = if held { [0.0, 0.0, 307.0, 32.0] } else { [0.0, 32.0, 307.0, 64.0] };
                        pen.spr(&mut s, &t, 16, Some(src), x, y);
                    }
                    if let Some(t) = tex(503) {
                        pen.spr(&mut s, &t, 16, None, x + 5.6667, y - 16.593);
                    }
                }
            }
            4 => {
                if let Some(t) = tex(515) {
                    pen.spr(&mut s, &t, 16, None, 0.0, 0.0);
                }
            }
            2 => {
                if let Some(t) = tex(509) {
                    let src = if lit == 4 { [0.0, 0.0, 552.0, 88.0] } else { [0.0, 88.0, 552.0, 176.0] };
                    pen.spr(&mut s, &t, 16, Some(src), 308.0, 219.02);
                }
                // the language's plate (the system's when the word is unset or past the languages)
                let mut l = i32::from(get_i16(self.file.config(), wcfg::LANGUAGE));
                if l < 0 || l >= game.assets.languages() as i32 {
                    l = i32::from(crate::assets::system_language(game.assets.build));
                }
                if let Some(t) = tex(511) {
                    let y0 = 88.0 * l as f32;
                    pen.spr(&mut s, &t, 16, Some([0.0, y0, 552.0, y0 + 88.0]), 308.0, 219.02);
                }
            }
            _ => {}
        }
        let cf = self.file.config();
        let mut places = Vec::new();
        for row in 0..pg.rows {
            let n = pg.cols[row].max(0) as usize;
            match pg.kind[row] {
                2 => {
                    // unk_2F07A0[346 + 30 focused]: the ends' captions, lit on the focused row
                    let (lo, hi) = ENDS[page][row];
                    let id = if page != 2 || row >= 2 { 499 } else { 510 };
                    let yo = if row as i32 == lit { 0.0 } else { 44.0 };
                    let cell = |k: usize| if k & 1 != 0 { [0.0, yo, 120.0, yo + 40.0] } else { [124.0, yo, 244.0, yo + 40.0] };
                    if let Some(t) = tex(id) {
                        let y = row as f32 * 36.504 + 82.963;
                        pen.spr(&mut s, &t, 16, Some(cell(lo)), 213.33, y);
                        pen.spr(&mut s, &t, 16, Some(cell(hi)), 546.67, y);
                    }
                    let [px, py] = pg.pos[row][0];
                    places.push((pg.ids[row][0], px + 10.333 + c.val[page][row][0] as f32 * 278.0 / 255.0, py + 6.637));
                    for col in 1..n {
                        places.push((pg.ids[row][col], pg.pos[row][col][0], pg.pos[row][col][1]));
                    }
                }
                5 => {
                    for (col, &voice) in VOICE_OF[row].iter().enumerate().take(n) {
                        let [px, py] = pg.pos[row][col];
                        if col & 1 != 0 {
                            let v = f32::from(get_i16(cf, wcfg::CHAR_VOICE + 2 * voice));
                            places.push((pg.ids[row][col], px + 9.6667 + v * 82.667 / 100.0, py + 6.637));
                        } else {
                            places.push((pg.ids[row][col], px, py));
                        }
                    }
                }
                _ => {
                    for col in 0..n {
                        places.push((pg.ids[row][col], pg.pos[row][col][0], pg.pos[row][col][1]));
                    }
                }
            }
        }
        for (id, x, y) in places {
            self.w.objs.place(id, x, y, 1.0);
        }
        let tex = &self.w.tex;
        self.w.objs.draw(&super::Pen::default(), page as u8 + 4, &mut s, &|id| tex.get(id));
        s
    }
}
