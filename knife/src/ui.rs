//! System screens: title, in-game menu, save/load, config, backlog and confirmation dialogs.

use std::collections::HashMap;

use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Renderer, TexRef};

use crate::{
    flow,
    fx::{self, LogoFx, MenuBg},
    game::Game,
    save::{self, SaveData},
    savedata::SaveFile,
    text::FontAtlas,
};

mod pad;
mod touch;

pub use pad::Pad;

const W: f32 = CANVAS_W as f32;
const H: f32 = CANVAS_H as f32;
const SLOTS_PER_PAGE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Confirm {
    Title,
    Overwrite(usize),
    Load(usize),
    Jump(u16),
    Movie(usize),
    /// mode panel: previous scene, next scene, quick load (dialogs 11, 10, 9)
    PrevScene,
    NextScene,
    QuickLoad,
    Ending(usize),
    /// config Default (dialog 27)
    Defaults,
}

impl Confirm {
    /// SYSMES message (sub_49710 -> table dword_3C9340) and whether the cursor starts on "No"
    /// (sub_4AD40's second argument selects the initial button; 1 = No).
    fn message(self) -> (usize, bool) {
        match self {
            Confirm::Title => (229, true),
            Confirm::Overwrite(_) => (180, true),
            Confirm::Load(_) => (179, true),
            Confirm::Jump(_) => (1497, false),
            Confirm::Movie(_) => (1443, true),
            Confirm::PrevScene => (233, true),
            Confirm::NextScene => (232, true),
            Confirm::QuickLoad => (230, true),
            Confirm::Ending(i) => (crate::gallery::ENDINGS[i].2, true),
            Confirm::Defaults => (160, true),
        }
    }
}

/// Boot and caution screens: the logo movie and notice (sub_B5650), and the title's PRESS ANY BUTTON
/// follow-up (sub_8E200 states 10..20). Alphas run at the exe's per-frame speeds (60 Hz).
/// A menu cursor snapshot (`Ui::cursor`).
#[derive(Clone, Copy, PartialEq)]
struct Cursor {
    screen: Screen,
    rest: (usize, bool, bool, usize, usize, usize, usize, usize),
    hover: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Intro {
    /// movie/TM_CI.mp4 (skippable after 2 s) over white
    Logo { t: f32 },
    /// parts.mrg #1 (caution2): fades in over white (0.025 / frame), shows up to 8 s (skippable), fades to black
    Notice { a: f32, t: f32, out: bool },
    /// PRESS ANY BUTTON pressed: the title fades to black (0.02 / frame up to 1.2)
    PressOut { a: f32 },
    /// caution / caution1 (title_parts 322, 323): fade 0.02 / frame, at least 4 s (skippable) and at most 12 s;
    /// the second one fades out to white
    Caution { idx: usize, a: f32, t: f32, out: bool, white: f32 },
    /// white (0.01 / frame to 1.2), then START_SCRIPT
    WhiteOut { a: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Screen {
    /// boot sequence / caution screens (see `Intro`)
    Intro,
    Title,
    Game,
    Menu,
    /// save list (sub_4F240): `scroll` is the first visible list entry
    SaveLoad { save: bool, scroll: usize },
    Config,
    Backlog { scroll: usize },
    /// `gallery`: opened from the title's Extra menu (no current playthrough)
    Flowchart { gallery: bool },
    /// CG grid; `scroll` is the first visible row
    Gallery,
    /// music player; `scroll` is the first visible track row
    Music,
    Movies,
    /// a gallery movie is playing
    MoviePlay,
    Endings,
    /// viewing the `pos`-th unlocked CG of a gallery group
    CgView { group: usize, pos: usize },
    Confirm(Confirm),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    NewGame,
    /// the NEW GAME fade finished: start the story
    StartNew,
    Continue,
    /// the CONTINUE fade finished: load this slot
    LoadNow(usize),
    OpenLoad,
    OpenSave,
    OpenConfig,
    OpenFlow,
    Node(u16),
    /// title GALLERY: the gallery in CG mode (title state 48, cGallery)
    OpenExtra,
    /// the screen went black after GALLERY: open the gallery
    EnterGallery,
    OpenGallery,
    Cg(usize),
    NextCg,
    OpenMusic,
    Track(usize),
    /// music player panel button: play, pause, stop, repeat, previous, next
    MusicCtl(u8),
    /// music player volume slider
    MusicVol,
    OpenMovies,
    Movie(usize),
    OpenEndings,
    Ending(usize),
    ToTitle,
    Slot(usize),
    Back,
    Setting(usize, i32),
    Yes,
    No,
}

/// Black message popup (sub_4ABE0, updated by sub_49750): fades in and out at 0.07 per frame; the
/// duration counts update calls (frames), and any button closes it early.
#[derive(Clone, Copy)]
struct Popup {
    id: usize,
    frames: f32,
    a: f32,
    closing: bool,
    /// held by the caller (sub_4ABD0(1)): buttons do not close it; it closes `close_at` frames after opening
    /// (the save flow's 30-frame minimum, dword_F1F468, then sub_4ACD0 once the write is done)
    close_at: Option<f32>,
    age: f32,
}

/// Mode banner (sub_3B430 / sub_3B4B0, popup_ja): 0 Skip, 1 Quick Save, 2 Auto Save, 3 Auto. `t` rises
/// 0.04 / frame to 3 (the first third pops it in), then falls from 1 to 0 replaying the pop backwards.
/// The "complete" banner on the title (sub_90440 / sub_91350): state 1 rises in, 2 holds, 3 fades out.
#[derive(Clone, Copy)]
struct Complete {
    state: u8,
    a: f32,
    off: f32,
    hold: f32,
}

#[derive(Clone, Copy)]
struct Banner {
    idx: usize,
    out: bool,
    t: f32,
}

/// popup_ja rows per banner (x0, y0, x1, y1, from dword_572B48).
const BANNER_SRC: [[f32; 4]; 4] = [[8.0, 200.0, 424.0, 256.0], [8.0, 72.0, 424.0, 128.0], [8.0, 8.0, 424.0, 64.0], [8.0, 136.0, 424.0, 192.0]];
/// unk_572BBC: (x scale, y scale) keys over t * 9.
const BANNER_KEYS: [[f32; 2]; 10] = [
    [0.0, 1.4],
    [0.2, 1.23],
    [0.6, 1.0],
    [1.2, 0.8],
    [1.15, 0.9],
    [1.1, 0.98],
    [1.03, 1.06],
    [1.02, 1.04],
    [1.0, 1.0],
    [1.0, 1.0],
];

pub struct Ui {
    pub screen: Screen,
    /// a system sound already played for the input being handled (no cursor-move sound 0 on top)
    sounded: bool,
    /// screen to return to from a sub screen
    stack: Vec<Screen>,
    hover: Option<usize>,
    mouse: (f32, f32),
    tex: HashMap<String, TexRef>,
    /// savedata.bin
    pub file: SaveFile,
    slots: Vec<Option<SaveData>>,
    /// message popup (sub_4ABE0 / sub_49750)
    popup: Option<Popup>,
    /// confirm dialog open animation, 0..1
    dlg_t: f32,
    title_movie: bool,
    /// the title was up last frame: entering it (sub_8D2E0) and leaving it (sub_90180) step the title stage
    on_title: bool,
    complete: Option<Complete>,
    /// unlocked entries per CG gallery group, refreshed when the gallery opens
    cg_unlocked: Vec<Vec<usize>>,
    music_ok: Vec<bool>,
    playing_track: Option<usize>,
    /// music player: seconds into the playing track, volume 0..1, repeat toggle
    music_clock: f32,
    music_vol: f32,
    music_repeat: bool,
    music_paused: bool,
    /// CG viewer zoom (0 = fit) and pan offset
    cg_zoom: f32,
    cg_pan: [f32; 2],
    /// size of the picture on view (for the Minus fill zoom)
    cg_size: [f32; 2],
    /// drawing the screen below a dialog / viewer (no guide or popup)
    under: bool,
    last_track: Option<usize>,
    /// SYSMES_TEXT strings (music titles), loaded on first use
    sysmes: Vec<String>,
    /// seconds since the title screen appeared (logo / menu fade-in)
    title_clock: f32,
    /// config screen tab (0 system, 1 sound, 2 controls) and its content scroll in canvas pixels as drawn
    conf_tab: usize,
    conf_scroll: f32,
    /// scroll target per tab (byte_AED320 + 72, kept across tab switches) and the tween towards it
    /// (qword_AED360: 12 frames, ease-out quad)
    conf_target: [f32; 3],
    conf_from: f32,
    conf_t: f32,
    /// flowchart layout, its scroll and the node under the cursor
    flow: Option<crate::flow::Chart>,
    flow_scroll: f32,
    /// flowchart scroll target and the tween towards it (sub_27BD4: 1/6 s, ease-out quad)
    flow_target: f32,
    flow_from: f32,
    flow_t: f32,
    flow_cur: usize,
    /// boot / caution sequence (Screen::Intro)
    intro: Option<Intro>,
    /// title in PRESS ANY BUTTON mode (sub_90180: the first title of a session before the cautions)
    pub press: bool,
    /// set at launch and cleared once the cautions were shown or skipped (byte at 0xAC0DE0)
    boot_flag: bool,
    /// title logo / particle alpha (qword_1536194), whether it is rising, and the PRESS ANY BUTTON blink clock
    logo_a: f32,
    logo_rising: bool,
    press_t: f32,
    logo_fx: LogoFx,
    menu_bg: MenuBg,
    /// black screen overlay (0..1.2) with its per-frame speed, and what to do once it is fully black
    black: f32,
    black_speed: f32,
    black_then: Option<Action>,
    /// colour of that overlay (NEW GAME fades to white)
    black_col: [f32; 3],
    /// CONTINUE: "ロード中です" is opening; then the title fades out and this slot loads
    cont_slot: Option<usize>,
    /// a load picked in the title's load window: the window closes first (sub_50FC0 state 19 -> 5 -> 27)
    load_after_close: Option<usize>,
    /// title state 30 after a load (byte_1536155): the logo, its glow and the menu stay hidden over the movie
    title_ui_hidden: bool,
    banner: Option<Banner>,
    /// back to title from the system menu: the menu fades while the Auto Save banner plays (state 18)
    title_after_banner: bool,
    /// the black belongs to the title (title state 48, under the submenu backdrop)
    black_under: bool,
    /// content alpha of the top screen; `closing` fades it out and then leaves the screen
    scr_a: f32,
    closing: bool,
    /// gallery mode crossfade (sub_8400: 0.5 s out, 0.5 s in) and the mode waiting for the fade-out
    gal_a: f32,
    gal_next: Option<Screen>,
    /// cGallery focus: the mode buttons (state 2) or the mode's content (state 4), the button cursor
    /// (a1 + 48), focus the new mode's content once a mode switch lands (sub_7F38 with a controller),
    /// and the content entry left when the focus went to the buttons
    gal_side: bool,
    gal_tab: usize,
    gal_focus_next: bool,
    gal_mem: Option<Action>,
    /// CG grid / music list scroll (sub_276B0 scroller of cGalleryCGMain + 3800 / cGalleryMusicList + 1608)
    gal_scroll: ListScroll,
    /// music panel button to come back to (a1 + 60) and the one the volume slider returns to (a1 + 64)
    music_btn: usize,
    music_vol_from: usize,
    /// music list cursor row (cGalleryMusicList a1 + 52), kept while the panel or the buttons have the focus
    music_row: usize,
    /// the key event being handled is an auto-repeat (the exe wraps lists only on the trigger set)
    nav_repeat: bool,
    /// config scroll widget shown (sub_2AA80: kept on a tab without scrolling, never shown first there)
    /// scroll widget alpha (sub_2AA80, qword_AED360 + 36)
    conf_sb_a: f32,
    /// tab content alpha (qword_AED338 low) and the tab being switched to (dword_AED330, sub_2C380 state 4/5)
    conf_ca: f32,
    conf_next: Option<usize>,
    /// shown voice rows of the sound tab (sub_29FA0): (row, conf_faceicon cell, conf_name cell)
    voice_rows: Vec<[usize; 3]>,
    /// put the gallery cursor back after a screen on top of it closed
    gal_restore: bool,
    /// config message speed samples (sub_2D570): revealed characters, fully shown characters, hold frames
    conf_prev: [[f32; 3]; 2],
    /// parts.mrg #1 (caution2), the boot notice
    notice: Option<TexRef>,
    /// help balloon (sub_AE110): the item it belongs to and its fade (+0.05 per frame, reset on change)
    help_item: Option<(usize, [i32; 2])>,
    /// the cursor was last moved with the keys (the mouse only hovers; it never takes the focus off the tab bar)
    key_focus: bool,
    help_t: f32,
    /// UI fonts besides the message font: ja2 TsukuGo (help balloon), ja3 FOT-Seurat (menu chapter name)
    font_go: Option<FontAtlas>,
    font_ui: Option<FontAtlas>,
    /// system menu tab under the cursor (HIDWORD(qword_B359EC)): kept across menu invocations
    sys_tab: usize,
    /// focus is on the system menu tabs while one of its submenus stays open (sub_4D580 -> sub_4CEA0),
    /// and the submenu item to return to
    side: bool,
    sub_hover: Option<usize>,
    /// config tab bar cursor (qword_AED328: tabs 51..53, default 54) as an index into the tab bar
    conf_bar: usize,
    /// the confirm button is held (a held A with left / right drags a config slider)
    pub enter_held: bool,
    /// touch ripples, story gestures and the mode panel
    touch: touch::Touch,
}

fn rect_hit(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.0 < r[0] + r[2] && p.1 >= r[1] && p.1 < r[1] + r[3]
}

/// Config scroll view (x, y, w, h) in canvas pixels (sub_B2E60(407, 165, 1402, 858)).
const CONF_VIEW: [f32; 4] = [407.0, 165.0, 1402.0, 858.0];
/// Scroll range per tab in canvas pixels (qword_46F810 + 16).
/// Config scroll range of the system tab (0x46F820); the sound tab's follows its rows (conf_max).
const CONF_MAX_SYSTEM: f32 = 485.0;
/// Config scroll tween length (sub_9AD04: 12 frames).
const CONF_SCROLL_MS: f32 = 200.0;
/// Voice setting rows (conf_faceicon / conf_name entries).
const VOICE_ROWS: usize = 19;

/// A config screen control. Positions come from the layout table at 0x46D238 (640x448 units) and
/// the object frames at 0x46B370.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ctl {
    Tab(usize),
    Default,
    Minus(usize),
    Plus(usize),
    Slider(usize),
    /// btn_base2 check buttons: page-at-once (unread, read), skip confirmation
    Toggle(usize),
    /// btn_base1 radio buttons: (pair, value)
    Radio(usize, bool),
    Mute(usize),
    Test(usize),
}

impl Ctl {
    fn code(self) -> usize {
        match self {
            Ctl::Tab(i) => i,
            Ctl::Default => 10,
            Ctl::Minus(i) => 100 + i,
            Ctl::Plus(i) => 200 + i,
            Ctl::Slider(i) => 300 + i,
            Ctl::Toggle(i) => 400 + i,
            Ctl::Radio(i, v) => 500 + i * 2 + usize::from(v),
            Ctl::Mute(i) => 600 + i,
            Ctl::Test(i) => 700 + i,
        }
    }

    fn from_code(c: usize) -> Option<Self> {
        Some(match c {
            0..=2 => Ctl::Tab(c),
            10 => Ctl::Default,
            100..=199 => Ctl::Minus(c - 100),
            200..=299 => Ctl::Plus(c - 200),
            300..=399 => Ctl::Slider(c - 300),
            400..=499 => Ctl::Toggle(c - 400),
            500..=599 => Ctl::Radio((c - 500) / 2, c % 2 == 1),
            600..=699 => Ctl::Mute(c - 600),
            700..=799 => Ctl::Test(c - 700),
            _ => return None,
        })
    }
}

/// conf_faceicon / conf_name source rects (x, y, w, h) per cell (tables at 0x34D500 / 0x34D5A0): one per voice
/// row, 18 / 19 the unknown faces and name.
const FACE_RECTS: [[f32; 4]; VOICE_ROWS + 1] = {
    const T: [[i16; 4]; VOICE_ROWS + 1] = [
        [32, 32, 336, 112], [32, 128, 336, 208], [32, 224, 336, 304], [32, 320, 336, 400], [32, 416, 336, 496],
        [32, 512, 336, 592], [32, 608, 336, 688], [32, 704, 336, 784], [32, 800, 336, 880], [32, 896, 336, 976],
        [352, 32, 656, 112], [352, 128, 656, 208], [352, 320, 656, 400], [352, 224, 656, 304], [352, 416, 656, 496],
        [352, 512, 656, 592], [352, 608, 656, 688], [352, 704, 656, 784], [352, 896, 656, 976],
        [352, 800, 656, 880],
    ];
    rects(T)
};
const NAME_RECTS: [[f32; 4]; VOICE_ROWS + 1] = {
    const T: [[i16; 4]; VOICE_ROWS + 1] = [
        [32, 32, 168, 80], [32, 96, 168, 144], [32, 160, 168, 208], [32, 224, 168, 272], [32, 288, 168, 336],
        [32, 352, 168, 400], [32, 416, 168, 464], [184, 32, 320, 80], [184, 96, 320, 144], [184, 160, 320, 208],
        [184, 224, 320, 272], [184, 288, 320, 336], [184, 416, 320, 464], [184, 352, 320, 400], [336, 32, 472, 80],
        [336, 96, 472, 144], [336, 160, 472, 208], [336, 224, 472, 272], [336, 352, 472, 400],
        [336, 416, 472, 464],
    ];
    rects(T)
};

/// (x0, y0, x1, y1) -> (x, y, w, h)
const fn rects<const N: usize>(t: [[i16; 4]; N]) -> [[f32; 4]; N] {
    let mut o = [[0.0; 4]; N];
    let mut i = 0;
    while i < N {
        let [x0, y0, x1, y1] = t[i];
        o[i] = [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32];
        i += 1;
    }
    o
}

/// btn_base1 rows (y, width) for each radio pair's (true, false) buttons.
const RADIO_CELLS: [[(f32, f32); 2]; 6] = [
    [(6.0, 140.0), (94.0, 140.0)],
    [(182.0, 140.0), (270.0, 188.0)],
    [(358.0, 228.0), (446.0, 260.0)],
    [(534.0, 260.0), (622.0, 260.0)],
    [(6.0, 140.0), (94.0, 140.0)],
    [(6.0, 140.0), (94.0, 140.0)],
];
/// savetitle source rects (x0, y0, x1, y1) by phase F023 (table at 0x592484).
const SAVE_TITLES: [[i32; 4]; 38] = [
    [0, 0, 1, 1], [32, 88, 288, 128], [32, 32, 288, 72], [32, 144, 288, 184], [32, 144, 288, 184], [32, 200, 288, 240],
    [32, 200, 288, 240], [32, 256, 288, 296], [32, 256, 288, 296], [32, 312, 288, 352], [32, 312, 288, 352],
    [32, 368, 288, 408], [32, 368, 288, 408], [304, 32, 560, 72], [304, 144, 560, 184], [304, 88, 560, 128],
    [304, 200, 560, 240], [304, 256, 560, 296], [304, 312, 560, 352], [304, 368, 560, 408], [576, 88, 832, 128],
    [576, 32, 832, 72], [576, 200, 832, 240], [576, 144, 832, 184], [32, 424, 288, 464], [576, 312, 832, 352],
    [576, 256, 832, 296], [576, 368, 832, 408], [848, 32, 1104, 72], [848, 88, 1104, 128], [848, 144, 1104, 184],
    [848, 256, 1104, 296], [848, 200, 1104, 240], [848, 200, 1104, 240], [848, 200, 1104, 240], [848, 312, 1104, 352],
    [848, 368, 1104, 408], [0, 0, 1, 1],
];
/// Route of phases 11..36 for the save info panel (dword_3506B0: 548 arc = 0, 549 ciel = 1, 550 unknown = 2).
const INFO_ROUTE: [u8; 26] = [2, 1, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 0, 1, 1, 1, 1, 1];
/// system_phasetitle_ja source rects (x0, y0, x1, y1) by phase index F023 (dword_5912E8 + 156*4).
const PHASE_TITLES: &[[i32; 4]] = &[
    [0, 0, 1, 1], [1192, 696, 1760, 776], [24, 24, 592, 104], [24, 120, 592, 200], [24, 120, 592, 200],
    [24, 216, 592, 296], [24, 216, 592, 296], [24, 312, 592, 392], [24, 312, 592, 392], [24, 408, 592, 488],
    [24, 408, 592, 488], [1192, 792, 1760, 872], [1192, 792, 1760, 872], [24, 504, 592, 584], [24, 696, 592, 776],
    [24, 600, 592, 680], [24, 792, 592, 872], [24, 888, 592, 968], [608, 24, 1176, 104], [608, 120, 1176, 200],
    [608, 312, 1176, 392], [608, 216, 1176, 296], [608, 504, 1176, 584], [608, 408, 1176, 488], [608, 600, 1176, 680],
    [608, 792, 1176, 872], [608, 696, 1176, 776], [608, 888, 1176, 968], [1192, 24, 1760, 104], [1192, 120, 1760, 200],
    [1192, 216, 1760, 296], [1192, 408, 1760, 488], [1192, 312, 1760, 392], [1192, 312, 1760, 392], [1192, 312, 1760, 392],
    [1192, 600, 1760, 680], [1192, 504, 1760, 584], [0, 0, 1, 1],
];
/// Button guide (sub_4A8F0): per guide index (unk_34FD48, 160 bytes each) a list of
/// (0 = help_key icon / 1 = help_txt_ja word, cell) pairs.
const GUIDE_SEQ: [&[(u8, u8)]; 11] = [
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 0), (1, 2), (0, 11), (1, 26)],
    &[(0, 10), (1, 0), (0, 15), (1, 13)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 0), (1, 12), (0, 15), (1, 13), (0, 11), (1, 26)],
    &[(0, 10), (1, 0), (0, 7), (1, 11), (0, 0), (1, 2)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 0), (1, 2)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 0), (1, 2)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 8), (1, 21), (0, 9), (1, 22), (0, 17), (0, 16), (1, 13), (0, 0), (1, 2), (0, 11), (1, 26)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 14), (0, 13), (1, 13), (0, 0), (1, 2), (0, 11), (1, 26)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 14), (0, 13), (1, 13), (0, 0), (1, 2)],
    &[(0, 10), (1, 0), (0, 15), (1, 23), (0, 11), (1, 25), (0, 12), (1, 24)],
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 0), (1, 2)],
];
/// help_key cells (a0080h, x0 y0 x1 y1): 0..6 +pad, 7 A, 8 Y, 9 X, 10 B, 11 L stick, 12 R stick, ...
const GUIDE_ICONS: [[u16; 4]; 23] = [
    [8, 8, 48, 48],
    [56, 8, 96, 48],
    [104, 8, 144, 48],
    [152, 8, 192, 48],
    [200, 8, 240, 48],
    [248, 8, 288, 48],
    [296, 8, 336, 48],
    [8, 56, 48, 96],
    [56, 56, 96, 96],
    [104, 56, 144, 96],
    [152, 56, 192, 96],
    [200, 56, 240, 96],
    [248, 56, 288, 96],
    [288, 50, 328, 90],
    [16, 98, 56, 138],
    [56, 104, 96, 144],
    [8, 152, 52, 192],
    [60, 152, 104, 192],
    [152, 104, 192, 144],
    [112, 152, 156, 192],
    [164, 152, 208, 192],
    [104, 104, 148, 144],
    [200, 104, 240, 144],
];
/// help_txt_ja words in atlas order (the exe loads these rects at runtime): 0 戻る 1 決定 2 移動 3 解除 4 再生
/// 5 変更 6 初期化 7 セーブ 8 ロード 9 ボイス登録 10 ボイス再生 11 クイックジャンプ 12 カーソル移動 13 ページ変更
/// 14 キャラクター変更 15 連続再生 16 削除 17 再生(長押しで連続再生) 18 データコピー 19 データ削除 20 再生曲変更
/// 21 停止 22 ループ変更 23 画像の切替 24 拡大・縮小 25 移動 26 スクロール
const GUIDE_WORDS: [[u16; 4]; 27] = [
    [9, 10, 51, 32],
    [65, 10, 108, 32],
    [121, 10, 164, 32],
    [177, 10, 220, 32],
    [232, 10, 276, 32],
    [289, 10, 332, 32],
    [344, 10, 408, 32],
    [9, 45, 73, 68],
    [86, 45, 147, 68],
    [161, 45, 268, 68],
    [281, 45, 388, 68],
    [9, 81, 153, 104],
    [165, 81, 296, 104],
    [309, 81, 420, 104],
    [9, 117, 153, 141],
    [165, 117, 252, 141],
    [265, 117, 308, 141],
    [320, 117, 497, 141],
    [10, 153, 139, 176],
    [154, 153, 264, 176],
    [276, 153, 388, 176],
    [401, 153, 444, 176],
    [9, 189, 120, 213],
    [133, 189, 244, 213],
    [257, 189, 368, 213],
    [381, 189, 424, 213],
    [10, 227, 120, 248],
];
/// chart view height (sub_1FA60 clip 1920x1022, above the guide bar)
const FLOW_VIEW: f32 = 1022.0;
/// Flowchart scroll tween (sub_27BD4 with 0.16667 s).
const FLOW_SCROLL_MS: f32 = 1000.0 / 6.0;
/// cGalleryCGMain (sub_AF78): 6 columns at x 392 + 240c, rows at y 109 + 139r inside the clip 109..1014
const CG_COLS: usize = 6;
/// cGalleryMusicList (sub_1A1A4): track rows every 59px from y 109
/// gallery content clip (sub_1FA60(0, 109, 1920, 905))
const GAL_CLIP: [f32; 2] = [109.0, 1014.0];
/// sub_27BD4 tween (0.16667 s, ease-out quad)
const LIST_SCROLL_MS: f32 = 1000.0 / 6.0;
/// music list scroll range (sub_1A1A4)
const MUSIC_SCROLL_MAX: f32 = 4720.0;

/// A scroll position moved by sub_27BD4: clamped to [0, max], animated over 1/6 s (ease-out quad) or set
/// at once.
#[derive(Clone, Copy, Default)]
struct ListScroll {
    pos: f32,
    target: f32,
    from: f32,
    t: f32,
}

impl ListScroll {
    fn to(&mut self, v: f32, max: f32, animate: bool) {
        self.target = v.clamp(0.0, max.max(0.0));
        if animate {
            self.from = self.pos;
            self.t = 0.0;
        } else {
            self.pos = self.target;
            self.t = LIST_SCROLL_MS;
        }
    }

    fn step(&mut self, dt: f32) {
        if self.t < LIST_SCROLL_MS {
            self.t = (self.t + dt).min(LIST_SCROLL_MS);
            let t = self.t / LIST_SCROLL_MS;
            self.pos = self.from + (self.target - self.from) * -(t * (t - 2.0));
        }
    }
}
/// music player buttons (objects 385..390): play, pause, stop, repeat at y 576, previous, next at y 656
const MUSIC_CTL: [(f32, f32); 6] = [(1432.0, 576.0), (1512.0, 576.0), (1592.0, 576.0), (1672.0, 576.0), (1512.0, 656.0), (1592.0, 656.0)];

impl Ui {
    pub fn new(game: &mut Game, r: &Renderer) -> Self {
        let mut tex = HashMap::new();
        for stem in ["TITLE_PARTS_JA", "MENU_PARTS_JA", "SAVE_PARTS_JA", "CONF_PARTS_JA", "DIALOG_PARTS_JA", "DISP_PARTS_JA", "MODEPNL_PARTS_JA"] {
            match game.assets.ui_textures(r, stem) {
                Ok(list) => tex.extend(list),
                Err(e) => tracing::warn!("ui textures {stem}: {e:#}"),
            }
        }
        let file = SaveFile::load();
        let mut ui = Self {
            screen: Screen::Title,
            stack: Vec::new(),
            hover: None,
            mouse: (0.0, 0.0),
            tex,
            file,
            slots: Vec::new(),
            popup: None,
            dlg_t: 0.0,
            title_movie: false,
            on_title: false,
            complete: None,
            cg_unlocked: Vec::new(),
            music_ok: Vec::new(),
            playing_track: None,
            music_clock: 0.0,
            music_vol: 1.0,
            music_repeat: true,
            music_paused: false,
            cg_zoom: 0.0,
            cg_pan: [0.0; 2],
            cg_size: [0.0; 2],
            under: false,
            last_track: None,
            sysmes: Vec::new(),
            title_clock: 0.0,
            conf_tab: 0,
            conf_scroll: 0.0,
            conf_target: [0.0; 3],
            conf_from: 0.0,
            conf_t: CONF_SCROLL_MS,
            flow: None,
            flow_scroll: 0.0,
            flow_target: 0.0,
            flow_from: 0.0,
            flow_t: FLOW_SCROLL_MS,
            flow_cur: 0,
            intro: None,
            press: false,
            boot_flag: true,
            logo_a: 0.0,
            logo_rising: true,
            press_t: 0.0,
            logo_fx: LogoFx::new(0x9e37_79b9_7f4a_7c15),
            menu_bg: MenuBg::new(0x2545_f491_4f6c_dd1d),
            black: 0.0,
            black_speed: 0.0,
            black_then: None,
            black_col: [0.0; 3],
            cont_slot: None,
            load_after_close: None,
            title_ui_hidden: false,
            banner: None,
            title_after_banner: false,
            black_under: false,
            scr_a: 1.0,
            closing: false,
            gal_a: 1.0,
            gal_next: None,
            gal_side: true,
            gal_tab: 0,
            gal_focus_next: false,
            gal_mem: None,
            gal_scroll: ListScroll::default(),
            music_btn: 0,
            music_vol_from: 4,
            music_row: 0,
            nav_repeat: false,
            conf_sb_a: 0.0,
            conf_ca: 1.0,
            conf_next: None,
            voice_rows: Vec::new(),
            gal_restore: false,
            conf_prev: [[0.0; 3]; 2],
            notice: None,
            help_item: None,
            key_focus: false,
            sys_tab: 0,
            sounded: false,
            side: false,
            sub_hover: None,
            conf_bar: 0,
            enter_held: false,
            touch: touch::Touch::new(),
            help_t: 0.0,
            font_go: None,
            font_ui: None,
        };
        ui.open_save_data(game);
        // sub_B5650: byte_AC0DE0 is cleared at launch once the prologue (node 485) was reached
        ui.boot_flag = !crate::game::get_bit(&game.nodes, 485);
        ui.refresh_slots(game);
        ui
    }

    /// Startup: settings and progress from savedata.bin.
    fn open_save_data(&mut self, game: &mut Game) {
        game.load_system(&self.file);
    }

    /// Write settings and progress (and the records already in memory) to savedata.bin.
    pub fn store_global(&mut self, game: &Game) {
        game.store_system(&mut self.file);
        if let Err(e) = self.file.store() {
            tracing::warn!("saving savedata.bin: {e:#}");
        }
    }

    /// Put a snapshot into a record, then write the file. The save list rebuilds its preview from the
    /// record (sub_884F0), so nothing else is stored.
    fn write_record(&mut self, game: &mut Game, slot: usize, data: &SaveData) -> anyhow::Result<()> {
        let rec = game.encode_record(data).ok_or_else(|| anyhow::anyhow!("position not saveable"))?;
        if slot == save::AUTO_FIRST {
            self.file.shift_auto();
        }
        self.file.set_record(slot, &rec);
        self.file.update_continue();
        game.store_system(&mut self.file);
        self.file.store()?;
        game.clear_previews(None);
        Ok(())
    }

    /// cGallery mode buttons (objects 379..384, sub_6124): CG, Music, Flowchart, Movie, Ending, Back
    fn hub_items() -> Vec<([f32; 4], Action)> {
        [Action::OpenGallery, Action::OpenMusic, Action::OpenFlow, Action::OpenMovies, Action::OpenEndings, Action::Back]
            .iter()
            .enumerate()
            .map(|(i, a)| ([80.0, 160.0 + i as f32 * 80.0, 256.0, 96.0], *a))
            .collect()
    }

    /// CG groups shown in the grid (sub_B830): only groups with at least one unlocked entry.
    fn cg_list(&self) -> Vec<usize> {
        (0..self.cg_unlocked.len()).filter(|&g| !self.cg_unlocked[g].is_empty()).collect()
    }

    /// Thumbnail rectangle of grid entry `k` (154x87 atlas cell at 1.5x).
    fn cg_cell(k: usize, scroll: f32) -> [f32; 4] {
        let (col, row) = ((k % CG_COLS) as f32, (k / CG_COLS) as f32);
        [392.0 + col * 240.0, 109.0 + row * 139.0 - scroll.trunc(), 231.0, 130.5]
    }

    /// sub_926F0(1): every ending node 533..556 completed (unlocks the QA_25 button, object 418).
    fn all_endings(game: &Game) -> bool {
        (533..=556).all(|n| crate::game::get_bit(&game.endings, n))
    }

    /// Movies in the movie gallery (sub_17DD0): the prologue, then each opening once unlocked.
    fn movie_list(&self, game: &Game) -> Vec<usize> {
        (0..crate::gallery::MOVIES.len())
            .filter(|&i| crate::gallery::MOVIES[i].1.is_none_or(|f| game.movie_flags & (1 << f) != 0))
            .collect()
    }

    /// Switch the gallery hub's content: from the hub this pushes, between modes it replaces.
    fn enter_mode(&mut self, game: &mut Game, s: Screen) {
        if Self::is_gallery_mode(self.screen) {
            // sub_8400: the current mode's content fades out over 0.5 s, then the new one fades in
            if std::mem::discriminant(&s) != std::mem::discriminant(&self.screen) {
                self.snd(game, 1);
                if self.playing_track.take().is_some() {
                    self.music_paused = false;
                    game.play_bgm(0, 1.0, 0.5);
                }
                self.gal_next = Some(s);
            }
        } else {
            // entering the gallery: the mode buttons have the focus (cGallery state 2)
            self.gal_focus_next = false;
            self.push(s);
            self.gal_mode_shown(game);
        }
    }

    /// A gallery mode just came on screen: its list starts at the top and the focus goes to its content
    /// when the mode was picked with the buttons (sub_7328), else stays on the buttons.
    fn gal_mode_shown(&mut self, game: &Game) {
        self.gal_scroll = ListScroll::default();
        self.gal_mem = None;
        self.music_btn = 0;
        self.music_row = 0;
        self.gal_tab = Self::gal_mode(self.screen).unwrap_or(0);
        self.hover = None;
        if std::mem::take(&mut self.gal_focus_next) && self.gal_to_content(game) {
            return;
        }
        self.gal_side = true;
        if self.key_focus {
            self.hover = self.items(game).iter().position(|(_, a)| *a == Self::hub_items()[self.gal_tab].1);
        }
    }

    /// The mode a gallery screen shows (cGallery a1 + 52: 0 CG, 1 music, 2 flowchart, 3 movie, 4 ending).
    fn gal_mode(s: Screen) -> Option<usize> {
        match s {
            Screen::Gallery => Some(0),
            Screen::Music => Some(1),
            Screen::Movies => Some(3),
            Screen::Endings => Some(4),
            _ => None,
        }
    }

    /// Scroll range of the CG grid (sub_AF78: (rows - 6) * 139 - 69.5) or the music list (4720).
    fn gal_scroll_max(&self) -> f32 {
        match self.screen {
            Screen::Gallery => (self.cg_list().len().div_ceil(CG_COLS) as f32 - 6.0) * 139.0 - 69.5,
            Screen::Music => MUSIC_SCROLL_MAX,
            _ => 0.0,
        }
    }

    /// The content entries of the current gallery mode a cursor can rest on, in their navigation order.
    fn gal_content(&self, game: &Game) -> Vec<Action> {
        match self.screen {
            Screen::Gallery => self.cg_list().into_iter().map(Action::Cg).collect(),
            Screen::Music => (0..crate::gallery_table::MUSIC.len()).map(Action::Track).collect(),
            Screen::Movies => self.movie_list(game).into_iter().map(Action::Movie).collect(),
            Screen::Endings => (0..25).filter(|&i| Self::ending_ok(game, i)).map(Action::Ending).collect(),
            _ => Vec::new(),
        }
    }

    /// Ending entry i reached (3 main endings, 21 bad ends, 24 = the QA_25 button).
    fn ending_ok(game: &Game, i: usize) -> bool {
        if i == 24 { Self::all_endings(game) } else { crate::game::get_bit(&game.nodes, crate::gallery::ENDINGS[i].0) }
    }

    /// Put the cursor on `a` (an entry of the current screen's items).
    fn hover_action(&mut self, game: &Game, a: Action) {
        self.hover = self.items(game).iter().position(|(_, b)| *b == a);
        if let Action::Track(i) = a {
            self.music_row = i;
        }
    }

    /// The buttons take the focus (content vt + 80 / hub state 2): the cursor goes back to the open mode.
    fn gal_to_side(&mut self, game: &Game) {
        let items = self.items(game);
        self.gal_mem = self.hover.and_then(|h| items.get(h)).map(|(_, a)| *a).filter(|a| !Self::hub_items().iter().any(|(_, b)| b == a));
        self.gal_side = true;
        self.gal_tab = Self::gal_mode(self.screen).unwrap_or(0);
        self.hover = items.iter().position(|(_, a)| *a == Self::hub_items()[self.gal_tab].1);
    }

    /// The mode's content takes the focus (content vt + 40) when it has something to select (vt + 32): back on
    /// the entry it had, else its first one.
    fn gal_to_content(&mut self, game: &Game) -> bool {
        let acts = self.gal_content(game);
        let Some(&first) = acts.first() else { return false };
        let a = match (self.screen, self.gal_mem) {
            (Screen::Music, Some(Action::MusicCtl(_) | Action::MusicVol)) => Action::MusicCtl(self.music_btn as u8),
            (_, Some(m)) if acts.contains(&m) => m,
            _ => first,
        };
        self.gal_side = false;
        self.gal_tab = Self::gal_mode(self.screen).unwrap_or(0);
        // scrolled out of view: bring it in first
        if let Action::Cg(_) | Action::Track(_) = a {
            self.gal_follow(game, a, None, false);
        }
        self.hover_action(game, a);
        true
    }

    /// Keep a CG row (sub_A968) or music row (sub_1B2F0) in view after a cursor move from row `prev`
    /// (None: either way).
    fn gal_follow(&mut self, game: &Game, a: Action, prev: Option<usize>, animate: bool) {
        let max = self.gal_scroll_max();
        let s = self.gal_scroll.target;
        match a {
            Action::Cg(g) => {
                let Some(k) = self.cg_list().iter().position(|&x| x == g) else { return };
                let (row, last) = (k / CG_COLS, self.cg_list().len().div_ceil(CG_COLS) - 1);
                let y = 109.0 + row as f32 * 139.0 - s;
                if prev.is_none_or(|p| row > p) && y > 804.0 {
                    let mut v = (row as f32 - 5.0) * 139.0;
                    if row == last {
                        v -= 69.5;
                    }
                    self.gal_scroll.to(v, max, animate);
                } else if prev.is_none_or(|p| row < p) && y < 178.5 {
                    let v = if row == 0 { 0.0 } else { (row as f32 - 1.0) * 139.0 + 69.5 };
                    self.gal_scroll.to(v, max, animate);
                }
            }
            Action::Track(row) => {
                let y = 173.0 + row as f32 * 59.0 - s;
                if prev.is_none_or(|p| row > p) && y > 940.0 {
                    self.gal_scroll.to((row as f32 - 13.0) * 59.0, max, animate);
                } else if prev.is_none_or(|p| row < p) && y < 232.0 {
                    self.gal_scroll.to(if row == 0 { 0.0 } else { (row as f32 - 1.0) * 59.0 }, max, animate);
                }
            }
            _ => {}
        }
        let _ = game;
    }

    /// After a manual scroll the cursor row is pulled back into view: CG cursor y (cell - 6) inside 103..943
    /// (sub_9CBC), music row y inside 138..940 (sub_1A7FC).
    fn gal_keep_cursor(&mut self, game: &Game) {
        if self.gal_side {
            return;
        }
        let s = self.gal_scroll.target;
        let cur = self.hover.and_then(|h| self.items(game).get(h).map(|(_, a)| *a)).or(self.gal_mem);
        match cur {
            Some(Action::Cg(g)) => {
                let list = self.cg_list();
                let Some(k) = list.iter().position(|&x| x == g) else { return };
                let (col, mut row) = (k % CG_COLS, k / CG_COLS);
                let mut y = 103.0 + row as f32 * 139.0 - s;
                while y < 103.0 {
                    y += 139.0;
                    row += 1;
                }
                while y >= 943.0 && row > 0 {
                    y -= 139.0;
                    row -= 1;
                }
                if let Some(&g) = list.get(col + row * CG_COLS) {
                    self.gal_mem = Some(Action::Cg(g));
                    self.hover_action(game, Action::Cg(g));
                }
            }
            Some(Action::Track(mut row)) => {
                let n = crate::gallery_table::MUSIC.len();
                let mut y = 131.0 + row as f32 * 59.0 - s;
                while y < 138.0 && row + 1 < n {
                    y += 59.0;
                    row += 1;
                }
                while y >= 940.0 && row > 0 {
                    y -= 59.0;
                    row -= 1;
                }
                self.gal_mem = Some(Action::Track(row));
                self.hover_action(game, Action::Track(row));
            }
            _ => {}
        }
    }

    /// Arrows and A in a gallery mode, from the exe handlers: the mode buttons (cGallery sub_6E28) and each
    /// mode's content (CG grid sub_9CBC, music list sub_1A7FC and panel sub_1C9FC / sub_1CC68, movies
    /// sub_183EC, endings sub_E7A4). Returns false when the A press is left to the item action.
    fn gal_nav(&mut self, game: &mut Game, r: &Renderer, dx: i32, dy: i32, enter: bool) -> bool {
        let Some(mode) = Self::gal_mode(self.screen) else { return false };
        let items = self.items(game);
        let hub = Self::hub_items();
        let cur = self.hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
        if self.gal_side {
            if dy != 0 {
                // wraps only on a fresh press
                let t = self.gal_tab as i32 + dy;
                if (0..6).contains(&t) || !self.nav_repeat {
                    self.gal_tab = t.rem_euclid(6) as usize;
                }
            } else if dx > 0 {
                // right: to the open mode's content
                self.gal_tab = mode;
                if !self.gal_to_content(game) {
                    self.hover = items.iter().position(|(_, a)| *a == hub[mode].1);
                }
                return true;
            } else if enter {
                if self.gal_tab == mode {
                    if self.gal_to_content(game) {
                        self.snd(game, 1);
                    }
                    return true;
                }
                let a = hub[self.gal_tab].1;
                // a mode picked with the buttons opens with its content focused (sub_7F38 a2 = 1)
                self.gal_focus_next = matches!(a, Action::OpenGallery | Action::OpenMusic | Action::OpenMovies | Action::OpenEndings);
                self.gal_mem = None;
                self.act(game, r, a);
                return true;
            }
            if dx == 0 {
                self.hover = items.iter().position(|(_, a)| *a == hub[self.gal_tab].1);
            }
            return true;
        }
        if enter {
            return match cur {
                // A on prev / next held on the volume slider does nothing
                Some(Action::MusicVol) => true,
                _ => false,
            };
        }
        if dx == 0 && dy == 0 {
            return true;
        }
        let acts = self.gal_content(game);
        match (self.screen, cur) {
            (Screen::Gallery, Some(Action::Cg(g))) => {
                let n = acts.len();
                let Some(k) = acts.iter().position(|&a| a == Action::Cg(g)) else { return true };
                let (col, row) = (k % CG_COLS, k / CG_COLS);
                let next = if dx < 0 {
                    if col == 0 {
                        self.gal_to_side(game);
                        return true;
                    }
                    k - 1
                } else if dx > 0 {
                    if col >= CG_COLS - 1 || k + 1 >= n {
                        return true;
                    }
                    k + 1
                } else if dy < 0 {
                    if row == 0 {
                        return true;
                    }
                    k - CG_COLS
                } else {
                    if k + CG_COLS >= n {
                        return true;
                    }
                    k + CG_COLS
                };
                self.gal_follow(game, acts[next], Some(row), dy != 0);
                self.gal_mem = Some(acts[next]);
                self.hover_action(game, acts[next]);
            }
            (Screen::Music, Some(Action::Track(i))) => {
                let n = acts.len();
                if dx < 0 {
                    self.gal_to_side(game);
                    return true;
                }
                if dx > 0 {
                    // to the player panel, on the button it had
                    self.gal_mem = Some(Action::Track(i));
                    self.hover_action(game, Action::MusicCtl(self.music_btn as u8));
                    return true;
                }
                let t = i as i32 + dy;
                if !(0..n as i32).contains(&t) && self.nav_repeat {
                    return true;
                }
                let next = t.rem_euclid(n as i32) as usize;
                self.gal_follow(game, Action::Track(next), Some(i), true);
                self.gal_mem = Some(Action::Track(next));
                self.hover_action(game, Action::Track(next));
            }
            (Screen::Music, Some(Action::MusicCtl(k))) => {
                // buttons 0 play, 1 pause, 2 stop, 3 repeat (top row), 4 prev, 5 next
                let k = usize::from(k);
                let next = match (dx, dy) {
                    (-1, _) if k == 0 || k == 4 => {
                        self.music_btn = 0;
                        let back = Action::Track(self.music_row);
                        self.gal_follow(game, back, None, false);
                        self.hover_action(game, back);
                        return true;
                    }
                    (-1, _) => k - 1,
                    (1, _) if k <= 2 || k == 4 => k + 1,
                    (_, -1) if k >= 4 => k - 3,
                    (_, 1) if k <= 1 => 4,
                    (_, 1) if k <= 3 => 5,
                    (_, 1) => {
                        // down again from prev / next: the volume slider (sub_1EC0C)
                        self.music_vol_from = k;
                        self.hover_action(game, Action::MusicVol);
                        return true;
                    }
                    _ => k,
                };
                self.music_btn = next;
                self.hover_action(game, Action::MusicCtl(next as u8));
            }
            (Screen::Music, Some(Action::MusicVol)) => {
                if dx != 0 {
                    self.music_vol = (self.music_vol + 0.01 * dx as f32).clamp(0.0, 1.0);
                    game.sound.set_bgm_volume(self.music_vol, 0.1);
                } else if dy < 0 {
                    self.music_btn = self.music_vol_from;
                    self.hover_action(game, Action::MusicCtl(self.music_vol_from as u8));
                }
            }
            (Screen::Movies, Some(a @ Action::Movie(_))) => {
                let Some(k) = acts.iter().position(|&b| b == a) else { return true };
                if dx < 0 {
                    if k == 0 {
                        self.gal_to_side(game);
                    } else {
                        self.hover_action(game, acts[k - 1]);
                    }
                } else if dx > 0 && k + 1 < acts.len() {
                    self.hover_action(game, acts[k + 1]);
                }
            }
            (Screen::Endings, Some(Action::Ending(i))) => match Self::ending_move(game, i, dx, dy) {
                Some(Some(j)) => self.hover_action(game, Action::Ending(j)),
                Some(None) => self.gal_to_side(game),
                None => {}
            },
            _ => {
                // nothing selected (e.g. the pointer left the list): the content's first entry
                if let Some(&a) = acts.first() {
                    self.hover_action(game, a);
                }
            }
        }
        true
    }

    /// Ending list cursor move (sub_F470 left, sub_F5A4 right, sub_F6F8 up / down): row 0 holds the three
    /// main endings, rows 1..3 the bad ends 8 to a row with the QA_25 button as entry 21 (row 3, column 5).
    /// Only reached entries take the cursor. Some(None): left past the first entry, to the mode buttons.
    fn ending_move(game: &Game, i: usize, dx: i32, dy: i32) -> Option<Option<usize>> {
        // (col, row) <-> entry
        let pos = |i: usize| if i < 3 { (i, 0) } else { ((i - 3) % 8, (i - 3) / 8 + 1) };
        let entry = |c: usize, r: usize| if r == 0 { c } else { 3 + c + 8 * (r - 1) };
        let ok = |c: usize, r: usize| {
            let valid = if r == 0 { c < 3 } else { r <= 3 && c < 8 && (r < 3 || c <= 5) };
            valid && Self::ending_ok(game, entry(c, r))
        };
        let (col, row) = pos(i);
        if dx < 0 {
            return Some((0..col).rev().find(|&c| ok(c, row)).map(|c| entry(c, row)));
        }
        if dx > 0 {
            let last = if row == 0 { 2 } else { 7 };
            return (col + 1..=last).find(|&c| ok(c, row)).map(|c| Some(entry(c, row)));
        }
        let qa = Self::all_endings(game);
        let nearest = |r: usize| -> Option<usize> {
            if r == 0 {
                let want = if row == 0 { col } else if col < 3 { 0 } else if col < 6 { 1 } else { 2 };
                // ties keep the lower column
                let mut best: Option<(usize, usize)> = None;
                for c in 0..3 {
                    if ok(c, 0) && best.is_none_or(|(_, d)| want.abs_diff(c) < d) {
                        best = Some((c, want.abs_diff(c)));
                    }
                }
                return best.map(|(c, _)| c);
            }
            let from = if row == 0 { [2, 5, 7][col] } else { col };
            // coming down from columns 6 / 7 lands on the QA button; going up from it, on column 7
            if qa && r == 3 && dy > 0 && (col & !1) == 6 && row > 0 {
                return Some(5);
            }
            if qa && r == 2 && dy < 0 && row == 3 && col == 5 && ok(7, 2) {
                return Some(7);
            }
            // ties take the later column
            let cols = if r == 3 { 5 } else { 8 };
            let mut best: Option<(usize, usize)> = None;
            for c in 0..cols {
                if ok(c, r) && best.is_none_or(|(_, d)| from.abs_diff(c) <= d) {
                    best = Some((c, from.abs_diff(c)));
                }
            }
            best.map(|(c, _)| c)
        };
        let rows: Vec<usize> = if dy < 0 { (0..row).rev().collect() } else { (row + 1..=3).collect() };
        rows.into_iter().find_map(|r| nearest(r).map(|c| Some(entry(c, r))))
    }

    fn refresh_gallery(&mut self, game: &Game) {
        use crate::game::get_bit;
        self.cg_unlocked = crate::gallery::unlocked(|n| game.is_seen(n), |n| get_bit(&game.nodes, n), |n| get_bit(&game.endings, n));
    }

    fn refresh_slots(&mut self, game: &mut Game) {
        self.slots = (0..save::SLOTS)
            .map(|n| {
                let rec = self.file.record(n)?;
                let d = game.decode_record(rec);
                if d.is_none() {
                    tracing::warn!("save record {n} does not resolve to a script position");
                }
                d
            })
            .collect();
        game.clear_previews(None);
    }

    /// sub_4ABE0(message, frames): dialog_win with a SYSMES line, without buttons.
    fn popup(&mut self, game: &Game, id: usize, frames: f32) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        self.popup = Some(Popup {
            id,
            frames,
            a: 0.0,
            closing: false,
            close_at: None,
            age: 0.0,
        });
    }

    /// A popup the caller closes itself after `frames` frames (counted from opening); buttons are ignored.
    fn popup_held(&mut self, game: &Game, id: usize, frames: f32) {
        self.popup(game, id, f32::INFINITY);
        if let Some(p) = &mut self.popup {
            p.close_at = Some(frames);
        }
    }

    /// A button while the popup is up closes it (sub_49750); the input goes no further.
    fn popup_input(&mut self) -> bool {
        match &mut self.popup {
            Some(p) => {
                if p.close_at.is_none() {
                    p.closing = true;
                }
                true
            }
            None => false,
        }
    }

    /// Debug: open a screen by name on top of the current one.
    pub fn debug_open(&mut self, game: &mut Game, r: &Renderer, name: &str) {
        // "<screen>@N": then put the cursor on item N
        if let Some((name, n)) = name.rsplit_once('@')
            && let Ok(n) = n.parse()
        {
            self.debug_open(game, r, name);
            self.hover = Some(n);
            return;
        }
        // "menu/<screen>": open the system menu first, then <screen> from it
        if let Some(rest) = name.strip_prefix("menu/") {
            self.debug_open(game, r, "menu");
            self.debug_open(game, r, rest);
            return;
        }
        // "<screen>@<n>": also hover item n (for screenshots)
        if let Some((base, n)) = name.split_once('@') {
            self.debug_open(game, r, base);
            self.hover = n.parse().ok();
            return;
        }
        if self.debug_intro(game, r, name) {
            return;
        }
        // the title's NEW GAME / CONTINUE, and the system menu's title return (after its confirmation)
        match name {
            "new_game" => return self.act(game, r, Action::NewGame),
            "continue" => return self.act(game, r, Action::Continue),
            "to_title" => {
                self.debug_open(game, r, "menu");
                return self.leave_to_title(game, r);
            }
            _ => {}
        }
        if name == "gallery_open" {
            // the title's GALLERY entry with its transition
            self.act(game, r, Action::OpenExtra);
            return;
        }
        let gallery = ["music", "movies", "endings", "gallery", "extra"].contains(&name)
            || ["cg:", "track:", "movie:", "ending:"].iter().any(|p| name.starts_with(p));
        if gallery {
            self.act(game, r, Action::EnterGallery);
        }
        let open = match name {
            "extra" => return,
            "music" => Some(Action::OpenMusic),
            "movies" => Some(Action::OpenMovies),
            "endings" => Some(Action::OpenEndings),
            _ => None,
        };
        // debug: mode switches apply at once (no crossfade)
        let mut mode = |ui: &mut Self, a: Action| {
            ui.act(game, r, a);
            if let Some(s) = ui.gal_next.take() {
                ui.screen = s;
                ui.gal_mode_shown(game);
            }
        };
        if let Some(a) = open {
            mode(self, a);
            return;
        }
        if let Some(n) = name.strip_prefix("track:").and_then(|n| n.parse().ok()) {
            mode(self, Action::OpenMusic);
            mode(self, Action::Track(n));
            return;
        }
        if let Some(n) = name.strip_prefix("ending:").and_then(|n| n.parse().ok()) {
            mode(self, Action::OpenEndings);
            mode(self, Action::Ending(n));
            return;
        }
        if let Some(n) = name.strip_prefix("movie:").and_then(|n| n.parse().ok()) {
            mode(self, Action::OpenMovies);
            mode(self, Action::Movie(n));
            return;
        }
        if name == "gallery" || name.starts_with("cg:") {
            self.act(game, r, Action::OpenGallery);
            if let Some(g) = name.strip_prefix("cg:").and_then(|g| g.parse().ok()) {
                self.act(game, r, Action::Cg(g));
            }
            return;
        }
        let s = match name {
            "menu" => {
                self.menu_bg.fade(0.0555);
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                Screen::Menu
            }
            "config" | "config1" | "config2" => {
                self.conf_tab = name.strip_prefix("config").and_then(|t| t.parse().ok()).unwrap_or(0);
                self.refresh_voice_rows(game);
                self.conf_show_tab();
                Screen::Config
            }
            "save" => {
                self.act(game, r, Action::OpenSave);
                return;
            }
            "load" => {
                self.act(game, r, Action::OpenLoad);
                return;
            }
            "backlog" => Screen::Backlog { scroll: 0 },
            "flowchart" if self.screen == Screen::Menu => {
                // "menu/flowchart": the system menu's button, with its availability check
                self.act(game, r, Action::OpenFlow);
                return;
            }
            "flowchart" | "flowchartg" => {
                self.open_flow(game, name == "flowchartg");
                return;
            }
            _ => {
                tracing::warn!("unknown screen {name}");
                return;
            }
        };
        if self.screen == Screen::Title && matches!(s, Screen::Config | Screen::SaveLoad { .. }) {
            self.menu_bg.fade(0.02);
        }
        self.push(s);
    }

    fn push(&mut self, s: Screen) {
        self.stack.push(self.screen);
        self.screen = s;
        self.hover = if s == Screen::Menu { Some(self.sys_tab) } else { None };
        self.side = false;
        self.closing = false;
        self.scr_a = if Self::fade_speeds(s).0 < 1.0 { 0.0 } else { 1.0 };
        self.gal_a = 1.0;
        self.gal_next = None;
        if let Screen::Confirm(c) = s {
            self.dlg_t = 0.0;
            self.hover = Some(usize::from(c.message().1));
        }
    }

    /// Open a confirm dialog (loads the SYSMES strings it needs).
    fn confirm(&mut self, game: &mut Game, c: Confirm) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        // sub_4AD40: system sound 3
        self.snd(game, 3);
        self.push(Screen::Confirm(c));
    }

    fn pop(&mut self) {
        self.screen = self.stack.pop().unwrap_or(Screen::Game);
        // back in the system menu the cursor stays on the tab that was open (state 9 / 12)
        self.hover = if self.screen == Screen::Menu { Some(self.sys_tab) } else { None };
        self.side = false;
        // back in the gallery (from the CG viewer, a movie, a dialog or the flowchart): the cursor returns
        // to where the focus was, the button cursor onto the open mode (sub_7624)
        if let Some(m) = Self::gal_mode(self.screen) {
            self.gal_tab = m;
            self.gal_restore = true;
        }
        self.closing = false;
        self.scr_a = 1.0;
    }

    pub fn in_game(&self) -> bool {
        self.screen == Screen::Game
    }

    // ---- help balloon -----------------------------------------------------------------------

    /// Load a UI font from romfs (ja2 / ja3).
    fn ui_font(game: &Game, r: &Renderer, path: &str) -> Option<FontAtlas> {
        let data = std::fs::read(game.assets.romfs.join(path)).map_err(|e| tracing::warn!("{path}: {e}")).ok()?;
        FontAtlas::new(r, data).map_err(|e| tracing::warn!("{path}: {e:#}")).ok()
    }

    /// Help text of the focused object (object tables: system menu 0x591438, config 0x46B370):
    /// (SYSMES id, object position, object rect, flags), all in 640x448 units.
    fn help_target(&self) -> Option<(usize, [f32; 2], [f32; 4], u32)> {
        let hover = self.hover;
        match self.screen {
            // objects 197..201 placed by sub_4C370
            // the focused tab (sub_AE0E0): the menu's cursor, or the side cursor over an open submenu
            Screen::Menu | Screen::SaveLoad { .. } | Screen::Config if self.screen == Screen::Menu || self.side => {
                let h = self.sys_tab;
                let id = [161, 162, 163, 168, 166][h];
                Some((id, [26.667, 43.556 + 31.111 * h as f32], [0.0, 0.0, 74.0, 29.0], 0x120))
            }
            // tab objects 51..53 and the default button 54
            // the focus object stays on the current tab (sub_29F00 / sub_AE040), so its help is always up
            // the balloon belongs to the tab bar: it goes away while the cursor is on the page or the side tabs
            Screen::Config if !self.side => {
                let layout = self.conf_layout();
                let tab = layout.iter().position(|(c, _)| *c == Ctl::Tab(self.conf_tab));
                let h = match hover {
                    Some(h) if matches!(layout.get(h), Some((Ctl::Tab(_) | Ctl::Default, _))) => h,
                    // a page element has the focus (keys or pointer): the balloon goes
                    Some(_) => return None,
                    None => tab?,
                };
                let (c, p) = layout.get(h).copied()?;
                let (id, rc) = match c {
                    Ctl::Tab(i) => ([140, 1, 158][i], [0.0, 4.0, 64.0, 29.0]),
                    Ctl::Default => (159, [0.0, 0.0, 96.0, 29.0]),
                    _ => return None,
                };
                Some((id, [p[0] / 3.0, p[1] * 448.0 / H], rc, 0x320))
            }
            _ => None,
        }
    }

    /// sub_AE240 / sub_AE590: the balloon sits at a quarter of the object's rect towards the screen centre,
    /// offset by 6 units, as mode_help stretched to the text width + 40 behind the text (alpha 0.7).
    fn draw_help(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some((id, pos, rc, flags)) = self.help_target() else { return };
        let a = self.help_t * self.scr_a;
        if a <= 0.0 || self.busy() || self.popup.is_some() {
            return;
        }
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let Some(text) = self.sysmes.get(id).cloned().filter(|t| !t.is_empty()) else { return };
        if self.font_go.is_none() {
            self.font_go = Self::ui_font(game, r, "ja2/TT_TsukuGo-D.ttf");
        }
        let Some(font) = self.font_go.as_mut() else { return };
        // text slot 2: 0.325 x 0.37 of the 60 px base glyph, letter spaced
        const SIZE: f32 = 25.5;
        const SPACING: f32 = -2.0;
        let n = text.chars().count() as f32;
        let tw = (font.measure(&text, SIZE) + SPACING * (n - 1.0).max(0.0)) / 3.0;
        let (x0, y0, x1, y1) = (pos[0] + rc[0], pos[1] + rc[1], pos[0] + rc[2], pos[1] + rc[3]);
        let mut q = match (x0 + x1 >= 640.0, y0 + y1 < 448.0) {
            (true, true) => 1,
            (true, false) => 0,
            (false, true) => 2,
            (false, false) => 3,
        };
        if flags & 0x100 != 0 {
            q = [1, 1, 2, 2][q];
        }
        if flags & 0x200 != 0 {
            q = [3, 2, 2, 3][q];
        }
        let mut ox = if q <= 1 { (x1 + 3.0 * x0) / 4.0 } else { (3.0 * x1 + x0) / 4.0 };
        let oy = if q == 0 || q == 3 { (y1 + 3.0 * y0) / 4.0 } else { (3.0 * y1 + y0) / 4.0 };
        let (dx, dy) = [(-6.0, -6.0), (-6.0, 6.0), (6.0, 6.0), (6.0, -6.0)][q];
        if q > 1 && ox + tw + 3.0 > 624.0 {
            ox = 624.0 - tw - 3.0 - dx;
        }
        let w = tw + 6.0;
        let mut x = ox + dx;
        let mut y = oy + dy;
        if q <= 1 {
            x = (x - w).max(0.0);
        }
        if q == 0 || q == 3 {
            y -= 13.0;
        }
        if let Some(t) = self.tex.get("mode_help") {
            let (bw, bh) = (t.width as f32 * (w + 40.0) / 640.0, t.height as f32 * 0.41489);
            out.push(Quad::rect(t.clone(), [(x - 20.0) * 3.0, y * H / 448.0, bw * 3.0, bh * H / 448.0], [1.0, 1.0, 1.0, a * 0.7]));
        }
        // white text with a dark rim
        let (tx, base) = ((x + 2.0) * 3.0, (y + 3.0) * H / 448.0 + SIZE);
        for (ddx, ddy, c) in [(-1.5, 0.0, 0.0), (1.5, 0.0, 0.0), (0.0, -1.5, 0.0), (0.0, 1.5, 0.0), (0.0, 0.0, 1.0)] {
            let mut cx = tx + ddx;
            for ch in text.chars() {
                if let Some(qd) = font.quad(r, ch, SIZE, cx, base + ddy, [c, c, c, if c > 0.0 { a } else { a * 0.6 }]) {
                    out.push(qd);
                }
                cx += font.advance(ch, SIZE) + SPACING;
            }
        }
    }

    // ---- boot, title and transitions ------------------------------------------------------

    /// Launch sequence (sub_B5650 / sub_8D2E0). Once the prologue was reached (node 485), the TYPE-MOON
    /// movie and the notice play before the title (`skip_intro` leaves them out). Before that the prologue
    /// scene (id 260) starts right away; its end returns to a title that asks for PRESS ANY BUTTON.
    pub fn boot(&mut self, game: &mut Game, r: &Renderer, skip_intro: bool) {
        if crate::game::get_bit(&game.nodes, 485) {
            self.boot_flag = false;
            if skip_intro {
                self.enter_title();
            } else {
                game.gallery_movie = true;
                game.play_movie(r, "TM_CI");
                self.intro = Some(Intro::Logo { t: 0.0 });
                self.screen = Screen::Intro;
            }
        } else if let Some(scene) = game.assets.scene_by_id(260) {
            game.start_scene(scene, None);
            self.screen = Screen::Game;
        }
    }

    /// Show the title, fading in from black (sub_8E200 state 2: 0.012 / frame).
    fn enter_title(&mut self) {
        self.screen = Screen::Title;
        self.stack.clear();
        self.intro = None;
        self.press = self.boot_flag;
        self.press_t = 0.0;
        self.logo_a = 0.0;
        self.logo_rising = true;
        self.title_movie = false;
        self.title_ui_hidden = false;
        self.black = 1.0;
        self.black_speed = -0.012;
        self.scr_a = 1.0;
        self.closing = false;
        self.hover = self.title_cursor(Action::NewGame);
    }

    /// The title cursor as sub_8E200 state 14 sets it: CONTINUE when it is offered, else LOAD GAME with any
    /// save among records 0..99 (sub_4EEB0), else NEW GAME (`fallback` replaces that last choice; leaving the
    /// gallery puts it on GALLERY instead).
    fn title_cursor(&self, fallback: Action) -> Option<usize> {
        let saves = self.slots.iter().take(100).any(|s| s.is_some());
        let target = if fallback != Action::NewGame {
            fallback
        } else if self.continue_slot().is_some() {
            Action::Continue
        } else if saves {
            Action::OpenLoad
        } else {
            Action::NewGame
        };
        self.title_entries().iter().position(|e| e.0 == target)
    }

    /// Debug: the title in PRESS ANY BUTTON mode, or a caution screen.
    pub fn debug_intro(&mut self, game: &mut Game, r: &Renderer, name: &str) -> bool {
        match name {
            "press" => {
                self.boot_flag = true;
                self.enter_title();
                self.black = 0.0;
            }
            "notice" => {
                self.load_notice(game, r);
                self.intro = Some(Intro::Notice { a: 1.0, t: 0.0, out: false });
                self.screen = Screen::Intro;
            }
            "caution" | "caution1" => {
                self.intro = Some(Intro::Caution { idx: usize::from(name == "caution1"), a: 1.0, t: 0.0, out: false, white: 0.0 });
                self.screen = Screen::Intro;
            }
            _ => return false,
        }
        true
    }

    fn load_notice(&mut self, game: &Game, r: &Renderer) {
        if self.notice.is_none() {
            self.notice = game.assets.parts_texture(r, 1);
        }
    }

    /// A button on an intro screen (or on PRESS ANY BUTTON): skips what the exe lets skip.
    fn intro_input(&mut self, game: &mut Game) {
        match &mut self.intro {
            Some(Intro::Logo { t }) if *t >= 2.0 => game.stop_movie(),
            Some(Intro::Notice { out, .. }) => *out = true,
            Some(Intro::Caution { t, out, .. }) if *t >= 240.0 => *out = true,
            None if self.screen == Screen::Title && self.press && self.black <= 0.0 => {
                // sub_8E200 state 9 -> 10: system sound 6, the title fades to black, then the cautions
                game.play_sys_sound(6);
                game.play_bgm(0, 1.0, 0.5);
                self.intro = Some(Intro::PressOut { a: 0.0 });
                self.screen = Screen::Intro;
            }
            _ => {}
        }
    }

    fn intro_update(&mut self, game: &mut Game, r: &Renderer, f: f32) {
        let Some(intro) = self.intro else { return };
        self.intro = match intro {
            Intro::Logo { t } => {
                if game.movie_active() {
                    Some(Intro::Logo { t: t + f / 60.0 })
                } else {
                    game.gallery_movie = false;
                    self.load_notice(game, r);
                    Some(Intro::Notice { a: 0.0, t: 0.0, out: false })
                }
            }
            Intro::Notice { a, t, out } if !out => {
                let a = (a + 0.025 * f).min(1.0);
                let t = if a >= 1.0 { t + f } else { t };
                Some(Intro::Notice { a, t, out: t >= 480.0 })
            }
            Intro::Notice { a, t, .. } => {
                let a = a - 0.025 * f;
                if a <= 0.0 {
                    self.enter_title();
                    return;
                }
                Some(Intro::Notice { a, t, out: true })
            }
            Intro::PressOut { a } => {
                let a = a + 0.02 * f;
                if a > 1.2 {
                    Some(Intro::Caution { idx: 0, a: 0.0, t: 0.0, out: false, white: 0.0 })
                } else {
                    Some(Intro::PressOut { a })
                }
            }
            Intro::Caution { idx, a, t, out: false, white } => {
                let a = (a + 0.02 * f).min(1.0);
                let t = if a >= 1.0 { t + f } else { t };
                Some(Intro::Caution { idx, a, t, out: t >= 720.0, white })
            }
            Intro::Caution { idx, a, t, white, .. } => {
                let a = a - 0.02 * f;
                let white = if idx == 1 { white + 0.02 * f } else { white };
                if a > 0.0 {
                    Some(Intro::Caution { idx, a, t, out: true, white })
                } else if idx == 0 {
                    Some(Intro::Caution { idx: 1, a: 0.0, t: 0.0, out: false, white })
                } else {
                    // state 13 -> 20: the cautions were shown (sub_B5620)
                    self.boot_flag = false;
                    Some(Intro::WhiteOut { a: white })
                }
            }
            Intro::WhiteOut { a } => {
                let a = a + 0.01 * f;
                if a > 1.2 {
                    // state 20 with node 485 read: start at START_SCRIPT (scene id 110)
                    self.intro = None;
                    self.press = false;
                    self.act(game, r, Action::StartNew);
                    return;
                }
                Some(Intro::WhiteOut { a })
            }
        };
    }

    fn draw_intro(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let white = r.white.clone();
        let full = [0.0, 0.0, W, H];
        match self.intro {
            Some(Intro::Logo { .. }) => {
                if !game.movie_playing() {
                    out.push(Quad::rect(white.clone(), full, [1.0; 4]));
                }
            }
            Some(Intro::Notice { a, out: o, .. }) => {
                let bg = if o { 0.0 } else { 1.0 };
                out.push(Quad::rect(white.clone(), full, [bg, bg, bg, 1.0]));
                if let Some(t) = &self.notice {
                    out.push(Quad::rect(t.clone(), full, [1.0, 1.0, 1.0, a.min(1.0)]));
                }
            }
            Some(Intro::PressOut { a }) => {
                self.draw_title(game, r, out);
                out.push(Quad::rect(white.clone(), full, [0.0, 0.0, 0.0, a.min(1.0)]));
            }
            Some(Intro::Caution { idx, a, white: w, .. }) => {
                out.push(Quad::rect(white.clone(), full, [0.0, 0.0, 0.0, 1.0]));
                let tex = ["caution(UpRGB)(scale)(x1.500000)", "caution1_1920x1080"][idx];
                self.sprite_c(out, tex, full, None, a.clamp(0.0, 1.0), Blend::Alpha);
                if w > 0.0 {
                    out.push(Quad::rect(white.clone(), full, [1.0, 1.0, 1.0, w.min(1.0)]));
                }
            }
            Some(Intro::WhiteOut { a }) => {
                out.push(Quad::rect(white.clone(), full, [0.0, 0.0, 0.0, 1.0]));
                out.push(Quad::rect(white.clone(), full, [1.0, 1.0, 1.0, a.min(1.0)]));
            }
            None => {}
        }
    }

    /// Per-screen content fade speeds (per frame): (in, out). Save/load and config: sub_50FC0 / sub_2B680
    /// (0.024, 0.048); system menu: sub_4CBA0 / sub_4D340 (0.0555, 0.0333); gallery: sub_47A90 (0.04).
    fn fade_speeds(s: Screen) -> (f32, f32) {
        match s {
            Screen::SaveLoad { .. } | Screen::Config => (0.024, 0.048),
            Screen::Menu => (0.0555, 0.0333),
            Screen::Gallery | Screen::Music | Screen::Movies | Screen::Endings => (0.04, 0.04),
            _ => (1.0, 1.0),
        }
    }

    fn is_gallery_mode(s: Screen) -> bool {
        matches!(s, Screen::Gallery | Screen::Music | Screen::Movies | Screen::Endings)
    }

    /// Leave the current screen with its fade-out (the pop happens in `update`).
    fn close(&mut self) {
        if Self::fade_speeds(self.screen).1 < 1.0 {
            self.closing = true;
        } else {
            self.pop();
        }
    }

    /// The fade-out finished: leave the screen; back on the title or in game the backdrop fades out.
    fn finish_close(&mut self, game: &mut Game) {
        let from = self.screen;
        if Self::is_gallery_mode(from) && self.playing_track.take().is_some() {
            self.music_paused = false;
            game.play_bgm(0, 1.0, 0.5);
        }
        self.pop();
        match self.screen {
            Screen::Title if Self::is_gallery_mode(from) => {
                // leaving the gallery (sub_5040): backdrop out at 0.04, the title fades in from black
                self.menu_bg.fade(-0.04);
                self.enter_title();
                // byte_1536158: the cursor comes back on GALLERY
                self.hover = self.title_cursor(Action::OpenExtra);
            }
            Screen::Title => {
                self.menu_bg.fade(-0.02);
                if let Some(i) = self.load_after_close.take() {
                    // title case 32, result 2: BGM out over 1.5 s, the UI hidden (byte_1536155), state 30 fades
                    // to black (+0.01 / frame to 1.2), then the game
                    game.sound.stop_bgm(1.5);
                    self.title_ui_hidden = true;
                    self.black_col = [0.0; 3];
                    self.black_under = false;
                    self.black_speed = 0.01;
                    self.black_then = Some(Action::LoadNow(i));
                }
            }
            Screen::Game => self.menu_bg.fade(-0.0333),
            _ => {}
        }
    }

    // ---- layout ------------------------------------------------------------------

    fn items(&self, game: &Game) -> Vec<([f32; 4], Action)> {
        match self.screen {
            // title menu (sub_8D2E0 / sub_8D5D0): sprite objects 232..237 placed from the slot table at
            // unk_352AF4 in a 640x448 space; CONTINUE only shows when there is something to resume
            Screen::Title if self.press => Vec::new(),
            Screen::Title => self
                .title_entries()
                .into_iter()
                .enumerate()
                .map(|(i, (a, _, _))| (Self::title_slot(i, self.title_entries().len()), a))
                .collect(),
            // system menu (sub_4C370): objects 197..201 at x 26.667, y 43.556 + 31.111*i (640x448 units)
            Screen::Menu => [
                Action::OpenSave,
                Action::OpenLoad,
                Action::OpenConfig,
                Action::OpenFlow,
                Action::ToTitle,
            ]
            .iter()
            .enumerate()
            .map(|(i, a)| ([80.0, 105.0 + i as f32 * 75.0, 224.0, 88.0], *a))
            .collect(),
            // save list (sub_4F240): four save_win frames at x 133.67, y 36.919 + 93.748*i (640x448)
            Screen::SaveLoad { save, scroll } => {
                let mut v: Vec<([f32; 4], Action)> = self
                    .save_list(save)
                    .into_iter()
                    .skip(scroll)
                    .take(SLOTS_PER_PAGE)
                    .enumerate()
                    .map(|(i, slot)| {
                        let [x, y] = Self::save_frame(i);
                        ([x + 32.0, y + 28.0, 898.0, 212.0], Action::Slot(slot))
                    })
                    .collect();
                if !self.stack.contains(&Screen::Menu) {
                    v.push(([0.0, 0.0, 360.0, H], Action::Back));
                }
                v
            }
            // config (sub_2AA80): tab buttons, the default button and the current tab's controls,
            // with content hit rects cut to the scroll view
            Screen::Config => self
                .conf_layout()
                .into_iter()
                .map(|(c, p)| {
                    let mut rc = Self::conf_rect(c, p);
                    if !matches!(c, Ctl::Tab(_) | Ctl::Default) {
                        let y0 = rc[1].max(CONF_VIEW[1]);
                        let y1 = (rc[1] + rc[3]).min(CONF_VIEW[1] + CONF_VIEW[3]);
                        rc = [rc[0], y0, rc[2], (y1 - y0).max(0.0)];
                    }
                    (rc, Action::Setting(c.code(), 0))
                })
                .collect(),
            // selectable chart nodes (scrolled, clipped to the chart view above the guide bar)
            Screen::Flowchart { .. } => self
                .flow
                .iter()
                .flat_map(|c| &c.nodes)
                .map(|n| {
                    let y = n.pos[1] - self.flow_scroll;
                    let (y0, y1) = (y.max(0.0), (y + flow::NODE_H).min(FLOW_VIEW));
                    ([n.pos[0], y0, flow::NODE_W, (y1 - y0).max(0.0)], Action::Node(n.node as u16))
                })
                .collect(),
            Screen::Gallery => {
                let scroll = self.gal_scroll.pos;
                let mut v: Vec<([f32; 4], Action)> = self
                    .cg_list()
                    .into_iter()
                    .enumerate()
                    // every entry stays (cut to the view, empty when outside it) so the cursor can move onto a
                    // row that is still scrolling in
                    .map(|(k, g)| {
                        let rc = Self::cg_cell(k, scroll);
                        let y0 = rc[1].clamp(GAL_CLIP[0], GAL_CLIP[1]);
                        let y1 = (rc[1] + rc[3]).clamp(y0, GAL_CLIP[1]);
                        ([rc[0], y0, rc[2], y1 - y0], Action::Cg(g))
                    })
                    .collect();
                v.extend(Self::hub_items());
                v
            }
            Screen::CgView { .. } => vec![([0.0, 0.0, W, H], Action::NextCg)],
            Screen::Intro => Vec::new(),
            Screen::Music => {
                // track titles (hit 860x44 at x 438, y 131 + 59i, scrolled and cut to the list view), then the
                // player panel controls (objects 385..390)
                let n = crate::gallery_table::MUSIC.len();
                let scroll = self.gal_scroll.pos.trunc();
                let mut v: Vec<([f32; 4], Action)> = (0..n)
                    .map(|i| {
                        let y = 131.0 + i as f32 * 59.0 - scroll;
                        let y0 = y.clamp(GAL_CLIP[0], GAL_CLIP[1]);
                        let y1 = (y + 44.0).clamp(y0, GAL_CLIP[1]);
                        ([438.0, y0, 860.0, y1 - y0], Action::Track(i))
                    })
                    .collect();
                v.extend(MUSIC_CTL.iter().enumerate().map(|(k, &(x, y))| ([x, y, 88.0, 80.0], Action::MusicCtl(k as u8))));
                v.push(([1376.0, 768.0, 448.0, 72.0], Action::MusicVol));
                v.extend(Self::hub_items());
                v
            }
            Screen::Movies => {
                // sub_17DD0: unlocked movies only, objects 391+ at (384 + 240c, 104 + 160r), 248x144
                let mut v: Vec<([f32; 4], Action)> = self
                    .movie_list(game)
                    .into_iter()
                    .enumerate()
                    .map(|(k, m)| ([384.0 + (k % 6) as f32 * 240.0, 104.0 + (k / 6) as f32 * 160.0, 248.0, 144.0], Action::Movie(m)))
                    .collect();
                v.extend(Self::hub_items());
                v
            }
            Screen::MoviePlay => vec![([0.0, 0.0, W, H], Action::Back)],
            Screen::Endings => {
                // cGalleryEnding (sub_DAF8 / sub_DDA4): reached endings get a play button, ending_btn
                // (object 394+i, 112x112 at 720 + 480i, 128) or ending_badendbtn (397+i, 88x88 on the plate)
                use crate::game::get_bit;
                let mut v: Vec<([f32; 4], Action)> = (0..24)
                    .filter(|&i| get_bit(&game.nodes, crate::gallery::ENDINGS[i].0))
                    .map(|i| {
                        let rc = if i < 3 {
                            [720.0 + i as f32 * 480.0, 128.0, 112.0, 112.0]
                        } else {
                            let b = i - 3;
                            [(b & 7) as f32 * 180.0 + 384.0, (b >> 3) as f32 * 96.0 + 680.0, 88.0, 88.0]
                        };
                        (rc, Action::Ending(i))
                    })
                    .collect();
                if Self::all_endings(game) {
                    v.push(([1702.0, 894.0, 112.0, 112.0], Action::Ending(24)));
                }
                v.extend(Self::hub_items());
                v
            }
            // sub_4AE90: buttons 0 (Yes) at 330 and 1 (No) at 252, y 249.01 (640x448 units), 216x56
            Screen::Confirm(_) => vec![([990.0, 600.0, 216.0, 56.0], Action::Yes), ([756.0, 600.0, 216.0, 56.0], Action::No)],
            Screen::Backlog { .. } => vec![([0.0, 0.0, W, H], Action::Back)],
            Screen::Game => {
                let _ = game;
                Vec::new()
            }
        }
    }

    /// Title menu entries: (action, texture, normal-state source rect). The hover and pressed states
    /// are further columns of the texture (+312 / +624 px).
    /// CONTINUE resumes the newest record of 0..139, manual or auto, never the quick save (sub_4EC60 mode 2).
    fn continue_slot(&self) -> Option<usize> {
        self.file.continue_slot().filter(|&i| self.slots.get(i).is_some_and(Option::is_some))
    }

    fn title_entries(&self) -> Vec<(Action, &'static str, [f32; 4])> {
        let cell = |y: f32| [24.0, y, 296.0, 72.0];
        let mut v = Vec::new();
        if self.continue_slot().is_some() {
            v.push((Action::Continue, "titlemenu", cell(376.0)));
        }
        v.push((Action::NewGame, "titlemenu", cell(24.0)));
        v.push((Action::OpenLoad, "titlemenu", cell(112.0)));
        v.push((Action::OpenConfig, "titlemenu", cell(200.0)));
        v.push((Action::OpenExtra, "titlemenu", cell(288.0)));
        v
    }

    /// Slot rectangle on the canvas: x 272, y from unk_352AF4 (4 entries from 312.36, 5 from 298.67,
    /// step 23.23) in 640x448 units, drawn at native texel size.
    fn title_slot(i: usize, n: usize) -> [f32; 4] {
        let y0 = if n >= 5 { 298.66666 } else { 312.35553 };
        let (kx, ky) = (W / 640.0, H / 448.0);
        [272.0 * kx, (y0 + i as f32 * 23.229630) * ky, 296.0, 72.0]
    }

    /// Source rect of a title entry in the given state (0 normal, 1 hover, 2 pressed), from the
    /// object frame tables at 0x65C7B0.
    fn title_src(normal: [f32; 4], state: usize) -> [f32; 4] {
        let [x, y, w, h] = normal;
        [x + 312.0 * state as f32, y, w, h]
    }

    /// Open the flowchart (cFlowchart): lay out the chart from the progress bits and put the cursor
    /// on the playthrough's current node.
    fn open_flow(&mut self, game: &mut Game, gallery: bool) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let chart = flow::build(&game.flow_flags(), gallery);
        let last = game.route_last();
        self.flow_cur = chart.nodes.iter().position(|n| Some(n.node) == last).unwrap_or(0);
        self.flow_scroll = 0.0;
        self.flow_target = 0.0;
        self.flow_t = FLOW_SCROLL_MS;
        self.flow = Some(chart);
        // sub_F8FC: the first placement does not animate
        self.flow_follow(false);
        self.push(Screen::Flowchart { gallery });
        self.hover = self.flow.as_ref().filter(|c| !c.nodes.is_empty()).map(|_| self.flow_cur);
    }

    /// Draw the screen below the current one (for dialogs and the CG viewer), without its guide.
    fn draw_under(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some(&below) = self.stack.last() else { return };
        let (screen, hover, under) = (self.screen, self.hover, self.under);
        self.screen = below;
        self.hover = None;
        self.under = true;
        self.draw(game, r, out);
        self.screen = screen;
        self.hover = hover;
        self.under = under;
    }

    /// CG viewer L / R: previous / next unlocked picture of the group (wraps), back to the fit view.
    fn cg_step(&mut self, game: &mut Game, d: i32) {
        if let Screen::CgView { group, pos } = self.screen {
            let n = self.cg_unlocked.get(group).map_or(0, Vec::len).max(1) as i32;
            let next = (pos as i32 + d).rem_euclid(n) as usize;
            // sub_C56C: sound 0 when the picture changes
            if next != pos {
                game.play_sys_sound(0);
            }
            self.screen = Screen::CgView { group, pos: next };
            self.cg_zoom = 0.0;
            self.cg_pan = [0.0; 2];
        }
    }

    /// CG viewer geometry (sub_BE84 / sub_D270): picture size, scale and on-screen rect. The scale runs from
    /// the fit-to-screen value up to 1:1 (or the fit value for pictures smaller than the screen); the pan
    /// keeps the picture covering the screen along any axis where it is larger.
    fn cg_rect(&mut self, w: f32, h: f32) -> [f32; 4] {
        self.cg_size = [w, h];
        let fit = (W / w).min(H / h);
        let max = fit.max(1.0);
        let s = if self.cg_zoom <= 0.0 { fit } else { self.cg_zoom.clamp(fit, max) };
        self.cg_zoom = s;
        let (dw, dh) = (w * s, h * s);
        let (mx, my) = (((dw - W) / 2.0).max(0.0), ((dh - H) / 2.0).max(0.0));
        self.cg_pan = [self.cg_pan[0].clamp(-mx, mx), self.cg_pan[1].clamp(-my, my)];
        [(W - dw) / 2.0 + self.cg_pan[0], (H - dh) / 2.0 + self.cg_pan[1], dw, dh]
    }

    /// Keep the cursor node inside the chart view (sub_10D5C).
    /// sub_10D5C: when the cursor node is above y 2 the row before it goes to y 4; when it is below y 966 the
    /// row after it goes to y 966 (positions are node tops in the view).
    fn flow_follow(&mut self, animate: bool) {
        let Some(c) = &self.flow else { return };
        let Some(n) = c.nodes.get(self.flow_cur) else { return };
        let y = n.pos[1] - self.flow_target;
        let target = if y < 2.0 {
            c.row_y[n.row.max(1) - 1] - 4.0
        } else if y > 966.0 {
            c.row_y[(n.row + 1).min(c.row_y.len() - 1)] - 966.0
        } else {
            return;
        };
        self.flow_scroll_to(target.trunc(), animate);
    }

    /// sub_27BD4: scroll the chart to `v` (clamped to its range), over 1/6 s or at once.
    fn flow_scroll_to(&mut self, v: f32, animate: bool) {
        let max = self.flow.as_ref().map_or(0.0, |c| c.scroll_max);
        self.flow_target = v.clamp(0.0, max);
        if animate {
            self.flow_from = self.flow_scroll;
            self.flow_t = 0.0;
        } else {
            self.flow_scroll = self.flow_target;
            self.flow_t = FLOW_SCROLL_MS;
        }
    }

    /// sub_12D1C: after a manual scroll the cursor moves row by row until its node is inside y 4..994.
    fn flow_keep_cursor(&mut self) {
        let Some(c) = &self.flow else { return };
        let Some(cur) = c.nodes.get(self.flow_cur) else { return };
        let y = cur.pos[1] - self.flow_target;
        let dy = if y < 4.0 {
            1
        } else if y >= 994.0 {
            -1
        } else {
            return;
        };
        let (x, mut row) = (cur.pos[0], cur.row as i64);
        let last = c.row_y.len() as i64 - 1;
        while (0..=last).contains(&(row + dy)) {
            row += dy;
            let near = (0..c.nodes.len())
                .filter(|&i| c.nodes[i].row as i64 == row)
                .min_by(|&a, &b| (c.nodes[a].pos[0] - x).abs().total_cmp(&(c.nodes[b].pos[0] - x).abs()));
            if let Some(i) = near {
                self.flow_cur = i;
                let y = c.nodes[i].pos[1] - self.flow_target;
                if (4.0..994.0).contains(&y) {
                    break;
                }
            }
        }
        self.hover = Some(self.flow_cur);
    }

    /// Controls of the config screen with their top-left on the canvas (content scrolled).
    fn conf_layout(&self) -> Vec<(Ctl, [f32; 2])> {
        let u = |x: f32, y: f32| [x * 3.0, y * H / 448.0 - self.conf_scroll];
        let mut v = vec![
            (Ctl::Tab(0), [408.0, 96.0]),
            (Ctl::Tab(1), [656.0, 96.0]),
            (Ctl::Tab(2), [888.0, 96.0]),
            (Ctl::Default, [1536.0, 96.0]),
        ];
        match self.conf_tab {
            0 => {
                for (i, (pm, sl)) in [(100.8, 56.0), (175.052, 130.252), (246.4, 201.6), (306.548, 261.748)].into_iter().enumerate() {
                    v.push((Ctl::Minus(i), u(245.0, pm)));
                    v.push((Ctl::Slider(i), u(120.667, sl)));
                    v.push((Ctl::Plus(i), u(452.0, pm)));
                    if i < 2 {
                        v.push((Ctl::Toggle(i), u(475.0, pm - 6.637)));
                    }
                }
                let radios = [(0, 364.622, 384.948), (1, 427.674, 448.0), (2, 491.141, 511.467), (3, 557.096, 577.422)];
                for (k, yt, yf) in radios {
                    v.push((Ctl::Radio(k, true), u(243.667, yt)));
                    v.push((Ctl::Radio(k, false), u(243.667, yf)));
                    if k == 0 {
                        v.push((Ctl::Toggle(2), u(290.333, 360.059)));
                    }
                }
            }
            1 => {
                for (i, y) in [104.948, 132.326, 160.533].into_iter().enumerate() {
                    v.push((Ctl::Minus(i), u(245.333, y)));
                    v.push((Ctl::Slider(i), u(121.0, y - 44.8)));
                    v.push((Ctl::Plus(i), u(452.333, y)));
                    v.push((Ctl::Mute(i), u(476.667, y - 4.978)));
                    if i == 1 {
                        v.push((Ctl::Test(0), u(560.667, 128.593)));
                    }
                }
                for (k, yt, yf) in [(4, 236.444, 258.844), (5, 297.007, 319.407)] {
                    v.push((Ctl::Radio(k, true), u(242.667, yt)));
                    v.push((Ctl::Radio(k, false), u(242.667, yf)));
                }
                // character rows (sub_29FA0): 370.01 + 50.607 n over the shown rows
                for (n, &[k, ..]) in self.voice_rows.iter().enumerate() {
                    let y = 370.01 + 50.607 * n as f32;
                    v.push((Ctl::Minus(3 + k), u(245.0, y)));
                    v.push((Ctl::Slider(3 + k), u(120.67, y - 44.8)));
                    v.push((Ctl::Plus(3 + k), u(452.0, y)));
                    v.push((Ctl::Mute(3 + k), u(476.33, y - 4.978)));
                    v.push((Ctl::Test(1 + k), u(560.33, y - 3.733)));
                }
            }
            _ => {}
        }
        v
    }

    /// Hit rectangle of a control placed at `p`.
    fn conf_rect(c: Ctl, p: [f32; 2]) -> [f32; 4] {
        let [x, y] = p;
        match c {
            Ctl::Tab(_) => [x, y, 192.0, 80.0],
            Ctl::Default => [x, y, 288.0, 72.0],
            Ctl::Minus(_) | Ctl::Plus(_) => [x, y, 80.0, 40.0],
            Ctl::Slider(_) => [x + 448.0, y + 100.0, 548.0, 48.0],
            Ctl::Toggle(_) => [x + 2.0, y, 304.0, 72.0],
            Ctl::Radio(k, v) => [x + 10.0, y, RADIO_CELLS[k][usize::from(!v)].1 - 10.0, 68.0],
            Ctl::Mute(_) => [x, y, 136.0, 64.0],
            Ctl::Test(_) => [x, y, 104.0, 56.0],
        }
    }

    /// (value, maximum, step) of slider row i on the current tab.
    fn conf_slider(&self, game: &Game, i: usize) -> (f32, f32, f32) {
        let st = &game.settings;
        match (self.conf_tab, i) {
            (0, 0 | 1) => (st.text_ms[i], 250.0, 5.0),
            (0, 2) => (st.auto_page_ms, 10000.0, 250.0),
            (0, _) => (st.auto_click_ms, 10000.0, 250.0),
            (_, 0) => (game.sound.vol.bgm * 100.0, 100.0, 5.0),
            (_, 1) => (game.sound.vol.se * 100.0, 100.0, 5.0),
            (_, 2) => (st.sys_se * 100.0, 100.0, 5.0),
            (_, k) => (st.char_voice.get(k - 3).copied().unwrap_or(1.0) * 100.0, 100.0, 5.0),
        }
    }

    fn conf_set_slider(&self, game: &mut Game, i: usize, v: f32) {
        let (_, max, _) = self.conf_slider(game, i);
        let v = v.clamp(0.0, max).round();
        let st = &mut game.settings;
        match (self.conf_tab, i) {
            (0, 0 | 1) => st.text_ms[i] = v,
            (0, 2) => st.auto_page_ms = v,
            (0, _) => st.auto_click_ms = v,
            (_, 0) => game.sound.vol.bgm = v / 100.0,
            (_, 1) => game.sound.vol.se = v / 100.0,
            (_, 2) => st.sys_se = v / 100.0,
            (_, k) => {
                if let Some(c) = st.char_voice.get_mut(k - 3) {
                    *c = v / 100.0;
                }
            }
        }
        game.sound.apply_volumes();
    }

    /// Whether a check / radio / mute control is on.
    fn conf_on(&self, game: &Game, c: Ctl) -> bool {
        let st = &game.settings;
        match c {
            Ctl::Toggle(i) if i < 2 => st.page_at_once[i],
            Ctl::Toggle(_) => st.skip_confirm,
            Ctl::Radio(k, v) => {
                let cur = match k {
                    0 => st.auto_skip_read,
                    1 => st.skip_unread,
                    2 => st.text_fade_in,
                    3 => st.text_fade_out,
                    4 => st.voice_on,
                    _ => st.voice_stop_on_advance,
                };
                cur == v
            }
            Ctl::Mute(i) if i < 3 => st.mute >> i & 1 != 0,
            Ctl::Mute(i) => st.char_mute >> (i - 3) & 1 != 0,
            Ctl::Tab(i) => self.conf_tab == i,
            _ => false,
        }
    }

    /// Applies config control `code`: d = +-1 steps a slider (left / right keys), 0 clicks.
    fn conf_change(&mut self, game: &mut Game, code: usize, d: i32) {
        let Some(c) = Ctl::from_code(code) else { return };
        let st = &mut game.settings;
        match c {
            Ctl::Tab(i) => {
                self.conf_bar = i;
                self.conf_switch(game, i);
            }
            Ctl::Default => {
                *st = crate::game::Settings::default();
                let v = crate::sound::Volumes::default();
                game.sound.vol.bgm = v.bgm;
                game.sound.vol.se = v.se;
                game.sound.vol.bgm_mute = false;
                game.sound.vol.se_mute = false;
                game.sound.apply_volumes();
            }
            Ctl::Minus(i) | Ctl::Plus(i) | Ctl::Slider(i) => {
                let (v, _, step) = self.conf_slider(game, i);
                let nv = match c {
                    Ctl::Minus(_) => v - step,
                    Ctl::Plus(_) => v + step,
                    _ if d != 0 => v + step * d as f32,
                    _ => {
                        // click on the track: the knob runs 169.33 units (508 px) from the track start
                        let Some(&(_, p)) = self.conf_layout().iter().find(|(k, _)| *k == c) else { return };
                        let (_, max, _) = self.conf_slider(game, i);
                        (self.mouse.0 - (p[0] + 468.0)) / 508.0 * max
                    }
                };
                self.conf_set_slider(game, i, nv);
            }
            Ctl::Toggle(i) if i < 2 => st.page_at_once[i] = !st.page_at_once[i],
            Ctl::Toggle(_) => st.skip_confirm = !st.skip_confirm,
            Ctl::Radio(k, v) => match k {
                0 => st.auto_skip_read = v,
                1 => st.skip_unread = v,
                2 => st.text_fade_in = v,
                3 => st.text_fade_out = v,
                4 => st.voice_on = v,
                _ => st.voice_stop_on_advance = v,
            },
            Ctl::Mute(i) if i < 3 => {
                st.mute ^= 1 << i;
                game.sound.vol.bgm_mute = st.mute & 1 != 0;
                game.sound.vol.se_mute = st.mute & 2 != 0;
                game.sound.apply_volumes();
            }
            Ctl::Mute(i) => st.char_mute ^= 1 << (i - 3),
            Ctl::Test(0) => {}
            // sub_2B930: a voice row's test button replays the row's last voice (G031 + 2 row) at the row's volume,
            // unless the row is muted or has none yet
            Ctl::Test(t) => {
                let k = t - 1;
                let g = |n: usize| game.var(&format!("G{n:03}"));
                let lo = g(31 + 2 * k);
                let id = (lo & 0xFFFF) | g(32 + 2 * k) << 16;
                if g(crate::game::VOICE_ROW_WORDS[k]) != 0 && lo >= 1 && id >= 0 && game.settings.char_mute >> k & 1 == 0
                    && let Some(name) = crate::voice_ids::name(id)
                {
                    game.play_test_voice(name, k);
                }
            }
        }
    }

    /// Draws a number right-aligned with its last digit at `x` (sub_2D420: digits 24 px apart,
    /// config_number_ja cells 32x32).
    fn conf_number(&self, out: &mut Vec<Quad>, n: u32, x: f32, y: f32) {
        let mut n = n;
        let mut x = x;
        loop {
            let d = (n % 10) as f32;
            self.sprite(out, "config_number_ja", [x, y, 32.0, 32.0], Some([d * 32.0, 0.0, 32.0, 32.0]));
            n /= 10;
            x -= 24.0;
            if n == 0 {
                break;
            }
        }
    }

    /// sub_2D570: the unread / read message speed samples (SYSMES 138, 139) type out at the configured speed;
    /// once ten characters are fully shown they stay for the auto mode delay, then start over.
    fn conf_preview_update(&mut self, game: &Game, f: f32) {
        let hold = game.settings.auto_click_ms * 60.0 / 1000.0;
        for k in 0..2 {
            let ms = game.settings.text_ms[k];
            let [p, shown, held] = &mut self.conf_prev[k];
            // sub_3B2D0 / sub_3B330: characters per frame and the width of the fading edge
            let speed = if ms < 1.0 { 768.0 } else { 16.667 / ms };
            let edge = if speed >= 768.0 { 0.0 } else { speed * (ms * 0.1 / 35.0).min(0.5) * 60.0 };
            let full = if speed >= 768.0 {
                *p = 768.0;
                *shown = 768.0;
                true
            } else {
                *p = (*p + speed * f).min(768.0);
                *shown = shown.max(*p - edge);
                *shown >= 10.0
            };
            if full {
                *held += f;
                if *held > hold {
                    *p = 0.0;
                    *shown = 0.0;
                    *held = 0.0;
                }
            }
        }
    }

    /// Text slots 58 / 59 at x 252.33, rows 126.93 / 201.19 (640x448), message font at 0.74.
    fn draw_conf_preview(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let [vx, vy, vw, vh] = CONF_VIEW;
        let clip = [[vx, vy], [vx + vw, vy], [vx + vw, vy + vh], [vx, vy + vh]];
        // sub_2B460: slots 58 / 59 at 0.74 of the 60 px base glyph (44.4 px em; 49 in ab_glyph's scale)
        let size = 49.0;
        for (k, row) in [126.93, 201.19].into_iter().enumerate() {
            let text = self.sysmes.get(138 + k).cloned().unwrap_or_default();
            let [p, shown, _] = self.conf_prev[k];
            let y = row * H / 448.0 - self.conf_scroll;
            let font = game.font_mut();
            let mut x = 252.333 * 3.0;
            for (i, c) in text.chars().enumerate() {
                let i = i as f32;
                let a = if p - shown > 1e-3 { ((p - i) / (p - shown)).clamp(0.0, 1.0) } else if i < p { 1.0 } else { 0.0 };
                if a > 0.0
                    && let Some(mut q) = font.quad(r, c, size, x, y + size, [1.0, 1.0, 1.0, a])
                {
                    q.clip = Some(clip);
                    out.push(q);
                }
                x += font.advance(c, size);
            }
        }
    }

    /// The config screen (sub_2AA80 and the per tab sub_2D570 / sub_2D980).
    fn draw_config(&self, game: &Game, out: &mut Vec<Quad>, hovered: impl Fn(usize) -> bool) {
        self.sprite(out, "config_innerbg", [360.0, 0.0, 1560.0, 1080.0], None);
        self.sprite(out, "tab_bg", [400.0, 96.0, 1416.0, 72.0], None);
        let u = |x: f32, y: f32| [x * 3.0, y * H / 448.0 - self.conf_scroll];
        let n0 = out.len();
        match self.conf_tab {
            0 => {
                let [x, y] = u(128.667, 68.03);
                self.sprite(out, "system_bg", [x, y, 1432.0, 1336.0], None);
            }
            1 => {
                let [x, y] = u(128.667, 68.03);
                self.sprite(out, "sound_bg", [x, y, 1432.0, 328.0], None);
                let [x, y] = u(128.667, 199.94);
                self.sprite(out, "voice_bg", [x, y, 1432.0, 386.0], Some([0.0, 0.0, 1432.0, 386.0]));
                // sub_2D980: the shown rows with their face / name cells
                for (i, &[_, face, name]) in self.voice_rows.clone().iter().enumerate() {
                    let n = i as f32 * 50.607;
                    let [x, y] = u(128.667, 360.06 + n);
                    self.sprite(out, "voice_bg", [x, y, 1432.0, 122.0], Some([0.0, 386.0, 1432.0, 122.0]));
                    let [fx, fy] = u(144.0, 365.04 + n);
                    let f = FACE_RECTS[face];
                    self.sprite(out, "conf_faceicon", [fx, fy, f[2], f[3]], Some(f));
                    let [nx, ny] = u(144.0, 394.9 + n);
                    let nm = NAME_RECTS[name];
                    self.sprite(out, "conf_name", [nx, ny, nm[2], nm[3]], Some(nm));
                }
                let [x, y] = u(128.667, 360.06 + self.voice_rows.len() as f32 * 50.607);
                self.sprite(out, "voice_bg", [x, y, 1432.0, 19.0], Some([0.0, 508.0, 1432.0, 19.0]));
            }
            _ => {
                let [x, y] = u(133.333, 73.007);
                self.sprite(out, "conf_manual0_ja", [x, y, 1416.0, 720.0], None);
            }
        }
        // values
        let num_y: &[f32] = if self.conf_tab == 0 { &[131.5, 205.75, 271.29, 330.61] } else { &[107.44, 134.81, 163.02] };
        let num_x = if self.conf_tab == 0 { 450.0 } else { 538.67 };
        if self.conf_tab < 2 {
            for (i, &y) in num_y.iter().enumerate() {
                let [x, y] = u(num_x, y);
                self.conf_number(out, self.conf_slider(game, i).0.round() as u32, x, y);
            }
        }
        if self.conf_tab == 1 {
            for (n, &[k, ..]) in self.voice_rows.clone().iter().enumerate() {
                let [x, y] = u(538.33, 372.5 + 50.607 * n as f32);
                self.conf_number(out, self.conf_slider(game, 3 + k).0.round() as u32, x, y);
            }
        }
        // controls; frames: normal / hover / selected columns of each texture
        let layout = self.conf_layout();
        let mut top = Vec::new();
        for (i, &(c, [x, y])) in layout.iter().enumerate() {
            let col = if self.conf_on(game, c) {
                2.0
            } else if hovered(i) {
                1.0
            } else {
                0.0
            };
            let q = if matches!(c, Ctl::Tab(_) | Ctl::Default) { &mut top } else { &mut *out };
            let q0 = q.len();
            match c {
                Ctl::Tab(t) => {
                    let src = [8.0 + col * 200.0, 8.0 + t as f32 * 88.0, 192.0, 80.0];
                    self.sprite(q, "btn_base0", [x, y, 192.0, 80.0], Some(src));
                }
                Ctl::Default => {
                    let col = if hovered(i) { 1.0 } else { 0.0 };
                    self.sprite(q, "btn_default", [x, y, 288.0, 72.0], Some([32.0, 24.0 + col * 96.0, 288.0, 72.0]));
                }
                Ctl::Minus(_) | Ctl::Plus(_) => {
                    let sx = if matches!(c, Ctl::Plus(_)) { 88.0 } else { 0.0 };
                    let sy = [0.0, 49.0, 97.0][usize::from(hovered(i))];
                    self.sprite(q, "conf_stxt01", [x, y, 80.0, 40.0], Some([sx, sy, 80.0, 40.0]));
                }
                Ctl::Slider(s) => {
                    let (v, max, _) = self.conf_slider(game, s);
                    let kx = x + v / max * 508.0;
                    let src = if hovered(i) { [0.0, 0.0, 936.0, 248.0] } else { [0.0, 256.0, 936.0, 248.0] };
                    self.sprite(q, "conf_sbtn", [kx, y, 936.0, 248.0], Some(src));
                }
                Ctl::Toggle(t) => {
                    let row = if t < 2 { 0.0 } else { 80.0 };
                    self.sprite(q, "btn_base2", [x + 2.0, y, 304.0, 72.0], Some([col * 304.0, row, 304.0, 72.0]));
                }
                Ctl::Radio(k, v) => {
                    let (cy, w) = RADIO_CELLS[k][usize::from(!v)];
                    self.sprite(q, "btn_base1", [x + 10.0, y, w - 10.0, 68.0], Some([col * 256.0, cy, w - 10.0, 68.0]));
                }
                Ctl::Mute(_) => {
                    self.sprite(q, "btn_mute", [x, y, 136.0, 64.0], Some([32.0 + col * 152.0, 32.0, 136.0, 64.0]));
                }
                Ctl::Test(_) => {
                    let col = if hovered(i) { 1.0 } else { 0.0 };
                    self.sprite(q, "btn_test", [x, y, 104.0, 56.0], Some([32.0 + col * 120.0, 32.0, 104.0, 56.0]));
                }
            }
            // sub_2B010 flags every config object 0x880: the focused one gets an additive copy pulsing 0..1 at
            // 0.02 / frame (sub_AEEC0, dword_1915114). The focus is the cursor, or the open tab's button
            // before anything else was picked.
            let focused = hovered(i) || (self.hover.is_none() && !self.side && c == Ctl::Tab(self.conf_tab));
            if focused {
                let pulse = 1.0 - ((self.title_clock * 1.2) % 2.0 - 1.0).abs();
                for k in q0..q.len() {
                    let mut d = q[k].clone();
                    d.color[3] *= pulse;
                    d.blend = Blend::Add;
                    q.push(d);
                }
            }
        }
        let [vx, vy, vw, vh] = CONF_VIEW;
        let clip = [[vx, vy], [vx + vw, vy], [vx + vw, vy + vh], [vx, vy + vh]];
        for q in &mut out[n0..] {
            q.clip = Some(clip);
            q.color[3] *= self.conf_ca;
        }
        out.extend(top);
    }

    // ---- input -------------------------------------------------------------------

    /// sub_ACEF0(n) for the input being handled.
    fn snd(&mut self, game: &mut Game, n: usize) {
        game.play_sys_sound(n);
        self.sounded = true;
    }

    /// Everything a cursor move changes (the CG viewer's picture plays its own sound in `cg_step`).
    fn cursor(&self) -> Cursor {
        let screen = match self.screen {
            Screen::CgView { group, .. } => Screen::CgView { group, pos: 0 },
            s => s,
        };
        Cursor {
            screen,
            rest: (self.sys_tab, self.side, self.gal_side, self.gal_tab, self.flow_cur, self.music_row, self.music_btn, self.conf_bar),
            hover: self.hover,
        }
    }

    /// Every menu plays system sound 0 when its cursor moves (sub_4CEA0, sub_50FC0, sub_29770, sub_6E28,
    /// sub_9CBC, sub_1A7FC, sub_28A90, sub_12424, ...): not when the input opened or closed a screen or
    /// played a sound of its own, nor when a pointer leaves the entries.
    fn cursor_sound(&mut self, game: &mut Game, before: Cursor) {
        let after = self.cursor();
        let same_screen = std::mem::discriminant(&before.screen) == std::mem::discriminant(&after.screen);
        let moved = (before.screen, before.rest) != (after.screen, after.rest) || (before.hover != after.hover && after.hover.is_some());
        if !std::mem::take(&mut self.sounded) && same_screen && moved {
            game.play_sys_sound(0);
        }
    }

    pub fn mouse_move(&mut self, game: &mut Game, x: f32, y: f32) {
        let before = self.cursor();
        self.sounded = false;
        self.mouse_move_inner(game, x, y);
        if self.screen != Screen::Game {
            self.cursor_sound(game, before);
        }
    }

    fn mouse_move_inner(&mut self, game: &mut Game, x: f32, y: f32) {
        if (x, y) != self.mouse {
            self.mouse = (x, y);
            self.touch_move();
        }
        if self.screen == Screen::Game {
            game.mouse_move(x, y);
            return;
        }
        // the system menu has no hover (touch only on Switch): its cursor moves with the keys or a tap
        if self.screen == Screen::Menu {
            return;
        }
        let mut hover = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)));
        if self.screen == Screen::Title && !self.press {
            // sub_8E200 state 17: off the entries the cursor stays; moving onto another one plays sound 0
            hover = hover.or(self.hover);
            if self.busy() {
                self.sounded = true;
            }
        }
        if hover == self.hover && (self.key_focus || self.side) {
            return;
        }
        self.hover = hover;
        self.key_focus = false;
        self.side = false;
        if self.screen == Screen::Config
            && let Some(h) = self.hover.filter(|&h| h < 4)
        {
            self.conf_bar = h;
        }
        if Self::gal_mode(self.screen).is_some()
            && let Some((_, a)) = self.hover.and_then(|h| self.items(game).get(h).copied())
        {
            match Self::hub_items().iter().position(|(_, b)| *b == a) {
                Some(i) => {
                    self.gal_side = true;
                    self.gal_tab = i;
                }
                None => {
                    self.gal_side = false;
                    self.gal_mem = Some(a);
                    match a {
                        Action::MusicCtl(k) => self.music_btn = usize::from(k),
                        Action::Track(i) => self.music_row = i,
                        _ => {}
                    }
                }
            }
        }
        if matches!(self.screen, Screen::Flowchart { .. })
            && let Some(h) = self.hover
        {
            self.flow_cur = h;
        }
    }

    pub fn click(&mut self, game: &mut Game, r: &Renderer) {
        self.skip_banner();
        if self.popup_input() || self.title_after_banner {
            return;
        }
        if self.screen == Screen::Game {
            game.advance();
            return;
        }
        if self.busy() {
            return;
        }
        if self.screen == Screen::Intro || (self.screen == Screen::Title && self.press) {
            self.intro_input(game);
            return;
        }
        if self.screen == (Screen::SaveLoad { save: false, scroll: 0 }) && self.save_list(false).is_empty() && self.mouse.0 >= 368.0 {
            game.play_sys_sound(2);
            self.popup(game, 193, 480.0);
            return;
        }
        // the system menu tabs stay tappable under its save / load / config pages (sub_4D580 runs sub_4CEA0):
        // a tap opens that tab (the open one too, which restarts it)
        if matches!(self.screen, Screen::SaveLoad { .. } | Screen::Config)
            && self.stack.last() == Some(&Screen::Menu)
            && let Some(i) = (0..5).find(|&i| rect_hit([80.0, 105.0 + i as f32 * 75.0, 224.0, 88.0], self.mouse))
        {
            self.help_t = 0.0;
            self.sys_tab = i;
            self.pop();
            let a = [Action::OpenSave, Action::OpenLoad, Action::OpenConfig, Action::OpenFlow, Action::ToTitle][i];
            self.act(game, r, a);
            return;
        }
        let items = self.items(game);
        if self.screen == Screen::Menu {
            // a tap (sub_4CEA0): the balloon restarts, a tab under it takes the cursor and opens; taps
            // elsewhere do nothing (only B closes the menu)
            self.help_t = 0.0;
            if let Some(i) = items.iter().position(|(rc, _)| rect_hit(*rc, self.mouse)) {
                self.sys_tab = i;
                self.hover = Some(i);
                self.act(game, r, items[i].1);
            }
            return;
        }
        if let Some((_, a)) = items.iter().find(|(rc, _)| rect_hit(*rc, self.mouse)) {
            self.act(game, r, *a);
        }
    }

    /// Right click / escape.
    pub fn cancel(&mut self, game: &mut Game, r: &Renderer) {
        if self.popup_input() {
            return;
        }
        match self.screen {
            // a story movie takes only B / Plus (sub_40480)
            Screen::Game if game.in_movie() => game.skip_movie(),
            Screen::Game => {
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                // sub_40ED0: the system menu opens with sound 3
                game.play_sys_sound(3);
                self.push(Screen::Menu);
                self.menu_bg.fade(0.0555);
            }
            Screen::Title | Screen::Intro => self.intro_input(game),
            // B in a gallery mode's content gives the focus back to the mode buttons; on the buttons it leaves
            s if Self::gal_mode(s).is_some() && !self.gal_side && !self.busy() => {
                game.play_sys_sound(2);
                self.gal_to_side(game);
            }
            _ => self.act(game, r, Action::Back),
        }
    }

    pub fn key_nav(&mut self, game: &mut Game, r: &Renderer, dy: i32, dx: i32, enter: bool) {
        let before = self.cursor();
        self.sounded = false;
        self.key_nav_inner(game, r, dy, dx, enter);
        self.cursor_sound(game, before);
    }

    fn key_nav_inner(&mut self, game: &mut Game, r: &Renderer, dy: i32, dx: i32, enter: bool) {
        if dx != 0 || dy != 0 {
            self.key_focus = true;
        }
        if enter {
            self.skip_banner();
        }
        if self.popup.is_some() {
            if enter {
                self.popup_input();
            }
            return;
        }
        if self.title_after_banner {
            return;
        }
        if self.screen == Screen::Game {
            if dy != 0 {
                game.choice_move(dy);
            }
            if enter {
                game.advance();
            }
            return;
        }
        if self.busy() {
            return;
        }
        if self.screen == Screen::Intro || (self.screen == Screen::Title && self.press) {
            self.intro_input(game);
            return;
        }
        if self.side {
            self.side_nav(game, r, dy, dx, enter);
            return;
        }
        if self.screen == Screen::Menu {
            // sub_4CEA0: up / down over the five tabs, wrapping; A opens the tab under the cursor
            if dy != 0 {
                self.sys_tab = (self.sys_tab as i32 + dy).rem_euclid(5) as usize;
            }
            self.hover = Some(self.sys_tab);
            if enter {
                let a = self.items(game)[self.sys_tab].1;
                self.act(game, r, a);
            }
            return;
        }
        if let Screen::SaveLoad { save, scroll } = self.screen {
            // left: to the system menu tabs (right comes back to the save that was highlighted)
            if dx < 0 && self.stack.last() == Some(&Screen::Menu) {
                self.enter_side();
                return;
            }
            // sub_50FC0: A on an empty load list -> "セーブデータがありません"
            if enter && !save && self.save_list(false).is_empty() {
                self.snd(game, 2);
                self.popup(game, 193, 480.0);
                return;
            }
            // a list of the saves: the cursor moves one entry at a time and scrolls the four visible frames
            if dy != 0 {
                let n = self.save_list(save).len();
                if n > 0 {
                    let cur = (scroll + self.hover.filter(|&h| h < SLOTS_PER_PAGE).unwrap_or(0)) as i32;
                    let next = (cur + dy).clamp(0, n as i32 - 1) as usize;
                    let scroll = if next < scroll { next } else if next >= scroll + SLOTS_PER_PAGE { next + 1 - SLOTS_PER_PAGE } else { scroll };
                    self.screen = Screen::SaveLoad { save, scroll };
                    self.hover = Some(next - scroll);
                }
                return;
            }
            if dx != 0 {
                return;
            }
        }
        if let Screen::Backlog { scroll } = &mut self.screen {
            if dy < 0 {
                *scroll = (*scroll + 1).min(game.backlog_len().saturating_sub(1));
            } else if dy > 0 {
                if *scroll == 0 {
                    // leaving the backlog: sound 2 (sub_28A90)
                    self.snd(game, 2);
                    self.pop();
                } else {
                    *scroll -= 1;
                }
            }
            return;
        }
        if matches!(self.screen, Screen::Flowchart { .. }) && (dx != 0 || dy != 0) {
            self.flow_move(dx, dy);
            return;
        }
        if self.screen == Screen::Config && (dx != 0 || dy != 0) {
            self.conf_nav(game, dx, dy);
            return;
        }
        if matches!(self.screen, Screen::CgView { .. }) && (dx != 0 || dy != 0) {
            // L / R: switch pictures; up / down (L stick): pan a zoomed picture
            if dx != 0 {
                self.cg_step(game, dx);
            } else {
                self.cg_pan[1] += 60.0 * dy as f32;
            }
            return;
        }
        if matches!(self.screen, Screen::Confirm(_)) && dx != 0 {
            // No is on the left, Yes on the right
            self.hover = Some(usize::from(dx < 0));
            return;
        }
        if Self::gal_mode(self.screen).is_some() && self.gal_nav(game, r, dx, dy, enter) {
            return;
        }
        let n = self.items(game).len();
        if n == 0 {
            return;
        }
        if dy != 0 {
            let cur = self.hover.map_or(if dy > 0 { -1 } else { 0 }, |h| h as i32);
            self.hover = Some((cur + dy).rem_euclid(n as i32) as usize);
        }
        if enter && let Some(h) = self.hover {
            // A on a slider knob only grabs it (left / right move it while held)
            if self.screen == Screen::Config && matches!(self.conf_layout().get(h), Some((Ctl::Slider(_), _))) {
                return;
            }
            let a = self.items(game)[h].1;
            self.act(game, r, a);
        }
    }

    /// The system menu tab a submenu belongs to.
    fn sub_tab(&self) -> Option<usize> {
        match self.screen {
            Screen::SaveLoad { save, .. } => Some(usize::from(!save)),
            Screen::Config => Some(2),
            Screen::Flowchart { gallery: false } => Some(3),
            _ => None,
        }
    }

    /// Hand the cursor to the system menu tabs; the submenu stays open (config state 7 / sub_4D580).
    fn enter_side(&mut self) {
        self.side = true;
        self.sub_hover = self.hover;
        if self.screen == Screen::Config {
            self.hover = None;
        }
    }

    fn leave_side(&mut self) {
        self.side = false;
        self.hover = self.sub_hover;
    }

    /// sub_4CEA0 while a submenu is open: up / down move the tab cursor, A on another tab opens it, A on
    /// the open tab or right returns to the page, B closes the submenu.
    fn side_nav(&mut self, game: &mut Game, r: &Renderer, dy: i32, dx: i32, enter: bool) {
        if dy != 0 {
            self.sys_tab = (self.sys_tab as i32 + dy).rem_euclid(5) as usize;
        } else if dx > 0 {
            self.leave_side();
        } else if enter {
            if Some(self.sys_tab) == self.sub_tab() {
                self.leave_side();
                return;
            }
            self.pop();
            let a = [Action::OpenSave, Action::OpenLoad, Action::OpenConfig, Action::OpenFlow, Action::ToTitle][self.sys_tab];
            self.act(game, r, a);
        }
    }

    /// Config rows of the current tab as indices into `conf_layout`, left to right (the navigation grid
    /// at dword_46CDB0: 12 rows on the system tab, 26 on the sound tab).
    fn conf_rows(&self, layout: &[(Ctl, [f32; 2])]) -> Vec<Vec<usize>> {
        let at = |c: Ctl| layout.iter().position(|(k, _)| *k == c);
        let row = |cs: &[Ctl]| cs.iter().filter_map(|&c| at(c)).collect::<Vec<_>>();
        let mut rows = Vec::new();
        match self.conf_tab {
            0 => {
                for i in 0..4 {
                    // page-at-once checks on the first two rows only (Toggle(2) is the skip confirmation)
                    let n = if i < 2 { 4 } else { 3 };
                    rows.push(row(&[Ctl::Minus(i), Ctl::Slider(i), Ctl::Plus(i), Ctl::Toggle(i)][..n]));
                }
                rows.push(row(&[Ctl::Radio(0, true), Ctl::Toggle(2)]));
                rows.push(row(&[Ctl::Radio(0, false)]));
                for k in 1..4 {
                    rows.push(row(&[Ctl::Radio(k, true)]));
                    rows.push(row(&[Ctl::Radio(k, false)]));
                }
            }
            1 => {
                for i in 0..3 {
                    rows.push(row(&[Ctl::Minus(i), Ctl::Slider(i), Ctl::Plus(i), Ctl::Mute(i), Ctl::Test(0)][..if i == 1 { 5 } else { 4 }]));
                }
                for k in 4..6 {
                    rows.push(row(&[Ctl::Radio(k, true)]));
                    rows.push(row(&[Ctl::Radio(k, false)]));
                }
                for &[k, ..] in &self.voice_rows {
                    rows.push(row(&[Ctl::Minus(3 + k), Ctl::Slider(3 + k), Ctl::Plus(3 + k), Ctl::Mute(3 + k), Ctl::Test(1 + k)]));
                }
            }
            _ => {}
        }
        rows
    }

    /// sub_29BF0: the tab bar (tabs, default button) sits above the rows; up / down keep the column index
    /// (clamped to the row), left / right walk a row; left off the first column goes to the system menu
    /// tabs in game. A held A with left / right moves a slider knob instead (sub_2B930).
    fn conf_nav(&mut self, game: &mut Game, dx: i32, dy: i32) {
        let layout = self.conf_layout();
        let rows = self.conf_rows(&layout);
        let pos = self.hover.and_then(|h| rows.iter().enumerate().find_map(|(r, row)| row.iter().position(|&x| x == h).map(|c| (r, c))));
        let in_menu = self.stack.last() == Some(&Screen::Menu);
        match pos {
            None => {
                let bar = self.hover.filter(|&h| h < 4).unwrap_or(self.conf_bar);
                if dx < 0 {
                    if bar > 0 {
                        self.hover = Some(bar - 1);
                    } else if in_menu {
                        self.conf_bar = 0;
                        self.hover = Some(0);
                        self.enter_side();
                        return;
                    }
                } else if dx > 0 {
                    self.hover = Some((bar + 1).min(3));
                } else if dy > 0 && !rows.is_empty() {
                    self.hover = Some(rows[0][0]);
                } else {
                    self.hover = Some(bar);
                }
                if let Some(h) = self.hover.filter(|&h| h < 4) {
                    self.conf_bar = h;
                }
            }
            Some((r, c)) => {
                if dx != 0 && self.enter_held && matches!(layout[rows[r][c]].0, Ctl::Slider(_)) {
                    self.conf_change(game, layout[rows[r][c]].0.code(), dx);
                    self.store_global(game);
                    return;
                }
                if dy < 0 {
                    self.hover = Some(if r == 0 { self.conf_bar } else { rows[r - 1][c.min(rows[r - 1].len() - 1)] });
                } else if dy > 0 && r + 1 < rows.len() {
                    self.hover = Some(rows[r + 1][c.min(rows[r + 1].len() - 1)]);
                } else if dx < 0 {
                    if c > 0 {
                        self.hover = Some(rows[r][c - 1]);
                    } else if in_menu {
                        self.enter_side();
                        return;
                    }
                } else if dx > 0 && c + 1 < rows[r].len() {
                    self.hover = Some(rows[r][c + 1]);
                }
            }
        }
        // sub_29930: keep the selected control in view. With y its unscrolled position (1080 units), the
        // scroll goes to y - 260 when that is above it, else to y - 870 when that is below it; voice volume
        // knobs (objects 99 + 5k, placed 44.8 units above their row) use y - 152 / y - 772.
        if let Some((c, p)) = self.hover.and_then(|h| layout.get(h).copied())
            && !matches!(c, Ctl::Tab(_) | Ctl::Default)
        {
            let y = p[1] + self.conf_scroll;
            let knob = self.conf_tab == 1 && matches!(c, Ctl::Slider(i) if i >= 3);
            let (top, bottom) = if knob { (y - 152.0, y - 772.0) } else { (y - 260.0, y - 870.0) };
            let cur = self.conf_target[self.conf_tab];
            if top < cur {
                self.conf_scroll_to(top);
            } else if bottom > cur {
                self.conf_scroll_to(bottom);
            }
        }
    }

    /// sub_9AE58: tween the config scroll to `v` (clamped to the tab's range) from where it is drawn now.
    fn conf_scroll_to(&mut self, v: f32) {
        let v = v.clamp(0.0, self.conf_max()).trunc();
        if v != self.conf_target[self.conf_tab] {
            self.conf_target[self.conf_tab] = v;
            self.conf_from = self.conf_scroll;
            self.conf_t = 0.0;
        }
    }

    /// Scroll range of the open tab: the sound tab's is set when it is built (sub_2A550: 122 per shown voice
    /// row, less the 113 that fit; 2205 with all 19 rows).
    fn conf_max(&self) -> f32 {
        self.conf_max_of(self.conf_tab)
    }

    /// sub_29FA0: the voice rows the sound tab shows. A row shows once its character has spoken (bit 4 of its
    /// progress word, set by VPLY); rows 15 / 16 keep the unknown face (18) until bit 1, the rows in 0x3CF36 the
    /// unknown name (19) until bit 2. Row 0 (Shiki) depends on where the game is: in a game, hidden while F023 is
    /// 0, unknown face and name in scenes 2 / 3, face 19 in scene 1; from the title, unknown until flowchart
    /// node 485 is read, then face 19 until node 1 is.
    fn refresh_voice_rows(&mut self, game: &Game) {
        let in_game = !self.stack.contains(&Screen::Title);
        let node = |n: usize| crate::game::get_bit(&game.nodes, n);
        self.voice_rows = (0..VOICE_ROWS)
            .filter_map(|r| {
                let w = game.var(&format!("G{:03}", crate::game::VOICE_ROW_WORDS[r]));
                if w & 4 == 0 {
                    return None;
                }
                let (face, name) = if r == 0 {
                    if in_game {
                        if game.var("F023") == 0 {
                            return None;
                        }
                        match game.scene_index {
                            2 | 3 => (18, 19),
                            1 => (19, 0),
                            _ => (0, 0),
                        }
                    } else if !node(485) {
                        (18, 19)
                    } else {
                        (if node(1) { 0 } else { 19 }, 0)
                    }
                } else {
                    let face = if (15..=16).contains(&r) && w & 1 == 0 { 18 } else { r };
                    let name = if 0x3CF36 >> r & 1 != 0 && w & 2 == 0 { 19 } else { r };
                    (face, name)
                };
                Some([r, face, name])
            })
            .collect();
    }

    /// sub_2C380 / sub_2B680: a tab shows at its own remembered scroll, without a tween.
    fn conf_show_tab(&mut self) {
        self.conf_scroll = self.conf_target[self.conf_tab];
        self.conf_t = CONF_SCROLL_MS;
    }

    /// Scroll range of tab `tab` (conf_max for the open one).
    fn conf_max_of(&self, tab: usize) -> f32 {
        match tab {
            0 => CONF_MAX_SYSTEM,
            1 => (122.0 * self.voice_rows.len() as f32).max(113.0) - 113.0,
            _ => 0.0,
        }
    }

    /// Tab switch (sub_29F00 -> sub_2C380 state 4): the content fades out at 0.05 / frame, the tab changes,
    /// then it fades back in (state 5); no input meanwhile.
    /// sub_29F00: switch tabs with system sound 1.
    fn conf_switch(&mut self, game: &mut Game, tab: usize) {
        if tab != self.conf_tab || self.conf_next.is_some() {
            self.conf_next = Some(tab);
            self.snd(game, 1);
        }
    }

    /// Per frame: the tab switch fade, and the scroll widget's alpha (sub_2AA80). On a tab that does not
    /// scroll it is min(screen · content, itself), so a switch's fade-out hides it for good; on a scrolling
    /// tab it follows screen · content while the tab being switched to does not scroll or it is not fully up,
    /// else the screen.
    fn conf_tick(&mut self, f: f32) {
        if let Some(t) = self.conf_next {
            self.conf_ca -= 0.05 * f;
            if self.conf_ca <= 0.0 {
                self.conf_ca = 0.0;
                self.conf_tab = t;
                self.conf_next = None;
                self.conf_show_tab();
            }
        } else {
            self.conf_ca = (self.conf_ca + 0.05 * f).min(1.0);
        }
        let (a, t) = (self.scr_a, self.conf_ca);
        let next = self.conf_next.unwrap_or(self.conf_tab);
        self.conf_sb_a = if self.conf_max_of(self.conf_tab) <= 0.0 {
            (a * t).min(self.conf_sb_a)
        } else if self.conf_max_of(next) <= 0.0 || self.conf_sb_a < 1.0 {
            a * t
        } else {
            a
        };
    }

    /// L / R buttons: the config switches tabs (sub_29F00, the cursor goes to the new tab's button);
    /// elsewhere they act as left / right.
    pub fn shoulder(&mut self, game: &mut Game, r: &Renderer, d: i32) {
        if self.screen == Screen::Config && !self.busy() && self.popup.is_none() {
            self.side = false;
            let tab = (self.conf_next.unwrap_or(self.conf_tab) as i32 + d).rem_euclid(3) as usize;
            self.conf_switch(game, tab);
            self.conf_bar = tab;
            self.hover = Some(tab);
            self.store_global(game);
        } else {
            self.key_nav(game, r, 0, d, false);
        }
    }

    /// Flowchart cursor: left/right steps through the nodes in order, up/down goes to the nearest
    /// node of the closest row above / below.
    fn flow_move(&mut self, dx: i32, dy: i32) {
        let Some(c) = &self.flow else { return };
        let n = c.nodes.len();
        if n == 0 {
            return;
        }
        let cur = self.flow_cur.min(n - 1);
        let next = if dy != 0 {
            let here = &c.nodes[cur];
            let row = |r: usize| if dy > 0 { r > here.row } else { r < here.row };
            let target = c.nodes.iter().filter(|m| row(m.row)).map(|m| m.row).reduce(|a, b| if (dy > 0) == (b < a) { b } else { a });
            target.and_then(|t| {
                (0..n).filter(|&i| c.nodes[i].row == t).min_by(|&a, &b| {
                    let d = |i: usize| (c.nodes[i].pos[0] - here.pos[0]).abs();
                    d(a).total_cmp(&d(b))
                })
            })
        } else {
            Some((cur as i32 + dx).clamp(0, n as i32 - 1) as usize)
        };
        if let Some(i) = next {
            self.flow_cur = i;
            self.flow_follow(true);
            self.hover = Some(i);
        }
    }

    /// Mouse wheel: up opens the backlog in game.
    pub fn wheel(&mut self, game: &mut Game, up: bool) {
        let before = self.cursor();
        self.sounded = false;
        self.wheel_inner(game, up);
        self.cursor_sound(game, before);
    }

    fn wheel_inner(&mut self, game: &mut Game, up: bool) {
        match &mut self.screen {
            Screen::Game => {
                if up && game.backlog_len() > 0 {
                    // sub_28950: the backlog opens with sound 3
                    self.snd(game, 3);
                    self.push(Screen::Backlog { scroll: 0 });
                } else if !up && !game.in_choice() {
                    game.advance();
                }
            }
            Screen::Config => {
                self.conf_scroll_to(self.conf_target[self.conf_tab] + if up { -120.0 } else { 120.0 });
                let (x, y) = self.mouse;
                self.hover = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)));
            }
            Screen::Flowchart { .. } => {
                // R stick (sub_11418): the scroll moves and the cursor follows into view (sub_12D1C)
                self.flow_scroll_to(self.flow_target + if up { -112.0 } else { 112.0 }, true);
                self.flow_keep_cursor();
            }
            Screen::CgView { .. } => {
                // R stick: zoom
                self.cg_zoom *= if up { 1.25 } else { 0.8 };
            }
            Screen::Gallery | Screen::Music => {
                // R stick (sub_9CBC / sub_1A7FC): the list scrolls by a row per step through the 1/6 s tween and
                // the cursor row is pulled back into view
                let row = if self.screen == Screen::Music { 59.0 } else { 139.0 };
                let max = self.gal_scroll_max();
                self.gal_scroll.to(self.gal_scroll.target + if up { -row } else { row }, max, true);
                if self.key_focus {
                    self.gal_keep_cursor(game);
                } else {
                    let (x, y) = self.mouse;
                    self.hover = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)));
                }
            }
            Screen::Backlog { scroll } => {
                if up {
                    *scroll = (*scroll + 1).min(game.backlog_len().saturating_sub(1));
                } else if *scroll == 0 {
                    self.snd(game, 2);
                    self.pop();
                } else {
                    *scroll -= 1;
                }
            }
            Screen::SaveLoad { save, scroll } => {
                let (save, cur) = (*save, *scroll);
                let max = self.save_list(save).len().saturating_sub(SLOTS_PER_PAGE);
                let scroll = if up { cur.saturating_sub(1) } else { (cur + 1).min(max) };
                self.screen = Screen::SaveLoad { save, scroll };
                let (x, y) = self.mouse;
                self.hover = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)));
            }
            _ => {}
        }
    }

    /// Input is held while a screen fades out, the screen goes black for a transition, or the gallery
    /// crossfades between modes.
    fn busy(&self) -> bool {
        self.closing
            || self.black_then.is_some()
            || self.gal_next.is_some()
            || self.cont_slot.is_some()
            || (self.screen == Screen::Config && (self.conf_next.is_some() || self.conf_ca < 1.0))
    }

    /// The save list (sub_50790): every used slot, newest first; when saving, the first free slot comes
    /// first as an empty frame (the quick / auto save slot is never offered).
    fn save_list(&self, save: bool) -> Vec<usize> {
        let mut used: Vec<(u64, usize)> =
            self.slots.iter().enumerate().take(save::LIST_SLOTS).filter_map(|(i, s)| s.as_ref().map(|s| (s.timestamp, i))).collect();
        used.sort_by(|a, b| b.cmp(a));
        let mut v = Vec::new();
        if save && let Some(free) = (0..save::LIST_SLOTS).find(|&i| self.slots.get(i).is_none_or(|s| s.is_none())) {
            v.push(free);
        }
        v.extend(used.into_iter().map(|(_, i)| i));
        v
    }

    /// The list entry of the newest save (highlighted when the load list opens).
    fn latest_entry(&self, save: bool) -> Option<usize> {
        let list = self.save_list(save);
        let used = |i: usize| self.slots.get(list[i]).is_some_and(|s| s.is_some());
        (0..list.len()).find(|&i| used(i)).or(if list.is_empty() { None } else { Some(0) })
    }

    fn act(&mut self, game: &mut Game, r: &Renderer, a: Action) {
        match a {
            Action::NewGame => {
                // title case 1 -> state 20: system sound 6, BGM out over 1.5 s; with node 485 read the screen
                // fades to white (+0.01 / frame up to 1.2) before START_SCRIPT
                if self.screen == Screen::Title {
                    self.snd(game, 6);
                }
                if self.screen == Screen::Title && crate::game::get_bit(&game.nodes, 485) {
                    game.sound.stop_bgm(1.5);
                    self.black = 0.0;
                    self.black_col = [1.0; 3];
                    self.black_speed = 0.01;
                    self.black_under = false;
                    self.black_then = Some(Action::StartNew);
                } else {
                    self.act(game, r, Action::StartNew);
                }
            }
            Action::StartNew => {
                self.black = 0.0;
                self.black_speed = 0.0;
                self.black_col = [0.0; 3];
                game.reset();
                game.start_scene(0, None);
                self.screen = Screen::Game;
                self.stack.clear();
                self.menu_bg = MenuBg::new(0x2545_f491_4f6c_dd1d);
            }
            Action::Continue => {
                let latest = self.continue_slot();
                // title case 0: "ロード中です" (184) held open (sub_4ABD0), state 28 loads and closes it, state 30
                // fades to black (+0.01 / frame to 1.2) with the BGM out over 1.5 s
                // sound 1 when it loads, 2 with nothing to load
                self.snd(game, if latest.is_some() { 1 } else { 2 });
                match latest {
                    Some(i) => {
                        self.popup(game, 184, f32::INFINITY);
                        self.cont_slot = Some(i);
                    }
                    None => self.popup(game, 193, 480.0),
                }
            }
            Action::LoadNow(i) => {
                self.black = 0.0;
                self.black_speed = 0.0;
                self.title_ui_hidden = false;
                if self.slots.get(i).is_some_and(Option::is_some) {
                    self.load(game, r, i);
                } else {
                    // the slot went missing or is unreadable: "ロードに失敗しました"
                    self.popup(game, 190, 480.0);
                }
            }
            Action::OpenLoad => {
                self.refresh_slots(game);
                // title case 2 / system menu tab 1 (sub_4CEA0; sound 2 while saving is disabled, sub_47B10)
                self.snd(game, 1);
                if self.screen == Screen::Title {
                    self.menu_bg.fade(0.02);
                }
                self.push(Screen::SaveLoad { save: false, scroll: 0 });
                self.hover = self.latest_entry(false);
            }
            Action::OpenSave => {
                let ok = game.can_save() || game.in_choice();
                self.snd(game, if ok { 1 } else { 2 });
                if ok {
                    self.refresh_slots(game);
                    self.push(Screen::SaveLoad { save: true, scroll: 0 });
                    // sub_50E00 zeroes the scroll and cursor (qword_F1F44C) on every open: the cursor starts
                    // on the first entry, the blank slot
                    self.hover = Some(0);
                }
            }
            Action::OpenConfig => {
                // title case 3 / system menu tab 2: system sound 1
                self.snd(game, 1);
                if self.screen == Screen::Title {
                    self.menu_bg.fade(0.02);
                }
                self.conf_prev = [[0.0; 3]; 2];
                self.push(Screen::Config);
                self.refresh_voice_rows(game);
                // sub_2B680: the open tab's scroll comes back without a tween; the scroll widget fades in with
                // the screen unless that tab does not scroll
                self.conf_sb_a = 0.0;
                self.conf_ca = 1.0;
                self.conf_next = None;
                self.conf_show_tab();
                // the cursor starts on the open tab's button in the tab bar
                self.conf_bar = self.conf_tab;
                self.hover = Some(self.conf_tab);
            }
            Action::OpenFlow => {
                // availability: the gallery needs node 1 read (loc_14CFC); in game sub_1767C needs route
                // node 1 and the current node outside the ending / "Teach me" list (unk_3374E8, 25 rows)
                let gallery = !matches!(self.screen, Screen::Menu);
                let fl = game.flow_flags();
                let ok = if gallery {
                    fl.read.get(1).copied().unwrap_or(false)
                } else {
                    let cur = game.route_last();
                    let ending = |n: i32| n == 557 || crate::gallery::ENDINGS.iter().any(|e| e.0 as i32 == n);
                    fl.route.get(1).copied().unwrap_or(false) && !cur.is_some_and(ending)
                };
                if ok {
                    self.snd(game, 1);
                    self.open_flow(game, gallery);
                } else {
                    self.snd(game, 2);
                    self.popup(game, 1496, 480.0);
                }
            }
            Action::Node(n) => {
                // sub_11418: A on a node plays sound 1, then the dialog opens
                if crate::flow::node_entry(n).is_some() {
                    self.snd(game, 1);
                    self.confirm(game, Confirm::Jump(n));
                }
            }
            Action::OpenExtra => {
                // title case 5 / state 48: system sound 1; the backdrop fades in (sub_49630 0.02) while the title
                // under it fades to black (0.02 / frame up to 1.2); then cGallery opens
                self.snd(game, 1);
                self.menu_bg.fade(0.02);
                self.black_speed = 0.02;
                self.black_under = true;
                self.black_then = Some(Action::EnterGallery);
            }
            Action::EnterGallery => {
                if !self.tex.contains_key("gallerymenu_btn") {
                    match game.assets.ui_textures(r, "GALLERY_PARTS_JA") {
                        Ok(list) => self.tex.extend(list),
                        Err(e) => tracing::warn!("gallery parts: {e:#}"),
                    }
                }
                self.black = 0.0;
                self.black_speed = 0.0;
                self.black_under = false;
                self.menu_bg.fade(0.04);
                self.act(game, r, Action::OpenGallery);
            }
            Action::OpenMusic => {
                use crate::game::get_bit;
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                self.music_ok = crate::gallery::music_unlocked(
                    |n| n > 0 && get_bit(&game.heard, n as usize),
                    |n| get_bit(&game.nodes, n),
                    game.movie_flags,
                );
                self.enter_mode(game, Screen::Music);
            }
            Action::Track(i) => {
                if self.music_ok.get(i).copied().unwrap_or(false) {
                    if self.playing_track == Some(i) {
                        if self.music_paused {
                            self.music_paused = false;
                            game.sound.set_bgm_paused(false);
                        }
                    } else {
                        self.music_paused = false;
                        game.play_bgm(crate::gallery_table::MUSIC[i].0, self.music_vol, 0.3);
                        self.playing_track = Some(i);
                        self.last_track = Some(i);
                        self.music_clock = 0.0;
                    }
                }
            }
            Action::MusicCtl(k) => {
                let n = crate::gallery_table::MUSIC.len();
                let step = |from: usize, d: i32| {
                    (1..=n as i32)
                        .map(|j| (from as i32 + d * j).rem_euclid(n as i32) as usize)
                        .find(|&t| self.music_ok.get(t).copied().unwrap_or(false))
                };
                let cur = self.playing_track.or(self.last_track);
                let target = match k {
                    // play: resume, or the last played track / the first unlocked one
                    0 if self.music_paused => {
                        self.music_paused = false;
                        game.sound.set_bgm_paused(false);
                        None
                    }
                    0 if self.playing_track.is_none() => cur.or_else(|| step(n - 1, 1)),
                    1 if self.playing_track.is_some() => {
                        self.music_paused = true;
                        game.sound.set_bgm_paused(true);
                        None
                    }
                    2 => {
                        self.music_paused = false;
                        if self.playing_track.take().is_some() {
                            game.play_bgm(0, 1.0, 0.3);
                        }
                        None
                    }
                    3 => {
                        self.music_repeat = !self.music_repeat;
                        None
                    }
                    4 => step(cur.unwrap_or(0), -1),
                    5 => step(cur.unwrap_or(n - 1), 1),
                    _ => None,
                };
                if let Some(t) = target {
                    self.playing_track = None;
                    self.act(game, r, Action::Track(t));
                }
            }
            Action::MusicVol => {
                let v = ((self.mouse.0 - 1412.0) / 364.0).clamp(0.0, 1.0);
                self.music_vol = (v * 20.0).round() / 20.0;
                game.sound.set_bgm_volume(self.music_vol, 0.1);
            }
            Action::OpenMovies => self.enter_mode(game, Screen::Movies),
            Action::Movie(i) => {
                let flag = crate::gallery::MOVIES[i].1;
                if flag.is_none_or(|f| game.movie_flags & (1 << f) != 0) {
                    // sub_183EC: sound 1, then the dialog
                    self.snd(game, 1);
                    self.confirm(game, Confirm::Movie(i));
                }
            }
            Action::OpenEndings => self.enter_mode(game, Screen::Endings),
            Action::Ending(i) => {
                let ok = if i == 24 { Self::all_endings(game) } else { crate::game::get_bit(&game.nodes, crate::gallery::ENDINGS[i].0) };
                if ok {
                    // sub_E7A4: sound 1, then the dialog
                    self.snd(game, 1);
                    self.confirm(game, Confirm::Ending(i));
                }
            }
            Action::OpenGallery => {
                if !self.tex.contains_key("cg_thumb_all") {
                    match game.assets.ui_textures(r, "CG_THUMB_PARTS_JA") {
                        Ok(list) => self.tex.extend(list),
                        Err(e) => tracing::warn!("gallery thumbnails: {e:#}"),
                    }
                }
                self.refresh_gallery(game);
                self.enter_mode(game, Screen::Gallery);
            }
            Action::Cg(g) => {
                if self.cg_unlocked.get(g).is_some_and(|e| !e.is_empty()) {
                    // sub_9CBC: sound 1 as the viewer opens
                    self.snd(game, 1);
                    self.cg_zoom = 0.0;
                    self.cg_pan = [0.0; 2];
                    self.push(Screen::CgView { group: g, pos: 0 });
                }
            }
            Action::NextCg => {
                if let Screen::CgView { group, pos } = self.screen {
                    let n = self.cg_unlocked.get(group).map_or(0, Vec::len);
                    if pos + 1 < n {
                        self.cg_step(game, 1);
                    } else {
                        self.pop();
                    }
                }
            }
            Action::ToTitle => self.confirm(game, Confirm::Title),
            Action::Back => {
                // B / the back button: sound 2 (sub_4CEA0, sub_50FC0, sub_2B5C0, sub_49A40, sub_28A90, sub_6E28, ...)
                if self.screen != Screen::MoviePlay {
                    self.snd(game, 2);
                }
                if self.screen == Screen::MoviePlay {
                    game.stop_movie();
                    game.gallery_movie = false;
                    self.pop();
                } else {
                    self.close();
                }
            }
            Action::Slot(i) => {
                let Screen::SaveLoad { save, .. } = self.screen else { return };
                if save {
                    if self.slots.get(i).is_some_and(|s| s.is_some()) {
                        self.confirm(game, Confirm::Overwrite(i));
                    } else {
                        self.save(game, r, i);
                    }
                } else if self.slots.get(i).is_some_and(|s| s.is_some()) {
                    if self.stack.contains(&Screen::Game) {
                        self.confirm(game, Confirm::Load(i));
                    } else if self.stack.contains(&Screen::Title) {
                        // sub_50FC0 state 19: sound 1
                        self.snd(game, 1);
                        // from the title the window closes over the title movie first (sub_50FC0 state 19
                        // applies the record only in game; the title loads it after state 30's fade)
                        self.load_after_close = Some(i);
                        self.close();
                    } else {
                        self.snd(game, 1);
                        self.load(game, r, i);
                    }
                }
            }
            Action::Setting(i, d) => {
                // sub_2B930: the option buttons (types 2, 6..10) play sound 1; sliders, their - / + buttons and the
                // voice test are silent; a tab switches with sound 1 (sub_29F00); Default opens dialog 27
                match Ctl::from_code(i) {
                    Some(Ctl::Default) => {
                        self.confirm(game, Confirm::Defaults);
                        return;
                    }
                    Some(Ctl::Toggle(_) | Ctl::Radio(..) | Ctl::Mute(_)) => self.snd(game, 1),
                    _ => {}
                }
                self.conf_change(game, i, d);
                self.store_global(game);
            }
            Action::Yes => {
                let Screen::Confirm(c) = self.screen else { return };
                // sub_49A40 / sub_49F00: A on either button plays sound 1
                self.snd(game, 1);
                self.pop();
                match c {
                    Confirm::Movie(i) => self.play_gallery_movie(game, r, i),
                    Confirm::Ending(i) => self.play_ending(game, i),
                    Confirm::Title => self.leave_to_title(game, r),
                    Confirm::Overwrite(i) => self.save(game, r, i),
                    Confirm::Load(i) => self.load(game, r, i),
                    Confirm::PrevScene => {
                        game.prev_scene(r);
                    }
                    Confirm::NextScene => game.next_scene(),
                    Confirm::QuickLoad => self.quick_load(game, r),
                    Confirm::Defaults => {
                        self.conf_change(game, Ctl::Default.code(), 0);
                        self.store_global(game);
                    }
                    Confirm::Jump(n) => {
                        self.store_global(game);
                        if game.jump_node(n) {
                            self.screen = Screen::Game;
                            self.stack.clear();
                            self.title_movie = false;
                            // as after a load: the system menu backdrop does not stay over the new scene
                            self.menu_bg = MenuBg::new(0x2545_f491_4f6c_dd1d);
                        }
                    }
                }
            }
            Action::No => {
                self.snd(game, 1);
                self.pop();
            }
        }
    }

    fn play_gallery_movie(&mut self, game: &mut Game, r: &Renderer, i: usize) {
        game.play_bgm(0, 1.0, 0.3);
        self.playing_track = None;
        game.gallery_movie = true;
        game.play_movie(r, crate::movie::MOVIES[crate::gallery::MOVIES[i].0]);
        self.title_movie = false;
        self.push(Screen::MoviePlay);
    }

    /// cGalleryEnding: plays the "Teach me, Ciel-sensei" scene of an ending (sub_5040).
    fn play_ending(&mut self, game: &mut Game, i: usize) {
        if let Some(scene) = game.assets.scene_by_id(crate::gallery::ENDINGS[i].1) {
            self.store_global(game);
            game.reset();
            game.start_scene(scene, None);
            self.screen = Screen::Game;
            self.stack.clear();
            self.title_movie = false;
        }
    }

    fn save(&mut self, game: &mut Game, _r: &Renderer, slot: usize) {
        let Some(data) = game.save_data() else { return };
        // sub_50FC0: "セーブ中です" (183) is shown with a 1200-frame limit but held (sub_4ABD0(1)) and closed
        // (sub_4ACD0) once the write is done and 30 frames have passed since it opened; "セーブに失敗しました"
        // (189) on failure
        match self.write_record(game, slot, &data) {
            Ok(()) => {
                // sub_50FC0 state 8 plays sound 1 as the popup closes; knife as the write is done
                self.snd(game, 1);
                self.popup_held(game, 183, 30.0);
            }
            Err(e) => {
                tracing::warn!("save to slot {slot}: {e:#}");
                self.popup(game, 189, 480.0);
            }
        }
        self.refresh_slots(game);
    }

    pub fn debug_save(&mut self, game: &mut Game, r: &Renderer, slot: usize) -> bool {
        if !game.can_save() {
            return false;
        }
        self.save(game, r, slot);
        tracing::info!("debug save to {slot}");
        true
    }

    pub fn debug_load(&mut self, game: &mut Game, r: &Renderer, slot: usize) {
        self.load(game, r, slot);
    }

    pub fn quick_save(&mut self, game: &mut Game, r: &Renderer) {
        if self.screen != Screen::Game {
            return;
        }
        // sub_52860 (mode panel): written silently, the Quick Save banner shows
        if game.can_save() && self.write_quiet(game, r, save::QUICK_SLOT) {
            self.show_banner(1);
        }
    }

    pub fn quick_load(&mut self, game: &mut Game, r: &Renderer) {
        if matches!(self.screen, Screen::Game | Screen::Title) && self.slots.get(save::QUICK_SLOT).is_some_and(|s| s.is_some()) {
            self.load(game, r, save::QUICK_SLOT);
        }
    }

    fn load(&mut self, game: &mut Game, r: &Renderer, slot: usize) {
        let Some(data) = self.file.record(slot).and_then(|rec| game.decode_record(rec)) else { return };
        game.reset();
        game.restore(r, &data);
        self.screen = Screen::Game;
        self.stack.clear();
        self.title_movie = false;
        self.menu_bg = MenuBg::new(0x2545_f491_4f6c_dd1d);
    }

    /// System menu -> title (sub_4CEA0 case 4 / sub_4CCA0): unless the game cannot be saved here, write the
    /// auto save (sub_52AA0) and show its banner; the menu fades out (1/30 per frame) and the title follows
    /// once the banner is gone (sub_4D710).
    fn leave_to_title(&mut self, game: &mut Game, r: &Renderer) {
        // sub_52A20: no new auto save (and no banner) when the record matches the newest auto save
        let same = game
            .save_data()
            .zip(self.slots.get(save::AUTO_FIRST).and_then(Option::as_ref))
            .is_some_and(|(a, b)| a.scene == b.scene && a.pc == b.pc);
        if same || !game.can_save() || !self.write_quiet(game, r, save::AUTO_FIRST) {
            self.to_title(game);
            return;
        }
        self.show_banner(2);
        self.title_after_banner = true;
        self.closing = true;
        self.menu_bg.fade(-1.0 / 30.0);
    }

    /// Save without the save-list popups (auto / quick save).
    fn write_quiet(&mut self, game: &mut Game, _r: &Renderer, slot: usize) -> bool {
        let Some(data) = game.save_data() else { return false };
        if let Err(e) = self.write_record(game, slot, &data) {
            tracing::warn!("save to slot {slot}: {e:#}");
            return false;
        }
        self.refresh_slots(game);
        true
    }

    /// sub_90440, every title frame: once every ending is completed (sub_926F0(1)) and G069 is clear, set G069
    /// and show the banner; it rises over 30 frames, holds 300, then fades out over 30.
    fn complete_update(&mut self, game: &mut Game, f: f32) {
        match &mut self.complete {
            Some(c) => match c.state {
                1 => {
                    c.a += f / 30.0;
                    if c.a >= 1.0 {
                        c.a = 1.0;
                        c.state = 2;
                    }
                    c.off = (1.0 - ((c.a - 1.0).powi(3) + 1.0)) * 60.0;
                }
                2 => {
                    c.hold += f;
                    if c.hold >= 300.0 {
                        c.state = 3;
                    }
                }
                _ => {
                    c.a -= f / 30.0;
                    if c.a <= 0.0 {
                        self.complete = None;
                    }
                }
            },
            None if Self::all_endings(game) && game.var("G069") == 0 => {
                game.set_var_pub("G069", 1);
                self.store_global(game);
                self.complete = Some(Complete { state: 1, a: 0.0, off: 60.0, hold: 0.0 });
            }
            None => {}
        }
    }

    /// sub_91350, in 960x540 (x2 here): a navy box at 0.5 alpha with a black one at 0.753 inset by 0.5 over
    /// x 384..936, y 480..517.5 (+ the rise offset), and SYSMES 239 (text slot 65, 0.42 of the 60 px glyph)
    /// at (401, 490).
    fn draw_complete(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some(c) = self.complete else { return };
        let y = (480.0 + c.off) * 2.0;
        out.push(Quad::rect(r.white.clone(), [768.0, y, 1104.0, 75.0], [0.19216, 0.24706, 0.58039, 0.50196 * c.a]));
        out.push(Quad::rect(r.white.clone(), [769.0, y + 1.0, 1102.0, 73.0], [0.0, 0.0, 0.0, 0.75294 * c.a]));
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let Some(text) = self.sysmes.get(239).cloned() else { return };
        // the config samples' 0.74 is 49 in ab_glyph's scale
        let size = 49.0 * 0.42 / 0.74;
        let font = game.font_mut();
        let (mut x, base) = (802.0, (490.0 + c.off) * 2.0 + size);
        for ch in text.chars() {
            if let Some(q) = font.quad(r, ch, size, x, base, [1.0, 1.0, 1.0, c.a]) {
                out.push(q);
            }
            x += font.advance(ch, size);
        }
    }

    fn show_banner(&mut self, idx: usize) {
        self.banner = Some(Banner { idx, out: false, t: 0.0 });
    }

    /// sub_3B480: a button while the banner holds sends it away.
    fn skip_banner(&mut self) {
        if let Some(b) = &mut self.banner
            && !b.out
            && b.t >= 1.0
        {
            b.t = 3.0;
        }
    }

    /// Auto mode toggled from the keyboard; turning it on shows the Auto banner (sub_40ED0).
    pub fn toggle_auto(&mut self, game: &mut Game) {
        game.auto = !game.auto;
        if game.auto {
            self.show_banner(3);
        }
    }

    fn draw_banner(&self, out: &mut Vec<Quad>) {
        let Some(b) = self.banner else { return };
        let p = (b.t.max(0.0) * 9.0).min(9.0);
        let (k, f) = (p as usize, p.fract());
        let (k0, k1) = (BANNER_KEYS[k.min(9)], BANNER_KEYS[(k + 1).min(9)]);
        let sx = k0[0] * (1.0 - f) + k1[0] * f;
        let sy = k0[1] * (1.0 - f) + k1[1] * f;
        let [x0, y0, x1, y1] = BANNER_SRC[b.idx];
        // anchor top-left at (0, 32) of 640x448
        self.sprite(out, "popup_ja", [0.0, 32.0 * H / 448.0, (x1 - x0) * sx, (y1 - y0) * sy], Some([x0, y0, x1 - x0, y1 - y0]));
    }

    fn to_title(&mut self, game: &mut Game) {
        self.store_global(game);
        game.reset();
        self.menu_bg = MenuBg::new(0x2545_f491_4f6c_dd1d);
        self.enter_title();
    }

    /// Per-frame bookkeeping.
    pub fn update(&mut self, game: &mut Game, r: &Renderer, dt: f32) {
        // SELA Yes: the save screen over the game (sub_84A90 states 6 / 7), then the script goes on
        if self.screen == Screen::Game {
            if game.take_choice_save() {
                self.refresh_slots(game);
                self.push(Screen::SaveLoad { save: true, scroll: 0 });
                self.hover = Some(0);
            } else {
                game.choice_save_closed();
            }
        }
        // dt is in milliseconds; the UI clocks run in seconds
        let secs = dt / 1000.0;
        self.gal_scroll.step(dt);
        if std::mem::take(&mut self.gal_restore) && self.key_focus && Self::gal_mode(self.screen).is_some() {
            let a = if self.gal_side { Some(Self::hub_items()[self.gal_tab].1) } else { self.gal_mem };
            if let Some(a) = a {
                self.hover_action(game, a);
            }
        }
        if self.flow_t < FLOW_SCROLL_MS {
            self.flow_t = (self.flow_t + dt).min(FLOW_SCROLL_MS);
            let t = self.flow_t / FLOW_SCROLL_MS;
            self.flow_scroll = self.flow_from + (self.flow_target - self.flow_from) * -(t * (t - 2.0));
        }
        if self.conf_t < CONF_SCROLL_MS {
            self.conf_t = (self.conf_t + dt).min(CONF_SCROLL_MS);
            let t = self.conf_t / CONF_SCROLL_MS;
            let e = -(t * (t - 2.0));
            self.conf_scroll = self.conf_from + (self.conf_target[self.conf_tab] - self.conf_from) * e;
        }
        if let Some(p) = &mut self.popup {
            let f = secs * 60.0;
            p.age += f;
            if p.close_at.is_some_and(|n| p.age >= n) {
                p.closing = true;
            }
            if p.closing {
                p.a -= 0.07 * f;
                if p.a <= 0.0 {
                    self.popup = None;
                }
            } else if p.a < 1.0 {
                p.a = (p.a + 0.07 * f).min(1.0);
            } else {
                p.frames -= f;
                p.closing = p.frames < 1.0;
            }
        }
        // sub_4AE90 opening: +1/10 per frame (60 fps)
        self.dlg_t = (self.dlg_t + secs * 6.0).min(1.0);
        // sub_4CBA0 -> sub_49630(1, ..) -> sub_2E000: the system menu blurs the scene (+1/12 per frame from
        // 0.001) and the message text is hidden; it ends once the backdrop has faded out (sub_2E030(0, 1))
        let f60 = secs * 60.0;
        if self.screen == Screen::Menu && self.stack.contains(&Screen::Game) && game.menu_blur.is_none() {
            game.menu_blur = Some(0.001);
        } else if let Some(a) = &mut game.menu_blur {
            let over_game = self.screen == Screen::Game || self.stack.contains(&Screen::Game);
            if !over_game || (self.screen == Screen::Game && !self.menu_bg.visible()) {
                game.menu_blur = None;
            } else {
                *a = (*a + f60 / 12.0).min(1.0);
            }
        }
        game.hide_text = matches!(self.screen, Screen::Backlog { .. }) || game.menu_blur.is_some() || self.touch.hidden;
        if let Some(t) = self.playing_track.filter(|_| !self.music_paused) {
            self.music_clock += secs;
            // without repeat the player moves on to the next unlocked track at the end
            let len = crate::gallery_table::MUSIC[t].2 as f32 / 1000.0;
            if self.music_clock >= len {
                self.music_clock -= len;
                if !self.music_repeat {
                    self.act(game, r, Action::MusicCtl(5));
                }
            }
        }
        if self.screen == Screen::Game {
            self.title_clock = 0.0;
        } else {
            self.title_clock += secs;
        }
        // the exe steps these per 60 Hz frame
        // sub_8E200 runs with no frame time while its movie is started but not ready yet (sub_1A30), so the
        // title stays black until the movie can show
        let title_wait = self.screen == Screen::Title && self.title_movie && game.movie_active() && !game.movie_ready();
        let f = if title_wait { 0.0 } else { secs * 60.0 };
        // the balloon restarts only when its object changes (or on a tap, see `click`)
        let item = self.help_target().map(|(id, p, _, _)| (id, [p[0].round() as i32, p[1].round() as i32]));
        if item != self.help_item {
            self.help_item = item;
            self.help_t = 0.0;
        }
        self.help_t = (self.help_t + 0.05 * f).min(1.0);
        self.intro_update(game, r, f);
        self.menu_bg.update(f);
        // the title movie stops while a submenu backdrop covers the title
        game.hold_movie(self.title_movie && self.menu_bg.visible());
        let on_title = self.screen == Screen::Title
            || self.stack.first() == Some(&Screen::Title)
            || matches!(self.intro, Some(Intro::PressOut { .. }));
        if on_title {
            self.logo_fx.update(f);
            // logo alpha (sub_8D5D0): +1/120 per frame, -1/60 while a submenu backdrop is up
            self.logo_rising = !self.menu_bg.visible();
            self.logo_a = (self.logo_a + if self.logo_rising { f / 120.0 } else { -f / 60.0 }).clamp(0.0, 1.0);
            self.press_t += f;
        }
        if self.black_speed != 0.0 {
            self.black = (self.black + self.black_speed * f).clamp(0.0, 1.2);
            if self.black <= 0.0 {
                self.black_speed = 0.0;
            } else if self.black >= 1.2 {
                self.black_speed = 0.0;
                if let Some(a) = self.black_then.take() {
                    self.act(game, r, a);
                }
            }
        }
        let (fin, fout) = Self::fade_speeds(self.screen);
        if self.closing {
            self.scr_a -= fout * f;
            if self.scr_a <= 0.0 {
                if self.title_after_banner {
                    self.scr_a = 0.0;
                } else {
                    self.finish_close(game);
                }
            }
        } else {
            self.scr_a = (self.scr_a + fin * f).min(1.0);
        }
        if let Some(next) = self.gal_next {
            self.gal_a -= secs * 2.0;
            if self.gal_a <= 0.0 {
                self.gal_a = 0.0;
                self.gal_next = None;
                self.screen = next;
                self.gal_mode_shown(game);
            }
        } else {
            self.gal_a = (self.gal_a + secs * 2.0).min(1.0);
        }
        if self.screen == Screen::Config {
            self.conf_tick(f);
        }
        if self.screen == Screen::Config && self.conf_tab == 0 {
            self.conf_preview_update(game, f);
        }
        let on_title = self.screen == Screen::Title;
        if on_title != self.on_title {
            self.on_title = on_title;
            game.title_stage_step(self.boot_flag);
            if on_title {
                game.play_title_bgm();
            }
        }
        if self.screen == Screen::Title && !self.press && !self.title_movie {
            self.title_movie = true;
            game.play_title_movie(r);
        }
        if on_title {
            self.complete_update(game, f);
        } else {
            self.complete = None;
        }
        // the banner (sub_3C970): +0.04 / frame up to 3, then back from 1 to 0
        if let Some(b) = &mut self.banner {
            if b.out {
                b.t -= 0.04 * f;
                if b.t < 0.0 {
                    self.banner = None;
                }
            } else {
                b.t += 0.04 * f;
                if b.t > 3.0 {
                    *b = Banner { out: true, t: 1.0, ..*b };
                }
            }
        }
        if self.title_after_banner && self.banner.is_none() {
            self.title_after_banner = false;
            self.closing = false;
            self.to_title(game);
        }
        if let Some(i) = self.cont_slot
            && self.popup.is_none_or(|p| p.a >= 1.0 || p.closing)
        {
            self.cont_slot = None;
            if let Some(p) = &mut self.popup {
                p.closing = true;
            }
            game.sound.stop_bgm(1.5);
            self.black_col = [0.0; 3];
            self.black_under = false;
            self.black_speed = 0.01;
            self.black_then = Some(Action::LoadNow(i));
        }
        if self.screen == Screen::MoviePlay && !game.movie_active() {
            game.gallery_movie = false;
            self.pop();
        }
        if self.screen == Screen::Game && game.ended {
            self.to_title(game);
        }
        self.touch_update(game, r, secs, f);
    }

    pub fn game_paused(&self) -> bool {
        !matches!(self.screen, Screen::Game)
    }

    // ---- drawing -----------------------------------------------------------------

    /// Draw a UI texture by name; `src` is a pixel rectangle inside it (whole texture if None).
    fn sprite(&self, out: &mut Vec<Quad>, name: &str, dst: [f32; 4], src: Option<[f32; 4]>) -> bool {
        let Some(t) = self.tex.get(name) else { return false };
        let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
        let [x, y, w, h] = src.unwrap_or([0.0, 0.0, t.width as f32, t.height as f32]);
        out.push(Quad::rect(t.clone(), dst, [1.0; 4]).with_uv_rect(x / tw, y / th, (x + w) / tw, (y + h) / th));
        true
    }

    /// The scroll widget (sub_276B0 / sub_2788C): scroll_bar at 1847,105 and scroll_slider at
    /// y 99 + 610 * pos / range, both hidden when range <= 0. sub_2788C passes x 1844 for the slider, but on
    /// the Switch its nub sits centred on the track (light span 1850..1861 over the 1854..1857 line), which
    /// is x 1839, as in the backlog.
    fn draw_scroll_widget(&self, out: &mut Vec<Quad>, pos: f32, range: f32) {
        if range > 0.0 {
            self.draw_scroll_bar(out, pos / range, 610.0);
        }
    }

    /// Track and thumb at `frac` of `travel` px below y 99 (sub_9B0C4, the backlog / config widget, runs
    /// 41.07..293.27 of 448 = 608 px; sub_2788C 610 px).
    fn draw_scroll_bar(&self, out: &mut Vec<Quad>, frac: f32, travel: f32) {
        self.sprite(out, "scroll_bar", [1847.0, 105.0, 16.0, 888.0], Some([0.0, 0.0, 16.0, 888.0]));
        let y = frac.clamp(0.0, 1.0) * travel + 99.0;
        self.sprite(out, "scroll_slider", [1839.0, y, 32.0, 288.0], Some([48.0, 24.0, 32.0, 288.0]));
    }

    /// Top-left of save list frame i on the canvas.
    fn save_frame(i: usize) -> [f32; 2] {
        [133.667 * 3.0, (36.919 + 93.748 * i as f32) * H / 448.0]
    }

    /// The system menu (sub_4C370) drawn under the save/load and config screens when they were opened
    /// from it. `open`: the button of the open submenu (selected column); `cursor`: the tab under the cursor
    /// (hover column, with the cursor light and line).
    fn draw_menu_base(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, open: Option<usize>, cursor: Option<usize>) {
        self.sprite(out, "system_titlebg", [0.0, 0.0, W, 392.0], None);
        self.sprite(out, "system_scenebg", [752.0, 0.0, 1168.0, 80.0], None);
        self.sprite(out, "underline", [0.0, 72.0, W, 16.0], None);
        let phase = game.var("F023");
        if let Some(&[x0, y0, x1, y1]) = usize::try_from(phase).ok().and_then(|p| PHASE_TITLES.get(p)) {
            let src = [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32];
            self.sprite(out, "system_phasetitle_ja", [8.0, 0.0, src[2], src[3]], Some(src));
        }
        let title = usize::try_from(game.var("F022")).ok().and_then(|t| self.sysmes.get(t + 395)).cloned();
        // text slot 60 (sub_4C370): colour 0 (black), 0.36 of the 60 px base glyph, right-aligned at x 608
        if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
            if self.font_ui.is_none() {
                self.font_ui = Self::ui_font(game, r, "ja3/FOT-SeuratPro-M.otf");
            }
            if let Some(font) = self.font_ui.as_mut() {
                let w = font.measure(&t, 24.0);
                font.draw_str(r, out, &t, 24.0, 1824.0 - w, 66.0, [0.0, 0.0, 0.0, 1.0]);
            }
        }
        self.sprite(out, "menu_window", [56.0, 0.0, 312.0, H], None);
        for i in 0..5 {
            // menu_btn_ja: rows Save, Load, Config, FlowChart, Title; columns normal / hover / selected
            let col = if open == Some(i) {
                2.0
            } else if cursor == Some(i) {
                1.0
            } else {
                0.0
            };
            let rc = [80.0, 105.0 + i as f32 * 75.0, 224.0, 88.0];
            self.sprite(out, "menu_btn_ja", rc, Some([24.0 + col * 240.0, 24.0 + i as f32 * 104.0, 224.0, 88.0]));
        }
        if let Some(i) = cursor {
            let blink = 0.5 + 0.5 * (self.title_clock * 3.0).sin().abs();
            self.sprite_c(out, "systemcursor_light", [0.0, 32.0 + i as f32 * 75.0, 528.0, 240.0], None, blink, Blend::Add);
            self.sprite_c(out, "systemcursor_line", [86.0, 136.0 + i as f32 * 75.0, 224.0, 56.0], None, 1.0, Blend::Add);
        }
    }

    /// Draws `digits` (0-9 plus one separator glyph at index 10) from a 24x32-cell number atlas
    /// (savedate_number / savetime_number; cells at x 16 + 32k, y 16).
    fn draw_digits(&self, out: &mut Vec<Quad>, tex: &str, digits: &[(u32, f32)], y: f32) {
        for &(d, x) in digits {
            let src = [16.0 + 32.0 * d as f32, 16.0, 24.0, 32.0];
            self.sprite(out, tex, [x, y, 24.0, 32.0], Some(src));
        }
    }

    /// `sprite` with an alpha and blend mode.
    #[allow(clippy::too_many_arguments)]
    fn sprite_c(&self, out: &mut Vec<Quad>, name: &str, dst: [f32; 4], src: Option<[f32; 4]>, alpha: f32, blend: Blend) -> bool {
        let n = out.len();
        if !self.sprite(out, name, dst, src) {
            return false;
        }
        for q in &mut out[n..] {
            q.color[3] = alpha;
            q.blend = blend;
        }
        true
    }

    /// Draw a UI texture at its native size (or `src`'s size) with the top-left at (x, y).
    fn spr(&self, out: &mut Vec<Quad>, name: &str, x: f32, y: f32, src: Option<[f32; 4]>) -> bool {
        let Some(t) = self.tex.get(name) else { return false };
        let (w, h) = src.map_or((t.width as f32, t.height as f32), |s| (s[2], s[3]));
        self.sprite(out, name, [x, y, w, h], src)
    }

    /// cGallery backdrop (sub_6124 sprite structs at 0x44E2F8): content frame, top underline and
    /// the mode's title banner.
    /// Over black (the title faded out before cGallery opened) and the submenu backdrop. Returns where the
    /// mode's own sprites start (they crossfade when the mode changes).
    fn draw_hub_bg(&self, out: &mut Vec<Quad>, white: &TexRef, banner: Option<&str>) -> usize {
        out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
        self.draw_menu_layer(out);
        let n = out.len();
        self.spr(out, "config_innerbg", 365.0, 0.0, None);
        self.spr(out, "underline", 0.0, 72.0, None);
        self.fade_quads(out, n, self.scr_a);
        let n0 = out.len();
        if let Some(b) = banner {
            self.spr(out, b, 64.0, 0.0, None);
        }
        n0
    }

    /// Multiply the alpha of the quads from `n` on (not while drawing the screen under a dialog).
    fn fade_quads(&self, out: &mut [Quad], n: usize, a: f32) {
        if self.under || a >= 1.0 {
            return;
        }
        for q in &mut out[n..] {
            q.color[3] *= a.max(0.0);
        }
    }

    /// The submenu backdrop (sub_4B880): menuBG01, the eight strips and the sparkles.
    fn draw_menu_layer(&self, out: &mut Vec<Quad>) {
        let bg = &self.menu_bg;
        if !bg.visible() {
            return;
        }
        self.sprite_c(out, "menuBG01_upscale", [0.0, 0.0, W, H], None, bg.a, Blend::Alpha);
        let (k, sa) = (bg.slide(), bg.strip_alpha());
        for (tex, alpha, cx, y0, dy) in fx::STRIPS {
            let Some(t) = self.tex.get(tex) else { continue };
            let (w, h) = (t.width as f32, t.height as f32);
            let cy = y0 + dy * k;
            self.sprite_c(out, tex, [cx - w / 2.0, cy - h / 2.0, w, h], None, sa * alpha / 255.0, Blend::Alpha);
        }
        for d in bg.sparks() {
            self.sprite_c(out, "kirakira", d.dst, Some(d.src), d.alpha, Blend::Alpha);
        }
    }

    /// The title screen (sub_8D5D0): background (title_bg0 in PRESS ANY BUTTON mode, else the title movie),
    /// the logo particles, the logo in three slices, the lumi glow, the menu or the blinking PRESS ANY
    /// BUTTON, and the copyright.
    fn draw_title(&mut self, game: &mut Game, _r: &Renderer, out: &mut Vec<Quad>) {
        if self.press {
            // title_bg0 (2670x1266) centred at (960, 508) at 0.94 (B0460(0.47, 0.4735) in 960x544)
            self.sprite(out, "title_bg0", [-294.9, -86.8, 2509.8, 1190.04], None);
        } else if !game.movie_playing() {
            // the stage's resource list (off_65BF98[n]) loads title_bg n
            self.sprite(out, &format!("title_bg{}", game.title_bg()), [0.0, 0.0, W, H], None);
        }
        // slices fade in with alpha a, a^2, a^3 and out together; none of it while a load fades out
        let a = if self.title_ui_hidden { 0.0 } else { self.logo_a };
        let hi = if self.logo_rising { a * a * a } else { a };
        if hi > 0.0
            && let Some(t) = self.tex.get("logoparticle_all")
        {
            let n0 = out.len();
            let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
            for d in self.logo_fx.draws(hi) {
                let [x, y, w, h] = d.src;
                let (u0, u1) = if d.flip { ((x + w) / tw, x / tw) } else { (x / tw, (x + w) / tw) };
                out.push(Quad::rect(t.clone(), d.dst, [1.0, 1.0, 1.0, d.alpha]).with_uv_rect(u0, y / th, u1, (y + h) / th));
            }
            let clip = [[0.0, 0.0], [W, 0.0], [W, fx::LOGO_CLIP_Y], [0.0, fx::LOGO_CLIP_Y]];
            for q in &mut out[n0..] {
                q.clip = Some(clip);
            }
        }
        for (k, (y0, y1)) in [(0.0, 285.0), (285.0, 570.0), (570.0, 648.0)].into_iter().enumerate() {
            let alpha = if self.logo_rising { a.powi(k as i32 + 1) } else { a };
            self.sprite_c(out, "title_titlelog", [680.0, 72.0 + y0, 560.0, y1 - y0], Some([0.0, y0, 560.0, y1 - y0]), alpha, Blend::Alpha);
        }
        if self.press {
            // press_start blinks (qword_1536188: 1/60 per frame up and down), top-centre at (960, 823)
            let blink = 1.0 - ((self.press_t / 60.0) % 2.0 - 1.0).abs();
            self.sprite_c(out, "press_start", [724.0, 823.17, 472.0, 88.0], None, blink, Blend::Alpha);
        } else {
            let pulse = 1.0 - ((self.title_clock * 0.5) % 2.0 - 1.0).abs();
            self.sprite_c(out, "titlemenu_lumi", [720.0, 552.0, 488.0, 256.0], None, hi * pulse, Blend::Add);
            let items = self.items(game);
            let entries = self.title_entries();
            for (i, (rc, _)) in items.iter().enumerate() {
                let (_, tex, normal) = entries[i];
                let src = Self::title_src(normal, usize::from(self.hover == Some(i)));
                self.sprite_c(out, tex, [rc[0], rc[1], src[2], src[3]], Some(src), hi, Blend::Alpha);
            }
        }
        self.sprite(out, "titlemenu_(c)type-moon", [872.0, 1008.0, 184.0, 64.0], None);
        self.draw_complete(game, _r, out);
        if self.black_under && self.black > 0.0 {
            out.push(Quad::rect(_r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, self.black.min(1.0)]));
        }
        self.draw_menu_layer(out);
    }

    /// cGallery left menu: gallerymenu_window and the mode buttons (gallerymenu_btn rows, columns
    /// normal / hover / active at x 8 / 272 / 536), with the cursor line and light on the hovered one, else
    /// on the active mode.
    /// `n0`: where the mode's sprites start; they fade with the screen and the mode crossfade.
    fn draw_hub_menu(&self, out: &mut Vec<Quad>, items: &[([f32; 4], Action)], hover: Option<usize>, mode: Option<usize>, n0: usize) {
        self.fade_quads(out, n0, self.scr_a * self.gal_a);
        let n1 = out.len();
        self.spr(out, "gallerymenu_window", 56.0, 0.0, None);
        let hub = Self::hub_items();
        let cur = hover.and_then(|h| items.get(h)).and_then(|(_, a)| hub.iter().position(|(_, b)| a == b));
        // the focus (and with it the cursor line and light) rests on the active mode unless another is hovered
        let cur = cur.or(mode);
        for i in 0..hub.len() {
            let sx = if mode == Some(i) {
                536.0
            } else if cur == Some(i) {
                272.0
            } else {
                8.0
            };
            self.spr(out, "gallerymenu_btn", 80.0, 160.0 + i as f32 * 80.0, Some([sx, 8.0 + 104.0 * i as f32, 256.0, 96.0]));
        }
        if let Some(i) = cur {
            let blink = 0.5 + 0.5 * (self.title_clock * 3.0).sin().abs();
            self.sprite_c(out, "systemcursor_light", [0.0, 96.0 + i as f32 * 80.0, 528.0, 240.0], None, blink, Blend::Add);
            self.sprite_c(out, "gallerycursor_line", [80.0, 192.0 + i as f32 * 80.0, 304.0, 56.0], None, 1.0, Blend::Add);
        }
        self.fade_quads(out, n1, self.scr_a);
    }

    /// sub_4A8F0: navy bar (0.0039, 0.2353, 0.3961 at 50%) over y 424.36..449 of 448, then the icons and
    /// words from x 55, bottom-aligned to y 445 / 444 of 448; each advances by its width + 10 / + 8.
    fn draw_guide(&self, out: &mut Vec<Quad>, white: &TexRef, g: usize) {
        let k = H / 448.0;
        out.push(Quad::rect(white.clone(), [0.0, 424.36 * k, W, (449.0 - 424.36) * k], [0.0039, 0.2353, 0.3961, 0.5]));
        let mut x = 55.0;
        for &(kind, id) in GUIDE_SEQ[g] {
            let (tex, r, bottom, pad) = if kind == 0 {
                ("help_key", GUIDE_ICONS[id as usize], 445.0, 6.0)
            } else {
                ("help_txt_ja", GUIDE_WORDS[id as usize], 444.0, 4.0)
            };
            let [x0, y0, x1, y1] = r.map(f32::from);
            self.sprite(out, tex, [x, bottom * k - (y1 - y0), x1 - x0, y1 - y0], Some([x0, y0, x1 - x0, y1 - y0]));
            x += x1 - x0 + pad + 4.0;
        }
    }

    /// dialog_win (824x416) centred on the screen, scaled by the opening animation.
    fn draw_dialog_win(&self, out: &mut Vec<Quad>, t: f32) {
        let k = (1.0 - (1.0 - t) * 0.2).powi(2);
        let (w, h) = (824.0 * k, 416.0 * k);
        self.sprite_c(out, "dialog_win", [(W - w) / 2.0, (H - h) / 2.0, w, h], None, t, Blend::Alpha);
    }

    /// SELS / SELA question (sub_85900 / sub_85E40, 640x448 units): dialog_BG over the screen and dialog_win
    /// at (320, 224) at the choice's alpha, the window scaled by its pop; the message (text slot 20, 0.37 x
    /// 0.46 of the base glyph) centred from y 224 - 42.311 * scale, and the No / Yes buttons.
    fn draw_choice_dialog(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some((msg, hover, a, k)) = game.choice_dialog() else { return };
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        self.sprite_c(out, "dialog_BG", [0.0, 0.0, W, H], None, a, Blend::Alpha);
        let (w, h) = (824.0 * k, 416.0 * k);
        self.sprite_c(out, "dialog_win", [(W - w) / 2.0, (H - h) / 2.0, w, h], None, a, Blend::Alpha);
        let msg = self.sysmes.get(msg).cloned().unwrap_or_default();
        let size = 60.0 * 0.37;
        let top = (224.0 - 42.311 * k) * H / 448.0;
        let font = game.font_mut();
        for (i, line) in msg.split('^').enumerate() {
            let lw = font.measure(line, size);
            font.draw_str(r, out, line, size, (W - lw) / 2.0, top + size + i as f32 * size * 1.33, [1.0, 1.0, 1.0, a]);
        }
        for (i, &(x, yes)) in crate::game::DIALOG_BUTTONS.iter().enumerate() {
            let y = if yes { 0.0 } else { 67.0 };
            let src = if hover == Some(i) { [0.0, y + 135.0, 216.0, 56.0] } else { [224.0, y, 216.0, 56.0] };
            let rc = [x * W / 640.0, crate::game::DIALOG_BUTTON_Y * H / 448.0, 216.0, 56.0];
            self.sprite_c(out, "dialog_btn_ja", rc, Some(src), a, Blend::Alpha);
        }
    }

    /// Dialog message: centred lines from `top` ('^' breaks lines).
    fn draw_dialog_text(&self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, msg: &str, top: f32) {
        let font = game.font_mut();
        for (k, line) in msg.split('^').enumerate() {
            let w = font.measure(line, 36.0);
            font.draw_str(r, out, line, 36.0, (W - w) / 2.0, top + 36.0 + k as f32 * 48.0, [1.0; 4]);
        }
    }

    pub fn draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        self.draw_screen(game, r, out);
        if self.screen == Screen::Game {
            self.draw_choice_dialog(game, r, out);
            self.draw_touch_game(game, out);
        }
        self.draw_ripples(r, out);
    }

    fn draw_screen(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let white = r.white.clone();
        let items = self.items(game);
        let hover = self.hover;
        let screen = self.screen;
        let hovered = |i: usize| hover == Some(i);
        match screen {
            // the backdrop fading out after the system menu closed
            Screen::Game => self.draw_menu_layer(out),
            Screen::Intro => self.draw_intro(game, r, out),
            Screen::Title => self.draw_title(game, r, out),
            Screen::Menu => {
                // sub_4C370 (640x448 units scaled to the canvas): title band, scene band, underline,
                // phase title (F023 indexes the rect table at dword_5912E8+624), window on the left,
                // buttons, cursor line + light, and the scene title SYSMES[F022+395] at the right
                // the cursor shows on the remembered tab as soon as the menu opens
                let cur = Some(self.sys_tab);
                self.draw_menu_layer(out);
                let n0 = out.len();
                self.draw_menu_base(game, r, out, None, cur);
                self.fade_quads(out, n0, self.scr_a);
            }
            Screen::SaveLoad { save, .. } => {
                if self.stack.contains(&Screen::Menu) {
                    self.draw_menu_layer(out);
                    // the line / dot cursor only shows while the tabs have the focus
                    let cur = self.side.then_some(self.sys_tab);
                    self.draw_menu_base(game, r, out, Some(usize::from(!save)), cur);
                } else {
                    // opened from the title: the title (with the backdrop) stays underneath
                    self.draw_under(game, r, out);
                }
                let n0 = out.len();
                // sub_4F240: config_innerbg at x 120 (640 units); per slot the save_win frame (white, or
                // blue when selected), then for used slots: thumbnail, save_scene_plus overlay (date
                // separators + SCENE label), date in savedate_number, time in savetime_number, AUTO SAVE
                // tag, chapter title (savetitle, by F023) and the scene title SYSMES[F022+395]
                self.sprite(out, "config_innerbg", [360.0, 0.0, 1560.0, 1080.0], None);
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                let mut selected = None;
                for (i, (_, a)) in items.iter().enumerate() {
                    let Action::Slot(slot) = *a else { continue };
                    let [fx, fy] = Self::save_frame(i);
                    let src = if hovered(i) { [31.0, 336.0, 960.0, 272.0] } else { [32.0, 32.0, 960.0, 272.0] };
                    self.sprite(out, "save_win", [fx, fy, 960.0, 272.0], Some(src));
                    let Some(d) = self.slots.get(slot).cloned().flatten() else { continue };
                    if hovered(i) {
                        selected = Some(d.clone());
                    }
                    let thumb = [fx + 52.0, fy + 56.0, 248.0, 140.0];
                    match game.preview(slot, &d.layers) {
                        Some(t) => out.push(Quad::rect(t, thumb, [1.0; 4])),
                        None => out.push(Quad::rect(white.clone(), thumb, [0.0, 0.0, 0.0, 1.0])),
                    }
                    self.sprite(out, "save_scene_plus", [fx, fy, 960.0, 272.0], Some([32.0, 32.0, 960.0, 272.0]));
                    // "YYYY/MM/DD HH:MM": glyph x from unk_350650 (640 units, relative to the frame)
                    let t: Vec<u32> = save::format_time(d.timestamp).chars().filter_map(|c| c.to_digit(10)).collect();
                    let date = [118.0, 123.5, 129.0, 134.5, 145.5, 151.0, 162.0, 167.5];
                    let time = [182.333, 187.833, 198.833, 204.333];
                    let gy = fy + 60.0;
                    let dd: Vec<(u32, f32)> = t.iter().zip(date).map(|(&d, u)| (d, fx + u * 3.0)).collect();
                    let tt: Vec<(u32, f32)> = t.iter().skip(8).zip(time).map(|(&d, u)| (d, fx + u * 3.0)).collect();
                    self.draw_digits(out, "savedate_number", &dd, gy);
                    self.draw_digits(out, "savetime_number", &tt, gy);
                    // sub_4F240 labels auto save records (>= 100); the list never holds them on Switch
                    if slot >= save::AUTO_FIRST {
                        self.sprite(out, "saveauto", [fx + 744.0, fy + 26.0, 192.0, 32.0], None);
                    }
                    let var = |k: &str| d.vars.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map_or(0, |v| v.1);
                    let phase = var("F023").clamp(0, 37) as usize;
                    let [x0, y0, x1, y1] = SAVE_TITLES[phase];
                    let src = [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32];
                    self.sprite(out, "savetitle", [fx + 359.0, fy + 121.0, src[2], src[3]], Some(src));
                    let scene = usize::try_from(var("F022")).ok().and_then(|t| self.sysmes.get(t + 395)).cloned();
                    if let Some(sc) = scene.filter(|s| !s.trim().is_empty()) {
                        game.font_mut().draw_str(r, out, &sc, 28.0, fx + 423.0, fy + 222.0, [0.1, 0.1, 0.15, 1.0]);
                    }
                }
                // info panel for the selected slot: saveinfobg_{arc,ciel,unknown} by route (phase
                // 11..36 via dword_3506B0) at 453.33,39.41 and the slot's text at 463.33,259.26
                if let Some(d) = selected {
                    let var = |k: &str| d.vars.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map_or(0, |v| v.1);
                    let bg = match usize::try_from(var("F023") - 11).ok().and_then(|k| INFO_ROUTE.get(k)) {
                        Some(0) => "saveinfobg_arc",
                        Some(1) => "saveinfobg_ciel",
                        _ => "saveinfobg_unknown",
                    };
                    self.sprite(out, bg, [1360.0, 95.0, 472.0, 712.0], None);
                    let chars: Vec<char> = d.preview.chars().collect();
                    let font = game.font_mut();
                    for (li, chunk) in chars.chunks(15).take(5).enumerate() {
                        let s: String = chunk.iter().collect();
                        font.draw_str(r, out, &s, 26.0, 1390.0, 655.0 + li as f32 * 36.0, [0.1, 0.1, 0.15, 1.0]);
                    }
                }
                self.fade_quads(out, n0, self.scr_a);
            }
            Screen::Config => {
                if self.stack.contains(&Screen::Menu) {
                    self.draw_menu_layer(out);
                    // the line / dot cursor only shows while the tabs have the focus
                    let cur = self.side.then_some(self.sys_tab);
                    self.draw_menu_base(game, r, out, Some(2), cur);
                } else {
                    self.draw_under(game, r, out);
                }
                let n0 = out.len();
                self.draw_config(game, out, hovered);
                if self.conf_tab == 0 {
                    let n1 = out.len();
                    self.draw_conf_preview(game, r, out);
                    for q in &mut out[n1..] {
                        q.color[3] *= self.conf_ca;
                    }
                }
                self.fade_quads(out, n0, self.scr_a);
                // sub_9B0C4 on the config scroller (qword_AED360): position = scroll / tab range, its own alpha
                if self.conf_sb_a > 0.0 {
                    let (n1, max) = (out.len(), self.conf_max());
                    self.draw_scroll_bar(out, if max > 0.0 { self.conf_scroll / max } else { 0.0 }, 608.0);
                    for q in &mut out[n1..] {
                        q.color[3] *= self.conf_sb_a;
                    }
                }
            }
            // cFlowchart (sub_F8FC / sub_FFFC): flow_bg, the chart scrolled inside 1920x1022, the cursor
            // (flow_cur cell 8,8 104x64) and the cFlowchartScene panel: flow_infobase at 1352,96, the
            // node preview (thumcg_top + 1 + n, 430x243) at 1380,120, the "this scene" tag at
            // 1672,120, and SYSMES title / description at 1392,404 and 1392,452
            Screen::Flowchart { .. } => {
                // over the submenu backdrop (sub_4B880), as the system menu / gallery left it
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
                self.draw_menu_layer(out);
                self.sprite(out, "flow_bg", [0.0, 0.0, W, H], None);
                self.sprite(out, "flow_infobase", [1352.0, 96.0, 488.0, 496.0], None);
                let clip = [[0.0, 0.0], [W, 0.0], [W, FLOW_VIEW], [0.0, FLOW_VIEW]];
                let n0 = out.len();
                if let Some(c) = &self.flow {
                    for sp in &c.sprites {
                        let d = sp.dst;
                        if d[1] - self.flow_scroll > FLOW_VIEW || d[1] + d[3] - self.flow_scroll < 0.0 {
                            continue;
                        }
                        self.sprite(out, sp.tex, [d[0], d[1] - self.flow_scroll, d[2], d[3]], Some(sp.src));
                    }
                }
                // phase titles (sub_11EB4): pinned at y 8 while their section scrolls past, pushed up by the
                // upper divider of the next section change
                if let Some(c) = &self.flow {
                    let off = -self.flow_scroll.trunc();
                    for (p, ts) in c.titles.iter().enumerate() {
                        let (Some(first), Some(last)) = (ts.first(), ts.last()) else { continue };
                        let mut o = off;
                        if first.dst[1] + o < 8.0 {
                            o = 8.0 - first.dst[1];
                        }
                        let bottom = last.dst[1] + o + 32.0;
                        if let Some(&lim) = c.limits.get(p)
                            && lim + off < bottom
                        {
                            o -= bottom - (lim + off);
                        }
                        for t in ts {
                            let d = t.dst;
                            self.sprite(out, t.tex, [d[0], d[1] + o, d[2], d[3]], Some(t.src));
                        }
                    }
                }
                let cur = self.flow.as_ref().and_then(|c| c.nodes.get(self.flow_cur)).map(|n| (n.pos, n.preview, n.name, n.desc));
                if let Some((p, ..)) = cur {
                    self.sprite(out, "flow_cur", [p[0], p[1] - self.flow_scroll, 104.0, 64.0], Some([8.0, 8.0, 104.0, 64.0]));
                }
                for q in &mut out[n0..] {
                    q.clip = Some(clip);
                }
                self.draw_scroll_widget(out, self.flow_scroll, self.flow.as_ref().map_or(0.0, |c| c.scroll_max));
                if let Some((_, preview, name, desc)) = cur {
                    if let Some(img) = game.assets.flow_preview_name(preview).and_then(|n| game.assets.image(r, &n)) {
                        // scale 2/3 in the scene's 1280-wide space: native texels on the canvas
                        let (w, h) = (img.tex.width as f32, img.tex.height as f32);
                        let uv = img.tex.uv_max();
                        out.push(Quad::rect(img.tex.clone(), [1380.0, 120.0, w, h], [1.0; 4]).with_uv_rect(0.0, 0.0, uv[0], uv[1]));
                    }
                    self.sprite(out, "flow_info", [1672.0, 120.0, 144.0, 32.0], None);
                    let text = |i: i32| usize::try_from(i).ok().and_then(|i| self.sysmes.get(i)).cloned().unwrap_or_default();
                    let (name, desc) = (text(name), text(desc));
                    let font = game.font_mut();
                    // sub_16E00: both texts at scale 1.2 (title style 0x4210 = RGB565 colour 66,65,132; description
                    // style 0, black), 33.6 px line pitch; sizes and baselines measured against the Switch
                    font.draw_str(r, out, &name, 24.0, 1392.0, 404.0 + 30.0, [66.0 / 255.0, 65.0 / 255.0, 132.0 / 255.0, 1.0]);
                    // SYSMES marks line breaks with '^'
                    let mut li = 0;
                    for para in desc.split('^') {
                        let chars: Vec<char> = para.chars().collect();
                        for chunk in chars.chunks(16) {
                            let s: String = chunk.iter().collect();
                            font.draw_str(r, out, &s, 24.0, 1392.0, 452.0 + 29.0 + li as f32 * 33.6, [0.0, 0.0, 0.0, 1.0]);
                            li += 1;
                        }
                    }
                }
            }
            Screen::Gallery => {
                let scroll = self.gal_scroll.pos;
                let nb = self.draw_hub_bg(out, &white, Some("cg_title"));
                let n0 = out.len();
                let cur = hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
                let blink = 0.5 + 0.5 * (self.title_clock * 3.0).sin().abs();
                for (k, g) in self.cg_list().into_iter().enumerate() {
                    let rc = Self::cg_cell(k, scroll);
                    if rc[1] + rc[3] < GAL_CLIP[0] || rc[1] > GAL_CLIP[1] {
                        continue;
                    }
                    // thumbnail of the group's first unlocked entry: cg_thumb_all cell (154 * (t % 36), 88 * (t / 36))
                    if let Some(e) = crate::gallery::entry(g, self.cg_unlocked[g][0]) {
                        use crate::gallery::{THUMB_COLS, THUMB_H, THUMB_W};
                        let t = e.thumb as u32;
                        let src = [(t % THUMB_COLS) as f32 * THUMB_W, (t / THUMB_COLS) as f32 * THUMB_H, THUMB_W, THUMB_H - 1.0];
                        self.sprite(out, "cg_thumb_all", rc, Some(src));
                    }
                    if cur == Some(Action::Cg(g)) {
                        // cg_cursor frame (prio 250) and its glow (prio 210) at (x - 8, y - 6)
                        let at = [rc[0] - 8.0, rc[1] - 6.0, 248.0, 144.0];
                        self.sprite(out, "cg_cursor", at, Some([8.0, 8.0, 248.0, 144.0]));
                        self.sprite_c(out, "cg_cursor", at, Some([264.0, 8.0, 248.0, 144.0]), blink, Blend::Add);
                    }
                }
                let clip = [[0.0, GAL_CLIP[0]], [W, GAL_CLIP[0]], [W, GAL_CLIP[1]], [0.0, GAL_CLIP[1]]];
                for q in &mut out[n0..] {
                    q.clip = Some(clip);
                }
                self.draw_scroll_widget(out, scroll, self.gal_scroll_max());
                self.draw_hub_menu(out, &items, hover, Some(0), nb);
            }
            Screen::Music => {
                let scroll = self.gal_scroll.pos.trunc();
                let nb = self.draw_hub_bg(out, &white, Some("music_title"));
                let cur = hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
                // the list cursor row (a1 + 52) stays lit while the panel or the buttons have the focus
                let list_row = self.music_row;
                let n = crate::gallery_table::MUSIC.len();
                let n0 = out.len();
                // music_soundbg (0x34D238): translucent list panel at (392, 106), prio 500
                self.spr(out, "music_soundbg", 392.0, 106.0, None);
                for i in 0..n {
                    let y = 109.0 + i as f32 * 59.0 - scroll;
                    if y + 64.0 < GAL_CLIP[0] || y > GAL_CLIP[1] {
                        continue;
                    }
                    self.spr(out, "music_bgline", 408.0, y + 64.0, None);
                    let ok = self.music_ok.get(i).copied().unwrap_or(false);
                    let id = if ok { crate::gallery_table::MUSIC[i].1 as usize } else { 244 };
                    let title = self.sysmes.get(id).cloned().unwrap_or_else(|| "？？？".into());
                    let c = if self.playing_track == Some(i) {
                        [0.35, 0.9, 1.0, 1.0]
                    } else if cur == Some(Action::Track(i)) {
                        [0.7, 0.95, 1.0, 1.0]
                    } else if ok {
                        [1.0; 4]
                    } else {
                        [0.55, 0.58, 0.68, 1.0]
                    };
                    // sub_1B2F0: every row at half alpha but the cursor row
                    let c = [c[0], c[1], c[2], if i == list_row { 1.0 } else { 0.5 }];
                    game.font_mut().draw_str(r, out, &title, 34.0, 438.0, y + 22.0 + 34.0, c);
                }
                let clip = [[0.0, GAL_CLIP[0]], [W, GAL_CLIP[0]], [W, GAL_CLIP[1]], [0.0, GAL_CLIP[1]]];
                for q in &mut out[n0..] {
                    q.clip = Some(clip);
                }
                self.draw_scroll_widget(out, scroll, MUSIC_SCROLL_MAX);
                // cGalleryMusicPanel (sub_1B7D8)
                self.spr(out, "music_btnbg", 1368.0, 104.0, None);
                let shown = self.playing_track.or(self.last_track);
                if let Some(t) = shown {
                    // sub_1DAB8: title and credit lines (歌 / 作詞 / 作曲 / 編曲 + name) centred on x 1600 at
                    // scale 1.2, starting at 260 - 32 * (lines / 2) - 16; the title is followed by a 58px gap
                    let credits = crate::gallery_table::MUSIC_CREDITS[t];
                    let lines = 1 + credits.iter().filter(|&&c| c >= 0).count();
                    let mut y = 260.0 - 32.0 * (lines / 2) as f32 - 16.0;
                    let size = 29.0;
                    let font = game.font_mut();
                    let mut centred = |s: &str, y: f32| {
                        let w = font.measure(s, size);
                        font.draw_str(r, out, s, size, 1600.0 - w / 2.0, y + size, [1.0; 4]);
                    };
                    let title = self.sysmes.get(crate::gallery_table::MUSIC[t].1 as usize).cloned().unwrap_or_default();
                    for (k, part) in title.split('^').enumerate() {
                        centred(part.trim(), y + k as f32 * size);
                    }
                    y += size * title.split('^').count() as f32 + 58.0;
                    for (k, &c) in credits.iter().enumerate() {
                        if c >= 0 {
                            let label = self.sysmes.get(245 + k).cloned().unwrap_or_default();
                            let name = self.sysmes.get(c as usize).cloned().unwrap_or_default();
                            centred(&format!("{label} {name}"), y);
                            y += 32.0;
                        }
                    }
                }
                for (k, &(x, y)) in MUSIC_CTL.iter().enumerate() {
                    let playing = self.playing_track.is_some();
                    let active = (k == 0 && playing && !self.music_paused) || (k == 1 && self.music_paused) || (k == 3 && self.music_repeat);
                    let hov = cur == Some(Action::MusicCtl(k as u8));
                    let sy = if active { 184.0 } else if hov { 96.0 } else { 8.0 };
                    self.spr(out, "music_controlbtn", x, y, Some([8.0 + 96.0 * k as f32, sy, 88.0, 80.0]));
                }
                // sub_1C184 / sub_1C2B0: "%2d:%02d" elapsed time and "%3d" volume
                let len = shown.map_or(0.0, |t| crate::gallery_table::MUSIC[t].2 as f32 / 1000.0);
                let el = if self.playing_track.is_some() { self.music_clock } else { 0.0 };
                let font = game.font_mut();
                font.draw_str(r, out, &format!("{:2}:{:02}", el as u32 / 60, el as u32 % 60), 30.0, 1548.0, 503.0 + 30.0, [1.0; 4]);
                let vol = format!("{}", (self.music_vol * 100.0).round() as u32);
                let right = 1562.0 + font.measure("100", 30.0);
                let w = font.measure(&vol, 30.0);
                font.draw_str(r, out, &vol, 30.0, right - w, 878.0 + 30.0, [1.0; 4]);
                let blink = 0.5 + 0.5 * (self.title_clock * 3.0).sin().abs();
                for (y, frac) in [(424.0, if len > 0.0 { (el / len).min(1.0) } else { 0.0 }), (792.0, self.music_vol)] {
                    self.spr(out, "music_slider", 1392.0, y, None);
                    let kx = 1376.0 + frac * 364.0;
                    self.spr(out, "music_soundbtn", kx, y - 24.0, Some([8.0, 8.0, 72.0, 72.0]));
                    if y > 500.0 && cur == Some(Action::MusicVol) {
                        self.sprite_c(out, "music_soundbtn", [kx, y - 24.0, 72.0, 72.0], Some([168.0, 8.0, 72.0, 72.0]), blink, Blend::Add);
                    }
                }
                self.draw_hub_menu(out, &items, hover, Some(1), nb);
            }
            Screen::Movies => {
                // movie_cur cells: rows prologue 312, OP1 8, OP2 160; columns normal / hover / pressed
                let nb = self.draw_hub_bg(out, &white, Some("movie_title"));
                for (i, (rc, a)) in items.iter().enumerate() {
                    if let Action::Movie(m) = a {
                        let sy = [312.0, 8.0, 160.0][*m];
                        let sx = if hovered(i) { 264.0 } else { 8.0 };
                        self.sprite(out, "movie_cur", *rc, Some([sx, sy, 248.0, 144.0]));
                    }
                }
                self.draw_hub_menu(out, &items, hover, Some(3), nb);
            }
            Screen::MoviePlay => {}
            Screen::Endings => {
                use crate::game::get_bit;
                let nb = self.draw_hub_bg(out, &white, Some("ending_title"));
                let cur = hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
                if Self::all_endings(game) {
                    let sx = if cur == Some(Action::Ending(24)) { 128.0 } else { 8.0 };
                    self.spr(out, "ending_btn", 1702.0, 894.0, Some([sx, 8.0, 112.0, 112.0]));
                }
                for e in 0..24 {
                    let ok = get_bit(&game.nodes, crate::gallery::ENDINGS[e].0);
                    let hov = cur == Some(Action::Ending(e));
                    if e < 3 {
                        let icon = if ok { ["ending_icon_Arcueid", "ending_icon_Ciel_1", "ending_icon_Ciel_2"][e] } else { "ending_icon_non" };
                        self.spr(out, icon, 416.0 + e as f32 * 480.0, 120.0, None);
                        if ok {
                            let sx = if hov { 128.0 } else { 8.0 };
                            self.spr(out, "ending_btn", 720.0 + e as f32 * 480.0, 128.0, Some([sx, 8.0, 112.0, 112.0]));
                        }
                    } else {
                        // ending_badend plates: (b & 7) * 180 + 8, row * 96 + 296 when reached, + 8 otherwise
                        let b = e - 3;
                        let (x, y) = ((b & 7) as f32 * 180.0 + 384.0, (b >> 3) as f32 * 96.0 + 680.0);
                        let y0 = if ok { 296.0 } else { 8.0 };
                        self.spr(out, "ending_badend", x, y, Some([(b & 7) as f32 * 180.0 + 8.0, (b >> 3) as f32 * 96.0 + y0, 180.0, 88.0]));
                        if ok {
                            let sx = if hov { 104.0 } else { 8.0 };
                            self.spr(out, "ending_badendbtn", x, y, Some([sx, 8.0, 88.0, 88.0]));
                        }
                    }
                }
                self.draw_hub_menu(out, &items, hover, Some(4), nb);
            }
            Screen::CgView { group, pos } => {
                // cGalleryCGSub (sub_BA18): the gallery below at 80% brightness (0x466738), then the picture
                self.draw_under(game, r, out);
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 0.2]));
                let e = self.cg_unlocked.get(group).and_then(|v| v.get(pos)).and_then(|&i| crate::gallery::entry(group, i));
                if let Some(e) = e
                    && let Some(img) = game.assets.image(r, e.name)
                {
                    let rc = self.cg_rect(e.width.max(1) as f32, e.height.max(1) as f32);
                    let uv = img.tex.uv_max();
                    out.push(Quad::rect(img.tex.clone(), rc, [1.0; 4]).with_uv_rect(0.0, 0.0, uv[0], uv[1]));
                }
            }
            // backlog (sub_27D10 / sub_28470): black at 75% over the scene, one history page in the
            // message layout, and the scroll widget (sub_9B0C4: scroll_bar at 615.67,43.56 and
            // scroll_slider running 41.07..293.27 at x 613, 640x448 units)
            Screen::Backlog { scroll } => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 0.75]));
                out.extend(game.backlog_quads(r, scroll));
                let n = game.backlog_len();
                self.draw_scroll_bar(out, if n > 1 { 1.0 - scroll as f32 / (n - 1) as f32 } else { 1.0 }, 608.0);
            }
            Screen::Confirm(c) => {
                // sub_4AE90: the screen below, dialog_BG at 75% and dialog_win popping in (scale
                // (1 - (1 - t) * 0.2)^2, alpha t), then the message (y 184 of 448) and the buttons
                self.draw_under(game, r, out);
                let t = self.dlg_t;
                self.sprite_c(out, "dialog_BG", [0.0, 0.0, W, H], None, t * 0.75, Blend::Alpha);
                self.draw_dialog_win(out, t);
                if t >= 1.0 {
                    let msg = self.sysmes.get(c.message().0).cloned().unwrap_or_default();
                    self.draw_dialog_text(game, r, out, &msg, 443.6);
                    for (i, (rc, a)) in items.iter().enumerate() {
                        // dialog_btn_ja frames: Yes (224,0) / glow (0,135), No (224,67) / glow (0,202)
                        let y = if *a == Action::Yes { 0.0 } else { 67.0 };
                        let src = if hovered(i) { [0.0, y + 135.0, 216.0, 56.0] } else { [224.0, y, 216.0, 56.0] };
                        self.sprite(out, "dialog_btn_ja", *rc, Some(src));
                    }
                }
                return;
            }
        }
        // button guide: cGalleryGuide (sub_5920 / sub_5B58) in the gallery, sub_4A8F0 elsewhere
        let guide = match screen {
            Screen::Movies | Screen::Endings => Some(10),
            // the mode buttons: 10; the content: its own (cGalleryGuide via vt + 136; music panel 10, list 0)
            s if Self::gal_mode(s).is_some() && self.gal_side => Some(10),
            Screen::Gallery => Some(if self.gal_scroll_max() > 0.0 { 0 } else { 10 }),
            Screen::Music if matches!(self.hover.and_then(|h| self.items(game).get(h).map(|(_, a)| *a)), Some(Action::MusicCtl(_) | Action::MusicVol)) => Some(10),
            Screen::Flowchart { .. } => Some(if self.flow.as_ref().is_some_and(|c| c.scroll_max > 0.0) { 0 } else { 10 }),
            Screen::CgView { .. } => Some(9),
            Screen::Music | Screen::SaveLoad { .. } => Some(0),
            Screen::Config => Some(2),
            Screen::Backlog { .. } => Some(3),
            _ => None,
        };
        if self.under {
            return;
        }
        if let Some(g) = guide {
            let n = out.len();
            self.draw_guide(out, &white, g);
            self.fade_quads(out, n, self.scr_a);
        }
        if !self.under {
            self.draw_banner(out);
            self.draw_help(game, r, out);
        }
        // sub_48540: message popup, dialog_win with the text at y 214 of 448
        if let Some(p) = self.popup {
            self.draw_dialog_win(out, p.a);
            // the text appears once the window is fully open
            if p.a >= 1.0 {
                let msg = self.sysmes.get(p.id).cloned().unwrap_or_default();
                self.draw_dialog_text(game, r, out, &msg, 515.8);
            }
        }
        if self.black > 0.0 && !self.black_under {
            let [cr, cg, cb] = self.black_col;
            out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [cr, cg, cb, self.black.min(1.0)]));
        }
    }
}
