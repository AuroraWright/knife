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
mod witch;

pub use pad::Pad;

const W: f32 = CANVAS_W as f32;
const H: f32 = CANVAS_H as f32;
const SLOTS_PER_PAGE: usize = 4;
/// Baseline of a text slot in the shared system font, as a ratio of the glyph height below the slot's y:
/// Switch (music panel title, credits, time and volume at 29.87 / 24.89 px glyphs, the help balloon at
/// 22.2 px; Chinese config labels within 0.1 px) 1.156: the system pair font's ascent at 40 px, the standard
/// font's 45 (1158 of 1024), + 1.24.
const SYS_ASCENT: f32 = 1.156;

/// sub_A7790's glyph scale of the language (English 0.9): text slot baselines and pitches follow the glyphs.
fn gs() -> f32 {
    crate::text::glyph_scale()
}
/// Canvas distance between save list frames (`save_frame`).
pub const SAVE_PITCH: f32 = 93.748 * H / 448.0;
/// Trackpad travel (canvas px on screen) that scrolls the save list a row.
pub const SAVE_WHEEL_ROW: f32 = 1.5 * SAVE_PITCH;
/// Trackpad travel (canvas px on screen) that steps Witch on the Holy Night's backlog an entry: 1.5 entry pitches
/// (116 units), as the save list's.
const WITCH_LOG_ROW: f32 = 1.5 * 116.0 * H / 448.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Confirm {
    Title,
    Overwrite(usize),
    Load(usize),
    /// flowchart rewind to the record (table row, id, scene id) under the cursor (dialog 26)
    Jump(usize, i32, i32),
    /// mode panel: previous scene, next scene, quick load (dialogs 11, 10, 9)
    PrevScene,
    NextScene,
    QuickLoad,
    Ending(usize),
    /// config Default (dialog 27)
    Defaults,
    /// backlog quick jump (dialog 23)
    BacklogJump,
    /// system menu FlowChart during a gallery flowchart replay: end the replay (dialog 12)
    EndReplay,
    /// overseas save list X: delete the save (dialog 21)
    Delete(usize),
}

impl Confirm {
    /// Panel quick load (sub_4E170 case 6): the dialog shows the quick save's thumbnail (sub_4B4A0(1)).
    fn thumbnail(self) -> bool {
        self == Confirm::QuickLoad
    }

    /// SYSMES message (sub_49710 -> table dword_3C9340) and whether the cursor starts on "No"
    /// (sub_4AD40's second argument selects the initial button; 1 = No).
    fn message(self) -> (usize, bool) {
        let (dialog, no) = match self {
            Confirm::Jump(..) => return (1497, false),
            Confirm::Ending(i) => return (crate::gallery::ENDINGS[i].2, true),
            Confirm::Defaults => return (160, true),
            c => c.dialog().unwrap_or_default(),
        };
        // sub_49710 -> sub_9A8C0(0, n): Witch on the Holy Night's own table (dword_2868A0)
        let table: &[usize] = if crate::assets::witch() { &WITCH_DIALOG_SYSMES } else { &DIALOG_SYSMES };
        (table[dialog], no)
    }

    /// The dialog id (sub_49710's row) and whether the cursor starts on "No"; None for the messages that are
    /// not rows of the table.
    fn dialog(self) -> Option<(usize, bool)> {
        Some(match self {
            // sub_4CEA0 cases 3 / 4 open dialogs 12 and 8 on Yes (sub_49710(n, 0))
            Confirm::Title => (8, false),
            Confirm::EndReplay => (12, false),
            Confirm::Overwrite(_) => (2, true),
            // sub_50FC0 load mode: sub_49710(0, 0), Yes first (Switch: title LOAD GAME)
            Confirm::Load(_) => (0, false),
            Confirm::PrevScene => (11, true),
            Confirm::NextScene => (10, true),
            Confirm::QuickLoad => (9, true),
            Confirm::BacklogJump => (23, true),
            // sub_50FC0 (overseas): sub_49710(21, 1)
            Confirm::Delete(_) => (21, true),
            Confirm::Jump(..) | Confirm::Ending(_) | Confirm::Defaults => return None,
        })
    }
}

/// SYSMES message per dialog id (sub_49710: dword_3C9340; 0 where the table has -1).
const DIALOG_SYSMES: [usize; 24] =
    [179, 178, 180, 199, 198, 200, 211, 194, 229, 230, 232, 233, 234, 0, 235, 0, 0, 0, 0, 0, 0, 182, 181, 231];
/// Witch on the Holy Night (dword_2868A0).
const WITCH_DIALOG_SYSMES: [usize; 24] =
    [155, 154, 156, 175, 174, 176, 187, 170, 204, 205, 207, 208, 209, 0, 210, 0, 0, 0, 0, 0, 0, 158, 157, 206];

/// Boot and caution screens: the logo movie and notice (sub_B5650), and the title's PRESS ANY BUTTON
/// follow-up (sub_8E200 states 10..20). Alphas run at the exe's per-frame speeds (60 Hz).
/// A menu cursor snapshot (`Ui::cursor`).
#[derive(Clone, Copy, PartialEq)]
struct Cursor {
    screen: Screen,
    rest: (usize, bool, bool, usize, crate::flow::Cell, usize, usize, usize),
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
    /// a Witch on the Holy Night system screen (`witch`: the task in `Ui::w`)
    Witch,
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
    /// a gallery replay ended and the screen is black: back to the gallery (sub_47A90)
    GalleryBack,
    /// the story faded to black on its way to the title (sub_48AC0 case 2 state 2)
    TitleNow,
    OpenGallery,
    Cg(usize),
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
    /// A or a tap on the backlog page: ask to jump back to it (sub_28A90)
    BacklogJump,
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

/// A confirm dialog answered (sub_49F00 states 3 / 4): it stays `hold` frames (15 after A, 1 after B), then
/// dword_B34B54 clears, the caller acts and the dialog fades out over the screen below at 0.07 per frame
/// (sub_4AE90: shrinking after Yes, cursor 0 in qword_B34B58, growing after No).
#[derive(Clone, Copy)]
struct DlgOut {
    c: Confirm,
    yes: bool,
    hold: f32,
    t: f32,
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
    /// the cursor each `stack` entry had when covered (same index): the save list keeps its slot lit under a
    /// dialog or popup and after it (sub_50FC0 leaves the list objects as they were)
    stack_hover: Vec<Option<usize>>,
    hover: Option<usize>,
    mouse: (f32, f32),
    tex: HashMap<String, TexRef>,
    /// savedata.bin
    pub file: SaveFile,
    slots: Vec<Option<SaveData>>,
    /// message popup (sub_4ABE0 / sub_49750)
    popup: Option<Popup>,
    /// frames since a successful save write (sub_50FC0 states 8 / 9): sound 1 as the popup closes at 30, the
    /// list is rebuilt 41 frames after that; input waits meanwhile
    save_wait: Option<f32>,
    /// overseas save deletion (sub_50FC0 states 23..25): frames since the record went
    delete_wait: Option<f32>,
    /// dword_AFFCC4: frames left before the next save write (sub_40E70: 0 at boot, 180 after a quick save, the
    /// system menu's auto save and, overseas, a deletion). sub_48AC0 counts it down each frame outside the
    /// title (off_AE04E8 2 / 3: the story, its menus and the title's load list). While it runs (sub_40E90)
    /// the panel's quick save plays sound 2, the save list ignores A on an entry and the auto save
    /// (sub_52AA0) only reaches the in-memory records, without a file write or banner.
    save_cool: u32,
    /// dword_F1F460: the save list's cursor flash, set to 1 when a touch, the pad or the scroller moves the
    /// cursor (sub_50FC0 state 2, not the list rebuilds); sub_4F240 draws the selected frame again over the
    /// cursor entry with blend 30 at that times the page alpha and lowers it by 0.2 per draw
    save_flash: f32,
    /// the save list's (page, cursor, scroll) last frame, to see the cursor moves that light the flash
    save_seen: Option<(bool, Option<usize>, usize)>,
    /// text slot 38, the info panel's notes: sub_50B10 lays out the cursor entry's text on opening, cursor
    /// moves and list rebuilds only, so while a save is written into the free entry (state 7) the panel
    /// shows the new record with the notes of the entry the cursor had (none for the free entry)
    save_notes: Option<String>,
    /// the slot being saved until the list is rebuilt (sub_50FC0 state 9): its record already shows the new
    /// save (state 7 copies it into the list records), its old thumbnail stays until then
    save_pending: Option<usize>,
    /// the save list's order while a save is pending: sub_50790 only rebuilds it in state 9
    save_order: Option<Vec<usize>>,
    /// confirm dialog open animation, 0..1
    dlg_t: f32,
    /// the confirm dialog after A / B (sub_49F00)
    dlg_out: Option<DlgOut>,
    title_movie: bool,
    /// the touch back button (sub_4A7A0, object 81 sliding in from x 1920 to 1640 at y 952): shown 0..1
    /// (jd_B34B38, 0.04 a frame while a screen has it on with sub_4A790(1)); docked it is drawn at alpha 0
    /// (sub_FBED4 == 1), but its tap zone still works
    back_btn: f32,
    /// the title was up last frame: entering it (sub_8D2E0) and leaving it (sub_90180) step the title stage
    on_title: bool,
    complete: Option<Complete>,
    /// unlocked entries per CG gallery group, refreshed when the gallery opens
    cg_unlocked: Vec<Vec<usize>>,
    music_ok: Vec<bool>,
    playing_track: Option<usize>,
    /// music player: seconds into the playing track, volume 0..1 (dword_469C10, 0.6 at boot), repeat toggle
    /// (byte_AE5868, off at boot); both stay for the session
    music_clock: f32,
    music_vol: f32,
    music_repeat: bool,
    music_paused: bool,
    /// without repeat, the track fading out at its end (sub_1D314): seconds left until it stops
    music_fade: Option<f32>,
    /// music list panel offset (sub_1B078): with the list offset o = -scroll, o itself while o >= -200,
    /// o + 4265 - 326 below -4465, and otherwise left where it was last set
    music_bg_off: f32,
    /// CG viewer zoom (0 = fit) and pan offset
    cg_zoom: f32,
    cg_pan: [f32; 2],
    /// size of the picture on view (for the Minus fill zoom)
    cg_size: [f32; 2],
    /// cGalleryCGPhoto transition (sub_C368 states 3 / 4 / 5) and its seconds: see `CgFx`
    cg_fx: CgFx,
    cg_t: f32,
    /// cGalleryGuide sub_604C / sub_5EA4: phase (0 hidden, 1 fading in, 2 shown, 3 fading out) and its seconds
    cg_guide: (u8, f32),
    /// right stick y (-128..128, up) for the CG viewer zoom; the right stick's directions are the d-pad's
    pub rstick: i32,
    /// drawing the screen below a dialog / viewer (no guide or popup)
    under: bool,
    last_track: Option<usize>,
    /// SYSMES_TEXT strings (music titles), loaded on first use
    sysmes: Vec<String>,
    /// help_txt word cells of the language (CONTHELP_TXT_UV, jp_9A710), and that language
    guide_words: (usize, Vec<[u16; 4]>),
    /// seconds since the title screen appeared (logo / menu fade-in)
    title_clock: f32,
    /// CG grid cursor group and the title_clock when it landed there (its glow flash restarts on each move)
    cg_flash: (Option<usize>, f32),
    /// cGalleryCG + 80: the variations grid (cGalleryCGSub, sub_D408) of this group, None for the group grid
    /// (cGalleryCGMain); grid entries (`Action::Cg`) are groups in the one and unlocked pictures in the other
    cg_sub: Option<usize>,
    /// the grid being replaced, fading out (sub_A4F4: 0.5 s, ease-out quad): (variations group, scroll, cursor)
    cg_old: Option<(Option<usize>, f32, Option<usize>)>,
    /// time since the shown grid was made (sub_9970 fades it in over 0.5 s, ease-in quad; input waits for it)
    cg_in: f32,
    /// cGalleryCG + 64 / + 56 / + 60 (sub_AEC8 on a pick): the group picked in the group grid and that grid's
    /// scroll, and the variations grid's scroll when a picture was opened from it
    cg_pick: (Option<usize>, f32, f32),
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
    /// its length (ms): 1/6 s, 0.2 s while the scroll bar is dragged
    flow_len: f32,
    /// cursor cell (record, shown row; cFlowchart + 56 / + 60), which can be a record without a node
    flow_cell: flow::Cell,
    /// chart y of the cursor sprite: moved only onto records with a node (sub_10BA4), 0 before that
    flow_cur_y: f32,
    /// cFlowchart + 52 as a scroll: the shown scroll (truncated) each frame (sub_11EB4), the unclamped target
    /// after a cursor follow (sub_10D5C); the follow, manual scrolls and sub_12D1C measure from it
    flow_ref: f32,
    /// a manual scroll (stick, touch drag, scroll bar) is running: the cursor follows the view (sub_12D1C)
    /// instead of the view the cursor (cFlowchart + 36 bit 5)
    flow_manual: bool,
    /// the cursor's node and when it last changed (sub_10BA4): the glow flash and the panel crossfade
    flow_flash: (Option<usize>, f32),
    /// the node the panel showed before, fading out (sub_16E00)
    flow_old: Option<usize>,
    /// the list touch drag (sub_40C, `TouchDrag`): flowchart, CG grid, music list, config
    drag: touch::TouchDrag,
    /// the scroll widget's slider held by the touch (sub_27974): touch y and shown scroll at the grab
    bar: Option<(f32, f32)>,
    /// the chart waits to open (gallery or not) while what is under it fades out
    flow_open: Option<bool>,
    /// system menu content alpha around the chart (sub_4D8C0 dword_B359F4: states 14 / 16, 1/12 per frame)
    menu_a: f32,
    /// the movie gallery's player (cGalleryMoviePlayer) on Screen::MoviePlay
    gal_movie: Option<GalMovie>,
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
    /// where `black_then` fires: 1.2 for the title's fades, 1.0 for the story's (sub_48AC0 case 2 states 2 / 3 / 7)
    black_top: f32,
    /// off_AE0288: [0] the gallery mode cGallery closed with (sub_82E4; 2 = the flowchart, sub_50F0),
    /// [1] the scene / node its last replay started from, [2] the mode before the flowchart (sub_5040)
    gal_ret: [i32; 3],
    /// cGalleryMenu + 60: the mode shown before the current one
    gal_prev: i32,
    /// The mode the next gallery open starts on (off_AE0288[0] taken by cGallery state 0).
    gal_first: i32,
    /// a replay picked in the gallery waits for the gallery to fade out: (scene id, node, mode before)
    replay_pending: Option<(i32, i32, i32)>,
    /// colour of that overlay (NEW GAME fades to white)
    black_col: [f32; 3],
    /// CONTINUE: "ロード中です" is opening; then the title fades out and this slot loads
    cont_slot: Option<usize>,
    /// a confirmed load (sub_50FC0 state 18 -> 19): "ロード中です" (184) opens, and once it is fully shown
    /// (sub_4AD30) the record loads and the popup closes
    load_popup: Option<usize>,
    /// a load picked in the title's load window: the window closes first (sub_50FC0 state 19 -> 5 -> 27)
    load_after_close: Option<usize>,
    /// Witch on the Holy Night's title (`witch::title`)
    wt: witch::WitchTitle,
    /// The other WoH screens (`witch`)
    w: witch::WUi,
    /// the WoH title starts over at the next update (jp_8D2E0 on the mode manager's return to mode 0)
    wt_reinit: bool,
    /// a button or touch for a WoH splash state
    wt_any: bool,
    /// the record the WoH title loads after its fade (states 21 / 25 -> 23)
    wt_loaded: Option<usize>,
    /// an in-game load is applied and the list fades out (sub_50FC0 states 5 -> 27); then the system menu cuts
    /// away at once (sub_4D340 result 2: backdrop speed -0.0333, then sub_49630(0, 0) zeroes it)
    load_cut: bool,
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
    /// the backlog is closing for a quick jump
    backlog_jump: bool,
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
    /// a stick / drag / slider scroll of the CG grid or music list is running (sub_9CBC / sub_1A7FC bit 5)
    gal_track: bool,
    /// music panel button to come back to (a1 + 60) and the one the volume slider returns to (a1 + 64)
    music_btn: usize,
    music_vol_from: usize,
    /// music list cursor row (cGalleryMusicList a1 + 52), kept while the panel or the buttons have the focus
    music_row: usize,
    /// the key event being handled is an auto-repeat (the exe wraps lists only on the trigger set)
    nav_repeat: bool,
    /// left stick, x / y in -128..128 with y up (sub_B37D0 sums every controller into pad +28 / +30), from the
    /// controllers and WASD
    pub stick: [i32; 2],
    /// 60 Hz frames owed to `stick_frame`
    stick_tick: f32,
    /// the save list's stick accumulator (sub_50FC0 dword_F1F478)
    stick_acc: i32,
    gal_guide: GalGuide,
    /// stick frames the mouse wheel / trackpad still holds the left stick for (+ up)
    wheel: f32,
    /// wheel frames toward the next flowchart stick press
    wheel_press: f32,
    /// wheel notches toward the next save list step (the list steps by notch, not by a held tilt)
    wheel_steps: f32,
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
    /// the shared system font for text slots (`sys_font`)
    font_sys: Option<FontAtlas>,
    /// system menu tab under the cursor (HIDWORD(qword_B359EC)): kept across menu invocations and title
    /// visits (set to 0 once at boot, sub_4CA50); the line / dot cursor marks it
    sys_tab: usize,
    /// the UI focus object (qword_19150EC, sub_AE0E0) is that tab: the tab is drawn highlighted with its
    /// help balloon. sub_4CEA0 sets it when the cursor moves or is tapped and a return from a submenu
    /// restores it (states 9 / 12 / 16); every other screen (the title, a choice, a submenu's page) takes
    /// the focus for its own objects, so the first menu after starting from the title shows none.
    sys_focus: bool,
    /// config cursor per tab (dword_46CDB0[628 * tab + 2] row, -1 the tab bar, and +550 + row column),
    /// kept for the whole run and shared by the title's config: sub_2B010 starts tabs 0 / 1 on their first
    /// object (55 / 78, the first slider's minus) and the controls tab on the tab bar
    conf_cur: [Option<(usize, usize)>; 3],
    /// title cursor entry (dword_65CBD4), kept while a submenu or dialog is open over the title
    title_cur: Option<Action>,
    /// focus is on the system menu tabs while one of its submenus stays open (sub_4D580 -> sub_4CEA0),
    /// and the submenu item to return to
    side: bool,
    sub_hover: Option<usize>,
    /// config tab bar cursor (qword_AED328: tabs 51..53, default 54) as an index into the tab bar
    conf_bar: usize,
    /// qword_46F810: the SE channel of the config's SE test, and the voice row whose test was pressed last
    /// (sub_2B680 clears both on opening; sub_2B5C0 stops the test SE and the test voice on closing)
    conf_test_se: Option<i32>,
    conf_test_row: Option<usize>,
    /// dword_46F81C: frames a held - / + waits before it repeats (20 from the press, then a step every frame)
    conf_hold: u32,
    /// overseas language change (sub_2D720 / sub_2D910's dword_9D3A44 steps) and the button it shows picked
    lang_seq: Option<LangSeq>,
    lang_pick: Option<usize>,
    /// dword_9D3A60: config input waits these frames (counted down once no capture shows)
    lang_cool: u32,
    /// time towards the next 60 Hz step of `lang_seq` / `lang_cool`
    lang_ms: f32,
    /// a held control changed a setting; the file is written once the hold ends
    conf_hold_dirty: bool,
    /// the confirm button is held (a held A with left / right drags a config slider)
    pub enter_held: bool,
    /// controller buttons held (bit per `Pad`)
    pub pads_held: u32,
    /// touch ripples, story gestures and the mode panel
    touch: touch::Touch,
}

/// The shared system fonts as one atlas (sub_CAA50's set): the first font, the rest filling in its missing glyphs.
pub(crate) fn shared_font_atlas(r: &Renderer, fonts: Vec<Vec<u8>>) -> Option<FontAtlas> {
    let mut fonts = fonts.into_iter();
    let mut atlas = FontAtlas::new(r, fonts.next()?).map_err(|e| tracing::warn!("system font: {e:#}")).ok()?;
    for d in fonts {
        atlas.add_fallback(d).unwrap_or_else(|e| tracing::warn!("system font fallback: {e:#}"));
    }
    Some(atlas)
}

fn rect_hit(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.0 < r[0] + r[2] && p.1 >= r[1] && p.1 < r[1] + r[3]
}

/// Config scroll view (x, y, w, h) in canvas pixels (sub_B2E60(407, 165, 1402, 858)).
const CONF_VIEW: [f32; 4] = [407.0, 165.0, 1402.0, 858.0];
/// Scroll range per tab in canvas pixels (qword_46F810 + 16).
/// Config scroll range of the system tab (0x46F820); the sound tab's follows its rows (conf_max).
const CONF_MAX_SYSTEM: f32 = 485.0;
/// The overseas system tab's range (0x35EF00): the language row sits below the radios.
const CONF_MAX_SYSTEM_EN: f32 = 750.0;
/// Overseas btn_base1 cells (x0, y0, x1, y1) of the radio objects 69 / 70 / 72..77 (frames 0x35EA98..) for
/// columns normal / hover / selected, drawn without offset; `english` takes sub_2BFE0's widths for objects
/// 74 (normal only) / 75 / 77 (241 / 441 / 448..889 / 896..1337 against 240 / 256..496 / 512..752).
fn en_radio_cell(k: usize, v: bool, col: usize, english: bool) -> [f32; 4] {
    let c = col as f32;
    let narrow = |y: f32| [256.0 * c, y, 256.0 * c + 240.0, y + 48.0];
    let wide = |y: f32| match col {
        0 => [0.0, y, 441.0, y + 48.0],
        1 => [448.0, y, 889.0, y + 48.0],
        _ => [896.0, y, 1337.0, y + 48.0],
    };
    match (k, v) {
        // the sound tab's voice radios (objects 95..98) share the first pair's frames
        (0 | 4 | 5, true) => [256.0 * c, 16.0, 256.0 * c + 120.0, 64.0],
        (0 | 4 | 5, false) if col == 2 => [512.0, 100.0, 632.0, 148.0],
        (0 | 4 | 5, false) => [256.0 * c, 104.0, 256.0 * c + 120.0, 152.0],
        (1, true) => [256.0 * c, 192.0, 256.0 * c + 248.0, 240.0],
        (1, false) => [256.0 * c, 280.0, 256.0 * c + 248.0, 328.0],
        (2, true) if col == 0 => [0.0, 368.0, if english { 241.0 } else { 240.0 }, 416.0],
        (2, true) => [256.0 * c, 368.0, 256.0 * c + 248.0, 416.0],
        (2, false) if english => wide(456.0),
        (2, false) => narrow(456.0),
        (3, true) => narrow(544.0),
        (_, false) if english => wide(632.0),
        _ => narrow(632.0),
    }
}

/// Overseas btn_base2 cells of the check objects 57 / 61 (row 0) and 71 (row 80) (frames 0x35EFF4..): 288
/// wide from x 8 + 304 col, or for English 392 wide from 400 col (sub_2BFE0).
fn en_toggle_cell(t: usize, col: usize, english: bool) -> [f32; 4] {
    let y = if t < 2 { 0.0 } else { 80.0 };
    let c = col as f32;
    if english { [400.0 * c, y, 400.0 * c + 392.0, y + 72.0] } else { [8.0 + 304.0 * c, y, 296.0 + 304.0 * c, y + 72.0] }
}
/// Config scroll tween length (sub_9AD04: 12 frames).
const CONF_SCROLL_MS: f32 = 200.0;
/// Voice setting rows (conf_faceicon / conf_name entries).
const VOICE_ROWS: usize = 19;

/// A language change of the overseas config: the dword_9D3A44 step (0: sub_2D720 still to run) and the
/// config word picked.
#[derive(Clone, Copy, Debug)]
struct LangSeq {
    state: u32,
    word: i16,
    /// pressed this frame
    fresh: bool,
}

/// The overseas config's language button texture (conf_parts index 19, object image 1242), whatever its name.
const CONF_LANG_TEX: &str = "conf_parts#19";

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
    /// overseas language buttons (objects 78..81, type 10): languages 0..3
    Lang(usize),
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
            Ctl::Lang(i) => 800 + i,
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
            800..=803 => Ctl::Lang(c - 800),
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

/// English conf_name cells (overseas sub_2D040 with language 1: table 0x2319E4 in a 504x624 sheet): names 3, 6,
/// 10 and 17 take wider cells below the others; every other language keeps the 0x34D5A0 table.
const NAME_RECTS_EN: [[f32; 4]; VOICE_ROWS + 1] = {
    const T: [[i16; 4]; VOICE_ROWS + 1] = [
        [32, 32, 168, 80], [32, 96, 168, 144], [32, 160, 168, 208], [32, 480, 232, 528], [32, 288, 168, 336],
        [32, 352, 168, 400], [248, 480, 448, 528], [184, 32, 320, 80], [184, 96, 320, 144], [184, 160, 320, 208],
        [32, 544, 232, 592], [184, 288, 320, 336], [184, 416, 320, 464], [184, 352, 320, 400], [336, 32, 472, 80],
        [336, 96, 472, 144], [336, 160, 472, 208], [248, 544, 448, 592], [336, 352, 472, 400],
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
/// The overseas build's savetitle rects (sub_4F240: off_351878 for English, off_351870 otherwise; the latter
/// is the Japanese table with phases 16 and 18 widened to x1 571).
const SAVE_TITLES_EN: [[i32; 4]; 38] = [
    [0, 0, 1, 1], [64, 112, 504, 152], [64, 56, 504, 96], [64, 167, 504, 207], [64, 167, 504, 207],
    [64, 223, 504, 263], [64, 223, 504, 263], [64, 279, 504, 319], [64, 279, 504, 319], [64, 335, 504, 375],
    [64, 335, 504, 375], [64, 390, 504, 430], [64, 390, 504, 430], [522, 56, 1222, 96], [522, 167, 1222, 207],
    [522, 112, 1222, 152], [522, 223, 1222, 263], [522, 279, 1222, 319], [522, 335, 1222, 375],
    [522, 390, 1222, 430], [1241, 112, 1669, 152], [1241, 56, 1669, 96], [1241, 223, 1669, 263],
    [1241, 167, 1669, 207], [64, 446, 504, 486], [1241, 335, 1669, 375], [1241, 279, 1669, 319],
    [1241, 390, 1669, 430], [1688, 56, 2168, 96], [1688, 112, 2168, 152], [1688, 167, 2168, 207],
    [1688, 279, 2168, 319], [1688, 223, 2168, 263], [1688, 223, 2168, 263], [1688, 223, 2168, 263],
    [1688, 335, 2168, 375], [1688, 390, 2168, 430], [0, 0, 1, 1],
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
/// The overseas phase title cells (dword_5912E8 + 232 words) for English: the English texture's wider cells.
const PHASE_TITLES_EN: &[[i32; 4]] = &[
    [0, 0, 1, 1], [2633, 698, 3541, 798], [24, 26, 1260, 126], [24, 122, 1260, 222], [24, 122, 1260, 222],
    [24, 218, 1260, 318], [24, 218, 1260, 318], [24, 314, 1260, 414], [24, 314, 1260, 414], [24, 410, 1260, 510],
    [24, 410, 1260, 510], [2633, 794, 3541, 894], [2633, 794, 3541, 894], [24, 506, 1260, 606],
    [24, 698, 1260, 798], [24, 602, 1260, 702], [24, 794, 1260, 894], [24, 890, 1260, 990], [1318, 26, 2586, 126],
    [1318, 122, 2586, 222], [1318, 314, 2586, 414], [1318, 218, 2586, 318], [1318, 506, 2586, 606],
    [1318, 410, 2586, 510], [1318, 602, 2586, 702], [1318, 794, 2586, 894], [1318, 698, 2586, 798],
    [1318, 890, 2586, 990], [2633, 26, 3541, 126], [2633, 122, 3541, 222], [2633, 218, 3541, 318],
    [2633, 410, 3541, 510], [2633, 313, 3541, 413], [2633, 313, 3541, 413], [2633, 313, 3541, 413],
    [2633, 506, 3541, 606], [2633, 602, 3541, 702], [0, 0, 1, 1],
];
/// The overseas phase title cells (+ 308 words) for the Chinese languages.
const PHASE_TITLES_ZH: &[[i32; 4]] = &[
    [0, 0, 1, 1], [1192, 696, 1760, 776], [24, 24, 592, 104], [24, 120, 592, 200], [24, 120, 592, 200],
    [24, 216, 592, 296], [24, 216, 592, 296], [24, 312, 592, 392], [24, 312, 592, 392], [24, 408, 592, 488],
    [24, 408, 592, 488], [1192, 792, 1760, 872], [1192, 792, 1760, 872], [24, 504, 592, 584], [24, 696, 592, 776],
    [24, 600, 592, 680], [24, 792, 604, 872], [24, 888, 592, 968], [608, 24, 1188, 104], [608, 120, 1176, 200],
    [608, 312, 1176, 392], [608, 216, 1176, 296], [608, 504, 1176, 584], [608, 408, 1176, 488],
    [608, 600, 1176, 680], [608, 792, 1176, 872], [608, 696, 1176, 776], [608, 888, 1176, 968],
    [1192, 24, 1760, 104], [1192, 120, 1760, 200], [1192, 216, 1760, 296], [1192, 408, 1760, 488],
    [1192, 312, 1760, 392], [1192, 312, 1760, 392], [1192, 312, 1760, 392], [1192, 504, 1760, 584],
    [1192, 600, 1760, 680], [0, 0, 1, 1],
];
/// Button guide (sub_4A8F0): per guide index (unk_34FD48, 160 bytes each) a list of
/// (0 = help_key icon / 1 = help_txt_ja word, cell) pairs. Index 11 (the save list's, with X 削除) is the
/// overseas build's; the Japanese one draws index 11 from its empty table past the end.
const GUIDE_SEQ: [&[(u8, u8)]; 12] = [
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
    &[(0, 10), (1, 0), (0, 7), (1, 1), (0, 9), (1, 16), (0, 0), (1, 2), (0, 11), (1, 26)],
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
/// help_txt_ja word cells (x0 y0 x1 y1): CONTHELP_TXT_UV.BIN (allui, loaded by sub_9A420; i16 count, then
/// from byte 8 count cells per language), Japanese = language 0 (sub_2DE70); the fallback when the file is
/// missing. 0 戻る 1 決定 2 移動 3 解除 4 再生 5 変更 6 初期化
/// 7 セーブ 8 ロード 9 ボイス登録 10 ボイス再生 11 クイックジャンプ 12 カーソル移動 13 ページ変更 14 キャラクター変更
/// 15 連続再生 16 削除 17 再生(長押しで連続再生) 18 データコピー 19 データ削除 20 再生曲変更 21 停止 22 ループ変更
/// 23 画像の切替 24 拡大・縮小 25 移動 26 スクロール
const GUIDE_WORDS: [[u16; 4]; 27] = [
    [8, 8, 56, 36],
    [64, 8, 112, 36],
    [120, 8, 168, 36],
    [176, 8, 224, 36],
    [232, 8, 280, 36],
    [288, 8, 336, 36],
    [344, 8, 412, 36],
    [8, 44, 76, 72],
    [84, 44, 152, 72],
    [160, 44, 272, 72],
    [280, 44, 392, 72],
    [8, 80, 156, 108],
    [164, 80, 300, 108],
    [308, 80, 424, 108],
    [8, 116, 156, 144],
    [164, 116, 256, 144],
    [264, 116, 312, 144],
    [320, 116, 504, 144],
    [152, 152, 268, 180],
    [72, 152, 156, 180],
    [276, 152, 392, 180],
    [400, 152, 448, 180],
    [8, 188, 124, 216],
    [132, 188, 248, 216],
    [256, 188, 372, 216],
    [380, 188, 428, 216],
    [8, 224, 124, 252],
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

/// cGalleryMoviePlayer state (see `play_gallery_movie`).
#[derive(Clone, Copy)]
struct GalMovie {
    /// gallery movie index (sub_18B0C a2)
    n: usize,
    /// 0 curtain in, 1 movie, 2 curtain out
    phase: u8,
    /// seconds into the curtain fade
    t: f32,
    /// the BGM to restore (sub_9C460 at the start)
    bgm: i32,
    /// 60 Hz frames since the movie became ready (a1 + 60)
    frames: f32,
    /// the curtain is white (the first opening, until the movie is fully shown)
    white: bool,
    /// B / Plus may still stop the movie (a1 + 100)
    skip: bool,
    /// BGM 2 started (the prologue at 360 frames)
    bgm2: bool,
}

/// CG viewer transitions (cGalleryCGPhoto sub_C368). Sprite 176 holds a capture over the viewer (lower
/// priority draws on top): opening, its 0.5 s quad-in tween (sub_C498 -> state 4, sub_CD58) fades the gallery
/// capture (surface 1) from 1 to 0 off the picture; closing (sub_CE60) fades it back in, quad-out over 0.5 s;
/// a picture change (sub_CA54) captures the screen (sub_A4610(1), surface 3) and wipes it away through
/// rule_26 (previous) / rule_27 (next) linearly over 0.5 s.
#[derive(Clone, Copy, PartialEq)]
enum CgFx {
    None,
    Open,
    Close,
    /// the picture before (group, index, zoom, pan) and whether the change went forward
    Switch(usize, usize, f32, [f32; 2], bool),
}

/// CG viewer transitions last 0.5 s
/// cGalleryGuide of the gallery: the index shown, the fade (`dir` -1 out / +1 in, `t` 0..1) and the next index.
#[derive(Clone, Copy)]
struct GalGuide {
    idx: usize,
    dir: i8,
    t: f32,
    next: usize,
}

impl GalGuide {
    fn shown(idx: usize) -> Self {
        GalGuide { idx, dir: 0, t: 0.0, next: idx }
    }

    fn alpha(&self) -> f32 {
        match self.dir {
            -1 => (1.0 - self.t) * (1.0 - self.t),
            1 => self.t * self.t,
            _ => 1.0,
        }
    }
}

/// the mouse wheel / trackpad: the left stick tilt and frames per notch
const WHEEL_TILT: i32 = 64;
const WHEEL_FRAMES: f32 = 4.0;
const CG_FX_SECS: f32 = 0.5;

/// A scroll position moved by sub_27BD4: clamped to [0, max], animated over 1/6 s (ease-out quad) or set
/// at once.
#[derive(Clone, Copy, Default)]
struct ListScroll {
    pos: f32,
    target: f32,
    from: f32,
    t: f32,
    /// tween length (ms)
    len: f32,
}

impl ListScroll {
    fn to(&mut self, v: f32, max: f32, animate: bool) {
        self.over(v, max, if animate { LIST_SCROLL_MS } else { 0.0 });
    }

    /// To `v` over `ms` (0: at once); the scroll bar drag takes 0.2 s (sub_27974).
    fn over(&mut self, v: f32, max: f32, ms: f32) {
        self.target = v.clamp(0.0, max.max(0.0));
        self.len = ms;
        if ms > 0.0 {
            self.from = self.pos;
            self.t = 0.0;
        } else {
            self.pos = self.target;
            self.t = 0.0;
        }
    }

    fn step(&mut self, dt: f32) {
        if self.t < self.len {
            self.t = (self.t + dt).min(self.len);
            let t = self.t / self.len;
            self.pos = self.from + (self.target - self.from) * -(t * (t - 2.0));
        }
    }
}
/// music player buttons (objects 385..390): play, pause, stop, repeat at y 576, previous, next at y 656
const MUSIC_CTL: [(f32, f32); 6] = [(1432.0, 576.0), (1512.0, 576.0), (1592.0, 576.0), (1672.0, 576.0), (1512.0, 656.0), (1592.0, 656.0)];

impl Ui {
    pub fn new(game: &mut Game, r: &Renderer) -> Self {
        let tex = Self::load_textures(game, r, false);
        let file = SaveFile::load();
        let mut ui = Self {
            screen: Screen::Title,
            stack: Vec::new(),
            stack_hover: Vec::new(),
            hover: None,
            mouse: (0.0, 0.0),
            tex,
            file,
            slots: Vec::new(),
            popup: None,
            save_wait: None,
            delete_wait: None,
            save_cool: 0,
            save_flash: 0.0,
            save_seen: None,
            save_notes: None,
            save_pending: None,
            save_order: None,
            dlg_t: 0.0,
            dlg_out: None,
            title_movie: false,
            on_title: false,
            back_btn: 0.0,
            complete: None,
            cg_unlocked: Vec::new(),
            music_ok: Vec::new(),
            playing_track: None,
            music_clock: 0.0,
            music_vol: 0.6,
            music_repeat: false,
            music_paused: false,
            music_fade: None,
            music_bg_off: 0.0,
            cg_zoom: 0.0,
            cg_pan: [0.0; 2],
            cg_size: [0.0; 2],
            cg_fx: CgFx::None,
            cg_t: 0.0,
            cg_guide: (0, 0.0),
            rstick: 0,
            under: false,
            last_track: None,
            sysmes: Vec::new(),
            guide_words: (usize::MAX, Vec::new()),
            title_clock: 0.0,
            cg_flash: (None, 0.0),
            cg_sub: None,
            cg_old: None,
            cg_in: CG_FX_SECS,
            cg_pick: (None, 0.0, 0.0),
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
            flow_len: FLOW_SCROLL_MS,
            flow_cell: (0, 0),
            flow_cur_y: 0.0,
            flow_ref: 0.0,
            flow_manual: false,
            flow_flash: (None, 0.0),
            flow_old: None,
            drag: touch::TouchDrag::default(),
            bar: None,
            flow_open: None,
            menu_a: 1.0,
            gal_movie: None,
            intro: None,
            press: false,
            boot_flag: true,
            logo_a: 0.0,
            logo_rising: true,
            press_t: 0.0,
            logo_fx: LogoFx::new(),
            menu_bg: MenuBg::new(),
            black: 0.0,
            black_speed: 0.0,
            black_then: None,
            black_top: 1.2,
            gal_ret: [-1; 3],
            gal_prev: -1,
            gal_first: -1,
            replay_pending: None,
            black_col: [0.0; 3],
            cont_slot: None,
            load_popup: None,
            load_after_close: None,
            wt: Default::default(),
            w: Default::default(),
            wt_reinit: false,
            wt_any: false,
            wt_loaded: None,
            load_cut: false,
            title_ui_hidden: false,
            banner: None,
            title_after_banner: false,
            black_under: false,
            scr_a: 1.0,
            closing: false,
            backlog_jump: false,
            gal_a: 1.0,
            gal_next: None,
            gal_side: true,
            gal_tab: 0,
            gal_focus_next: false,
            gal_mem: None,
            gal_scroll: ListScroll::default(),
            gal_track: false,
            music_btn: 0,
            music_vol_from: 4,
            music_row: 0,
            nav_repeat: false,
            stick: [0; 2],
            stick_tick: 0.0,
            stick_acc: 0,
            wheel: 0.0,
            gal_guide: GalGuide::shown(10),
            wheel_press: 0.0,
            wheel_steps: 0.0,
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
            sys_focus: false,
            conf_cur: [Some((0, 0)), Some((0, 0)), None],
            title_cur: None,
            sounded: false,
            side: false,
            sub_hover: None,
            conf_bar: 0,
            conf_test_se: None,
            conf_test_row: None,
            conf_hold: 0,
            lang_seq: None,
            lang_pick: None,
            lang_cool: 0,
            lang_ms: 0.0,
            conf_hold_dirty: false,
            enter_held: false,
            pads_held: 0,
            touch: touch::Touch::new(),
            help_t: 0.0,
            font_sys: None,
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
    /// The system atlases of the game language (sub_9A940), and GALLERY_PARTS when `gallery`. Textures are
    /// found by name; the overseas language buttons (conf_parts #19) also by position.
    fn load_textures(game: &Game, r: &Renderer, gallery: bool) -> HashMap<String, TexRef> {
        let mut tex = HashMap::new();
        let stems = ["TITLE_PARTS", "MENU_PARTS", "SAVE_PARTS", "CONF_PARTS", "DIALOG_PARTS", "DISP_PARTS", "MODEPNL_PARTS", "GALLERY_PARTS"];
        for stem in stems.into_iter().filter(|s| gallery || *s != "GALLERY_PARTS") {
            let atlas = game.assets.ui_atlas(stem);
            match game.assets.ui_textures(r, &atlas) {
                Ok(list) => {
                    if stem == "CONF_PARTS"
                        && let Some((_, t)) = list.get(19)
                    {
                        tex.insert(CONF_LANG_TEX.to_string(), t.clone());
                    }
                    tex.extend(list);
                }
                Err(e) => tracing::warn!("ui textures {atlas}: {e:#}"),
            }
        }
        tex
    }

    /// One 60 Hz frame of the overseas language change (sub_2D910 with dword_9D3A44 set): the screen is
    /// captured (sub_A2550) as the change starts; step 10 writes the config word and reloads the language's
    /// text, atlases and fonts (sub_9A940, sub_2BFE0, sub_AB0F0) and lays the shown text out again (sub_7FE20
    /// / sub_7D0C0); step 15 fades the capture out over 250 ms (sub_A27F0) and step 16 waits for it. A pick
    /// of the word already set does nothing (sub_2D720).
    fn lang_frame(&mut self, game: &mut Game, r: &Renderer) {
        let Some(mut q) = self.lang_seq else {
            if game.screen_clear() && self.lang_cool > 0 {
                self.lang_cool -= 1;
            }
            return;
        };
        // the press sets config state 10; sub_2D910 runs sub_2D720 for it the next frame
        if q.fresh {
            q.fresh = false;
            self.lang_seq = Some(q);
            return;
        }
        if q.state == 0 {
            if q.word == self.file.language() {
                self.lang_seq = None;
                self.lang_pick = None;
                return;
            }
            q.state = 1;
        }
        match q.state {
            1 => {
                game.freeze_screen();
                q.state = 2;
            }
            10 => {
                self.file.set_language(q.word);
                game.change_language(r, q.word);
                let gallery = self.tex.contains_key("gallerymenu_btn");
                self.tex = Self::load_textures(game, r, gallery);
                self.sysmes.clear();
                self.font_sys = None;
                self.store_global(game);
                q.state = 11;
            }
            15 => {
                game.thaw_screen(250.0);
                q.state = 16;
            }
            16 => {
                if game.screen_clear() {
                    self.lang_seq = None;
                    self.lang_pick = None;
                    return;
                }
            }
            n => q.state = n + 1,
        }
        self.lang_seq = Some(q);
    }

    pub fn store_global(&mut self, game: &Game) {
        game.store_system(&mut self.file);
        if let Err(e) = self.file.store() {
            tracing::warn!("saving savedata.bin: {e:#}");
        }
    }

    /// Put a snapshot into a record, then write the file. The save list rebuilds its preview from the
    /// record (sub_884F0), so nothing else is stored.
    fn write_record(&mut self, game: &mut Game, slot: usize, data: &SaveData) -> anyhow::Result<()> {
        self.put_record(game, slot, data)?;
        game.store_system(&mut self.file);
        self.file.store()?;
        Ok(())
    }

    /// The record into the in-memory save file only (the auto save's shift included).
    fn put_record(&mut self, game: &mut Game, slot: usize, data: &SaveData) -> anyhow::Result<()> {
        let rec = game.encode_record(data).ok_or_else(|| anyhow::anyhow!("position not saveable"))?;
        if slot == save::auto_first() {
            self.file.shift_auto();
        }
        self.file.set_record(slot, &rec);
        self.file.update_continue();
        Ok(())
    }

    /// sub_52AA0 with dword_AFFCC4 running: the record goes into the list, the file is written by a later save.
    fn write_memory(&mut self, game: &mut Game, slot: usize) {
        let Some(data) = game.save_data() else { return };
        if let Err(e) = self.put_record(game, slot, &data) {
            tracing::warn!("save to slot {slot}: {e:#}");
            return;
        }
        self.refresh_slots(game);
    }

    /// cGallery mode buttons (objects 379..384, sub_6124): CG, Music, Flowchart, Movie, Ending, Back
    fn hub_items() -> Vec<([f32; 4], Action)> {
        [Action::OpenGallery, Action::OpenMusic, Action::OpenFlow, Action::OpenMovies, Action::OpenEndings, Action::Back]
            .iter()
            .enumerate()
            .map(|(i, a)| ([80.0, 160.0 + i as f32 * 80.0, 256.0, 96.0], *a))
            .collect()
    }

    /// The shown grid's entries: CG groups with at least one unlocked picture (cGalleryCGMain sub_B830), or the
    /// unlocked pictures of the variations grid's group (cGalleryCGSub sub_D468), as indices into `cg_unlocked`.
    fn cg_list(&self) -> Vec<usize> {
        self.cg_grid(self.cg_sub)
    }

    fn cg_grid(&self, sub: Option<usize>) -> Vec<usize> {
        match sub {
            Some(g) => (0..self.cg_unlocked.get(g).map_or(0, Vec::len)).collect(),
            None => (0..self.cg_unlocked.len()).filter(|&g| !self.cg_unlocked[g].is_empty()).collect(),
        }
    }

    /// The picture a grid entry shows: a group's first unlocked one (sub_B830), or the variation itself.
    fn cg_thumb(&self, sub: Option<usize>, id: usize) -> Option<crate::gallery::Entry> {
        match sub {
            Some(g) => self.cg_entry(g, id),
            None => self.cg_entry(id, 0),
        }
    }

    /// Every unlocked picture in group order (cGallery + 88, sub_4470): the viewer's L / R steps through it.
    fn cg_flat(&self) -> Vec<(usize, usize)> {
        self.cg_unlocked.iter().enumerate().flat_map(|(g, v)| (0..v.len()).map(move |k| (g, k))).collect()
    }

    /// Make the shown grid (sub_9384 / sub_8D08 -> sub_AE8C, sub_9970): `sub` with the cursor on entry `cur`.
    /// The saved scroll is cut to whole rows (sub_AE8C ceils it, sub_9970 floors it by 139), then moved a row at a
    /// time until the cursor (cell y - 6) is inside 109..804; the grid fades in over 0.5 s. `fade_old`: the
    /// grid it replaces fades out at the same time (sub_ABE8), else it goes at once.
    fn cg_show(&mut self, game: &Game, sub: Option<usize>, cur: usize, scroll: f32, fade_old: bool) {
        let items = self.items(game);
        let old_cur = self.hover.and_then(|h| items.get(h)).and_then(|(_, a)| match a {
            Action::Cg(i) => Some(*i),
            _ => None,
        });
        self.cg_old = fade_old.then_some((self.cg_sub, self.gal_scroll.pos, old_cur));
        self.cg_sub = sub;
        self.cg_in = 0.0;
        let row = self.cg_list().iter().position(|&x| x == cur).unwrap_or(0) / CG_COLS;
        let mut k = ((scroll + 0.001).ceil() + 0.001) as i32 / 139;
        let y = 103.0 + row as f32 * 139.0;
        while y - k as f32 * 139.0 < 109.0 {
            k -= 1;
        }
        while y - k as f32 * 139.0 >= 804.0 {
            k += 1;
        }
        let max = self.gal_scroll_max();
        self.gal_scroll.to(k as f32 * 139.0, max, false);
        self.gal_mem = Some(Action::Cg(cur));
        if !self.gal_side {
            self.hover_action(game, Action::Cg(cur));
        }
    }

    /// Thumbnail rectangle of grid entry `k` (154x87 atlas cell at 1.5x).
    fn cg_cell(k: usize, scroll: f32) -> [f32; 4] {
        let (col, row) = ((k % CG_COLS) as f32, (k / CG_COLS) as f32);
        [392.0 + col * 240.0, 109.0 + row * 139.0 - scroll.trunc(), 231.0, 130.5]
    }

    /// sub_926F0(1): every ending node 533..556 completed (unlocks the QA_25 button, object 418).
    fn all_endings(game: &Game) -> bool {
        match game.assets.build {
            knife_core::exe::Build::Japanese => (533..=556).all(|n| crate::game::get_bit(&game.endings, n)),
            // sub_FF40: every record's completed node
            knife_core::exe::Build::Overseas => (0..24).all(|i| Self::ending_ok(game, i)),
            // Witch on the Holy Night has no Q&A corner
            knife_core::exe::Build::Witch => false,
        }
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
                // leaving the music player (sub_19AC4) stops its track at once (sub_9C9B0(3333.3))
                if self.screen == Screen::Music {
                    self.playing_track = None;
                    self.music_paused = false;
                    self.music_fade = None;
                    game.sound.stop_bgm_rate(3333.3);
                }
                self.gal_next = Some(s);
            }
        } else {
            // entering the gallery: the mode buttons have the focus (cGallery state 2), unless off_AE0288[1] is
            // set. cGallery state 0 (sub_49B0) builds cGalleryMenu from off_AE0288 on every open and then
            // clears only [0] and [2], so the node / scene of the last replay (sub_5040) stays: its + 56 >= 0 makes
            // sub_6934 open the first mode with the content focused (sub_7F38(.., 1), which then sets + 56 = -1)
            self.gal_focus_next = self.gal_ret[1] >= 0;
            self.push(s);
            self.gal_mode_shown(game);
        }
    }

    /// A gallery mode just came on screen: its list starts at the top and the focus goes to its content
    /// when the mode was picked with the buttons (sub_7328), else stays on the buttons.
    fn gal_mode_shown(&mut self, game: &mut Game) {
        self.gal_scroll = ListScroll::default();
        self.gal_track = false;
        self.music_bg_off = 0.0;
        self.gal_mem = None;
        // cGalleryCG starts on the group grid (sub_8818)
        self.cg_sub = None;
        self.cg_old = None;
        self.cg_in = CG_FX_SECS;
        self.cg_pick = (None, 0.0, 0.0);
        self.music_btn = 0;
        self.music_row = 0;
        // cGalleryMusic (sub_19838 state 0) stops the BGM at once as it fades in
        if self.screen == Screen::Music {
            game.sound.stop_bgm_rate(3333.3);
        }
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
        use crate::game::get_bit;
        match game.assets.build {
            _ if i == 24 => Self::all_endings(game),
            knife_core::exe::Build::Japanese => get_bit(&game.nodes, crate::gallery::ENDINGS[i].0),
            knife_core::exe::Build::Overseas => {
                let (a, b) = crate::gallery::EN_ENDING_DONE[i];
                [a, b].into_iter().any(|n| n >= 0 && get_bit(&game.endings, n as usize))
            }
            knife_core::exe::Build::Witch => false,
        }
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

    /// The mode's content takes the focus (content vt + 40) when it has something to select (vt + 32). Giving it
    /// up (vt + 80) keeps the music row, but the CG grid drops its column (sub_ACA0 -> -1, sub_A8C4 puts column
    /// 0 of the same row back) and the movies (sub_189E4 / sub_18930) and endings (sub_F278 / sub_EEC0) their
    /// whole cursor, so those come back on their first entry.
    fn gal_to_content(&mut self, game: &Game) -> bool {
        let acts = self.gal_content(game);
        let Some(&first) = acts.first() else { return false };
        let a = match (self.screen, self.gal_mem) {
            (Screen::Music, Some(Action::MusicCtl(_) | Action::MusicVol)) => Action::MusicCtl(self.music_btn as u8),
            (Screen::Gallery, Some(m)) => acts.iter().position(|&a| a == m).map_or(first, |i| acts[i - i % CG_COLS]),
            (Screen::Music, Some(m)) if acts.contains(&m) => m,
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
        // a cursor move clears the manual-scroll flag (sub_1A7FC / sub_9CBC bit 5)
        self.gal_track = false;
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
    /// (sub_9CBC), music row y inside 138..940 (sub_1A7FC), measured on the scroll shown this frame (a1 + 80 /
    /// a1 + 60, refreshed from the scroller each frame), so the lit row moves with the rows on screen.
    fn gal_keep_cursor(&mut self, game: &Game) {
        if self.gal_side {
            return;
        }
        let s = self.gal_scroll.pos.trunc();
        let cur = self.hover.and_then(|h| self.items(game).get(h).map(|(_, a)| *a)).or(self.gal_mem);
        // the music list keeps its cursor row (a1 + 52) when the mouse leaves the rows
        let cur = cur.or((self.screen == Screen::Music).then_some(Action::Track(self.music_row)));
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
                    game.sound.set_bgm_volume(Self::music_gain(self.music_vol), 0.1);
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
            if qa && r == 2 && dy < 0 && row == 3 && col == 5 {
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
        self.reload_slots(game);
        game.clear_previews(None);
    }

    /// The slots' records again, keeping their thumbnails.
    fn reload_slots(&mut self, game: &mut Game) {
        self.slots = (0..save::slots())
            .map(|n| {
                let rec = self.file.record(n)?;
                let d = game.decode_record(rec);
                if d.is_none() {
                    tracing::warn!("save record {n} does not resolve to a script position");
                }
                d
            })
            .collect();
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
        // Witch on the Holy Night's save screen: the title's Load (state 25), else the system menu's entry
        if crate::assets::witch() && matches!(name, "save" | "load") {
            let save = name == "save";
            if self.screen == Screen::Title {
                if !save {
                    self.wt.st = 25;
                    self.ws_open(game, r, false);
                    self.w.save.title = true;
                }
                return;
            }
            self.wm_open(game, r);
            self.w.menu.cur = if save { 0 } else { 1 };
            self.w.menu.st = 4;
            self.w.menu.a = 0.0;
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
        if self.screen == Screen::Title {
            self.title_cur = self.hover.and_then(|i| self.title_entries().get(i).map(|e| e.0));
        }
        self.stack_hover.truncate(self.stack.len());
        self.stack_hover.push(self.hover);
        self.stack.push(self.screen);
        self.screen = s;
        if s != Screen::Menu {
            self.sys_focus = false;
        }
        self.hover = if s == Screen::Menu { self.sys_focus.then_some(self.sys_tab) } else { None };
        self.side = false;
        self.closing = false;
        self.scr_a = if Self::fade_speeds(s).0 < 1.0 { 0.0 } else { 1.0 };
        self.gal_a = 1.0;
        if s == Screen::Menu {
            self.menu_a = 1.0;
        }
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
        let kept = self.stack_hover.get(self.stack.len()).copied().flatten();
        // back in the system menu the cursor stays on the tab that was open and takes the focus (state 9 / 12)
        if self.screen == Screen::Menu {
            self.sys_focus = true;
        }
        self.hover = match self.screen {
            Screen::Menu => Some(self.sys_tab),
            // back on the title from LOAD GAME, CONFIG or a dialog: states 15 / 17 keep dword_65CBD4
            Screen::Title => self.title_cur.and_then(|a| self.title_entries().iter().position(|e| e.0 == a)),
            Screen::SaveLoad { .. } => kept,
            _ => None,
        };
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

    /// sub_4EC00 (overseas): the system menu shows (dword_B359F4 > 0: open, or fading, and kept at 1 under
    /// its save / load screens and dialogs; 0 while the chart runs) or its config screen is open (states
    /// 11 / 12). The overseas sub_391A0 then draws no message window, text or choice.
    pub fn system_menu_shown(&self) -> bool {
        let under_menu = self.stack.contains(&Screen::Menu);
        match self.screen {
            Screen::Menu => self.scr_a * self.menu_a > 0.0,
            Screen::Config => true,
            Screen::Flowchart { .. } => false,
            _ => under_menu,
        }
    }

    // ---- help balloon -----------------------------------------------------------------------

    /// The shared system font (nn::pl::GetSharedFont): sub_A7790 slots with flag 0x100000 (every type-3 text
    /// object by default, sub_25C94; the help balloon, banner, save list scene names, menu chapter name and
    /// music panel) set +101785, and sub_C406C then draws them with font variant 3 (sub_CBC68: the font set
    /// sub_CAA50 builds from the shared fonts sub_C9F3C fetches). Switch (save list "SCENE プロローグ", flags
    /// 0x183040): the 0x80000 / 0x200000 font index (+55436) does not change it. The shared fonts are the
    /// language's (`Assets::shared_font_names`); falls back to the UI font (TsukuGo) when the assets dir has none.
    fn sys_font(game: &Game, r: &Renderer) -> Option<FontAtlas> {
        shared_font_atlas(r, game.assets.shared_fonts()).or_else(|| Self::ui_font(game, r, "2"))
    }

    /// A font of the language's folders (`Assets::font_path`).
    fn ui_font(game: &Game, r: &Renderer, suffix: &str) -> Option<FontAtlas> {
        let path = game.assets.font_path(suffix).or_else(|| {
            tracing::warn!("no font in {}{suffix}", game.assets.language_folder());
            None
        })?;
        let data = std::fs::read(&path).map_err(|e| tracing::warn!("{}: {e}", path.display())).ok()?;
        FontAtlas::new(r, data).map_err(|e| tracing::warn!("{}: {e:#}", path.display())).ok()
    }

    /// Help text of the focused object (object tables: system menu 0x591438, config 0x46B370):
    /// (SYSMES id, object position, object rect, flags), all in 640x448 units.
    fn help_target(&self) -> Option<(usize, [f32; 2], [f32; 4], u32)> {
        let hover = self.hover;
        match self.screen {
            // objects 197..201 placed by sub_4C370
            // the focused tab (sub_AE0E0): the menu's cursor, or the side cursor over an open submenu
            Screen::Menu if hover.is_none() => None,
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
            Screen::Game => self.panel_help(),
            _ => None,
        }
    }

    /// sub_AE240 / sub_AE590: the balloon sits at a quarter of the object's rect towards the screen centre,
    /// offset by 6 units, as mode_help stretched to the text width + 40 behind the text (alpha 0.7).
    fn draw_help(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some((id, pos, rc, flags)) = self.help_target() else { return };
        // a target the update has not seen yet (the screen changed after it ran this frame, e.g. a submenu's
        // close finishing) starts from nothing next frame; drawing it now would flash the old balloon alpha
        if self.help_item != Some((id, [pos[0].round() as i32, pos[1].round() as i32])) {
            return;
        }
        let a = self.help_t * self.scr_a;
        if a <= 0.0 || self.busy() || self.popup.is_some() {
            return;
        }
        self.draw_help_at(game, r, out, id, pos, rc, flags, a);
    }

    /// The balloon of SYSMES `id` for an object at `pos` with touch rect `rc` and object flags `flags` (640x448
    /// units), at alpha `a`.
    #[allow(clippy::too_many_arguments)]
    fn draw_help_at(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, id: usize, pos: [f32; 2], rc: [f32; 4], flags: u32, a: f32) {
        if a <= 0.0 {
            return;
        }
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let Some(text) = self.sysmes.get(id).cloned().filter(|t| !t.is_empty()) else { return };
        if self.font_sys.is_none() {
            self.font_sys = Self::sys_font(game, r);
        }
        let Some(font) = self.font_sys.as_mut() else { return };
        // text slot 2 (sub_AE110): 0.325 x 0.37 of the 60 px glyph, a7 = 0 (spacing 4 px); the balloon is the
        // slot's width (sub_AB870, 640 units) truncated, + 6 (dword_1915104)
        const SIZE: [f32; 2] = [60.0 * 0.325, 60.0 * 0.37];
        const SPACING: f32 = 4.0;
        // sub_AE240 works on the object's integer position and corners (C division), keeping the fraction
        let tw = font.measure_slot(&text, SIZE, SPACING) / 3.0;
        let (px, py) = (pos[0] as i32, pos[1] as i32);
        let (x0, y0, x1, y1) = (rc[0] as i32 + px, rc[1] as i32 + py, rc[2] as i32 + px, rc[3] as i32 + py);
        let mut q = match (x0 + x1 >= 640, y0 + y1 < 448) {
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
        let mut offx = (if q <= 1 { (x1 + 3 * x0) / 4 } else { (3 * x1 + x0) / 4 } - px) as f32;
        let offy = (if q == 0 || q == 3 { (y1 + 3 * y0) / 4 } else { (3 * y1 + y0) / 4 } - py) as f32;
        let (dx, dy) = [(-6.0, -6.0), (-6.0, 6.0), (6.0, 6.0), (6.0, -6.0)][q];
        if q > 1 && offx + pos[0] + tw + 3.0 > 624.0 {
            offx = 624.0 - pos[0] - tw - 3.0 - dx;
        }
        // dword_1915104
        let w = tw.trunc() + 6.0;
        let mut x = offx + pos[0] + dx;
        let mut y = offy + pos[1] + dy;
        if q <= 1 {
            x = (x - w).max(0.0);
        }
        if q == 0 || q == 3 {
            y -= 13.0;
        }
        // Witch on the Holy Night's jp_AE590: the backing 0.41625 high, the text slot at y + 0
        let witch = crate::assets::witch();
        if let Some(t) = self.tex.get("mode_help") {
            let (bw, bh) = (t.width as f32 * (w + 40.0) / 640.0, t.height as f32 * if witch { 0.41625 } else { 0.41489 });
            out.push(Quad::rect(t.clone(), [(x - 20.0) * 3.0, y * H / 448.0, bw * 3.0, bh * H / 448.0], [1.0, 1.0, 1.0, a * 0.7]));
        }
        // sub_AB710: the slot at (x + 2, y + 3), white text with a dark rim
        let (tx, base) = ((x + 2.0) * 3.0, (y + if witch { 0.0 } else { 3.0 }) * H / 448.0 + SYS_ASCENT * gs() * SIZE[1]);
        for (ddx, ddy, c) in [(-1.5, 0.0, 0.0), (1.5, 0.0, 0.0), (0.0, -1.5, 0.0), (0.0, 1.5, 0.0), (0.0, 0.0, 1.0)] {
            font.draw_str_slot(r, out, &text, SIZE, SPACING, [tx + ddx, base + ddy], [c, c, c, if c > 0.0 { a } else { a * 0.6 }]);
        }
    }

    // ---- boot, title and transitions ------------------------------------------------------

    /// Launch sequence (sub_B5650 / sub_8D2E0). Once the prologue was reached (node 485), the TYPE-MOON
    /// movie and the notice play before the title (`skip_intro` leaves them out). Before that the prologue
    /// scene (id 260) starts right away; its end returns to a title that asks for PRESS ANY BUTTON.
    pub fn boot(&mut self, game: &mut Game, r: &Renderer, skip_intro: bool) {
        if crate::assets::witch() {
            self.wt_boot(game, r, skip_intro);
            return;
        }
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

    /// The screens that turn the touch back button on (sub_4A790(1)): the backlog (sub_28F80), the config
    /// (sub_2B680 / sub_2C6E0), the save list (sub_50ED0 / sub_50FC0), the flowchart (sub_110D4) and a CG
    /// group's grid (sub_8D08 / sub_9384); closing them and the gallery's mode switch (sub_6E28) turn it off.
    fn back_btn_on(&self) -> bool {
        match self.screen {
            Screen::Backlog { .. } | Screen::Config | Screen::SaveLoad { .. } | Screen::Flowchart { .. } => true,
            Screen::Gallery => !self.gal_side,
            _ => false,
        }
    }

    /// Show the title, fading in from black (sub_8E200 state 2: 0.012 / frame).
    fn enter_title(&mut self) {
        self.wt_reinit = crate::assets::witch();
        self.screen = Screen::Title;
        // the title's entries take the UI focus (sub_8E200)
        self.sys_focus = false;
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

    /// The title cursor on entry (sub_8E200 state 14): GALLERY (`fallback`, byte_1536158) back from the gallery,
    /// else CONTINUE when offered, else LOAD GAME when some manual record is in use (sub_4EEB0), else NEW GAME.
    fn title_cursor(&self, fallback: Action) -> Option<usize> {
        let act = if fallback != Action::NewGame {
            fallback
        } else if self.continue_slot().is_some() {
            Action::Continue
        } else if self.file.manual_count() > 0 {
            Action::OpenLoad
        } else {
            Action::NewGame
        };
        self.title_entries().iter().position(|e| e.0 == act)
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
            // overseas sub_B5650 state 3: screen 1 becomes 1 + language (past 3: 1), parts.mrg 1..4 holding the
            // notice per language, and state 8 then steps past the other three (qword_1835524 = 4 - language)
            let index = match game.assets.build {
                knife_core::exe::Build::Overseas => 1 + if game.assets.language() > 3 { 1 } else { game.assets.language() },
                knife_core::exe::Build::Japanese | knife_core::exe::Build::Witch => 1,
            };
            self.notice = game.assets.parts_texture(r, index);
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
                // BGM out at 0.5 / frame (sub_9C9B0)
                game.sound.stop_bgm_rate(0.5);
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
                    // the same title state 20: its white goes on as the story's
                    self.black_col = [1.0; 3];
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
    /// (0.024, 0.048); system menu: sub_4CBA0 / sub_4D8C0 state 4 (1/18, 1/30); gallery: sub_47A90 (0.04).
    fn fade_speeds(s: Screen) -> (f32, f32) {
        match s {
            Screen::SaveLoad { .. } | Screen::Config => (0.024, 0.048),
            Screen::Menu => (1.0 / 18.0, 1.0 / 30.0),
            Screen::Gallery | Screen::Music | Screen::Movies | Screen::Endings => (0.04, 0.04),
            // backlog: sub_28F80 states 4 and 8 (and the quick jump, sub_28D50)
            Screen::Backlog { .. } => (0.05, 0.05),
            // cFlowchart states 0 / 5 (sub_110D4, sub_11BEC) and its panel (sub_1697C): 0.5 s, quad in / out
            // on the way in / out, i.e. the square of this linear alpha both ways
            Screen::Flowchart { .. } => (1.0 / 30.0, 1.0 / 30.0),
            _ => (1.0, 1.0),
        }
    }

    fn is_gallery_mode(s: Screen) -> bool {
        matches!(s, Screen::Gallery | Screen::Music | Screen::Movies | Screen::Endings)
    }

    /// Leave the current screen with its fade-out (the pop happens in `update`).
    fn close(&mut self) {
        if Self::fade_speeds(self.screen).1 < 1.0 {
            // the system menu over the game (sub_4D8C0 state 4) fades its backdrop with it (sub_49630(0, -1/30))
            if self.screen == Screen::Menu && matches!(self.stack.last(), None | Some(Screen::Game)) {
                self.menu_bg.fade(-1.0 / 30.0);
            }
            self.closing = true;
        } else {
            self.pop();
        }
    }

    /// The fade-out finished: leave the screen; back on the title or in game the backdrop fades out.
    fn finish_close(&mut self, game: &mut Game, r: &Renderer) {
        let from = self.screen;
        if std::mem::take(&mut self.load_cut) {
            self.closing = false;
            self.enter_loaded();
            return;
        }
        if let Some((scene, _, _)) = self.replay_pending.take() {
            self.start_replay(game, scene);
            return;
        }
        // the backlog faded out for a quick jump (sub_28D50): replay from the page as a rewind does (sub_5BAF0),
        // or sound 2 when its entry is gone
        if let (Screen::Backlog { scroll }, true) = (from, std::mem::take(&mut self.backlog_jump))
            && !game.backlog_jump(r, scroll) {
                self.snd(game, 2);
            }
        // the music player's track stops at once as it closes (sub_19AC4)
        if Self::is_gallery_mode(from) && self.playing_track.take().is_some() {
            self.music_paused = false;
            self.music_fade = None;
            game.sound.stop_bgm_rate(3333.3);
        }
        self.pop();
        // back from the chart to a mode other than the music player, the gallery BGM plays (sub_7624 ->
        // sub_8E1C0; the same BGM keeps playing)
        if matches!(from, Screen::Flowchart { gallery: true }) && Self::is_gallery_mode(self.screen) && self.screen != Screen::Music {
            game.play_title_bgm();
        }
        // after the chart the gallery's content goes on fading in (it started with the chart's fade-out,
        // sub_7624 -> sub_82FC); the system menu fades in from 0 (state 16)
        match self.screen {
            Screen::Title if Self::is_gallery_mode(from) => {
                // cGallery state 1: [0] = the mode it closes with (sub_82E4), the first mode of the next open
                self.gal_ret[0] = Self::gal_mode(from).map_or(-1, |m| m as i32);
                // leaving the gallery (sub_48AC0 mode 0 substate 10): sub_8D2E0 restarts the title (black, state
                // 0, fading in at 0.012 in state 2) and the backdrop and strips fade out at 0.02 over it, so the
                // backdrop crossfades into the title instead of going to black first
                self.menu_bg.fade(-0.02);
                self.enter_title();
                self.black_under = true;
                // byte_1536158: the cursor comes back on GALLERY
                self.hover = self.title_cursor(Action::OpenExtra);
            }
            Screen::Title if crate::assets::witch() => match self.load_after_close.take() {
                Some(i) => self.wt_loaded_from_list(game, i),
                None => self.wt_back(),
            },
            Screen::Title => {
                self.menu_bg.fade(-0.02);
                if let Some(i) = self.load_after_close.take() {
                    // title case 32, result 2: BGM out at 1.5 / frame (sub_9C9B0), the UI hidden (byte_1536155), state 30 fades
                    // to black (+0.01 / frame to 1.2), then the game
                    game.sound.stop_bgm_rate(1.5);
                    self.title_ui_hidden = true;
                    self.black_col = [0.0; 3];
                    self.black_under = false;
                    self.black_speed = 0.01;
                    self.black_then = Some(Action::LoadNow(i));
                }
            }
            // the system menu's alpha reached 0: sub_49630(0, 0) cuts the backdrop (and the blur) at once
            Screen::Game if from == Screen::Menu => self.menu_bg.fade(0.0),
            Screen::Game => self.menu_bg.fade(-0.0333),
            _ => {}
        }
    }

    // ---- layout ------------------------------------------------------------------

    fn items(&self, game: &Game) -> Vec<([f32; 4], Action)> {
        match self.screen {
            // title menu (sub_8D2E0 / sub_8D5D0): sprite objects 232..237 placed from the slot table at
            // unk_352AF4 in a 640x448 space; CONTINUE only shows when there is something to resume. The touch
            // area is the objects' own rect (unk_65CBD8: (0, 4)-(98, 25) from the slot, top-left anchored,
            // whole units inclusive, sub_AE840), not the 72 px sprite: the slots are 23.23 apart, so the
            // sprites overlap but the touch areas do not
            Screen::Title if self.press => Vec::new(),
            Screen::Title => {
                let n = self.title_entries().len();
                let ky = H / 448.0;
                self.title_entries()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (a, _, _))| {
                        (Self::object_rect([272.0, Self::title_slot(i, n)[1] / ky], [0, 4, 98, 25]), a)
                    })
                    .collect()
            }
            // system menu (sub_4C370): objects 197..201 at x 26.667, y 43.556 + 31.111*i (640x448 units), touch
            // rect (0, 0)-(74, 29) (unk_591438)
            Screen::Menu => [
                Action::OpenSave,
                Action::OpenLoad,
                Action::OpenConfig,
                Action::OpenFlow,
                Action::ToTitle,
            ]
            .iter()
            .enumerate()
            .map(|(i, a)| (Self::menu_tab_rect(i), *a))
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
                    // a slider's touch object is its knob, which rides the value along the 508 px track
                    if let Ctl::Slider(i) = c {
                        let (v, max, _) = self.conf_slider(game, i);
                        rc[0] += v / max * 508.0;
                    }
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
            // the viewer has no buttons: touch pans and flicks (`cg_drag`, `cg_flick`)
            Screen::CgView { .. } => Vec::new(),
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
            // cGalleryMoviePlayer reads no touch
            Screen::MoviePlay => vec![],
            Screen::Endings => {
                // cGalleryEnding (sub_DAF8 / sub_DDA4): reached endings get a play button, ending_btn
                // (object 394+i, 112x112 at 720 + 480i, 128) or ending_badendbtn (397+i, 88x88 on the plate)
                // an unreached entry has no object, but sub_F6F8 can still put the cursor on one (column 7 of
                // row 2 going up from the QA button): it gets an empty rect, so nothing is drawn or touched there
                let mut v: Vec<([f32; 4], Action)> = (0..24)
                    .map(|i| {
                        let rc = if !Self::ending_ok(game, i) {
                            [0.0; 4]
                        } else if i < 3 {
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
            // the WoH screens hit-test their own objects (`witch`)
            Screen::Witch => Vec::new(),
            Screen::Confirm(c) => {
                // sub_4AE90: Yes (object 0) at x 330, No (object 1) at 252, y 249.01 of 448 (261.46 under a
                // thumbnail); touch rect (0, -2)-(72, 24) (unk_590F08)
                let y = if c.thumbnail() { 261.46 } else { 249.01 };
                vec![
                    (Self::object_rect([330.0, y], [0, -2, 72, 24]), Action::Yes),
                    (Self::object_rect([252.0, y], [0, -2, 72, 24]), Action::No),
                ]
            }
            // sub_28A90: a tap left of x 543 (640x448 units, clear of the scroll bar) asks for the quick jump;
            // the button guide (sub_4A410: x >= 546, y > 393) closes
            // (the button guide's corner closes it: `back_btn_on`)
            Screen::Backlog { .. } => vec![([0.0, 0.0, 543.0 * 3.0, H], Action::BacklogJump)],
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

    /// Touch area of a sprite object (sub_AE840) in screen pixels: the object's rect (x0, y0)-(x1, y1) in
    /// 640x448 units from its position truncated to whole units, top-left anchored, bounds inclusive.
    fn object_rect(pos: [f32; 2], r: [i32; 4]) -> [f32; 4] {
        let (kx, ky) = (W / 640.0, H / 448.0);
        let (x, y) = (pos[0].trunc() + r[0] as f32, pos[1].trunc() + r[1] as f32);
        [x * kx, y * ky, (r[2] - r[0] + 1) as f32 * kx, (r[3] - r[1] + 1) as f32 * ky]
    }

    /// System menu tab `i` (objects 197..201, sub_4C370): touch rect (0, 0)-(74, 29) (unk_591438) at
    /// (26.667, 43.556 + 31.111 i).
    fn menu_tab_rect(i: usize) -> [f32; 4] {
        Self::object_rect([26.667, 43.556 + i as f32 * 31.111], [0, 0, 74, 29])
    }

    /// Slot rectangle on the canvas: x 272, y from unk_352AF4 (4 entries from 312.36, 5 from 298.67,
    /// step 23.23) in 640x448 units, drawn at native texel size.
    fn title_slot(i: usize, n: usize) -> [f32; 4] {
        let y0 = if n >= 5 { 298.66666 } else { 312.35553 };
        let (kx, ky) = (W / 640.0, H / 448.0);
        [272.0 * kx, (y0 + i as f32 * 23.229_63) * ky, 296.0, 72.0]
    }

    /// Source rect of a title entry in the given state (0 normal, 1 hover, 2 pressed), from the
    /// object frame tables at 0x65C7B0.
    fn title_src(normal: [f32; 4], state: usize) -> [f32; 4] {
        let [x, y, w, h] = normal;
        [x + 312.0 * state as f32, y, w, h]
    }

    /// Open the flowchart (cFlowchart): lay out the chart from the progress bits and put the cursor
    /// on the playthrough's current node.
    /// Chart availability: the gallery needs node 1 read (loc_14CFC); in game sub_1767C needs route node 1 and
    /// the current node outside the ending / "Teach me" list (unk_3374E8, 25 rows).
    fn flow_ok(&self, game: &Game, gallery: bool) -> bool {
        let fl = game.flow_flags();
        if gallery {
            fl.read.get(1).copied().unwrap_or(false)
        } else {
            let cur = game.current_node();
            let ending = |n: i32| n == 557 || crate::gallery::ENDINGS.iter().any(|e| e.0 as i32 == n);
            fl.route.get(1).copied().unwrap_or(false) && !ending(cur)
        }
    }

    fn open_flow(&mut self, game: &mut Game, gallery: bool) {
        self.open_flow_at(game, gallery, -1);
    }

    /// `open_flow` with the gallery's node (cGalleryMenu + 56, off_AE0288[1] on the way back from a replay).
    fn open_flow_at(&mut self, game: &mut Game, gallery: bool, node: i32) {
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let chart = flow::build(game.flow_flags(), gallery);
        self.flow_scroll = 0.0;
        self.flow_target = 0.0;
        self.flow_t = FLOW_SCROLL_MS;
        self.flow_cur_y = 0.0;
        self.flow_ref = 0.0;
        self.flow_manual = false;
        self.drag = touch::TouchDrag::default();
        // sub_F8FC(a3): the cursor starts on the record (0, 0), then on each shown row's first record with id
        // a3, the last row winning, each placement followed at once (sub_10BA4, sub_10D5C(0)). In game a3 is
        // the current node F110 (sub_1767C), so a playthrough at id 0 (no JUMP yet) ends on the last row
        // holding an empty record. The gallery passes cGalleryMenu + 56: the node a replay started from
        // (off_AE0288[1], sub_5040) when the gallery comes back from it, else -1.
        let a3 = if gallery { node } else { game.current_node() };
        let mut placed = Vec::new();
        self.flow_cell = chart.open_cursor(a3, |c| placed.push(c));
        self.flow = Some(chart);
        for c in placed {
            self.flow_cell = c;
            self.flow_place(false);
        }
        self.push(Screen::Flowchart { gallery });
        self.hover = self.flow_node();
        // the chart opens on its node without a flash or panel fade
        self.flow_flash = (self.hover, f32::NEG_INFINITY);
        self.flow_old = None;
    }

    /// The node under the flowchart cursor, if its record has one.
    fn flow_node(&self) -> Option<usize> {
        self.flow.as_ref().and_then(|c| c.node_at(self.flow_cell))
    }

    /// sub_10BA4 + sub_10D5C: the cursor sprite moves onto the cursor's node (when its record has one), and
    /// the view follows it.
    fn flow_place(&mut self, animate: bool) {
        if let Some(i) = self.flow_node() {
            self.flow_cur_y = self.flow.as_ref().map_or(0.0, |c| c.nodes[i].pos[1]);
        }
        self.flow_follow(animate);
    }

    /// Draw the screen below the current one at alpha `a`, with its content (the gallery mode's own sprites)
    /// at `gal_a` (fading out under the chart): the gallery frame and buttons around the flowchart's fades.
    fn draw_below(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, a: f32) {
        let Some(&below) = self.stack.last() else { return };
        let saved = (self.screen, self.hover, self.gal_a, self.scr_a, self.closing);
        (self.screen, self.hover, self.scr_a, self.closing) = (below, None, 1.0, false);
        let n = out.len();
        self.draw_screen(game, r, out);
        (self.screen, self.hover, self.gal_a, self.scr_a, self.closing) = saved;
        self.fade_quads(out, n, a);
    }

    /// Draw the screen below the current one (for dialogs and the CG viewer), without its guide.
    fn draw_under(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        // the screen below draws with its own stack below it (a dialog over LOAD GAME over the title)
        let Some(below) = self.stack.pop() else { return };
        let (screen, hover, under) = (self.screen, self.hover, self.under);
        self.screen = below;
        self.hover = match below {
            Screen::SaveLoad { .. } => self.stack_hover.get(self.stack.len()).copied().flatten(),
            _ => None,
        };
        self.under = true;
        self.draw(game, r, out);
        self.stack.push(below);
        self.screen = screen;
        self.hover = hover;
        self.under = under;
    }

    /// Gallery entry of picture `pos` of CG group `group`.
    fn cg_entry(&self, group: usize, pos: usize) -> Option<crate::gallery::Entry> {
        self.cg_unlocked.get(group).and_then(|v| v.get(pos)).and_then(|&i| crate::gallery::entry(group, i))
    }

    /// CG viewer L / R (sub_C56C: L or left, R or right, with at least two pictures): the previous / next
    /// unlocked picture of the whole gallery (cGallery + 88 in group order, wrapping) with sound 0 and the rule wipe (sub_CA54). sub_BE84 keeps the
    /// zoom and the position when the new picture has the same size, else it opens at the fit scale, centred.
    fn cg_step(&mut self, game: &mut Game, d: i32) {
        if self.cg_fx != CgFx::None {
            return;
        }
        if let Screen::CgView { group, pos } = self.screen {
            let flat = self.cg_flat();
            let n = flat.len() as i32;
            let Some(i) = flat.iter().position(|&p| p == (group, pos)) else { return };
            if n < 2 {
                return;
            }
            let (g2, p2) = flat[(i as i32 + d).rem_euclid(n) as usize];
            game.play_sys_sound(0);
            self.cg_fx = CgFx::Switch(group, pos, self.cg_zoom, self.cg_pan, d > 0);
            self.cg_t = 0.0;
            self.screen = Screen::CgView { group: g2, pos: p2 };
            let size = |e: Option<crate::gallery::Entry>| e.map(|e| (e.width, e.height));
            if size(self.cg_entry(group, pos)) != size(self.cg_entry(g2, p2)) {
                self.cg_zoom = 0.0;
                self.cg_pan = [0.0; 2];
            }
            self.cg_guide_show();
        }
    }

    /// cGalleryGuide sub_604C(-1, 0.5, 2.0): fade in over 0.5 s (quad in) unless already shown, then stay 2 s
    /// (counted again from here) before sub_5EA4 fades it out.
    fn cg_guide_show(&mut self) {
        self.cg_guide = match self.cg_guide.0 {
            1 => self.cg_guide,
            2 => (2, 0.0),
            _ => (1, 0.0),
        };
    }

    /// The viewer's guide alpha: quad in 0 -> 1, quad out 1 -> 0 (sub_5A6C), 0.5 s each.
    fn cg_guide_a(&self) -> f32 {
        let u = (self.cg_guide.1 / CG_FX_SECS).min(1.0);
        match self.cg_guide.0 {
            1 => u * u,
            2 => 1.0,
            3 => (1.0 - u) * (1.0 - u),
            _ => 0.0,
        }
    }

    /// Advance the viewer transition and its guide by `secs`.
    fn cg_update(&mut self, game: &Game, secs: f32) {
        if !matches!(self.screen, Screen::CgView { .. }) {
            return;
        }
        self.cg_guide.1 += secs;
        match self.cg_guide {
            (1, t) if t >= CG_FX_SECS => self.cg_guide = (2, 0.0),
            (2, t) if t >= 2.0 => self.cg_guide = (3, 0.0),
            (3, t) if t >= CG_FX_SECS => self.cg_guide = (0, 0.0),
            _ => {}
        }
        if self.cg_fx == CgFx::None {
            return;
        }
        self.cg_t += secs;
        if self.cg_t >= CG_FX_SECS {
            if self.cg_fx == CgFx::Close
                && let Screen::CgView { group, pos } = self.screen
            {
                self.pop();
                self.cg_viewed(game, group, pos);
            }
            self.cg_fx = CgFx::None;
        }
    }

    /// The viewer closed on picture `pos` of `group` (sub_D228 -> sub_7E24 -> sub_9384, run as the close starts,
    /// so the new grid has faded in under the closing viewer by its end): the picked group's own grid comes back
    /// (variations grid with the cursor on the picture, or the group grid on the group); another group with
    /// several unlocked pictures opens its variations grid on the picture, any other the group grid on it.
    fn cg_viewed(&mut self, game: &Game, group: usize, pos: usize) {
        let (picked, main_scroll, sub_scroll) = self.cg_pick;
        let sub = if picked == Some(group) {
            self.cg_sub.is_some()
        } else {
            self.cg_unlocked.get(group).is_some_and(|v| v.len() > 1)
        };
        if sub {
            self.cg_show(game, Some(group), pos, sub_scroll, false);
        } else {
            self.cg_show(game, None, group, main_scroll, false);
        }
        self.cg_pick.0 = Some(group);
        self.cg_in = CG_FX_SECS;
    }

    /// B, a guide tap or a strong vertical flick in the viewer (sub_C56C -> state 5, sub_CE60): sound 2, the
    /// guide fades out (sub_5EA4, 0.5 s) and the gallery capture fades back in over the picture.
    fn cg_close(&mut self, game: &mut Game) {
        if self.cg_fx != CgFx::None {
            return;
        }
        game.play_sys_sound(2);
        self.cg_fx = CgFx::Close;
        self.cg_t = 0.0;
        if matches!(self.cg_guide.0, 1 | 2) {
            self.cg_guide = (3, 0.0);
        }
    }

    /// CG viewer geometry (sub_BE84 / sub_D270) of a `w` x `h` picture at `zoom` (0 = the minimum) moved `pan`
    /// from the centre: the scale runs from the fit-to-screen ratio up to 1:1 (pictures smaller than the screen
    /// stay at the fit ratio); sub_D270 keeps a picture larger than the screen covering it along that axis and
    /// a smaller one inside it. Returns the rect with the clamped zoom and pan.
    fn cg_geom(w: f32, h: f32, zoom: f32, pan: [f32; 2]) -> ([f32; 4], f32, [f32; 2]) {
        let fit = (W / w).min(H / h);
        let max = fit.max(1.0);
        let s = if zoom <= 0.0 { fit } else { zoom.clamp(fit, max) };
        let (dw, dh) = (w * s, h * s);
        let (mx, my) = (((dw - W) / 2.0).abs(), ((dh - H) / 2.0).abs());
        let pan = [pan[0].clamp(-mx, mx), pan[1].clamp(-my, my)];
        ([(W - dw) / 2.0 + pan[0], (H - dh) / 2.0 + pan[1], dw, dh], s, pan)
    }

    /// `cg_geom` for the picture on view, keeping the clamped zoom and pan.
    fn cg_rect(&mut self, w: f32, h: f32) -> [f32; 4] {
        self.cg_size = [w, h];
        let (rc, zoom, pan) = Self::cg_geom(w, h, self.cg_zoom, self.cg_pan);
        self.cg_zoom = zoom;
        self.cg_pan = pan;
        rc
    }

    /// Keep the cursor node inside the chart view (sub_10D5C).
    /// sub_10D5C: when the cursor node is above y 2 the row before it goes to y 4; when it is below y 966 the
    /// row after it goes to y 966 (positions are node tops in the view).
    /// It measures the cursor sprite, which stays put while the cursor is on a record without a node, against
    /// `flow_ref`: the shown scroll, so while the 1/6 s tween of the last move runs (a held direction repeats
    /// every 5 frames) the next row is still below 966 and the view moves on by a row per step. The target
    /// it stores there is not clamped. sub_10BA4 skips it during a manual scroll.
    fn flow_follow(&mut self, animate: bool) {
        if self.flow_manual {
            return;
        }
        let Some(c) = &self.flow else { return };
        let Ok(row) = usize::try_from(self.flow_cell.1) else { return };
        if c.row_y.is_empty() {
            return;
        }
        let y = self.flow_cur_y - self.flow_ref;
        let target = if y < 2.0 {
            c.row_y[row.max(1) - 1] - 4.0
        } else if y > 966.0 {
            c.row_y[(row + 1).min(c.row_y.len() - 1)] - 966.0
        } else {
            return;
        };
        self.flow_ref = target.trunc();
        self.flow_scroll_to(self.flow_ref, animate);
    }

    /// A manual scroll (sub_11418) to `v`: from then on the cursor follows the view.
    fn flow_scroll_by_hand(&mut self, v: f32) {
        self.flow_scroll_to(v.trunc(), true);
        self.flow_manual = true;
    }

    /// One 60 Hz frame of the list touch drag (sub_40C) and what reads its speed (sub_6C4):
    /// - CG grid (sub_9CBC) and music list (sub_1A7FC), from a speed of 3: the scroll moves by it at once and
    ///   the cursor row is pulled back into view (CG 103..943, music 138..940) with sound 0;
    /// - config (sub_2B930): the tab's target moves by it (clamped) through the 12-frame tween;
    /// - flowchart (`flow_frame`).
    fn drag_frame(&mut self, game: &mut Game) {
        self.drag.frame();
        if self.busy() || self.popup.is_some() {
            return;
        }
        if self.bar_frame(game) {
            return;
        }
        let v = self.drag.vel;
        match self.screen {
            Screen::Flowchart { .. } => self.flow_frame(game),
            Screen::Gallery | Screen::Music if v.abs() >= 3.0 => {
                let before = self.hover;
                let max = self.gal_scroll_max();
                self.gal_scroll.to(self.gal_scroll.pos + v, max, false);
                self.gal_track = true;
                let _ = before;
            }
            Screen::Config if v.abs() > f32::EPSILON => {
                self.conf_scroll_to(self.conf_target[self.conf_tab] + v);
            }
            _ => {}
        }
    }

    /// The scroll widget's range and shown position on this screen (flowchart, CG grid, music list).
    fn bar_scroll(&self) -> Option<(f32, f32)> {
        match self.screen {
            Screen::Flowchart { .. } => self.flow.as_ref().map(|c| (c.scroll_max, self.flow_scroll)),
            Screen::Gallery | Screen::Music => Some((self.gal_scroll_max(), self.gal_scroll.pos)),
            _ => None,
        }
        .filter(|&(max, _)| max > 0.0)
    }

    /// A touch on the scroll widget's slider (sub_27974 via sub_25218) grabs it, keeping the touch y and the
    /// shown scroll; the touch then does nothing else.
    fn bar_grab(&mut self, y: f32) -> bool {
        let Some((max, pos)) = self.bar_scroll() else { return false };
        let top = (pos / max).clamp(0.0, 1.0) * 610.0 + 99.0;
        if !((1839.0..=1871.0).contains(&self.mouse.0) && (top..=top + 288.0).contains(&y)) {
            return false;
        }
        self.bar = Some((Self::bar_y(y), pos));
        true
    }

    /// Canvas y as sub_27974 reads it: whole 640x448 touch units scaled to 1079.
    fn bar_y(y: f32) -> f32 {
        ((y * 448.0 / H) as i32) as f32 / 447.0 * 1079.0
    }

    /// While the slider is held (sub_27974, each frame): the scroll heads for the grab's scroll plus the touch's
    /// move times range / 610 (clamped) over 0.2 s, ease-out quad, and the manual flag is set, so the cursor
    /// follows the view (flowchart sub_12D1C; CG grid / music list as after a stick scroll).
    fn bar_frame(&mut self, game: &mut Game) -> bool {
        let Some((y0, s0)) = self.bar else { return false };
        let Some((max, _)) = self.bar_scroll().filter(|_| self.touch.down) else {
            self.bar = None;
            return false;
        };
        let v = (s0 + (Self::bar_y(self.mouse.1) - y0) * max / 610.0).clamp(0.0, max);
        if matches!(self.screen, Screen::Flowchart { .. }) {
            self.flow_scroll_over(v, 200.0);
            self.flow_manual = true;
            self.flow_frame(game);
        } else {
            self.gal_scroll.over(v, max, 200.0);
            self.gal_track = true;
        }
        true
    }

    /// The flowchart's frame (sub_11418): the drag speed scrolls by 6 times itself from `flow_ref`; while a
    /// manual scroll runs and the chart can scroll, the cursor is kept in view every frame (sub_12D1C), and the
    /// flag drops once the tween has ended.
    fn flow_frame(&mut self, game: &mut Game) {
        let v = self.drag.vel;
        if v.abs() > f32::EPSILON {
            self.flow_scroll_by_hand(v * 6.0 + self.flow_ref);
        }
        if self.flow_manual {
            if self.flow.as_ref().is_some_and(|c| c.scroll_max > 0.0) {
                if self.flow_t >= self.flow_len {
                    self.flow_manual = false;
                }
                self.flow_keep_cursor(game);
            } else {
                self.flow_manual = false;
            }
        }
    }

    /// sub_27BD4: scroll the chart to `v` (clamped to its range), over 1/6 s or at once.
    fn flow_scroll_to(&mut self, v: f32, animate: bool) {
        self.flow_scroll_over(v, if animate { FLOW_SCROLL_MS } else { 0.0 });
    }

    /// sub_27BD4 / sub_27974: scroll the chart to `v` (clamped) over `ms`, at once for 0.
    fn flow_scroll_over(&mut self, v: f32, ms: f32) {
        let max = self.flow.as_ref().map_or(0.0, |c| c.scroll_max);
        self.flow_target = v.clamp(0.0, max);
        if ms > 0.0 {
            self.flow_from = self.flow_scroll;
            self.flow_t = 0.0;
            self.flow_len = ms;
        } else {
            self.flow_scroll = self.flow_target;
            self.flow_t = FLOW_SCROLL_MS;
            self.flow_len = FLOW_SCROLL_MS;
        }
    }

    /// sub_12D1C: during a manual scroll the cursor moves row by row until its node is inside y 4..994 of the
    /// view at `flow_ref` (the shown scroll), with sound 0 when it moved.
    fn flow_keep_cursor(&mut self, game: &mut Game) {
        let Some(c) = &self.flow else { return };
        let at = self.flow_ref;
        if let Some(cell) = c.keep_in_view(self.flow_cell, |i| c.nodes[i].pos[1] - at) {
            self.flow_cell = cell;
            if let Some(i) = c.node_at(cell) {
                self.flow_cur_y = c.nodes[i].pos[1];
            }
            self.snd(game, 0);
        }
        self.hover = self.flow_node();
    }

    /// Controls of the config screen with their top-left on the canvas (content scrolled).
    fn conf_layout(&self) -> Vec<(Ctl, [f32; 2])> {
        self.conf_layout_at(self.conf_scroll)
    }

    /// The config controls at scroll `scroll`; the positions are the grid's floats (dword_46CDB0 + 1160 + 40 row
    /// + 8 column, 640 x 448 units), exact so the scroll (sub_29930) truncates as on the Switch.
    fn conf_layout_at(&self, scroll: f32) -> Vec<(Ctl, [f32; 2])> {
        let u = |x: f32, y: f32| [x * 3.0, y * H / 448.0 - scroll];
        let mut v = vec![
            (Ctl::Tab(0), [408.0, 96.0]),
            (Ctl::Tab(1), [656.0, 96.0]),
            (Ctl::Tab(2), [888.0, 96.0]),
            (Ctl::Default, [1536.0, 96.0]),
        ];
        match self.conf_tab {
            0 => {
                let rows = [(100.8, 56.000004, 94.162964), (175.05185, 130.25185, 168.41481), (246.4, 201.59999, 0.0), (306.54816, 261.74817, 0.0)];
                for (i, (pm, sl, tg)) in rows.into_iter().enumerate() {
                    v.push((Ctl::Minus(i), u(245.0, pm)));
                    v.push((Ctl::Slider(i), u(120.666664, sl)));
                    v.push((Ctl::Plus(i), u(452.0, pm)));
                    if i < 2 {
                        v.push((Ctl::Toggle(i), u(475.0, tg)));
                    }
                }
                let radios = [(0, 364.62222, 384.94815), (1, 427.67407, 448.0), (2, 491.14075, 511.46667), (3, 557.0963, 577.42224)];
                for (k, yt, yf) in radios {
                    v.push((Ctl::Radio(k, true), u(243.66667, yt)));
                    v.push((Ctl::Radio(k, false), u(243.66667, yf)));
                    if k == 0 {
                        v.push((Ctl::Toggle(2), u(290.33334, 360.05927)));
                    }
                }
                // overseas: the language buttons (grid rows 12..15 at 0x35C378)
                if crate::assets::overseas() {
                    for (k, y) in [616.4148, 636.7407, 657.06665, 677.3926].into_iter().enumerate() {
                        v.push((Ctl::Lang(k), u(243.66667, y)));
                    }
                }
            }
            1 => {
                let rows = [(104.94815, 60.14815, 99.970375), (132.32593, 87.525925, 127.34815), (160.53334, 115.73334, 155.55556)];
                for (i, (y, sl, mute)) in rows.into_iter().enumerate() {
                    v.push((Ctl::Minus(i), u(245.33333, y)));
                    v.push((Ctl::Slider(i), u(120.99999, sl)));
                    v.push((Ctl::Plus(i), u(452.3333, y)));
                    v.push((Ctl::Mute(i), u(476.66666, mute)));
                    if i == 1 {
                        v.push((Ctl::Test(0), u(560.6667, 128.59259)));
                    }
                }
                for (k, yt, yf) in [(4, 236.44444, 258.84445), (5, 297.00742, 319.4074)] {
                    v.push((Ctl::Radio(k, true), u(242.66667, yt)));
                    v.push((Ctl::Radio(k, false), u(242.66667, yf)));
                }
                // character rows (sub_29FA0): 370.01 + 50.607 n over the shown rows
                for (n, &[k, ..]) in self.voice_rows.iter().enumerate() {
                    let y = n as f32 * 50.607 + 370.01;
                    v.push((Ctl::Minus(3 + k), u(245.0, y)));
                    v.push((Ctl::Slider(3 + k), u(120.67, y + -44.8)));
                    v.push((Ctl::Plus(3 + k), u(452.0, y)));
                    v.push((Ctl::Mute(3 + k), u(476.33, y + -4.9778)));
                    v.push((Ctl::Test(1 + k), u(560.33, y + -3.7333)));
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
            // the knob object (0x46B370 entries 81, 86, 90, 99 + 5k...): rect (144, 39)-(167, 63) of 640 x 448 inside
            // the conf_sbtn sprite, at value 0 (`items` adds the knob's travel)
            Ctl::Slider(_) => [x + 432.0, y + 39.0 * 1080.0 / 448.0, 69.0, 24.0 * 1080.0 / 448.0],
            Ctl::Toggle(t) if crate::assets::overseas() => {
                let [x0, _, x1, _] = en_toggle_cell(t, 0, crate::text::english());
                [x, y, x1 - x0, 72.0]
            }
            Ctl::Toggle(_) => [x + 2.0, y, 304.0, 72.0],
            Ctl::Radio(k, v) if crate::assets::overseas() => {
                let [x0, _, x1, _] = en_radio_cell(k, v, 0, crate::text::english());
                [x, y, x1 - x0, 48.0]
            }
            Ctl::Radio(k, v) => [x + 10.0, y, RADIO_CELLS[k][usize::from(!v)].1 - 10.0, 68.0],
            // the object rect (0, 7)-(56, 26) of 640 x 448
            Ctl::Lang(_) => [x, y + 7.0 * H / 448.0, 168.0, 19.0 * H / 448.0],
            Ctl::Mute(_) => [x, y, 136.0, 64.0],
            Ctl::Test(_) => [x, y, 104.0, 56.0],
        }
    }

    /// (value, maximum, step) of slider row i on the current tab: maxima from dword_46CDB0[628 tab + 602 + row];
    /// - / + and a held A with left / right step 1, or 50 for a maximum over 999 (sub_2B930).
    fn conf_slider(&self, game: &Game, i: usize) -> (f32, f32, f32) {
        let st = &game.settings;
        match (self.conf_tab, i) {
            // Witch on the Holy Night: its 0..255 text speed (`Game::chars_per_frame`)
            (0, 0 | 1) => (st.text_ms[i], if game.assets.build == knife_core::exe::Build::Witch { 255.0 } else { 250.0 }, 1.0),
            (0, 2) => (st.auto_page_ms, 10000.0, 50.0),
            (0, _) => (st.auto_click_ms, 10000.0, 50.0),
            (_, 0) => (game.sound.vol.bgm * 100.0, 100.0, 1.0),
            (_, 1) => (game.sound.vol.se * 100.0, 100.0, 1.0),
            (_, 2) => (st.sys_se * 100.0, 100.0, 1.0),
            (_, k) => (st.char_voice.get(k - 3).copied().unwrap_or(1.0) * 100.0, 100.0, 1.0),
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
        game.apply_volumes();
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
            // sub_2A550: the config word's button (1..3), else the first; a press selects its own at once
            // (sub_29500 type 10)
            Ctl::Lang(k) => {
                k == self.lang_pick.unwrap_or(match self.file.language() {
                    w @ 1..=3 => w as usize,
                    _ => 0,
                })
            }
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
                game.apply_volumes();
            }
            Ctl::Minus(i) | Ctl::Plus(i) | Ctl::Slider(i) => {
                let (v, _, step) = self.conf_slider(game, i);
                let nv = match c {
                    Ctl::Minus(_) => v - step,
                    Ctl::Plus(_) => v + step,
                    _ if d != 0 => v + step * d as f32,
                    _ => {
                        // a touch on the track (sub_2B930 case 4): (int)((x - 276.67) / 169.33 * max) in 640 units,
                        // the knob running 169.33 units (508 px) from the track start
                        let Some(&(_, p)) = self.conf_layout().iter().find(|(k, _)| *k == c) else { return };
                        let (_, max, _) = self.conf_slider(game, i);
                        ((self.mouse.0 - (p[0] + 468.0)) / 508.0 * max).trunc().max(0.0)
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
                4 => {
                    st.voice_on = v;
                    game.apply_volumes();
                }
                _ => st.voice_stop_on_advance = v,
            },
            Ctl::Mute(i) if i < 3 => {
                st.mute ^= 1 << i;
                game.sound.vol.bgm_mute = st.mute & 1 != 0;
                game.sound.vol.se_mute = st.mute & 2 != 0;
                game.apply_volumes();
            }
            Ctl::Mute(i) => {
                st.char_mute ^= 1 << (i - 3);
                game.apply_volumes();
            }
            // sub_2B930 object 85 -> sub_29140: a random test SE
            Ctl::Test(0) => self.conf_test_se = game.conf_test_se(self.conf_test_se.take()),
            // sub_2B930 case 1: a voice row's test button plays the row's last voice (G031 + 2 row) on player 4 at
            // the row's volume, unless the row is muted; a row without one yet clears the held row
            // started by `Action::Setting` (sub_2B930 -> sub_2D720)
            Ctl::Lang(_) => {}
            Ctl::Test(t) => {
                let k = t - 1;
                let g = |n: usize| game.var(&format!("G{n:03}"));
                let lo = g(31 + 2 * k);
                let id = (lo & 0xFFFF) | g(32 + 2 * k) << 16;
                self.conf_test_row = None;
                if g(crate::game::VOICE_ROW_WORDS[k]) != 0 && lo >= 1 {
                    self.conf_test_row = Some(k);
                    if id >= 0 && game.settings.char_mute >> k & 1 == 0
                        && let Some(name) = crate::voice_ids::name(id)
                    {
                        game.conf_test_voice(name, k);
                    }
                }
            }
        }
        // sub_2A9B0 after a change: the held voice row's mute and volume reach the scene voice
        if !matches!(c, Ctl::Tab(_) | Ctl::Default | Ctl::Test(0) | Ctl::Lang(_))
            && let Some(k) = self.conf_test_row
        {
            game.conf_test_row_volume(k);
        }
    }

    /// One 60 Hz frame of a held config control (sub_2B930 with A held, or a touch held on the object under the
    /// cursor): - / + wait out dword_46F81C (20 frames from the press), then step every frame; a slider under a
    /// held touch follows its x every frame. The cursor follows the touch, so the control is whichever one the
    /// finger is on now.
    fn conf_hold_frame(&mut self, game: &mut Game) {
        let ctl = if self.screen != Screen::Config || self.side || self.busy() || self.popup.is_some() {
            None
        } else if self.touch.down {
            self.items(game)
                .iter()
                .find(|(rc, _)| rect_hit(*rc, self.mouse))
                .and_then(|(_, a)| if let Action::Setting(c, _) = a { Ctl::from_code(*c) } else { None })
        } else if self.enter_held {
            self.hover.and_then(|h| self.conf_layout().get(h).map(|(c, _)| *c))
        } else {
            None
        };
        match ctl {
            Some(c @ (Ctl::Minus(_) | Ctl::Plus(_))) => {
                if self.conf_hold >= 1 {
                    self.conf_hold -= 1;
                } else {
                    self.conf_change(game, c.code(), 0);
                    self.conf_hold_dirty = true;
                }
            }
            Some(c @ Ctl::Slider(_)) if self.touch.down => {
                self.conf_change(game, c.code(), 0);
                self.conf_hold_dirty = true;
            }
            Some(_) => {}
            None if std::mem::take(&mut self.conf_hold_dirty) => self.store_global(game),
            None => {}
        }
    }

    /// Draws a number right-aligned with its last digit at `x` (sub_2D420: digits 24 px apart,
    /// sub_2CE40: config_number_ja source rects (2 + 32 d, 0, 24, 32) from the table at off_AE0538).
    fn conf_number(&self, out: &mut Vec<Quad>, n: u32, x: f32, y: f32) {
        let mut n = n;
        let mut x = x;
        loop {
            let d = (n % 10) as f32;
            self.sprite(out, "config_number_ja", [x, y, 24.0, 32.0], Some([2.0 + d * 32.0, 0.0, 24.0, 32.0]));
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
        // sub_2B460: slots 58 / 59 at 0.74 of the 60 px base glyph, laid out like the messages (`text::layout`:
        // advance + 4 px character spacing; English: sub_A7790's 0.9 and the font's kerning)
        let size = 60.0 * 0.74 * gs();
        let english = crate::text::english();
        for (k, row) in [126.93, 201.19].into_iter().enumerate() {
            let text = self.sysmes.get(138 + k).cloned().unwrap_or_default();
            let [p, shown, _] = self.conf_prev[k];
            let y = row * H / 448.0 - self.conf_scroll;
            let font = game.font_mut();
            let mut x = 252.333 * 3.0;
            let mut prev = None;
            for (i, c) in text.chars().enumerate() {
                let i = i as f32;
                if english {
                    x += font.kern(prev.replace(c), c, size);
                }
                let a = if p - shown > 1e-3 { ((p - i) / (p - shown)).clamp(0.0, 1.0) } else if i < p { 1.0 } else { 0.0 };
                if a > 0.0
                    && let Some(q) = font.quad(r, c, size, x, y + crate::text::msg_ascent(crate::game::ASCENT * 0.74 / 0.76), [1.0, 1.0, 1.0, a])
                {
                    // the messages' dark rim (sub_A7790 draws both)
                    let mut q = crate::text::FontAtlas::rim(q, crate::text::edge_rim(1, size), crate::text::RIM_ALPHA);
                    q.clip = Some(clip);
                    out.push(q);
                }
                x += font.advance(c, size) + 4.0;
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
                // (sprite 1230, overseas 1233: the overseas system_bg is taller, with the language row)
                let [x, y] = u(128.667, 68.03);
                let (w, h) = self.tex.get("system_bg").map_or((1432.0, 1336.0), |t| (t.width as f32, t.height as f32));
                self.sprite(out, "system_bg", [x, y, w, h], None);
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
                    let nm = if game.assets.language() == 1 { NAME_RECTS_EN } else { NAME_RECTS }[name];
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
                Ctl::Toggle(t) if crate::assets::overseas() => {
                    let [x0, y0, x1, y1] = en_toggle_cell(t, col as usize, crate::text::english());
                    self.sprite(q, "btn_base2", [x, y, x1 - x0, y1 - y0], Some([x0, y0, x1 - x0, y1 - y0]));
                }
                Ctl::Toggle(t) => {
                    let row = if t < 2 { 0.0 } else { 80.0 };
                    // frames 0x46F4B0..: offset (-10/3, 0) of 640 x 448, source from x -2 (the cell's 0 at x - 8)
                    self.sprite(q, "btn_base2", [x - 8.0, y, 304.0, 72.0], Some([col * 304.0, row, 304.0, 72.0]));
                }
                Ctl::Radio(k, v) if crate::assets::overseas() => {
                    let [x0, y0, x1, y1] = en_radio_cell(k, v, col as usize, crate::text::english());
                    self.sprite(q, "btn_base1", [x, y, x1 - x0, y1 - y0], Some([x0, y0, x1 - x0, y1 - y0]));
                }
                Ctl::Lang(k) => {
                    // texture 1242 (conf_parts #19): a row of 88 per language, columns 256 apart
                    let src = [col * 256.0, 88.0 * k as f32, 248.0, 80.0];
                    self.sprite(q, CONF_LANG_TEX, [x, y, 248.0, 80.0], Some(src));
                }
                Ctl::Radio(k, v) => {
                    let (cy, w) = RADIO_CELLS[k][usize::from(!v)];
                    // the sheet draws the selected OFF 4 px higher; its frame (0x46F490) reads from y 90, not 94
                    let cy = if cy == 94.0 && col == 2.0 { 90.0 } else { cy };
                    // frames 0x46F3F0..: offset (-10/3, -4.148) of 640 x 448 (-10, -10 px), source from x -10
                    self.sprite(q, "btn_base1", [x, y - 10.0, w - 10.0, 68.0], Some([col * 256.0, cy, w - 10.0, 68.0]));
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
            rest: (self.sys_tab, self.side, self.gal_side, self.gal_tab, self.flow_cell, self.music_row, self.music_btn, self.conf_bar),
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
        // the WoH screens: a pointer that has not moved (the events a trackpad scroll sends) is no hover, so
        // it does not take the cursor back to the entry under it
        if self.screen == Screen::Witch || (self.screen == Screen::Title && crate::assets::witch()) {
            if (x, y) == self.mouse {
                return;
            }
            self.mouse = (x, y);
            self.touch_move();
            self.witch_mouse_move();
            return;
        }
        let before = self.cursor();
        self.sounded = false;
        self.mouse_move_inner(game, x, y);
        if self.screen != Screen::Game {
            self.cursor_sound(game, before);
        }
    }

    fn mouse_move_inner(&mut self, game: &mut Game, x: f32, y: f32) {
        let moved = (x, y) != self.mouse;
        if moved {
            self.mouse = (x, y);
            self.touch_move();
        }
        if self.screen == Screen::Game {
            game.mouse_move(x, y, self.touch.panel == touch::Panel::Closed);
            return;
        }
        // the system menu: the pointer moves the tab cursor like the other menus' (the Switch's tap moves it and
        // opens the tab at once, so only a click opens here); off the tabs the cursor stays, as a touch outside
        // them does nothing (sub_4CEA0)
        if self.screen == Screen::Menu {
            if let Some(i) = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)))
                && (Some(i) != self.hover || !self.sys_focus)
            {
                self.sys_tab = i;
                self.sys_focus = true;
                self.hover = Some(i);
                self.key_focus = false;
            }
            return;
        }
        // the tabs stay live under the save / load / config pages (sub_4D580 runs sub_4CEA0): hovering one hands
        // the cursor to the tabs as pad Left does (enter_side) and moves the tab cursor; a click opens it
        if matches!(self.screen, Screen::SaveLoad { .. } | Screen::Config)
            && self.stack.last() == Some(&Screen::Menu)
            && let Some(i) = (0..5).find(|&i| rect_hit(Self::menu_tab_rect(i), (x, y)))
        {
            if !self.side {
                self.enter_side();
            }
            self.sys_tab = i;
            self.sys_focus = true;
            self.key_focus = false;
            return;
        }
        let mut hover = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)));
        if self.screen == Screen::Title && !self.press {
            // sub_8E200 state 17: off the entries the cursor stays; moving onto another one plays sound 0
            // (sub_AE0E0 / sub_AE0B0 only run there, so the cursor stays put until the fade-in has ended)
            hover = hover.or(self.hover);
            if self.busy() {
                hover = self.hover;
                self.sounded = true;
            }
        }
        // touch only moves these cursors onto their own objects; anywhere else (a system menu tab, the back
        // area) they stay, so the slot or control a page opened on (the free slot, the newest save, the
        // remembered config control) keeps its highlight: sub_50FC0 follows only touched slots 8..11,
        // sub_29770 only the config's own objects
        let mut page_hit = hover.is_some();
        if let Screen::SaveLoad { .. } = self.screen {
            // a pointer that has not moved (the events a trackpad scroll sends) leaves a scrolled cursor be
            if !moved && self.key_focus {
                return;
            }
            let items = self.items(game);
            let slot = hover.filter(|&h| matches!(items[h].1, Action::Slot(_)));
            page_hit = slot.is_some();
            hover = slot.or(self.hover);
        } else if self.screen == Screen::Config {
            hover = hover.or(self.hover);
        }
        // with the tabs focused, pointing at the page's own objects hands the cursor back to the page
        if hover == self.hover && (self.key_focus || (self.side && !page_hit)) {
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
            && let Some(cell) = self.flow.as_ref().and_then(|c| c.cell_of(h))
        {
            self.flow_cell = cell;
            self.flow_cur_y = self.flow.as_ref().map_or(0.0, |c| c.nodes[h].pos[1]);
        }
    }

    pub fn click(&mut self, game: &mut Game, r: &Renderer) {
        self.skip_banner();
        if self.popup_input() || self.title_after_banner {
            return;
        }
        if self.screen == Screen::Game {
            if self.touch.scene_cd.is_none() {
                game.advance();
            }
            return;
        }
        if self.busy() {
            return;
        }
        if self.screen == Screen::Title && crate::assets::witch() {
            self.wt_click();
            return;
        }
        if self.screen == Screen::Intro || (self.screen == Screen::Title && self.press) {
            self.intro_input(game);
            return;
        }
        // sub_4A410: with the back button fully in (state 2), a touch at x >= 546, y > 393 (640x448 units)
        // presses B
        if self.back_btn >= 1.0 && self.back_btn_on() && self.mouse.0 >= 546.0 * 3.0 && self.mouse.1 > 393.0 * H / 448.0 {
            self.cancel(game, r);
            return;
        }
        // sub_50FC0: a touch confirms only on the cursor's own row object (8 + cursor, sub_AE0B0), so on an
        // empty list only the first frame's place shows the popup (Switch: a tap lower down does nothing)
        let [fx, fy] = Self::save_frame(0);
        if self.screen == (Screen::SaveLoad { save: false, scroll: 0 })
            && self.save_list(false).is_empty()
            && rect_hit([fx + 32.0, fy + 28.0, 898.0, 212.0], self.mouse)
        {
            game.play_sys_sound(2);
            self.popup(game, 193, 480.0);
            return;
        }
        // the system menu tabs stay tappable under its save / load / config pages (sub_4D580 runs sub_4CEA0):
        // a tap opens that tab (the open one too, which restarts it)
        if matches!(self.screen, Screen::SaveLoad { .. } | Screen::Config)
            && self.stack.last() == Some(&Screen::Menu)
            && let Some(i) = (0..5).find(|&i| rect_hit(Self::menu_tab_rect(i), self.mouse))
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
                self.sys_focus = true;
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
        if self.popup_input() || self.dlg_holding() {
            return;
        }
        match self.screen {
            // a story movie takes only B / Plus (sub_40480)
            Screen::Game if game.in_movie() => game.skip_movie(),
            // sub_40ED0: nothing until the first message after a story start or a load (dword_AFFCA4)
            Screen::Game if game.menu_lock => {}
            Screen::Game if crate::assets::witch() => self.wm_open(game, r),
            Screen::Witch => self.w.inp.pressed |= witch::pad_bit(Pad::B),
            Screen::Game => {
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                // sub_40ED0: the system menu opens with sound 3
                game.play_sys_sound(3);
                self.push(Screen::Menu);
                self.menu_bg.fade(0.0555);
            }
            Screen::Title if crate::assets::witch() => self.wt_pad(game, r, Pad::B, false),
            Screen::Title | Screen::Intro => self.intro_input(game),
            // B in a gallery mode's content gives the focus back to the mode buttons; on the buttons it leaves
            s if Self::gal_mode(s).is_some() && !self.gal_side && !self.busy() => {
                game.play_sys_sound(2);
                // sub_8D08: B in a variations grid makes the group grid again on the picked group, crossfading
                if let (Screen::Gallery, Some(g)) = (s, self.cg_sub) {
                    self.cg_show(game, None, g, self.cg_pick.1, true);
                } else {
                    self.gal_to_side(game);
                }
            }
            // sub_4CEA0: B (or a left / up swipe) closes the system menu and moves the remembered touch to y 398
            Screen::Menu => {
                if !self.busy() {
                    self.touch.at[1] = 398.0;
                }
                self.act(game, r, Action::Back);
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
            if self.touch.scene_cd.is_some() {
                return;
            }
            if dy != 0 {
                game.choice_move(dy, self.nav_repeat);
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
        // A acts on the record under the cursor, also a filler without a node (sub_11418)
        if enter && matches!(self.screen, Screen::Flowchart { .. }) && self.flow_node().is_none() {
            self.flow_press(game);
            return;
        }
        if self.side {
            self.side_nav(game, r, dy, dx, enter);
            return;
        }
        if enter && matches!(self.screen, Screen::Backlog { .. }) {
            self.act(game, r, Action::BacklogJump);
            return;
        }
        if self.screen == Screen::Menu {
            // sub_4CEA0: up / down over the five tabs, wrapping; A opens the tab under the cursor
            // A acts on the cursor without moving the focus onto it
            if dy != 0 {
                self.sys_tab = (self.sys_tab as i32 + dy).rem_euclid(5) as usize;
                self.sys_focus = true;
            }
            self.hover = self.sys_focus.then_some(self.sys_tab);
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
            // a list of the saves: the cursor moves one entry at a time and scrolls the four visible frames;
            // past either end a fresh press wraps to the other (sub_50FC0)
            if dy != 0 {
                let n = self.save_list(save).len();
                if n > 0 {
                    let cur = (scroll + self.hover.filter(|&h| h < SLOTS_PER_PAGE).unwrap_or(0)) as i32;
                    let next = if (0..n as i32).contains(&(cur + dy)) || self.nav_repeat {
                        (cur + dy).clamp(0, n as i32 - 1) as usize
                    } else if dy < 0 {
                        n - 1
                    } else {
                        0
                    };
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
        if let Screen::Backlog { scroll } = self.screen {
            // sub_28A90: up / down (d-pad, right stick; held repeats) one page older / newer, a fresh press past
            // the oldest / newest wraps to the other end; left goes to the oldest page, right to the newest
            let max = game.backlog_len().saturating_sub(1);
            let fresh = !self.nav_repeat;
            let next = if dy < 0 {
                if scroll < max { scroll + 1 } else if fresh { 0 } else { scroll }
            } else if dy > 0 {
                if scroll > 0 { scroll - 1 } else if fresh { max } else { 0 }
            } else if dx < 0 && fresh {
                max
            } else if dx > 0 && fresh {
                0
            } else {
                scroll
            };
            self.backlog_to(game, next);
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
        if matches!(self.screen, Screen::CgView { .. }) {
            // sub_C56C: left / right (and L / R) switch pictures unless up / down came with them; the sticks
            // pan and zoom (`stick_frame`); A does nothing
            if dx != 0 && dy == 0 {
                self.cg_step(game, dx);
            }
            return;
        }
        if matches!(self.screen, Screen::Confirm(_)) && (dx != 0 || dy != 0) {
            // sub_49F00 state 2: any direction (pad or stick, mask 0xF000F0) toggles between No and Yes, so left
            // from No goes to Yes (index 0 Yes, 1 No)
            self.hover = Some(usize::from(self.hover != Some(1)));
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
            self.sys_focus = true;
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
                for k in 0..4 {
                    rows.push(row(&[Ctl::Lang(k)]));
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
        if let Some((c, [_, y])) = self.hover.and_then(|h| self.conf_layout_at(0.0).get(h).copied())
            && !matches!(c, Ctl::Tab(_) | Ctl::Default)
        {
            let knob = self.conf_tab == 1 && matches!(c, Ctl::Slider(i) if i >= 3);
            let (top, bottom) = if knob { (y - 260.0 + 108.0, y - 870.0 + 98.0) } else { (y - 260.0, y - 870.0) };
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
            0 if crate::assets::overseas() => CONF_MAX_SYSTEM_EN,
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
                // sub_2C380: the focus goes to the new tab's remembered row / column (sub_29770); a tab left on
                // the tab bar (row -1, 操作説明 always) takes the last tab-bar object (LODWORD(qword_AED328)), so
                // R onto 操作説明 lights システム設定 with its help (Switch)
                let layout = self.conf_layout();
                let rows = self.conf_rows(&layout);
                self.hover = Some(match self.conf_cur[t].filter(|&(r, _)| r < rows.len()) {
                    Some((r, c)) => rows[r][c.min(rows[r].len() - 1)],
                    None => self.conf_bar,
                });
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

    /// L / R buttons: the config switches tabs (sub_29F00; the cursor follows once the tab shows);
    /// elsewhere they act as left / right.
    /// Backlog page `next` (0 = the newest): sound 0 when it changes (sub_28A90 LABEL_38).
    pub(super) fn backlog_to(&mut self, game: &mut Game, next: usize) {
        if let Screen::Backlog { scroll } = &mut self.screen
            && *scroll != next
        {
            *scroll = next;
            self.snd(game, 0);
        }
    }

    pub fn shoulder(&mut self, game: &mut Game, r: &Renderer, d: i32) {
        // the backlog (sub_28A90): mask [8] (L | ZR) to the oldest page, [9] (R) to the newest
        if matches!(self.screen, Screen::Backlog { .. }) {
            if !self.busy() {
                let next = if d < 0 { game.backlog_len().saturating_sub(1) } else { 0 };
                self.backlog_to(game, next);
            }
            return;
        }
        // the save list (sub_50FC0): L to the first save, R to the last
        if let Screen::SaveLoad { save, .. } = self.screen
            && !self.side
            && !self.busy()
            && self.popup.is_none()
        {
            let n = self.save_list(save).len();
            if n > 0 {
                let before = self.cursor();
                self.sounded = false;
                let next = if d < 0 { 0 } else { n - 1 };
                let scroll = next.saturating_sub(SLOTS_PER_PAGE - 1);
                self.screen = Screen::SaveLoad { save, scroll };
                self.hover = Some(next - scroll);
                self.key_focus = true;
                self.cursor_sound(game, before);
            }
            return;
        }
        if self.screen == Screen::Config && !self.busy() && self.popup.is_none() {
            self.side = false;
            // sub_29F00 only moves the tab glow (sub_ADE80); the focus stays until the fade-out ends
            let tab = (self.conf_next.unwrap_or(self.conf_tab) as i32 + d).rem_euclid(3) as usize;
            self.conf_switch(game, tab);
            self.store_global(game);
        } else {
            self.key_nav(game, r, 0, d, false);
        }
    }

    /// Flowchart cursor (sub_11418): left / right sub_1262C / sub_1280C, up / down sub_129F4 / sub_12B68.
    fn flow_move(&mut self, dx: i32, dy: i32) {
        let Some(c) = &self.flow else { return };
        let next = match (dx.signum(), dy.signum()) {
            (-1, _) => c.move_left(self.flow_cell),
            (1, _) => c.move_right(self.flow_cell),
            (_, -1) => c.move_up(self.flow_cell),
            (_, 1) => c.move_down(self.flow_cell),
            _ => None,
        };
        if let Some(cell) = next {
            self.flow_cell = cell;
            self.flow_place(true);
            self.hover = self.flow_node();
        }
    }

    /// Mouse wheel / trackpad (`lines` notches, + up): the left stick tilted to `WHEEL_TILT` for
    /// `WHEEL_FRAMES` frames a notch, read like the stick everywhere (`stick_frame`), except in the save list,
    /// which steps a save at once per `rows` (a wheel notch, or a trackpad drag of one frame's height on
    /// screen), like the stick's steps (a held tilt there steps at a fixed rate and runs on after the trackpad
    /// stops). It has no Switch control.
    pub fn wheel(&mut self, lines: f32, rows: f32, trackpad: bool) {
        // Witch on the Holy Night reads the exe's own mouse wheel (`WIn::wheel`), not the stick; the backlog
        // steps by the travel instead (a notch an entry; the trackpad's travel over an entry's height on screen)
        if crate::assets::witch() {
            self.w.inp.wheel(lines);
            if self.screen == Screen::Witch && self.w.scr == witch::WScr::Backlog {
                self.w.inp.wheel_rows(if trackpad { rows * SAVE_WHEEL_ROW / WITCH_LOG_ROW } else { rows });
            }
            return;
        }
        if self.busy() || self.popup.is_some() {
            self.wheel = 0.0;
            self.wheel_steps = 0.0;
            return;
        }
        if matches!(self.screen, Screen::SaveLoad { .. }) && !self.side {
            if rows == 0.0 {
                return;
            }
            if self.wheel_steps != 0.0 && self.wheel_steps.signum() != rows.signum() {
                self.wheel_steps = 0.0;
            }
            self.wheel_steps += rows;
            return;
        }
        if self.wheel != 0.0 && self.wheel.signum() != lines.signum() {
            self.wheel = 0.0;
        }
        self.wheel = (self.wheel + lines * WHEEL_FRAMES).clamp(-8.0 * WHEEL_FRAMES, 8.0 * WHEEL_FRAMES);
    }

    /// Input is held while a screen fades out, the screen goes black for a transition, the gallery
    /// crossfades between modes, or the title fades in from black (sub_8E200 takes input only from state 17,
    /// after state 2's fade).
    fn busy(&self) -> bool {
        self.closing
            || self.dlg_holding()
            || (self.screen == Screen::Title && self.black_speed < 0.0)
            || (self.screen == Screen::Gallery && self.cg_in < CG_FX_SECS)
            || self.cg_fx != CgFx::None
            || self.black_then.is_some()
            || self.gal_next.is_some()
            || self.flow_open.is_some()
            || (self.screen == Screen::Menu && self.menu_a < 1.0)
            || self.cont_slot.is_some()
            || self.load_popup.is_some()
            || self.save_wait.is_some()
            || self.delete_wait.is_some()
            || (self.screen == Screen::Config && (self.conf_next.is_some() || self.conf_ca < 1.0))
            || self.lang_seq.is_some()
            || self.lang_cool > 0
    }

    /// An answered confirm dialog still up: input waits for dword_B34B54 (sub_496F0).
    fn dlg_holding(&self) -> bool {
        self.dlg_out.is_some_and(|o| o.hold > 0.0)
    }

    /// The save list (sub_50790): every used slot, newest first; when saving, the first free slot comes
    /// first as an empty frame (the quick / auto save slot is never offered).
    fn save_list(&self, save: bool) -> Vec<usize> {
        if let Some(order) = self.save_order.as_ref().filter(|_| save) {
            return order.clone();
        }
        let mut used: Vec<(u64, usize)> =
            self.slots.iter().enumerate().take(save::list_slots()).filter_map(|(i, s)| s.as_ref().map(|s| (s.timestamp, i))).collect();
        used.sort_by(|a, b| b.cmp(a));
        let mut v = Vec::new();
        if save && let Some(free) = (0..save::list_slots()).find(|&i| self.slots.get(i).is_none_or(|s| s.is_none())) {
            v.push(free);
        }
        v.extend(used.into_iter().map(|(_, i)| i));
        v
    }

    /// Scrolls the save list `d` rows (no Switch control: the wheel / trackpad), keeping the cursor on its
    /// entry while that stays in view, else on the nearest visible row.
    fn save_scroll_by(&mut self, game: &mut Game, d: i32) {
        let Screen::SaveLoad { save, scroll } = self.screen else { return };
        let n = self.save_list(save).len();
        let max = n.saturating_sub(SLOTS_PER_PAGE);
        let next = (scroll as i32 + d).clamp(0, max as i32) as usize;
        if next == scroll {
            return;
        }
        let was = scroll + self.hover.filter(|&h| h < SLOTS_PER_PAGE).unwrap_or(0);
        let cur = was.clamp(next, next + SLOTS_PER_PAGE - 1);
        self.screen = Screen::SaveLoad { save, scroll: next };
        self.hover = Some(cur - next);
        self.key_focus = true;
        // sound 0 only when the cursor itself moves (sub_50FC0)
        if cur != was {
            game.play_sys_sound(0);
        }
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
                    game.sound.stop_bgm_rate(1.5);
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
                game.reset();
                // with node 485 read the title's full white goes on as the story's (sub_47B90(1.0))
                if self.black_col == [1.0; 3] {
                    game.white = 1.0;
                }
                self.black_col = [0.0; 3];
                game.start_scene(0, None);
                self.screen = Screen::Game;
                self.stack.clear();
                self.menu_bg = MenuBg::new();
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
            // sub_4CEA0 cases 0 / 1: sound 2 while saving is off (F025, sub_47B10)
            Action::OpenLoad | Action::OpenSave if self.screen == Screen::Menu && game.var("F025") != 0 => self.snd(game, 2),
            Action::OpenLoad => {
                self.refresh_slots(game);
                // title case 2 / system menu tab 1 (sub_4CEA0; sound 2 while saving is disabled, sub_47B10); WoH's
                // title case 2 sound 0
                self.snd(game, if crate::assets::witch() && self.screen == Screen::Title { 0 } else { 1 });
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
                // title case 3 / system menu tab 2: system sound 1 (WoH's title case 3: 0)
                self.snd(game, if crate::assets::witch() && self.screen == Screen::Title { 0 } else { 1 });
                if self.screen == Screen::Title {
                    self.menu_bg.fade(0.02);
                }
                self.conf_prev = [[0.0; 3]; 2];
                self.conf_test_se = None;
                self.conf_test_row = None;
                self.push(Screen::Config);
                self.refresh_voice_rows(game);
                // sub_2B680: the open tab's scroll comes back without a tween; the scroll widget fades in with
                // the screen unless that tab does not scroll
                self.conf_sb_a = 0.0;
                self.conf_ca = 1.0;
                self.conf_next = None;
                self.conf_show_tab();
                // the cursor comes back where it was on the open tab (sub_2B680), on the tab bar for a tab
                // left there
                self.conf_bar = self.conf_tab;
                let layout = self.conf_layout();
                let rows = self.conf_rows(&layout);
                self.hover = Some(match self.conf_cur[self.conf_tab].filter(|&(r, _)| r < rows.len()) {
                    Some((r, c)) => rows[r][c.min(rows[r].len() - 1)],
                    None => self.conf_tab,
                });
            }
            // sub_4CEA0 case 3 during a gallery flowchart replay (sub_50F0): dialog 12 instead of the chart
            Action::OpenFlow if self.screen == Screen::Menu && self.gal_ret[0] == 2 => self.confirm(game, Confirm::EndReplay),
            Action::OpenFlow => {
                let gallery = !matches!(self.screen, Screen::Menu);
                if !gallery {
                    // sub_4CEA0 case 3: sound 1 and the menu fades out over 12 frames (sub_4D8C0 state 14) before
                    // the chart task runs (state 15), which checks availability itself (sub_1767C)
                    self.snd(game, 1);
                    self.flow_open = Some(false);
                } else if self.flow_ok(game, true) {
                    self.snd(game, 1);
                    // sub_6934: cGalleryMenu + 60 keeps the mode shown before
                    self.gal_prev = Self::gal_mode(self.screen).map_or(-1, |m| m as i32);
                    if self.screen == Screen::Music {
                        // the music player closes too (vt + 48 -> sub_19AC4): its track stops at once
                        self.playing_track = None;
                        self.music_paused = false;
                        self.music_fade = None;
                        game.sound.stop_bgm_rate(3333.3);
                    }
                    // sub_8400 (a1 + 48 == 2): the content starts fading out, the chart is made at once
                    // (sub_7F38) and the frame and buttons tween 1 -> 0 over 0.5 s as it fades in (state 5)
                    self.open_flow(game, true);
                } else {
                    self.snd(game, 2);
                    self.popup(game, 1496, 480.0);
                }
            }
            Action::Node(n) => {
                // a tapped node takes the cursor first
                if let Some(chart) = &self.flow
                    && let Some(i) = chart.nodes.iter().position(|x| x.node == i32::from(n))
                    && let Some(cell) = chart.cell_of(i)
                {
                    self.flow_cell = cell;
                    self.flow_cur_y = chart.nodes[i].pos[1];
                }
                self.flow_press(game);
            }
            Action::GalleryBack => self.gallery_back(game, r),
            // Witch on the Holy Night (jp_48AC0 mode 2 state 2, mode 5 state 10): a story ended by a TeaTime's
            // archive button (G009) opens the Archive (jp_47AE0) under the title's BGM at once
            Action::TitleNow if crate::assets::witch() && game.var("G009") != 0 => {
                self.black_speed = 0.0;
                self.store_global(game);
                game.reset();
                game.play_bgm(if crate::ui::witch::title_complete(game) { 42 } else { 65 }, 1.0, 0.0);
                self.title_movie = false;
                self.black = 0.0;
                self.warc_init(game, false);
            }
            Action::TitleNow => {
                self.black_speed = 0.0;
                // nothing on the way to the title stops the BGM (sub_48AC0 case 2 state 2): the script's end already
                // stopped it unless RTMN / RTM2 kept it, and the title's sub_9C470(2) leaves a playing BGM 2 alone,
                // so the first boot's prologue BGM goes on under PRESS ANY BUTTON
                self.go_to_title(game, true);
            }
            Action::OpenExtra => {
                // cGallery state 0: the menu takes [0] as its first mode (sub_6124, 0 when unset) and [2] as its
                // mode before, then [0] and [2] go to -1
                self.gal_first = self.gal_ret[0];
                self.gal_prev = self.gal_ret[2];
                self.gal_ret[0] = -1;
                self.gal_ret[2] = -1;
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
                    match game.assets.ui_textures(r, &game.assets.ui_atlas("GALLERY_PARTS")) {
                        Ok(list) => self.tex.extend(list),
                        Err(e) => tracing::warn!("gallery parts: {e:#}"),
                    }
                }
                self.black = 0.0;
                self.black_speed = 0.0;
                self.black_under = false;
                self.menu_bg.fade(0.04);
                // the first mode (cGalleryMenu + 48): the one the gallery last closed on
                match std::mem::replace(&mut self.gal_first, -1) {
                    m @ (1 | 3 | 4) => self.act(game, r, Self::hub_items()[m as usize].1),
                    2 => {
                        self.act(game, r, Action::OpenGallery);
                        self.gal_tab = 2;
                        self.open_flow_at(game, true, -1);
                    }
                    _ => self.act(game, r, Action::OpenGallery),
                }
            }
            Action::OpenMusic => {
                use crate::game::get_bit;
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                // cGalleryMusic (sub_19518) first marks the BGM playing (sub_9C460: the last one started, here
                // the gallery's) heard (sub_64F60, n <= 0x417)
                if let Some((n, _)) = game.sound.bgm_state()
                    && (0..=0x417).contains(&n)
                {
                    crate::game::set_bit(&mut game.heard, n as usize);
                }
                self.music_ok = crate::gallery::music_unlocked(
                    |n| n > 0 && get_bit(&game.heard, n as usize),
                    |n| get_bit(&game.nodes, n),
                    game.movie_flags,
                );
                // sub_19EC4 marks a row's own BGM heard when an alternate or a flag unlocked it (sub_64F60)
                for (i, _) in self.music_ok.iter().enumerate().filter(|(_, ok)| **ok) {
                    crate::game::set_bit(&mut game.heard, crate::gallery_table::MUSIC[i].0 as usize);
                }
                self.enter_mode(game, Screen::Music);
            }
            Action::Track(i) => {
                // sub_1DAB8: A on a row plays it from the start; on the track already playing (or paused), or on
                // a locked row, the player stops
                if self.playing_track != Some(i) && self.music_ok.get(i).copied().unwrap_or(false) {
                    self.play_track(game, i);
                } else {
                    self.stop_track(game);
                }
            }
            Action::MusicCtl(k) => {
                let n = crate::gallery_table::MUSIC.len();
                // the search stops short of the track it started from (no replay of a lone unlocked track)
                let step = |from: usize, d: i32| {
                    (1..n as i32)
                        .map(|j| (from as i32 + d * j).rem_euclid(n as i32) as usize)
                        .find(|&t| self.music_ok.get(t).copied().unwrap_or(false))
                };
                // sub_1E98C: a1 + 68 is the last track played (0 at first)
                let cur = self.playing_track.or(self.last_track).unwrap_or(0);
                let target = match k {
                    // play (loc_1DEC4): the list cursor's track from the start, whatever plays
                    0 => Some(self.music_row),
                    // pause (sub_1F380) toggles while a track plays
                    1 if self.playing_track.is_some() => {
                        self.music_paused = !self.music_paused;
                        game.sound.set_bgm_paused(self.music_paused);
                        None
                    }
                    2 => {
                        self.stop_track(game);
                        None
                    }
                    // repeat (sub_1C400) toggles the global flag
                    3 => {
                        self.music_repeat = !self.music_repeat;
                        None
                    }
                    4 => step(cur, -1),
                    5 => step(cur, 1),
                    _ => None,
                };
                if let Some(t) = target {
                    if self.music_ok.get(t).copied().unwrap_or(false) {
                        self.play_track(game, t);
                    } else {
                        self.stop_track(game);
                    }
                }
            }
            Action::MusicVol => {
                let v = ((self.mouse.0 - 1412.0) / 364.0).clamp(0.0, 1.0);
                self.music_vol = (v * 20.0).round() / 20.0;
                game.sound.set_bgm_volume(Self::music_gain(self.music_vol), 0.1);
            }
            Action::OpenMovies => self.enter_mode(game, Screen::Movies),
            Action::Movie(i) => {
                let flag = crate::gallery::MOVIES[i].1;
                if flag.is_none_or(|f| game.movie_flags & (1 << f) != 0) {
                    // sub_183EC: A plays the movie at once (sub_4F28) with sound 1; its dialog 25 (state 3,
                    // sub_1873C) is never entered, as A sets state 4 directly
                    self.play_gallery_movie(game, i);
                    self.snd(game, 1);
                }
            }
            Action::OpenEndings => self.enter_mode(game, Screen::Endings),
            Action::Ending(i) => {
                let ok = Self::ending_ok(game, i);
                if ok {
                    // sub_E7A4: sound 1, then the dialog; cGalleryEnding keeps its cursor (+56 / +60) under it
                    self.gal_mem = Some(Action::Ending(i));
                    self.snd(game, 1);
                    self.confirm(game, Confirm::Ending(i));
                }
            }
            Action::OpenGallery => {
                if !self.tex.contains_key("cg_thumb_all") {
                    match game.assets.ui_textures(r, &game.assets.ui_atlas("CG_THUMB_PARTS")) {
                        Ok(list) => self.tex.extend(list),
                        Err(e) => tracing::warn!("gallery thumbnails: {e:#}"),
                    }
                }
                self.refresh_gallery(game);
                self.enter_mode(game, Screen::Gallery);
            }
            Action::Cg(id) => {
                // sub_9CBC: A plays sound 1; sub_8D08 (sub_AEC8 keeps the pick) opens a group with one unlocked
                // picture, or a variation, in the viewer, and a group with several in its variations grid
                let scroll = self.gal_scroll.pos;
                let (g, pos) = match self.cg_sub {
                    Some(g) => {
                        self.cg_pick.2 = scroll;
                        (g, id)
                    }
                    None => {
                        self.cg_pick.0 = Some(id);
                        self.cg_pick.1 = scroll;
                        (id, 0)
                    }
                };
                let n = self.cg_unlocked.get(g).map_or(0, Vec::len);
                if self.cg_sub.is_none() && n > 1 {
                    self.snd(game, 1);
                    self.cg_show(game, Some(g), 0, 0.0, true);
                } else if pos < n {
                    // it fades in over the gallery (sub_C498)
                    self.snd(game, 1);
                    self.cg_zoom = 0.0;
                    self.cg_pan = [0.0; 2];
                    self.push(Screen::CgView { group: g, pos });
                    self.cg_fx = CgFx::Open;
                    self.cg_t = 0.0;
                    self.cg_guide = (0, 0.0);
                    self.cg_guide_show();
                }
            }
            // sub_4CEA0 case 4: the confirm (sub_49710(8), state 19) only while F024 is set (the archive
            // scene); otherwise sound 1 and straight to the auto save and the fade out
            Action::ToTitle if game.var("F024") != 0 => self.confirm(game, Confirm::Title),
            Action::ToTitle => {
                self.snd(game, 1);
                self.leave_to_title(game, r);
            }
            Action::BacklogJump => {
                // sub_28A90: dialog 23 (sub_49710(23, 1), its sound 3), then sound 1, only for a page whose ring
                // entry is there and was not taken during a story movie or the staff roll (word +13760 == 0);
                // otherwise nothing happens
                let back = if let Screen::Backlog { scroll } = self.screen { scroll } else { 0 };
                if game.backlog_can_jump(back) {
                    self.confirm(game, Confirm::BacklogJump);
                    self.snd(game, 1);
                }
            }
            Action::Back if matches!(self.screen, Screen::CgView { .. }) => self.cg_close(game),
            Action::Back if matches!(self.screen, Screen::Confirm(_)) => {
                let Screen::Confirm(c) = self.screen else { return };
                // sub_49F00: B plays sound 2 and answers No a frame later
                self.snd(game, 2);
                self.dlg_out = Some(DlgOut { c, yes: false, hold: 1.0, t: 1.0 });
            }
            Action::Back => {
                // B / the back button: sound 2 (sub_4CEA0, sub_50FC0, sub_2B5C0, sub_49A40, sub_28A90, sub_6E28, ...)
                if self.screen != Screen::MoviePlay {
                    self.snd(game, 2);
                }
                if self.screen == Screen::MoviePlay {
                    self.gal_movie_skip(game);
                } else {
                    self.close();
                }
            }
            Action::Slot(i) => {
                let Screen::SaveLoad { save, .. } = self.screen else { return };
                // sub_50FC0 0x52230: with dword_AFFCC4 running the press is taken as navigation only
                if self.save_cool > 0 {
                    return;
                }
                if save {
                    if self.slots.get(i).is_some_and(|s| s.is_some()) {
                        self.confirm(game, Confirm::Overwrite(i));
                    } else {
                        self.save(game, r, i);
                    }
                } else if self.slots.get(i).is_some_and(|s| s.is_some()) {
                    if self.stack.contains(&Screen::Game) || self.stack.contains(&Screen::Title) {
                        // sub_50FC0 load mode: a used slot always goes to state 18 behind sub_49710(0, 0)
                        // (SYSMES 179, Yes first), from the title too
                        self.confirm(game, Confirm::Load(i));
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
                    // objects 78..81: byte_9D3A24 = 4 + language, config state 10 (sub_2D720) the next frame,
                    // input held for 5 frames past the change (dword_9D3A60)
                    Some(Ctl::Lang(k)) => {
                        self.snd(game, 1);
                        self.conf_hold = 20;
                        self.lang_pick = Some(k);
                        self.lang_cool = 5;
                        self.lang_seq = Some(LangSeq { state: 0, word: k as i16, fresh: true });
                        return;
                    }
                    Some(Ctl::Toggle(_) | Ctl::Radio(..) | Ctl::Mute(_)) => self.snd(game, 1),
                    _ => {}
                }
                // sub_2B930: every press restarts the hold delay
                self.conf_hold = 20;
                self.conf_change(game, i, d);
                self.store_global(game);
            }
            Action::Yes | Action::No => {
                let Screen::Confirm(c) = self.screen else { return };
                // sub_49F00: A on either button plays sound 1; the caller acts 15 frames later
                self.snd(game, 1);
                self.dlg_out = Some(DlgOut { c, yes: a == Action::Yes, hold: 15.0, t: 1.0 });
            }
        }
    }

    /// cGalleryMoviePlayer (sub_18B0C, update sub_18CD4) for movie `i` (0 prologue, 1 / 2 the openings): the BGM
    /// fades out at 3.33 / frame (sub_9C9B0: 0.5 s from full volume) while a full-screen curtain (white for the first opening, else black)
    /// fades in over the gallery in 0.5 s (quad out); then the movie plays over it (sub_17C0), B / Plus stopping
    /// it (sub_1A80, once); once the player has closed the curtain fades out in 0.5 s and the BGM that was
    /// playing comes back at once (sub_9C470). The prologue switches to BGM 2 at 360 frames, as in the story.
    fn play_gallery_movie(&mut self, game: &mut Game, i: usize) {
        let bgm = game.sound.bgm_id();
        game.sound.stop_bgm_rate(10.0 / 3.0);
        self.playing_track = None;
        self.title_movie = false;
        self.gal_movie = Some(GalMovie { n: i, phase: 0, t: 0.0, bgm, frames: 0.0, white: i == 1, skip: true, bgm2: false });
        game.movie_by_ui = true;
        self.push(Screen::MoviePlay);
    }

    /// One update of the gallery movie player.
    fn gal_movie_step(&mut self, game: &mut Game, r: &Renderer, secs: f32) {
        let Some(m) = &mut self.gal_movie else { return };
        match m.phase {
            0 => {
                m.t += secs;
                if m.t >= 0.5 {
                    game.gallery_movie = true;
                    {
                        // jp_18EA4: the language's copy (sub_41260)
                        let entry = crate::movie::localized(crate::gallery::MOVIES[m.n].0, game.assets.language());
                        game.play_movie(r, &crate::movie::movie_name(entry).unwrap_or_default());
                    }
                    m.phase = 1;
                }
            }
            1 => {
                // state 2: once the movie is fully shown the curtain turns black (sub_1C10)
                if game.movie_alpha() >= 1.0 {
                    m.white = false;
                }
                if game.movie_ready() {
                    m.frames += secs * 60.0;
                }
                if m.n == 0 && m.frames >= 360.0 && !m.bgm2 {
                    m.bgm2 = true;
                    game.play_bgm(2, 1.0, 0.0);
                }
                if !game.movie_active() {
                    game.gallery_movie = false;
                    m.phase = 2;
                    m.t = 0.0;
                }
            }
            _ => {
                m.t += secs;
                if m.t >= 0.5 {
                    let bgm = m.bgm;
                    self.gal_movie = None;
                    game.movie_by_ui = false;
                    if bgm > 0 {
                        game.play_bgm(bgm, 1.0, 0.0);
                    }
                    self.pop();
                }
            }
        }
    }

    /// B / Plus while the gallery movie plays (state 2, mask 0x4008): the movie stops, once.
    fn gal_movie_skip(&mut self, game: &mut Game) {
        if let Some(m) = &mut self.gal_movie
            && m.phase == 1
            && std::mem::take(&mut m.skip)
        {
            game.fade_movie_out();
        }
    }

    /// A on a flowchart cell (sub_11418, sound 1): in game the rewind dialog for its record; in the gallery
    /// the record replays at once (sub_5040), a filler (scene -1) closing the gallery to the title.
    fn flow_press(&mut self, game: &mut Game) {
        let Some(Screen::Flowchart { gallery }) = Some(self.screen) else { return };
        let Some((row, id, scene)) = self.flow.as_ref().and_then(|c| c.record_at(self.flow_cell)) else { return };
        self.snd(game, 1);
        if gallery {
            self.gal_replay(scene, id, self.gal_prev);
        } else {
            self.confirm(game, Confirm::Jump(row, id, scene));
        }
    }

    /// cGallery replay (sub_5040): the backdrop fades out (-0.04 / frame) while the gallery closes (sub_826C),
    /// then cGallery's result scene + 2 starts the archive replay (sub_48AC0 case 4); scene -1 gives result 1,
    /// the title. off_AE0288 keeps the mode, the node (else the scene) and `prev` for the way back.
    fn gal_replay(&mut self, scene: i32, node: i32, prev: i32) {
        self.menu_bg.fade(-0.04);
        self.gal_ret[1] = if node >= 0 { node } else { scene };
        self.gal_ret[2] = prev;
        // cGallery state 1: [0] = the mode it closes with (sub_82E4)
        self.gal_ret[0] = match self.screen {
            Screen::Flowchart { .. } => 2,
            s => Self::gal_mode(s).map_or(-1, |m| m as i32),
        };
        if let Screen::Flowchart { .. } = self.screen {
            // the chart has no fade-out of its own: back on the hub, which fades
            self.pop();
        }
        if scene >= 0 {
            self.replay_pending = Some((scene, node, prev));
        }
        self.closing = true;
    }

    /// The music player's BGM volume: sub_9C8F0 takes the player's volume as whole percent.
    fn music_gain(vol: f32) -> f32 {
        (vol * 100.0) as i32 as f32 / 100.0
    }

    /// sub_1DAB8: track `i` plays from the start (sub_19E98 -> sub_9C470(track, 100, 0), at once) at the
    /// player's volume.
    fn play_track(&mut self, game: &mut Game, i: usize) {
        self.music_paused = false;
        self.music_fade = None;
        game.play_bgm(crate::gallery_table::MUSIC[i].0, Self::music_gain(self.music_vol), 0.0);
        self.playing_track = Some(i);
        self.last_track = Some(i);
        self.music_clock = 0.0;
    }

    /// loc_1DC40: the player stops (sub_1A028 -> sub_9C610, at once) and its clock goes back to 0.
    fn stop_track(&mut self, game: &mut Game) {
        self.music_paused = false;
        self.music_fade = None;
        self.music_clock = 0.0;
        if self.playing_track.take().is_some() {
            game.play_bgm(0, 1.0, 0.0);
        }
    }

    /// The gallery has faded out over a replay pick: 528_ARCHIVE starts (sub_48AC0 case 4).
    fn start_replay(&mut self, game: &mut Game, scene: i32) {
        self.store_global(game);
        if self.playing_track.take().is_some() {
            self.music_paused = false;
            self.music_fade = None;
        }
        if game.start_archive(scene) {
            self.screen = Screen::Game;
            self.stack.clear();
            self.title_movie = false;
            self.closing = false;
        } else {
            tracing::warn!("528_ARCHIVE missing");
            self.go_to_title(game, false);
        }
    }

    /// The story ended (sub_40ED0 returned 1 / 2): BGM out, the screen fades to black at 0.02 / frame
    /// (sub_48AC0 case 2 states 2 / 3 / 7), then the title, or the gallery while F024 is set.
    fn end_story(&mut self, game: &mut Game) {
        // the story task is over (sub_40ED0 returned 1): the script does not step again during the fade
        game.ended = true;
        game.end_story_window();
        game.sound.stop_se(-1);
        game.sound.stop_voice();
        self.black = self.black.max(0.0);
        self.black_col = [0.0; 3];
        self.black_under = false;
        self.black_speed = 0.02;
        self.black_top = 1.0;
        self.black_then = Some(if game.var("F024") != 0 { Action::GalleryBack } else { Action::TitleNow });
        // Witch on the Holy Night: sub_7320 takes the Archive's cursors for the replayed scene (F026)
        if crate::assets::witch() && game.var("F024") != 0 {
            self.warc_restore(game.var("F026"));
        }
    }

    /// sub_47A90 after a replay: the stage goes (the system data is written first after a result 2 end, RDED or
    /// RTM_, states 7 / 5), BGM 2 keeps playing or the title BGM starts, the backdrop fades in at 0.04 and cGallery reopens
    /// on the mode it closed with, its content focused on the replayed entry (sub_6124 / sub_7F38 with
    /// off_AE0288[1]).
    fn gallery_back(&mut self, game: &mut Game, r: &Renderer) {
        if crate::assets::witch() {
            // jp_48AC0 mode 2 state 3, jp_47A90: the title's BGM at once, the Archive with its panel up
            self.store_global(game);
            game.reset();
            game.play_bgm(if crate::ui::witch::title_complete(game) { 42 } else { 65 }, 1.0, 0.0);
            self.title_movie = false;
            self.black = 0.0;
            self.black_speed = 0.0;
            self.warc_init(game, true);
            return;
        }
        let done = game.replay_done;
        if done {
            self.store_global(game);
        }
        let keep = game.sound.bgm_state().is_some_and(|(id, _)| id == 2);
        if keep {
            game.reset_keep_bgm();
        } else {
            game.reset();
            game.play_title_bgm();
        }
        let [mode, entry, prev] = self.gal_ret;
        self.screen = Screen::Title;
        self.stack.clear();
        self.title_movie = false;
        // the first mode is [0] itself here (the chart or the ending list, below); the hub's own mode under
        // the chart is not created with the focus
        self.gal_ret[1] = -1;
        self.act(game, r, Action::EnterGallery);
        self.gal_ret[1] = entry;
        // sub_6124 takes [2] as the mode before, then cGallery state 0 resets [0] and [2]
        self.gal_prev = prev;
        self.gal_ret[0] = -1;
        self.gal_ret[2] = -1;
        match mode {
            2 => {
                self.gal_tab = 2;
                self.open_flow_at(game, true, entry);
            }
            4 => {
                self.screen = Screen::Endings;
                self.gal_mode_shown(game);
                if let Some(i) = crate::gallery::ENDINGS.iter().position(|e| e.1 == entry) {
                    self.gal_mem = Some(Action::Ending(i));
                }
                self.gal_to_content(game);
            }
            _ => {}
        }
    }

    /// A confirm dialog's Yes, once its hold is over (the callers wait for sub_496F0() == 0).
    fn confirm_yes(&mut self, game: &mut Game, r: &Renderer, c: Confirm) {
        match c {
            // cGalleryEnding: the "Teach me, Ciel-sensei" scene replays (sub_5040(scene, -1, -1))
            Confirm::Ending(i) => self.gal_replay(crate::gallery::ENDINGS[i].1, -1, -1),
            Confirm::Title => {
                // sub_4CCA0 with byte_B359DC = 0: F024 off, so the story ends to the title; a flowchart
                // replay's mode goes back to the one before it (sub_50C8)
                game.set_var_pub("F024", 0);
                if self.gal_ret[0] == 2 {
                    self.gal_ret[0] = self.gal_ret[2];
                    self.gal_ret[2] = -1;
                }
                self.leave_to_title(game, r);
            }
            // byte_B359DC = 1: F024 stays, so the story ends back in the gallery
            Confirm::EndReplay => self.leave_to_title(game, r),
            Confirm::Overwrite(i) => self.save(game, r, i),
            Confirm::Delete(i) => self.delete(game, i),
            // state 18, Yes: popup 184 held open (sub_4ABE0(0xB8) + sub_4ABD0(1)), state 19 loads once it shows
            Confirm::Load(i) => {
                self.popup(game, 184, f32::INFINITY);
                self.load_popup = Some(i);
            }
            Confirm::BacklogJump => {
                // sub_28A90 state 6 -> 7: back in the backlog, which fades out and jumps
                self.backlog_jump = true;
                self.closing = true;
            }
            // sub_40ED0 off_AE0680 5 -> 6 / 2 -> 3 (and off_AE06B0 2 -> 3 after an RDAS hit): 20 frames first
            // (only when sub_53980(-1) / sub_53980(1) allow it: 0x41308 / 0x41428)
            Confirm::PrevScene if game.prev_scene_allowed() => self.start_scene_countdown(false, 20),
            Confirm::NextScene if game.next_scene_allowed() => self.start_scene_countdown(true, 20),
            Confirm::PrevScene | Confirm::NextScene => {}
            Confirm::QuickLoad => {
                self.quick_load(game, r);
                self.close_panel_after_quick_load();
                // Witch on the Holy Night's Minus (byte_649884): sub_2F280(1, 1), jp_527A0(120), sound 13
                if crate::assets::witch() {
                    game.play_sys_sound(13);
                }
            }
            Confirm::Defaults => {
                self.conf_change(game, Ctl::Default.code(), 0);
                self.store_global(game);
            }
            Confirm::Jump(row, n, scene) => {
                self.store_global(game);
                if !game.jump_record(row, n, scene) {
                    // a filler record (scene -1): no scene to start, the chart closes back into the
                    // system menu (sub_4D8C0 state 16)
                    if matches!(self.screen, Screen::Flowchart { .. }) {
                        self.pop();
                    }
                } else {
                    self.screen = Screen::Game;
                    self.stack.clear();
                    self.title_movie = false;
                    // as after a load: the system menu backdrop does not stay over the new scene
                    self.menu_bg = MenuBg::new();
                }
            }
        }
    }

    fn save(&mut self, game: &mut Game, _r: &Renderer, slot: usize) {
        let Some(data) = game.save_data() else { return };
        // sub_50FC0: "セーブ中です" (183) is shown with a 1200-frame limit but held (sub_4ABD0(1)) and closed
        // (sub_4ACD0) once the write is done and 30 frames have passed since it opened; "セーブに失敗しました"
        // (189) on failure
        let order = self.save_list(true);
        match self.write_record(game, slot, &data) {
            Ok(()) => {
                // the write is done at once here, so state 8 closes the popup (with sound 1) at its 30 frames
                self.popup_held(game, 183, 30.0);
                self.save_wait = Some(0.0);
                self.save_pending = Some(slot);
                // state 7 (LABEL_72) copies the new record into the list records at once: the entry shows its
                // date and titles in place while the list keeps its order and the old thumbnail
                self.save_order = Some(order);
                self.reload_slots(game);
            }
            Err(e) => {
                tracing::warn!("save to slot {slot}: {e:#}");
                self.popup(game, 189, 480.0);
                self.refresh_slots(game);
            }
        }
    }

    /// sub_50FC0 (overseas) state 23, once the dialog has closed: "Deleting..." (188) held, the record cleared
    /// and the file written (sub_4EC60(2)); state 24 plays sound 1 when the write is done ("Failed" 189
    /// otherwise), and state 25 15 frames on closes the popup, rebuilds the list (sub_51CF0) and shows
    /// "Deleted" (217 of the overseas file) for 300 frames; state 26 waits for it to close.
    fn delete(&mut self, game: &mut Game, slot: usize) {
        self.popup_held(game, 188, f32::INFINITY);
        // state 23 sets dword_AFFCC4 to 180 (sub_40E70)
        self.save_cool = 180;
        self.file.delete_record(slot);
        self.file.update_continue();
        match self.file.store() {
            Ok(()) => self.delete_wait = Some(0.0),
            Err(e) => {
                tracing::warn!("deleting save {slot}: {e:#}");
                self.popup(game, 189, 480.0);
                self.refresh_slots(game);
            }
        }
    }

    /// sub_51CF0: the list again (sub_50790), its scroll kept in range and the cursor on the last entry when
    /// the list got shorter than it.
    fn delete_refresh(&mut self, game: &mut Game) {
        self.save_order = None;
        self.refresh_slots(game);
        let Screen::SaveLoad { save, scroll } = self.screen else { return };
        let count = self.save_list(save).len();
        let mut cur = self.hover.filter(|&h| h < SLOTS_PER_PAGE).unwrap_or(0);
        let scroll = if count == 0 {
            cur = 0;
            0
        } else if count > SLOTS_PER_PAGE {
            scroll.min(count - SLOTS_PER_PAGE)
        } else {
            cur = cur.min(count - 1);
            0
        };
        self.screen = Screen::SaveLoad { save, scroll };
        self.hover = Some(cur);
        self.save_seen = Some((save, self.hover, scroll));
        self.save_notes = self.cursor_notes();
        self.save_pointer_row(game);
    }

    /// The save list's X (overseas sub_50FC0 state 2, 0x1000): a used entry under the cursor asks to delete it.
    pub(super) fn save_delete_key(&mut self, game: &mut Game) {
        let Screen::SaveLoad { save, scroll } = self.screen else { return };
        // (0x1000 && sub_40E90: not while dword_AFFCC4 runs)
        if game.assets.build != knife_core::exe::Build::Overseas || self.side || self.busy() || self.save_cool > 0 {
            return;
        }
        let Some(slot) = self.hover.filter(|&h| h < SLOTS_PER_PAGE).and_then(|h| self.save_list(save).get(scroll + h).copied()) else {
            return;
        };
        if self.slots.get(slot).is_some_and(|s| s.is_some()) {
            self.confirm(game, Confirm::Delete(slot));
        } else {
            // state 21 (overseas 0x51FC0, LABEL_293): a blank slot gets popup 193 "No save data found."
            self.popup(game, 193, 480.0);
        }
    }

    /// sub_50FC0 state 9, 41 frames after the popup closed: the list is rebuilt (sub_50790) with the cursor on
    /// the new save: entry 1 when the list (count, the blank slot included) is 2..4 long, else scroll 1 and
    /// row 0 while a blank slot leads, else entry 0.
    fn save_refresh(&mut self, game: &mut Game) {
        // sub_8C2E0 frees the saved slot's thumbnail (word_F1F438[40] flag cleared): sub_4F240 draws none
        // there until sub_89AD0 has made it again; the others are kept
        if let Some(slot) = self.save_pending.take() {
            game.clear_previews(Some(slot));
        }
        self.save_order = None;
        self.reload_slots(game);
        let Screen::SaveLoad { save, .. } = self.screen else { return };
        let list = self.save_list(save);
        let count = list.len();
        let used = list.iter().filter(|&&i| self.slots.get(i).is_some_and(|s| s.is_some())).count();
        let (scroll, cur) = if count < 5 || used >= save::list_slots() { (0, usize::from(count > 1 && count <= 4)) } else { (1, 0) };
        self.screen = Screen::SaveLoad { save, scroll };
        self.hover = Some(cur);
        self.save_seen = Some((save, self.hover, scroll));
        self.save_notes = self.cursor_notes();
        self.save_pointer_row(game);
    }

    /// sub_50FC0 state 2 after a list rebuild, which sets the cursor without sub_AE0E0: the hovered object
    /// (sub_AE0B0) is still the entry last touched or pointed at (sub_AE840 keeps it while no touch or mouse
    /// move is active, until a pad move refocuses another object), so the next frame the cursor goes back to
    /// it with sound 0 and the flash (Switch: a touch save on the new-save entry leaves the cursor there)
    fn save_pointer_row(&mut self, game: &mut Game) {
        if self.key_focus || self.side {
            return;
        }
        let items = self.items(game);
        let Some(h) = items.iter().position(|(r, a)| matches!(a, Action::Slot(_)) && rect_hit(*r, self.mouse)) else { return };
        if self.hover != Some(h) {
            self.hover = Some(h);
            self.snd(game, 0);
            self.save_flash = 1.0;
            if let Screen::SaveLoad { save, scroll } = self.screen {
                self.save_seen = Some((save, self.hover, scroll));
            }
            self.save_notes = self.cursor_notes();
        }
    }

    /// sub_50B10: the notes text of the save list's cursor entry (slot 38), none for a free entry
    fn cursor_notes(&self) -> Option<String> {
        let Screen::SaveLoad { save, scroll } = self.screen else { return None };
        let h = self.hover.filter(|&h| h < SLOTS_PER_PAGE)?;
        let slot = *self.save_list(save).get(scroll + h)?;
        self.slots.get(slot).cloned().flatten().map(|d| d.preview)
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
        // sub_52860 (mode panel): written silently, the Quick Save banner shows; sub_4E170 then sets
        // dword_AFFCC4 to 180 (sub_40E70)
        if game.can_save() && self.write_quiet(game, r, save::quick_slot()) {
            self.show_banner(1);
        }
        self.save_cool = 180;
    }

    /// Witch on the Holy Night's mask [14] (Plus | L3; sub_40ED0 at a click wait, jp_83E50 / sub_65380 in a
    /// choice): sound 1 while saving is off (F025), the cooldown runs (jp_40E90), in a TeaTime scene
    /// (sub_3A800) or with the quick record already at this point (sub_359F0); else sound 0, the Saved banner
    /// (sub_1D4A0(1)), the quick record and the file written (sub_358C0), cooldown 180, sound 13.
    pub(super) fn witch_quick_save(&mut self, game: &mut Game, r: &Renderer) {
        let same = game
            .save_data()
            .zip(self.slots.get(save::quick_slot()).and_then(Option::as_ref))
            .is_some_and(|(a, b)| a.scene == b.scene && a.pc == b.pc);
        if game.var("F025") != 0 || self.save_cool > 0 || game.teatime_scene() || same {
            self.snd(game, 1);
            return;
        }
        self.snd(game, 0);
        self.show_banner(1);
        if game.can_save() {
            self.write_quiet(game, r, save::quick_slot());
        }
        self.save_cool = 180;
        game.play_sys_sound(13);
    }

    /// Witch on the Holy Night's mask [15] (Minus, sub_40ED0): with a quick record (sub_35B30(120)), sound 1
    /// while saving is off or in a TeaTime scene, else dialog 9 with the record's preview (jp_884F0).
    pub(super) fn witch_quick_load(&mut self, game: &mut Game, r: &Renderer) {
        if !self.slots.get(save::quick_slot()).is_some_and(Option::is_some) {
            return;
        }
        if game.var("F025") != 0 || game.teatime_scene() {
            self.snd(game, 1);
            return;
        }
        self.wd_ask(game, r, Confirm::QuickLoad);
    }

    pub fn quick_load(&mut self, game: &mut Game, r: &Renderer) {
        if matches!(self.screen, Screen::Game | Screen::Title) && self.slots.get(save::quick_slot()).is_some_and(|s| s.is_some()) {
            self.load(game, r, save::quick_slot());
        }
    }

    fn load(&mut self, game: &mut Game, r: &Renderer, slot: usize) {
        if self.restore_slot(game, r, slot) {
            self.enter_loaded();
        }
    }

    /// Read record `slot` into the game (sub_82330); false when it is missing or unreadable.
    fn restore_slot(&mut self, game: &mut Game, r: &Renderer, slot: usize) -> bool {
        let Some(data) = self.file.record(slot).and_then(|rec| game.decode_record(rec)) else { return false };
        game.reset();
        game.restore(r, &data);
        true
    }

    /// Show the loaded game: every menu and the backdrop gone at once.
    fn enter_loaded(&mut self) {
        self.screen = Screen::Game;
        self.stack.clear();
        self.title_movie = false;
        self.menu_bg = MenuBg::new();
    }

    /// System menu -> title (sub_4CEA0 case 4 / sub_4CCA0): unless the game cannot be saved here, write the
    /// auto save (sub_52AA0) and show its banner; the menu fades out (1/30 per frame) and the title follows
    /// once the banner is gone (sub_4D710).
    fn leave_to_title(&mut self, game: &mut Game, r: &Renderer) {
        // sub_52A20: no new auto save (and no banner) when the record matches the newest auto save
        let same = game
            .save_data()
            .zip(self.slots.get(save::auto_first()).and_then(Option::as_ref))
            .is_some_and(|(a, b)| a.scene == b.scene && a.pc == b.pc);
        // without a new auto save straight to state 21, else the banner first (state 18); either way the menu
        // and its backdrop fade out at 1/30 per frame and the title follows once both are done (sub_4D7E0)
        // and none at all while saving is off (F025, sub_47B10)
        let off = game.var("F025") != 0;
        if !(same || off || !game.can_save()) {
            // sub_52AA0 puts the record in the list either way; with dword_AFFCC4 running it returns before
            // the file write (sub_4EC60(2)) and so without the banner. The caller then sets 180.
            if self.save_cool > 0 {
                self.write_memory(game, save::auto_first());
            } else if self.write_quiet(game, r, save::auto_first()) {
                self.show_banner(2);
            }
            self.save_cool = 180;
        }
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
    /// x 385..940 (English 355..940), y 480..517.5 (+ the rise offset), and SYSMES 239 (text slot 65, 0.42 of
    /// the 60 px glyph) at x 401 (English 371), y 490.
    fn draw_complete(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some(c) = self.complete else { return };
        let y = (480.0 + c.off) * 2.0;
        let (x0, tx) = if crate::text::english() { (355.0, 371.0) } else { (385.0, 401.0) };
        out.push(Quad::rect(r.white.clone(), [x0 * 2.0, y, (940.0 - x0) * 2.0, 75.0], [0.19216, 0.24706, 0.58039, 0.50196 * c.a]));
        out.push(Quad::rect(r.white.clone(), [x0 * 2.0 + 1.0, y + 1.0, (940.0 - x0) * 2.0 - 2.0, 73.0], [0.0, 0.0, 0.0, 0.75294 * c.a]));
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        let Some(text) = self.sysmes.get(239).cloned() else { return };
        // slot 65 (flags 0x301000: the shared system font) at 0.42 x 0.42 of the 60 px glyph, 4 px spacing
        const SIZE: [f32; 2] = [60.0 * 0.42, 60.0 * 0.42];
        if self.font_sys.is_none() {
            self.font_sys = Self::sys_font(game, r);
        }
        let Some(font) = self.font_sys.as_mut() else { return };
        let base = (490.0 + c.off) * 2.0 + SYS_ASCENT * gs() * SIZE[1];
        font.draw_str_slot_rimmed(r, out, &text, SIZE, 4.0, [tx * 2.0, base], [1.0, 1.0, 1.0, c.a], 1);
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
        // (Witch on the Holy Night's banner only says Saved; its mode display is sub_1D670)
        if game.auto && !crate::assets::witch() {
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
        if crate::assets::witch() {
            // Witch on the Holy Night's sub_3B4B0: popup_ja (DISP_PARTS 6) row "Saved" (0, 4)-(450, 80) whatever
            // the banner, anchor 3 (left, middle) at (0, 424), x 0.85 of the 1/3 x 0.41481 scale (native pixels)
            let (w, h) = (450.0 * 0.85 * sx, 76.0 * 0.85 * sy);
            self.sprite(out, "popup_ja", [0.0, 424.0 * H / 448.0 - h / 2.0, w, h], Some([0.0, 4.0, 450.0, 76.0]));
            return;
        }
        let [x0, y0, x1, y1] = BANNER_SRC[b.idx];
        // anchor top-left at (0, 32) of 640x448
        self.sprite(out, "popup_ja", [0.0, 32.0 * H / 448.0, (x1 - x0) * sx, (y1 - y0) * sy], Some([x0, y0, x1 - x0, y1 - y0]));
    }

    fn go_to_title(&mut self, game: &mut Game, keep_bgm: bool) {
        self.store_global(game);
        if keep_bgm {
            game.reset_keep_bgm();
        } else {
            game.reset();
        }
        self.menu_bg = MenuBg::new();
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
            // the TeaTime menu: its save button (states 3 / 4), the title button's write
            match game.take_teatime_req() {
                // (a Witch on the Holy Night menu: its own save screen)
                Some(crate::game::TeaReq::Save) => {
                    self.ws_open(game, r, true);
                    self.push(Screen::Witch);
                    self.w.scr = witch::WScr::Save;
                }
                // sub_35790(1): the resume record and the system data are written whatever the cooldown,
                // which is then 180 (sub_40E70); sub_1D4A0(1) the Saved banner
                Some(crate::game::TeaReq::TitleSave) => {
                    if game.can_save() {
                        self.write_quiet(game, r, save::auto_first());
                    } else {
                        self.store_global(game);
                    }
                    self.save_cool = 180;
                    self.show_banner(1);
                }
                Some(crate::game::TeaReq::Store) => self.store_global(game),
                None => game.teatime_save_closed(),
            }
        }
        // sub_2B5C0: leaving the config stops its test SE and test voice
        if self.screen != Screen::Config && !self.stack.contains(&Screen::Config) {
            if let Some(ch) = self.conf_test_se.take() {
                game.sound.stop_se(ch);
            }
            if self.conf_test_row.take().is_some() {
                game.sound.stop_test_voice();
            }
        }
        self.lang_ms += dt;
        while self.lang_ms >= 1000.0 / 60.0 {
            self.lang_ms -= 1000.0 / 60.0;
            self.lang_frame(game, r);
        }
        if crate::assets::witch() && self.screen == Screen::Title {
            if std::mem::take(&mut self.wt_reinit) {
                self.wt_reenter(game, r);
            }
            self.wt_update(game, r, dt);
        }
        if crate::assets::witch() && self.screen != Screen::Title && (self.screen == Screen::Witch || self.w.dlg.shown()) {
            self.witch_update(game, r, dt);
        }
        // dt is in milliseconds; the UI clocks run in seconds
        let secs = dt / 1000.0;
        self.stick_tick = (self.stick_tick + dt * 0.06).min(4.0);
        while self.stick_tick >= 1.0 {
            self.stick_tick -= 1.0;
            self.stick_frame(game, r);
            self.drag_frame(game);
            self.conf_hold_frame(game);
            // sub_40EB0 in sub_48AC0 (off_AE04E8 2 / 3; the title's LOAD runs in mode 2)
            let story = self.screen == Screen::Game || self.stack.contains(&Screen::Game);
            let load = matches!(self.screen, Screen::SaveLoad { .. })
                || self.stack.iter().any(|s| matches!(s, Screen::SaveLoad { .. }));
            if story || load {
                self.save_cool = self.save_cool.saturating_sub(1);
            }
            self.save_flash = (self.save_flash - 0.2).max(0.0);
            if let Screen::SaveLoad { save, scroll } = self.screen {
                let cur = (save, self.hover, scroll);
                if !self.side && self.save_seen.is_some_and(|(s, h, sc)| s == save && (h, sc) != (cur.1, cur.2)) {
                    self.save_flash = 1.0;
                }
                if self.save_seen != Some(cur) {
                    self.save_notes = self.cursor_notes();
                }
                self.save_seen = Some(cur);
            } else {
                self.save_seen = None;
            }
        }
        self.gal_scroll.step(dt);
        // sub_9CBC / sub_1A7FC: while a stick, drag or slider scroll runs (flag bit 5), each frame re-picks the
        // cursor row from the shown scroll, lighting it at once (sub_1B2F0 with 0 s) with sound 0 on a change;
        // the flag clears once the scroller's tween has ended
        if self.gal_track && Self::is_gallery_mode(self.screen) {
            let before = self.hover;
            self.gal_keep_cursor(game);
            if self.hover != before {
                self.snd(game, 0);
            }
            if self.gal_scroll.t >= self.gal_scroll.len && self.bar.is_none() && self.drag.vel.abs() < 3.0 {
                self.gal_track = false;
            }
        } else {
            self.gal_track = false;
        }
        self.gal_guide_tick(game, secs);
        self.cg_update(game, secs);
        if self.cg_in < CG_FX_SECS {
            self.cg_in = (self.cg_in + secs).min(CG_FX_SECS);
            if self.cg_in >= CG_FX_SECS {
                self.cg_old = None;
            }
        }
        if std::mem::take(&mut self.gal_restore) && self.key_focus && Self::gal_mode(self.screen).is_some() {
            let a = if self.gal_side { Some(Self::hub_items()[self.gal_tab].1) } else { self.gal_mem };
            if let Some(a) = a {
                self.hover_action(game, a);
            }
        }
        if self.flow_t < self.flow_len {
            self.flow_t = (self.flow_t + dt).min(self.flow_len);
            let t = self.flow_t / self.flow_len;
            self.flow_scroll = self.flow_from + (self.flow_target - self.flow_from) * -(t * (t - 2.0));
        }
        // sub_11EB4: cFlowchart + 52 = the shown scroll, each frame
        self.flow_ref = self.flow_scroll.trunc();
        if self.conf_t < CONF_SCROLL_MS {
            self.conf_t = (self.conf_t + dt).min(CONF_SCROLL_MS);
            let t = self.conf_t / CONF_SCROLL_MS;
            let e = -(t * (t - 2.0));
            self.conf_scroll = self.conf_from + (self.conf_target[self.conf_tab] - self.conf_from) * e;
        }
        if let Some(w) = self.save_wait {
            let n = w + secs * 60.0;
            if w < 30.0 && n >= 30.0 {
                self.snd(game, 1);
            }
            if n >= 30.0 + 41.0 {
                self.save_wait = None;
                self.save_refresh(game);
            } else {
                self.save_wait = Some(n);
            }
        }
        if let Some(w) = self.delete_wait {
            let n = w + secs * 60.0;
            if w < 1.0 && n >= 1.0 {
                self.snd(game, 1);
            }
            // state 24 the frame after (frame 1), then state 25 counting dword_F1F470 down from 15 (frame 17)
            if n >= 17.0 {
                self.delete_wait = None;
                self.delete_refresh(game);
                self.popup(game, crate::assets::SYSMES_DELETED, 300.0);
            } else {
                self.delete_wait = Some(n);
            }
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
        // sub_49F00 state 1: +0.07 per frame
        self.dlg_t = (self.dlg_t + secs * 60.0 * 0.07).min(1.0);
        if let Some(o) = self.dlg_out {
            let f = secs * 60.0;
            if o.hold > 0.0 {
                let hold = o.hold - f;
                self.dlg_out = Some(DlgOut { hold: hold.max(0.0), ..o });
                if hold <= 0.0 {
                    // dword_B34B54 clears (state 3 -> 4): the screen below takes over and the caller acts
                    self.pop();
                    if o.yes {
                        self.confirm_yes(game, r, o.c);
                    }
                }
            } else {
                let t = o.t - 0.07 * f;
                self.dlg_out = (t > 0.0).then_some(DlgOut { t, ..o });
            }
        }
        // sub_4CBA0 -> sub_49630(1, ..) -> sub_2E000: the system menu blurs the scene (+1/12 per frame from
        // 0.001) and the message text is hidden; it ends once the backdrop has faded out (sub_2E030(0, 1)).
        // The backlog blurs it the same way (sub_28F80 state 1) until its own fade-out ends (state 8 /
        // sub_28D50: sub_2E030(0, 0) at alpha 0).
        let f60 = secs * 60.0;
        let backlog = matches!(self.screen, Screen::Backlog { .. })
            || self.stack.iter().any(|s| matches!(s, Screen::Backlog { .. }));
        if self.screen == Screen::Witch {
            // the WoH screens set the blur themselves (`witch_update`)
        } else if (self.screen == Screen::Menu || backlog) && self.stack.contains(&Screen::Game) && game.menu_blur.is_none() {
            game.menu_blur = Some(0.001);
        } else if let Some(a) = &mut game.menu_blur {
            let over_game = self.screen == Screen::Game || self.stack.contains(&Screen::Game);
            if !over_game || (self.screen == Screen::Game && !self.menu_bg.visible()) {
                game.menu_blur = None;
            } else {
                *a = (*a + f60 / 12.0).min(1.0);
            }
        }
        // (WoH's capture takes the window and text along, sub_7FCA0)
        game.hide_text = backlog || (game.menu_blur.is_some() && !crate::assets::witch()) || self.touch.hidden;
        game.touch_hidden = self.touch.hidden;
        if let Some(t) = self.playing_track.filter(|_| !self.music_paused) {
            // sub_1D314: the BGM loops between the track's loop points; with repeat the clock wraps back by the
            // loop's length past the end, without it the clock holds at the end and the track fades out over
            // 1.5 s from the end (from 1.5 s before it when field 6 is 0), then the player stops
            self.music_clock += secs;
            let (start, end) = crate::gallery_table::MUSIC_LOOP[t];
            let (start, end) = ((start / 48) as f32 / 1000.0, (end / 48) as f32 / 1000.0);
            if self.music_repeat {
                if end > start {
                    while self.music_clock > end {
                        self.music_clock -= end - start;
                    }
                }
            } else {
                let fade_at = if crate::gallery_table::MUSIC[t].2 == 0 { end - 1.5 } else { end };
                if self.music_fade.is_none() && self.music_clock > fade_at {
                    // sub_9C9B0(vol * 100 * 1000 / 60 / 1500): 1.5 s at the player's volume
                    game.sound.stop_bgm(1.5);
                    self.music_fade = Some(1.5);
                }
            }
            if let Some(f) = self.music_fade.as_mut() {
                *f -= secs;
                if *f <= 0.0 {
                    self.stop_track(game);
                }
            }
            if self.playing_track.is_some() {
                self.music_clock = self.music_clock.min(end);
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
        // the backdrop keeps fading while the title waits for its movie (the exe's backdrop runs outside
        // sub_8E200)
        self.menu_bg.update(secs * 60.0);
        // sub_49630 -> sub_1B20: the title movie pauses when a submenu backdrop starts fading in and resumes as
        // soon as it starts fading out, so the title shows under the fading backdrop
        game.hold_movie(self.title_movie && self.menu_bg.movie_paused);
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
                self.black_under = false;
            } else if self.black >= self.black_top {
                self.black_speed = 0.0;
                self.black_top = 1.2;
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
                    self.finish_close(game, r);
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
                // the music player, closed for another mode (sub_8400 -> vt + 128 sets its flag), starts the
                // gallery BGM again from the start (sub_19838 state 4 -> sub_8E1C0)
                if self.screen == Screen::Music {
                    game.play_title_bgm();
                }
                self.screen = next;
                self.gal_mode_shown(game);
            }
        } else if matches!(self.screen, Screen::Flowchart { gallery: true }) && !self.closing {
            // the content fading out under the chart; it comes back as the chart fades out (sub_7624, sub_82FC)
            self.gal_a = (self.gal_a - secs * 2.0).max(0.0);
        } else {
            self.gal_a = (self.gal_a + secs * 2.0).min(1.0);
        }
        match self.flow_open {
            Some(gallery) => {
                if !gallery {
                    self.menu_a = (self.menu_a - f / 12.0).max(0.0);
                }
                if (gallery && self.gal_a <= 0.0) || (!gallery && self.menu_a <= 0.0) {
                    self.flow_open = None;
                    if gallery || self.flow_ok(game, false) {
                        self.open_flow(game, gallery);
                    } else {
                        // cFlowchartTask state 3 (sub_17530): popup 1496 for 480 ms, then back to the menu, which
                        // fades in again (sub_4D8C0 state 16)
                        self.popup(game, 1496, 480.0);
                    }
                }
            }
            // the menu stays out while the chart task's popup is up
            None if self.screen == Screen::Menu && self.menu_a < 1.0 && self.popup.is_some() => {}
            None if self.screen == Screen::Menu => self.menu_a = (self.menu_a + f / 12.0).min(1.0),
            None => {}
        }
        if self.screen == Screen::Config {
            self.conf_tick(f);
            // sub_29770: each move of the config cursor is kept as the tab's row / column (-1: the tab bar)
            if !self.side
                && self.conf_next.is_none()
                && let Some(h) = self.hover
            {
                let layout = self.conf_layout();
                let rows = self.conf_rows(&layout);
                self.conf_cur[self.conf_tab] =
                    rows.iter().enumerate().find_map(|(r, row)| row.iter().position(|&x| x == h).map(|c| (r, c)));
            }
        }
        // a choice takes the UI focus (sub_83B20)
        if self.screen == Screen::Game && game.in_choice() {
            self.sys_focus = false;
        }
        if self.screen == Screen::Config && self.conf_tab == 0 {
            self.conf_preview_update(game, f);
        }
        self.back_btn = (self.back_btn + if self.back_btn_on() { 0.04 } else { -0.04 } * f).clamp(0.0, 1.0);
        // WoH's title keeps its own BGM and has no stage or movie (`witch::title`)
        let on_title = self.screen == Screen::Title && !crate::assets::witch();
        if on_title != self.on_title {
            self.on_title = on_title;
            game.title_stage_step(self.boot_flag);
            // back from LOAD GAME with a record picked (title case 32, result 2) the BGM is already fading out for
            // the load; starting it here played the title BGM again from the start until the load cut it
            if on_title && !matches!(self.black_then, Some(Action::LoadNow(_))) {
                game.play_title_bgm();
            }
        }
        if on_title && !self.press && !self.title_movie {
            self.title_movie = true;
            game.play_title_movie(r);
        }
        if on_title {
            self.complete_update(game, f);
        } else {
            self.complete = None;
        }
        if std::mem::take(&mut game.banner_away) {
            self.skip_banner();
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
        game.banner_up = self.banner.is_some();
        if self.title_after_banner && self.banner.is_none() && self.scr_a <= 0.0 {
            // sub_4D7E0 done: sub_40ED0 returns 1 and the story ends (to the title, or the gallery with F024)
            self.title_after_banner = false;
            self.closing = false;
            self.screen = Screen::Game;
            self.stack.clear();
            game.drop_messages();
            self.end_story(game);
        }
        if let Some(i) = self.load_popup
            && self.popup.is_none_or(|p| p.a >= 1.0 || p.closing)
        {
            // sub_50FC0 state 19: the record is read and the popup closes (sub_4ACD0); in game it applies at
            // once, from the title the window closes over the title first and the title loads it after
            // state 30's fade
            self.load_popup = None;
            if let Some(p) = &mut self.popup {
                p.closing = true;
            }
            if self.stack.contains(&Screen::Title) {
                self.load_after_close = Some(i);
                self.close();
            } else if self.restore_slot(game, r, i) {
                self.load_cut = true;
                self.close();
            }
        }
        if let Some(i) = self.cont_slot
            && self.popup.is_none_or(|p| p.a >= 1.0 || p.closing)
        {
            self.cont_slot = None;
            if let Some(p) = &mut self.popup {
                p.closing = true;
            }
            game.sound.stop_bgm_rate(1.5);
            self.black_col = [0.0; 3];
            self.black_under = false;
            self.black_speed = 0.01;
            self.black_then = Some(Action::LoadNow(i));
        }
        self.gal_movie_step(game, r, secs);
        // sub_40ED0 top state 1 after an RDAS hit: the next scene, asked first (dialog 10) when the config's
        // confirmation is on
        if self.screen == Screen::Game && std::mem::take(&mut game.auto_skip_hit) && !game.ended {
            game.drop_capture();
            if game.settings.skip_confirm {
                self.confirm(game, Confirm::NextScene);
            } else {
                // off_AE06B8 = 4: a 4-frame countdown
                self.start_scene_countdown(true, 4);
            }
        }
        if self.screen == Screen::Game && game.ended && self.black_then.is_none() {
            self.end_story(game);
        }
        self.touch_update(game, r, secs, f);
    }

    /// The frame's held input for `Game::set_held`: any controller button, and the advance (A or a touch).
    pub fn held_input(&self) -> (bool, bool) {
        (self.pads_held != 0, self.enter_held || self.touch.down)
    }

    pub fn game_paused(&self) -> bool {
        !matches!(self.screen, Screen::Game)
    }

    /// A gallery flowchart replay runs (sub_50F0: off_AE0288 == 2).
    pub fn flow_replay(&self) -> bool {
        self.gal_ret[0] == 2
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
        [133.667 * 3.0, 36.919 * H / 448.0 + SAVE_PITCH * i as f32]
    }

    /// The system menu (sub_4C370) drawn under the save/load and config screens when they were opened
    /// from it. `open`: the button of the open submenu (selected column); `cursor`: the tab under the cursor
    /// (hover column, with the cursor light and line).
    fn draw_menu_base(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, open: Option<usize>, cursor: Option<usize>, focus: Option<usize>) {
        self.sprite(out, "system_titlebg", [0.0, 0.0, W, 392.0], None);
        self.sprite(out, "system_scenebg", [752.0, 0.0, 1168.0, 80.0], None);
        self.sprite(out, "underline", [0.0, 72.0, W, 16.0], None);
        let phase = game.var("F023");
        // sub_4C370: the cell table by language (overseas: English + 232, Chinese + 308, Japanese + 156 words)
        let cells = match game.assets.language() {
            0 => PHASE_TITLES,
            1 => PHASE_TITLES_EN,
            _ => PHASE_TITLES_ZH,
        };
        if let Some(&[x0, y0, x1, y1]) = usize::try_from(phase).ok().and_then(|p| cells.get(p)) {
            let src = [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32];
            self.sprite(out, "system_phasetitle_ja", [8.0, 0.0, src[2], src[3]], Some(src));
        }
        let title = usize::try_from(game.var("F022")).ok().and_then(|t| self.sysmes.get(t + 395)).cloned();
        // text slot 60 (sub_4C370, flags 0x181000: the system font): colour 0 (black), 0.36 of the 60 px glyph,
        // spacing -3 + 4; sub_AB710 places it at (608, 17) with 0x200 (sub_A84F0: nn::font flags 0x22, right
        // aligned, top origin)
        if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
            if self.font_sys.is_none() {
                self.font_sys = Self::sys_font(game, r);
            }
            if let Some(font) = self.font_sys.as_mut() {
                const SIZE: [f32; 2] = [60.0 * 0.36, 60.0 * 0.36];
                let w = font.measure_slot(&t, SIZE, 1.0);
                font.draw_str_slot(r, out, &t, SIZE, 1.0, [1824.0 - w, 17.0 * H / 448.0 + SYS_ASCENT * gs() * SIZE[1]], [0.0, 0.0, 0.0, 1.0]);
            }
        }
        // texture 502 at (18.667, 1) in 640x448 units (sub_4C370)
        self.sprite(out, "menu_window", [56.0, H / 448.0, 312.0, H], None);
        // sub_4CB30: while saving is off (F025) Save and Load show their normal cell in every state at alpha
        // 0x8F (sprite sets 0x5912F8 / 0x591348)
        let locked = game.var("F025") != 0;
        for i in 0..5 {
            // menu_btn_ja: rows Save, Load, Config, FlowChart, Title; columns normal / hover / selected
            let rc = [80.0, 105.0 + i as f32 * 75.0, 224.0, 88.0];
            if locked && i < 2 {
                let src = [24.0, 24.0 + i as f32 * 104.0, 224.0, 88.0];
                self.sprite_c(out, "menu_btn_ja", rc, Some(src), 143.0 / 255.0, Blend::Alpha);
                continue;
            }
            let col = if open == Some(i) {
                2.0
            } else if focus == Some(i) {
                1.0
            } else {
                0.0
            };
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

    /// One CG grid (cGalleryCGMain / cGalleryCGSub, sub_AF78): thumbnails at 392 + 240 col, 109 + 139 row, and on
    /// entry `cur` the cursor frame with its glow at `flash`.
    fn draw_cg_grid(&self, out: &mut Vec<Quad>, sub: Option<usize>, scroll: f32, cur: Option<usize>, flash: Option<f32>) {
        for (k, id) in self.cg_grid(sub).into_iter().enumerate() {
            let rc = Self::cg_cell(k, scroll);
            if rc[1] + rc[3] < GAL_CLIP[0] || rc[1] > GAL_CLIP[1] {
                continue;
            }
            // cg_thumb_all cell (154 * (t % 36), 88 * (t / 36))
            if let Some(e) = self.cg_thumb(sub, id) {
                use crate::gallery::{THUMB_COLS, THUMB_H, THUMB_W};
                let t = e.thumb as u32;
                let src = [(t % THUMB_COLS) as f32 * THUMB_W, (t / THUMB_COLS) as f32 * THUMB_H, THUMB_W, THUMB_H - 1.0];
                self.sprite(out, "cg_thumb_all", rc, Some(src));
            }
            if cur == Some(id) {
                // cg_cursor frame (prio 250) and over it its glow (prio 210; lower draws on top) at
                // (x - 8, y - 6), both blend mode 2 (sub_1FA44: blend state 0, default shader, plain alpha)
                let at = [rc[0] - 8.0, rc[1] - 6.0, 248.0, 144.0];
                self.sprite(out, "cg_cursor", at, Some([8.0, 8.0, 248.0, 144.0]));
                if let Some(f) = flash.filter(|&f| f > 0.0) {
                    self.sprite_c(out, "cg_cursor", at, Some([264.0, 8.0, 248.0, 144.0]), f, Blend::Alpha);
                }
            }
        }
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
    fn draw_title(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        if crate::assets::witch() {
            self.wt_draw(game, r, out);
            // jp_8D5D0 then the load screen (state 25), the dialog and the popup
            if self.wt.st == 25 {
                self.ws_draw(game, r, out);
            }
            // sub_BD90 (layer 21)
            self.wc_draw(game, r, out);
            self.wd_draw(game, r, out);
            self.wp_draw(game, r, out);
            return;
        }
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
            let cy = fx::logo_clip_y();
            let clip = [[0.0, 0.0], [W, 0.0], [W, cy], [0.0, cy]];
            for q in &mut out[n0..] {
                q.clip = Some(clip);
            }
        }
        // sub_8D5D0: three slices of texture 317 (jd_65CBD0 + 24 * language) at unk_231580[language] in
        // 960x544 units; English draws a wider logo lower, in two slices (the first cell is empty)
        let english = game.assets.language() == 1;
        let (lx, ly, w, slices) = if english {
            (35.0, 111.82222 * H / 544.0, 1850.0, [(0.0, 1.0), (1.0, 330.0), (330.0, 440.0)])
        } else {
            (680.0, 36.266666 * H / 544.0, 560.0, [(0.0, 285.0), (285.0, 570.0), (570.0, 648.0)])
        };
        for (k, (y0, y1)) in slices.into_iter().enumerate() {
            let alpha = if self.logo_rising { a.powi(k as i32 + 1) } else { a };
            let w = if english && k == 0 { 1.0 } else { w };
            self.sprite_c(out, "title_titlelog", [lx, ly + y0, w, y1 - y0], Some([0.0, y0, w, y1 - y0]), alpha, Blend::Alpha);
        }
        if self.press {
            // press_start blinks (qword_1536188: 1/60 per frame up and down), top-centre at (960, 823)
            let blink = 1.0 - ((self.press_t / 60.0) % 2.0 - 1.0).abs();
            self.sprite_c(out, "press_start", [724.0, 823.17, 472.0, 88.0], None, blink, Blend::Alpha);
        } else {
            let pulse = 1.0 - ((self.title_clock * 0.5) % 2.0 - 1.0).abs();
            // texture 321 at unk_231560[language] (960x544 units; English y 196.948)
            let ly = if game.assets.language() == 1 { 196.94815 } else { 278.04443 } * H / 544.0;
            self.sprite_c(out, "titlemenu_lumi", [720.0, ly, 488.0, 256.0], None, hi * pulse, Blend::Add);
            let entries = self.title_entries();
            for (i, &(_, tex, normal)) in entries.iter().enumerate() {
                let rc = Self::title_slot(i, entries.len());
                let src = Self::title_src(normal, usize::from(self.hover == Some(i)));
                self.sprite_c(out, tex, [rc[0], rc[1], src[2], src[3]], Some(src), hi, Blend::Alpha);
            }
        }
        self.sprite(out, "titlemenu_(c)type-moon", [872.0, 1008.0, 184.0, 64.0], None);
        self.draw_complete(game, r, out);
        if self.black_under && self.black > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, self.black.min(1.0)]));
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
        // the line and light (cGalleryMenu + 104 / + 112) hide whenever the mode's content takes the focus (state 4:
        // sub_6E28, sub_7328, sub_6BBC) and come back with the buttons' focus (sub_82FC)
        if let Some(i) = cur.filter(|_| self.gal_side) {
            let blink = 0.5 + 0.5 * (self.title_clock * 3.0).sin().abs();
            self.sprite_c(out, "systemcursor_light", [0.0, 96.0 + i as f32 * 80.0, 528.0, 240.0], None, blink, Blend::Add);
            self.sprite_c(out, "gallerycursor_line", [80.0, 192.0 + i as f32 * 80.0, 304.0, 56.0], None, 1.0, Blend::Add);
        }
        self.fade_quads(out, n1, self.scr_a);
    }

    /// sub_4A8F0: navy bar (0.0039, 0.2353, 0.3961 at 50%) over y 424.36..449 of 448, then the icons and
    /// words from x 55, bottom-aligned to y 445 / 444 of 448; each advances by its width + 10 / + 8.
    /// The guide index `screen` shows (sub_4A8F0; cGalleryGuide via content vt + 136 in the gallery).
    fn guide_for(&self, game: &mut Game, screen: Screen) -> Option<usize> {
        match screen {
            Screen::Movies | Screen::Endings => Some(10),
            // the mode buttons: 10; the content: its own (cGalleryGuide via vt + 136; music panel 10, list 0)
            s if Self::gal_mode(s).is_some() && self.gal_side => Some(10),
            Screen::Gallery => Some(if self.gal_scroll_max() > 0.0 { 0 } else { 10 }),
            Screen::Music if matches!(self.hover.and_then(|h| self.items(game).get(h).map(|(_, a)| *a)), Some(Action::MusicCtl(_) | Action::MusicVol)) => Some(10),
            Screen::Flowchart { .. } => Some(if self.flow.as_ref().is_some_and(|c| c.scroll_max > 0.0) { 0 } else { 10 }),
            Screen::CgView { .. } => Some(9),
            // the overseas save list's guide (11) adds X 削除
            Screen::SaveLoad { .. } if game.assets.build == knife_core::exe::Build::Overseas => Some(11),
            Screen::Music | Screen::SaveLoad { .. } => Some(0),
            // sub_2AA80 (overseas 0x2BA10): the Controls tab (dword_AED32C == 2) has guide 1 (B back, L/R page)
            Screen::Config => Some(if self.conf_tab == 2 { 1 } else { 2 }),
            Screen::Backlog { .. } => Some(3),
            _ => None,
        }
    }

    /// cGallery's guide (a1 + 184, sub_5C4C(guide, content vt + 136, 0.5) as the content changes): another
    /// index fades the shown one out (sub_5EA4: 0.5 s quad out), then the new one in (0.5 s quad in); an index
    /// asked for while it fades out waits for that (a1 + 52).
    fn gal_guide_tick(&mut self, game: &mut Game, secs: f32) {
        // the gallery's chart is its content too (state 5, sub_7624): the guide crossfades to the chart's index
        // and back to 10 once the chart has closed
        let gal = Self::gal_mode(self.screen).is_some() || matches!(self.screen, Screen::Flowchart { gallery: true });
        let want = if gal { self.guide_for(game, self.screen) } else { None };
        let Some(w) = want else {
            // the guide comes with the gallery (sub_6124), shown
            if !matches!(self.screen, Screen::CgView { .. }) {
                self.gal_guide = GalGuide::shown(10);
            }
            return;
        };
        let g = &mut self.gal_guide;
        if g.dir < 0 {
            g.next = w;
        } else if w != g.idx {
            g.dir = -1;
            g.t = 0.0;
            g.next = w;
        }
        if g.dir != 0 {
            g.t += secs / 0.5;
            if g.t >= 1.0 {
                g.t = 0.0;
                if g.dir < 0 {
                    g.idx = g.next;
                    g.dir = 1;
                } else {
                    g.dir = 0;
                }
            }
        }
    }

    /// `dy`: moved down by this much (of 448).
    fn draw_guide(&self, out: &mut Vec<Quad>, white: &TexRef, g: usize, dy: f32) {
        let k = H / 448.0;
        out.push(Quad::rect(white.clone(), [0.0, (424.36 + dy) * k, W, (449.0 - 424.36) * k], [0.0039, 0.2353, 0.3961, 0.5]));
        let mut x = 55.0;
        for &(kind, id) in GUIDE_SEQ[g] {
            let (tex, r, bottom, pad) = if kind == 0 {
                ("help_key", GUIDE_ICONS[id as usize], 445.0, 6.0)
            } else {
                ("help_txt_ja", self.guide_words.1.get(id as usize).copied().unwrap_or(GUIDE_WORDS[id as usize]), 444.0, 4.0)
            };
            let [x0, y0, x1, y1] = r.map(f32::from);
            self.sprite(out, tex, [x, (bottom + dy) * k - (y1 - y0), x1 - x0, y1 - y0], Some([x0, y0, x1 - x0, y1 - y0]));
            x += x1 - x0 + pad + 4.0;
        }
    }

    /// dialog_win (824x416) centred on the screen, scaled by the opening animation.
    /// sub_4AE90's thumbnail (the quick save, record 139, loaded by sub_89590 and made by sub_89AD0): 100 *
    /// 0.82667 * 0.825 x 53 * 1.0859 * 0.825 units hanging from (320, 202) (English 214) over a black frame 2
    /// units wider each side, once it is made.
    fn draw_dialog_thumb(&self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some(d) = self.slots.get(save::quick_slot()).cloned().flatten() else { return };
        let Some(t) = game.preview(save::quick_slot(), &d) else { return };
        let (w, h) = (100.0 * 0.82667 * 0.825, 53.0 * 1.0859 * 0.825);
        let (ux, uy) = (W / 640.0, H / 448.0);
        // the frame's top: 212 for English (the longer message), else 200; Witch on the Holy Night's jp_4AE90
        // puts it at 179.79 for every language
        let y = if crate::assets::witch() {
            179.79259
        } else if crate::text::english() {
            212.0
        } else {
            200.0
        };
        out.push(Quad::rect(r.white.clone(), [(318.0 - w / 2.0) * ux, y * uy, (w + 4.0) * ux, (h + 4.0) * uy], [0.0, 0.0, 0.0, 1.0]));
        out.push(Quad::rect(t, [(320.0 - w / 2.0) * ux, (y + 2.0) * uy, w * ux, h * uy], [1.0; 4]));
    }

    /// dialog_win at alpha `t`, scaled by `k` about the centre.
    fn draw_dialog_win(&self, out: &mut Vec<Quad>, t: f32, k: f32) {
        let (w, h) = (824.0 * k, 416.0 * k);
        self.sprite_c(out, "dialog_win", [(W - w) / 2.0, (H - h) / 2.0, w, h], None, t, Blend::Alpha);
    }

    /// SELS / SELA question (sub_85900 / sub_85E40, 640x448 units): dialog_BG over the screen and dialog_win
    /// at (320, 224) at the choice's alpha, the window scaled by its pop; the message (text slot 20, 0.37 x
    /// 0.46044 of the base glyph times the pop) centred from y 224 - 42.311 * scale, and the No / Yes buttons.
    fn draw_choice_dialog(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let Some((msg, hover, a, k)) = game.choice_dialog() else { return };
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        self.sprite_c(out, "dialog_BG", [0.0, 0.0, W, H], None, a, Blend::Alpha);
        let (w, h) = (824.0 * k, 416.0 * k);
        self.sprite_c(out, "dialog_win", [(W - w) / 2.0, (H - h) / 2.0, w, h], None, a, Blend::Alpha);
        let msg = self.sysmes.get(msg).cloned().unwrap_or_default();
        // text slot 20 (sub_AB4D0 scale 0.37 k x 0.46044 k, spacing 0 + 4 / 0 + 14 px, sub_A7790)
        let size = [60.0 * 0.37 * k, 60.0 * 0.46044 * k];
        let lay = crate::game::LAYOUT;
        let top = (224.0 - 42.311 * k) * H / 448.0;
        let baseline = top + crate::text::msg_ascent(lay.ascent * size[1] / lay.size);
        let pitch = crate::text::line_feed() * gs() * size[1] / crate::text::BASE_SIZE + 14.0;
        let font = game.font_mut();
        for (i, line) in msg.split('^').enumerate() {
            let lw = font.measure_slot(line, size, 4.0);
            font.draw_str_slot(r, out, line, size, 4.0, [(W - lw) / 2.0, baseline + i as f32 * pitch], [1.0, 1.0, 1.0, a]);
        }
        for (i, &(x, yes)) in crate::game::DIALOG_BUTTONS.iter().enumerate() {
            let y = if yes { 0.0 } else { 67.0 };
            let src = if hover == Some(i) { [0.0, y + 135.0, 216.0, 56.0] } else { [224.0, y, 216.0, 56.0] };
            let rc = [x * W / 640.0, crate::game::DIALOG_BUTTON_Y * H / 448.0, 216.0, 56.0];
            self.sprite_c(out, "dialog_btn_ja", rc, Some(src), a, Blend::Alpha);
        }
    }

    /// Dialog / popup message (text slot 12 + n, sub_4AD40; 10 + n, sub_4ABE0): scale 0.6 x 0.7 of the 60 px
    /// base glyph, centred lines from `top`, character spacing 0 + 4 px and line spacing `line_sp` + 14 px
    /// (sub_A7790 -> sub_C3944); '^' breaks lines. Switch (load dialog): glyphs 36 x 42, 40.6 px apart.
    fn draw_dialog_text(&self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, msg: &str, top: f32, line_sp: f32) {
        const SIZE: [f32; 2] = [60.0 * 0.6, 60.0 * 0.7];
        const SPACING: f32 = 4.0;
        let lay = crate::game::LAYOUT;
        let baseline = top + crate::text::msg_ascent(lay.ascent * SIZE[1] / lay.size);
        let pitch = crate::text::line_feed() * gs() * SIZE[1] / crate::text::BASE_SIZE + line_sp + 14.0;
        let font = game.font_mut();
        for (k, line) in msg.split('^').enumerate() {
            let w = font.measure_slot(line, SIZE, SPACING);
            font.draw_str_slot(r, out, line, SIZE, SPACING, [(W - w) / 2.0, baseline + k as f32 * pitch], [1.0; 4]);
        }
    }

    pub fn draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        if self.guide_words.0 != game.assets.language() {
            self.guide_words = (game.assets.language(), game.assets.guide_words());
        }
        self.draw_screen(game, r, out);
        if self.screen == Screen::Game {
            self.draw_choice_dialog(game, r, out);
            self.draw_touch_game(r, out);
            // a story dialog still fading out after its answer (`wd_ask`)
            if crate::assets::witch() && self.w.dlg.shown() {
                self.wd_draw(game, r, out);
            }
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
            Screen::Witch => self.witch_draw(game, r, out),
            Screen::Title => self.draw_title(game, r, out),
            Screen::Menu => {
                // sub_4C370 (640x448 units scaled to the canvas): title band, scene band, underline,
                // phase title (F023 indexes the rect table at dword_5912E8+624), window on the left,
                // buttons, cursor line + light, and the scene title SYSMES[F022+395] at the right
                // the line / dot cursor shows on the remembered tab as soon as the menu opens; the tab is
                // highlighted only while it has the focus
                let cur = Some(self.sys_tab);
                let focus = self.hover;
                self.draw_menu_layer(out);
                let n0 = out.len();
                self.draw_menu_base(game, r, out, None, cur, focus);
                self.fade_quads(out, n0, self.scr_a * self.menu_a);
            }
            Screen::SaveLoad { save, scroll } => {
                if self.stack.contains(&Screen::Menu) {
                    self.draw_menu_layer(out);
                    // the line / dot cursor only shows while the tabs have the focus
                    let cur = self.side.then_some(self.sys_tab);
                    self.draw_menu_base(game, r, out, Some(usize::from(!save)), cur, cur);
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
                    if hovered(i) && self.save_flash > 0.0 {
                        self.sprite_c(out, "save_win", [fx, fy, 960.0, 272.0], Some([31.0, 336.0, 960.0, 272.0]), self.save_flash, Blend::Add);
                    }
                    let Some(d) = self.slots.get(slot).cloned().flatten() else { continue };
                    if hovered(i) {
                        selected = Some(d.clone());
                    }
                    // sub_4F240: 100 * 0.827 x 53 * 1.092 units at (151, y + 23.23), quad scale 1.15 about its top left
                    let thumb = [fx + 52.0, fy + 56.0, 300.0 * 0.827 * 1.15, 53.0 * 1.092 * 1.15 * H / 448.0];
                    // drawn only once made (word_F1F438[40]); nothing in its place until then. Overseas the whole
                    // entry (frame overlay, date, time, AUTO tag, titles) waits for it too: a fresh save's row stays
                    // blank until its thumbnail is made, while the info panel already shows the record
                    let pic = game.preview(slot, &d);
                    if pic.is_none() && game.assets.build == knife_core::exe::Build::Overseas {
                        continue;
                    }
                    if let Some(t) = pic {
                        out.push(Quad::rect(t, thumb, [1.0; 4]));
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
                    if slot >= save::auto_first() {
                        self.sprite(out, "saveauto", [fx + 744.0, fy + 26.0, 192.0, 32.0], None);
                    }
                    let var = |k: &str| d.vars.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map_or(0, |v| v.1);
                    let phase = var("F023").clamp(0, 37) as usize;
                    let [x0, y0, x1, y1] = match (game.assets.build, game.assets.language()) {
                        (knife_core::exe::Build::Overseas, 1) => SAVE_TITLES_EN[phase],
                        (b, _) => {
                            let mut r = SAVE_TITLES[phase];
                            if b == knife_core::exe::Build::Overseas && matches!(phase, 16 | 18) {
                                r[2] = 571;
                            }
                            r
                        }
                    };
                    let src = [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32];
                    self.sprite(out, "savetitle", [fx + 359.0, fy + 121.0, src[2], src[3]], Some(src));
                    let scene = usize::try_from(var("F022")).ok().and_then(|t| self.sysmes.get(t + 395)).cloned();
                    // sub_50B10: slots 34..37 (flags 0x183040: the system font) at 0.35, spacing
                    // -3 + 4; sub_4F240 places them at (274.667, y + 79.644)
                    if let Some(sc) = scene.filter(|s| !s.trim().is_empty()) {
                        if self.font_sys.is_none() {
                            self.font_sys = Self::sys_font(game, r);
                        }
                        if let Some(font) = self.font_sys.as_mut() {
                            const SIZE: [f32; 2] = [60.0 * 0.35, 60.0 * 0.35];
                            let base = fy + 79.644 * H / 448.0 + SYS_ASCENT * gs() * SIZE[1];
                            font.draw_str_slot(r, out, &sc, SIZE, 1.0, [fx + 423.0, base], [0.0, 0.0, 0.0, 1.0]);
                        }
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
                    // sub_50B10: slot 38 in the message font (flags 0x83040) at 0.4, box 130 x 46, spacing
                    // (-5, -7) + (4, 14), laid out like the messages (dash runs in the exfont); sub_4F240 places it
                    // at (463.333, 259.259)
                    let size = 60.0 * 0.4 * gs();
                    let lay = crate::text::Layout {
                        left: 1390.0,
                        // sub_A7790: the box width in px, (int)(130 * 1920 / 640)
                        right: 1390.0 + 390.0,
                        size,
                        // character spacing -5 + 4 (English -4 + 4, sub_50B10)
                        spacing: if crate::text::english() { 0.0 } else { -1.0 },
                        sx: 1.0,
                        line_height: crate::text::line_feed() * size / crate::text::BASE_SIZE + 7.0,
                        ascent: crate::text::msg_ascent(crate::game::LAYOUT.ascent * 60.0 * 0.4 / crate::game::LAYOUT.size),
                        ruby_y: crate::text::RUBY_Y,
                    };
                    let runs = crate::text::parse_markup_edge(self.save_notes.as_deref().unwrap_or(""), false);
                    let mut placed = Vec::new();
                    let font = game.font_mut();
                    let top = 259.259 * H / 448.0 + lay.ascent;
                    crate::text::layout(font, &lay, &runs, lay.left, top, 0, &mut placed);
                    // colour 0 (black); sub_A8330(38, 4): the writer shows 4 lines. Flag 0x2000 is style bit 1, for
                    // which sub_BDC2C lays the ruby out but keeps it hidden (+287, slot +101007 = !(a4 & 1)):
                    // Switch, the train page's note without the まえぶ over 前触
                    let shown = |pc: &&crate::text::PlacedChar| pc.baseline < top + 3.5 * lay.line_height && pc.size == lay.size;
                    for pc in placed.iter().filter(shown) {
                        if let Some(q) = font.placed_quad(r, pc, 0.0, 0.0, [0.0, 0.0, 0.0, 1.0]) {
                            out.push(q);
                        }
                    }
                    // the note slot has no edge: the plain bar sheet
                    out.extend(placed.iter().filter(shown).filter_map(|pc| game.bar_quad(pc, None, [0.0, 0.0, 0.0, 1.0], false)));
                }
                // the sub_9B0C4 widget (qword_F1F480, sub_50E00): range max(entries, 4) - 4, thumb at the top row
                let range = self.save_list(save).len().saturating_sub(SLOTS_PER_PAGE);
                if range >= 1 {
                    self.draw_scroll_bar(out, scroll as f32 / range as f32, 608.0);
                }
                self.fade_quads(out, n0, self.scr_a);
            }
            Screen::Config => {
                if self.stack.contains(&Screen::Menu) {
                    self.draw_menu_layer(out);
                    // the line / dot cursor only shows while the tabs have the focus
                    let cur = self.side.then_some(self.sys_tab);
                    self.draw_menu_base(game, r, out, Some(2), cur, cur);
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
                // over the submenu backdrop (sub_4B880), as the system menu / gallery left it: in game over the
                // blurred scene (flow_bg is translucent, alpha 140..255; Switch: the scene shows through)
                if matches!(screen, Screen::Flowchart { gallery: true }) {
                    out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
                }
                self.draw_menu_layer(out);
                // the chart fades in / out over 0.5 s (quad in / out); in the gallery the menu frame and
                // buttons fade the other way at the same time (sub_7328 / sub_7624: 0.5 s quad out / in) with
                // its content already gone, in game the system menu is hidden while the chart shows
                let s = self.scr_a;
                if matches!(screen, Screen::Flowchart { gallery: true }) && s < 1.0 {
                    self.draw_below(game, r, out, (1.0 - s) * (1.0 - s));
                }
                let nf = out.len();
                // flow_bg (object +72) has the chart's 1920x1022 scissor too (sub_1FA60): the guide bar's strip
                // shows the frame below (Switch EN in game: the bar over the blurred menu backdrop)
                self.sprite(out, "flow_bg", [0.0, 0.0, W, FLOW_VIEW], Some([0.0, 0.0, W, FLOW_VIEW]));
                // flow_infobase at its own size: 488x496, the overseas flow_infobase_en 488x560 (its "Note."
                // band under the picture, the description box taller)
                let base_h = self.tex.get("flow_infobase").map_or(496.0, |t| t.height as f32);
                self.sprite(out, "flow_infobase", [1352.0, 96.0, 488.0, base_h], None);
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
                // sub_11BEC hides them at once as the chart closes
                if let Some(c) = self.flow.as_ref().filter(|_| !self.closing) {
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
                let node = self.flow_node();
                if node != self.flow_flash.0 {
                    self.flow_old = self.flow_flash.0;
                    self.flow_flash = (node, self.title_clock);
                }
                let panel = |i: usize| self.flow.as_ref().map(|c| &c.nodes[i]).map(|n| (n.pos, n.preview, n.name, n.desc));
                let cur = node.and_then(panel);
                if let Some((p, ..)) = cur {
                    let dst = [p[0], p[1] - self.flow_scroll, 104.0, 64.0];
                    self.sprite(out, "flow_cur", dst, Some([8.0, 8.0, 104.0, 64.0]));
                    // sub_10BA4 on a new node: the glow sprite (flow_cur cell 120,8) at vertex alpha 1 runs
                    // unk_34B7F0, op 19 down to -1 over 30 frames (shown for 15), as the CG grid cursor
                    let a = 1.0 - 2.0 * (self.title_clock - self.flow_flash.1) * 60.0 / 30.0;
                    if a > 0.0 && self.sprite(out, "flow_cur", dst, Some([120.0, 8.0, 104.0, 64.0])) {
                        out.last_mut().unwrap().color[3] = a;
                    }
                }
                for q in &mut out[n0..] {
                    q.clip = Some(clip);
                }
                self.draw_scroll_widget(out, self.flow_scroll, self.flow.as_ref().map_or(0.0, |c| c.scroll_max));
                // sub_16E00 on a new node: the old preview, title and description fade out and the new texts
                // fade in over 5 frames (sub_21A20 on alpha, 0.08333 s, ease-out quad); the new preview shows
                // at once over the old one (old preview prio 350)
                let t = ((self.title_clock - self.flow_flash.1) / (5.0 / 60.0)).clamp(0.0, 1.0);
                let e = t * (2.0 - t);
                let old = self.flow_old.filter(|_| e < 1.0).and_then(panel);
                for (info, a, preview_a) in [(old, 1.0 - e, 1.0 - e), (cur, e, 1.0)] {
                    let Some((_, preview, name, desc)) = info else { continue };
                    let overseas = game.assets.build == knife_core::exe::Build::Overseas;
                    if let Some(img) = game.assets.flow_preview_name(preview).and_then(|n| game.assets.image(r, &n)) {
                        // scale 2/3 in the scene's 1280-wide space: native texels on the canvas
                        let (w, h) = (img.tex.width as f32, img.tex.height as f32);
                        let uv = img.tex.uv_max();
                        // overseas: the previews of nodes 107, 114, 188 and 481 stack a 645-wide strip per
                        // language, from y (int)(language * 364.5 + 0.5), 364 - language % 2 high (sub_250A0)
                        let strip = (overseas && matches!(preview, 107 | 114 | 188 | 481)).then(|| {
                            let l = game.assets.language() as i32;
                            ((l as f32 * 364.5 + 0.5) as i32 as f32, (364 - (l & 1)) as f32)
                        });
                        let q = match strip {
                            Some((y0, sh)) => Quad::rect(img.tex.clone(), [1380.0, 120.0, 645.0, sh], [1.0, 1.0, 1.0, preview_a])
                                .with_uv_rect(0.0, y0 / h * uv[1], 645.0 / w * uv[0], (y0 + sh) / h * uv[1]),
                            None => Quad::rect(img.tex.clone(), [1380.0, 120.0, w, h], [1.0, 1.0, 1.0, preview_a]).with_uv_rect(0.0, 0.0, uv[0], uv[1]),
                        };
                        out.push(q);
                    }
                    let text = |i: i32| usize::try_from(i).ok().and_then(|i| self.sysmes.get(i)).cloned().unwrap_or_default();
                    let (name, desc) = (text(name), text(desc));
                    // sub_16E00: two sub_227F0 type-3 text widgets (slots 175 + 2k / 176 + 2k) at (1392, 404) and
                    // (1392, 452), widget scale (1.2, 1.0), boxes 400 x 48 / 400 x 200, spacing (-3, -12); title
                    // colour 0x4210 (RGB565 66,65,132), description colour 0 (black). Neither sets 0x100000, so
                    // the message font. sub_25DD4 -> sub_AB4D0 scale 1.2 * 640 / 1920 x 1.0 * 448 / 1080,
                    // character spacing -3 + 4 px, line spacing -12 + 14 px; sub_A7790 box width (int)(400 * 1920 / 640) px.
                    // Overseas: boxes 200 x 48 / 150 x 200 (English: 136 wide, both at x 1390 = 0x44ADC000, character spacing
                    // -3.75); sub_25DD4 adds slot flags 0x9001000 (0x1000: no sub_C3834 hold, which in the
                    // Japanese build only hides the texts on the change frame, where the fade-in is at 0)
                    const SIZE: [f32; 2] = [1.2 * 640.0 / 1920.0 * 60.0, 448.0 / 1080.0 * 60.0];
                    let english = crate::text::english();
                    let spacing = if english { -3.75 + 4.0 } else { 1.0 };
                    let x = if english { 1390.0 } else { 1392.0 };
                    let (title_box, desc_box) = match (overseas, english) {
                        (false, _) => (400.0, 400.0),
                        (true, false) => (200.0, 150.0),
                        (true, true) => (200.0, 136.0),
                    };
                    let lay = crate::game::LAYOUT;
                    let ascent = crate::text::msg_ascent(lay.ascent * SIZE[1] / lay.size);
                    let pitch = crate::text::line_feed() * gs() * SIZE[1] / crate::text::BASE_SIZE + 2.0;
                    let font = game.font_mut();
                    let title = [66.0 / 255.0, 65.0 / 255.0, 132.0 / 255.0, a];
                    // SYSMES marks line breaks with '^'; the writer wraps at the box width
                    let mut block = |text: &str, y: f32, bx: f32, color: [f32; 4]| {
                        let mut li = 0;
                        for para in text.split('^') {
                            for line in font.wrap_slot(para, SIZE, spacing, bx * 3.0) {
                                font.draw_str_slot(r, out, &line, SIZE, spacing, [x, y + ascent + li as f32 * pitch], color);
                                li += 1;
                            }
                        }
                    };
                    block(&name, 404.0, title_box, title);
                    block(&desc, 452.0, desc_box, [0.0, 0.0, 0.0, a]);
                }
                if cur.is_some() {
                    self.sprite(out, "flow_info", [1672.0, 120.0, 144.0, 32.0], None);
                }
                self.fade_quads(out, nf, s * s);
            }
            Screen::Gallery => {
                let scroll = self.gal_scroll.pos;
                let nb = self.draw_hub_bg(out, &white, Some("cg_title"));
                let n0 = out.len();
                let cur = hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
                // sub_A968 on each cursor move: the glow's vertex alpha is set to 1 and op 19 of unk_337380
                // tweens it linearly to -1 over 30 frames (hidden at <= 0), so it flashes for 15 frames
                let cg = match cur {
                    Some(Action::Cg(g)) => Some(g),
                    _ => None,
                };
                if cg != self.cg_flash.0 {
                    self.cg_flash = (cg, self.title_clock);
                }
                let flash = 1.0 - 2.0 * (self.title_clock - self.cg_flash.1) * 60.0 / 30.0;
                // a grid change (sub_8D08): the old grid fades out (ease-out quad) as the new one fades in (ease-in
                // quad), both over 0.5 s
                let u = self.cg_in / CG_FX_SECS;
                if let Some((sub, sc, oc)) = self.cg_old {
                    let n1 = out.len();
                    self.draw_cg_grid(out, sub, sc, oc, None);
                    self.fade_quads(out, n1, (1.0 - u) * (1.0 - u));
                }
                let n1 = out.len();
                self.draw_cg_grid(out, self.cg_sub, scroll, cg, Some(flash));
                self.fade_quads(out, n1, u * u);
                let clip = [[0.0, GAL_CLIP[0]], [W, GAL_CLIP[0]], [W, GAL_CLIP[1]], [0.0, GAL_CLIP[1]]];
                for q in &mut out[n0..] {
                    q.clip = Some(clip);
                }
                self.draw_scroll_widget(out, scroll, self.gal_scroll_max());
                self.draw_hub_menu(out, &items, hover, Some(0), nb);
            }
            Screen::Music => {
                let scroll = self.gal_scroll.pos.trunc();
                let o = -scroll;
                if o >= -200.0 {
                    self.music_bg_off = o;
                } else if o < -4465.0 {
                    self.music_bg_off = o + 4265.0 - 326.0;
                }
                let nb = self.draw_hub_bg(out, &white, Some("music_title"));
                let cur = hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
                // sub_1B2F0 lights the list cursor row (a1 + 52); sub_1B43C, entered when the focus leaves the list
                // for the panel or the side bar, dims every row (Switch: B back to the tabs leaves no row lit).
                // The text slots draw in the shared system font (`sys_font`).
                let list_row = if self.gal_side || matches!(cur, Some(Action::MusicCtl(_) | Action::MusicVol)) { None } else { Some(self.music_row) };
                if self.font_sys.is_none() {
                    self.font_sys = Self::sys_font(game, r);
                }
                let n = crate::gallery_table::MUSIC.len();
                let n0 = out.len();
                // music_soundbg (0x34D238): translucent list panel at (392, 106), prio 500, clipped with the rows;
                // it follows the scroll only over the first 200 px and past 4465 (sub_1B078, `music_bg_off`)
                self.spr(out, "music_soundbg", 392.0, 106.0 + self.music_bg_off, None);
                for i in 0..n {
                    let y = 109.0 + i as f32 * 59.0 - scroll;
                    if y + 64.0 < GAL_CLIP[0] || y > GAL_CLIP[1] {
                        continue;
                    }
                    self.spr(out, "music_bgline", 408.0, y + 64.0, None);
                    let ok = self.music_ok.get(i).copied().unwrap_or(false);
                    let id = if ok { crate::gallery_table::MUSIC[i].1 as usize } else { 244 };
                    // the one-line rows leave out the title's '^' break (the panel title breaks there)
                    let title = self.sysmes.get(id).map_or_else(|| "？？？".into(), |t| t.replace('^', ""));
                    // white rows, locked ones too (sub_1A1A4 builds them alike; the playing track is not marked),
                    // every row at half alpha (sub_1FA20(row, 0.5)) but the cursor row
                    let c = [1.0, 1.0, 1.0, if list_row == Some(i) { 1.0 } else { 0.5 }];
                    // sub_1A1A4: text slot 61 + i at (438, 131 + 59 i), scale 1.3333 x 1.2 (26.67 x 29.87 px cells)
                    if let Some(font) = self.font_sys.as_mut() {
                        const ROW: [f32; 2] = [60.0 * 1.3333 * 640.0 / 1920.0, 60.0 * 1.2 * 448.0 / 1080.0];
                        font.draw_str_slot_rimmed(r, out, &title, ROW, 4.0, [438.0, y + 22.0 + SYS_ASCENT * gs() * ROW[1]], c, 1);
                    }
                }
                let clip = [[0.0, GAL_CLIP[0]], [W, GAL_CLIP[0]], [W, GAL_CLIP[1]], [0.0, GAL_CLIP[1]]];
                for q in &mut out[n0..] {
                    q.clip = Some(clip);
                }
                self.draw_scroll_widget(out, scroll, MUSIC_SCROLL_MAX);
                // cGalleryMusicPanel (sub_1B7D8)
                self.spr(out, "music_btnbg", 1368.0, 104.0, None);
                // the title and credits show while a track plays or is paused; a stop clears them (loc_1DC40)
                let shown = self.playing_track;
                if let Some(t) = shown {
                    // sub_1DAB8: title and credit lines (歌 / 作詞 / 作曲 / 編曲 + name) starting at
                    // 260 - 32 * (lines / 2) - 16. Text objects (box 420 x 64, sub_1B7D8) at scale 1.2: sub_25DD4 hands
                    // sub_A7790 1.2 * 640 / 1920 x 1.2 * 448 / 1080 of the 60 px base glyph; the title (slot 52,
                    // params 0x4697A8) has line spacing -16, which sub_25DD4 scales by the object's 1.2 like the glyphs:
                    // (1.2 * -16 + 14) px after the message fonts' line feed at the glyph height (Switch, English
                    // "Comp:^Keita Haga": baselines 30 px apart). The credits start the title's height (sub_261CC: last
                    // line minus first) + 58 below it, 32 apart.
                    const SIZE: [f32; 2] = [60.0 * 1.2 * 640.0 / 1920.0, 60.0 * 1.2 * 448.0 / 1080.0];
                    let credits = crate::gallery_table::MUSIC_CREDITS[t];
                    // English: the lyrics / composition / arrangement rows break after their label ('^'), count
                    // as two lines, sit 32 more apart and the first credit row 4 more (overseas sub_1DAB8, the
                    // three given line spacing -16 by sub_25FC8)
                    let english = crate::text::english();
                    let lines: usize = 1 + credits.iter().enumerate().filter(|&(_, &c)| c >= 0).map(|(k, _)| if english && k > 0 { 2 } else { 1 }).sum::<usize>();
                    let mut y = 260.0 - 32.0 * (lines / 2) as f32 - 16.0;
                    let pitch = crate::text::line_feed() * gs() * SIZE[1] / crate::text::BASE_SIZE + 1.2 * -16.0 + 14.0;
                    let Some(font) = self.font_sys.as_mut() else { return };
                    // sub_26054's flag 0x100 -> nn::font draw flags 0x11 (sub_A84F0: each line centred, origin centred);
                    // the lines keep their trailing spaces. Switch: all three lines centre 7 px left of x 1600
                    let mut centred = |s: &str, y: f32| {
                        let w = font.measure_slot(s, SIZE, 4.0);
                        font.draw_str_slot_rimmed(r, out, s, SIZE, 4.0, [1593.0 - w / 2.0, y + SYS_ASCENT * gs() * SIZE[1]], [1.0; 4], 1);
                    };
                    let title = self.sysmes.get(crate::gallery_table::MUSIC[t].1 as usize).cloned().unwrap_or_default();
                    for (k, part) in title.split('^').enumerate() {
                        centred(part, y + k as f32 * pitch);
                    }
                    y += pitch * (title.split('^').count() - 1) as f32 + 58.0;
                    for (k, &c) in credits.iter().enumerate() {
                        if c >= 0 {
                            let label = self.sysmes.get(245 + k).cloned().unwrap_or_default();
                            let name = self.sysmes.get(c as usize).cloned().unwrap_or_default();
                            let sep = if english && k > 0 { "^" } else { " " };
                            for (n, part) in format!("{label}{sep}{name}").split('^').enumerate() {
                                centred(part, y + n as f32 * pitch);
                            }
                            y += 32.0;
                            if english {
                                y += if k > 0 { 32.0 } else { 4.0 };
                            }
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
                let len = shown.map_or(0.0, |t| (crate::gallery_table::MUSIC_LOOP[t].1 / 48) as f32 / 1000.0);
                let el = if self.playing_track.is_some() { self.music_clock } else { 0.0 };
                // text slots 50 / 51 (0x4699A8 / 0x4699D0): type 3 at (1548, 503) / (1562, 878), scale 1.25 x 1.0
                // (25 x 24.89 px cells), left-aligned; "%3d" pads the volume with spaces
                const NUM: [f32; 2] = [60.0 * 1.25 * 640.0 / 1920.0, 60.0 * 448.0 / 1080.0];
                let Some(font) = self.font_sys.as_mut() else { return };
                let time = format!("{:2}:{:02}", el as u32 / 60, el as u32 % 60);
                font.draw_str_slot_rimmed(r, out, &time, NUM, 4.0, [1548.0, 503.0 + SYS_ASCENT * NUM[1]], [1.0; 4], 1);
                let vol = format!("{:3}", (self.music_vol * 100.0) as i32);
                font.draw_str_slot_rimmed(r, out, &vol, NUM, 4.0, [1562.0, 878.0 + SYS_ASCENT * NUM[1]], [1.0; 4], 1);
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
            Screen::MoviePlay => {
                // the gallery under the player's curtain (alpha: quad out in, quad out back), the movie over it
                if let Some(m) = self.gal_movie {
                    let u = (m.t / 0.5).clamp(0.0, 1.0);
                    let a = match m.phase {
                        0 => u * (2.0 - u),
                        1 => 1.0,
                        _ => (1.0 - u) * (1.0 - u),
                    };
                    let c = if m.white { 1.0 } else { 0.0 };
                    if a < 1.0 {
                        self.draw_under(game, r, out);
                    }
                    out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [c, c, c, a]));
                    // only the player's movie: until it starts (state 1) the title's held movie is still loaded
                    if m.phase == 1 {
                        out.extend(game.movie_quad());
                    }
                }
            }
            Screen::Endings => {
                let nb = self.draw_hub_bg(out, &white, Some("ending_title"));
                let cur = hover.and_then(|h| items.get(h)).map(|(_, a)| *a);
                if Self::all_endings(game) {
                    let sx = if cur == Some(Action::Ending(24)) { 128.0 } else { 8.0 };
                    self.spr(out, "ending_btn", 1702.0, 894.0, Some([sx, 8.0, 112.0, 112.0]));
                }
                for e in 0..24 {
                    let ok = Self::ending_ok(game, e);
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
                // cGalleryCGPhoto (sub_BA18): a full-screen layer (0x466738, priority 700) at colour 0.85
                // (sub_E2490 0x3F59999A; Switch grey 217) under
                // the picture (priority 380), so the screen around a picture is light grey. Opening and closing
                // crossfade with the gallery as captured (`CgFx`).
                let u = (self.cg_t / CG_FX_SECS).min(1.0);
                let a = match self.cg_fx {
                    CgFx::Open => u * u,
                    CgFx::Close => (1.0 - u) * (1.0 - u),
                    _ => 1.0,
                };
                if a < 1.0 {
                    self.draw_under(game, r, out);
                    // the capture holds the gallery's own guide
                    let n = out.len();
                    let ga = self.gal_guide.alpha();
                    self.draw_guide(out, &white, self.gal_guide.idx, (1.0 - ga) * 25.0);
                    self.fade_quads(out, n, ga);
                }
                let n0 = out.len();
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.85, 0.85, 0.85, 1.0]));
                // sub_BE84 -> sub_251A4: the entry's source rect (x, y, w, h) of the image; overseas, a CG that
                // stacks a strip per language moves down by its height times the language
                let pic = |ui: &Self, game: &mut Game, group: usize, i: usize| {
                    let e = ui.cg_entry(group, i)?;
                    let strip = if crate::gallery::language_strip(e.name) { game.assets.language() as f32 } else { 0.0 };
                    // sub_BE84 -> sub_B6A20: the picture bakes through pre_alter like every image
                    let img = game.assets.image(r, e.name)?;
                    let (w, h) = (e.width.max(1) as f32, e.height.max(1) as f32);
                    // the table is in 1920-wide units, the images are stored at 1280 (IMG1685: 2106 x 1266 of a
                    // 1404 x 844 texture)
                    let (tw, th) = (img.tex.width.max(1) as f32 * 1.5, img.tex.height.max(1) as f32 * 1.5);
                    let uv = img.tex.uv_max();
                    let (x0, y0) = (e.x as f32, e.y as f32 + h * strip);
                    let src = [x0 / tw * uv[0], y0 / th * uv[1], (x0 + w) / tw * uv[0], (y0 + h) / th * uv[1]];
                    Some((img.tex.clone(), w, h, src))
                };
                if let Some((tex, w, h, [u0, v0, u1, v1])) = pic(self, game, group, pos) {
                    let rc = self.cg_rect(w, h);
                    out.push(Quad::rect(tex, rc, [1.0; 4]).with_uv_rect(u0, v0, u1, v1));
                }
                // a picture change: sprite 176 (the capture, here the old view) over the new picture at colour
                // alpha 1 - u through the rule, a sub_9FE00 draw (progress = that alpha, the rule read inverted)
                if let CgFx::Switch(og, old, zoom, pan, next) = self.cg_fx
                    && let Some((tex, w, h, [u0, v0, u1, v1])) = pic(self, game, og, old)
                {
                    let (rc, _, _) = Self::cg_geom(w, h, zoom, pan);
                    let c = 1.0 - u;
                    let mut q = Quad::rect(tex, rc, [1.0, 1.0, 1.0, c]).with_uv_rect(u0, v0, u1, v1);
                    q.rule = game
                        .assets
                        .rule(r, if next { "RULE_27" } else { "RULE_26" })
                        .map(|i| knife_render::RuleMask {
                            invert: true,
                            soft: game.stage.rule_soft,
                            ..knife_render::RuleMask::new(i.tex.clone(), c)
                        });
                    out.push(q);
                }
                self.fade_quads(out, n0, a);
            }
            // backlog (sub_27D10 / sub_28470): black at 75% over the scene, one history page in the
            // message layout, and the scroll widget (sub_9B0C4: scroll_bar at 615.67,43.56 and
            // scroll_slider running 41.07..293.27 at x 613, 640x448 units)
            Screen::Backlog { scroll } => {
                let n0 = out.len();
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 0.75]));
                out.extend(game.backlog_quads(r, scroll));
                let n = game.backlog_len();
                // sub_9B0C4: with nothing to scroll (max <= min) the slider sits at the top; the overseas
                // sub_27D10 draws the widget only with more than one page (AE71E4 >= 1)
                if n > 1 || game.assets.build != knife_core::exe::Build::Overseas {
                    self.draw_scroll_bar(out, if n > 1 { 1.0 - scroll as f32 / (n - 1) as f32 } else { 0.0 }, 608.0);
                }
                self.fade_quads(out, n0, self.scr_a);
            }
            Screen::Confirm(c) => {
                // sub_4AE90: the screen below, dialog_BG at 75% and dialog_win settling in (scale
                // (1 + (1 - t) * 0.2)^2, alpha t), then the message (y 184 of 448) and the buttons
                self.draw_under(game, r, out);
                let t = self.dlg_t;
                self.sprite_c(out, "dialog_BG", [0.0, 0.0, W, H], None, t * 0.75, Blend::Alpha);
                self.draw_dialog_win(out, t, (1.0 + (1.0 - t) * 0.2).powi(2));
                if t >= 1.0 {
                    let msg = self.sysmes.get(c.message().0).cloned().unwrap_or_default();
                    // the thumbnail variant (sub_4B4A0(1), byte_B34AF0 + 192) moves the message up to y 160
                    self.draw_dialog_text(game, r, out, &msg, if c.thumbnail() { 160.0 * H / 448.0 } else { 443.6 }, 0.0);
                    if c.thumbnail() {
                        self.draw_dialog_thumb(game, r, out);
                    }
                    for (i, (_, a)) in items.iter().enumerate() {
                        // dialog_btn_ja frames: Yes (224,0) / glow (0,135), No (224,67) / glow (0,202), drawn at
                        // native size from the object position
                        let y = if *a == Action::Yes { 0.0 } else { 67.0 };
                        let src = if hovered(i) { [0.0, y + 135.0, 216.0, 56.0] } else { [224.0, y, 216.0, 56.0] };
                        let by = if c.thumbnail() { 261.46 * H / 448.0 } else { 600.0 };
                        let rc = [if *a == Action::Yes { 990.0 } else { 756.0 }, by, 216.0, 56.0];
                        self.sprite(out, "dialog_btn_ja", rc, Some(src));
                    }
                }
                // the button guide of the screen below stays on top of the dialog and its backdrop (Switch: the
                // LOAD GAME guide over ロードしますか？ at full brightness, its bar darker over the dimmed list)
                if !self.under
                    && let Some(&below) = self.stack.last()
                    && let Some(g) = self.guide_for(game, below)
                {
                    self.draw_guide(out, &white, g, 0.0);
                }
                return;
            }
        }
        // button guide: cGalleryGuide (sub_5920 / sub_5B58) in the gallery, sub_4A8F0 elsewhere
        let guide = self.guide_for(game, screen);
        if self.under {
            return;
        }
        if let Some(g) = guide {
            let n = out.len();
            // the gallery's and the viewer's guides are cGalleryGuides made sliding (sub_5920 a3 = 1): they sit
            // (1 - alpha) * 25 lower as they fade (sub_5B58). The viewer's shows for a while and fades
            // (sub_604C / sub_5EA4); the gallery's crossfades between guides (sub_5C4C).
            let (g, a, slide) = if matches!(screen, Screen::CgView { .. }) {
                (g, self.cg_guide_a(), true)
            } else if matches!(screen, Screen::Flowchart { gallery: true }) && screen == self.screen {
                // still the gallery's guide, over the chart and apart from its fade
                (self.gal_guide.idx, self.gal_guide.alpha(), true)
            } else if Self::gal_mode(screen).is_some() && screen == self.screen {
                (self.gal_guide.idx, self.scr_a * self.gal_guide.alpha(), true)
            } else {
                (g, self.scr_a, false)
            };
            let dy = if slide { (1.0 - if matches!(screen, Screen::CgView { .. }) { a } else { self.gal_guide.alpha() }) * 25.0 } else { 0.0 };
            self.draw_guide(out, &white, g, dy);
            self.fade_quads(out, n, a);
        }
        if !self.under {
            self.draw_banner(out);
            self.draw_help(game, r, out);
        }
        // sub_4AE90: an answered dialog fading out over the screen below (no text below t 1), shrinking after
        // Yes and growing after No
        if !self.under
            && let Some(o) = self.dlg_out.filter(|o| o.hold <= 0.0)
        {
            let d = (1.0 - o.t) * 0.2;
            self.sprite_c(out, "dialog_BG", [0.0, 0.0, W, H], None, o.t * 0.75, Blend::Alpha);
            self.draw_dialog_win(out, o.t, (if o.yes { 1.0 - d } else { 1.0 + d }).powi(2));
        }
        // sub_48540: message popup, dialog_win with the text at y 214 of 448
        if let Some(p) = self.popup {
            self.draw_dialog_win(out, p.a, (1.0 - (1.0 - p.a) * 0.2).powi(2));
            // the text appears once the window is fully open
            if p.a >= 1.0 {
                let msg = self.sysmes.get(p.id).cloned().unwrap_or_default();
                self.draw_dialog_text(game, r, out, &msg, 515.8, 4.0);
            }
        }
        // sub_48420: the story's white (effect 27, once)
        if game.white > 0.0 {
            out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [1.0, 1.0, 1.0, game.white.min(1.0)]));
        }
        if self.black > 0.0 && !self.black_under {
            let [cr, cg, cb] = self.black_col;
            out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [cr, cg, cb, self.black.min(1.0)]));
            // sub_8D5D0 draws the NEW GAME white twice: blended at w (effect 27), then added at w / 2 (effect
            // 30), so the screen is white well before w reaches 1 (Switch: about 1 s after the press)
            if self.black_col == [1.0; 3] {
                let mut q = Quad::rect(white.clone(), [0.0, 0.0, W, H], [1.0, 1.0, 1.0, self.black.min(1.0) * 0.5]);
                q.blend = Blend::Add;
                out.push(q);
            }
        }
    }
}
