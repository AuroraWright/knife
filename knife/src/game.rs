//! The game: script interpreter, stage, message page, choices and frame rendering.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

mod choice;
mod flow_jump;
mod records;
mod story_movie;

pub use choice::{ChoiceKind, ChoiceOpt, DIALOG_BUTTON_Y, DIALOG_BUTTONS};
use choice::Choice;
use story_movie::StoryMovie;

use knife_core::vm::parse::{Command, Op, Scene};
use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, RenderTarget, Renderer, Shade, TexRef};

use crate::{
    assets::Assets,
    motion::MotionTable,
    sound::Sound,
    stage::{Layer, Source, Stage, Tween},
    text::{self, FontAtlas, Layout, PlacedChar},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wait {
    None,
    /// `click`: a click ends the wait early
    Time { left: f32, click: bool },
    Click,
    Transition,
    Layer(i32),
    Choice,
    Voice,
    Movie,
    Halt,
}

struct Transition {
    dur: f32,
    t: f32,
    rule: Option<TexRef>,
}

#[derive(Default)]
pub struct Page {
    pub chars: Vec<PlacedChar>,
    /// raw texts of the messages on the page (for saves)
    texts: Vec<String>,
    pen: (f32, f32),
    order: usize,
    revealed: f32,
    visible: bool,
    /// window alpha (off_AE0558): towards 1 while shown, 128 ms either way (sub_38D40, speed 16.667/128 per
    /// frame from sub_2E170(128) at a scene load); `quick` (CLO3, byte_AFEB74) hides it in 64 ms
    alpha: f32,
    quick: bool,
    /// 1 - the content fade off_AE05B0 (CLO4 / CLO5 / a message start) and its change per ms (off_AE05A8 is
    /// the per-frame step): the text draws at alpha * 2 * max(content - 0.5, 0), the page icon at
    /// alpha * content; the window background only follows `alpha`
    dim: f32,
    dim_vel: f32,
    /// the page waits for a click before more text (shown as an icon)
    waiting: bool,
    /// per message: first index into `chars`, whether its text ends in @k, frames since it was superseded
    msgs: Vec<(usize, bool, f32)>,
    /// SELD options listed after the page's messages (history entries of type 12, shown in the backlog)
    choice_texts: Vec<String>,
}

/// Message text (sub_391A0): text slots at 0.76 of the 60 px base glyph (50 in ab_glyph's scale), 464 units
/// wide. Page mode (F822 == 1, set by START_SCRIPT, every normal scene) draws from x 88 units (v278; 30 when
/// sub_2DF20, non-Japanese) with line n's box top at word_136BAB2 (65 units) + n * pitch (the dword_AFECCC
/// table); the pitch is slot 19's measured line height, (60 px font line + 10 spacing) * 0.76 in 720p units =
/// 79.8 px here (the Switch measures 80.4). Full-width glyphs advance `size` px (1000-unit advances), 49.6 on
/// the Switch. Verified against choice.mov (page glyphs from x 264, glyph tops 169 + n * 80.5).
pub const LAYOUT: Layout = Layout {
    left: 88.0 * 3.0,
    right: (88.0 + 464.0) * 3.0,
    size: 49.75,
    line_height: (60.0 + 10.0) * 0.76 * 1.5,
    ruby_size: 22.0,
};
/// F822 != 1 (LABEL_135): the message alone (no earlier history entries) from x 169.58 units, line 0's box
/// at 43.141 units (trainscene.mov: glyph tops y 116, first glyph x 562 after the indent).
const WINDOW_LAYOUT: Layout = Layout { left: 169.58 * 3.0, right: (169.58 + 464.0) * 3.0, ..LAYOUT };
/// Glyph ascent below a line box top (the box at 43.141 units = 104 px has its glyph tops at y 116 on the
/// Switch, baseline 155.3).
const ASCENT: f32 = 155.3 - 43.141 * UY;
/// Baseline of the first line: line box 0 at 65 units.
const TOP_BASELINE: f32 = 65.0 * UY + ASCENT;
const WINDOW_TOP_BASELINE: f32 = 43.141 * UY + ASCENT;
/// 640x448 units -> canvas pixels.
const UX: f32 = CANVAS_W as f32 / 640.0;
const UY: f32 = CANVAS_H as f32 / 448.0;
/// Message window types (stage word F1105, sub_391A0): image slot and origin in 1280x720 units
/// (unk_34D94C). Types 0..2 are the full screen windows (DISP txtwindow01..03), 3..32 the QA corner bubbles
/// (DISP 39..68, the "b" textures for even types).
const WINDOWS: [(u16, f32, f32); 33] = [
    (60, 0.0, 0.0), (61, 0.0, 0.0), (62, 0.0, 0.0),
    (91, 525.0, 414.0), (92, 525.0, 414.0), (93, 328.0, 393.0), (94, 328.0, 393.0), (95, 90.0, 388.0), (96, 90.0, 388.0),
    (97, 525.0, 44.0), (98, 525.0, 44.0), (99, 328.0, 34.0), (100, 328.0, 34.0), (101, 90.0, 13.0), (102, 90.0, 13.0),
    (103, 94.0, 414.0), (104, 94.0, 414.0), (105, 79.0, 393.0), (106, 79.0, 393.0), (107, 63.0, 388.0), (108, 63.0, 388.0),
    (109, 94.0, 44.0), (110, 94.0, 44.0), (111, 79.0, 34.0), (112, 79.0, 34.0), (113, 63.0, 13.0), (114, 63.0, 13.0),
    (115, 338.0, 229.0), (116, 338.0, 229.0), (117, 196.0, 210.0), (118, 196.0, 210.0), (119, 71.0, 198.0), (120, 71.0, 198.0),
];
/// Text origin of window types 3..32 relative to the window, in 1280x720 units (unk_34DAD8).
const BUBBLE_TEXT: [(f32, f32); 15] = [
    (98.0, 80.0), (100.0, 114.0), (100.0, 138.0), (98.0, 67.0), (100.0, 84.0), (100.0, 89.0), (79.0, 80.0),
    (90.0, 114.0), (113.0, 138.0), (79.0, 67.0), (90.0, 84.0), (113.0, 89.0), (98.0, 67.0), (100.0, 64.0), (100.0, 89.0),
];
/// Text width of window types 3..32 in 640-wide units (flt_34DEFC, by type - 3; others 464).
const BUBBLE_WIDTH: [f32; 3] = [299.83, 405.57, 509.77];
/// Window offsets in 1280x720 units: x by F1106 (flt_34D84C), y by F1107 (flt_34D8B8); out of range = 0.
const WINDOW_DX: [f32; 27] = [
    0.0, 100.0, 120.0, 150.0, 20.0, 200.0, 250.0, 30.0, 40.0, 50.0, 500.0, 60.0, 70.0, 80.0, -100.0, -120.0, -150.0,
    -200.0, -250.0, -30.0, -300.0, -40.0, -50.0, -60.0, -70.0, -80.0, 400.0,
];
const WINDOW_DY: [f32; 37] = [
    0.0, 10.0, 100.0, 15.0, 150.0, 20.0, 200.0, 220.0, 230.0, 240.0, 25.0, 250.0, 270.0, 30.0, 300.0, 35.0, 50.0, 65.0,
    -10.0, -100.0, -15.0, -150.0, -170.0, -180.0, -190.0, -20.0, -200.0, -210.0, -220.0, -230.0, -30.0, -300.0, -40.0,
    -45.0, -50.0, -55.0, -60.0,
];
/// Image slots 52.. are DISP_PARTS_JA in order.
const DISP_SLOT0: u16 = 52;
/// sub_38EC0 / the bubble cursor draw icons at 0.36 x 0.45 units per texture pixel.
const ICON_SCALE: (f32, f32) = (0.36 * UX, 0.45 * UY);
/// System sounds (sub_710: dword_3341A0[n] = sound 0x10040D4 + n): 0 cursor move, 1 confirm, 2 refusal / back,
/// 6 PRESS ANY BUTTON / NEW GAME, 7 phase title movies.
const SYS_SOUNDS: [&str; 9] = [
    "WSD_MOON_SE00001",
    "WSD_MOON_SE00002",
    "WSD_MOON_SE00003",
    "WSD_MOON_SE00004",
    "WSD_MOON_SE00007",
    "WSD_MOON_SE00010",
    "WSD_MOON_SE00011",
    "WSD_MOON_SE00013",
    "WSD_MOON_SE00014",
];
/// sub_CA0's volume curve (flt_3355F0, 100 points over 0..1, linear in between; 1.0 from 0.99).
const VOLUME_CURVE: [f32; 100] = [
    0.0, 0.001442, 0.002885, 0.004327, 0.005769, 0.007212, 0.008654, 0.010096, 0.011538, 0.016667, 0.021795,
    0.026923, 0.030769, 0.034615, 0.038462, 0.042308, 0.046154, 0.05, 0.055769, 0.061538, 0.069231, 0.076923,
    0.082692, 0.088462, 0.094231, 0.1, 0.105769, 0.111538, 0.119231, 0.126923, 0.132692, 0.138462, 0.15, 0.155769,
    0.161538, 0.176923, 0.182692, 0.188462, 0.2, 0.205769, 0.211538, 0.226923, 0.238462, 0.244231, 0.25, 0.261538,
    0.276923, 0.288462, 0.294231, 0.3, 0.311538, 0.326923, 0.338462, 0.35, 0.361538, 0.376923, 0.382692, 0.388462,
    0.4, 0.411538, 0.426923, 0.438462, 0.45, 0.461538, 0.476923, 0.488462, 0.5, 0.511538, 0.526923, 0.538462, 0.55,
    0.576923, 0.588462, 0.6, 0.611538, 0.625, 0.638462, 0.65, 0.661538, 0.676923, 0.688462, 0.711538, 0.725,
    0.738462, 0.75, 0.776923, 0.788462, 0.8, 0.811538, 0.826923, 0.85, 0.863462, 0.876923, 0.9, 0.913462, 0.926923,
    0.938462, 0.961538, 0.976923, 0.988462,
];

/// sub_CA0: below 0.0014423 as is; else 100v < 1 -> 0, the table interpolated up to 98, 1.0 above.
fn volume_curve(v: f32) -> f32 {
    if v < 0.0014423 {
        return v;
    }
    let x = v * 100.0;
    let i = x as usize;
    match i {
        0 => 0.0,
        1..=98 => VOLUME_CURVE[i] + (x - i as f32) * (VOLUME_CURVE[i + 1] - VOLUME_CURVE[i]),
        _ => 1.0,
    }
}
/// Voice name prefix -> speaker (sub_2360, table off_44D970).
const VOICE_SPEAKERS: [(&str, usize); 55] = [
    ("ZZZ", 0), ("A00", 1), ("A10", 2), ("A11", 2), ("A12", 2), ("A20", 3), ("A21", 3), ("A22", 3), ("A23", 3),
    ("A24", 3), ("B00", 4), ("B10", 5), ("B20", 6), ("B30", 7), ("B40", 8), ("B50", 9), ("B60", 10), ("B70", 11),
    ("B80", 12), ("B90", 13), ("C00", 14), ("C10", 15), ("C20", 16), ("C30", 17), ("C40", 18), ("C50", 19),
    ("C60", 20), ("C70", 21), ("C80", 0), ("C90", 22), ("D00", 0), ("D10", 0), ("D20", 0), ("D30", 0), ("D40", 0),
    ("D50", 0), ("D60", 0), ("D70", 0), ("D80", 0), ("D90", 0), ("D91", 0), ("D92", 0), ("M00", 23), ("M01", 23),
    ("M02", 23), ("M03", 23), ("M04", 0), ("M05", 0), ("M06", 0), ("M07", 0), ("M08", 0), ("M09", 0), ("M10", 0),
    ("M11", 23), ("M12", 23),
];
/// Speaker -> config voice row bit (off_AE04D8, row = log2).
const SPEAKER_ROW_BIT: [u32; 24] = [
    262144, 1, 2, 4, 512, 65536, 16, 32, 8, 128, 4096, 32768, 131072, 1, 1024, 8192, 16384, 2048, 64, 262144, 262144,
    256, 262144, 262144,
];
/// Speaker of a voice file (sub_2360: its first three letters; 0 when unlisted).
fn voice_speaker(name: &str) -> usize {
    name.get(..3).map(str::to_ascii_uppercase).and_then(|p| VOICE_SPEAKERS.iter().find(|s| s.0 == p)).map_or(0, |s| s.1)
}

/// Config voice row of a voice file's speaker (sub_29390).
fn voice_row(name: &str) -> usize {
    SPEAKER_ROW_BIT[voice_speaker(name)].trailing_zeros() as usize
}

/// Config voice row -> its progress word (dword_34D4B4): bit 4 = row shown, 1 = own face (rows 15, 16),
/// 2 = own name (rows in 0x3CF36).
pub const VOICE_ROW_WORDS: [usize; 19] = [11, 12, 13, 14, 15, 16, 27, 17, 29, 18, 23, 26, 19, 24, 25, 20, 21, 22, 30];
const BOTTOM_LIMIT: f32 = 1000.0;
const PARAGRAPH_GAP: f32 = 0.0;
/// sub_391A0: earlier messages on the page fade to 100/255 over (config word 7) * 1.2 = 42 frames; a run of
/// messages ending in @k shares the fade of the message that ends it.
const READ_ALPHA: f32 = 100.0 / 255.0;
/// rewind history length (sub_814C0: 800 page records)
const HISTORY: usize = 800;
const READ_FADE_FRAMES: f32 = 42.0;
/// ms per 60 fps frame
const FRAME_MS: f32 = 1000.0 / 60.0;

/// User settings, modelled on the original config screen (CONF_PARTS; sub_2AA80).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// ms per character for unread / read text (0..250; 0 = instant)
    pub text_ms: [f32; 2],
    /// "ページ末まで一気に表示": show the rest of the page at once (unread / read)
    pub page_at_once: [bool; 2],
    /// auto mode: wait at a page end / at a click wait inside a page (ms, 0..10000)
    pub auto_page_ms: f32,
    pub auto_click_ms: f32,
    /// "既読シーンのオートスキップ" and its confirmation message
    pub auto_skip_read: bool,
    pub skip_confirm: bool,
    /// "メッセージの早送り": 全文 (also unread text) / 既読のみ
    pub skip_unread: bool,
    /// text appears with a fade (else instantly) / the page fades out (else vanishes)
    pub text_fade_in: bool,
    pub text_fade_out: bool,
    /// "すべてのボイスの再生" / "メッセージ送り時の音声停止"
    pub voice_on: bool,
    pub voice_stop_on_advance: bool,
    /// system sound effect volume (0..1) and mute bits (0 BGM, 1 SE, 2 system SE)
    pub sys_se: f32,
    pub mute: u32,
    /// per character voice volume (rows of the voice settings, 0..1) and mute bits
    pub char_voice: Vec<f32>,
    pub char_mute: u32,
}

/// The exe's initial config (0x46B080).
impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&crate::savedata::CONFIG_DEFAULTS)
    }
}

pub struct Game {
    pub assets: Assets,
    motions: MotionTable,
    pub stage: Stage,
    pub sound: Sound,
    font: FontAtlas,
    pub settings: Settings,

    pub scene_index: usize,
    /// index of the last SELD option / SELS button picked (off_AE0AA0; not saved), read by `jump_special`
    last_pick: i32,
    script: Arc<Scene>,
    pc: usize,
    pub wait: Wait,
    pub vars: HashMap<String, i32>,
    page: Page,
    sel_opts: Vec<ChoiceOpt>,
    choice: Option<Choice>,
    /// SELECT_PARTS_JA (texture ids 142..147), loaded with the first choice
    select_parts: Option<Vec<TexRef>>,
    /// choice option texts and their blur (texture 5)
    choice_text_rt: Option<RenderTarget>,
    choice_glow: Option<RenderTarget>,
    sysmes: Option<Vec<String>>,
    trans: Option<Transition>,
    snapshot: bool,
    shake: Option<(f32, f32, f32)>,
    /// MSQK message-text quake: (remaining ms, amp x px, amp y px) and the current offset
    msg_quake: Option<(f32, f32, f32)>,
    msg_offset: (f32, f32),
    pub skip: bool,
    /// skip was stopped by unread text; stays off until the skip key is released
    skip_blocked: bool,
    /// debug: skip never stops at unread text (not persisted)
    pub skip_all: bool,
    /// read keys (exe off_AE0350 + 13): highest `_ZMsssmm` sub-index read per sss; the message being shown is
    /// `read_key` and counts as read once the next one starts
    pub read: Vec<u8>,
    read_key: Option<(usize, u8)>,
    /// persistent bitmaps: flowchart nodes (RDST), endings (RDED)
    pub nodes: Vec<u64>,
    pub endings: Vec<u64>,
    /// flowchart edges taken in any playthrough (exe system words 400+), bit = flag - 564
    pub edges: Vec<u64>,
    /// images shown at least once, by seen bit (sub_774F0; `seen_bit`), for CG gallery unlocks
    pub seen: Vec<u64>,
    /// BGM numbers played at least once (music gallery, exe bit 3200+n)
    pub heard: Vec<u64>,
    /// movies watched (exe sub_92400 flags): 0 op1, 1 op2, 2 prologue, 3..5 endings 1..3
    pub movie_flags: u16,
    /// the TOMP movie task (sub_40480)
    story_movie: Option<StoryMovie>,
    /// debug: every story movie can be skipped, with A / a tap too
    pub skip_movies: bool,
    /// a gallery movie is playing while the game is paused
    pub gallery_movie: bool,
    pub auto: bool,
    /// debug: pick the first option of every choice
    pub auto_choice: bool,
    /// a system screen is open: freeze the script and animations
    pub paused: bool,
    auto_timer: f32,
    /// the message being shown was already read (selects the read text speed)
    line_read: bool,
    pub ended: bool,
    mouse: (f32, f32),
    time: f32,
    warned: HashSet<String>,
    /// raw texts of finished pages, oldest first (the backlog shows one page at a time)
    pub backlog_pages: Vec<Vec<String>>,
    /// hide the message window and text (while the backlog is shown)
    pub hide_text: bool,
    /// script / effect steps per frame (sub_3FBD0 runs its effect steps this many times): the touch
    /// gesture and mode panel skip and rewind speeds, 1 normally
    pub speed: u32,
    /// per-message rewind history (sub_814C0 walks an 800-entry page ring): snapshot, the RDST count it
    /// belongs to (for the mode panel's previous scene) and its (scene, message pc, page line) key
    history: std::collections::VecDeque<(crate::save::SaveData, u32, (usize, usize, usize))>,
    /// What a save writes (the exe's in-memory record, sub_527F0): the state as the first message of the
    /// current page started (F823 <= 1, never a choice), so a save resumes at the start of its page
    save_point: Option<crate::save::SaveData>,
    /// The stage backup block (off_AE08D0, record +6664): the stage as it was before the first stage command
    /// since the last refresh (F991 = 0), or at the last FADS, VPLY, MSAD, WTTM or `_Z` message / label that
    /// found F991 set. None until the first refresh (the current stage is written then).
    stage_backup: Option<Vec<u8>>,
    /// The newest rewind-ring entry (word_136B440 ring: messages and ATSV), which the next save point keeps
    /// as its previous entry.
    last_ring: Option<crate::save::SaveData>,
    node_seq: u32,
    /// mode panel next scene (sub_53950(1)): skip, unread text included, up to the next RDST and stop
    /// at its first message
    pub seek_node: bool,
    seek_hit: bool,
    /// voice of the message being shown (mode panel voice replay)
    last_voice: Option<String>,
    /// VPLY's voice until the next message takes it (off_AE08B0)
    pending_voice: Option<String>,
    /// the shown message's voice and speaker (sub_6E0D0: page entry +13752 / +13756, off_AE0590)
    msg_voice: Option<String>,
    speaker: i16,
    /// System menu over the game (sub_2E000 / sub_3BBA0): the scene is captured once, gaussian-blurred
    /// (sub_A0A90: sigma 2.25 taps of 1.75 px) and crossfaded in at this alpha; None when no menu is up.
    pub menu_blur: Option<f32>,
    menu_rt: Option<RenderTarget>,
    menu_captured: bool,
    movie: Option<crate::movie::Movie>,
    /// command index of the message currently shown
    msg_pc: usize,
    stuck: (usize, f32),
    /// WNTY: 0 = line-break icon, 1 = page-break icon
    wnty: i32,
    /// DISP_PARTS_JA textures in order: image slots DISP_SLOT0.. (message windows, wait icons)
    disp: Vec<TexRef>,

    stage_rt: RenderTarget,
    /// STNS render target 40, created on first use
    noise_rt: Option<RenderTarget>,
    /// Save list previews (sub_884F0 / sub_89AD0): the Switch keeps no images, it rebuilds each preview
    /// from the record's stage block. Rendered in `render` (which has the renderer mutably), by slot.
    previews: std::collections::HashMap<usize, TexRef>,
    preview_queue: Vec<(usize, Vec<Option<crate::save::LayerSave>>)>,
    preview_rt: Option<RenderTarget>,
    old_rt: RenderTarget,
    blur_rt: RenderTarget,
    /// FADE 40: blur the stage (FADE 41 clears)
    post_blur: bool,
    canvas: RenderTarget,
}

fn rgb(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 0xFF) as f32 / 255.0,
        ((c >> 8) & 0xFF) as f32 / 255.0,
        (c & 0xFF) as f32 / 255.0,
    ]
}

/// STBR mode -> blend. The layer keeps the mode byte (0 stored as -1); sub_37F50 maps it through
/// dword_34DD70 (identity for 1..39) to an effect index, and the draw (sub_B0640 -> sub_A27C0 / sub_A2790)
/// takes the blend state from dword_3C9748[i - 1] and the shader from dword_3C9698[i - 1]. Blend state 7
/// copies the framebuffer for the shader to read; multiply and screen equal fixed-function blends.
fn blend_mode(mode: i32) -> (Blend, Shade) {
    use knife_render::PsOp as P;
    let b = match mode {
        1 | 11 => Blend::Ps(P::Add),
        4 | 13 => Blend::Ps(P::Darken),
        5 => Blend::Ps(P::DodgeAlpha),
        6 | 20 => Blend::Ps(P::Lighten),
        7 | 21 => Blend::Multiply,
        8 => Blend::Ps(P::NoAlpha),
        9 | 23 => Blend::Screen,
        10 => Blend::Ps(P::Sub),
        12 => Blend::Ps(P::Burn),
        14 => Blend::Ps(P::Difference),
        15 => Blend::Ps(P::DifferenceAlpha),
        16 => Blend::Ps(P::Dodge),
        17 => Blend::Ps(P::DodgeAlpha),
        18 => Blend::Ps(P::Exclusion),
        19 => Blend::Ps(P::HardLight),
        22 => Blend::Ps(P::Overlay),
        24 => Blend::Ps(P::SoftLight),
        25 => Blend::Ps(P::LinearBurn),
        26 => Blend::Ps(P::HardLightEx),
        28 => Blend::Ps(P::AddNoAlpha),
        29 => Blend::Ps(P::SubNoAlpha),
        // 2 = default shader; 3 and 27 have no shader; 0 / -1 = none
        _ => Blend::Alpha,
    };
    (b, Shade::Sprite)
}

impl Game {
    pub fn new(assets: Assets, motions: MotionTable, font: FontAtlas, r: &Renderer) -> Self {
        let script = Arc::new(Scene::parse(b""));
        let disp = assets.ui_textures(r, "DISP_PARTS_JA").unwrap_or_default();
        let mut g = Self::new_inner(assets, motions, font, r, script);
        g.disp = disp.into_iter().map(|(_, t)| t).collect();
        g
    }

    fn new_inner(assets: Assets, motions: MotionTable, font: FontAtlas, r: &Renderer, script: Arc<Scene>) -> Self {
        Self {
            assets,
            motions,
            stage: Stage::new(),
            sound: Sound::new(),
            font,
            settings: Settings::default(),
            scene_index: 0,
            last_pick: 0,
            script,
            pc: 0,
            wait: Wait::Halt,
            vars: HashMap::new(),
            page: Page::default(),
            sel_opts: Vec::new(),
            choice: None,
            select_parts: None,
            choice_text_rt: None,
            choice_glow: None,
            sysmes: None,
            trans: None,
            snapshot: false,
            shake: None,
            msg_quake: None,
            msg_offset: (0.0, 0.0),
            skip: false,
            skip_blocked: false,
            skip_all: false,
            read: vec![0; crate::savedata::READ_KEYS],
            read_key: None,
            nodes: Vec::new(),
            endings: Vec::new(),
            edges: Vec::new(),
            seen: Vec::new(),
            heard: Vec::new(),
            movie_flags: 0,
            story_movie: None,
            skip_movies: false,
            gallery_movie: false,
            auto: false,
            auto_choice: false,
            paused: false,
            auto_timer: 0.0,
            line_read: false,
            ended: false,
            mouse: (0.0, 0.0),
            time: 0.0,
            warned: HashSet::new(),
            backlog_pages: Vec::new(),
            hide_text: false,
            speed: 1,
            history: Default::default(),
            save_point: None,
            stage_backup: None,
            last_ring: None,
            node_seq: 0,
            seek_node: false,
            seek_hit: false,
            last_voice: None,
            pending_voice: None,
            msg_voice: None,
            speaker: 0,
            menu_blur: None,
            menu_rt: None,
            menu_captured: false,
            movie: None,
            msg_pc: 0,
            stuck: (0, 0.0),
            wnty: 0,
            disp: Vec::new(),
            stage_rt: r.create_target(CANVAS_W, CANVAS_H, "stage"),
            noise_rt: None,
            previews: Default::default(),
            preview_queue: Vec::new(),
            preview_rt: None,
            old_rt: r.create_target(CANVAS_W, CANVAS_H, "old frame"),
            blur_rt: r.create_target(CANVAS_W, CANVAS_H, "blur"),
            post_blur: false,
            canvas: r.create_target(CANVAS_W, CANVAS_H, "canvas"),
        }
    }

    // ---- script control -------------------------------------------------------

    pub fn start_scene(&mut self, index: usize, label: Option<&str>) {
        let Some(script) = self.assets.scene(index) else {
            tracing::error!("scene {index} missing");
            self.ended = true;
            return;
        };
        tracing::info!("scene {index} {} label {label:?}", self.assets.scene_name(index));
        // debugging: `@N` starts at command index N, `@NAME(a,b)` 8 commands before the
        // first matching command
        let pc = label
            .and_then(|l| match l.strip_prefix('@') {
                Some(n) => n.parse().ok().or_else(|| {
                    script
                        .commands
                        .iter()
                        .position(|c| format!("{}({})", c.name, c.args.join(",")).starts_with(n))
                        .map(|p| p.saturating_sub(8))
                }),
                None => script.label(l),
            })
            .unwrap_or(0);
        self.scene_index = index;
        self.script = script;
        self.pc = pc;
        self.wait = Wait::None;
        self.ended = false;
    }

    pub fn current_pc(&self) -> usize {
        self.pc
    }

    pub fn var(&self, name: &str) -> i32 {
        self.vars.get(&var_key(name)).copied().unwrap_or(0)
    }

    fn value(&self, s: Option<&str>) -> i32 {
        match s {
            Some(v) if v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') => self.var(v),
            Some(v) => knife_core::vm::parse::eval_int(v).unwrap_or(0),
            None => 0,
        }
    }

    pub fn set_var_pub(&mut self, name: &str, v: i32) {
        self.set_var(name, v);
    }

    fn set_var(&mut self, name: &str, v: i32) {
        self.vars.insert(var_key(name), v);
    }

    fn goto_label(&mut self, label: &str) {
        match self.script.label(label) {
            Some(pc) => self.pc = pc,
            None => tracing::warn!("label {label} not found in scene {}", self.scene_index),
        }
    }

    fn warn_once(&mut self, what: &str) {
        if self.warned.insert(what.to_string()) {
            tracing::info!("unimplemented: {what}");
        }
    }

    /// Hold-to-skip input; skipping stays blocked after hitting unread text until released.
    pub fn set_skip(&mut self, held: bool) {
        if !held {
            self.skip_blocked = false;
        }
        self.skip = held && !self.skip_blocked;
    }

    /// A `_ZMsssmm` message starts (script_dispatch 0x78070 / 0x78894): the previous message becomes read
    /// (its key's highest sub-index), this one is read when its sub-index is at most the stored one
    /// (sub_53A60); skip stops at unread text.
    fn note_read(&mut self, name: &str) {
        if let Some((i, s)) = self.read_key
            && let Some(r) = self.read.get_mut(i)
        {
            *r = (*r).max(s);
        }
        self.read_key = read_key(name);
        let Some((i, s)) = self.read_key else {
            self.line_read = false;
            return;
        };
        let was = self.read.get(i).is_some_and(|&r| r != 0 && s <= r);
        self.line_read = was;
        if !was && self.skip && !self.settings.skip_unread && !self.skip_all {
            tracing::info!("skip stopped at unread message {name}");
            self.skip = false;
            self.skip_blocked = true;
        }
    }

    fn resolve_text(&self, s: &str) -> String {
        // `$NNNNNN` references a script_text line, anywhere in the text (the QA scenes join two with `^`, a
        // line break: `$042766^$042767`); scripts have no other `$`
        let mut out = String::new();
        let mut rest = s;
        while let Some(at) = rest.find('$') {
            out.push_str(&rest[..at]);
            let tail = &rest[at + 1..];
            let n = tail.bytes().take_while(u8::is_ascii_digit).count();
            match tail[..n].parse::<usize>() {
                Ok(i) => out.push_str(&self.assets.line(i)),
                Err(_) => out.push('$'),
            }
            rest = &tail[n..];
        }
        out.push_str(rest);
        out
    }

    // ---- input ----------------------------------------------------------------

    /// Primary action (click / enter / space).
    pub fn advance(&mut self) {
        if self.choice.is_some() {
            self.choice_confirm();
            return;
        }
        if self.wait == Wait::Movie {
            if self.skip_movies {
                self.skip_movie();
            }
            return;
        }
        if let Wait::Time { click: true, .. } = self.wait {
            self.wait = Wait::None;
            return;
        }
        match self.wait {
            Wait::Click => {
                if (self.page.revealed as usize) < self.page.order {
                    self.page.revealed = self.page.order as f32;
                } else {
                    self.finish_click();
                }
            }
            Wait::Transition => {}
            _ => {}
        }
    }

    fn finish_click(&mut self) {
        // sub_40ED0 LABEL_307 (leaving a click wait; sub_2C500 too) resets the 11 per-message colour words
        for i in 0..11 {
            self.set_var(&format!("F{}", 800 + i), Self::TEXT_COLOUR >> 8);
            self.set_var(&format!("F{}", 811 + i), Self::TEXT_COLOUR & 0xFF);
        }
        self.last_voice = None;
        self.page.waiting = false;
        self.wait = Wait::None;
        self.auto_timer = 0.0;
        if self.settings.voice_stop_on_advance {
            self.sound.stop_voice();
        }
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        self.choice_hover_at(x, y);
    }

    // ---- per-frame update ---------------------------------------------------------

    pub fn update(&mut self, r: &Renderer, dt: f32) {
        for _ in 0..self.speed.max(1) {
            self.update_step(r, dt);
        }
    }

    fn update_step(&mut self, r: &Renderer, dt: f32) {
        self.time += dt;
        self.sound.update();
        if self.paused {
            if let Some(m) = &mut self.movie
                && (m.looping || self.gallery_movie)
            {
                m.update(r, dt);
                if m.closed {
                    self.stop_movie();
                }
            }
            return;
        }
        let fast = self.skip || self.seek_node;
        let step = if fast { dt * 8.0 } else { dt };

        // layer tweens and motions
        self.stage.advance(step);
        for l in self.stage.layers.iter_mut().flatten() {
            if let Some(m) = &mut l.motion {
                let f = self.motions.advance(m, step);
                if let Some(x) = f.x {
                    l.x = Tween::fixed(x);
                }
                if let Some(y) = f.y {
                    l.y = Tween::fixed(y);
                }
                if let Some(a) = f.alpha {
                    l.alpha = Tween::fixed(a);
                }
                if let Some(z) = f.zoom_x {
                    l.zoom_x = Tween::fixed(z);
                }
                if let Some(z) = f.zoom_y {
                    l.zoom_y = Tween::fixed(z);
                }
                if let Some(rot) = f.rot {
                    l.rotation = Tween::fixed(rot);
                }
                if let Some(p) = f.prio {
                    l.prio = p;
                }
                if f.clip.iter().any(|c| c.is_some()) {
                    let mut c = l.clip.unwrap_or_default();
                    if let Some(v) = f.clip[0] {
                        c.x = v;
                    }
                    if let Some(v) = f.clip[1] {
                        c.y = v;
                    }
                    if let Some(v) = f.clip[2] {
                        c.w = v;
                    }
                    if let Some(v) = f.clip[3] {
                        c.h = v;
                    }
                    if let Some(v) = f.clip[4] {
                        c.rot = v / 10.0;
                    }
                    l.clip = Some(c);
                }
            }
        }
        // message quake: jitter that alternates direction every frame
        if let Some((left, ax, ay)) = &mut self.msg_quake {
            *left -= step;
            if *left <= 0.0 || fast {
                self.msg_quake = None;
                self.msg_offset = (0.0, 0.0);
            } else {
                let flip = if (self.time / 33.0) as u32 % 2 == 0 { 1.0 } else { -1.0 };
                self.msg_offset = (*ax * flip, *ay * -flip);
            }
        }
        // screen shake
        if let Some((amp, period, left)) = &mut self.shake {
            *left -= step;
            if *left <= 0.0 {
                self.shake = None;
                self.stage.shake = (0.0, 0.0);
            } else {
                let ph = (*left / period.max(1.0)) * std::f32::consts::PI;
                self.stage.shake = (0.0, ph.sin() * *amp);
            }
        }
        // transition
        if let Some(t) = &mut self.trans {
            t.t += step;
            if t.t >= t.dur || fast {
                self.trans = None;
            }
        }
        // earlier messages fade towards READ_ALPHA (the trailing run of @k messages counts as current)
        let mut i = self.page.msgs.len();
        while i > 0 && (i == self.page.msgs.len() || self.page.msgs[i - 1].1) {
            i -= 1;
        }
        for m in &mut self.page.msgs[..i] {
            m.2 = (m.2 + dt * 0.06).min(READ_FADE_FRAMES);
        }
        // window fade / text reveal
        let target = if self.page.visible { 1.0 } else { 0.0 };
        let instant = fast || (target < self.page.alpha && !self.settings.text_fade_out);
        let wstep = if self.page.quick && !self.page.visible { 0.26042 * dt / FRAME_MS } else { dt / 128.0 };
        self.page.alpha = if instant {
            target
        } else if target > self.page.alpha {
            (self.page.alpha + wstep).min(target)
        } else {
            (self.page.alpha - wstep).max(target)
        };
        if self.page.alpha <= 0.0 {
            self.page.quick = false;
        }
        if self.page.dim_vel != 0.0 {
            self.page.dim += self.page.dim_vel * if fast { 1.0e9 } else { dt };
            if !(0.0..=1.0).contains(&self.page.dim) {
                self.page.dim = self.page.dim.clamp(0.0, 1.0);
                self.page.dim_vel = 0.0;
            }
        }
        let k = usize::from(self.line_read);
        let ms = self.settings.text_ms[k];
        if ms <= 0.0 || self.settings.page_at_once[k] || fast {
            self.page.revealed = self.page.order as f32;
        } else {
            self.page.revealed = (self.page.revealed + dt / ms).min(self.page.order as f32);
        }
        self.update_choice(dt);

        // movie
        if let Some(m) = &mut self.movie {
            m.update(r, dt);
            if self.skip_all && self.wait == Wait::Movie {
                self.stop_movie();
            } else if self.story_movie.is_some() {
                self.update_story_movie(dt);
            } else if self.movie.as_ref().is_some_and(|m| m.closed) {
                self.stop_movie();
            }
        }

        // resolve waits
        match self.wait {
            Wait::Movie => {
                if self.movie.is_none() {
                    self.wait = Wait::None;
                }
            }
            Wait::Time { left, click } => {
                let left = left - step;
                self.wait = if left <= 0.0 || fast { Wait::None } else { Wait::Time { left, click } };
            }
            Wait::Transition => {
                if self.trans.is_none() {
                    self.wait = Wait::None;
                }
            }
            Wait::Layer(l) => {
                if fast {
                    for layer in self.stage.layers.iter_mut().flatten() {
                        layer.finish();
                    }
                }
                if !self.stage.animating(l) || fast {
                    self.wait = Wait::None;
                }
            }
            Wait::Voice => {
                if !self.sound.voice_playing() || fast {
                    self.wait = Wait::None;
                }
            }
            Wait::Click => {
                let revealed = self.page.revealed as usize >= self.page.order;
                if fast && revealed {
                    if self.seek_node && self.seek_hit {
                        self.seek_node = false;
                    } else {
                        self.finish_click();
                    }
                } else if self.auto && revealed && !self.sound.voice_playing() {
                    self.auto_timer += dt;
                    // a click wait right before a page clear is a page end
                    let page_end = self.script.commands.get(self.pc).is_some_and(|c| c.name == "PGST");
                    let limit = if page_end { self.settings.auto_page_ms } else { self.settings.auto_click_ms };
                    if self.auto_timer >= limit {
                        self.finish_click();
                    }
                }
            }
            Wait::Choice => self.seek_node = false,
            _ => {}
        }

        // debug aid: report long waits while skipping
        if fast {
            if self.pc == self.stuck.0 {
                self.stuck.1 += dt;
                if self.stuck.1 > 5000.0 && self.stuck.1 - dt <= 5000.0 {
                    tracing::warn!(
                        "stuck at scene {} pc {} wait {:?}: {:?}",
                        self.scene_index,
                        self.pc,
                        self.wait,
                        self.script.commands.get(self.pc.saturating_sub(1)).map(|c| (&c.name, &c.args))
                    );
                }
            } else {
                self.stuck = (self.pc, 0.0);
            }
        }

        // sub_391A0, the message layer's draw callback (sub_3B9A0: layer 15), set F824 = 65 last frame
        self.set_var("F824", 65);

        // run the script
        let mut budget = 20000;
        while self.wait == Wait::None && !self.ended && budget > 0 {
            budget -= 1;
            let Some(cmd) = self.script.commands.get(self.pc).cloned() else {
                tracing::info!("scene {} ran off the end", self.scene_index);
                self.ended = true;
                break;
            };
            self.pc += 1;
            self.exec(r, &cmd);
        }
        self.record_history();
        self.assets.trim_images(400);
    }

    /// Remember each new message for rewinding.
    fn record_history(&mut self) {
        if self.wait != Wait::Click {
            return;
        }
        let key = (self.scene_index, self.msg_pc, self.page.texts.len());
        if self.history.back().is_some_and(|h| h.2 == key) {
            return;
        }
        if let Some(s) = self.snapshot() {
            if self.var("F823") <= 1 {
                // the save point keeps the newest rewind entry before it (0x79028: sub_527F0 gets ring[idx]
                // before this message is pushed)
                self.save_point = Some(crate::save::SaveData { prev: self.last_ring.clone().map(Box::new), ..s.clone() });
            }
            self.last_ring = Some(s.clone());
            if self.history.len() >= HISTORY {
                self.history.pop_front();
            }
            self.history.push_back((s, self.node_seq, key));
        }
    }

    pub fn can_rewind(&self) -> bool {
        self.history.len() >= 2
    }

    /// The current message is in place and its effects have finished (the rewind takes its next step).
    pub fn settled(&self) -> bool {
        self.wait == Wait::Click && self.trans.is_none() && self.page.alpha == if self.page.visible { 1.0 } else { 0.0 }
    }

    /// Back one message (sub_814C0).
    pub fn rewind_step(&mut self, r: &Renderer) -> bool {
        if !self.can_rewind() {
            return false;
        }
        self.history.pop_back();
        self.rewind_to(r, self.history.len() - 1);
        true
    }

    /// Mode panel previous scene (sub_53950(-1) -> sub_81A50): the first message of the current RDST
    /// node, or of the node before when already there.
    pub fn prev_scene(&mut self, r: &Renderer) -> bool {
        let n = self.history.len();
        if n < 2 {
            return false;
        }
        let first = |tag: u32| self.history.iter().position(|h| h.1 == tag).unwrap_or(0);
        let mut target = first(self.history[n - 1].1);
        if target == n - 1 {
            if target == 0 {
                return false;
            }
            target = first(self.history[target - 1].1);
        }
        self.history.truncate(target + 1);
        self.rewind_to(r, target);
        true
    }

    fn rewind_to(&mut self, r: &Renderer, i: usize) {
        let (d, tag, _) = self.history[i].clone();
        self.node_seq = tag;
        self.seek_node = false;
        self.restore_with(r, &d, true);
        // the ring still holds the entries before this one
        let prev = i.checked_sub(1).and_then(|j| self.history.get(j)).map(|h| Box::new(h.0.clone()));
        self.last_ring = prev.as_deref().cloned();
        if let Some(sp) = self.save_point.as_mut().filter(|sp| sp.scene == d.scene && sp.pc == d.pc) {
            sp.prev = prev;
        }
    }

    /// Mode panel next scene.
    pub fn next_scene(&mut self) {
        self.seek_node = true;
        self.seek_hit = false;
    }

    /// Mode panel voice replay (case 7: the current message's voice).
    pub fn replay_voice(&mut self) {
        if let Some(n) = self.last_voice.clone() {
            self.play_test_voice(&n, voice_row(&n));
        }
    }

    // ---- commands ---------------------------------------------------------------

    fn exec(&mut self, r: &Renderer, c: &Command) {
        match &c.op {
            Op::Marker => {
                // every `_Z` command takes the read-key step of a message (0x78070: the previous key is merged,
                // this one becomes current, skip stops when it is unread); `_ZY` labels also refresh the
                // backup (0x78efc)
                self.note_read(&c.name);
                if c.name.starts_with("ZY") {
                    self.refresh_backup();
                }
                return;
            }
            Op::Message => {
                self.refresh_backup();
                // sub_6E0D0: the message takes VPLY's voice and speaker (0 without one)
                self.msg_voice = self.pending_voice.take();
                self.speaker = self.msg_voice.as_deref().map_or(0, |n| voice_speaker(n) as i16);
                // sub_6E0D0: unless the content is fading in, a page shown in full keeps it; otherwise (after
                // CLO4, or F1103 set) it starts over from 0 and fades in over 18 frames. F1103 ends here.
                if self.page.dim_vel >= 0.0 {
                    if self.page.dim <= 0.0 && self.var("F1103") == 0 {
                        self.page.dim_vel = 0.0;
                    } else {
                        self.page.dim = 1.0;
                        self.page.dim_vel = -1.0 / (18.0 * FRAME_MS);
                    }
                }
                self.set_var("F1103", 0);
                let raw = c.args.first().map(String::as_str).unwrap_or("");
                self.note_read(&c.name);
                let text = self.resolve_text(raw);
                // sub_391A0 draws the last F823 - 1 history messages above this one: the page starts over at
                // a message with F823 <= 1 (the scripts count F823 up before each message, zero it at a page end)
                // (only in page mode, F822 == 1; otherwise every message has the window to itself)
                if self.var("F823") <= 1 || !self.page_mode() {
                    self.clear_page();
                }
                // sub_6E0D0: in page mode message n = min(F823, 11) of the page records its colour and the F31 /
                // F32 text scale in F799+n / F810+n / F824+n / F835+n
                if self.page_mode() {
                    let n = self.var("F823").clamp(0, 11);
                    let (f31, f32) = (self.var("F031"), self.var("F032"));
                    for (base, v) in [(799, Self::TEXT_COLOUR >> 8), (810, Self::TEXT_COLOUR & 0xFF), (824, f31), (835, f32)] {
                        self.set_var(&format!("F{}", base + n), v);
                    }
                }
                self.show_message(&text, true);
                return;
            }
            Op::Cmd(_) => {}
        }
        let name = c.name.as_str();
        self.note_stage_change(c);
        match name {
            // variables and flow
            "WKST" => {
                let v = self.value(c.arg(2));
                if let Some(var) = c.arg(0) {
                    self.set_var(var, v);
                }
            }
            "WKAD" => {
                if let Some(var) = c.arg(0) {
                    let v = self.var(var) + self.value(c.arg(1));
                    self.set_var(var, v);
                }
            }
            "WKOR" => {
                if let Some(var) = c.arg(0) {
                    let v = self.var(var) | self.value(c.arg(1));
                    self.set_var(var, v);
                }
            }
            "IF__" => {
                let a = self.value(c.arg(0));
                let b = self.value(c.arg(2));
                let cond = match c.arg(1).unwrap_or("eq") {
                    "eq" => a == b,
                    "ne" => a != b,
                    "gt" => a > b,
                    "lt" => a < b,
                    "ge" => a >= b,
                    "le" => a <= b,
                    "an" => a & b != 0,
                    op => {
                        tracing::warn!("IF__ op {op}");
                        false
                    }
                };
                if !cond && let Some(l) = c.arg(3) {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            "IFNX" | "IFP4" | "IFSC" | "IFUR" => {}
            "GOTO" => {
                if let Some(l) = c.arg(0) {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            "GOTS" => {
                let v = self.value(c.arg(0));
                if let Some(l) = c.args.get(1 + v.max(0) as usize).filter(|s| !s.is_empty()) {
                    let l = l.clone();
                    self.goto_label(&l);
                } else {
                    tracing::warn!("GOTS: no label for {v}");
                }
            }
            "JUMP" => {
                let id = c.int_or(0, -1);
                match self.assets.scene_by_id(id) {
                    Some(idx) => {
                        let label = c.arg(1).map(str::to_string);
                        self.jump_marks(self.scene_index, id);
                        self.start_scene(idx, label.as_deref());
                    }
                    None => {
                        tracing::error!("JUMP to unknown scene id {id}");
                        self.ended = true;
                    }
                }
            }
            "END_" | "RTM_" | "RTMN" | "RTM2" => {
                tracing::info!("{name}: scene finished");
                self.ended = true;
            }
            "WTTM" => {
                let ms = c.int_or(0, 0) as f32;
                if ms > 0.0 {
                    self.wait = Wait::Time {
                        left: ms,
                        click: c.int_or(1, 0) == 0,
                    };
                }
            }

            // messages
            "PGST" => {
                // the page id is kept in F1122 (script_dispatch 0x78cd0, nothing else: the page text stays, e.g.
                // above a choice); a non-1010 load restarts there
                self.set_var("F1122", c.int_or(0, 0));
            }
            // window commands (script_dispatch 0x7f3c4, `F1103` = text not drawn until the next message)
            "CLO2" => {
                // the messages go (sub_53AB0(1)), the window is off at once
                self.clear_page();
                self.page.visible = false;
                self.page.alpha = 0.0;
                self.set_var("F1103", 1);
            }
            "CLO3" => {
                // a shown window fades out in 64 ms; the page stays (drawn again from the next message)
                if self.page.visible {
                    self.page.visible = false;
                    self.page.quick = true;
                    self.set_var("F1103", 1);
                }
            }
            "CLO4" => {
                // fade the content (text, icon) out over `ms`; at once when skipping or ms <= 0
                let ms = c.int_or(0, 0) as f32;
                let p = &mut self.page;
                if self.skip || ms <= 0.0 || p.dim_vel > 0.0 {
                    p.dim = 1.0;
                    p.dim_vel = 0.0;
                } else {
                    if p.dim_vel < 0.0 {
                        p.dim = 0.0;
                    }
                    p.dim_vel = 1.0 / ms;
                }
            }
            "CLO5" => {
                // the window is shown at full alpha at once and the content fades in over `ms`
                let ms = c.int_or(0, 0) as f32;
                let f1103 = self.var("F1103") != 0;
                let p = &mut self.page;
                if self.skip || ms <= 0.0 || (p.dim_vel <= 0.0 && p.dim < 1.0 && !f1103) {
                    p.dim = 0.0;
                    p.dim_vel = 0.0;
                } else {
                    p.dim = 1.0;
                    p.dim_vel = -1.0 / ms;
                }
                p.visible = true;
                p.alpha = 1.0;
                p.quick = false;
                self.set_var("F1103", 0);
            }
            "WNTY" => {
                // sub_38EA0 also keeps whether it is a page break in F799 (read when a saved message is replayed)
                self.wnty = c.int_or(0, 0);
                self.set_var("F799", (self.wnty != 0).into());
            }
            "MSAD" => {
                if let Some(a) = c.args.first() {
                    let text = self.resolve_text(a);
                    let wait = text.contains("@k");
                    self.show_message(&text, wait);
                    if !wait {
                        self.wait = Wait::None;
                    }
                }
            }

            // choices
            "SELR" => {
                let value = c.int_or(0, self.sel_opts.len() as i32);
                let text = self.resolve_text(c.arg(1).unwrap_or(""));
                let disabled = c.int_or(2, 0) == 1;
                self.cmd_selr(value, &text, disabled);
            }
            "SELD" | "SELA" | "SELS" => {
                let var = c.arg(0).unwrap_or("_D000").to_string();
                self.cmd_select(name, var);
            }

            // stage
            "STCH" | "SCH2" | "STCG" => self.cmd_stch(r, c),
            "STCC" => {
                // sub_62140: no argument clears layer 0, a layer above 39 nothing, a negative one all of them
                let l = c.int_or(0, 0);
                if l <= 39 {
                    let range = if l < 0 { 0..crate::stage::LAYERS } else { l as usize..l as usize + 1 };
                    for i in range {
                        self.stage.kept[i] = Some(self.kept_of(i));
                    }
                    self.stage.clear(l);
                }
            }
            "SACL" => {
                let on = c.int_or(1, 0) != 0;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.absolute = on;
                }
            }
            "SBAS" => {
                let b = c.int_or(1, -1);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.base = usize::try_from(b).ok().filter(|&b| b < crate::stage::LAYERS);
                }
            }
            "STCP" => {
                let p = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.prio = p;
                }
            }
            "STZ4" => {
                let ms = c.int_or(5, 0) as f32;
                let ease = c.int_or(6, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    let (zx0, zy0) = (c.int(1), c.int(2));
                    let (zx1, zy1) = (c.int(3).or(zx0), c.int(4).or(zy0));
                    if let Some(z) = zx1 {
                        let from = zx0.map_or(l.zoom_x.value(), |v| v as f32 / 1000.0);
                        l.zoom_x = Tween::new(from, z as f32 / 1000.0, ms, ease);
                    }
                    if let Some(z) = zy1 {
                        let from = zy0.map_or(l.zoom_y.value(), |v| v as f32 / 1000.0);
                        l.zoom_y = Tween::new(from, z as f32 / 1000.0, ms, ease);
                    }
                }
            }
            // STCL(rel, layer, ...): position tween that stops the layer's motion.
            //   rel == 0: (x0, y0, x1, y1, ms, ease)   rel != 0: (dx, dy, ms, ease) relative to current
            "STCL" => {
                let rel = c.int_or(0, 0) != 0;
                if let Some(l) = self.stage.get(c.int_or(1, -1)) {
                    l.motion = None;
                    let (cx, cy) = (l.x.value(), l.y.value());
                    let (from, to, ms, ease) = if rel {
                        let to = (cx + c.int_or(2, 0) as f32, cy + c.int_or(3, 0) as f32);
                        ((cx, cy), to, c.int_or(4, 0), c.int_or(5, 0))
                    } else {
                        let from = (c.int(2).map_or(cx, |v| v as f32), c.int(3).map_or(cy, |v| v as f32));
                        let to = (c.int(4).map_or(from.0, |v| v as f32), c.int(5).map_or(from.1, |v| v as f32));
                        (from, to, c.int_or(6, 0), c.int_or(7, 0))
                    };
                    l.x = Tween::new(from.0, to.0, ms as f32, ease);
                    l.y = Tween::new(from.1, to.1, ms as f32, ease);
                }
            }
            "STRT" => {
                let ms = c.int_or(3, 0) as f32;
                let ease = c.int_or(4, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    let from = c.int(1).map_or(l.rotation.value(), |v| v as f32 / 100.0);
                    let to = c.int(2).map_or(from, |v| v as f32 / 100.0);
                    l.rotation = Tween::new(from, to, ms, ease);
                }
            }
            "STGS" => {
                let ms = c.int_or(3, 0) as f32;
                let ease = c.int_or(4, 0);
                let layer = c.int_or(0, -1);
                if let Some(l) = self.stage.get(layer) {
                    let from = c.int(1).map_or(l.alpha.value(), |v| v as f32 / 255.0);
                    let to = c.int(2).map_or(from, |v| v as f32 / 255.0);
                    l.alpha = Tween::new(from, to, ms, ease);
                }
            }
            "STPV" => {
                let ms = c.int_or(3, 0) as f32;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    if let Some(x) = c.int(1) {
                        l.x.retarget(x as f32, ms, 0);
                    }
                    if let Some(y) = c.int(2) {
                        l.y.retarget(y as f32, ms, 0);
                    }
                }
            }
            "STCF" => {
                let l = c.int_or(0, -1);
                if self.stage.animating(l) {
                    self.wait = Wait::Layer(l);
                }
            }
            "STEC" => {
                // sub_3DE0: 0 resets, 1 fills with the colour, 2..=82 select a colour grade that the layer
                // texture is built with (sub_A3180, shader pre_alter); every mode but 1 resets the fill colour
                let mode = c.int_or(1, 0);
                let color = c.int(2).map(|v| rgb(v as u32));
                if let Some(l) = self.stage.get(c.int_or(0, -1))
                    && (0..=82).contains(&mode)
                {
                    let grade = if mode >= 2 { mode } else { 0 };
                    match mode {
                        1 => l.fill = Some(color.unwrap_or([0.0; 3])),
                        0 => {
                            l.fill = None;
                            l.shade = Shade::Sprite;
                        }
                        _ => l.fill = None,
                    }
                    if grade != l.grade {
                        l.grade = grade;
                        if let Source::Image(_) = l.source
                            && let Some(img) = self.assets.image_graded(r, &l.name, grade)
                        {
                            l.source = Source::Image(img);
                        }
                    }
                }
            }
            "STBR" => {
                let mode = c.int_or(1, 0);
                let (blend, _) = blend_mode(mode);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.blend = blend;
                    l.stbr = mode;
                }
            }
            // MSQK(ms, ay, ax): shake the message text (exe stores ay*448/720 and ax*640/1280, 640x448 units)
            "MSQK" => {
                let ms = c.int_or(0, 1000).clamp(0, 10000) as f32;
                let ay = c.int_or(1, 0) as f32 * 448.0 / 720.0 * CANVAS_H as f32 / 448.0;
                let ax = c.int_or(2, 0) as f32 * 640.0 / 1280.0 * CANVAS_W as f32 / 640.0;
                self.msg_quake = (ms > 0.0).then_some((ms, ax, ay));
            }
            "SPMA" => {
                let pause = c.int_or(0, 0) == 1;
                for l in self.stage.layers.iter_mut().flatten() {
                    if let Some(m) = &mut l.motion {
                        m.paused = pause;
                    }
                }
            }
            "SGMA" => {
                let id = c.int_or(1, 0);
                let key = c.int_or(2, 0).max(0) as usize;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    use crate::motion::field;
                    let mut v = vec![
                        (field::X, l.x.value().round() as i32),
                        (field::Y, l.y.value().round() as i32),
                        (field::ALPHA, (l.alpha.value() * 255.0).round() as i32),
                        (field::ROT, (l.rotation.value() * 100.0).round() as i32),
                        (field::ZOOM_X, (l.zoom_x.value() * 1000.0).round() as i32),
                        (field::ZOOM_Y, (l.zoom_y.value() * 1000.0).round() as i32),
                    ];
                    if let Some(cr) = l.clip.filter(|cr| cr.w != 0.0 || cr.h != 0.0) {
                        v.extend([(11, cr.x as i32), (12, cr.y as i32), (13, cr.w as i32), (14, cr.h as i32)]);
                    }
                    self.motions.patch_key(id, key, &v);
                }
            }
            "STMA" => {
                let id = c.int_or(1, 0);
                let start = c.int_or(3, 0) as f32;
                let exists = self.motions.exists(id);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    if id > 0 && exists {
                        let mut m = crate::motion::MotionState::new(id, start);
                        // apply the first frame immediately
                        let f = self.motions.advance(&mut m, 0.0);
                        if let Some(x) = f.x {
                            l.x = Tween::fixed(x);
                        }
                        if let Some(y) = f.y {
                            l.y = Tween::fixed(y);
                        }
                        if let Some(a) = f.alpha {
                            l.alpha = Tween::fixed(a);
                        }
                        if let Some(z) = f.zoom_x {
                            l.zoom_x = Tween::fixed(z);
                        }
                        if let Some(z) = f.zoom_y {
                            l.zoom_y = Tween::fixed(z);
                        }
                        l.motion = Some(m);
                    } else {
                        l.motion = None;
                    }
                }
            }
            "FADS" => {
                let mode = c.int_or(0, 1);
                let dur = c.int_or(1, 0) as f32;
                let rule = c.int_or(2, 0);
                self.stage.commit();
                // sub_60610 copies the committed stage into the backup
                self.stage_backup = Some(self.stage_bytes());
                if mode == 1 || dur <= 0.0 || self.skip {
                    self.trans = None;
                } else {
                    let rule = (rule > 0)
                        .then(|| self.assets.image(r, &format!("RULE_{rule:02}")))
                        .flatten()
                        .map(|i| i.tex.clone());
                    self.snapshot = true;
                    self.trans = Some(Transition { dur, t: 0.0, rule });
                    self.wait = Wait::Transition;
                }
            }
            "SSW2" => {
                // (layer, amp_min=7, amp_max=15, period_min_ms=1000, period_max_ms=2000,
                //  strength=1, dur_ms=-1, fade)
                let li = c.int_or(0, -1);
                let dur = c.int_or(6, -1);
                let amp = (c.int_or(1, 7) as f32, c.int_or(2, 15) as f32);
                let period = (c.int_or(3, 1000) as f32, c.int_or(4, 2000) as f32);
                let strength = c.int_or(5, 1) as f32;
                if let Some(l) = self.stage.get(li) {
                    l.sway = (dur != 0 && strength != 0.0).then(|| {
                        let seed = (li as u32 + 1).wrapping_mul(2654435761) ^ self.pc as u32;
                        crate::stage::Sway::new(amp, period, strength, if dur < 0 { -1.0 } else { dur as f32 }, seed)
                    });
                }
            }
            // SPFC(layer, image, snx2, snx3, va, vb, stec, color): effect/texture precache
            "SPFC" => {
                if let Some(name) = c.args.get(1).filter(|n| !n.is_empty()) {
                    let _ = self.assets.image(r, name);
                }
            }
            // SPRD(layer, image, ?, ?, va, vb): precache of a variant image
            "SPRD" => {
                if let Some(name) = c.args.get(1).filter(|n| !n.is_empty()) {
                    let name = match (c.int(4), c.int(5)) {
                        (Some(a), Some(b)) if (a, b) != (0, 0) => format!("{name}_{a:02}_{b:02}"),
                        _ => name.clone(),
                    };
                    let _ = self.assets.image(r, &name);
                }
            }
            "STRA" => {}
            // known, intentionally unimplemented effects (see re/NOTES.md): SOXY scissor inset,
            // STNS grain overlay, STTR per-layer rule fade, FADE post filters, FCLN/FCSP screen
            // effects, SPRC render-target release
            // SOXY: scripts always pass (layer,20,20,20,20); the scissor inset needs arg4 in 1..6 and
            // no fragment shader reads the per-draw value it sets (checked by disassembly), so no effect;
            // FCLN never appears in the scripts, so FCSP (clear) has nothing to undo
            "SOXY" | "FCLN" | "FCSP" | "SPRC" => {}
            // XSTP/XRSM: start/resume an engine system task (sub_3F600/sub_3F9B0) around the staff roll
            "XSTP" | "XRSM" => {}
            // FADE(40, ..): blur the whole stage; FADE(41): off. Other modes are engine states.
            "FADE" => match c.int_or(0, 0) {
                40 => self.post_blur = true,
                41 => self.post_blur = false,
                _ => {}
            },
            // STNS(layer, n): grain overlay (always on white img2345 layers); 0 clears
            "STNS" => {
                self.load_noise(r);
                let n = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.noise = (n > 0).then_some(n);
                }
            }
            // STTR(layer, rule): following alpha fades wipe through RULE_nn; -1/0 clears
            "STTR" => {
                let n = c.int_or(1, -1);
                let tex = (n > 0)
                    .then(|| self.assets.image(r, &format!("RULE_{n:02}")))
                    .flatten()
                    .map(|i| (n, i.tex.clone()));
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.rule = tex;
                }
            } // per-layer effect seed for the sway/raster animation; no visible effect alone
            "SQK2" => {
                let li = c.int_or(0, -1);
                let ax = c.int_or(1, 15) as f32;
                let ay = c.int_or(2, 15) as f32;
                let period = c.int_or(3, 0).max(0) as f32 + 16.0;
                let dur = c.int_or(4, -1);
                let q = (ax != 0.0 || ay != 0.0) && dur != 0;
                let quake = q.then_some(crate::stage::Quake {
                    ax,
                    ay,
                    period,
                    left: if dur < 0 { -1.0e10 } else { dur as f32 },
                    t: 0.0,
                    args: [c.int_or(3, i32::MIN), dur, c.int_or(5, 0), c.int_or(6, 0), c.int_or(7, 0)],
                });
                let apply = |l: &mut Layer| {
                    l.quake = quake;
                    if quake.is_none() {
                        l.offset = (0.0, 0.0);
                    }
                };
                if li < 0 {
                    self.stage.layers.iter_mut().flatten().for_each(apply);
                } else if let Some(l) = self.stage.get(li) {
                    apply(l);
                }
            }
            "SNX2" => {
                let v = c.int_or(1, 0) as f32;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.bright = 1.0 + v / 255.0;
                }
            }
            "STRR" => {
                let v = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.motion_rot = if v == -99999 { 0.0 } else { (v % 36000) as f32 / 100.0 };
                }
            }
            "SNX4" | "FANX" | "STQK" | "TRPY" | "SNX3" => {}
            "SHOK" | "SHK2" => {
                let amp = c.int_or(1, 3) as f32;
                let period = c.int_or(2, 100) as f32;
                let count = c.int_or(3, 5).max(1) as f32;
                if !self.skip {
                    self.shake = Some((amp, period, period * count));
                }
            }

            // audio
            "SEPL" => {
                let ch = c.int_or(0, 0);
                let looping = c.int_or(1, 0) != 0;
                if let Some(n) = c.arg(2) {
                    let vol = c.keyed_int(17).unwrap_or(100) as f32 / 100.0;
                    let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                    if let Some(pcm) = self.assets.sound_effect(n) {
                        if !(self.skip && !looping) {
                            self.sound.play_se(ch, n, pcm, vol, fade, looping);
                        }
                    }
                }
            }
            "SEFD" | "SESP" => {
                let ch = c.int_or(0, -1);
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                self.sound.stop_se(ch, fade);
            }
            "SEVL" => {
                let ch = c.int_or(0, -1);
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                if let Some(v) = c.keyed_int(17) {
                    self.sound.set_se_volume(ch, v as f32 / 100.0, fade);
                }
            }
            "MPLY" => {
                let n = c.int_or(0, 0);
                let vol = c.keyed_int(17).unwrap_or(100) as f32 / 100.0;
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                // only MPLY marks a BGM heard (sub_649E0, n <= 0x417); the title and movie BGM do not
                if (1..=0x417).contains(&n) {
                    set_bit(&mut self.heard, n as usize);
                }
                self.play_bgm(n, vol, fade);
            }
            "MFAD" => {
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                self.sound.stop_bgm(fade);
            }
            "MVOL" => {
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                if let Some(v) = c.keyed_int(17) {
                    self.sound.set_bgm_volume(v as f32 / 100.0, fade);
                }
            }
            "VPLY" | "VPL2" => {
                if let Some(n) = c.arg(0) {
                    self.note_voice(n);
                    self.pending_voice = Some(n.to_string());
                    self.play_voice(n);
                }
            }
            "VCWT" => {
                if self.sound.voice_playing() {
                    self.wait = Wait::Voice;
                }
            }
            "TOMP" => {
                let n = c.int_or(0, -1);
                // sub_40480 / sub_40A00: these movies set the flags the movie / music galleries check once they
                // end, and can be skipped only when already watched
                let flag = [(5, 0), (6, 1), (4, 2), (35, 3), (36, 4), (37, 5)].iter().find(|m| m.0 == n).map(|m| m.1);
                match crate::movie::MOVIES.get(n.max(0) as usize).filter(|_| n >= 0) {
                    Some(name) if !self.skip_all => {
                        self.play_movie(r, name);
                        if self.movie.is_some() {
                            let skippable = flag.is_none_or(|f| self.movie_flags & (1 << f) != 0);
                            self.story_movie = Some(StoryMovie::new(n, flag, skippable));
                        }
                    }
                    Some(_) => {}
                    None => tracing::warn!("TOMP {n}: unknown movie"),
                }
            }
            "TOMW" => {
                if self.movie.is_some() {
                    self.wait = Wait::Movie;
                }
            }
            // bookkeeping commands with no visible effect here
            "RDST" => {
                self.node_seq += 1;
                self.seek_hit |= self.seek_node;
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    set_bit(&mut self.nodes, n as usize);
                }
            }
            "RTST" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    self.route_enter(n);
                }
            }
            "RDED" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    set_bit(&mut self.endings, n as usize);
                }
            }
            "ATSV" => {
                // 0x7a888: a type 6 page-ring entry at the command (also the save point when F823 <= 1, and the
                // newest rewind-ring entry a following save keeps; knife's rewind does not stop at it)
                let mut s = self.snapshot_at(self.pc - 1, Vec::new(), None);
                s.kind = 6;
                s.voice = None;
                s.preview.clear();
                if self.var("F823") <= 1 {
                    self.save_point = Some(crate::save::SaveData { prev: self.last_ring.clone().map(Box::new), ..s.clone() });
                }
                self.last_ring = Some(s);
            }
            "RDAS" | "SHAF" | "SQS2" | "SHS2" | "SSS2" | "NOP_" | "WTSV" => {}
            other => self.warn_once(other),
        }
    }

    pub fn play_movie(&mut self, r: &Renderer, name: &str) {
        let path = self.assets.romfs.join(format!("movie/{name}.mp4"));
        match crate::movie::Movie::open(r, &path, false) {
            Ok(m) => {
                if let Some(pcm) = crate::movie::decode_audio(&path) {
                    self.sound.play_movie_audio(Arc::new(pcm));
                }
                self.movie = Some(m);
            }
            Err(e) => tracing::warn!("movie {name}: {e:#}"),
        }
    }

    /// Title stage G002, stepped by both title init (sub_8D2E0 -> sub_8D1C0) and teardown (sub_90180):
    /// 0 -> 1 once node 560 is read, 1 -> 3 once 383 is, 3 -> 5 once 562 is, one step per call; other values
    /// reset to 0. During the PRESS ANY BUTTON boot (byte_AC0DE0) it is reset to 0 instead.
    pub fn title_stage_step(&mut self, boot: bool) {
        let read = |n| get_bit(&self.nodes, n);
        let stage = match self.var("G002") {
            _ if boot => 0,
            0 if read(560) => 1,
            1 if read(383) => 3,
            3 if read(562) => 5,
            s @ (0 | 1 | 3 | 5) => s,
            _ => 0,
        };
        self.set_var("G002", stage);
    }

    /// Title BGM (sub_8E200 state 0, sub_9C470: full volume at once): 72 at title stage 5, else 2.
    pub fn play_title_bgm(&mut self) {
        self.play_bgm(if self.var("G002") == 5 { 72 } else { 2 }, 1.0, 0.0);
    }

    /// The title background number for the stage: 4 at stage 5, else dword_3529E4[stage].
    pub fn title_bg(&self) -> usize {
        match self.var("G002") {
            5 => 4,
            s => [1, 2, 3, 3, 2][s.clamp(0, 4) as usize],
        }
    }

    /// Title movie titlebg`n` (sub_17C0(n - 1, 0, 1)), n = `title_bg`.
    pub fn play_title_movie(&mut self, r: &Renderer) {
        let n = self.title_bg();
        let path = self.assets.romfs.join(format!("movie/titlebg{n}.mp4"));
        match crate::movie::Movie::open(r, &path, true) {
            // sub_8E200 state 1: sub_1C20(flt_34DCD0[stage] / 30), 240 frames for every stage
            Ok(mut m) => {
                m.loop_start = 240.0 / 30.0;
                self.movie = Some(m);
            }
            Err(e) => tracing::warn!("title movie: {e:#}"),
        }
    }

    /// Pause / resume the current movie (the title background under a submenu).
    pub fn hold_movie(&mut self, hold: bool) {
        if let Some(m) = &mut self.movie {
            m.hold = hold;
        }
    }

    pub fn movie_active(&self) -> bool {
        self.movie.is_some()
    }

    /// sub_ACEF0(n): system sound n on the system SE volume (config, linear x 0.01).
    pub fn play_sys_sound(&mut self, n: usize) {
        tracing::debug!("system sound {n}");
        let vol = if self.settings.mute & 4 != 0 { 0.0 } else { self.settings.sys_se };
        if let Some(pcm) = SYS_SOUNDS.get(n).and_then(|name| self.assets.sound_effect(name)) {
            self.sound.play_sys(0, pcm, vol);
        }
    }

    /// sub_ACFA0(n): system sound n on the SE volume through the sub_CA0 curve.
    fn play_se_sound(&mut self, n: usize) {
        let vol = volume_curve(self.sound.se_volume());
        if let Some(pcm) = SYS_SOUNDS.get(n).and_then(|name| self.assets.sound_effect(name)) {
            self.sound.play_sys(1, pcm, vol);
        }
    }

    /// sub_1A30 for the current movie.
    pub fn movie_ready(&self) -> bool {
        self.movie.as_ref().is_some_and(|m| m.ready())
    }

    pub fn movie_playing(&self) -> bool {
        self.movie.as_ref().is_some_and(|m| m.has_frame())
    }

    pub fn font_mut(&mut self) -> &mut FontAtlas {
        &mut self.font
    }

    pub fn stop_movie(&mut self) {
        if let Some(mut m) = self.movie.take() {
            m.stop();
        }
        self.sound.stop_movie_audio();
        if let Some(f) = self.story_movie.take().and_then(|t| t.flag()) {
            self.movie_flags |= 1 << f;
        }
    }

    /// sub_1A80: stops the movie but keeps its last frame (the story movie task still draws over it).
    fn stop_movie_playback(&mut self) {
        if let Some(m) = &mut self.movie {
            m.stop();
        }
        self.sound.stop_movie_audio();
    }

    /// A story movie is playing (TOMW waits for it).
    pub fn in_movie(&self) -> bool {
        self.wait == Wait::Movie
    }

    /// B / Plus during a story movie (sub_40480 mask 0x4008): fades a skippable one out.
    pub fn skip_movie(&mut self) {
        let force = self.skip_movies;
        if let Some(t) = &mut self.story_movie {
            t.press(force);
        }
    }

    pub fn play_bgm(&mut self, n: i32, vol: f32, fade: f64) {
        if n <= 0 {
            self.sound.stop_bgm(fade);
            return;
        }
        match self.assets.sar.stream_sound(n as usize - 1) {
            Ok(s) => {
                let path = self.assets.romfs.join(&s.path);
                let base = s.volume;
                let loop_range = s.loop_range;
                self.sound.play_bgm(n, vol, fade, move || {
                    let data = std::fs::read(&path)?;
                    let mut pcm = knife_audio::codec::decode_nopus(&data)?;
                    let len = pcm.frames.len();
                    pcm.loop_range = Some(loop_range.map_or((0, len), |(a, b)| (a.min(len), b.min(len))));
                    Ok((pcm, base))
                });
            }
            Err(e) => tracing::warn!("MPLY {n}: {e:#}"),
        }
    }

    /// The engine's stage bookkeeping words, before a command runs:
    /// - F991: the stage changed since the backup was refreshed. The stage commands that call sub_5F3B0 (or
    ///   inline it) first copy the stage into the backup when it is 0 and set it; FADS (sub_60610 at the
    ///   start of its wait) copies the stage after committing; VPLY (0x78140), MSAD (0x7f454), WTTM (0x7f4ac)
    ///   and the `_Z` message / label path (0x78efc) copy it and clear F991 when it is set.
    /// - F996: the stage changed since the last FADS (set by the stage commands once their layer passes their
    ///   guard, cleared by sub_60610). Nothing reads it.
    /// - F997: a layer was created or cleared (STCH 0x7b278, STCC, STNS), cleared by FADS; only the load replay
    ///   (sub_802C0) and FADS's wait (with F985, which nothing sets) read it.
    /// All three are in the saved F subset.
    fn note_stage_change(&mut self, c: &Command) {
        let arg = |i: usize| c.int(i);
        // signed `> 39` guards accept -1 (all layers); unsigned ones only 0..=39; absent args pass
        let signed = |i: usize| arg(i).is_none_or(|l| l <= 39);
        let unsigned = |i: usize| arg(i).is_none_or(|l| (0..=39).contains(&l));
        let name = c.name.as_str();
        match name {
            "FADS" => {
                for f in ["F996", "F997", "F985"] {
                    self.set_var(f, 0);
                }
                // sub_60610: arg 3 = 1 hides the text until the next message; 2 too when the fade lasts 2+ ms
                // and no content fade runs (config byte 99, 1 unless a whole-config copy changes it)
                let wnd = arg(3).unwrap_or(-1);
                if wnd == 1 || (wnd == 2 && arg(1).unwrap_or(0) >= 2 && self.page.dim_vel == 0.0) {
                    self.set_var("F1103", 1);
                }
                return;
            }
            // sub_99620: the same switch in arg 2, without the duration
            "STMA" if matches!(arg(2), Some(1)) || (arg(2) == Some(2) && self.page.dim_vel == 0.0) => {
                self.set_var("F1103", 1);
            }
            "VPLY" | "MSAD" | "WTTM" => {
                self.refresh_backup();
                return;
            }
            _ => {}
        }
        // cmd_STCH_impl fails only for a layer outside 0..=39 (absent = -1); it checks before the backup, as
        // STZ4 does; the others back up first
        let stch_ok = arg(0).is_some_and(|l| (0..=39).contains(&l));
        let backs_up = match name {
            "STCH" | "SCH2" | "SOXY" | "STTR" => stch_ok,
            "STZ4" => signed(0),
            "STCC" | "STBR" | "STCL" | "STCP" | "STRT" | "STRC" | "STRR" | "STGS" | "STRA" | "STNS" | "FCSP" | "FCLN"
            | "FLNT" => true,
            _ => false,
        };
        if backs_up && self.var("F991") == 0 {
            self.stage_backup = Some(self.stage_bytes());
            self.set_var("F991", 1);
        }
        let changed = match name {
            "STCH" => stch_ok,
            "STZ4" | "STGS" | "SQK2" | "SQS2" | "SHK2" | "SHS2" | "SSW2" | "SSS2" => signed(0),
            "STCL" => signed(1),
            "STRR" | "STMA" => unsigned(0),
            "SGMA" => unsigned(0) && arg(1).is_some_and(|m| m >= 1),
            "SPMA" | "FCSP" | "FCLN" | "FLNT" => true,
            _ => false,
        };
        if changed {
            self.set_var("F996", 1);
        }
        if match name {
            "STCH" => stch_ok,
            "STCC" => signed(0),
            "STNS" => unsigned(0),
            _ => false,
        } {
            self.set_var("F997", 1);
        }
    }

    /// VPLY / MSAD / WTTM / `_Z` commands: a stage changed since the backup becomes the backup.
    fn refresh_backup(&mut self) {
        if self.var("F991") != 0 {
            self.stage_backup = Some(self.stage_bytes());
            self.set_var("F991", 0);
        }
    }

    fn cmd_stch(&mut self, r: &Renderer, c: &Command) {
        let li = c.int_or(0, -1);
        if c.name == "STCG" {
            // STCG(image) (0x7b214) only marks the image seen: its own table index, when below 2701
            if let Some(i) = c.arg(0).and_then(|n| self.assets.image_index_of(&n.to_ascii_uppercase()))
                && i < records::SEEN_DIRECT
            {
                set_bit(&mut self.seen, i);
            }
            return;
        }
        let Some(base) = c.arg(1).map(str::to_string) else { return };
        // copy state from a source layer (SCH2 arg 18) or the layer being replaced
        let src_layer = c.int(18).filter(|&s| s >= 0);
        let prev = src_layer
            .and_then(|s| self.stage.layers.get(s as usize).cloned().flatten())
            .or_else(|| self.stage.layers.get(li.max(0) as usize).cloned().flatten());
        let variant = match (c.int(9), c.int(10)) {
            // `_00_00` is the base image itself
            (Some(a), Some(b)) if (a, b) != (0, 0) => Some(format!("{base}_{a:02}_{b:02}")),
            _ => None,
        };
        let upper = base.to_ascii_uppercase();
        let source = match upper.as_str() {
            "BLACK" => Source::Color([0.0; 3]),
            "WHITE" => Source::Color([1.0; 3]),
            _ => {
                let img = variant
                    .as_deref()
                    .and_then(|v| self.assets.image(r, v))
                    .or_else(|| self.assets.image(r, &base));
                match img {
                    Some(i) => Source::Image(i),
                    None => return,
                }
            }
        };
        let x = c.int(2).map(|v| v as f32).or(prev.as_ref().map(|p| p.x.value())).unwrap_or(240.0);
        let y = c.int(3).map(|v| v as f32).or(prev.as_ref().map(|p| p.y.value())).unwrap_or(136.0);
        let prio = c.int(6).or(prev.as_ref().map(|p| p.prio)).unwrap_or(li * 10);
        // the exe marks shown images in a persistent bitmap (sub_774F0) for gallery unlocks, BLACK and WHITE
        // (table indices 0 and 5) too
        if let Some(b) = self.seen_bit(variant.as_deref().unwrap_or(&upper)) {
            set_bit(&mut self.seen, b);
        }
        let mut l = Layer::new(variant.unwrap_or(base), source, x, y, prio);
        if let Some(p) = &prev {
            l.zoom_x = Tween::fixed(p.zoom_x.value());
            l.zoom_y = Tween::fixed(p.zoom_y.value());
            l.alpha = Tween::fixed(p.alpha.value());
            l.rotation = Tween::fixed(p.rotation.value());
            l.parent = p.parent;
            l.blend = p.blend;
            l.stbr = p.stbr;
            if src_layer.is_some() {
                l.fill = p.fill;
                l.shade = p.shade;
                if p.grade != 0
                    && let Source::Image(_) = l.source
                    && let Some(img) = self.assets.image_graded(r, &l.name, p.grade)
                {
                    l.source = Source::Image(img);
                    l.grade = p.grade;
                }
            }
        }
        if let Some(z) = c.int(4) {
            l.zoom_x = Tween::fixed(z as f32 / 1000.0);
            l.zoom_y = Tween::fixed(z as f32 / 1000.0);
        }
        if let Some(a) = c.int(5) {
            l.alpha = Tween::fixed(a as f32 / 255.0);
        }
        if let Some(p) = c.int(11) {
            l.parent = (p >= 0).then_some(p as usize);
        }
        if let (Some(x), Some(y), Some(w), Some(h)) = (c.int(12), c.int(13), c.int(14), c.int(15)) {
            l.clip = Some(crate::stage::ClipRect {
                x: x as f32,
                y: y as f32,
                w: w as f32,
                h: h as f32,
                rot: c.int_or(16, 0) as f32 / 10.0,
            });
        } else if let Some(p) = &prev
            && src_layer.is_some()
        {
            l.clip = p.clip;
        }
        if let Some(rot) = c.int(19) {
            l.rotation = Tween::fixed((rot % 36000) as f32 / 100.0);
        }
        self.stage.set(li, l);
    }

    /// sub_6E0D0: a voice plays unless voices are off or its speaker's row is muted, at that row's volume.
    fn play_voice(&mut self, n: &str) {
        if self.skip || !self.settings.voice_on {
            return;
        }
        tracing::debug!("voice {n}");
        self.last_voice = Some(n.to_string());
        let row = voice_row(n);
        if self.settings.char_mute >> row & 1 == 0 {
            let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
            match self.assets.voice(n) {
                Ok(pcm) => self.sound.play_voice_gain(Arc::new(pcm), gain),
                Err(e) => tracing::warn!("voice {n}: {e:#}"),
            }
        }
    }

    /// Voice progress (script_dispatch VPLY): the speaker (sub_2360, by the name's first three letters) gets
    /// its bit in G073, and its config voice row (sub_29300) the voice for the test button (G031 + 2 row, low
    /// / high half of the sub_23C0 id) and bit 4 of its progress word, which shows the row.
    fn note_voice(&mut self, name: &str) {
        let speaker = voice_speaker(name);
        let g = |n: usize| format!("G{n:03}");
        let heard = self.var(&g(73)) | i32::from((1u32 << speaker) as u16 as i16);
        self.set_var(&g(73), heard);
        if speaker == 23 {
            return;
        }
        let row = voice_row(name);
        if row < VOICE_ROW_WORDS.len() {
            let id = crate::voice_ids::id(name);
            self.set_var(&g(31 + 2 * row), i32::from(id as i16));
            self.set_var(&g(32 + 2 * row), i32::from((id >> 16) as i16));
            let w = g(VOICE_ROW_WORDS[row]);
            let v = self.var(&w) | 4;
            self.set_var(&w, v);
        }
    }

    /// Config voice test (sub_2B930): `name` at voice row `row`'s volume.
    pub fn play_test_voice(&mut self, name: &str, row: usize) {
        let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
        match self.assets.voice(name) {
            Ok(pcm) => self.sound.play_voice_gain(Arc::new(pcm), gain),
            Err(e) => tracing::warn!("voice {name}: {e:#}"),
        }
    }

    // ---- messages -----------------------------------------------------------------

    fn clear_page(&mut self) {
        let mut texts = std::mem::take(&mut self.page.texts);
        texts.append(&mut self.page.choice_texts);
        if !texts.is_empty() {
            self.backlog_pages.push(texts);
            if self.backlog_pages.len() > 300 {
                self.backlog_pages.drain(..100);
            }
        }
        self.page.chars.clear();
        self.page.texts.clear();
        self.page.msgs.clear();
        self.page.order = 0;
        self.page.revealed = 0.0;
        let (lay, top) = self.text_layout();
        self.page.pen = (lay.left, top);
    }

    /// Text colour of the page's messages (sub_391A0 draws history lines with it): hi / lo byte of 0xDC6D
    /// (read text) when config u16 24 is 1, else 0xFFFF. Only a whole-config copy writes that word, so it is
    /// 0 on the Switch and every colour is 0xFFFF.
    const TEXT_COLOUR: i32 = 0xFFFF;

    /// Page mode (F822 == 1, set by START_SCRIPT): a page keeps its earlier messages (sub_391A0).
    fn page_mode(&self) -> bool {
        self.var("F822") == 1
    }

    /// The message layout and its first baseline (sub_391A0: page mode, else LABEL_135). Window types 3..32
    /// put the text at the bubble's text origin (unk_34DAD8, plus the F1106 / F1107 offsets), flt_34DEFC wide.
    fn text_layout(&self) -> (Layout, f32) {
        if !self.page_mode() {
            return (WINDOW_LAYOUT, WINDOW_TOP_BASELINE);
        }
        match self.window_type() {
            Some(t @ 3..=32) => {
                let (_, wx, wy) = WINDOWS[t];
                let (tx, ty) = BUBBLE_TEXT[(t - 3) / 2];
                let (dx, dy) = self.window_offset();
                // 1280x720 units -> 640x448 -> canvas pixels
                let left = (wx + tx + dx) * 640.0 / 1280.0 * UX;
                let top = (wy + ty + dy) * 448.0 / 720.0 * UY;
                let right = left + BUBBLE_WIDTH[(t - 3) % 6 / 2] * UX;
                (Layout { left, right, ..LAYOUT }, top + ASCENT)
            }
            _ => (LAYOUT, TOP_BASELINE),
        }
    }

    /// Message window type (stage word F1105; F1109 instead only for non-Japanese text, sub_2DF20).
    fn window_type(&self) -> Option<usize> {
        usize::try_from(self.var("F1105")).ok()
    }

    /// Window offset in 1280x720 units: flt_34D84C[F1106], flt_34D8B8[F1107] (0 out of range).
    fn window_offset(&self) -> (f32, f32) {
        let at = |t: &[f32], w: &str| usize::try_from(self.var(w)).ok().and_then(|i| t.get(i)).copied().unwrap_or(0.0);
        (at(&WINDOW_DX, "F1106"), at(&WINDOW_DY, "F1107"))
    }

    fn show_message(&mut self, text: &str, wait: bool) {
        let runs = text::parse_markup(text);
        self.page.visible = true;
        // lay out on a scratch copy; restart the page if it overflows
        let (lay, top) = self.text_layout();
        let start = if self.page.chars.is_empty() {
            (lay.left, top)
        } else {
            (lay.left, self.page.pen.1 + lay.line_height + PARAGRAPH_GAP)
        };
        let mut placed = Vec::new();
        let (x, b, order) = text::layout(&self.font, &lay, &runs, start.0, start.1, self.page.order, &mut placed);
        self.msg_pc = self.pc.saturating_sub(1);
        if b > BOTTOM_LIMIT && !self.page.chars.is_empty() {
            self.clear_page();
            placed.clear();
            let (x, b, order) = text::layout(&self.font, &lay, &runs, lay.left, top, 0, &mut placed);
            self.page.pen = (x, b);
            self.page.order = order;
        } else {
            self.page.pen = (x, b);
            self.page.order = order;
        }
        self.page.msgs.push((self.page.chars.len(), text.trim_end().ends_with("@k"), 0.0));
        self.page.chars.extend(placed);
        self.page.texts.push(text.to_string());
        if wait {
            self.page.waiting = true;
            self.wait = Wait::Click;
            self.auto_timer = 0.0;
        }
    }

    // ---- rendering ------------------------------------------------------------------

    /// Pages the backlog can show: the finished pages plus the current one.
    pub fn backlog_len(&self) -> usize {
        self.backlog_pages.len() + usize::from(!self.page.texts.is_empty() || !self.page.choice_texts.is_empty())
    }

    /// Text quads of backlog page `back` (0 = the current page), laid out like the message window
    /// (sub_28470 lays the history out with the message text style at x 88 units).
    pub fn backlog_quads(&mut self, r: &Renderer, back: usize) -> Vec<Quad> {
        let cur = usize::from(!self.page.texts.is_empty() || !self.page.choice_texts.is_empty());
        let texts = if back < cur {
            self.page.texts.iter().chain(&self.page.choice_texts).cloned().collect()
        } else {
            match self.backlog_pages.len().checked_sub(back + 1 - cur) {
                Some(i) => self.backlog_pages[i].clone(),
                None => return Vec::new(),
            }
        };
        let mut placed = Vec::new();
        let (mut pen_y, mut order) = (None, 0);
        for t in &texts {
            let runs = text::parse_markup(t);
            let y = pen_y.map_or(TOP_BASELINE, |y: f32| y + LAYOUT.line_height + PARAGRAPH_GAP);
            let (_, b, o) = text::layout(&self.font, &LAYOUT, &runs, LAYOUT.left, y, order, &mut placed);
            pen_y = Some(b);
            order = o;
        }
        let mut out = Vec::new();
        for pc in &placed {
            Self::rimmed_glyph(&mut self.font, r, &mut out, pc, 1.0);
        }
        out
    }

    /// A message glyph: white with a dark rim all around.
    fn rimmed_glyph(font: &mut FontAtlas, r: &Renderer, out: &mut Vec<Quad>, pc: &text::PlacedChar, alpha: f32) {
        const RIM: f32 = 2.0;
        for (dx, dy) in [(-RIM, 0.0), (RIM, 0.0), (0.0, -RIM), (0.0, RIM), (-1.5, -1.5), (1.5, -1.5), (-1.5, 1.5), (1.5, 1.5)] {
            if let Some(q) = font.quad(r, pc.c, pc.size, pc.x + dx, pc.baseline + dy, [0.0, 0.0, 0.0, 0.55 * alpha]) {
                out.push(q);
            }
        }
        if let Some(q) = font.quad(r, pc.c, pc.size, pc.x, pc.baseline, [1.0, 1.0, 1.0, alpha]) {
            out.push(q);
        }
    }

    fn ui_quads(&mut self, r: &Renderer) -> Vec<Quad> {
        let mut out = Vec::new();
        let a = if self.hide_text { 0.0 } else { self.page.alpha };
        if a > 0.0 {
            // the window (F1104 == 0, page mode): image slot WINDOWS[F1105] at its origin + the F1106 / F1107
            // offsets + the MSQK offset, natively sized (0.333 x 0.4148 units per 1920x1080 pixel). Types 0..2
            // cover the screen at the page alpha * config word 0 (255); type 0 = txtwindow01 (alpha 102/255 in
            // the middle, falling off to the left and right edges). The bubbles draw at 0.78431 * the page alpha.
            // F822 != 1 draws image slots 10 / 2 instead (sub_B6DE0 bands of a 1920-wide atlas, not identified);
            // the Switch shows no window there (trainscene.mov)
            let (ox, oy) = self.msg_offset;
            if self.page_mode() && self.var("F1104") == 0 {
                let w = self.window_type().and_then(|t| Some((t, WINDOWS.get(t)?)));
                if let Some((t, &(slot, wx, wy))) = w
                    && let Some(tex) = self.disp.get(usize::from(slot - DISP_SLOT0))
                {
                    let (dx, dy) = self.window_offset();
                    let alpha = if t > 2 { 0.78431 } else { 1.0 } * a;
                    let size = tex.texture.size();
                    let rect = [(wx + dx) * 1.5 + ox, (wy + dy) * 1.5 + oy, size.width as f32, size.height as f32];
                    out.push(Quad::rect(tex.clone(), rect, [1.0, 1.0, 1.0, alpha]));
                }
            }
            let revealed = self.page.revealed;
            // sub_391A0: no text while F1103; else alpha * 2 * max(content - 0.5, 0)
            let content = 1.0 - self.page.dim;
            let text_a = if self.var("F1103") != 0 { 0.0 } else { a * (2.0 * (content - 0.5)).clamp(0.0, 1.0) };
            // fade of each message, resolved through runs of @k messages to the message that ends them
            let msgs = &self.page.msgs;
            let fades: Vec<f32> = (0..msgs.len())
                .map(|i| {
                    let j = (i..msgs.len()).find(|&j| !msgs[j].1).unwrap_or(msgs.len() - 1);
                    1.0 - (1.0 - READ_ALPHA) * msgs[j].2 / READ_FADE_FRAMES
                })
                .collect();
            let mut m = 0;
            for (ci, pc) in self.page.chars.iter().enumerate() {
                while m + 1 < msgs.len() && msgs[m + 1].0 <= ci {
                    m += 1;
                }
                let fade = fades.get(m).copied().unwrap_or(1.0);
                let pc = &text::PlacedChar { x: pc.x + ox, baseline: pc.baseline + oy, ..*pc };
                let mut vis = (revealed - pc.order as f32).clamp(0.0, 1.0);
                if !self.settings.text_fade_in && vis > 0.0 {
                    vis = 1.0;
                }
                if vis <= 0.0 {
                    continue;
                }
                let alpha = vis * text_a * fade;
                Self::rimmed_glyph(&mut self.font, r, &mut out, pc, alpha);
            }
            if self.wait == Wait::Click && revealed as usize >= self.page.order {
                // sub_38EC0 at the pen + (2, 3.5) units for the F799 line icon (slot 54, lineBreak_a), (2, 5.5)
                // for the page icon (slot 55, pageBreak_a, at half alpha); F822 != 1 always the page icon at
                // (2, 6.5). Window types 3..32 draw slot 56 (pageBreak_ciel_a, 59 frames of 43x46) at (4, 4).
                // 30 frames a second, 0.36 x 0.45 units per texture pixel.
                let bubble = self.page_mode() && matches!(self.window_type(), Some(3..=32));
                let page_icon = self.wnty == 1 || !self.page_mode();
                let (slot, frames, (fw, fh), alpha, (dx, dy)) = if bubble {
                    (56, 59, (43.0, 46.0), 1.0, (4.0, 4.0))
                } else if !self.page_mode() {
                    (55, 105, (39.0, 39.0), 0.5, (2.0, 6.5))
                } else if page_icon {
                    (55, 105, (39.0, 39.0), 0.5, (2.0, 5.5))
                } else {
                    (54, 105, (39.0, 39.0), 1.0, (2.0, 3.5))
                };
                // the pen's line box top, calibrated on trainscene.mov / sakihodo.mov (icon tops 204)
                let (x, y) = (self.page.pen.0 + dx * UX, self.page.pen.1 - 33.0 + (dy - 6.5) * UY);
                if let Some(t) = self.disp.get(slot - usize::from(DISP_SLOT0)) {
                    let frame = ((self.time / 33.3) as u32 % frames) as f32;
                    let tw = t.texture.size().width as f32;
                    let th = t.texture.size().height as f32;
                    let (w, h) = (fw * ICON_SCALE.0, fh * ICON_SCALE.1);
                    out.push(
                        Quad::rect(t.clone(), [x, y, w, h], [1.0, 1.0, 1.0, alpha * a * content])
                            .with_uv_rect(frame * fw / tw, 0.0, (frame + 1.0) * fw / tw, fh / th),
                    );
                }
            }
        }
        self.choice_quads(r, &mut out, a);
        out
    }

    fn load_noise(&mut self, r: &Renderer) {
        if self.stage.noise.is_none() {
            self.stage.noise = self.assets.ui_textures(r, "NOISE_PARTS").ok().and_then(|t| {
                let find = |n: &str| t.iter().find(|(name, _)| name == n).map(|(_, t)| t.clone());
                // texture ids 389/390 = NOISE_PARTS entries 2/3 (ids start at 387 in the exe's texture table)
                Some([find("noisehd0")?, find("noisehd1")?])
            });
        }
    }

    pub fn render(&mut self, r: &mut Renderer, overlay: &mut dyn FnMut(&mut Self, &Renderer, &mut Vec<Quad>)) {
        let mut enc = r.encoder();
        self.render_previews(r, &mut enc);
        if self.snapshot {
            r.copy_target(&mut enc, &self.stage_rt, &self.old_rt);
            self.snapshot = false;
        }
        if let Some(q) = self.stage.noise_pass() {
            let fresh = self.noise_rt.is_none();
            let rt = self.noise_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "noise rt40"));
            if fresh {
                // nothing earlier to show through the uncovered strip yet: start from a full-screen copy
                let full = Quad::rect(q.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]);
                r.draw(&mut enc, rt, Some([0.0, 0.0, 0.0, 1.0]), &[full]);
            }
            r.draw(&mut enc, rt, None, &[q]);
            self.stage.noise_rt = Some(rt.tex.clone());
        }
        self.render_choice_glow(r, &mut enc);
        let quads = self.stage.quads(&r.white);
        r.draw(&mut enc, &self.stage_rt, Some([0.0, 0.0, 0.0, 1.0]), &quads);
        if self.post_blur {
            // separable gaussian; FADE 40 uses sigma ~3.1 in 640-wide units -> 3 px taps
            let step = CANVAS_W as f32 / 640.0;
            r.blur(&mut enc, &self.stage_rt.tex, &self.blur_rt, [step, 0.0], 3.1);
            r.blur(&mut enc, &self.blur_rt.tex, &self.stage_rt, [0.0, step], 3.1);
        }
        match &self.trans {
            Some(t) => {
                let p = (t.t / t.dur).clamp(0.0, 1.0);
                let rule = t.rule.clone();
                r.transition(&mut enc, &self.canvas, &self.old_rt.tex, &self.stage_rt.tex, rule.as_deref(), p, 0.25, false);
            }
            None => r.copy_target(&mut enc, &self.stage_rt, &self.canvas),
        }
        match self.menu_blur {
            Some(a) => {
                let rt = self.menu_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "menu blur"));
                if !self.menu_captured {
                    // sub_A0A90(.., 2.25): sigma 2.25 taps spaced 1.75 px (of 1920)
                    let step = 1.75 * CANVAS_W as f32 / 1920.0;
                    r.blur(&mut enc, &self.canvas.tex, &self.blur_rt, [step, 0.0], 2.25);
                    r.blur(&mut enc, &self.blur_rt.tex, rt, [0.0, step], 2.25);
                    self.menu_captured = true;
                }
                let q = Quad::rect(rt.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0, 1.0, 1.0, a.min(1.0)]);
                r.draw(&mut enc, &self.canvas, None, &[q]);
            }
            None => self.menu_captured = false,
        }
        let mut ui = Vec::new();
        if let Some(m) = &self.movie
            && m.has_frame()
        {
            ui.push(Quad::rect(m.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0, 1.0, 1.0, m.alpha]));
        }
        ui.extend(self.story_movie.as_ref().and_then(|t| t.cover_quad(r)));
        ui.extend(self.ui_quads(r));
        overlay(self, r, &mut ui);
        if !ui.is_empty() {
            r.draw(&mut enc, &self.canvas, None, &ui);
        }
        r.present(enc, &self.canvas);
    }

    // ---- save / load --------------------------------------------------------------

    /// Whether the game is at a point that can be saved (a message or a choice).
    pub fn can_save(&self) -> bool {
        // a SELS / SELA question is not a resumable point (knife keeps no record kind for it)
        let choice_ok = self.choice.as_ref().is_some_and(|c| c.kind == ChoiceKind::Page);
        (self.wait == Wait::Click || (self.wait == Wait::Choice && choice_ok)) && !self.ended
    }

    /// The snapshot a save writes: the current page's save point (see `save_point`), or the current
    /// position when the page has none yet.
    pub fn save_data(&self) -> Option<crate::save::SaveData> {
        if !self.can_save() {
            return None;
        }
        match &self.save_point {
            Some(s) => Some(crate::save::SaveData { timestamp: crate::save::now(), ..s.clone() }),
            None => {
                // the entry before the current message (which the history already holds)
                let n = self.history.len();
                let prev = n.checked_sub(2).and_then(|i| self.history.get(i)).map(|h| Box::new(h.0.clone()));
                self.snapshot().map(|s| crate::save::SaveData { prev, ..s })
            }
        }
    }

    /// The committed layers as a save snapshot stores them (pending STCH layers are left out).
    pub(crate) fn layer_saves(&self) -> Vec<Option<crate::save::LayerSave>> {
        use crate::save::LayerSave;
        self
            .stage
            .layers
            .iter()
            .map(|l| {
                l.as_ref().filter(|l| !l.pending).map(|l| LayerSave {
                    image: match &l.source {
                        Source::Image(_) => Some(l.name.clone()),
                        Source::Color(_) => None,
                    },
                    color: match &l.source {
                        Source::Color(c) => Some(*c),
                        Source::Image(_) => None,
                    },
                    x: l.x.to,
                    y: l.y.to,
                    zoom_x: l.zoom_x.to,
                    zoom_y: l.zoom_y.to,
                    alpha: l.alpha.to,
                    rotation: l.rotation.to,
                    prio: l.prio,
                    parent: l.parent,
                    base: l.base,
                    absolute: l.absolute,
                    sway: l.sway.map(|w| [w.amp.0, w.amp.1, w.period.0, w.period.1, w.strength, w.left]),
                    quake: l.quake.map(|q| ([q.ax, q.ay, q.left, q.t], q.args)),
                    rule: l.rule.as_ref().map(|(n, _)| *n),
                    noise: l.noise,
                    // the restore takes the blend from `stbr` when one is set (backdrop blends always have one)
                    blend: match l.blend {
                        Blend::Alpha | Blend::Ps(_) => 0,
                        Blend::Add => 1,
                        Blend::Sub => 2,
                        Blend::Multiply => 3,
                        Blend::Screen => 4,
                        Blend::Opaque => 5,
                    },
                    shade: l.shade as u8,
                    fill: l.fill,
                    grade: l.grade,
                    motion: l.motion.as_ref().map(|m| (m.id, m.t)),
                    clip: l.clip.map(|c| [c.x, c.y, c.w, c.h, c.rot]),
                    bright: Some(l.bright),
                    stbr: Some(l.stbr),
                    motion_rot: l.motion_rot,
                })
            })
            .collect()
    }

    pub fn snapshot(&self) -> Option<crate::save::SaveData> {
        if !self.can_save() {
            return None;
        }
        let (pc, page_texts, choice) = match self.wait {
            Wait::Click => {
                let mut t = self.page.texts.clone();
                t.pop();
                (self.msg_pc, t, None)
            }
            _ => {
                let c = self.choice.as_ref()?;
                (
                    self.pc,
                    Vec::new(),
                    Some((c.var.clone(), c.opts.iter().map(|o| (o.value, o.text.clone())).collect::<Vec<_>>())),
                )
            }
        };
        Some(self.snapshot_at(pc, page_texts, choice))
    }

    /// The state as a snapshot resuming at command `pc`.
    fn snapshot_at(
        &self,
        pc: usize,
        page_texts: Vec<String>,
        choice: Option<(String, Vec<(i32, String)>)>,
    ) -> crate::save::SaveData {
        use crate::save::{SaveData, SeSave};
        // a choice entry (type 12) keeps sub_5A910's "no voice"
        let choice_entry = choice.is_some();
        let layers = self.layer_saves();
        let preview = self
            .page
            .texts
            .last()
            .map(|t| {
                text::parse_markup(t)
                    .into_iter()
                    .filter_map(|r| match r {
                        text::Run::Text(t) => Some(t),
                        text::Run::Ruby { base, .. } => Some(base),
                        text::Run::Newline => None,
                    })
                    .collect::<String>()
            })
            .or_else(|| choice.as_ref().map(|(_, o)| o.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" / ")))
            .unwrap_or_default();
        SaveData {
            version: 1,
            scene: self.scene_index,
            scene_name: self.assets.scene_name(self.scene_index).to_string(),
            pc,
            vars: self
                .vars
                .iter()
                .filter(|(k, _)| !k.starts_with('G'))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            layers,
            kept: self.stage.kept.clone(),
            backup: self.stage_backup.clone(),
            prev: None,
            page_texts,
            choice,
            bgm: self.sound.bgm_state(),
            loops: self
                .sound
                .looping_effects()
                .into_iter()
                .map(|(channel, name, volume)| SeSave { channel, name, volume })
                .collect(),
            timestamp: crate::save::now(),
            preview,
            blur: self.post_blur,
            kind: 0,
            voice: if choice_entry { None } else { self.msg_voice.clone() },
            speaker: self.speaker,
            read_key: self.read_key,
        }
    }

    /// A stage layer as a snapshot describes it (restores and save list previews).
    fn layer_from_save(&mut self, r: &Renderer, i: usize, ls: &crate::save::LayerSave) -> Option<Layer> {
        let source = match (&ls.image, ls.color) {
            (Some(name), _) => match self.assets.image_graded(r, name, ls.grade) {
                Some(img) => Source::Image(img),
                None => return None,
            },
            (None, Some(c)) => Source::Color(c),
            (None, None) => return None,
        };
        let mut l = Layer::new(ls.image.clone().unwrap_or_default(), source, ls.x, ls.y, ls.prio);
        l.zoom_x = Tween::fixed(ls.zoom_x);
        l.zoom_y = Tween::fixed(ls.zoom_y);
        l.alpha = Tween::fixed(ls.alpha);
        l.rotation = Tween::fixed(ls.rotation);
        l.parent = ls.parent;
        l.base = ls.base;
        l.absolute = ls.absolute;
        l.noise = ls.noise;
        if l.noise.is_some() {
            self.load_noise(r);
        }
        l.rule = ls
            .rule
            .and_then(|n| self.assets.image(r, &format!("RULE_{n:02}")).map(|i| (n, i.tex.clone())));
        l.sway = ls.sway.map(|[a0, a1, p0, p1, st, left]| {
            crate::stage::Sway::new((a0, a1), (p0, p1), st, left, i as u32 + 1)
        });
        l.quake = ls.quake.map(|([ax, ay, left, t], args)| crate::stage::Quake {
            ax,
            ay,
            period: args[0].max(0) as f32 + 16.0,
            left,
            t,
            args,
        });
        l.blend = match (ls.stbr, ls.blend) {
            (Some(mode), _) if mode >= 0 => blend_mode(mode).0,
            (_, 1) => Blend::Add,
            (_, 2) => Blend::Sub,
            (_, 3) => Blend::Multiply,
            (_, 4) => Blend::Screen,
            (_, 5) => Blend::Opaque,
            _ => Blend::Alpha,
        };
        l.stbr = ls.stbr.unwrap_or(-1);
        l.motion_rot = ls.motion_rot;
        l.shade = match ls.shade {
            3 => Shade::Mono,
            4 => Shade::Nega,
            _ => Shade::Sprite,
        };
        l.fill = ls.fill;
        l.grade = ls.grade;
        l.motion = ls.motion.map(|(id, t)| crate::motion::MotionState::new(id, t));
        l.clip = ls.clip.map(|[x, y, w, h, rot]| crate::stage::ClipRect { x, y, w, h, rot });
        l.bright = ls.bright.unwrap_or(1.0);
        l.pending = false;
        Some(l)
    }

    /// The preview of save slot `slot` showing `layers`, or None until `render` has drawn it.
    pub fn preview(&mut self, slot: usize, layers: &[Option<crate::save::LayerSave>]) -> Option<TexRef> {
        if let Some(t) = self.previews.get(&slot) {
            return Some(t.clone());
        }
        if !self.preview_queue.iter().any(|(s, _)| *s == slot) {
            self.preview_queue.push((slot, layers.to_vec()));
        }
        None
    }

    /// Forget the previews (of one slot, or all) after their records changed.
    pub fn clear_previews(&mut self, slot: Option<usize>) {
        match slot {
            Some(s) => {
                self.previews.remove(&s);
                self.preview_queue.retain(|(q, _)| *q != s);
            }
            None => {
                self.previews.clear();
                self.preview_queue.clear();
            }
        }
    }

    /// Draw the queued previews: the layers as a stage on black at canvas size, then halved twice.
    fn render_previews(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) {
        if self.preview_queue.is_empty() {
            return;
        }
        let rt = self.preview_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "preview")).tex.clone();
        let half = r.create_target(CANVAS_W / 2, CANVAS_H / 2, "preview half");
        for (slot, layers) in std::mem::take(&mut self.preview_queue) {
            let mut stage = Stage::new();
            for (i, ls) in layers.iter().enumerate() {
                if let Some(l) = ls.as_ref().and_then(|ls| self.layer_from_save(r, i, ls)) {
                    stage.set(i as i32, l);
                }
            }
            let quads = stage.quads(&r.white);
            let full = RenderTarget { tex: rt.clone() };
            r.draw(enc, &full, Some([0.0, 0.0, 0.0, 1.0]), &quads);
            let (w, h) = (CANVAS_W as f32, CANVAS_H as f32);
            r.draw(enc, &half, Some([0.0, 0.0, 0.0, 1.0]), &[Quad::rect(rt.clone(), [0.0, 0.0, w / 2.0, h / 2.0], [1.0; 4])]);
            let out = r.create_target(CANVAS_W / 4, CANVAS_H / 4, "preview");
            r.draw(enc, &out, Some([0.0, 0.0, 0.0, 1.0]), &[Quad::rect(half.tex.clone(), [0.0, 0.0, w / 4.0, h / 4.0], [1.0; 4])]);
            self.previews.insert(slot, out.tex);
        }
    }

    pub fn restore(&mut self, r: &Renderer, d: &crate::save::SaveData) {
        self.restore_with(r, d, false);
    }

    /// `rewind`: the BGM keeps playing when the snapshot has the same one.
    fn restore_with(&mut self, r: &Renderer, d: &crate::save::SaveData, rewind: bool) {
        self.post_blur = d.blur;
        // the in-memory record: a load copies the record in (sub_527A0); the replay (sub_5BAF0 LABEL_137)
        // refreshes it only at a page's first message (F823 <= 1, not a choice), so a rewind elsewhere keeps
        // the earlier one
        let first = d.vars.iter().find(|(k, _)| k == "F823").map_or(0, |v| v.1) <= 1 && d.choice.is_none();
        if !rewind || first {
            self.save_point = Some(d.clone());
        }
        // a load puts the record's previous entry back into the rewind ring (sub_82330 -> sub_5D950)
        if !rewind {
            self.history.clear();
            if let Some(p) = d.prev.as_deref().filter(|p| p.kind == 0) {
                self.history.push_back((p.clone(), self.node_seq, (p.scene, p.pc, usize::MAX)));
            }
            self.last_ring = d.prev.as_deref().cloned();
        }
        self.stop_movie();
        if rewind && self.sound.bgm_state().map(|b| b.0) == d.bgm.map(|b| b.0) {
            self.sound.stop_se(-1, 0.3);
            self.sound.stop_voice();
            self.sound.stop_movie_audio();
        } else {
            self.sound.stop_all();
        }
        self.choice = None;
        self.sel_opts.clear();
        self.trans = None;
        self.shake = None;
        let noise = self.stage.noise.take();
        self.stage = Stage::new();
        self.stage.noise = noise;
        for (k, v) in self.stage.kept.iter_mut().zip(&d.kept) {
            *k = *v;
        }
        self.stage_backup = d.backup.clone();
        self.vars.retain(|k, _| k.starts_with('G'));
        for (k, v) in &d.vars {
            self.vars.insert(k.clone(), *v);
        }
        for (i, ls) in d.layers.iter().enumerate() {
            let Some(ls) = ls else { continue };
            if let Some(l) = self.layer_from_save(r, i, ls) {
                self.stage.set(i as i32, l);
            }
        }
        if let Some((id, vol)) = d.bgm {
            self.play_bgm(id, vol, 0.5);
        }
        for se in &d.loops {
            if let Some(pcm) = self.assets.sound_effect(&se.name) {
                self.sound.play_se(se.channel, &se.name, pcm, se.volume, 0.5, true);
            }
        }
        // scene and page
        match self.assets.scene(d.scene) {
            Some(script) => {
                self.script = script;
                self.scene_index = d.scene;
            }
            None => {
                tracing::error!("save refers to missing scene {}", d.scene);
                self.ended = true;
                return;
            }
        }
        self.ended = false;
        self.clear_page();
        self.page.visible = false;
        self.page.alpha = 0.0;
        self.page.quick = false;
        self.page.dim = 0.0;
        self.page.dim_vel = 0.0;
        for t in &d.page_texts {
            self.show_message(t, false);
        }
        self.page.revealed = self.page.order as f32;
        for m in &mut self.page.msgs {
            m.2 = READ_FADE_FRAMES;
        }
        self.pc = d.pc;
        self.wait = Wait::None;
        // the entry's voice plays again (load: sub_5D950 0x5ee6c; rewind: sub_5BAF0 makes it pending for the
        // replayed message)
        self.pending_voice = None;
        self.msg_voice = d.voice.clone();
        self.speaker = d.speaker;
        // the entry's read key is current again (sub_5BAF0), so the next `_Z` command marks it read
        self.read_key = d.read_key;
        self.last_voice = None;
        if let Some(n) = d.voice.clone() {
            self.play_voice(&n);
        }
        if let Some((var, opts)) = &d.choice {
            let opts = opts.iter().map(|(v, t)| ChoiceOpt { value: *v, text: t.clone(), disabled: false }).collect();
            self.start_choice(Choice::new(var.clone(), ChoiceKind::Page, opts));
        }
        self.snapshot = false;
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.save_point = None;
        self.read_key = None;
        self.node_seq = 0;
        self.seek_node = false;
        self.speed = 1;
        self.post_blur = false;
        self.stop_movie();
        self.sound.stop_all();
        // sub_57450 resets every layer field but the anchor (sub_3D70 starts past it)
        let leftovers = self.reset_leftovers();
        self.stage = Stage::new();
        self.stage.kept = leftovers;
        self.stage_backup = None;
        self.last_ring = None;
        self.pending_voice = None;
        self.msg_voice = None;
        self.speaker = 0;
        self.vars.retain(|k, _| k.starts_with('G'));
        self.clear_page();
        self.page.visible = false;
        self.page.alpha = 0.0;
        self.page.quick = false;
        self.page.dim = 0.0;
        self.page.dim_vel = 0.0;
        self.choice = None;
        self.sel_opts.clear();
        self.trans = None;
        self.shake = None;
        self.wait = Wait::Halt;
        self.ended = false;
    }

    /// Flowchart jump (sub_11A58 -> sub_95800 / sub_178C8, then sub_48AC0 case 2: sub_40CD0, sub_17AF0): the
    /// current playthrough rewinds to node `n`. The play bits of every later row are cleared, chapter words
    /// by the row (sub_95800), F026 = the node's scene id and F110 = n are set, and a subset of F words is
    /// kept through the scene load that clears all F words (sub_57200); the scene starts from its top, where
    /// the scripts' `IF__(F026, ne, 0)` blocks set up the jump entry. Nothing is stored per node.
    pub fn jump_node(&mut self, n: u16) -> bool {
        let Some((row, scene_id)) = crate::flow::node_entry(n) else { return false };
        let Some(idx) = self.assets.scene_by_id(scene_id) else { return false };
        let f = |i: usize| format!("F{i:03}");
        // the rows after the target, back to the first phase 0 row: node (RTST) and edge bits off
        for (i, (phase, recs)) in crate::flow_table::ROWS.iter().enumerate().rev() {
            if i <= row || *phase < 1 {
                break;
            }
            for r in recs.iter() {
                if r[0] != 0 && (r[0].unsigned_abs() as usize) < crate::flow::NODE_BITS {
                    self.clear_play_bit('R', r[0].unsigned_abs() as usize);
                }
                if let Ok(e) = usize::try_from(r[2] - crate::flow::EDGE_BASE)
                    && e < crate::flow::EDGE_BITS
                {
                    self.clear_play_bit('E', e);
                }
            }
        }
        // sub_95800: chapter words the row has not reached yet
        for (limit, word) in JUMP_CLEARS {
            if row <= limit && !(word == 62 && row == 161) {
                self.set_var(&f(word), 0);
            }
        }
        // sub_178C8
        self.set_var("F110", i32::from(n));
        self.set_var("F026", scene_id);
        self.set_var("F1103", 1);
        for w in [823, 847, 1104, 1105, 1106, 1107, 1108, 1109, 1110, 1111, 1112, 1113] {
            self.set_var(&f(w), 0);
        }
        let words: Vec<(usize, i32)> = JUMP_KEEP_WORDS.iter().map(|&w| (w, self.var(&f(w)))).collect();
        // word_34BA16: kept as signed bytes
        let bytes: Vec<(usize, i32)> = JUMP_KEEP_BYTES.iter().map(|&w| (w, i32::from(self.var(&f(w)) as i8))).collect();
        // unk_34BC6E: kept as set / clear
        let bits: Vec<usize> = JUMP_KEEP_BITS.iter().copied().filter(|&w| self.var(&f(w)) != 0).collect();
        self.reset();
        for (w, v) in words.into_iter().chain(bytes) {
            self.set_var(&f(w), v);
        }
        for w in bits {
            self.set_var(&f(w), 1);
        }
        self.start_scene(idx, None);
        self.wait = Wait::None;
        true
    }

    pub fn dump_layers(&self) -> String {
        let mut s = String::new();
        for (i, l) in self.stage.layers.iter().enumerate() {
            if let Some(l) = l {
                s += &format!(
                    "  L{i:02} {:<24} prio {:5} xy ({:.0},{:.0}) z ({:.2},{:.2}) a {:.2} rot {:.1} {:?} {:?} fill {:?} motion {:?} pending {}{}\n",
                    l.name,
                    l.prio,
                    l.x.value(),
                    l.y.value(),
                    l.zoom_x.value(),
                    l.zoom_y.value(),
                    l.alpha.value(),
                    l.rotation.value(),
                    l.blend,
                    l.shade,
                    l.fill,
                    l.motion.as_ref().map(|m| (m.id, m.t as i32)),
                    l.pending,
                    l.sway.map_or(String::new(), |w| format!(" sway ({:.1},{:.1})", w.offset.0, w.offset.1))
                        + &l.noise.map_or(String::new(), |n| format!(" noise {n}{}", if self.stage.noise.is_some() { "" } else { " (no tex)" }))
                );
            }
        }
        s
    }

    /// RGBA8 copy of the last presented canvas.
    pub fn screenshot(&self, r: &Renderer) -> Vec<u8> {
        r.read_target(&self.canvas)
    }
}

/// sub_95800 (flowchart jump): (last row index, F word) pairs; a jump to a row up to the index zeroes the word
/// (F062 is kept at row 161 itself).
const JUMP_CLEARS: [(usize, usize); 19] = [
    (203, 69), (184, 65), (184, 66), (184, 67), (184, 68), (176, 64), (161, 63), (161, 62), (156, 61), (146, 60),
    (142, 73), (131, 72), (124, 74), (121, 59), (55, 58), (47, 57), (30, 56), (16, 55), (10, 54),
];
/// F words sub_178C8 / sub_17AF0 keep whole through a flowchart jump (F026 and F110 hold the jump itself).
const JUMP_KEEP_WORDS: [usize; 39] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 22, 991, 985, 954, 966, 965, 964, 931, 932, 933, 934, 935, 936, 937, 938, 939, 859,
    860, 861, 862, 863, 864, 865, 866, 867, 868, 869, 110, 1122, 26,
];
/// F words kept as signed bytes (word_34BA16, up to the -1).
const JUMP_KEEP_BYTES: &[usize] = &[
    298, 1100, 989, 982, 963, 962, 1101, 981, 799, 800, 801, 802, 803, 804, 805, 806, 807, 808, 809, 810, 811, 812,
    813, 814, 815, 816, 817, 818, 819, 820, 821, 823, 824, 825, 826, 827, 828, 829, 830, 831, 832, 833, 834, 835,
    836, 837, 838, 839, 840, 841, 842, 843, 844, 845, 846, 847, 848, 849, 850, 851, 852, 853, 854, 855, 856, 857,
    858, 870, 871, 872, 873, 874, 875, 876, 877, 878, 879, 880, 23, 50, 51, 52, 53, 75, 76, 77, 78, 79, 80, 81, 24,
    25, 26, 27, 31, 32, 33, 1105, 1106, 1107, 1108, 1110, 1111, 1112, 1113, 1109, 100, 101, 1098, 201, 202, 203,
    204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225,
    226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247,
    248, 249, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 267, 268, 269,
    270, 271, 272, 199, 200, 995, 600, 601, 602, 603, 604, 605, 606, 607, 608, 609, 610, 611, 612, 613, 614, 615,
    616, 617, 618, 619, 620, 621, 622, 623, 624, 625, 626, 627, 628, 629, 630, 631, 632, 633, 634, 635, 636, 637,
    638, 639, 640, 641, 642, 643, 644, 645, 646, 647, 648, 649, 650, 651, 652, 653, 654, 655, 656, 657, 658, 659,
    660, 661, 662, 663, 664, 665, 666, 667, 668, 669, 670, 671, 672, 673, 674, 675, 676, 677, 678, 679, 680, 681,
    682, 683, 684, 685, 686, 687, 1120, 1121,
];
/// F words kept as flags (unk_34BC6E, 42 entries).
const JUMP_KEEP_BITS: [usize; 42] = [
    998, 997, 996, 994, 993, 991, 992, 988, 987, 986, 984, 822, 1103, 1104, 929, 983, 975, 460, 54, 55, 56, 57, 58,
    59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 1118, 1123, 28,
];

/// Per-playthrough flowchart state lives in F words like the exe's, so saves carry it: RTST node bits
/// from F201 (sub_92340) and edge bits from F600 (sub_92380), 8 per word; `~LAST` (not saved; rebuilt from
/// the scene on load) is the last RTST node + 1.
impl Game {
    fn play_word(key: char, i: usize) -> String {
        format!("F{:03}", if key == 'R' { 201 } else { 600 } + i / 8)
    }

    fn play_bit(&self, key: char, i: usize) -> bool {
        self.var(&Self::play_word(key, i)) >> (i % 8) & 1 != 0
    }

    fn clear_play_bit(&mut self, key: char, i: usize) {
        let w = Self::play_word(key, i);
        let v = self.var(&w) & !(1 << (i % 8));
        self.set_var(&w, v);
    }

    fn set_play_bit(&mut self, key: char, i: usize) {
        let w = Self::play_word(key, i);
        let v = self.var(&w) | 1 << (i % 8);
        self.set_var(&w, v);
    }

    /// RTST(n) (sub_92770): the playthrough reaches node n, which F110 records for nodes 533..557 or while
    /// F24 is set. Edges are marked at the JUMP that leaves the scene (`jump_marks`).
    fn route_enter(&mut self, n: i32) {
        self.set_play_bit('R', n as usize);
        if (533..=557).contains(&n) || self.var("F024") != 0 {
            self.set_var("F110", n);
        }
        self.vars.insert("~LAST".into(), n + 1);
    }

    /// Last node reached in this playthrough.
    pub fn route_last(&self) -> Option<i32> {
        Some(self.var("~LAST") - 1).filter(|&n| n >= 0)
    }

    pub fn flow_flags(&self) -> crate::flow::Flags {
        use crate::flow::{EDGE_BITS, NODE_BITS};
        crate::flow::Flags {
            completed: (0..NODE_BITS).map(|i| get_bit(&self.endings, i)).collect(),
            read: (0..NODE_BITS).map(|i| get_bit(&self.nodes, i)).collect(),
            edge: (0..EDGE_BITS).map(|i| get_bit(&self.edges, i)).collect(),
            route: (0..NODE_BITS).map(|i| self.play_bit('R', i)).collect(),
            route_edge: (0..EDGE_BITS).map(|i| self.play_bit('E', i)).collect(),
        }
    }
}

/// Variable map key. The exe parses `F`, `G` and `Z` names as an array index (sub_59C60), so `G70` and
/// `G070` are the same word: those keys are normalised to three or more digits.
pub fn var_key(name: &str) -> String {
    let up = name.to_ascii_uppercase();
    match up.as_bytes() {
        [b'F' | b'G' | b'Z', rest @ ..] if !rest.is_empty() && rest.iter().all(u8::is_ascii_digit) => {
            let n: u32 = up[1..].parse().unwrap_or(0);
            format!("{}{n:03}", &up[..1])
        }
        _ => up,
    }
}

/// `_ZMsssmm` read key: sss and mm in hex (script_dispatch 0x78894).
pub fn read_key(name: &str) -> Option<(usize, u8)> {
    let d: Vec<u32> = name.get(2..7)?.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    Some(((d[0] * 256 + d[1] * 16 + d[2]) as usize, (d[3] * 16 + d[4]) as u8))
}

pub fn get_bit(bits: &[u64], i: usize) -> bool {
    bits.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 != 0)
}

/// Set bit `i`; returns whether it was already set.
pub fn set_bit(bits: &mut Vec<u64>, i: usize) -> bool {
    let (w, b) = (i / 64, i % 64);
    if bits.len() <= w {
        bits.resize(w + 1, 0);
    }
    let was = bits[w] >> b & 1 != 0;
    bits[w] |= 1 << b;
    was
}
