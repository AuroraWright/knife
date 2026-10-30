//! The game: script interpreter, stage, message page, choices and frame rendering.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

mod choice;
mod flow_jump;
mod focus;
mod shake;
mod ghost;
mod records;
mod story_movie;
mod teatime;
mod witch_log;

pub use choice::{ChoiceKind, ChoiceOpt, DIALOG_BUTTON_Y, DIALOG_BUTTONS};
use choice::Choice;
pub use teatime::TeaReq;
pub use witch_log::{LogEntry, WitchSlot};
use story_movie::StoryMovie;

use knife_core::exe::Build;
use knife_core::vm::parse::{Command, Op, Scene};
use knife_render::{Blend, BlurMode, CANVAS_H, CANVAS_W, Quad, RenderTarget, Renderer, Shade, TexRef};

use crate::{
    assets::Assets,
    motion::MotionTable,
    save::SeQueued,
    sound::Sound,
    stage::{Layer, Source, Stage, Tween, WitchQuake},
    text::{self, FontAtlas, Layout, PlacedChar},
};

/// A screen capture (`Game::freeze_screen`): still to be taken, its alpha and its fade per frame.
struct Freeze {
    capture: bool,
    alpha: f32,
    step: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wait {
    None,
    /// `click`: a click ends the wait early
    Time { left: f32, click: bool },
    Click,
    Transition,
    Layer(i32, i32),
    Choice,
    Voice,
    /// a message with an end code (`@e` / `@n`, or MSAD's): the script goes on once its text is out
    /// (sub_81B30 turns AE04B8 bit 1 into 0x40) and, with `voice` (bit 4: an `@e` message's own voice that
    /// sub_6E0D0 played), once that voice ends (sub_802C0 clears bit 4 when sub_9C400 reports none); a
    /// click or a skip ends it early. No click icon.
    Message { voice: bool },
    Movie,
    Halt,
    /// TTCT: the TeaTime menu runs instead of the script (sub_40ED0 while byte_F4763C is set)
    TeaTime,
}

struct Transition {
    dur: f32,
    t: f32,
    rule: Option<TexRef>,
    /// FADS's capture (sub_A46F0) drawn whole over the presented stage before its transition starts
    hold: bool,
}

impl Transition {
    fn hold() -> Self {
        Self { dur: 1.0, t: 0.0, rule: None, hold: true }
    }
}

/// A FADS in progress (script_dispatch 0x77a40 -> loc_7FD08; it runs again each frame, loc_7FEF4 returning the
/// command itself, until it is done).
#[derive(Clone, Copy)]
struct Fads {
    /// frames left before the transition starts (dword_146E660)
    hold: u8,
    /// sub_60610 started the transition (qword_146E65C = 1); the next run ends the command
    started: bool,
}

#[derive(Default)]
pub struct Page {
    pub chars: Vec<PlacedChar>,
    /// raw texts of the messages on the page (for saves)
    texts: Vec<String>,
    /// per message: F(847 + its index on the page) when it was written, which its history record keeps
    /// (sub_28470 spaces the backlog entries with it)
    extras: Vec<i32>,
    pen: (f32, f32),
    order: usize,
    /// where the last message's layout started (pen, order) and its slot's base x for tag 12: MSAD lays the grown
    /// message out again from here
    last_start: ((f32, f32), usize),
    last_base: f32,
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
    /// per message: first index into `chars`, whether its text ends in @k, how far its read fade has gone
    /// (0..1) and that fade's length in frames (stage word 870 + i: the text speed * 1.2 while it was current)
    msgs: Vec<(usize, bool, f32, f32)>,
    /// the message drawn a line above line 0 (`Game::show_message`): its first index into `chars` and how far
    /// down line 0 is; the next message moves it there
    lifted: Option<(usize, f32)>,
    /// SELD options listed after the page's messages (history entries of type 12, shown in the backlog)
    choice_texts: Vec<String>,
    /// their F(847 + index), as `extras`
    choice_extras: Vec<i32>,
    /// per message: Witch on the Holy Night's `@z` (`Game::z_cur`)
    zs: Vec<Option<bool>>,
    /// time towards the glyph wobbles' next 60 Hz step (`text::Wobble`)
    wobble_ms: f32,
}

/// Message text (sub_391A0): text slots 464 units wide at glyph scale 0.76 of the W / 32 px font size
/// (sub_A7790: 45.6 px glyphs at 1080p), with nn::font character spacing (a7 + 4) * W / 1920 = 4 px and
/// line spacing (a8 + 14) * H / 1080 = 24 px (sub_C3944, a8 = 10). Page mode (F822 == 1, set by
/// START_SCRIPT, every normal scene) draws from x 88 units (v278; 30 when sub_2DF20, non-Japanese) with line
/// n's box top at word_136BAB2 (65 units) + n * pitch (the dword_AFECCC table); the pitch is slot 19's
/// measured two-line height: the pair font's (exfont + shared font, sub_CAE88) line feed at its 40 px base
/// size, `text::line_feed` (50), scaled by 45.6 / 40, plus the 24 px spacing = 81 px (Switch: 81 in Astris and
/// choice.mov). `msg_layout` fills in `line_height`.
pub const LAYOUT: Layout = Layout {
    left: 88.0 * 3.0,
    right: (88.0 + 464.0) * 3.0,
    size: 60.0 * 0.76,
    spacing: 4.0,
    sx: 1.0,
    line_height: 0.0,
    ascent: ASCENT,
    ruby_y: text::RUBY_Y,
};
/// English (overseas build, sub_391A0 with sub_2DF20, docked): page text from x 32 units, line 0's box at
/// (int)(65 - 9.8824) = 55 units, the slot sub_39050 widens from 464 to 573 units, and every text slot's
/// glyphs at 0.9 of the W / 32 size (sub_A7790); the pitch is English's 53 px line feed at that size plus 24.
pub const EN_LAYOUT: Layout = Layout {
    left: 32.0 * 3.0,
    right: (32.0 + 573.0) * 3.0,
    size: EN_SIZE,
    spacing: 4.0,
    sx: 1.0,
    line_height: 0.0,
    // the font's ascent applies at run time (`msg_layout`)
    ascent: ASCENT * 0.9,
    ruby_y: text::RUBY_Y,
};
const EN_SIZE: f32 = 60.0 * 0.9 * 0.76;
/// English message slot width in 640-wide units (sub_39050: 464 -> 573 for non-bubble windows)
const EN_WIDTH: f32 = 573.0;
/// F822 != 1 (LABEL_135): the message alone (no earlier history entries) from x 169.58 units, line 0's box
/// at 43.141 units (trainscene.mov: glyph tops y 116, first glyph x 562 after the indent).
const WINDOW_LEFT: f32 = 169.58;
/// Witch on the Holy Night's page layout (WoH jp_391A0, docked): text from x 92 units (30 for English, v239),
/// line 0's box at F824 = 65 units (English 55), slot 28 436 units wide (English 556, sub_1AE20), glyphs at 0.74
/// of the base size; the line spacing argument is 9 (Tsukihime 10).
pub const WITCH_LAYOUT: Layout = Layout {
    left: 92.0 * 3.0,
    right: (92.0 + 436.0) * 3.0,
    size: 60.0 * 0.74,
    spacing: 4.0,
    sx: 1.0,
    line_height: 0.0,
    ascent: ASCENT,
    ruby_y: text::RUBY_Y_WITCH,
};
/// Witch on the Holy Night in English: x 30, 556 units wide (sub_1AE20), and WoH's sub_A7790 also takes 0.9 of every
/// English text slot's glyph size.
pub const WITCH_EN_LAYOUT: Layout = Layout {
    left: 30.0 * 3.0,
    right: (30.0 + 556.0) * 3.0,
    size: 60.0 * 0.9 * 0.74,
    spacing: 4.0,
    sx: 1.0,
    line_height: 0.0,
    ascent: ASCENT,
    ruby_y: text::RUBY_Y_WITCH,
};
/// The overseas build's message layout keeps the Japanese one for Japanese and Chinese; the docked game
/// (sub_A59D0: the 1920x1080 display mode) never takes its handheld branch (box 46, 0.85 glyphs, 508 wide).
pub fn msg_layout() -> Layout {
    let witch = crate::assets::witch();
    let lay = match (witch, text::english()) {
        (true, true) => WITCH_EN_LAYOUT,
        (true, false) => WITCH_LAYOUT,
        (false, true) => EN_LAYOUT,
        (false, false) => LAYOUT,
    };
    // the line spacing (a8 + 14) * H / 1080 = 24 px (Witch on the Holy Night 23) after the font set's line feed
    // at the glyph size; the baseline at the pair font's ascent
    let spacing = if witch { 23.0 } else { 24.0 };
    // the baseline scales with the glyph (Witch on the Holy Night: 0.74 of the W / 32 size, Tsukihime 0.76)
    let ascent = text::msg_ascent(ASCENT) * if witch { 0.74 / 0.76 } else { 1.0 };
    Layout { line_height: text::line_feed() * lay.size / text::BASE_SIZE + spacing, ascent, ..lay }
}
/// Baseline of the page's first line (box 65 units, English 55).
pub fn top_baseline() -> f32 {
    (if text::english() { 55.0 } else { 65.0 }) * UY + msg_layout().ascent
}
/// Glyph baseline below a line box top for the Japanese fonts: their pair ascent 41 + 1 at the 40 px base
/// (`text::msg_ascent`), scaled to the 45.6 px glyphs (Switch: page line 0's baseline 47.91 px below its box at
/// 65 units, a sub-pixel fit of the prologue's first page in both builds).
pub const ASCENT: f32 = (text::JA_ASCENT + 1.0) * 60.0 * 0.76 / text::BASE_SIZE;
const WINDOW_TOP_BASELINE: f32 = 43.141 * UY + ASCENT;
/// 640x448 units -> canvas pixels.
const UX: f32 = CANVAS_W as f32 / 640.0;
const UY: f32 = CANVAS_H as f32 / 448.0;
/// Save-list preview picture size: 82.7 x 57.9 of 640x448 (sub_4F240)
pub const PREVIEW_SIZE: (u32, u32) = (248, 140);
/// The preview render target ("thumbnail surface", sub_A5C10), the stage scaled by 256 / 1920
const PREVIEW_TARGET: (u32, u32) = (256, 144);
/// How many commands ahead `Game::prefetch_images` reads the images while skipping.
const PREFETCH_SKIP: usize = 240;
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
/// The overseas build's window types 0..55 (flt_231D74: image slot, origin in 1280x720 units). DISP_PARTS has
/// three more images before the bubbles, and types 33..55 reuse bubble images at other origins.
const EN_WINDOWS: [(u16, f32, f32); 56] = [
    (60, 0.0, 0.0), (61, 0.0, 0.0), (62, 0.0, 0.0),
    (94, 525.0, 414.0), (95, 525.0, 414.0), (96, 328.0, 393.0), (97, 328.0, 393.0), (98, 90.0, 388.0), (99, 90.0, 388.0),
    (100, 525.0, 44.0), (101, 525.0, 44.0), (102, 328.0, 34.0), (103, 328.0, 34.0), (104, 90.0, 13.0), (105, 90.0, 13.0),
    (106, 94.0, 414.0), (107, 94.0, 414.0), (108, 79.0, 393.0), (109, 79.0, 393.0), (110, 63.0, 388.0), (111, 63.0, 388.0),
    (112, 94.0, 44.0), (113, 94.0, 44.0), (114, 79.0, 34.0), (115, 79.0, 34.0), (116, 63.0, 13.0), (117, 63.0, 13.0),
    (118, 338.0, 229.0), (119, 338.0, 229.0), (120, 196.0, 210.0), (121, 196.0, 210.0), (122, 71.0, 198.0), (123, 71.0, 198.0),
    (96, 318.0, 393.0), (96, 318.0, 393.0), (98, 70.0, 388.0), (98, 90.0, 378.0), (98, 250.0, 378.0), (99, 300.0, 388.0),
    (102, 268.0, 34.0), (102, 98.0, 34.0), (103, 198.0, 34.0), (104, -5.0, 13.0), (104, 90.0, 13.0), (104, 105.0, 23.0),
    (105, 90.0, 13.0), (108, -101.0, 393.0), (110, 43.0, 388.0), (110, 63.0, 388.0), (114, 29.0, 34.0), (116, -47.0, 13.0),
    (116, -17.0, 13.0), (117, 18.0, 13.0), (122, 21.0, 198.0), (122, 61.0, 198.0), (123, 11.0, 198.0),
];
/// The overseas build's bubble text origins by window type (flt_232020, 1280x720 units).
const EN_BUBBLE_TEXT: [(f32, f32); 56] = [
    (0.0, 0.0), (0.0, 0.0), (0.0, 0.0),
    (98.0, 80.0), (98.0, 80.0), (100.0, 114.0), (100.0, 114.0), (100.0, 138.0), (100.0, 138.0), (98.0, 67.0),
    (98.0, 67.0), (100.0, 84.0), (100.0, 84.0), (100.0, 89.0), (100.0, 89.0), (79.0, 80.0), (79.0, 80.0),
    (90.0, 114.0), (90.0, 114.0), (113.0, 138.0), (113.0, 138.0), (79.0, 67.0), (79.0, 67.0), (90.0, 84.0),
    (90.0, 84.0), (113.0, 89.0), (113.0, 89.0), (98.0, 67.0), (98.0, 67.0), (100.0, 64.0), (100.0, 64.0),
    (100.0, 89.0), (100.0, 89.0), (70.0, 110.0), (80.0, 102.0), (81.0, 134.0), (99.0, 120.0), (90.0, 120.0),
    (99.0, 110.0), (65.0, 80.0), (65.0, 80.0), (85.0, 82.66), (45.0, 89.0), (65.0, 89.0), (95.0, 65.0),
    (99.0, 69.0), (90.0, 110.0), (104.0, 120.0), (85.0, 126.0), (89.0, 82.66), (103.0, 65.0), (93.0, 89.0),
    (103.0, 89.0), (70.0, 80.33), (82.0, 55.0), (70.0, 80.33),
];
/// The overseas build's bubble text widths (jd_34DEFC, by type - 3) for the types bit-set in
/// EN_BUBBLE_WIDTH_MASK (sub_39050); the others keep the message width.
const EN_BUBBLE_WIDTH: [f32; 40] = [
    299.83, 299.83, 405.57, 405.57, 509.77, 509.77, 299.83, 299.83, 405.57, 405.57, 509.77, 509.77, 299.83, 299.83,
    405.57, 405.57, 509.77, 509.77, 299.83, 299.83, 405.57, 405.57, 509.77, 509.77, 299.83, 299.83, 405.57, 405.57,
    509.77, 509.77, 299.83, 299.83, 299.83, 299.83, 299.83, 299.83, 299.83, 299.83, 299.83, 580.0,
];
const EN_BUBBLE_WIDTH_MASK: u64 = 0x80_3FFF_FFFF;
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
/// Image slots 52.. are DISP_PARTS_JA in order (Witch on the Holy Night: 50.., its object table at 0x3AFBC0).
const DISP_SLOT0: u16 = 52;
const WITCH_DISP_SLOT0: u16 = 50;
/// sub_38EC0 / the bubble cursor draw icons at 0.36 x 0.45 units per texture pixel.
const ICON_SCALE: (f32, f32) = (0.36 * UX, 0.45 * UY);
/// System sounds (sub_710: dword_3341A0[n] = sound 0x10040D4 + n): 0 cursor move, 1 confirm, 2 refusal / back,
/// 6 PRESS ANY BUTTON / NEW GAME, 7 phase title movies.
/// off_46B138: the config's SE test effects.
const TEST_SES: [&str; 14] = [
    "moon_se02009",
    "moon_se02010",
    "moon_se02011",
    "moon_se02023",
    "moon_se02035",
    "moon_se02037",
    "moon_se02052",
    "moon_se02058",
    "moon_se02062",
    "moon_se02091",
    "moon_se10047",
    "moon_se02097",
    "moon_se02103",
    "moon_se09005",
];

/// sub_710's sound ids (dword_3341A0): BFSAR items 0x10040d4.. in order. 0x10040d5 (1) and 0x10040db (7) have no
/// name; the others are WSD_MOON_SE00001, 03, 04, 07, 10, 11 and 14.
const SYS_SOUNDS: [u32; 9] = [0x10040d4, 0x10040d5, 0x10040d6, 0x10040d7, 0x10040d8, 0x10040d9, 0x10040da, 0x10040db, 0x10040dc];
/// Witch on the Holy Night's system sounds (jp_710: jd_3341A0, 14 entries, 11 / 12 none).
const WITCH_SYS_SOUNDS: [u32; 14] =
    [0x10024fe, 0x10024ff, 0x1002500, 0x1002501, 0x1002502, 0x1002503, 0x1002504, 0x1002505, 0x1002506, 0x1002507, 0x1002508, 0, 0, 0x1002501];

/// System sound n's BFSAR item (sub_ACEF0 / sub_ACFA0's table).
fn sys_sound_id(n: usize) -> Option<u32> {
    let table: &[u32] = if crate::assets::witch() { &WITCH_SYS_SOUNDS } else { &SYS_SOUNDS };
    table.get(n).copied().filter(|&id| id != 0)
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
/// Witch on the Holy Night's voice name prefixes -> speaker (sub_1D70, table 0x2E7698; exact letters, 0 when
/// unlisted).
const WITCH_VOICE_SPEAKERS: [(&str, usize); 22] = [
    ("ZZZ", 0), ("A10", 1), ("A11", 1), ("A40", 2), ("A30", 3), ("B70", 4), ("B10", 5), ("B11", 5), ("B50", 6),
    ("B60", 7), ("B40", 8), ("B41", 8), ("B30", 9), ("B20", 10), ("A20", 11), ("A21", 11), ("A50", 12), ("B80", 13),
    ("B81", 13), ("B90", 14), ("BA0", 0), ("C70", 0),
];
/// Witch on the Holy Night's speaker -> config voice row bit (AE04D8 = 0x459270: speaker 0 row 14, k row k - 1).
const WITCH_SPEAKER_ROW_BIT: [u32; 15] = [16384, 1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192];

/// Speaker of a voice file (sub_2360: its first three letters; 0 when unlisted).
fn voice_speaker(name: &str) -> usize {
    if crate::assets::witch() {
        return name.get(..3).and_then(|p| WITCH_VOICE_SPEAKERS.iter().find(|s| s.0 == p)).map_or(0, |s| s.1);
    }
    name.get(..3).map(str::to_ascii_uppercase).and_then(|p| VOICE_SPEAKERS.iter().find(|s| s.0 == p)).map_or(0, |s| s.1)
}

/// Config voice row of a voice file's speaker (sub_29390).
fn voice_row(name: &str) -> usize {
    if crate::assets::witch() {
        return WITCH_SPEAKER_ROW_BIT[voice_speaker(name)].trailing_zeros() as usize;
    }
    SPEAKER_ROW_BIT[voice_speaker(name)].trailing_zeros() as usize
}

/// Config voice row -> its progress word (dword_34D4B4): bit 4 = row shown, 1 = own face (rows 15, 16),
/// 2 = own name (rows in 0x3CF36).
pub const VOICE_ROW_WORDS: [usize; 19] = [11, 12, 13, 14, 15, 16, 27, 17, 29, 18, 23, 26, 19, 24, 25, 20, 21, 22, 30];
const BOTTOM_LIMIT: f32 = 1000.0;
const PARAGRAPH_GAP: f32 = 0.0;
/// sub_391A0: earlier messages on the page fade to 100/255 over (config word 7 / 8, the unread / read text
/// speed the message had while current) * 1.2 frames; a run of messages ending in @k shares the fade of the
/// message that ends it.
const READ_ALPHA: f32 = 100.0 / 255.0;
/// A rewind-ring entry: the snapshot, the RDST count it belongs to (for the mode panel's previous scene), its
/// (scene, message pc, page line) key and a serial the backlog pages refer to.
/// The sound words a resume (sub_5D7E0) plays: the BGM id and volume, the looping SE per channel.
#[derive(Clone, Debug, Default)]
struct SoundState {
    bgm: Option<(i32, f32)>,
    loops: std::collections::BTreeMap<i32, (String, f32)>,
}

#[derive(Clone)]
struct RingEntry {
    data: crate::save::SaveData,
    node: u32,
    key: (usize, usize, usize),
    serial: u64,
    /// record word +13760 != 0: taken while a story movie (task slot 0) or the staff roll task (slot 1,
    /// XSTP) ran (sub_3F960), so the backlog cannot quick-jump to it (sub_28A90)
    locked: bool,
    /// entry type +32: 0 a message, 1 the first stage change since the backup refresh (sub_5F3B0), 9 a backup
    /// refresh (VPLY / MSAD / WTTM / `_ZY`); a rewind stops only at messages
    kind: u16,
}

impl RingEntry {
    /// The entry of a page's first message: one its message pushed, or a record a load or scene point put back
    /// in the ring (a message entry that was the save point, so F823 <= 1).
    fn page_start(&self) -> bool {
        self.key.2 == 1 || (self.key.2 == usize::MAX - 1 && self.kind == 0)
    }
}

/// A scene-point ring entry: a copy of the page-ring entry ATSV pushed (or, when sub_5A910 held the push, of
/// the newest entry), and that ATSV entry's serial, by which an undone ATSV entry drops it (sub_5BAF0 cases
/// 6 / 7 compare the entry's ring slot).
#[derive(Clone)]
struct ScenePoint {
    data: crate::save::SaveData,
    serial: Option<u64>,
}

/// How a ring entry is restored: a held rewind (sub_814C0), a backlog quick jump (sub_28D50) or a scene
/// point (sub_5D950, restored like a load).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Back {
    Rewind,
    Jump,
    Point,
}

/// scene-point ring length (word_136B440 stays at 299 once full, the oldest entries move down)
const SCENE_POINTS: usize = 300;
/// entry text length (+13956: 0x1FE bytes and a terminating 0)
const ENTRY_TEXT: usize = 0x1fe;

/// A finished page in the backlog: its texts and the serials of its first and last ring entries (None when
/// they were never recorded, as for the page a load restored).
struct BacklogPage {
    texts: Vec<String>,
    /// per entry: F(847 + its index) from its record (`Page::extras`)
    extras: Vec<i32>,
    first: Option<u64>,
    last: Option<u64>,
    /// the overseas sub_28470 draws entries of bubble windows (their F1105 >= 3, sub_39F40) with text flag
    /// 0x800000, the bubble font (the Japanese build passes 0)
    bubble: bool,
}

/// rewind history length (sub_814C0: 800 page records)
const HISTORY: usize = 800;
/// frames a message stays up under a skip (measured on the Switch: about 20 messages a second within a page)
const SKIP_FRAMES: f32 = 3.0;
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
    /// the message text's glyph edge and shadow (sub_A7790: config u16 20, Witch on the Holy Night's 0x26),
    /// unless the slot's flags force them
    pub text_edge: bool,
    /// Witch on the Holy Night's text colour mode (config u16 0x2A, jp_6E0D0 / jp_391A0): 1 tints read text
    /// 0xDC6D, 2 takes the speaker table (all 0xFFFF); 0 at first
    pub text_colour: u16,
    /// Witch on the Holy Night's message window opacity (config u16 0x10 / 255, jp_391A0: window types 0..2)
    pub window_alpha: f32,
    /// Witch on the Holy Night's BGM auto adjust (config u16 0x1C): the BGM ducks under a voice (jp_48AC0)
    pub bgm_auto: bool,
    /// Witch on the Holy Night's skip release at a choice (config u16 0x44, jp_83E50)
    pub choice_skip_stop: bool,
    /// the button help balloons (Witch on the Holy Night's config u16 0x34, jp_ADC40)
    pub help_text: bool,
    /// Witch on the Holy Night's config 0x32: the mode panel's previous / next scene buttons act (jp_4E170;
    /// nothing in the config pages writes it, 1 by default)
    pub scene_jump: bool,
}

/// The exe's initial config (0x46B080; Witch on the Holy Night's text speed, `savedata::config_defaults`).
impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&crate::savedata::config_defaults())
    }
}

pub struct Game {
    pub assets: Assets,
    motions: MotionTable,
    pub stage: Stage,
    pub sound: Sound,
    font: FontAtlas,
    /// ja3 FOT-Seurat: the message text while the window is a bubble (see `msg_font`)
    font_bubble: Option<FontAtlas>,
    /// the message glyph cache's font order (`text::GlyphCache`)
    glyphs: text::GlyphCache,
    /// the registrations of the messages laid out since the page last drew, by message (`text::GlyphCache::first_print`)
    glyph_regs: Vec<(usize, Vec<text::Registration>)>,
    pub settings: Settings,
    /// jp_48AC0's BGM ducking (Witch on the Holy Night): the level of 256 (dword_3CD650), the level the BGM
    /// volume was last set for (dword_3CD648), the 60 Hz step's time and sub_FF70's BGM curve level
    duck: i32,
    duck_set: i32,
    duck_ms: f32,
    bgm_curve: i32,

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
    /// WIK_SEL_PARTS (Witch on the Holy Night's SEWI, image slots 4640..), loaded while the select shows
    wik_parts: Option<Vec<TexRef>>,
    /// TTCT's menu (Witch on the Holy Night)
    teatime: Option<teatime::TeaTime>,
    /// choice option texts and their blur (texture 5)
    choice_text_rt: Option<RenderTarget>,
    choice_glow: Option<RenderTarget>,
    sysmes: Option<Vec<String>>,
    trans: Option<Transition>,
    snapshot: bool,
    fads: Option<Fads>,
    /// sub_4B570: the rule mask the last FADS loaded (dword_590E8C, rule - 1), -1 none
    fads_rule: i32,
    /// SHOK / SHAF screen shake
    shake: shake::ScreenShake,
    /// MSQK message-text quake and its random source
    msg_quake: shake::MsgQuake,
    fx_rng: crate::fx::Rng,
    /// the exe's app random source (sub_A8F90)
    app_rng: crate::fx::TinyMt,
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
    /// the UI draws the movie itself (`movie_quad`), over the gallery movie player's curtain
    pub movie_by_ui: bool,
    pub auto: bool,
    /// R skip mode (off_AE0660 bit 1)
    pub skip_mode: bool,
    /// debug: pick the first option of every choice
    pub auto_choice: bool,
    /// a system screen is open: freeze the script and animations
    pub paused: bool,
    /// Witch on the Holy Night's XSKR(n, label) staff roll skip: n (dword_459414, -1 off), the label (qword_E66AE8)
    /// and the frames a button has been held (dword_E66AF0)
    xskr: i32,
    xskr_pc: Option<usize>,
    xskr_hold: f32,
    /// a controller button is held (the pad state's +8 word, sub_40ED0's v110)
    pub pad_held: bool,
    /// dword_E66B34: set when a click wait ends (jp_802C0 0x600c0), cleared at the start of the next script pass
    /// while neither the advance buttons nor a touch are held; it holds the XSKR count back
    adv_latch: bool,
    auto_timer: f32,
    /// the message being shown was already read (selects the read text speed)
    line_read: bool,
    pub ended: bool,
    /// the script stopped with sub_802C0 result 2 (RTM_ / RTMN, or RDED of the replayed node while F024 is
    /// set): the system data is written before the gallery comes back (sub_48AC0 case 2 states 7 / 5), and
    /// the stage goes with the window when F0 is 0
    pub replay_done: bool,
    /// RDAS hit a completed node with "既読シーンのオートスキップ" on (sub_92580 -> off_AE06B0 = 1)
    pub auto_skip_hit: bool,
    /// the mode banner (sub_3B430, `Ui::banner`) is up, as of the last frame's UI step (qword_AFEC90 state != 0)
    pub banner_up: bool,
    /// RDAS asks the UI to send the banner away (sub_3B480)
    pub banner_away: bool,
    /// a gallery flowchart replay runs (sub_50F0: off_AE0288 == 2, `Ui::flow_replay`), set before each update
    pub flow_replay: bool,
    /// the command just run made script_dispatch return (loc_7FEF0 / loc_7FEF4)
    dispatch_return: bool,
    /// a next / previous scene countdown runs (sub_40ED0 states 3 / 6 return before the script)
    pub hold: bool,
    /// off_AE05B8: a held rewind runs sub_814C0 instead of the script step (sub_40ED0), so the restored
    /// messages are not shown until it stops
    pub rewinding: bool,
    /// Witch on the Holy Night: a rewind is held. jp_40ED0 runs its steps at speed 0 (dword_649890 restarts on
    /// every frame between them), so jp_81B30 does not move the text head until it is released.
    pub rewind_held: bool,
    /// Witch on the Holy Night's off_6327C0: a click passed the finished message (jp_802C0 clearing 0x100), so the
    /// next message laid out (jp_6E0D0, or jp_5BAF0 case 0 for a restored one) flips the text slot pair (AE05D0)
    /// and starts in a cleared slot
    slot_flip: bool,
    /// Witch on the Holy Night: the frame between two steps has run its script once; the further display frames
    /// until the next step (a faster display) run no script, as jp_40ED0 runs one game frame between two steps
    pub rest_hold: bool,
    /// XSTP's staff roll task (sub_3F600 slot 1 kind 1: start sub_40B70, frame sub_40C10, end sub_40C60): F36 = 1,
    /// auto and skip mode off (off_AE0660 & 3 kept in word_AFFC98 for the end), off_AE0650 = 1 (the story
    /// keeps only A and ZR, held ZR fast-forwarding at 32); it ends when F36 is 0 or on XRSM
    pub staff_task: bool,
    /// word_AFFC98: auto / skip mode (off_AE0660 & 3) as the last story movie or staff roll start found them
    /// (sub_402A0 / sub_40B70 clear both); every end puts them back (sub_40A00 / sub_40C60), also an end
    /// whose start was skipped
    task_modes: (bool, bool),
    /// off_AE0650: set by the story movie and staff roll inits (sub_402A0 / sub_40B70), which keep the old value
    /// in dword_AFFC9C for their ends to put back; while set the story keeps only A and ZR (sub_40ED0) and
    /// skip mode is cleared every frame. A task started during a next-scene seek skips its init, so it runs
    /// without it
    task_lock: (bool, bool),
    /// a gallery replay (byte_B34AF0, sub_47B00): set when cGallery starts 528_ARCHIVE (sub_48AC0 case 4),
    /// cleared by every other start; `IFSC` tests it
    pub archive: bool,
    /// off_632648 (Witch on the Holy Night): a TeaTime replayed from the Archive (jp_48AC0 mode 5 state 11) until
    /// its TTCT (sub_73EB0) clears it; STGS takes its end at once meanwhile
    pub teatime_replay: bool,
    /// dword_AFFCA4: set by a story start (sub_40CD0) or a load (sub_82330), cleared when a message starts
    /// (sub_6E0D0 / sub_6E950); while set Y does not open the system menu (sub_40ED0)
    pub menu_lock: bool,
    /// SE commands waiting out their key 68 delay (byte_146E640 + 1112 + 20 * ch): (the slot, ms left, queued
    /// this frame). The frame count starts with the queueing frame's own sound update, so `ms left` is
    /// one frame short of it and that frame's tick passes the entry by.
    se_delayed: Vec<(SeQueued, f32, bool)>,
    /// The next-scene seek's sound state (off_AE0568 set): sub_802C0 stops all sound (sub_5EF10(0)), the BGM /
    /// SE commands then only record what they would play (word_146E654 / 650, the channel words at
    /// byte_146E640), and SELD / SELA / SELS (sub_92800 / sub_92930) resume it (sub_5D7E0); TOMP ends it
    /// without a resume.
    seek_audio: Option<SoundState>,
    mouse: (f32, f32),
    time: f32,
    warned: HashSet<String>,
    /// finished pages, oldest first (the backlog shows one page at a time)
    backlog_pages: Vec<BacklogPage>,
    /// hide the message window and text (while the backlog is shown)
    pub hide_text: bool,
    /// the player hid the message window (touch / hide button): not even the menu blur's capture has it
    pub touch_hidden: bool,
    /// the UI's system menu or its config is up (`Ui::system_menu_shown`, sub_4EC00)
    pub system_menu_shown: bool,
    /// The story's screen white (qword_B34B18, drawn by sub_48420 over everything): the title sets it to 1 when
    /// its NEW GAME white is full (sub_47B90(1.0)), and the story frame (sub_48AC0) drops it before the script
    /// step once START_SCRIPT has jumped to another scene (word_6533A4 >= 1)
    pub white: f32,
    /// script / effect steps per frame (sub_3FBD0 runs its effect steps this many times): the touch
    /// gesture and mode panel skip and rewind speeds, 1 normally
    pub speed: u32,
    /// per-message rewind history (sub_814C0 walks an 800-entry page ring, which the backlog lists too)
    history: std::collections::VecDeque<RingEntry>,
    /// ms the current click wait has been up (a skip passes it after `SKIP_FRAMES` 60 Hz frames)
    click_ms: f32,
    /// sub_391A0's click icon fade (dword_AFEC88), its animation ticks (dword_572B60, advanced only while the
    /// icon draws) and the last frame's ms
    icon_fade: f32,
    icon_ticks: f32,
    frame_ms: f32,
    /// ms the page text has been all out at a frame's end (sub_81B30 sees the slot's progress, set by the draw,
    /// a frame later)
    text_out_ms: f32,
    /// frames run (`update`) and the last frame a new message's reveal holds through (sub_A7790 gives the slot
    /// a 2 frame delay, sub_C3834, during which sub_A8240 reports progress 0 and the draw resets the head)
    frame_no: u64,
    reveal_hold: u64,
    /// ms since the script last ran under a skip (one dispatch per 60 Hz frame)
    dispatch_ms: f32,
    /// game time left before the script goes on after a command script_dispatch returns at (the next 60 Hz
    /// frame's sub_802C0; under a frame so that frame-time jitter at 60 Hz does not add one)
    resume_ms: f32,
    /// serial of the next history entry
    ring_serial: u64,
    /// serials of the current page's first and newest history entries
    page_ring: (Option<u64>, Option<u64>),
    /// What a save writes (the exe's in-memory record, sub_527F0): the state as the first message of the
    /// current page started (F823 <= 1, never a choice), so a save resumes at the start of its page
    save_point: Option<crate::save::SaveData>,
    /// The stage backup block (off_AE08D0, record +6664): the stage as it was before the first stage command
    /// since the last refresh (F991 = 0), or at the last FADS, VPLY, MSAD, WTTM or `_Z` message / label that
    /// found F991 set. None until the first refresh (the current stage is written then).
    stage_backup: Option<Vec<u8>>,
    /// The scene-point ring (off_AE04A0, 300 entries, top index word_136B440): ATSV pushes its entry there
    /// (0x7c174) unless arg bit 2 is set or an engine task runs. Previous scene loads from it (sub_81A50), a
    /// save point keeps its top as the previous entry (sub_527F0), a load puts the record and its previous
    /// entry back (sub_82330), and a rewind / backlog jump drops the entries of the ATSV entries it undoes.
    scene_ring: Vec<ScenePoint>,
    /// qword_146E66C: 2 when an ATSV point or a previous-scene load is made, one less per message (sub_6E0D0),
    /// 0 after a load or a rewind past a point; previous scene restarts the newest point once it is below 1
    scene_msgs: i32,
    /// byte_146E644: a message or MSAD ending in `@n` holds the ring pushes (sub_5A910) until the next message
    /// or MSAD starts
    ring_hold: bool,
    /// Witch on the Holy Night's `@z` (jp_BDC2C): the slot's text in font variant 3 (+101793, the shared system
    /// font as flag 0x100000 gives a bubble) and, once a slot, its stored line spacing (+101780) less 8 more
    /// with its spacing set from it (+101794 latched; the slots are only made at boot, jp_1C70 -> jp_B0280):
    /// which page slots (4 + the message's index) have taken it, and for the current message Some(whether it
    /// took it) when its text has the code (jp_A7790 stores the spacing and variant again for the next).
    /// Switch: NZ6's 「アリス、明日、…」 (the only `@z`) in the gothic font, lines 82 px apart after a boot.
    z_latch: u64,
    z_cur: Option<bool>,
    /// the `zs` of the page being laid out again (`relayout_page`)
    relay_zs: Vec<Option<bool>>,
    /// per text slot of the menus (`slot_z`): the text it was last given and its `@z`
    slot_texts: HashMap<usize, (String, Option<bool>)>,
    /// byte_146E64D: RTMN / RTM2 keep the BGM playing when the script ends (sub_5F200)
    keep_bgm_at_end: bool,
    /// byte_146EC28: the current message's raw text up to its end code (MSAD appends; ATSV() clears it), the
    /// text entries keep (+13956)
    msg_text: String,
    node_seq: u32,
    /// mode panel next scene (sub_53950(1)): skip, unread text included, up to the next RDST and stop
    /// at its first message
    pub seek_node: bool,
    /// the seek ended this frame; sub_802C0's after-loop work is still to run
    seek_ended: bool,
    /// overseas off_3516E0: the seek loop ended last frame (sub_41E80), so this frame runs its after-loop work
    /// (sub_7DDA0) instead of the dispatch
    seek_defer: bool,
    /// byte_146E648: a ZL rewind restored a message; until the next ring push, the message / MSAD / ATSV
    /// paths do not refresh the save point
    ring_restored: bool,
    /// a load ended with the ring indices back at 0 (sub_82330 after sub_5D950 / jp_82330 after sub_3E8D0:
    /// AE0460 = AE04D0 = 0): the next push takes slot 0, the record's own
    ring_reuse: bool,
    /// voice of the message being shown (mode panel voice replay)
    last_voice: Option<String>,
    /// VPLY's voice until the next message takes it (off_AE08B0)
    pending_voice: Option<String>,
    /// Word 0x7CA of the script block: an STCH / SCH2 changed the stage since the last FADS. sub_802C0 then
    /// presents the stage without a transition (sub_5D3A0(0, 0)) once script_dispatch returns at a wait.
    stage_dirty: bool,
    /// VPLY's voice waiting for the next message start (AE04C8)
    voice_to_play: Option<String>,
    /// VLOG (Witch on the Holy Night): the pending voice only becomes the replay voice
    log_voice: bool,
    /// Witch on the Holy Night's word_E66AD6: ms since the last load or rewind (jp_5BAF0 zeroes it, jp_81B30
    /// adds the frame's steps, at most 1200 frames); WTTM's third argument waits only what is left of its time
    since_restore: f32,
    /// off_632860 (Witch on the Holy Night): the last WTTM / WTIN wait could not be clicked through; CLO5's
    /// seek-end wait keeps it
    wait_no_click: bool,
    /// the shown message's voice and speaker (sub_6E0D0: page entry +13752 / +13756, off_AE0590)
    msg_voice: Option<String>,
    speaker: i16,
    /// System menu over the game (sub_2E000 / sub_3BBA0): the scene is captured once, gaussian-blurred
    /// (sub_A0A90: sigma 2.25 taps of 1.75 px; overseas sub_9EC50 at half size) and crossfaded in at this
    /// alpha; None when no menu is up.
    pub menu_blur: Option<f32>,
    menu_rt: Option<RenderTarget>,
    /// overseas sub_9EC50's half-size buffers ("filter Defocus" off_351DB0 and its pass target)
    menu_half: Option<[RenderTarget; 2]>,
    /// Witch on the Holy Night's sub_7FDB0 buffers: the capture at a quarter of the screen ("backbuffer
    /// Defocus" off_632A78 drawn at 0.25) and its pass target
    menu_quarter: Option<[RenderTarget; 2]>,
    /// Offscreen passes the UI queues for this frame (target, clear colour, quads), drawn before its quads
    pub ui_passes: Vec<(RenderTarget, Option<[f32; 4]>, Vec<Quad>)>,
    /// overseas screen capture (sub_A2550: jd_18AFD94 / jd_18AFD98): the frame as it was, drawn over everything
    /// at `alpha` (sub_A59F0), which falls by `step` per frame once released (sub_A27F0)
    freeze: Option<Freeze>,
    freeze_rt: Option<RenderTarget>,
    menu_captured: bool,
    movie: Option<crate::movie::Movie>,
    /// command index of the message currently shown
    msg_pc: usize,
    stuck: (usize, f32),
    /// WNTY: 0 = line-break icon, 1 = page-break icon
    wnty: i32,
    /// DISP_PARTS_JA textures in order: image slots DISP_SLOT0.. (message windows, wait icons)
    disp: Vec<TexRef>,
    /// BORDER_PARTS alphagradation020..512 and their `_inv` (stage::ClipRect::band)
    border: Vec<TexRef>,
    /// `relayout_page` runs: a message past the page bottom stays on the page
    relaying: bool,

    stage_rt: RenderTarget,
    /// STNS render target 40, created on first use
    noise_rt: Option<RenderTarget>,
    /// Save list previews (sub_884F0 / sub_89AD0): the Switch keeps no images, it rebuilds each preview
    /// from the record's stage block. Rendered in `render` (which has the renderer mutably), by slot.
    previews: std::collections::HashMap<usize, TexRef>,
    /// (slot, layers, overseas sub_86840 picture)
    preview_queue: Vec<(usize, Vec<Option<crate::save::LayerSave>>, Option<String>)>,
    preview_rt: Option<RenderTarget>,
    /// Target 0's bucket-1 draws (the stage, the FADE(40) effect, the FCLN effect): what a FADS captures
    scene_rt: RenderTarget,
    old_rt: RenderTarget,
    blur_rt: RenderTarget,
    /// FADE 40: blur the stage (FADE 41 clears)
    /// FADE(40) channel-split screen effect
    ghost: ghost::Ghost,
    /// FADE(40) scratch targets (the exe's render targets 2 and 3)
    ghost_rt: Option<[RenderTarget; 2]>,
    /// The layer group surfaces (stage::Draw): pixel (off_AE0CC8) and mask (off_AE0CD0), which keep their
    /// contents across frames, and the composite's scratch (render target 3)
    group_rt: Option<[RenderTarget; 3]>,
    /// FCLN focus lines / falling notes, the stage byte that restarts them on load (6569) and their
    /// EFFECT_PARTS textures (FallNote0..2, FocusLine)
    focus: focus::Focus,
    focus_saved: u8,
    focus_tex: Option<[TexRef; 4]>,
    canvas: RenderTarget,
}

/// STRR / STCH arg 19: 1/100 degrees wrapped into -18000..=18000 (as the exe's i16 store does).
fn wrap_rot2(v: i32) -> i16 {
    let v = v % 36000;
    (if v > 18000 { v - 36000 } else if v <= -18001 { v + 36000 } else { v }) as i16
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
/// copies the framebuffer for the shader to read; multiply and screen equal fixed-function blends. Mode 30 is
/// the default shader through blend state 1 (SrcAlpha / One).
///
/// Witch on the Holy Night (jd_3C9698[i - 3], so modes 1 and 2 get the default shader; its nn::gfx state 3 is
/// DstColor / 1-SrcAlpha): 1 and 30 add (state 1), 4 / 6 / 9 the non-ps darken / lighten / screen shaders, 7 the
/// default shader through state 3, 26 and 29 the default shader with blending off; 5, 8, 10..25 and 28 as here.
fn blend_mode(mode: i32) -> (Blend, Shade) {
    use knife_render::PsOp as P;
    if crate::assets::witch() {
        let b = match mode {
            1 | 30 => Some(Blend::Add),
            2 => Some(Blend::Alpha),
            4 => Some(Blend::Ps(P::DarkenColorAlpha)),
            6 => Some(Blend::Ps(P::LightenColorAlpha)),
            7 => Some(Blend::Ps(P::MultiplyDst)),
            9 => Some(Blend::Ps(P::ScreenColorAlpha)),
            26 | 29 => Some(Blend::Ps(P::Copy)),
            _ => None,
        };
        if let Some(b) = b {
            return (b, Shade::Sprite);
        }
    }
    let b = match mode {
        30 => Blend::Add,
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
        let disp = assets.ui_textures(r, &assets.ui_atlas("DISP_PARTS")).unwrap_or_default();
        // BORDER_PARTS (texture slots 338..355) for the clip bands: alphagradation020..512, then their `_inv`
        let border = assets.ui_textures(r, "BORDER_PARTS").unwrap_or_default();
        let border = ["", "_inv"]
            .iter()
            .flat_map(|v| ["020", "064", "080", "128", "256", "512"].map(|n| format!("alphagradation{v}{n}")))
            .map(|n| border.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone()))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        let mut g = Self::new_inner(assets, motions, font, r, script);
        g.disp = disp.into_iter().map(|(_, t)| t).collect();
        g.border = border;
        g
    }

    fn new_inner(assets: Assets, motions: MotionTable, font: FontAtlas, r: &Renderer, script: Arc<Scene>) -> Self {
        Self {
            assets,
            motions,
            stage: Stage::new(),
            sound: Sound::new(),
            font,
            font_bubble: None,
            glyphs: text::GlyphCache::default(),
            glyph_regs: Vec::new(),
            settings: Settings::default(),
            duck: 256,
            duck_set: 256,
            duck_ms: 0.0,
            bgm_curve: 0,
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
            wik_parts: None,
            teatime: None,
            choice_text_rt: None,
            choice_glow: None,
            sysmes: None,
            trans: None,
            snapshot: false,
            fads: None,
            fads_rule: -1,
            shake: shake::ScreenShake::default(),
            msg_quake: shake::MsgQuake::default(),
            fx_rng: crate::fx::Rng::default(),
            app_rng: crate::fx::TinyMt::from_time(),
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
            movie_by_ui: false,
            auto: false,
            skip_mode: false,
            auto_choice: false,
            paused: false,
            auto_timer: 0.0,
            line_read: false,
            ended: false,
            replay_done: false,
            auto_skip_hit: false,
            banner_up: false,
            banner_away: false,
            flow_replay: false,
            dispatch_return: false,
            hold: false,
            rewinding: false,
            rewind_held: false,
            slot_flip: false,
            rest_hold: false,
            staff_task: false,
            task_modes: (false, false),
            task_lock: (false, false),
            archive: false,
            teatime_replay: false,
            menu_lock: true,
            se_delayed: Vec::new(),
            seek_audio: None,
            mouse: (0.0, 0.0),
            time: 0.0,
            warned: HashSet::new(),
            backlog_pages: Vec::new(),
            click_ms: 0.0,
            icon_fade: 0.0,
            icon_ticks: 0.0,
            frame_ms: 0.0,
            text_out_ms: 0.0,
            frame_no: 0,
            reveal_hold: 0,
            dispatch_ms: 0.0,
            resume_ms: 0.0,
            ring_serial: 0,
            page_ring: (None, None),
            hide_text: false,
            touch_hidden: false,
            system_menu_shown: false,
            white: 0.0,
            speed: 1,
            history: Default::default(),
            save_point: None,
            stage_backup: None,
            scene_ring: Vec::new(),
            scene_msgs: 0,
            ring_hold: false,
            z_latch: 0,
            z_cur: None,
            relay_zs: Vec::new(),
            slot_texts: HashMap::new(),
            keep_bgm_at_end: false,
            msg_text: String::new(),
            node_seq: 0,
            seek_node: false,
            seek_ended: false,
            seek_defer: false,
            ring_restored: false,
            ring_reuse: false,
            last_voice: None,
            pending_voice: None,
            stage_dirty: false,
            voice_to_play: None,
            log_voice: false,
            since_restore: 0.0,
            xskr: -1,
            xskr_pc: None,
            xskr_hold: 0.0,
            pad_held: false,
            adv_latch: false,
            wait_no_click: false,
            msg_voice: None,
            speaker: 0,
            menu_blur: None,
            menu_rt: None,
            menu_half: None,
            menu_quarter: None,
            ui_passes: Vec::new(),
            freeze: None,
            freeze_rt: None,
            menu_captured: false,
            movie: None,
            msg_pc: 0,
            stuck: (0, 0.0),
            wnty: 0,
            disp: Vec::new(),
            border: Vec::new(),
            relaying: false,
            stage_rt: r.create_canvas_target(CANVAS_W, CANVAS_H, "stage"),
            noise_rt: None,
            previews: Default::default(),
            preview_queue: Vec::new(),
            preview_rt: None,
            scene_rt: r.create_canvas_target(CANVAS_W, CANVAS_H, "scene"),
            old_rt: r.create_canvas_target(CANVAS_W, CANVAS_H, "old frame"),
            blur_rt: r.create_canvas_target(CANVAS_W, CANVAS_H, "blur"),
            ghost: ghost::Ghost::default(),
            ghost_rt: None,
            group_rt: None,
            focus: focus::Focus::default(),
            focus_saved: 0,
            focus_tex: None,
            canvas: r.create_canvas_target(CANVAS_W, CANVAS_H, "canvas"),
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

    /// Present the stage (sub_5D3A0; Witch on the Holy Night's sub_3E130). Witch on the Holy Night's commit is
    /// also where an image counts as seen: every layer's image word (and stage word 0, never set) through
    /// jp_644A0, below 3200; its STCH marks nothing.
    fn commit_stage(&mut self, r: &Renderer) {
        self.build_textures(r);
        self.stage.commit();
        if crate::assets::witch() {
            let names: Vec<String> = self.stage.layers.iter().flatten().map(|l| l.name.clone()).collect();
            for n in names {
                if let Some(b) = self.seen_bit(&n) {
                    set_bit(&mut self.seen, b);
                }
            }
        }
    }

    pub fn var(&self, name: &str) -> i32 {
        self.vars.get(&self.var_name(name)).copied().unwrap_or(0)
    }

    /// The word a script variable names: `Cn` is F(n + F823) (script_dispatch's case 'C', 0x7a2b0), the
    /// per-message words of the page (C847 extra lines before the next message, C859 its fade timer).
    fn var_name(&self, name: &str) -> String {
        match name.as_bytes() {
            [b'C' | b'c', rest @ ..] if !rest.is_empty() && rest.iter().all(u8::is_ascii_digit) => {
                let n: i32 = name[1..].parse().unwrap_or(0);
                let f823 = self.vars.get("F823").copied().unwrap_or(0);
                var_key(&format!("F{}", n + f823))
            }
            _ => var_key(name),
        }
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
        let k = self.var_name(name);
        self.vars.insert(k, v);
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

    /// The character spacing the page's pen keeps after the last message's last glyph (`text::trailing_gap`:
    /// half after a Japanese ASCII run's, none after an English exfont run), which the slot's end cursor lacks
    /// (Switch, a bubble line ending in ASCII "d": its click icon a unit right of the full spacing's).
    fn pen_gap(&self, lay: &Layout) -> f32 {
        self.page.texts.last().map_or(0.0, |t| text::trailing_gap(&text::parse_markup(&self.resolve_text(t)), lay.spacing))
    }

    fn resolve_text(&self, s: &str) -> String {
        resolve_refs(&self.assets, s)
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
            Wait::Message { .. } => {
                if (self.page.revealed as usize) < self.page.order {
                    self.page.revealed = self.page.order as f32;
                } else {
                    // sub_802C0's 0x40 branch: the click (AE0698 / the confirm keys) clears the wait, stopping
                    // the voice when sub_9C3B0's condition holds
                    self.wait = Wait::None;
                    if self.settings.voice_stop_on_advance {
                        self.sound.stop_voice();
                    }
                }
            }
            Wait::Transition => {}
            _ => {}
        }
    }

    /// Witch on the Holy Night's staff roll skip (jp_81B30): while XSKR has a label, a held controller button
    /// (not the advance still held from the last click) counts frames; the one after 180 jumps to the label,
    /// ending a timed wait (AE04A8) and a voice wait (AE04B8 bit 4, stopping the voice).
    fn step_xskr(&mut self, dt: f32) {
        let Some(pc) = self.xskr_pc.filter(|_| self.xskr >= 0) else {
            return;
        };
        if self.adv_latch || !self.pad_held {
            self.xskr_hold = 0.0;
            return;
        }
        if self.xskr_hold < 180.0 - 0.01 {
            self.xskr_hold = (self.xskr_hold + dt / FRAME_MS).min(180.0);
            return;
        }
        self.pc = pc;
        self.xskr_pc = None;
        self.xskr_hold = 0.0;
        match self.wait {
            Wait::Time { .. } => self.wait = Wait::None,
            Wait::Voice => {
                self.sound.stop_voice();
                self.wait = Wait::None;
            }
            _ => {}
        }
    }

    /// sub_1DAE0's hold bar: frames held (0 hides it).
    pub fn xskr_hold(&self) -> f32 {
        if self.xskr >= 0 && self.xskr_pc.is_some() { self.xskr_hold } else { 0.0 }
    }

    /// Input for the frame, before `update` (the start of jp_802C0): `advance` = the advance buttons or a touch
    /// are held.
    pub fn set_held(&mut self, pad: bool, advance: bool) {
        self.pad_held = pad;
        if !advance {
            self.adv_latch = false;
        }
    }

    fn finish_click(&mut self) {
        self.click_ms = 0.0;
        self.slot_flip = true;
        self.adv_latch = true;
        // (the 11 per-message colour words stay: sub_40ED0 resets them only as a rewind starts, `start_rewind`)
        self.last_voice = None;
        self.page.waiting = false;
        self.wait = Wait::None;
        self.auto_timer = 0.0;
        if self.settings.voice_stop_on_advance {
            self.sound.stop_voice();
        }
    }

    /// `choice`: the choice reads the pointer (not while the mode panel is up: sub_83E50 returns while
    /// sub_4E170 reports it busy, AE06C0).
    pub fn mouse_move(&mut self, x: f32, y: f32, choice: bool) {
        self.mouse = (x, y);
        if choice {
            self.choice_hover_at(x, y);
            self.teatime_hover_at(x, y);
        }
    }

    // ---- per-frame update ---------------------------------------------------------

    pub fn update(&mut self, r: &Renderer, dt: f32) {
        self.frame_no += 1;
        // (the last frame's draw)
        self.latch_read_colours();
        // sub_A59F0: the capture fades by its step per frame, gone at 0
        if let Some(f) = &mut self.freeze
            && !f.capture
            && f.step > 0.0
        {
            f.alpha -= f.step * dt * 60.0 / 1000.0;
            if f.alpha <= 0.0 {
                self.freeze = None;
            }
        }
        if self.wait == Wait::Click {
            self.click_ms += dt;
        }
        self.dispatch_ms = (self.dispatch_ms + dt).min(1000.0 / 60.0);
        self.resume_ms = (self.resume_ms - dt).max(0.0);
        // an instant FADS draws its capture in the frame sub_60610 runs only (loc_7FD38); the frame after, the
        // script is held by the transition flag and nothing draws it
        if self.resume_ms <= FRAME_MS * 0.75
            && self.fads.is_some_and(|f| f.started)
            && self.trans.as_ref().is_some_and(|t| t.hold)
        {
            self.trans = None;
        }
        if self.white > 0.0 && self.scene_index >= 1 {
            self.white = 0.0;
        }
        self.sound.begin_frame(dt);
        if self.assets.build == Build::Witch {
            self.witch_duck(dt);
        }
        for i in 0..self.speed.max(1) {
            self.update_step(r, dt, i == 0);
        }
        if !self.paused {
            self.step_read_fades(dt);
        }
        // the sound update (sub_9B7F0) runs once a frame after the script: sub_65E50 counts the queued SE
        // commands down (a key-68 SE queued by this frame's script already loses a frame: 500 ms is 29 frames
        // after the SEPL, 475 ms measured on the Switch), except while a rewind is held (off_AE05B8); then
        // nn::atk's update. The main loop (sub_B5FE0) runs it whatever screen is up, so a queued SE still plays
        // (or ends a loop) under the system menu.
        if !self.rewinding {
            self.tick_se_delayed(if self.skip && self.speed <= 1 { dt * 8.0 } else { dt });
        }
        self.sound.step_se_pans(dt * 60.0 / 1000.0);
        self.sound.end_frame();
        // sub_391A0, once a frame while the window is up: the click icon fades in by 0.1 a frame once the slot's
        // text is all out (sub_AB8E0 -> sub_A8730) and the message waits for a click (AE04B8 0x400, set by
        // sub_6E0D0 / sub_6E950 with the message); otherwise it is 0
        self.frame_ms = dt;
        let out = self.page.revealed as usize >= self.page.order;
        self.text_out_ms = if out { self.text_out_ms + dt } else { 0.0 };
        if self.page.alpha > 0.0 {
            self.icon_fade = if self.wait == Wait::Click && out { (self.icon_fade + dt * 0.006).min(1.0) } else { 0.0 };
        }
    }

    /// The page's earlier messages fade towards READ_ALPHA (the trailing run of @k messages counts as current).
    /// sub_391A0 counts once a drawn frame (jp_B0190's frames, not the speed), only while F1103 is 0 and the
    /// content fade (off_AE05B0) is at 1, so a CLO4 / message-start fade holds them where they are.
    fn step_read_fades(&mut self, dt: f32) {
        let mut i = self.page.msgs.len();
        while i > 0 && (i == self.page.msgs.len() || self.page.msgs[i - 1].1) {
            i -= 1;
        }
        if self.var("F1103") != 0 || self.page.dim != 0.0 {
            i = 0;
        }
        // the current message's fade length follows the text speed of its read state (dword_572B64, set by
        // sub_6E0D0 / a rewind's layout); config byte 98 (fade out off) ends every fade at once
        let fade_frames = self.read_fade_frames();
        if let Some(m) = self.page.msgs.last_mut() {
            m.3 = fade_frames;
        }
        let instant = !self.settings.text_fade_out;
        for m in &mut self.page.msgs[..i] {
            m.2 = if instant || m.3 <= 0.0 { 1.0 } else { (m.2 + dt * 0.06 / m.3).min(1.0) };
        }
    }

    /// One of the frame's `speed` steps; `first`: the frame's first, which also steps what the exe updates once
    /// a frame whatever the speed (the choice, sub_81B30 -> sub_83E50; the TeaTime menu, sub_745E0).
    fn update_step(&mut self, r: &Renderer, dt: f32, first: bool) {
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
        // the next-scene seek steps nothing: it runs whole in the dispatch below, its waits zeroed
        let fast = self.skip;
        // the speed already repeats the steps (sub_3FBD0); a skip at speed 1 (the debug skip) runs time 8x
        let step = if fast && self.speed <= 1 { dt * 8.0 } else { dt };
        let cut = fast && self.speed <= 1;
        self.since_restore = (self.since_restore + dt).min(1200.0 * 1000.0 / 60.0);

        // layer tweens and motions
        // Witch on the Holy Night's type 4 quake ends once no voice plays at a click wait (jp_3C970)
        self.stage.talk_done = self.wait == Wait::Click && !self.sound.voice_playing();
        self.stage.advance(step);
        let (p, q) = (self.var("F1120") - 127, self.var("F1121"));
        self.ghost.advance(step, p, q);
        self.focus.advance(step);
        for li in 0..self.stage.layers.len() {
            let cur = self.motion_cur(li);
            let Some(m) = self.stage.layers[li].as_mut().and_then(|l| l.motion.as_mut()) else { continue };
            let f = self.motions.advance(m, step, &cur);
            self.apply_motion(r, li, f);
        }
        self.msg_quake.advance(step, &mut self.fx_rng);
        self.shake.advance(step);
        self.stage.shake = self.shake.offset;
        // transition
        if let Some(t) = &mut self.trans
            && !t.hold
        {
            t.t += step;
            if t.t >= t.dur || cut {
                self.trans = None;
            }
        }
        // window fade / text reveal
        // sub_81B30 (before sub_38D40 steps the fades) holds the text head while the window is shown and still
        // fading in or the content fade (A8) runs, unless F1103 (Switch 499_QA_05's message after CLO3: the text
        // starts once the bubble is in, the content fade's 18 frames after the message); instant text and a
        // skip set the head after that test too
        let head_held = self.page.visible
            && (self.page.alpha < 1.0 || self.page.dim_vel < 0.0)
            && self.var("F1103") == 0;
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
        // the message slots' glyph wobbles (jp_C0684 in each slot's update): one step per 60 Hz frame
        self.page.wobble_ms += dt;
        while self.page.wobble_ms >= FRAME_MS {
            self.page.wobble_ms -= FRAME_MS;
            let rng = &mut self.app_rng;
            for w in self.page.chars.iter_mut().filter_map(|pc| pc.wobble.as_mut()) {
                w.step(|a, b| rng.uniform(a, b));
            }
        }
        let k = usize::from(self.line_read);
        let rate = self.chars_per_frame(k);
        if self.frame_no > self.reveal_hold && !head_held && !self.rewind_held {
            match rate {
                Some(rate) if !self.settings.page_at_once[k] && !fast => {
                    self.page.revealed = (self.page.revealed + rate * dt / FRAME_MS).min(self.page.order as f32);
                }
                _ => self.page.revealed = self.page.order as f32,
            }
        }
        if first {
            self.update_choice(dt);
            self.update_teatime(dt);
        }

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
            // under a skip at speed n the exe's waits and effects run n steps a frame (the wait budget
            // sub_802C0 gets is speed x frames); knife cuts them short only for the debug skip at speed 1
            // (the exe counts whole frames; the tolerance keeps f32 steps of 1000 / 60 from adding one)
            Wait::Time { left, click } => {
                let left = left - step;
                self.wait = if left <= 0.01 || cut { Wait::None } else { Wait::Time { left, click } };
            }
            // sub_3C970 clears the transition flag (AE04B8 8) once the progress (sub_4B860) is out, after the
            // frame's script step, so the script goes on the frame after
            Wait::Transition => {
                if self.trans.is_none() {
                    self.wait = Wait::None;
                    self.resume_ms = FRAME_MS * 0.75;
                }
            }
            Wait::Layer(l, mask) => {
                if cut {
                    for layer in self.stage.layers.iter_mut().flatten() {
                        layer.finish();
                    }
                }
                if !self.stage.animating(l, mask, &|id| self.motions.looping(id)) || cut {
                    self.wait = Wait::None;
                }
            }
            Wait::Voice => {
                if !self.sound.voice_playing() || fast {
                    self.wait = Wait::None;
                }
            }
            // the draw sets the slot's progress (sub_AB710) after sub_81B30 moved the head, so sub_81B30 swaps bit 1
            // for 0x40 the frame after the text is out and sub_802C0 goes on the frame after that (the exe's head
            // also takes its first step in the dispatch frame, knife's in the next)
            Wait::Message { voice } => {
                let out = self.page.revealed as usize >= self.page.order && self.text_out_ms >= FRAME_MS - 1.0;
                if out && (!voice || fast || !self.sound.voice_playing()) {
                    self.wait = Wait::None;
                }
            }
            Wait::Click => {
                let revealed = self.page.revealed as usize >= self.page.order;
                // a skip passes a message once it has been up SKIP_FRAMES frames: sub_802C0 runs once a frame,
                // clears the text flag (4) a frame after the text is complete and only then the click wait
                // (AE0670 set or skip mode), and dispatches up to the next wait; the speed only multiplies the
                // effect steps (sub_3FBD0) and wait timers.
                if fast && revealed && self.click_ms >= SKIP_FRAMES * 1000.0 / 60.0 - 1.0 {
                    self.finish_click();
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
            _ => {}
        }
        self.step_xskr(dt);

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

        // sub_40C10: the staff roll task ends once the script clears F36
        if self.staff_task && self.var("F36") == 0 {
            self.end_staff_task();
        }

        // sub_391A0, the message layer's draw callback (sub_3B9A0: layer 15), set F824 = 65 last frame
        let f824 = self.witch_f824();
        self.set_var("F824", f824);
        // jp_48AC0: the backdrop colour word of the scene, every story frame
        if self.assets.build == Build::Witch {
            self.stage.backdrop = u8::from(!crate::savedata::witch_dark_scene(self.scene_index));
        }

        // run the script. sub_802C0 dispatches once per 60 Hz frame, so under a skip each wait the script stops
        // at costs a frame (knife resolves the waits within the frame's speed steps); the next-scene seek
        // runs in one frame
        // overseas sub_40ED0: while off_3516E0 is set, sub_802C0 is not called; its last frame runs sub_7DDA0
        if std::mem::take(&mut self.seek_defer) {
            self.seek_finish(r);
            return;
        }
        if self.skip && !self.seek_node && self.wait == Wait::None && !self.ended {
            if self.dispatch_ms < 1000.0 / 60.0 - 1.0 {
                return;
            }
            self.dispatch_ms = 0.0;
        }
        let mut budget = 20000;
        while self.resume_ms <= 0.0 && self.wait == Wait::None && !self.ended && !self.hold && !self.rewinding && !self.rest_hold && budget > 0 {
            // the next-scene seek runs whole in this frame (sub_802C0's AE0568 loop)
            if !self.seek_node {
                budget -= 1;
            }
            let Some(cmd) = self.script.commands.get(self.pc).cloned() else {
                tracing::info!("scene {} ran off the end", self.scene_index);
                self.ended = true;
                break;
            };
            self.pc += 1;
            self.exec(r, &cmd);
            // script_dispatch always returns after these (loc_7FEF0), also when they leave no wait (every CLO, STCF,
            // `_NOP_`, TOMW without a movie), so the commands after them run the next frame (Switch 196 at 1980:
            // the click cuts the voice a frame before the SESP behind PGST / CLO3)
            let returns = matches!(
                cmd.name.as_str(),
                "CLO2" | "CLO3" | "CLO4" | "CLO5" | "STCF" | "NOP_" | "TOMW" | "MSAD" | "WTTM" | "SELA" | "SELD" | "SELS" | "END_" | "JUMP"
                    | "WTIN" | "SEWI"
            ) && !(crate::assets::witch()
                && (matches!(cmd.name.as_str(), "CLO3" | "CLO4" | "SELA" | "SELS")
                    || cmd.name == "WTTM" && cmd.int_or(0, 0) == 0))
                || std::mem::take(&mut self.dispatch_return);
            if self.seek_node {
                // a pass of the seek loop ends where script_dispatch returns
                if self.wait != Wait::None || returns {
                    self.seek_pass_end(r);
                }
                self.seek_clear_wait();
            } else if returns && self.wait == Wait::None {
                self.resume_ms = self.resume_ms.max(FRAME_MS * 0.75);
                break;
            }
        }
        // the pass that ended the seek: script_dispatch went on to the next wait first
        if self.seek_ended && !self.seek_node {
            self.seek_pass_end(r);
        }
        self.prefetch_images(r);
        // sub_802C0: a stage an STCH / SCH2 changed with no FADS after it is presented as the dispatch returns
        // (sub_5D3A0(0, 0): sub_2E1B0(0, 0) ends any transition)
        if std::mem::take(&mut self.stage_dirty) && !self.seek_node {
            self.commit_stage(r);
            self.trans = None;
            if self.wait == Wait::Transition {
                self.wait = Wait::None;
            }
        }
        if std::mem::take(&mut self.seek_ended) {
            if self.assets.build == knife_core::exe::Build::Overseas {
                self.seek_defer = true;
            } else {
                self.seek_finish(r);
            }
        }
        // sub_391A0 (the message layer's draw) clears the page (sub_53AB0(1)) in page mode while F823 == 0: the
        // scripts zero it right after a page's last click wait, so the text goes then, not at the next message
        if self.page_mode() && self.var("F823") == 0 && !self.page.texts.is_empty() {
            self.clear_page(true);
        }
        self.assets.trim_images(400);
    }

    /// sub_802C0 after each dispatch: sub_6F4D0(…, 24) queues the images the next 24 commands use into
    /// off_AE0990 and sub_AC420 loads them, so an STCH finds its image loaded (no stall at the command). knife reads and decodes the STCH /
    /// SCH2 images (the `_aa_bb` variant when it exists, else the base, as `cmd_stch` takes them) on worker
    /// threads and uploads them as they arrive; one not ready yet is waited for at the command. Each STCH /
    /// SCH2 layer's STEC / SNX2 / SNX3 are followed to the next FADS, whose commit builds the graded image
    /// (`build_textures`), so that build is read ahead too. A skip runs through the 24 commands within a frame
    /// or two: then the look-ahead reaches `PREFETCH_SKIP` commands.
    fn prefetch_images(&mut self, r: &Renderer) {
        self.assets.finish_prefetched(r);
        if self.seek_node {
            return;
        }
        let none = crate::grade::Bake::NONE;
        let span = if self.skip { PREFETCH_SKIP } else { 24 };
        let end = (self.pc + span).min(self.script.commands.len());
        let mut want = Vec::new();
        // each layer's image and grade (STEC mode, SNX2 / SNX3) as the commands ahead leave them
        let mut grades: HashMap<i32, (String, i32, [f32; 2])> = HashMap::new();
        let graded = |want: &mut Vec<(String, crate::grade::Bake)>, (name, mode, snx): (String, i32, [f32; 2])| {
            let bake = crate::grade::Bake::new(mode, snx[0], snx[1]);
            if bake.cpu() {
                want.push((name, bake));
            }
        };
        for c in self.script.commands.get(self.pc..end).unwrap_or_default() {
            let li = c.int_or(0, -1);
            match c.name.as_str() {
                "STCH" | "SCH2" => {
                    grades.remove(&li);
                    let Some(base) = c.arg(1).filter(|b| !b.is_empty() && !b.eq_ignore_ascii_case("BLACK") && !b.eq_ignore_ascii_case("WHITE")) else {
                        continue;
                    };
                    let variant = match (c.int(9), c.int(10)) {
                        (Some(a), Some(b)) if (a, b) != (0, 0) => Some(format!("{base}_{a:02}_{b:02}")),
                        _ => None,
                    };
                    let known = |n: &str| self.assets.image_index_of(&n.to_ascii_uppercase()).is_some();
                    let name = variant.filter(|v| known(v)).unwrap_or_else(|| base.to_string());
                    if !known(&name) {
                        continue;
                    }
                    want.push((name.clone(), none));
                    // a source layer (SCH2 arg 18) brings its grade along; the layer is new otherwise (`cmd_stch`)
                    let (mode, snx) = match c.int(18).filter(|&s| s >= 0) {
                        Some(s) => grades.get(&s).map(|g| (g.1, g.2)).unwrap_or_else(|| {
                            self.stage.layers.get(s as usize).cloned().flatten().map_or((0, [1.0; 2]), |l| (l.stec_mode, l.snx))
                        }),
                        None => (0, [1.0; 2]),
                    };
                    grades.insert(li, (name, mode, snx));
                }
                "STEC" => {
                    let mode = c.int_or(1, 0);
                    if let Some(g) = grades.get_mut(&li)
                        && (0..=82).contains(&mode)
                    {
                        g.1 = mode;
                    }
                }
                // as the exec reads them (brightness 1 + v / 255, contrast 1 + v / 100)
                "SNX2" | "SNX3" => {
                    if let Some(g) = grades.get_mut(&li) {
                        let v = c.int(1).map_or(1.0, |v| if c.name == "SNX2" { 1.0 + v as f32 / 255.0 } else { 1.0 + v as f32 * 0.01 });
                        g.2[usize::from(c.name == "SNX3")] = v;
                    }
                }
                "FADS" => {
                    for (_, g) in grades.drain() {
                        graded(&mut want, g);
                    }
                }
                // the sound effects too (`cmd_se`; a skip plays only the loops)
                "SEPL" if !self.skip || c.int_or(1, 0) == 1 => {
                    if let Some(n) = c.arg(2) {
                        self.assets.prefetch_sound(n);
                    }
                }
                "SEAC" if !self.skip => {
                    if let Some(n) = c.arg(1) {
                        self.assets.prefetch_sound(n);
                    }
                }
                _ => {}
            }
        }
        // a commit past the look-ahead: the grades as they stand so far
        for (_, g) in grades.drain() {
            graded(&mut want, g);
        }
        for (name, bake) in want {
            self.assets.prefetch(&name, bake);
        }
    }

    /// sub_60610, FADS's start: the window flags, the stage presented and backed up, the transition (rule mask
    /// sub_4B4C0 loaded outside the seek) and its flag. Outside the seek / a rewind FADS then runs once more
    /// (`fads`).
    fn fads_start(&mut self, r: &Renderer, c: &Command) {
        let arg = |i: usize| c.int(i);
        for f in ["F996", "F997", "F985"] {
            self.set_var(f, 0);
        }
        // arg 3 = 1 hides the text until the next message; 2 too when the fade lasts 2+ ms and no content fade
        // runs (config byte 99, 1 unless a whole-config copy changes it)
        // (Witch on the Holy Night's jp_60610 has no such byte: none / -1 act as 2)
        let wnd = arg(3).unwrap_or(-1);
        let two = wnd == 2 || (wnd == -1 && crate::assets::witch());
        if wnd == 1 || (two && arg(1).unwrap_or(0) >= 2 && self.page.dim_vel == 0.0) {
            self.set_var("F1103", 1);
        }
        let mode = c.int_or(0, 1);
        let dur = c.int_or(1, 0) as f32;
        let rule = c.int_or(2, 0);
        self.commit_stage(r);
        self.stage_dirty = false;
        // sub_60610 pushes a type 1 ring entry (sub_5A910(.., 1); Witch on the Holy Night's jp_60610 too), then
        // copies the committed stage into the backup (Switch: AE0460 steps in the frame AE04B8 gets 8, so a held
        // ZR passes two more entries than messages and backing-up commands alone give)
        self.push_ring_event(1);
        self.stage_backup = Some(self.stage_bytes());
        let rule_before = self.fads_rule;
        // Witch on the Holy Night's FADS (jp_60610) sets the rule wipe edge from its fifth argument (64 by default)
        if crate::assets::witch() {
            self.stage.rule_soft = soft_width(c.int_or(4, 64) as f32);
        }
        // sub_4B4C0: no rule clears the mask even in the seek; a rule loads outside it (Witch on the Holy Night's
        // jp_60610 also not under the fast skip, AE0670, or skip mode, AE0660 bit 1)
        if rule < 1 {
            self.fads_rule = -1;
        } else if !self.seek_node && !(crate::assets::witch() && (self.skip || self.skip_mode)) {
            self.fads_rule = rule - 1;
        }
        if mode == 1 || dur <= 0.0 || self.skip {
            // sub_60610 returns 0 when a rule was loaded before (mode 1: sub_4B570 >= 0), and FADS then draws its
            // capture this frame too (loc_7FD38)
            let keep = !self.seek_node && !self.rewinding && mode == 1 && rule_before >= 0;
            self.trans = keep.then(Transition::hold);
            // sub_4B740 starts the progress at 1 with a step of -1, taken at the end of sub_48AC0 after
            // sub_3C970's check: the transition flag holds the script one frame, then FADS returns (Switch:
            // 001_00_00's CLO3 .. FADS(1,0,0) + STGS(0,,255,800), the fade 7 frames after the window goes)
            if !self.seek_node && !self.rewinding {
                self.resume_ms = FRAME_MS * 1.75;
            }
        } else {
            // sub_47DD0: only mode 2 draws the old frame through the rule (sub_A48C0); mode 0 is a crossfade
            // (sub_A46F0) even with a rule loaded; it wipes through the loaded mask (7177), which Witch on the Holy
            // Night's skip mode leaves as an earlier FADS loaded it
            let rule = (mode == 2 && self.fads_rule >= 0)
                .then(|| self.assets.rule(r, &format!("RULE_{:02}", self.fads_rule + 1)))
                .flatten()
                .map(|i| i.tex.clone());
            if self.seek_node || self.rewinding {
                self.snapshot = true;
            }
            self.trans = Some(Transition { dur, t: 0.0, rule, hold: false });
            self.wait = Wait::Transition;
        }
        if !self.seek_node && !self.rewinding {
            self.fads = Some(Fads { hold: 0, started: true });
        }
    }

    /// sub_538E0, as sub_40ED0 opens the next / previous scene dialog or countdown (after an RDAS hit or a mode
    /// panel button): FADS's old-frame capture (byte_146E640) and its image-load wait count (qword_146E65C) are
    /// dropped. A FADS from the last dispatch is still in that wait (its STCH images load after the dispatch,
    /// sub_6F4D0), so the live new stage shows under the dialog, and when the script goes on FADS runs again,
    /// captures that stage and runs its transition between the same frames (Switch 010 at RDAS(66): the
    /// classroom of FADS(0,1500) is up in full the frame the dialog appears). A transition already stepping is
    /// left alone.
    pub fn drop_capture(&mut self) {
        if self.fads.take().is_some_and(|f| !f.started) && self.trans.as_ref().is_some_and(|t| t.hold) {
            self.trans = None;
        }
    }

    /// sub_6E0D0: every message pushes its type 0 ring entry as it starts (sub_5A910, not while a held rewind
    /// runs), with the text up to its end code, and is the save point when it is a page's first (F823 <= 1)
    /// outside an engine task.
    fn record_message(&mut self) {
        if self.ended {
            return;
        }
        let key = (self.scene_index, self.msg_pc, self.page.texts.len());
        if let Some(h) = self.history.back().filter(|h| h.key == key) {
            // the message a rewind / quick jump restored, replayed: its entry stays, and it is the page's
            // ring entry again (the replay's page start dropped the serials rewind_to set)
            let serial = h.serial;
            if key.2 == 1 {
                self.page_ring.0 = Some(serial);
            }
            self.page_ring.1 = Some(serial);
            return;
        }
        let mut t = self.page.texts.clone();
        t.pop();
        let mut s = self.snapshot_at(self.msg_pc, t);
        s.text = entry_text(&self.msg_text);
        let locked = self.in_movie() || self.staff_task;
        if self.var("F929") == 0 {
            let serial = self.push_ring(s.clone(), key, locked, 0);
            if key.2 == 1 {
                self.page_ring.0 = Some(serial);
            }
            self.page_ring.1 = Some(serial);
        }
        // 0x6e7a0: the save point keeps the newest scene point as its previous entry (sub_527F0); not while
        // byte_146E648 (a rewound message, until the next push) is set
        if !locked && self.var("F823") <= 1 && !self.ring_restored {
            self.save_point = Some(self.with_scene_prev(s));
        }
    }

    /// sub_6E950 (MSAD): the newest ring entry takes the grown text, and is the save point again when it
    /// qualifies.
    fn update_message_entry(&mut self) {
        let text = entry_text(&self.msg_text);
        let preview = self.snapshot_at(self.msg_pc, Vec::new()).preview;
        let Some(h) = self.history.back_mut() else { return };
        h.data.text = text;
        h.data.preview = preview;
        let (d, locked, kind) = (h.data.clone(), h.locked, h.kind);
        if !locked && !matches!(kind, 11 | 12) && self.var("F823") <= 1 && !self.ring_restored {
            self.save_point = Some(self.with_scene_prev(d));
        }
    }

    /// The next-scene seek ends (off_AE0568 = 0): its sound comes back (ATSV, SELD / SELA / SELS) or is left
    /// stopped (TOMP); the loop's after work (`seek_finish`) runs once the frame's dispatch stops.
    fn end_seek(&mut self, resume: bool) {
        self.seek_node = false;
        self.seek_ended = true;
        match self.seek_audio.take() {
            Some(sa) if resume => self.resume_sound(&sa),
            _ => {}
        }
    }

    /// The overseas seek loop (sub_802C0) steps more after each pass's script_dispatch and sub_3FBD0(10): the
    /// queued SE commands count down 64 frames (sub_65E50(64)) and every layer's motion runs on 60 frames
    /// (sub_97B80(layer, 60.0) for layers 0..39).
    fn seek_pass_end(&mut self, r: &Renderer) {
        if self.assets.build != knife_core::exe::Build::Overseas {
            return;
        }
        self.tick_se_delayed(64.0 * 1000.0 / 60.0);
        for li in 0..self.stage.layers.len() {
            let cur = self.motion_cur(li);
            let Some(m) = self.stage.layers[li].as_mut().and_then(|l| l.motion.as_mut()) else { continue };
            let f = self.motions.advance(m, 1000.0, &cur);
            self.apply_motion(r, li, f);
        }
    }

    /// Each pass of sub_802C0's seek loop starts with every wait word zeroed (AE04B8 flags, AE04C0, AE04A8,
    /// the timer AE04B0, AE06E0, AE0A20, the STCF bitset AE08A8): a wait the script stops at ends without its
    /// effect finishing and without the click wait's exit (sub_40ED0 LABEL_307), and nothing is stepped
    /// between passes but the engine tasks (sub_3FBD0(10)).
    fn seek_clear_wait(&mut self) {
        if matches!(self.wait, Wait::Time { .. } | Wait::Click | Wait::Transition | Wait::Layer(..) | Wait::Voice | Wait::Message { .. }) {
            self.wait = Wait::None;
            self.page.waiting = false;
            self.click_ms = 0.0;
        }
    }

    /// After the seek loop (sub_802C0, once script_dispatch returns at the first wait past the end, which then
    /// stays): sub_32C10 zeroes every layer's position and alpha tween offsets, so those land while their
    /// progress runs on (rotation keeps its offset); the stage is presented without a transition
    /// (sub_5D3A0(0, -1)), which lands each zoom tween too.
    fn seek_finish(&mut self, r: &Renderer) {
        for l in self.stage.layers.iter_mut().flatten() {
            l.x.diff = 0.0;
            l.y.diff = 0.0;
            l.alpha.diff = 0.0;
        }
        self.commit_stage(r);
        self.trans = None;
        if self.wait == Wait::Transition {
            self.wait = Wait::None;
        }
    }

    /// The script ends (END_ / RTM_ / RDED in a replay): a next-scene seek ends with its sound resumed, then
    /// sub_5F200 stops every SE channel, the voice and, unless RTMN / RTM2 set byte_146E64D (cleared here),
    /// the BGM (sub_9C650: out at 416.67 a frame, at once), the screen shake and the FCLN effect.
    fn script_end(&mut self) {
        if self.seek_node {
            self.end_seek(true);
        }
        // sub_5F200 also clears the queued SE ops, the loop words, the volume words (-1) and the pans
        self.se_delayed.clear();
        self.sound.stop_se(-1);
        self.sound.se_word = [-1; 20];
        self.sound.se_loop = [false; 20];
        self.sound.se_pan = [0; 20];
        self.sound.stop_voice();
        if !std::mem::take(&mut self.keep_bgm_at_end) {
            self.sound.stop_bgm_rate(416.67);
        }
        self.shake.stop();
        self.focus.start(0, 0);
        self.ended = true;
    }

    /// sub_5AFC0: the message buffer is laid out again and shown in full (reveal counters at 768).
    fn redisplay_message(&mut self) {
        if self.page.texts.is_empty() && !self.msg_text.is_empty() {
            let t = message_end(&self.msg_text).0;
            self.show_message(&t, false);
        }
        self.page.revealed = self.page.order as f32;
    }

    /// sub_527F0(entry, idx + 1, ring[idx]): the record and the scene ring's top as its previous entry.
    fn with_scene_prev(&self, d: crate::save::SaveData) -> crate::save::SaveData {
        crate::save::SaveData { prev: self.scene_ring.last().map(|p| Box::new(p.data.clone())), ..d }
    }

    /// sub_5A910: a new rewind-ring entry; the oldest of the 800 goes, with the backlog pages that leave with it.
    fn push_ring(&mut self, data: crate::save::SaveData, key: (usize, usize, usize), locked: bool, kind: u16) -> u64 {
        if self.history.len() >= HISTORY {
            self.history.pop_front();
            // the backlog lists the ring (sub_27EB0): pages that left it go too
            let oldest = self.history.front().map_or(0, |h| h.serial);
            self.backlog_pages.retain(|p| p.last.is_some_and(|l| l >= oldest));
        }
        if std::mem::take(&mut self.ring_reuse) {
            self.history.pop_back();
        }
        let serial = self.ring_serial;
        self.ring_serial += 1;
        self.ring_restored = false;
        self.history.push_back(RingEntry { data, node: self.node_seq, key, serial, locked, kind });
        serial
    }

    /// A non-message ring entry (type 1 / 9) at the command now running: the state before it, resuming at it.
    /// sub_5A910 pushes those only while F929 and F1102 are 0. The replay of the command a rewind stopped at
    /// does not push it again.
    fn push_ring_event(&mut self, kind: u16) {
        // (also not after a message ending in `@n` until the next one starts, byte_146E644)
        if self.var("F929") != 0 || self.var("F1102") != 0 || self.ring_hold || self.ended {
            return;
        }
        let pc = self.pc.saturating_sub(1);
        let key = (self.scene_index, pc, usize::MAX - 1);
        if self.history.back().is_some_and(|h| h.key == key) {
            return;
        }
        let mut s = self.snapshot_at(pc, self.page.texts.clone());
        s.kind = kind;
        s.voice = None;
        s.preview.clear();
        let locked = self.in_movie() || self.staff_task;
        self.push_ring(s, key, locked, kind);
    }

    pub fn can_rewind(&self) -> bool {
        self.history.len() >= 2
    }

    /// The rewind takes its next step. sub_814C0 steps once a frame when no wait is pending; the script does
    /// not run while rewinding, so the restored entries never show their message (only the first step waits
    /// for the current one). A FADS's capture hold sets no transition flag (AE04B8 8 comes with the start), so
    /// it does not stop a step (Witch on the Holy Night: the frame between two steps can reach a FADS).
    pub fn settled(&self) -> bool {
        (self.wait == Wait::Click || self.rewinding) && self.trans.as_ref().is_none_or(|t| t.hold)
    }

    /// The held rewind stopped: the text shows again (sub_391A0 draws none while off_AE05B8 is set), and a
    /// restored choice shows the page under it at once.
    pub fn end_rewind(&mut self) {
        self.rewinding = false;
        if self.wait == Wait::Choice && !self.page.texts.is_empty() {
            self.page.visible = true;
        }
    }

    /// Back one page (sub_814C0); false when there was nothing earlier to go back to (sub_3B410(0)).
    pub fn rewind_step(&mut self, r: &Renderer) -> bool {
        if self.history.is_empty() {
            return false;
        }
        if !self.can_rewind() {
            // the newest entry is the only one: it comes back itself unless it is a message's (Tsukihime: type 0,
            // 12 or 13; Witch on the Holy Night: below 1), and nothing else
            let kind = self.history[0].kind;
            let own = if self.assets.build == Build::Witch { kind >= 1 } else { !matches!(kind, 0 | 12 | 13) };
            if own {
                self.rewind_to(r, 0, Back::Rewind);
            }
            return false;
        }
        // sub_814C0: back to the newest earlier entry of a page's first message (type 0, F823 <= 1 when it was
        // recorded) taken outside a story movie / the staff roll (the entries between are undone on the way),
        // or the ring's oldest entry when there is none
        // (Witch on the Holy Night's jp_814C0 also passes over entries whose command, copied up to its `;` (256
        // bytes at most), holds `@e`: a page's first message that ends the page)
        let cur = self.history.len() - 1;
        let target = self.rewind_target(cur).unwrap_or(0);
        self.rewind_to(r, target, Back::Rewind);
        true
    }

    /// The newest page start before ring entry `before` that sub_814C0 steps back to.
    fn rewind_target(&mut self, before: usize) -> Option<usize> {
        for i in (0..before).rev() {
            let h = &self.history[i];
            if h.kind != 0 || h.locked || entry_f823(&h.data) > 1 {
                continue;
            }
            let (scene, pc) = (h.data.scene, h.data.pc);
            if self.assets.build == Build::Witch && self.ends_page(scene, pc) {
                continue;
            }
            return Some(i);
        }
        None
    }

    /// The command at `pc` of scene `scene` holds `@e` within the 256 bytes jp_814C0 copies of it.
    fn ends_page(&mut self, scene: usize, pc: usize) -> bool {
        let script = if scene == self.scene_index { Some(self.script.clone()) } else { self.assets.scene(scene) };
        script.and_then(|s| s.commands.get(pc).map(|c| format!("_{}({})", c.name, c.args.join(",")))).is_some_and(|t| {
            let mut n = t.len().min(256);
            while !t.is_char_boundary(n) {
                n -= 1;
            }
            t[..n].contains("@e")
        })
    }

    /// The mode panel's previous scene button is lit (sub_4E170 0x4e840): during a choice with two scene
    /// points before the newest, otherwise with one when two messages have shown since it, else with two.
    pub fn can_prev_scene(&self) -> bool {
        let idx = self.scene_ring.len() as i32 - 1;
        if self.in_choice() {
            idx > 1
        } else {
            idx > 0 && (self.scene_msgs < 1 || idx > 1)
        }
    }

    /// sub_53980(-1): the previous scene dialog's Yes starts the countdown (sub_40ED0 0x41308).
    pub fn prev_scene_allowed(&self) -> bool {
        let idx = self.scene_ring.len() as i32 - 1;
        if self.in_choice() { idx > 0 } else { idx >= 0 }
    }

    /// sub_53980(1): the next scene dialog's Yes starts the countdown (F998 0, no choice up).
    pub fn next_scene_allowed(&self) -> bool {
        self.var("F998") == 0 && !self.in_choice()
    }

    /// Previous scene (sub_53950(-1) -> sub_81A50): with no engine task during a choice, the scene point
    /// before the newest; otherwise the newest when two messages have shown since it (qword_146E66C < 1),
    /// else the one before it. sub_5D950 restores it and the ring drops it and every newer point.
    pub fn prev_scene(&mut self, r: &Renderer) -> bool {
        let idx = self.scene_ring.len() as i32 - 1;
        let tasks = self.in_movie() || self.staff_task;
        let target = if !tasks && self.in_choice() {
            if idx < 2 {
                return false;
            }
            idx - 1
        } else if idx >= 1 && self.scene_msgs < 1 {
            idx
        } else if idx >= 2 {
            idx - 1
        } else {
            return false;
        };
        self.load_scene_point(r, target as usize);
        true
    }

    /// sub_5D950(n): the scene point is restored like a load (sub_5EF10(1) first stops all sound), from its
    /// page-ring entry while the ring still holds it (sub_5BAF0 on that slot), else from a ring started over
    /// with it; the ring keeps the points before it and the message count is 2 again.
    fn load_scene_point(&mut self, r: &Renderer, n: usize) {
        let point = self.scene_ring[n].clone();
        self.scene_ring.truncate(n + 1);
        match point.serial.and_then(|s| self.history.iter().position(|h| h.serial == s)) {
            Some(i) => self.rewind_to(r, i, Back::Point),
            None => {
                self.history.clear();
                self.ring_reuse = false;
                self.backlog_pages.clear();
                self.page_ring = (None, None);
                let d = point.data.clone();
                let key = (d.scene, d.pc, usize::MAX - 1);
                let serial = self.push_ring(d.clone(), key, false, d.kind);
                self.scene_ring[n].serial = Some(serial);
                self.restore_with(r, &d, false);
                self.restore_save_point(&d, false, d.kind);
            }
        }
        self.scene_ring.truncate(n);
        self.scene_msgs = 2;
    }

    /// Backlog quick jump (sub_28A90 state 7 -> sub_28D50): replay from the first message of backlog page
    /// `back` (0 = the newest), as a rewind does (sub_5BAF0). False when its entry has left the ring.
    pub fn backlog_jump(&mut self, r: &Renderer, back: usize) -> bool {
        let Some(page) = self.backlog_pages.len().checked_sub(back + 1).map(|i| &self.backlog_pages[i]) else {
            return false;
        };
        let Some(i) = page.first.and_then(|s| self.history.iter().position(|h| h.serial == s)) else {
            return false;
        };
        self.rewind_to(r, i, Back::Jump);
        true
    }

    /// Restore ring entry `i` (sub_5BAF0), dropping the newer entries. The ATSV entries undone on the way
    /// drop their scene points (cases 6 / 7; a rewind also zeroes the message count, sub_28D50 does not), and
    /// the entry is the save point again when it qualifies (LABEL_137).
    fn rewind_to(&mut self, r: &Renderer, i: usize, how: Back) {
        let RingEntry { data: d, node: tag, serial, locked, kind, .. } = self.history[i].clone();
        // (the current message's slot as it was: its text and how far it is out)
        let slot = self.page.texts.last().filter(|_| !self.slot_flip).cloned().map(|t| (t, self.page.revealed - self.page.last_start.1 as f32));
        while how != Back::Point && self.scene_ring.last().is_some_and(|p| p.serial.is_some_and(|s| s > serial)) {
            self.scene_ring.pop();
            if how == Back::Rewind {
                self.scene_msgs = 0;
            }
        }
        self.history.truncate(i + 1);
        // sub_5BAF0 sets AE04D0 to the slot after the restored one
        self.ring_reuse = false;
        self.node_seq = tag;
        self.seek_node = false;
        self.restore_with(r, &d, how != Back::Point);
        // the quick jump's window comes back with the replayed message
        if how == Back::Jump {
            self.page.visible = false;
            self.page.alpha = 0.0;
        }
        self.restore_save_point(&d, locked, kind);
        // sub_5BAF0 case 0 with a2 != 0 (the ZL rewind) sets byte_146E648 and lays the message out
        if how == Back::Rewind && kind == 0 {
            self.ring_restored = true;
            self.lay_out_rewound(&d, slot);
        }
        // a rewind that ends on an entry without a message (only the ring's oldest, a new game's first stage
        // command in START_SCRIPT) shows no window until the next message: Switch EN, rewound from the third
        // page of a new game, shows START_SCRIPT's white and the opening fade without the window
        if how == Back::Rewind && kind != 0 {
            self.page.visible = false;
            self.page.alpha = 0.0;
        }
        // the backlog shares the ring: pages from this entry on are gone, and the page around it is current again
        self.backlog_pages.retain(|p| p.last.is_some_and(|l| l < serial));
        // (its first message: the newest page start up to here, unless that belongs to a kept page)
        let kept = self.backlog_pages.last().and_then(|p| p.last);
        let start = self.history.iter().take(i + 1).rev().find(|h| h.page_start()).map(|h| h.serial);
        self.page_ring = (start.filter(|&s| kept.is_none_or(|l| s > l)), Some(serial));
    }

    /// sub_5BAF0 case 0 with a2 != 0: the entry's message is laid out in full at once (Witch on the Holy Night: at
    /// head 0 unless its slot holds that text already) (sub_53CF0 on its script
    /// text) with no voice (AE04C8 / AE08B0 = -1; the replay voice AE0790 stays the entry's), and the script
    /// resumes past it (+16). Without `@n` it waits for a click (AE04B8 0x500, 0x140 for `@e` or a full
    /// page), which the next held frame's sub_814C0 clears again, so a rewind held past its step goes on into
    /// the next message once released; an `@n` message goes on at once and holds the next ring push
    /// (byte_146E644).
    fn lay_out_rewound(&mut self, d: &crate::save::SaveData, slot: Option<(String, f32)>) {
        let Some(c) = self.script.commands.get(self.pc).filter(|c| matches!(c.op, Op::Message)).cloned() else {
            return;
        };
        let raw = c.args.first().map(String::as_str).unwrap_or("");
        let (_, code) = raw_message(raw, "");
        let (text, _) = message_end(&self.resolve_text(raw));
        // (sub_3B2B0(0, 1): the laid-out message counts as read, dword_572B64)
        self.line_read = true;
        self.pc += 1;
        self.show_message(&message_end(raw).0, code != Some(b'n'));
        crate::text_stream::message(&text, (self.scene_index as u64) << 32 | self.msg_pc as u64);
        // (Witch on the Holy Night's jp_5BAF0 zeroes the head and tail, so the message types out again once the
        // rewind is released; jp_AB4D0 leaves a slot that holds the same text as it was (jp_A7790 returns before
        // laying it out) and jp_AB710 takes the slot's progress back as the head, unless a click passed since the
        // last message, off_6327C0 flipping to the cleared slot)
        self.page.revealed = if self.assets.build != Build::Witch {
            self.page.order as f32
        } else {
            let start = self.page.last_start.1 as f32;
            let kept = slot.filter(|(t, _)| self.page.texts.last() == Some(t)).map_or(0.0, |(_, p)| p.max(0.0));
            (start + kept).min(self.page.order as f32)
        };
        self.ring_hold = code == Some(b'n');
        self.pending_voice = None;
        self.voice_to_play = None;
        self.last_voice = d.voice.clone();
    }

    /// A rewind starts (off_AE05B8 0 -> 1): the WTTM count (AE04A8) goes; ZL and the mode panel's button
    /// (`colours`: sub_40ED0 LABEL_307, Witch on the Holy Night's LABEL_302) also reset the 11 per-message
    /// colour words, the drag gesture and TeaTime's ZL do not.
    pub fn start_rewind(&mut self, colours: bool) {
        if matches!(self.wait, Wait::Time { .. }) {
            self.wait = Wait::None;
        }
        if colours {
            self.reset_colour_words();
        }
    }

    /// The 11 per-message colour words F800.. / F811.. (hi / lo byte; each message sets its own as it starts)
    /// take `line_reset_colour`: as a rewind starts, as the config closes (sub_2C500; Witch on the Holy
    /// Night's sub_D700 state 8) and on Witch on the Holy Night's language row (sub_D700 case 4). Nothing else
    /// resets them, a click neither.
    pub fn reset_colour_words(&mut self) {
        let colour = i32::from(self.line_reset_colour());
        for i in 0..11 {
            self.set_var(&format!("F{}", 800 + i), colour >> 8);
            self.set_var(&format!("F{}", 811 + i), colour & 0xFF);
        }
    }

    /// jp_391A0 draws the page's earlier messages in colour mode 1 with the read colour while the current
    /// message is read, and writes it into their words (31200 in a bubble), so they keep it under a later unread
    /// message of the page.
    fn latch_read_colours(&mut self) {
        if self.assets.build != Build::Witch || self.settings.text_colour != 1 || !self.line_read {
            return;
        }
        if !self.page.visible || self.rewinding {
            return;
        }
        let colour = if self.witch_bubble_text() { 31200 } else { 0xDC6D };
        for i in 0..self.page.msgs.len().saturating_sub(1).min(11) {
            self.set_var(&format!("F{}", 800 + i), colour >> 8);
            self.set_var(&format!("F{}", 811 + i), colour & 0xFF);
        }
    }

    /// sub_814C0 starts every held frame by clearing the click wait (AE04B8 0x500) a step left.
    pub fn rewind_frame(&mut self) {
        if self.wait == Wait::Click {
            self.wait = Wait::None;
            self.page.waiting = false;
        }
    }

    /// sub_5BAF0 LABEL_137: a restored entry recorded outside an engine task, not a choice (11 / 12), at a
    /// page's first message (F823 <= 1) becomes the save point, with the scene ring's top as its previous
    /// entry; otherwise the save point stays.
    fn restore_save_point(&mut self, d: &crate::save::SaveData, locked: bool, kind: u16) {
        if !locked && !matches!(kind, 11 | 12) && d.choice.is_none() && entry_f823(d) <= 1 {
            self.save_point = Some(self.with_scene_prev(d.clone()));
        }
    }

    /// sub_40C60: F36 = 0, off_AE0650 and the auto / skip bits come back.
    fn end_staff_task(&mut self) {
        if std::mem::take(&mut self.staff_task) {
            self.set_var("F36", 0);
            self.end_mode_task();
        }
    }

    /// sub_402A0 / sub_40B70: off_AE0650 is kept in dword_AFFC9C and set, auto and skip mode are kept in
    /// word_AFFC98 and turned off.
    fn begin_mode_task(&mut self) {
        self.task_lock = (true, self.task_lock.0);
        self.task_modes = (self.auto, self.skip_mode);
        self.auto = false;
        self.skip_mode = false;
    }

    /// sub_40A00 / sub_40C60: off_AE0650 = dword_AFFC9C, off_AE0660's auto and skip bits = word_AFFC98.
    fn end_mode_task(&mut self) {
        self.task_lock.0 = self.task_lock.1;
        (self.auto, self.skip_mode) = self.task_modes;
    }

    /// off_AE0650 (sub_40ED0 clears skip mode every frame while it is set).
    pub fn mode_task(&self) -> bool {
        self.task_lock.0
    }

    /// Whether backlog page `back` (0 = the newest) can be quick-jumped to (sub_28A90): its first ring entry is
    /// still there and was not recorded during a story movie or the staff roll task.
    pub fn backlog_can_jump(&self, back: usize) -> bool {
        let Some(page) = self.backlog_pages.len().checked_sub(back + 1).map(|i| &self.backlog_pages[i]) else {
            return false;
        };
        page.first.and_then(|s| self.history.iter().find(|h| h.serial == s)).is_some_and(|h| !h.locked)
    }

    /// Mode panel next scene.
    pub fn next_scene(&mut self) {
        self.seek_node = true;
        self.seek_clear_wait();
        // overseas: sub_2F1D0 marks the screen wobble's phase as wrapped (word_AFEB94), so it stops wobbling
        if self.assets.build == knife_core::exe::Build::Overseas {
            self.ghost.wrapped = true;
        }
        // sub_802C0: sub_5EF10(0) stops the BGM and every SE channel at once, keeping their words
        self.seek_audio = Some(SoundState {
            bgm: self.sound.bgm_state(),
            loops: self.sound.looping_effects().into_iter().map(|(c, n, v)| (c, (n, v))).collect(),
        });
        self.sound.stop_bgm(0.0);
        self.sound.stop_se(-1);
    }

    /// sub_5D7E0: the BGM word plays at once (a track already playing keeps going, sub_9C470) at its volume
    /// (sub_9C870), each channel with a looping SE restarts it at once, the others stop.
    fn resume_sound(&mut self, st: &SoundState) {
        match st.bgm {
            Some((id, vol)) => self.play_bgm(id, vol, 0.0),
            None => self.sound.stop_bgm(0.0),
        }
        self.sound.stop_se(-1);
        for (&ch, (name, vol)) in &st.loops {
            // sub_5D7E0 rebinds the channel from its saved speaker byte, 0 for every looping SEPL, and sets the
            // pan word (sub_9CE60)
            self.sound.bind_speaker(ch, None);
            let pan = self.sound.se_pan.get(ch as usize).map_or(0.0, |&p| f32::from(p) / 1000.0);
            if let Some(pcm) = self.assets.sound_effect(name) {
                self.sound.play_se(ch, name, pcm, *vol, 0.0, true, pan);
            }
        }
        // Witch on the Holy Night's jp_5D7E0 ends by making the alpha the STGS start word of every layer whose
        // image index is >= 1, so a later commit has nothing left to fade
        if crate::assets::witch() {
            for l in self.stage.layers.iter_mut().flatten() {
                if self.assets.image_index_of(&l.name).is_some_and(|i| i >= 1) {
                    l.wfade[0] = (l.alpha.to * 255.0).round() as i16;
                }
            }
        }
    }

    /// Category volumes changed: the speaker rows' voice volumes (0 for a muted row or with voices off, the
    /// type 2 SE channels' category in sub_9B7F0), then every playing sound.
    pub fn apply_volumes(&mut self) {
        let st = &self.settings;
        let gains = (0..19)
            .map(|r| if st.voice_on && st.char_mute >> r & 1 == 0 { st.char_voice.get(r).copied().unwrap_or(1.0) } else { 0.0 })
            .collect();
        self.sound.set_speaker_gains(gains);
        self.sound.apply_volumes();
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
                // this one becomes current, skip stops when it is unread), then refreshes the backup with a type
                // 9 ring entry when the stage changed (0x78efc, before the `_ZY` check)
                self.note_read(&c.name);
                self.refresh_backup();
                return;
            }
            Op::Message => {
                self.menu_lock = false;
                // `_ZM` takes the `_Z` path first (0x78efc): backup refresh and type 9 entry
                self.refresh_backup();
                // sub_6E0D0: a scene point's message count goes down, the `@n` push hold ends
                if self.scene_msgs >= 1 {
                    self.scene_msgs -= 1;
                }
                self.ring_hold = false;
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
                // Witch on the Holy Night's jp_6E0D0 ends only F1103 == 1 (CLO5's 2 stays)
                if !crate::assets::witch() || self.var("F1103") == 1 {
                    self.set_var("F1103", 0);
                }
                let raw = c.args.first().map(String::as_str).unwrap_or("");
                self.note_read(&c.name);
                // sub_6E0D0: VPLY's voice plays unless skipping (a fast skip into unread text with
                // read-only skip has stopped by now, so it plays there too) and becomes the replay voice
                let voiced = match self.voice_to_play.take() {
                    Some(n) if std::mem::take(&mut self.log_voice) => {
                        // the voices still stop (sub_9C3B0)
                        if !self.skip && !self.seek_node && self.settings.voice_on && self.speaker_voice_on(&n) {
                            self.sound.stop_voice();
                            self.last_voice = Some(n);
                        }
                        false
                    }
                    Some(n) => {
                        self.play_voice(&n, true);
                        true
                    }
                    None => false,
                };
                let text = self.resolve_text(raw);
                // sub_391A0 draws the last F823 - 1 history messages above this one: the page starts over at
                // a message with F823 <= 1 (the scripts count F823 up before each message, zero it at a page end)
                // (only in page mode, F822 == 1; otherwise every message has the window to itself)
                if self.var("F823") <= 1 || !self.page_mode() {
                    self.clear_page(true);
                }
                // sub_6E0D0: in page mode message n = min(F823, 11) of the page records its colour and the F31 /
                // F32 text scale in F799+n / F810+n / F824+n / F835+n
                if self.page_mode() {
                    let n = self.var("F823").clamp(0, 11);
                    let (f31, f32) = (self.var("F031"), self.var("F032"));
                    let colour = i32::from(self.msg_colour());
                    for (base, v) in [(799, colour >> 8), (810, colour & 0xFF), (824, f31), (835, f32)] {
                        self.set_var(&format!("F{}", base + n), v);
                    }
                }
                let (text, wait) = message_end(&text);
                self.show_message(&message_end(raw).0, wait);
                // sub_A7790 without 0x20000: the slot's 2 frame delay (none in the seek, 0x1000) costs the reveal
                // the next frame's steps; MSAD keeps the progress and has none. Witch on the Holy Night's jp_6E0D0
                // sets 0x1000 for CLO5's F1103 == 2 instead, so its seek keeps the delay
                let no_hold = if crate::assets::witch() { self.var("F1103") == 2 } else { self.seek_node };
                if !no_hold {
                    self.reveal_hold = self.frame_no + 1;
                }
                crate::text_stream::message(&text, (self.scene_index as u64) << 32 | self.msg_pc as u64);
                let (raw_text, code) = raw_message(raw, "");
                if !wait {
                    // (sub_6E0D0 sets bit 4 for the voice it played unless the code is `@n`)
                    self.wait = Wait::Message { voice: voiced && code != Some(b'n') };
                }
                self.msg_text = raw_text;
                self.record_message();
                self.ring_hold = code == Some(b'n');
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
            // IFSC(, label) (0x77b2c): on through a gallery start (sub_47B00), else to the label
            "IFSC" if !self.archive => {
                if let Some(l) = c.arg(1) {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            // IFP4(, label) (0x79868) jumps to its label on the Switch (sub_59500), skipping the PS4 block;
            // IFNX goes to the default case (a no-op), so the Switch block after it runs
            "IFP4" => {
                if let Some(l) = c.arg(1).filter(|l| !l.is_empty()) {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            // IFLA(language, label) (script_dispatch 'I' "FLA"; sub_75950 replays it for a rewind): on when the
            // game language (sub_2DE70) is the argument, else to the label
            "IFLA" => {
                if self.value(c.arg(0)) != self.assets.language() as i32
                    && let Some(l) = c.arg(1).filter(|l| !l.is_empty())
                {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            // IFUR(_, label) (0x79c00, both builds): on while sub_926F0(1) finds an ending node of 533..556 not
            // completed (G300 + n, RDED), else to the label; the argument is not read. 519_QA_25 opens with
            // IFUR(1, COMP_CHECK_OK, ..) and an RTM_ after it
            "IFUR" => {
                if (533..=556).all(|n| get_bit(&self.endings, n))
                    && let Some(l) = c.arg(1).filter(|l| !l.is_empty())
                {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            "IFNX" | "IFSC" => {}
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
            // END_ (0x7f408, result 1) / RTM_ (0x789c8 -> loc_7F9A4, result 2 like a replay's RDED; RTMN first
            // sets byte_146E64D, RTM2 only sets it): the script ends (sub_5F200 at 0x7f5d4 / 0x7f9f8). RTM_ also
            // zeroes stage word 0x19A4, which nothing sets
            "RTM2" => self.keep_bgm_at_end = true,
            "END_" | "RTM_" | "RTMN" => {
                if name == "RTMN" {
                    self.keep_bgm_at_end = true;
                }
                self.replay_done = name != "END_";
                tracing::info!("{name}: scene finished");
                self.script_end();
            }
            // 0x7f4ac stores max(1, ms * 3 / 50) whole 60 Hz frames (AE04A8), so even WTTM(0) waits a frame.
            // Witch on the Holy Night (0x58520): WTTM(0) does nothing and the dispatch goes on; a third argument
            // takes off the frames since the last load or rewind (word_E66AD6, at most 1200), so it only waits
            // out what is left of its time after one.
            "WTTM" => {
                let ms = c.int_or(0, 0);
                if crate::assets::witch() && ms == 0 {
                    return;
                }
                let mut frames = ms * 3 / 50;
                if crate::assets::witch() && c.arg(2).is_some() {
                    frames = (frames - (self.since_restore * 0.06) as i32).max(1);
                }
                self.wait_no_click = c.int_or(1, 0) != 0;
                self.wait = Wait::Time {
                    left: frames.max(1) as f32 * 50.0 / 3.0,
                    click: c.int_or(1, 0) == 0,
                };
            }
            // WTIN(ms, ms_fast) (Witch on the Holy Night, 0x5ee7c): WTTM's backup refresh, then a clickable wait of
            // max(1, ms * 3 / 50) frames, ms_fast in auto mode or while the mode icon shows (sub_1D640: off_AE0660
            // bit 0 / dword_AFEC9C: skip mode, fast forward, rewind)
            "WTIN" => {
                let fast = self.auto || self.skip_mode || self.skip;
                let frames = (c.int_or(usize::from(fast), 0) * 3 / 50).max(1);
                self.wait_no_click = false;
                self.wait = Wait::Time { left: frames as f32 * 50.0 / 3.0, click: true };
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
                self.clear_page(true);
                self.page.visible = false;
                self.page.alpha = 0.0;
                self.set_var("F1103", 1);
            }
            // Witch on the Holy Night's dispatcher has no CLO3 (default case)
            "CLO3" if crate::assets::witch() => {}
            "CLO3" => {
                // a shown window fades out in 64 ms; the page stays (drawn again from the next message)
                if self.page.visible {
                    self.page.visible = false;
                    self.page.quick = true;
                    self.set_var("F1103", 1);
                }
            }
            // Witch on the Holy Night's CLO4 (0x58294): as below, but a negative ms leaves a running fade-out
            // alone, and the dispatch goes on
            "CLO4" if crate::assets::witch() => {
                let ms = c.int_or(0, 0) as f32;
                let p = &mut self.page;
                if self.skip || self.seek_node || ms <= 0.0 {
                    if !(ms < 0.0 && p.dim_vel > 0.0) {
                        p.dim = 1.0;
                        p.dim_vel = 0.0;
                    }
                } else if p.dim_vel < 0.0 || p.dim == 0.0 {
                    p.dim = 0.0;
                    p.dim_vel = 1.0 / ms;
                } else {
                    p.dim = 1.0;
                    p.dim_vel = 0.0;
                }
            }
            // CLO5(n) (Witch on the Holy Night, 0x5f190): n 0 / 2 (or none) hide the text until a CLO5(1 / 3) or
            // CLO6: F1103 = 2, which a message start keeps (it ends only 1), with the text head, its fade tail
            // and the slot progress at 99999; n 1 / 3 turn a set F1103 into 1, so the next message shows. n 0 /
            // 1 also end a running next-scene seek (AE0568 = 0, without sub_5D7E0's sound resume) with a
            // 10 frame wait (AE04A8, its click flag as the last WTTM left it).
            "CLO5" if crate::assets::witch() => {
                let n = c.int_or(0, 0);
                if n & !2 == 0 {
                    self.set_var("F1103", 2);
                    self.page.revealed = 99999.0_f32.min(self.page.order as f32);
                } else if self.var("F1103") != 0 {
                    self.set_var("F1103", 1);
                }
                if n & !1 != 2 && self.seek_node {
                    self.end_seek(false);
                    self.wait = Wait::Time { left: 10.0 * 50.0 / 3.0, click: !self.wait_no_click };
                }
            }
            // CLO6(ms) (Witch on the Holy Night, 0x588d4) is Tsukihime's CLO5, F1103 == 1 only restarting the
            // fade; the dispatch goes on
            "CLO6" => {
                let ms = c.int_or(0, 0) as f32;
                let f1103 = self.var("F1103") == 1;
                let p = &mut self.page;
                if self.skip || self.seek_node || ms <= 0.0 || (p.dim_vel <= 0.0 && p.dim < 1.0 && !f1103) {
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
                // `_MSAD()` (36 in the scripts, closing `@n` chains) appends nothing but still ends like any MSAD
                // without a code: sub_6E950 stops its copy at the `)` and sets the click wait (0x500)
                let a = c.args.first().map(String::as_str).unwrap_or("");
                // sub_6E950: the push hold ends, the raw text grows (a trailing `@k` dropped), the newest
                // ring entry takes it, and an `@n` holds the pushes again
                self.ring_hold = false;
                let prev = std::mem::take(&mut self.msg_text);
                let base = prev.strip_suffix("@k").unwrap_or(&prev).to_string();
                let (raw_text, code) = raw_message(a, &base);
                self.msg_text = raw_text;
                let (text, wait) = message_end(&self.resolve_text(a));
                self.append_message(&message_end(a).0, wait);
                if !wait {
                    // sub_6E950 sets bit 1 but plays no voice
                    self.wait = Wait::Message { voice: false };
                }
                // the text hooker gets the addition (the message itself went out already)
                if !text.is_empty() {
                    crate::text_stream::message(&text, (self.scene_index as u64) << 32 | self.pc as u64);
                }
                self.update_message_entry();
                self.ring_hold = code == Some(b'n');
            }

            // choices
            "SELR" => {
                // 0x78a6c: the group's first option pushes a type 13 entry and adds 1 to C847 (when F823 >= 1);
                // Witch on the Holy Night's SELR (0x5aa70) only stores the option, with no disabled flag
                if self.sel_opts.is_empty() && !crate::assets::witch() {
                    self.push_ring_event(13);
                    if self.var("F823") >= 1 {
                        let v = self.var("C847") + 1;
                        self.set_var("C847", v);
                    }
                }
                let value = c.int_or(0, self.sel_opts.len() as i32);
                // the option keeps the script's text; its `$` reference resolves where it is laid out
                let text = c.arg(1).unwrap_or("").to_string();
                let disabled = c.int_or(2, 0) == 1 && !crate::assets::witch();
                self.cmd_selr(value, &text, disabled);
            }
            "TTCT" if crate::assets::witch() => self.cmd_ttct(c.int(0)),
            // Witch on the Holy Night's dispatcher has no SELA / SELS (default case)
            "SELA" | "SELS" if crate::assets::witch() => {}
            "SELD" | "SELA" | "SELS" | "SEWI" => {
                // 0x7f6d8 / sub_92800 / sub_92930 end a next-scene seek with the sound resumed
                if self.seek_node {
                    self.end_seek(true);
                }
                let var = c.arg(0).unwrap_or("_D000").to_string();
                self.cmd_select(name, var);
            }

            // stage
            "STCH" | "SCH2" | "STCG" => self.cmd_stch(r, c),
            "CKDP" => self.cmd_ckdp(r, c),
            "CKTT" => self.cmd_cktt(c),
            "STCC" => {
                // sub_62140: no argument clears layer 0, a layer above 39 nothing, a negative one all of them
                let l = c.int_or(0, 0);
                if l <= 39 {
                    let range = if l < 0 { 0..crate::stage::LAYERS } else { l as usize..l as usize + 1 };
                    for i in range {
                        self.stage.kept[i] = Some(self.kept_of(i));
                        // the render rule (+280) goes at once; the image leaves the render at the commit
                        self.stage.rules[i] = None;
                    }
                    self.stage.clear(l);
                    // word 0x7CA: the dispatch presents the stage as it returns
                    self.stage_dirty = true;
                }
            }
            // SACL(layer = 0, on = 0) (inline 0x7af80): stage byte +0x54 bit 0x40, drawn from the next commit
            "SACL" => {
                let on = c.int_or(1, 0) != 0;
                let i = c.int_or(0, 0);
                if let Some(l) = self.stage.get(i) {
                    l.staged.absolute = Some(on);
                } else if let Some(k) = usize::try_from(i).ok().and_then(|i| self.stage.kept.get_mut(i)) {
                    // an empty slot keeps the bit for its record (STCH clears it)
                    match k {
                        Some(k) => k.absolute = on,
                        None if on => *k = Some(crate::save::Kept { neg_anchor: (0, 0), blend: 0xff, stcc: false, absolute: true }),
                        None => {}
                    }
                }
            }
            // SBAS(layer = 0, base = -1) (inline 0x7afec): stage word 0x93C / 2 + layer, drawn from the next commit
            "SBAS" => {
                let b = c.int_or(1, -1);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.staged.base = Some(usize::try_from(b).ok().filter(|&b| b < crate::stage::LAYERS));
                }
            }
            // STCP(layer = 0, prio = 0) (sub_62A40): stage word 282 + layer; sub_5D3A0 -> sub_32450 draws it from
            // the next commit, so STCP(l, 0) before a wait leaves the layer up until then
            "STCP" => {
                let p = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.staged.prio = Some(p);
                }
            }
            // STZ4(layer = 0, zx0, zy0, zx1, zy1, ms, ease, flag) (sub_62D20 -> sub_31F30): zoom x1000 over
            // 60 ms / 1000 frames. Each from / to defaults to the last target (stage words 1002 / 1042). The flag
            // (byte 2164, render +162: draw unzoomed) is 0 in every script call.
            "STZ4" => {
                let frames = c.int(5).map_or(0, |ms| 60 * ms / 1000);
                let ease = c.int_or(6, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    let last = |t: &Tween| (t.to * 1000.0).round() as i32;
                    let (zx, zy) = (last(&l.zoom_x), last(&l.zoom_y));
                    let (fx, fy) = (c.int_or(1, zx), c.int_or(2, zy));
                    let (tx, ty) = (c.int_or(3, zx), c.int_or(4, zy));
                    l.zoom_x = Tween::new(fx as f32 / 1000.0, tx as f32 / 1000.0, frames, ease);
                    l.zoom_y = Tween::new(fy as f32 / 1000.0, ty as f32 / 1000.0, frames, ease);
                }
            }
            // STCL(rel, layer = 0, ...) (sub_625C0 -> sub_32630): a position tween that stops the layer's motion
            // (sub_958D4). From and the defaults are the last target (stage +164 / +324).
            //   rel == 0: (x0, y0, x1, y1, ms, ease), to defaulting to from   rel != 0: (dx, dy, ms, ease)
            "STCL" => {
                let rel = c.int_or(0, 0) != 0;
                if let Some(l) = self.stage.get(c.int_or(1, 0)) {
                    l.motion = None;
                    let (cx, cy) = (l.x.to, l.y.to);
                    let f = |i: usize, d: f32| c.int(i).map_or(d, |v| v as f32);
                    let (from, to, ms, ease) = if rel {
                        ((cx, cy), (cx + f(2, 0.0), cy + f(3, 0.0)), c.int(4), c.int_or(5, 0))
                    } else {
                        let from = (f(2, cx), f(3, cy));
                        (from, (f(4, from.0), f(5, from.1)), c.int(6), c.int_or(7, 0))
                    };
                    let frames = ms.map_or(0, |ms| 60 * ms / 1000);
                    l.x = Tween::new(from.0, to.0, frames, ease);
                    l.y = Tween::new(from.1, to.1, frames, ease);
                }
            }
            // STRT(layer = 0, from = last target, to = 0, ms, ease) (sub_62B20 -> sub_326E0), 1/100 degrees.
            // Args 5 / 6 land in render words 57 / 58, which nothing reads.
            "STRT" => {
                let frames = c.int(3).map_or(0, |ms| 60 * ms / 1000);
                let ease = c.int_or(4, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    // the default start is the stage rotation (dword 161), which STRT then sets to the target
                    let from = c.int(1).map_or(l.stage_rot.unwrap_or(l.rotation.to), |v| v as f32 / 100.0);
                    let to = c.int_or(2, 0) as f32 / 100.0;
                    l.rotation = Tween::new(from, to, frames, ease);
                    l.stage_rot = None;
                }
            }
            // STGS(layer = 0, from = last target, to = 0, ms, ease) (sub_637A0 -> sub_32820): alpha 0..255,
            // both clamped to at most 255.
            "STGS" => {
                let frames = c.int(3).map_or(0, |ms| 60 * ms / 1000);
                let ease = c.int_or(4, 0);
                // Witch on the Holy Night (jp_637A0) also keeps the fade in its stage words: (from, from, frames),
                // or (to, to, 0) in the next-scene seek, skip mode or a TeaTime replayed from the Archive
                let fast = self.seek_node || self.skip_mode || self.teatime_replay;
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    let from = c.int(1).unwrap_or((l.alpha.to * 255.0).round() as i32).min(255);
                    let to = c.int_or(2, 0).min(255);
                    l.alpha = Tween::new(from as f32 / 255.0, to as f32 / 255.0, frames, ease);
                    let (w, n) = if fast { (to, 0) } else { (from, frames) };
                    l.wfade = [w as i16, n as i16];
                    l.wfade_ease = ease as u8;
                }
            }
            // STPV(layer, px, py, rot) (sub_639A0): set the pivot (stage anchor floats 221 / 261, and the render
            // pivot through sub_32BD0) in script units, scaled by the image level and offset by a variant's
            // centre. The render quad offset stays, so the image does not move until the next commit, but zoom
            // and rotation turn about the new point; the stage shift word becomes (int)-pivot, which the commit
            // (sub_5D3A0) makes the render offset, putting the pivot on the layer's position. Arg 3 sets the
            // stage rotation only.
            "STPV" => {
                let li = c.int_or(0, -1);
                let delta = self.stage.get(li).map(|l| l.name.clone()).map_or((0.0, 0.0), |n| self.variant_delta(&n));
                if let Some(l) = self.stage.get(li) {
                    let (centre, k) = match &l.source {
                        Source::Image(img) => (img.anchor(), img.level),
                        Source::Color(_) => ([0.0; 2], 1.0),
                    };
                    let mut piv = l.stage_pivot.unwrap_or(centre);
                    if let Some(v) = c.int(1) {
                        piv[0] = v as f32 * 1.875 / 0.46875 * k * 0.33333 + delta.0;
                    }
                    if let Some(v) = c.int(2) {
                        piv[1] = v as f32 * 1.875 * 576.0 / 272.0 * k * 362.0 / 1080.0 + delta.1;
                    }
                    l.stage_pivot = Some(piv);
                    l.pivot = Some(piv);
                    l.stage_shift = Some([(-piv[0]).trunc(), (-piv[1]).trunc()]);
                    if let Some(rot) = c.int(3) {
                        l.stage_rot = Some(rot as f32 / 100.0);
                    }
                }
            }
            // STCF(layer = 0, mask = 63) (sub_62F90): wait on the layer's animations (sub_329F0); a layer
            // over 39 or negative waits on all of them with the same mask (Layer::animating). Quakes are not
            // waited on.
            "STCF" => {
                let l = c.int_or(0, 0);
                let l = if (0..40).contains(&l) { l } else { -1 };
                let mask = c.int_or(1, 63);
                if self.stage.animating(l, mask, &|id| self.motions.looping(id)) {
                    self.wait = Wait::Layer(l, mask);
                }
            }
            "STEC" => {
                // sub_3DE0: 0 resets, 1 fills with the colour, 2..=82 select a colour grade that the layer
                // texture is built with (sub_A3180, shader pre_alter); every mode but 1 resets the fill colour.
                // The mode lands in the stage; the texture is only built with it when the layer's image changes
                // at the next commit, so a layer already showing its image keeps its look.
                let mode = c.int_or(1, 0);
                let color = c.int(2).map(|v| rgb(v as u32));
                if let Some(l) = self.stage.get(c.int_or(0, -1))
                    && (0..=82).contains(&mode)
                {
                    l.stec_mode = mode;
                    l.stec_color = if mode == 1 { color.unwrap_or([0.0; 3]) } else { [1.0; 3] };
                    if l.pending && !l.keeps_texture {
                        match mode {
                            1 => {}
                            0 => {
                                l.fill = None;
                                l.shade = Shade::Sprite;
                            }
                            _ => l.fill = None,
                        }
                    }
                    self.rebake(c.int_or(0, -1));
                }
            }
            // STBR(layer = 0, mode = 0, override) (sub_62490): a non-negative arg 2 replaces the mode; stage byte 124,
            // drawn from the next commit (sub_322E0)
            "STBR" => {
                let mode = c.int(2).filter(|&v| v >= 0).map_or(c.int_or(1, 0), |v| v & 0xff);
                let (blend, _) = blend_mode(mode);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.staged.stbr = Some((mode, blend));
                }
            }
            // MSQK(ms = 1000, ax = 5, ay = 5) (0x77d8c -> sub_39110(ms, ay, ax)): shake the message text
            "MSQK" => self.msg_quake.start(c.int_or(0, 1000), c.int_or(1, 5), c.int_or(2, 5)),
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
                        (field::X, l.x.value_x().round() as i32),
                        (field::Y, l.y.value().round() as i32),
                        (field::ALPHA, (l.alpha.value() * 255.0).round() as i32),
                        (field::ROT, (l.rotation.value_rot() * 100.0).round() as i32),
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
                let li = c.int_or(0, -1);
                let cur = self.motion_cur(li.max(0) as usize);
                if let Some(l) = self.stage.get(li) {
                    if id > 0 && exists {
                        // sub_99620 applies the first frame at once (sub_97B80(layer, 0))
                        let mut m = crate::motion::MotionState::new(id, start);
                        let f = self.motions.advance(&mut m, 0.0, &cur);
                        l.motion = Some(m);
                        self.apply_motion(r, li as usize, f);
                    } else {
                        l.motion = None;
                    }
                }
            }
            // FADS (0x77a40): in the seek or while a rewind is held the capture and counts clear and the script goes
            // on (0x77a58). Otherwise it runs again every frame (loc_7FEF4 returns the command itself):
            // - first run: the screen is captured (sub_A4560). A stage whose images changed since it was presented
            //   (sub_2E0B0; knife: a pending layer) is presented under the capture (sub_5D3A0(1, -1)), which stays
            //   drawn while dword_146E660 counts a frame, so the transition starts two frames later; otherwise
            //   it starts now
            // - the start (sub_60610 once sub_B6920 reports the images loaded): the transition, its flag and the
            //   window flags; an instant one draws the capture once more when the last FADS left a rule loaded
            // - the run after the transition flag clears ends it, and script_dispatch returns with the next
            //   command (loc_80260)
            // So the command after an instant FADS of a changed stage runs four frames later, the new stage
            // showing in the third.
            "FADS" => {
                if self.seek_node || self.rewinding {
                    self.fads = None;
                    self.fads_start(r, c);
                    return;
                }
                match self.fads {
                    None => {
                        self.snapshot = true;
                        if self.stage.layers.iter().flatten().any(|l| l.pending) {
                            self.commit_stage(r);
                            self.stage_dirty = false;
                            self.trans = Some(Transition::hold());
                            self.fads = Some(Fads { hold: 1, started: false });
                        } else {
                            self.fads_start(r, c);
                        }
                    }
                    Some(Fads { hold, started: false }) if hold > 0 => {
                        self.fads = Some(Fads { hold: hold - 1, started: false });
                    }
                    Some(Fads { started: false, .. }) => self.fads_start(r, c),
                    Some(Fads { started: true, .. }) => {
                        self.fads = None;
                        if self.trans.as_ref().is_some_and(|t| t.hold) {
                            self.trans = None;
                        }
                        self.dispatch_return = true;
                        return;
                    }
                }
                if self.fads.is_some() {
                    self.pc -= 1;
                    self.dispatch_return = true;
                }
            }
            // SSW2(layer = 0, amp_min = 7, amp_max = 15, period_min_ms = 1000, period_max_ms = 2000, strength = 1,
            // dur_ms = -1, delay_ms) (sub_91CF0): quake slot 1 mode 3; a timed one is dropped during the load label scan (byte_146E638), which knife does not run
            "SSW2" => {
                let li = c.int_or(0, 0);
                let dur = c.int_or(6, -1);
                if (0..40).contains(&li) {
                    let ms = |i: usize, d: i32| c.int(i).map_or(d, |v| 3 * v / 50);
                    let p = [c.int_or(1, 7), c.int_or(2, 15), ms(3, 60), ms(4, 120), c.int_or(5, 1), 0];
                    let q = crate::stage::Quake::new(3, p.map(|v| v as i16), dur, c.int_or(7, 0));
                    self.start_quake(li, 1, q);
                }
            }
            // SPFC(layer, image, snx2, snx3, va, vb, stec, color): effect/texture precache
            "SPFC" => {
                if let Some(name) = c.args.get(1).filter(|n| !n.is_empty()) {
                    let _ = self.assets.image(r, name);
                }
            }
            // SPRD(layer, image, a0, b0, a1, b1) (0x7e214): precaches image_AA_BB for a in {a0, a1} and b in
            // {b0, b1} (the loops step by max(a1 - a0, 1)), the bare name for (0, 0) or past 35 characters
            "SPRD" => {
                if let Some(name) = c.args.get(1).filter(|n| !n.is_empty()).cloned() {
                    let [a0, b0, a1, b1] = [2, 3, 4, 5].map(|i| c.int_or(i, 0));
                    let mut a = a0;
                    while a <= a1 {
                        let mut b = b0;
                        while b <= b1 {
                            let v = if a | b == 0 || name.len() + 6 > 35 {
                                name.clone()
                            } else {
                                format!("{name}_{:02}_{:02}", a.rem_euclid(100), b.rem_euclid(100))
                            };
                            let _ = self.assets.image(r, &v);
                            b += (b1 - b0).max(1);
                        }
                        a += (a1 - a0).max(1);
                    }
                }
            }
            // sub_645F0: STRA(layer or -1 for all, id, ms) sets the haze of every layer with an image
            "STRA" => {
                let li = c.int_or(0, -1);
                let id = c.int_or(1, 0) as i16 as i32;
                let ms = c.int_or(2, 0) as i16 as i32;
                let entry = self.assets.tables.haze(id).copied();
                let apply = |l: &mut Layer| {
                    if matches!(l.source, Source::Image(_)) {
                        l.haze.set(id, ms, entry);
                    }
                };
                if li == -1 {
                    self.stage.layers.iter_mut().flatten().for_each(apply);
                } else if (0..40).contains(&li)
                    && let Some(l) = self.stage.get(li)
                {
                    apply(l);
                }
            }
            // SOXY(layer, _, _, a, b) (0x7b118): only args 3 / 4 are stored, as a draw offset (Layer::soxy)
            "SOXY" => {
                let li = c.int_or(0, 0);
                let off = (c.int_or(3, 0) as i16, c.int_or(4, 0) as i16);
                if let Some(l) = self.stage.get(li) {
                    l.staged.soxy = Some(off);
                }
            }
            // FCLN (sub_92120): only arg 5 counts, the effect (the others repeat its table entry); stage byte
            // 6569 keeps it for loads. FCSP (sub_921B0): off, fading over arg 0 ms (60 * ms / 1000 frames).
            // Witch on the Holy Night's handlers (the dispatcher, jp_77790): FCLN starts effect 4 whatever its
            // arguments (sub_6EC00) and FCSP stops over 24 frames (sub_6EC70); FLNT effect 1 (jp_91FA0), FLOP(n)
            // 2 for n = 5 else 3 (jp_92010), FLSP stops over 48 frames (jp_920B0). Tsukihime has the last three
            // too (sub_91FA0 / sub_92010 / sub_920B0) but no script uses them.
            "FCLN" if crate::assets::witch() => {
                self.focus_saved = 4;
                self.focus.start(4, 0);
            }
            "FCSP" if crate::assets::witch() => {
                self.focus_saved = 0;
                self.focus.start(0, 24);
            }
            "FLNT" => {
                self.focus_saved = 1;
                self.focus.start(1, 0);
            }
            "FLOP" => {
                let n = if c.int(0) == Some(5) { 2 } else { 3 };
                self.focus_saved = n as u8;
                self.focus.start(n, 0);
            }
            "FLSP" => {
                self.focus_saved = 0;
                self.focus.start(0, 48);
            }
            "FCLN" => {
                let n = c.int_or(5, 0);
                self.focus_saved = n as u8;
                self.focus.start(n, 0);
            }
            "FCSP" => {
                self.focus_saved = 0;
                self.focus.start(0, 60 * c.int_or(0, 0) / 1000);
            }
            // SPRC (sub_B6B50) releases render targets, which wgpu owns here
            "SPRC" => {}
            // XSTP (0x78054): F36 = 1, then sub_3F600(1, 1, 36, ..), the staff roll task; XRSM (sub_3F9B0(1, 1))
            // ends it next frame. During a next-scene seek the init does not run (dword_AFF010) and kind 1 lacks
            // flag 8, so the task keeps running past the seek without off_AE0650 or the mode save; its end still
            // puts back what the previous task start kept.
            "XSTP" => {
                self.end_staff_task();
                self.set_var("F36", 1);
                self.staff_task = true;
                if !self.seek_node {
                    // sub_40B70 also turns the fast skip off (off_AE0670 = 0)
                    self.speed = 1;
                    self.begin_mode_task();
                }
            }
            "XRSM" => self.end_staff_task(),
            // XSKR(n, label) (Witch on the Holy Night, 0x57db0): the label a held button skips the staff roll to;
            // XSKR(-1,) turns it off. Either restarts the hold count.
            "XSKR" if crate::assets::witch() => {
                self.xskr = c.int(0).unwrap_or(-1);
                self.xskr_pc = if self.xskr >= 0 { c.arg(1).and_then(|l| self.script.label(l)) } else { None };
                self.xskr_hold = 0.0;
            }
            // XSOF() (Witch on the Holy Night): the fast skip (AE0670) and the auto / skip modes (AE0660) go off and
            // a next-scene seek ends with its sound (jp_5D7E0, AE0568 = 0)
            "XSOF" => {
                self.skip = false;
                self.speed = 1;
                self.auto = false;
                self.skip_mode = false;
                if self.seek_node {
                    self.end_seek(true);
                }
            }
            // FADE(40, p, q) (sub_5F420): F1100 = 4, F1120 = p + 127, F1121 = q, then the channel-split effect;
            // FADE(41): F1100 = 0 and the effect fades out. Other modes are engine states.
            "FADE" => match c.int_or(0, 0) {
                40 => {
                    let reset = self.var("F1100") == 0;
                    let (p, q) = (c.int_or(1, 0), c.int_or(2, 0));
                    self.set_var("F1100", 4);
                    self.set_var("F1120", p + 127);
                    self.set_var("F1121", q);
                    self.ghost.start(p, q, reset);
                }
                41 => {
                    self.set_var("F1100", 0);
                    self.ghost.set_mode(5, 0, 0);
                }
                _ => {}
            },
            // STNS(layer, n): grain overlay (always on white img2345 layers); 0 clears
            "STNS" => {
                self.load_noise(r);
                let n = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.noise = (n > 0).then_some(n);
                }
                // sub_648A0 sets word 0x7CA for a layer 0..39: the dispatch presents the stage as it returns
                if (0..=39).contains(&c.int_or(0, 0)) {
                    self.stage_dirty = true;
                }
            }
            // STTR(layer, rule): following alpha fades wipe through RULE_nn; -1/0 clears
            // STTR(layer = 0, rule = 0, b = 0) (inline 0x7b188): stage word 1142 = (rule - 1) | (b << 12), -1 for no
            // rule; the same word goes to render +280 at once when the slot shows an image (render +12), and
            // nothing else ever copies it there (sub_36520 passes (word >> 12) & 7; a negative word is no rule)
            "STTR" => {
                let li = c.int_or(0, 0);
                let (n, b) = (c.int_or(1, 0), c.int_or(2, 0));
                if !(0..=39).contains(&li) {
                    return;
                }
                let word = if n >= 1 { ((n - 1) & 0xffff | b << 12) as i16 } else { -1 };
                // Witch on the Holy Night's STTR (0x5ae0c) runs only outside the seek and AE0670, and sets the
                // wipe edge from its fourth argument (64 by default)
                if crate::assets::witch() {
                    if self.seek_node || self.skip {
                        return;
                    }
                    self.stage.rule_soft = soft_width(c.int_or(3, 64) as f32);
                }
                if let Some(l) = self.stage.get(li) {
                    l.rule = (word >= 0).then_some(n);
                    l.rule_flip = (word.max(0) >> 12 & 7) as u8;
                }
                if self.stage.shown[li as usize] {
                    self.stage.rules[li as usize] = (word >= 0)
                        .then(|| self.assets.rule(r, &format!("RULE_{:02}", (word & 0xfff) + 1)))
                        .flatten()
                        .map(|i| crate::stage::RenderRule { tex: i.tex.clone(), flip: (word >> 12 & 7) as u8 });
                }
            } // per-layer effect seed for the sway/raster animation; no visible effect alone
            // SQK2(layer = 0, ax = 15, ay = 15, period_ms, dur_ms = -1, delay_ms, follow, ramp_ms) (sub_91540):
            // quake slot 0 mode 1 on the layer, or on every layer with an image for -1; a timed one is dropped
            // during the load label scan (byte_146E638), which knife does not run
            "SQK2" => {
                let li = c.int_or(0, 0);
                let dur = c.int_or(4, -1);
                if li <= 39 {
                    let frames = match c.int(3) {
                        Some(v) if v < 0 => 1,
                        Some(v) if 3 * v + 49 >= 99 => 3 * v / 50 + 1,
                        _ => 2,
                    };
                    let a7 = c.int(7).map_or(0, |v| 3 * v);
                    let total = if dur >= 0 && a7 <= 49 { 3 * dur / 50 } else { 0 };
                    let p = [c.int_or(1, 15), c.int_or(2, 15), frames, a7 / 50, total, c.int_or(6, 0)];
                    let q = crate::stage::Quake::new(1, p.map(|v| v as i16), dur, c.int_or(5, 0));
                    self.start_quake(li, 0, q);
                }
            }
            // SHK2(layer = 0, ax = 0, ay = 15, period_ms = 300, dur_ms = -1, delay_ms, fade, ramp_ms, sparse = 1)
            // (sub_91900): quake slot 1 mode 2, a sine shake; -1 reaches the layers with an image
            "SHK2" => {
                let li = c.int_or(0, 0);
                let dur = c.int_or(4, -1);
                if (-1..40).contains(&li) {
                    let p = [
                        c.int_or(1, 0),
                        c.int_or(2, 15),
                        c.int(3).map_or(18, |v| 3 * v / 50),
                        c.int_or(6, 0),
                        c.int(7).map_or(0, |v| 3 * v / 50),
                        c.int_or(8, 1),
                    ];
                    let q = crate::stage::Quake::new(2, p.map(|v| v as i16), dur, c.int_or(5, 0));
                    self.start_quake(li, 1, q);
                }
            }
            // SQS2 / SHS2 / SSS2(layer = 0) (sub_91870 / sub_91C60 / sub_91F10 -> sub_63680): stop quake slot 0 /
            // 1 / 1 on the layer, or on all 40 for a negative one
            "SQS2" | "SHS2" | "SSS2" => {
                let li = c.int_or(0, 0);
                let slot = usize::from(name != "SQS2");
                if li <= 39 {
                    for (k, l) in self.stage.layers.iter_mut().enumerate() {
                        if (li < 0 || k as i32 == li)
                            && let Some(l) = l
                        {
                            l.quake[slot] = crate::stage::Quake::default();
                        }
                    }
                }
            }
            // STQK(layer = 0, ax = 0, ay = 0, period_ms = 34, dur_ms = -1, delay_ms = 0, type = 0) (Witch on the
            // Holy Night, sub_438A0): the layer's quake slot (`WitchQuake::new`); -1 sets layers 0 and 1 and every
            // other layer that is drawn or holds an image, which type 1 makes followers of layers 0 / 1 (ax = ay =
            // -1, type 0, P at least 0)
            "STQK" => {
                let li = c.int_or(0, 0);
                if li <= 39 {
                    let (kind, period, dur) = (c.int_or(6, 0), c.int_or(3, 34), c.int_or(4, -1));
                    let delay = c.int(5).map_or(0, |v| 3 * v / 50) as i16;
                    let q = WitchQuake::new(kind, c.int_or(1, 0) as i16, c.int_or(2, 0) as i16, period, dur, delay);
                    let follow = WitchQuake::new(0, -1, -1, period, dur, delay);
                    for (k, l) in self.stage.layers.iter_mut().enumerate() {
                        if (li < 0 || k as i32 == li)
                            && let Some(l) = l
                        {
                            l.wquake = if li < 0 && k >= 2 && kind == 1 { follow } else { q };
                        }
                    }
                }
            }
            // STSW(layer = 0) (jp_91870 -> sub_43770(layer, 0, 0, 0, -1, 0, 1)): the layer sways
            "STSW" => {
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.wquake = WitchQuake::sway();
                }
            }
            // STQS(layer = 0) (sub_44320): zeroes the layer's quake slot but its type, -1 every layer's after
            // stopping the screen shake (sub_2EA70(-1))
            "STQS" => {
                let li = c.int_or(0, 0);
                if li <= 39 {
                    if li < 0 {
                        self.shake.stop();
                    }
                    for (k, l) in self.stage.layers.iter_mut().enumerate() {
                        if (li < 0 || k as i32 == li)
                            && let Some(l) = l
                        {
                            l.wquake.stop();
                        }
                    }
                }
            }
            "SNX2" | "SNX3" => {
                // sub_4030 / sub_4120: brightness 1 + v / 255 (default 0) / contrast 1 + v / 100 (default 1.0
                // as is) into the stage and render floats; the texture build bakes them (pre_alter)
                let li = c.int_or(0, -1);
                let (k, v) = match c.int(1) {
                    Some(v) if name == "SNX2" => (0, 1.0 + v as f32 / 255.0),
                    None if name == "SNX2" => (0, 1.0),
                    Some(v) => (1, 1.0 + v as f32 * 0.01),
                    None => (1, 1.0),
                };
                if let Some(l) = self.stage.get(li) {
                    l.snx[k] = v;
                    self.rebake(li);
                }
            }
            // STRR(layer = 0, r) (sub_633D0): the clip rotation its children are composited with; -99999 stores
            // -32768, which follows the child's own rotation. Stage word 1522 only, drawn from the next commit
            // (sub_5D3A0 -> render +328)
            "STRR" => {
                let v = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.staged.rot2 = Some(if c.int(1).is_none() { 0 } else if v == -99999 { i16::MIN } else { wrap_rot2(v) });
                }
            }
            // SNX4(layer = -1, a = 0, b = 0) (Witch on the Holy Night, 0x58f50): two floats into the stage (4844 +
            // 16 layer + 8 / + 12) and render (+0x20C / +0x210) that nothing draws with; Tsukihime ignores it
            "SNX4" => {
                let v = [c.int_or(1, 0) as f32, c.int_or(2, 0) as f32];
                if crate::assets::witch()
                    && let Some(l) = self.stage.get(c.int_or(0, -1))
                {
                    l.snx4 = v;
                }
            }
            // SPIA / SPAB fall to the dispatcher's default case in Witch on the Holy Night
            "FANX" | "TRPY" | "SPIA" | "SPAB" => {}
            // SHOK(ax, ay, period_ms = 300, count = 3, delay_ms) (sub_5F8F0 -> sub_2EE70(1, ..)): the sine screen
            // shake (ignored only during the load label scan)
            "SHOK" => {
                self.shake.start(c.int_or(0, 0), c.int_or(1, 0), c.int_or(2, 300), c.int_or(3, 3), c.int_or(4, 0));
            }
            // SHAF() stops the screen shake (sub_2E560(0, 0, 0, 0)); SHAF(ms) fades it (sub_2E9D0)
            "SHAF" => match c.int(0) {
                Some(ms) => self.shake.fade_out(ms),
                None => self.shake.stop(),
            },

            // audio
            "SEPL" | "SEFD" | "SESP" | "SEVL" | "SEPN" | "SEAC" => {
                if self.se_pushes(c) {
                    self.push_ring_event(3);
                }
                self.cmd_se(c);
            }
            // MPLY(n, key 17 volume = 100, key 1 fade-in ms, key 11 fade-out ms, key 69 start sample) (sub_649E0):
            // a track already playing is left alone; with a fade-in the old track fades out at 1666.7 / (key 11
            // or key 1) volume points a frame while the new one rises at 1666.7 / key 1 (so key 1 ms at full
            // volume); without one the old track stops at once
            "MPLY" => {
                // a type 2 ring entry first (sub_649E0 / sub_64FA0 / MSTP 0x78e84 / sub_653B0)
                self.push_ring_event(2);
                let n = c.int_or(0, 0);
                let vol = c.keyed_int(17).unwrap_or(100);
                let fade_in = c.keyed_int(1).unwrap_or(0);
                let fade_out = c.keyed_int(11).unwrap_or(0);
                let start = c.keyed_int(69).unwrap_or(0).max(0) as usize;
                // only MPLY marks a BGM heard (sub_649E0, n <= 0x417; jp_649E0, n <= 0x207); the title and
                // movie BGM do not
                let witch = crate::assets::witch();
                if (0..=if witch { 0x207 } else { 0x417 }).contains(&n) {
                    set_bit(&mut self.heard, n as usize);
                }
                // jp_649E0 keeps the volume as 255 x key 17 / 100 (jd_146E650): a fade-in goes to the whole
                // percent of it at that word x 1000 / 60 / key 1 percent a frame, the old track out at its own
                // percent x 1000 / 60 / (key 11 or key 1) (out in that many ms at any volume); without a fade
                // jp_9C470 plays at word / 255
                let word = 255 * vol / 100;
                let frac = if witch { word as f32 / 255.0 } else { vol as f32 / 100.0 };
                if n >= 0 {
                    self.sound.bgm_word = if witch { word } else { vol } as i16;
                }
                if witch && self.seek_audio.is_none() && n >= 0 && fade_in != 0 && self.sound.bgm_state().map(|b| b.0) != Some(n) {
                    let out = if fade_out == 0 { fade_in } else { fade_out };
                    let old = self.sound.bgm_state().map_or(0.0, |b| b.1 * 100.0);
                    self.sound.stop_bgm_rate(old * 1000.0 / 60.0 / out as f32);
                    let pct = 100 * word / 255;
                    // pct / (word x 1000 / 60 / ms) frames
                    let secs = if word > 0 { f64::from(pct) * fade_in as f64 / (f64::from(word) * 1000.0) } else { 0.0 };
                    self.play_bgm_at(n, pct as f32 / 100.0, secs, start);
                } else if let Some(sa) = &mut self.seek_audio {
                    sa.bgm = (n >= 0).then_some((n, frac));
                } else if n < 0 {
                    self.sound.stop_bgm(0.0);
                } else if self.sound.bgm_state().map(|b| b.0) != Some(n) {
                    let vol = frac;
                    if fade_in != 0 {
                        let out = if fade_out == 0 { fade_in } else { fade_out };
                        self.sound.stop_bgm_rate(1666.7 / out as f32);
                        self.play_bgm_at(n, vol, f64::from(vol) * fade_in as f64 / 1000.0, start);
                    } else {
                        self.play_bgm_at(n, vol, 0.0, start);
                    }
                }
            }
            // MFAD(key 1 ms) (sub_64FA0): out at word_146E650 * 1000 / 60 / ms points a frame (ms), else at once
            "MFAD" => {
                // a type 2 ring entry first (sub_649E0 / sub_64FA0 / MSTP 0x78e84 / sub_653B0)
                self.push_ring_event(2);
                let ms = c.keyed_int(1).unwrap_or(0);
                if let Some(sa) = &mut self.seek_audio {
                    sa.bgm = None;
                } else {
                    self.sound.stop_bgm(ms.max(0) as f64 / 1000.0);
                }
            }
            // MSTP() (Witch on the Holy Night): outside the next-scene seek (AE0568) the BGM fades out at 4.17 points
            // a frame (jp_9C9B0: 0.4 s from full volume); the BGM word clears
            "MSTP" => {
                // a type 2 ring entry first (sub_649E0 / sub_64FA0 / MSTP 0x78e84 / sub_653B0)
                self.push_ring_event(2);
                if let Some(sa) = &mut self.seek_audio {
                    sa.bgm = None;
                } else {
                    self.sound.stop_bgm_rate(4.1666665);
                }
            }
            // MVOL(key 1 ms, key 17 volume = 0) (sub_653B0): to the volume over max(ms, 1) ms
            // Witch on the Holy Night's jp_653B0 keeps 255 x volume / 100 and steps the player to word x 100 / 255
            // percent at |word - old word| x 1000 / 60 / ms a frame (that is, in ms x 100 / 255)
            "MVOL" => {
                // a type 2 ring entry first (sub_649E0 / sub_64FA0 / MSTP 0x78e84 / sub_653B0)
                self.push_ring_event(2);
                let ms = c.keyed_int(1).unwrap_or(0).max(1);
                let pct = c.keyed_int(17).unwrap_or(0);
                let witch = crate::assets::witch();
                let word = if witch { 255 * pct / 100 } else { pct };
                let (v, secs) = if witch {
                    let to = word as f32 * 100.0 / 255.0;
                    let d = (word - i32::from(self.sound.bgm_word)).abs();
                    let from = f32::from(self.sound.bgm_word) * 100.0 / 255.0;
                    (to / 100.0, if d > 0 { f64::from((to - from).abs()) * ms as f64 / (f64::from(d) * 1000.0) } else { 0.0 })
                } else {
                    (pct as f32 / 100.0, ms as f64 / 1000.0)
                };
                if let Some(sa) = &mut self.seek_audio {
                    if let Some(b) = &mut sa.bgm {
                        b.1 = v;
                    }
                } else {
                    self.sound.set_bgm_volume(v, secs);
                }
                self.sound.bgm_word = word as i16;
            }
            "VPLY" | "VPL2" | "VLOG" => {
                if let Some(n) = c.arg(0) {
                    // Witch on the Holy Night's handler (0x57c90) keeps no voice progress
                    if self.assets.build != Build::Witch {
                        self.note_voice(n);
                    }
                    self.pending_voice = Some(n.to_string());
                    // VPLY leaves the voice for the next message start (AE04C8, played by sub_6E0D0); VPL2
                    // plays it at once, without making it the replay voice, and drops it. VLOG (Witch on the
                    // Holy Night) marks the history entry (+13281 bit 7), so the next message makes it the replay
                    // and backlog voice without playing it
                    self.log_voice = name == "VLOG";
                    if name == "VPL2" {
                        self.play_voice(n, false);
                        self.voice_to_play = None;
                    } else {
                        self.voice_to_play = Some(n.to_string());
                    }
                }
            }
            "VCWT" => {
                if self.sound.voice_playing() {
                    self.wait = Wait::Voice;
                }
            }
            // Witch on the Holy Night (0x5813c) reads no argument: it ends a next-scene seek without its sound and
            // the fast skip, then starts engine task 0 as kind 0 (jp_3F600(0, 0, 2, 1, 0, 0)), the opening movie
            "TOMP" if self.assets.build == Build::Witch => {
                if self.seek_node {
                    self.end_seek(false);
                }
                self.skip = false;
                self.start_witch_opening(r);
            }
            "TOMP" => {
                let n = c.int_or(0, -1);
                // sub_6EF30: but for movie 7 it ends the next-scene seek without resuming its sound and stops the
                // fast skip; movie 7 does not play during a seek. F1103 = 1.
                if n != 7 {
                    if self.seek_node {
                        self.end_seek(false);
                    }
                    self.skip = false;
                } else if self.seek_node {
                    return;
                }
                self.set_var("F1103", 1);
                // sub_40480 / sub_40A00: these movies set the flags the movie / music galleries check once they
                // end, and can be skipped only when already watched
                let flag = [(5, 0), (6, 1), (4, 2), (35, 3), (36, 4), (37, 5)].iter().find(|m| m.0 == n).map(|m| m.1);
                // sub_402A0 plays the language's copy (sub_41260) and picks the cover by that entry
                let language = self.assets.language();
                let entry = usize::try_from(n).ok().map(|n| crate::movie::localized(n, language));
                match entry.and_then(crate::movie::movie_name) {
                    Some(name) if !self.skip_all => {
                        self.play_movie(r, &name);
                        if self.movie.is_some() {
                            let skippable = flag.is_none_or(|f| self.movie_flags & (1 << f) != 0);
                            let entry = entry.unwrap_or_default() as i32;
                            self.story_movie = Some(StoryMovie::new(n, entry, language, flag, skippable));
                            self.begin_mode_task();
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
            // Witch on the Holy Night (jp_92630): n <= 255 sets bit n of the G words from 130 (the title's
            // completion test, jp_8D180)
            "RDST" if crate::assets::witch() => {
                self.node_seq += 1;
                if let Some(n) = c.int(0).filter(|&n| (0..=255).contains(&n)) {
                    let g = format!("G{:03}", 130 + n / 16);
                    let v = self.var(&g) | i32::from((1u32 << (n % 16)) as u16 as i16);
                    self.set_var(&g, v);
                }
            }
            "RDST" => {
                self.node_seq += 1;
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    set_bit(&mut self.nodes, n as usize);
                }
            }
            "RTST" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    self.route_enter(n);
                }
            }
            // RDED(n) (sub_92440): node n is completed only when it is the current node F110 and was reached in
            // this playthrough (RTST); then a replay (F024) ends there with result 2 (0x79be8)
            "RDED" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n))
                    && n == self.var("F110")
                    && self.play_bit('R', n as usize)
                {
                    set_bit(&mut self.endings, n as usize);
                    if self.var("F024") != 0 {
                        tracing::info!("RDED {n}: gallery replay finished");
                        self.replay_done = true;
                        self.script_end();
                    }
                }
            }
            // ATSV(flags) (0x778e8): bit 3 does nothing at all. Otherwise it ends a next-scene seek (resume,
            // 0x78bf8), pushes its ring entry (type 7 with the last message's text after an argument of mode
            // flags & 3 == 1, else type 6; ATSV() also clears that text), is the save point when F823 <= 1,
            // and becomes a scene point (0x7c174) unless bit 2 is set or an engine task runs.
            "ATSV" => {
                let flags = c.int_or(0, 0);
                if flags & 8 != 0 {
                    return;
                }
                let has_arg = c.arg(0).is_some_and(|a| !a.is_empty());
                if self.seek_node {
                    self.end_seek(true);
                    // 0x78c1c: a seek ending at ATSV(1) shows the last message again (sub_5AFC0)
                    if has_arg && flags & 3 == 1 {
                        self.redisplay_message();
                    }
                }
                let kind = if has_arg && flags & 3 == 1 { 7 } else { 6 };
                if !has_arg {
                    self.msg_text.clear();
                }
                let mut s = self.snapshot_at(self.pc - 1, Vec::new());
                s.kind = kind;
                s.voice = None;
                if kind == 7 {
                    s.text = entry_text(&self.msg_text);
                    let mut t = self.page.texts.clone();
                    t.pop();
                    s.page_texts = t;
                } else {
                    s.preview.clear();
                }
                let locked = self.in_movie() || self.staff_task;
                // sub_5A910 (type 6 / 7: not while F1102 is set); a held push leaves the newest entry as it is
                let pushed = self.var("F929") == 0 && self.var("F1102") == 0 && !self.ring_hold;
                let (point, entry_locked, entry_kind) = if pushed {
                    let key = (self.scene_index, self.pc - 1, usize::MAX - 1);
                    let serial = self.push_ring(s.clone(), key, locked, kind);
                    (ScenePoint { data: s, serial: Some(serial) }, locked, kind)
                } else {
                    // the text goes into the newest entry, which the point copies
                    let Some(h) = self.history.back_mut() else { return };
                    h.data.text = s.text.clone();
                    (ScenePoint { data: h.data.clone(), serial: None }, h.locked, h.kind)
                };
                if !entry_locked && !matches!(entry_kind, 11 | 12) && self.var("F823") <= 1 && !self.ring_restored {
                    self.save_point = Some(self.with_scene_prev(point.data.clone()));
                }
                if flags & 4 == 0 && !locked {
                    if self.scene_ring.len() >= SCENE_POINTS {
                        self.scene_ring.remove(0);
                    }
                    self.scene_ring.push(point);
                    self.scene_msgs = 2;
                }
            }
            // RDAS(n) (0x79b78): with the auto skip of read scenes on and node n completed, a mode banner still up
            // is sent away (sub_3B480: one holding jumps to its fall) and RDAS runs again each frame until it is
            // gone (sub_3B580; loc_7FEF4 returns the command itself); then sub_92580 asks for the next scene
            // (sub_40ED0 top state 1, from the next frame) unless the next-scene seek runs (off_AE0568; a skip does
            // not stop it), and the dispatch goes on to the next wait. In a gallery flowchart replay (sub_50F0)
            // RDAS does nothing. The seek loop does not step the banner (sub_3C970 runs once a frame after
            // sub_40ED0), so knife only waits for it outside the seek.
            "RDAS" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n))
                    && self.settings.auto_skip_read
                    && !self.flow_replay
                    && !self.skip_all
                    && get_bit(&self.endings, n as usize)
                {
                    if self.banner_up && !self.seek_node {
                        self.banner_away = true;
                        self.pc -= 1;
                        self.dispatch_return = true;
                    } else if !self.seek_node {
                        self.auto_skip_hit = true;
                    }
                }
            }
            "NOP_" | "WTSV" => {}
            other => self.warn_once(other),
        }
    }

    pub fn play_movie(&mut self, r: &Renderer, name: &str) {
        let path = self.assets.path(format!("movie/{name}.mp4"));
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
        let path = self.assets.path(format!("movie/titlebg{n}.mp4"));
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

    /// The movie sprite (off_AE01D0) over the whole canvas at its alpha.
    pub fn movie_quad(&self) -> Option<Quad> {
        let m = self.movie.as_ref().filter(|m| m.has_frame())?;
        Some(Quad::rect(m.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0, 1.0, 1.0, m.alpha]))
    }

    /// The movie sprite's alpha (0 before the first frame).
    pub fn movie_alpha(&self) -> f32 {
        self.movie.as_ref().filter(|m| m.has_frame()).map_or(0.0, |m| m.alpha)
    }

    /// sub_1A80 from the UI: the movie stops and fades out (then closes).
    pub fn fade_movie_out(&mut self) {
        self.stop_movie_playback();
    }

    /// sub_ACEF0(n): system sound n on the system SE volume (config, linear x 0.01).
    pub fn play_sys_sound(&mut self, n: usize) {
        tracing::debug!("system sound {n}");
        let vol = if self.settings.mute & 4 != 0 { 0.0 } else { self.settings.sys_se };
        if let Some(pcm) = sys_sound_id(n).and_then(|id| self.assets.sound_effect_id(id)) {
            self.sound.play_sys(0, pcm, vol);
        }
    }

    /// sub_ACFA0(n): system sound n on the SE volume through the sub_CA0 curve.
    fn play_se_sound(&mut self, n: usize) {
        let vol = knife_audio::volume_curve(self.sound.se_volume());
        if let Some(pcm) = sys_sound_id(n).and_then(|id| self.assets.sound_effect_id(id)) {
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
        if let Some(t) = self.story_movie.take() {
            if let Some(f) = t.flag() {
                self.movie_flags |= 1 << f;
            }
            // sub_21DE0: F36 = 0
            if t.is_witch() {
                self.set_var("F36", 0);
            }
            self.end_mode_task();
        }
    }

    /// Witch on the Holy Night's opening task init (sub_21AD0): F36 = 1, the modes saved and auto / skip off,
    /// and opening_<language>.mp4 (jd_40B868) at the movie volume. jp_3F600 ends the slot's running task first.
    fn start_witch_opening(&mut self, r: &Renderer) {
        if self.story_movie.is_some() {
            self.stop_movie();
        }
        if self.skip_all {
            return;
        }
        let name = ["opening_ja", "opening_en", "opening_zc", "opening_zt"][self.assets.language().min(3)];
        self.play_movie(r, name);
        if self.movie.is_some() {
            self.set_var("F36", 1);
            self.story_movie = Some(StoryMovie::witch_opening(self.assets.language()));
            self.begin_mode_task();
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
        self.play_bgm_at(n, vol, fade, 0);
    }

    pub fn play_bgm_at(&mut self, n: i32, vol: f32, fade: f64, start: usize) {
        if n <= 0 {
            self.sound.stop_bgm(fade);
            return;
        }
        match self.assets.sar.stream_sound(n as usize - 1) {
            Ok(s) => {
                let path = self.assets.path(&s.path);
                self.sound.play_bgm(n, vol, fade, start, path, s.loop_range);
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
    ///
    /// All three are in the saved F subset.
    fn note_stage_change(&mut self, c: &Command) {
        let arg = |i: usize| c.int(i);
        // signed `> 39` guards accept -1 (all layers); unsigned ones only 0..=39; absent args pass
        let signed = |i: usize| arg(i).is_none_or(|l| l <= 39);
        let unsigned = |i: usize| arg(i).is_none_or(|l| (0..=39).contains(&l));
        let name = c.name.as_str();
        match name {
            // FADS: `fads_start`, once its transition starts
            "FADS" => return,
            // sub_99620: the same switch in arg 2, without the duration
            "STMA" if matches!(arg(2), Some(1)) || (arg(2) == Some(2) && self.page.dim_vel == 0.0) => {
                self.set_var("F1103", 1);
            }
            // WoH's WTTM(0) returns before the refresh
            "WTTM" if crate::assets::witch() && arg(0).unwrap_or(0) == 0 => return,
            "VPLY" | "VLOG" | "MSAD" | "WTTM" | "WTIN" => {
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
            | "FLNT" | "FLOP" | "FLSP" | "STMA" | "SPMA" | "SGMA" | "SQK2" | "SQS2" | "SHK2" | "SHS2" | "SSW2" | "SSS2" | "STQK" | "STQS"
            | "STSW" => true,
            // sub_3DE0 / sub_4030 / sub_4120 back up once their layer passes (STEC 0..=39, SNX2 / SNX3 >= 0)
            "STEC" => stch_ok,
            "SNX2" | "SNX3" => arg(0).is_some_and(|l| l >= 0),
            // Witch on the Holy Night's SNX4 backs up before it looks at its layer
            "SNX4" => crate::assets::witch(),
            // Witch on the Holy Night's clock (sub_73A0 / sub_7BB0) backs up once its layer is 0..=34
            "CKDP" | "CKTT" => arg(0).is_some_and(|l| (0..=34).contains(&l)),
            _ => false,
        };
        if backs_up && self.var("F991") == 0 {
            // sub_5F3B0: a type 1 ring entry, then the stage is the backup
            self.push_ring_event(1);
            self.stage_backup = Some(self.stage_bytes());
            self.set_var("F991", 1);
        }
        let changed = match name {
            "STCH" => stch_ok,
            "STZ4" | "STGS" | "SQS2" | "SHS2" | "SSS2" | "STQK" | "STQS" | "STSW" => signed(0),
            // the quakes skip a timed call (arg 4 / 4 / 6 >= 0) only during the load label scan
            "SQK2" => signed(0),
            "SHK2" => arg(0).is_none_or(|l| (-1..=39).contains(&l)),
            "SSW2" => unsigned(0),
            "STCL" => signed(1),
            "STRR" | "STMA" => unsigned(0),
            "SGMA" => unsigned(0) && arg(1).is_some_and(|m| m >= 1),
            "SPMA" | "FCSP" | "FCLN" | "FLNT" | "FLOP" | "FLSP" => true,
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

    /// Whether speaker row `key70 - 1` has its voice on (SEPL key 70: voices on and the row's bit set in the
    /// config, off_AE03D8 + 6 / + 0x24); a key outside the rows reads off.
    fn se_speaker_on(&self, key70: i32) -> bool {
        usize::try_from(key70 - 1).is_ok_and(|r| r < 19 && self.settings.voice_on && self.settings.char_mute >> r & 1 == 0)
    }

    /// Whether an SE command from the script pushes a type 3 ring entry before it acts: SEFD, SEVL, SESP, SEPN
    /// and SEAC always (sub_6B310 / sub_6AE10 / 0x7a734 / sub_6B820 / sub_6C0A0; a delayed one when sub_66200
    /// queues it), SEPL only when queued, when it stops its channel (no sound) or when it starts a loop
    /// (sub_6A340; a one-shot pushes nothing). The SEPL / SEVL / SEPN that SEAC runs push nothing (no script
    /// position). Witch on the Holy Night's jp_ handlers are the same.
    fn se_pushes(&self, c: &Command) -> bool {
        if c.name != "SEPL" {
            return true;
        }
        let queued = self.seek_audio.is_none() && c.keyed_int(68).is_some();
        queued || c.arg(2).is_none_or(str::is_empty) || c.int_or(1, 0) == 1
    }

    /// SEPL / SEFD / SEVL / SESP (sub_6A340 / sub_6B310 / sub_6AE10 / 0x7a6f4). With a delay (key 68, ms, past
    /// the first argument) outside a seek (off_AE0568 / AE0900), sub_66200 queues the command on its channel (all
    /// 20 for a negative one) for (3 ms + 49) / 50 frames (1 for -32..0); an op already queued there runs at once
    /// first. sub_65E50 counts the frames down and sub_65A60 runs it (`run_se_queued`).
    fn cmd_se(&mut self, c: &Command) {
        let ch = c.int_or(0, if matches!(c.name.as_str(), "SEPL" | "SEAC") { 0 } else { -1 });
        let seek = self.seek_audio.is_some();
        if c.name == "SEPN" {
            // SEPN(ch, pan 0..128, ms) (sub_6B820): the pan word (x 1000, -1000..1000, 0 without one) and outside
            // a seek the player's pan, at once for ms <= 0 (sub_9CE60) or from the word at a rate a frame
            // (sub_9D110); no delay key
            let pan = c.int(1).map_or(0, |p| ((1000 * p - 64000) / 64).clamp(-1000, 1000));
            let ms = c.int_or(2, 0);
            let Ok(ku) = usize::try_from(ch % 20) else { return };
            if !seek {
                let word = i32::from(self.sound.se_pan[ku]);
                if ms <= 0 {
                    self.sound.set_se_pan(ch % 20, pan);
                } else {
                    self.sound.ramp_se_pan(ch % 20, word, pan, (pan - word) as f32 * 1000.0 / 60.0 / ms as f32);
                }
                self.sound.se_pan[ku] = pan as i16;
            }
            return;
        }
        if c.name == "SESP" {
            // SESP(ch % 20, all 20 when negative) clears the channel's loop words and drops its queued op unrun;
            // outside the seek it stops the channel at once (sub_9CF90). The volume word stays.
            let k = ch % 20;
            self.se_delayed.retain(|d| k >= 0 && d.0.channel != k);
            for k in if k < 0 { 0..20 } else { k..k + 1 } {
                if let Some(sa) = &mut self.seek_audio {
                    sa.loops.remove(&k);
                } else {
                    self.sound.stop_se(k);
                }
            }
            return;
        }
        if !seek && let Some(ms) = c.keyed_int(68) {
            let frames = if (-32..=0).contains(&ms) { 1 } else { (3 * ms + 49) / 50 };
            // SEAC (type 4) without a sound queues as SEFD; its duration is never stored (+1116 stays -1)
            let kind = match c.name.as_str() {
                "SEPL" => 1,
                "SEFD" => 2,
                "SEAC" if c.arg(1).is_none() => 2,
                "SEAC" => 4,
                _ => 3,
            };
            let seac = c.name == "SEAC";
            for k in if ch < 0 { 0..20 } else { ch % 20..ch % 20 + 1 } {
                if let Some(i) = self.se_delayed.iter().position(|d| d.0.channel == k) {
                    let (old, _, _) = self.se_delayed.remove(i);
                    self.run_se_queued(&old);
                }
                let key = |n| c.keyed_int(n);
                let q = if seac {
                    // SEAC(ch, sound, loop, volume, end volume, pan = 64, end pan = pan, ms)
                    let pan = c.int_or(5, 64);
                    SeQueued {
                        channel: k,
                        frames: frames as i16,
                        kind,
                        name: (kind == 4).then(|| c.arg(1).map(str::to_string)).flatten(),
                        fade: -1,
                        volume: if kind == 4 { c.int(3).map_or(-1, |v| v as i8) } else { -1 },
                        looping: kind == 4 && c.int_or(2, 0) != 0,
                        pan: if kind == 4 { pan as i16 } else { -1 },
                        speaker: 0,
                        vol_end: if kind == 4 { c.int(4).map_or(-1, |v| v as i8) } else { -1 },
                        pan_end: if kind == 4 { c.int_or(6, pan) as i16 } else { -1 },
                    }
                } else {
                    SeQueued {
                        channel: k,
                        frames: frames as i16,
                        kind,
                        name: (kind == 1).then(|| c.arg(2).map(str::to_string)).flatten(),
                        fade: if kind == 1 || kind == 2 || kind == 3 { key(1).unwrap_or(-1) } else { -1 },
                        volume: if kind != 2 { key(17).map_or(-1, |v| v as i8) } else { -1 },
                        looping: kind == 1 && c.int_or(1, 0) != 0,
                        pan: if kind == 1 { key(18).map_or(-1, |v| v as i16) } else { -1 },
                        speaker: if kind == 1 { key(70).unwrap_or(0) } else { 0 },
                        vol_end: -1,
                        pan_end: -1,
                    }
                };
                self.se_delayed.push((q, (frames - 1) as f32 * 1000.0 / 60.0, true));
            }
            return;
        }
        if c.name == "SEAC" {
            // SEAC (sub_6C0A0) writes and runs SEPL(ch, loop, sound, , `017:volume (0 -> 1, none without one),
            // `018:pan), then SEVL(ch, , , `017:end volume, `001:ms) for an end volume >= 0 other than the start,
            // then SEPN(ch, end pan, ms) for an end pan other than the start
            let vol = c.int(3).map(|v| if v == 0 { 1 } else { v });
            let vol_end = c.int(4).unwrap_or(-1);
            let pan = c.int_or(5, 64);
            let pan_end = c.int_or(6, pan);
            let ms = c.int_or(7, 0);
            let cmd = |name: &str, args: Vec<String>| Command {
                op: knife_core::vm::parse::Op::Cmd(name.as_bytes().try_into().unwrap()),
                name: name.to_string(),
                args,
                offset: c.offset,
                legacy_offset: c.legacy_offset,
            };
            let mut args = vec![ch.to_string(), c.int_or(2, 0).to_string(), c.arg(1).unwrap_or("").to_string(), String::new()];
            args.extend(vol.map(|v| format!("`017:{v}")));
            args.push(format!("`018:{pan}"));
            self.cmd_se(&cmd("SEPL", args));
            if vol_end >= 0 && vol_end != vol.unwrap_or(-1) {
                let args = vec![ch.to_string(), String::new(), String::new(), format!("`017:{vol_end}"), format!("`001:{ms}")];
                self.cmd_se(&cmd("SEVL", args));
            }
            if pan_end != pan {
                self.cmd_se(&cmd("SEPN", vec![ch.to_string(), pan_end.to_string(), ms.to_string()]));
            }
            return;
        }
        let fade = c.keyed_int(1).unwrap_or(0);
        match c.name.as_str() {
            "SEPL" => {
                let ch = ch % 20;
                let looping = c.int_or(1, 0) == 1;
                let Some(name) = c.arg(2) else {
                    // no sound: the channel stops (sub_9CF90) and its loop word clears
                    if seek {
                        self.seek_audio.as_mut().unwrap().loops.remove(&ch);
                    } else {
                        self.sound.stop_se(ch);
                    }
                    return;
                };
                let raw = c.keyed_int(17);
                let mut vol = raw.unwrap_or(100).clamp(0, 100);
                let mut word = raw.map_or(100, |v| v as i16);
                let pan = c.keyed_int(18).map_or(0, |p| (1000 * p - 64000) / 64);
                // a loop already looping on the channel at a volume is left alone; under the fast skip (AE0670) a
                // one-shot SE does not play and stops the channel's loop (both also during the seek, on its words)
                let cur_loop = match &self.seek_audio {
                    Some(sa) => sa.loops.get(&ch).map(|l| l.0.clone()),
                    None => self.sound.looping_on(ch).map(str::to_string),
                };
                if looping && cur_loop.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(name)) && self.sound.se_word[ch as usize] > 0 {
                    return;
                }
                self.sound.se_loop[ch as usize] = looping;
                if !looping && self.skip {
                    match &mut self.seek_audio {
                        Some(sa) => {
                            sa.loops.remove(&ch);
                        }
                        None if cur_loop.is_some() => self.sound.stop_se(ch),
                        None => {}
                    }
                    return;
                }
                if let Some(sa) = &mut self.seek_audio {
                    if looping {
                        sa.loops.insert(ch, (name.to_string(), 0.0));
                    } else {
                        sa.loops.remove(&ch);
                    }
                }
                // 0x6ac64: key 70 binds the channel to that speaker (sub_9CF70; 0 otherwise); with voices off
                // or the speaker's voice off the volume and its word become 0
                let speaker = c.keyed_int(70);
                if speaker.is_some_and(|s| !self.se_speaker_on(s)) {
                    vol = 0;
                    word = 0;
                }
                self.sound.bind_speaker(ch, speaker.and_then(|s| usize::try_from(s - 1).ok()).filter(|&r| r < 19));
                if seek {
                    let sa = self.seek_audio.as_mut().unwrap();
                    if let Some(l) = sa.loops.get_mut(&ch) {
                        l.1 = f32::from(word) / 100.0;
                    }
                } else {
                    // a fade-in (key 1 given, even 0) on a channel still fading out takes a second longer
                    let fade = if c.keyed_int(1).is_some() && self.sound.se_state(ch) == 2 { fade + 1000 } else { fade };
                    if let Some(pcm) = self.assets.sound_effect(name) {
                        let fade = if fade < 2 { 0.0 } else { f64::from(fade) / 1000.0 };
                        self.sound.play_se(ch, name, pcm, vol as f32 / 100.0, fade, looping, pan as f32 / 1000.0);
                    } else {
                        self.sound.stop_se(ch);
                    }
                }
                self.sound.se_word[ch as usize] = word;
                // the pan word (a queued SEPL plays with its pan but leaves the word)
                self.sound.se_pan[ch as usize] = pan as i16;
            }
            "SEFD" => {
                for k in if ch % 20 < 0 { 0..20 } else { ch % 20..ch % 20 + 1 } {
                    // a queued SEPL on the channel is dropped (a queued slot's counter is still >= 1)
                    self.se_delayed.retain(|d| !(d.0.channel == k && d.0.kind == 1));
                    if let Some(sa) = &mut self.seek_audio {
                        sa.loops.remove(&k);
                    } else if fade < 1 {
                        self.sound.stop_se(k);
                    } else {
                        // sub_9D020: from the player's current volume to 0 over the fade
                        let cur = self.sound.se_player_volume(k) * 100.0;
                        self.sound.forget_loop(k);
                        self.sound.fade_se(k, cur * 1000.0 / 60.0 / fade as f32);
                    }
                    self.sound.se_word[k as usize] = 0;
                }
            }
            "SEVL" => {
                let k = ch % 20;
                let Ok(ku) = usize::try_from(k) else { return };
                let mut vol = c.keyed_int(17).unwrap_or(0).clamp(0, 100);
                // a queued SEPL on the channel takes the volume instead
                if let Some(d) = self.se_delayed.iter_mut().find(|d| d.0.channel == k && d.0.kind == 1) {
                    d.0.volume = vol as i8;
                    return;
                }
                if self.sound.chan_speaker(k).is_some_and(|r| !self.se_speaker_on(r as i32 + 1)) {
                    vol = 0;
                }
                if !seek {
                    let rate = if fade < 1 {
                        2000.0
                    } else {
                        (vol - i32::from(self.sound.se_word[ku])) as f32 * 1000.0 / 60.0 / fade as f32
                    };
                    self.sound.ramp_se(k, vol as f32 / 100.0, rate);
                    self.sound.se_word[ku] = vol as i16;
                }
            }
            _ => {}
        }
    }

    /// sub_65A60: a queued SE op runs (its slot then clears).
    fn run_se_queued(&mut self, q: &SeQueued) {
        let (ch, chu) = (q.channel, q.channel as usize);
        let seek = self.seek_audio.is_some();
        let word = i32::from(self.sound.se_word[chu]);
        match q.kind {
            // sub_65780
            1 => {
                self.sound.se_loop[chu] = q.looping;
                let Some(name) = q.name.as_deref() else {
                    self.sound.stop_se(ch);
                    return;
                };
                if seek {
                    let sa = self.seek_audio.as_mut().unwrap();
                    if q.looping {
                        sa.loops.insert(ch, (name.to_string(), f32::from(self.sound.se_word[chu]) / 100.0));
                    } else {
                        sa.loops.remove(&ch);
                    }
                    return;
                }
                if !q.looping && self.skip {
                    if self.sound.looping_on(ch).is_some() {
                        self.sound.stop_se(ch);
                    }
                    return;
                }
                let mut vol = i32::from(q.volume);
                if q.speaker >= 1 && !self.se_speaker_on(q.speaker) {
                    vol = 0;
                }
                self.sound.bind_speaker(ch, usize::try_from(q.speaker - 1).ok().filter(|&r| r < 19));
                let fade = if q.fade < 1 {
                    0
                } else if self.sound.se_state(ch) == 2 {
                    q.fade + 1000
                } else {
                    q.fade
                };
                let pan = if q.pan < 0 { 0 } else { (1000 * i32::from(q.pan) - 64000) / 64 };
                if let Some(pcm) = self.assets.sound_effect(name) {
                    let secs = if fade < 2 { 0.0 } else { f64::from(fade) / 1000.0 };
                    self.sound.play_se(ch, name, pcm, vol.clamp(0, 100) as f32 / 100.0, secs, q.looping, pan as f32 / 1000.0);
                } else {
                    self.sound.stop_se(ch);
                }
                if vol < 0 {
                    self.sound.se_word[chu] = 100;
                } else {
                    // the rate then comes from the volume word: a fade-in to the channel's last volume steps at
                    // the minimum 0.2 points a frame
                    self.sound.ramp_se(ch, vol as f32 / 100.0, (vol - word) as f32 * 1000.0 / 60.0 / fade.max(1) as f32);
                    self.sound.se_word[chu] = vol as i16;
                }
            }
            2 => {
                // Witch on the Holy Night (sub_465C0) does nothing without a loop on the channel (+100 < 0) and
                // keeps the volume word
                let witch = self.assets.build == Build::Witch;
                let looping = match &self.seek_audio {
                    Some(sa) => sa.loops.contains_key(&ch),
                    None => self.sound.looping_on(ch).is_some(),
                };
                if witch && !looping {
                    return;
                }
                if let Some(sa) = &mut self.seek_audio {
                    sa.loops.remove(&ch);
                } else if q.fade > 0 && word > 0 {
                    // sub_9CFE0: the rate from the volume word, not the player's volume
                    self.sound.forget_loop(ch);
                    self.sound.fade_se(ch, word as f32 * 1000.0 / 60.0 / q.fade as f32);
                } else {
                    self.sound.stop_se(ch);
                }
                if !witch {
                    self.sound.se_word[chu] = 0;
                }
            }
            // SEAC (sub_65A60 case 4): a missing start volume / pan takes the end one, the SE plays as a queued
            // SEPL, then the volume (0 counts as 1) and the pan move to their ends, at once without a duration
            4 => {
                let vol = if q.volume == -1 && q.vol_end != -1 { q.vol_end } else { q.volume };
                let pan = if q.pan == -1 && q.pan_end != -1 { q.pan_end } else { q.pan };
                self.run_se_queued(&SeQueued { kind: 1, volume: vol, pan, ..q.clone() });
                let vol = if vol == 0 { 1 } else { vol };
                let dur = q.fade as i16;
                if q.vol_end >= 0 && vol != q.vol_end && !seek {
                    let word = i32::from(self.sound.se_word[chu]);
                    let to = i32::from(q.vol_end);
                    let rate = if dur >= 1 { (to - word) as f32 * 1000.0 / 60.0 / f32::from(dur) } else { 2000.0 };
                    self.sound.ramp_se(ch, to as f32 / 100.0, rate);
                    self.sound.se_word[chu] = to as i16;
                }
                if q.pan_end >= 0 && pan != q.pan_end && !seek {
                    let x = |p: i16| ((1000 * i32::from(p) - 64000) / 64).clamp(-1000, 1000);
                    let (from, to) = (x(pan), x(q.pan_end));
                    if dur >= 1 {
                        self.sound.ramp_se_pan(ch, from, to, (to - from) as f32 * 1000.0 / 60.0 / f32::from(dur));
                    } else {
                        self.sound.set_se_pan(ch, to);
                    }
                    self.sound.se_pan[chu] = to as i16;
                }
            }
            3 if !seek && q.volume >= 0 => {
                let vol = i32::from(q.volume);
                self.sound.ramp_se(ch, vol as f32 / 100.0, (vol - word) as f32 * 1000.0 / 60.0 / q.fade.max(1) as f32);
                self.sound.se_word[chu] = vol as i16;
            }
            _ => {}
        }
    }

    /// sub_65E50: `ms` of frames pass for the queued SE commands; one queued this frame already had its first
    /// frame (`se_delayed`). Due ones run (the tolerance keeps f32 steps of 1000 / 60 from adding a frame).
    fn tick_se_delayed(&mut self, ms: f32) {
        if self.se_delayed.is_empty() {
            return;
        }
        for d in &mut self.se_delayed {
            d.1 -= if std::mem::take(&mut d.2) { (ms - 1000.0 / 60.0).max(0.0) } else { ms };
        }
        let due: Vec<SeQueued> = self.se_delayed.extract_if(.., |d| d.1 <= 0.01).map(|d| d.0).collect();
        for q in due {
            self.run_se_queued(&q);
        }
    }

    /// The queued SE commands as a save keeps them (the exe's frame counters before this frame's count-down).
    fn se_queue_save(&self) -> Vec<SeQueued> {
        self.se_delayed
            .iter()
            .map(|(q, ms, fresh)| SeQueued { frames: ((ms * 60.0 / 1000.0).round() as i16 + i16::from(*fresh)).max(1), ..q.clone() })
            .collect()
    }

    /// VPLY / MSAD / WTTM / `_ZY` (0x78174 / 0x7f498 / 0x7f4f0 / 0x78f40) also push a type 9 ring entry.
    /// The stage becomes the backup first, the entry is pushed with F991 still set, then F991 clears; nothing
    /// happens during the load label scan (byte_146E638).
    fn refresh_backup(&mut self) {
        if self.var("F991") != 0 {
            self.stage_backup = Some(self.stage_bytes());
            self.push_ring_event(9);
            self.set_var("F991", 0);
        }
    }

    fn cmd_stch(&mut self, r: &Renderer, c: &Command) {
        let li = c.int_or(0, -1);
        if c.name == "STCG" {
            // STCG(image) (0x7b214) only marks the image seen: its own table index, when below 2701. Witch on the
            // Holy Night (0x5a6bc): the index when at most 2880 (BG_ERROR, not a name, else), through jp_644A0
            // (a variant counts as its base image) when below 3200.
            let idx = c.arg(0).and_then(|n| self.assets.image_index_of(&n.to_ascii_uppercase()));
            if crate::assets::witch() {
                if let Some(n) = c.arg(0).filter(|_| idx.is_some_and(|i| i <= 2880))
                    && let Some(b) = self.seen_bit(&n.to_ascii_uppercase())
                {
                    set_bit(&mut self.seen, b);
                }
            } else if let Some(i) = idx.filter(|&i| i < records::SEEN_DIRECT) {
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
        let dest_prev = self.stage.layers.get(li.max(0) as usize).cloned().flatten();
        let src_prev = src_layer.and_then(|s| self.stage.layers.get(s as usize).cloned().flatten());
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
        let x = c.int(2).map(|v| v as f32).or(prev.as_ref().map(|p| p.x.value_x())).unwrap_or(240.0);
        let y = c.int(3).map(|v| v as f32).or(prev.as_ref().map(|p| p.y.value())).unwrap_or(136.0);
        // STCH runs cmd_STCH_impl / jp_60840 with flag 1, SCH2 with 0: STCH's missing priority is 10 x layer and
        // its missing alpha 255, also over a source layer's copied words; SCH2 keeps them
        let fresh = c.name == "STCH";
        let prio = c.int(6).or(prev.as_ref().filter(|_| !fresh).map(|p| p.stage_prio())).unwrap_or(li * 10);
        // the exe marks shown images in a persistent bitmap (sub_774F0) for gallery unlocks, BLACK and WHITE
        // (table indices 0 and 5) too (Witch on the Holy Night: at the commit, `commit_stage`)
        if !crate::assets::witch()
            && let Some(b) = self.seen_bit(variant.as_deref().unwrap_or(&upper))
        {
            set_bit(&mut self.seen, b);
        }
        let mut l = Layer::new(variant.unwrap_or(base), source, x, y, prio);
        if let Some(p) = &prev {
            l.zoom_x = Tween::fixed(p.zoom_x.value());
            l.zoom_y = Tween::fixed(p.zoom_y.value());
            l.alpha = Tween::fixed(p.alpha.value());
            l.rotation = Tween::fixed(p.rotation.value_rot());
            l.stage_rot = p.stage_rot;
            // without a source layer cmd_STCH_impl resets the clip parent (word 1102) to -1 and the SACL byte,
            // but leaves the SBAS base (1182), the clip words (651..), inset and SOXY as the slot had them
            if src_layer.is_some() {
                l.parent = p.parent;
            }
            l.base = p.stage_base();
            l.clip = p.clip;
            l.rot2 = p.stage_rot2();
            l.clip_inset = p.clip_inset;
            l.soxy = p.stage_soxy();
            // a source layer's quake slots come along; otherwise cmd_STCH_impl clears them
            if src_layer.is_some() {
                l.quake = p.quake;
            }
            // Witch on the Holy Night's slot stays the destination's own render slot (jp_60840); a source layer
            // gives only its stage words, else they are zeroed with D and the delay
            if let Some(d) = &dest_prev {
                l.wquake = d.wquake;
            }
            if src_layer.is_some() {
                (l.wquake.stage, l.wquake.stage_kind) =
                    src_prev.as_ref().map_or(([0; 6], 0), |sp| (sp.wquake.stage, sp.wquake.stage_kind));
            } else {
                l.wquake.stch_clear();
            }
            // cmd_STCH_impl resets the stage anchor on every call: without a source layer to the centre cut to
            // whole units (the defaults, `Image::anchor`); with one (SCH2 arg 18) to the source's pivot moved by
            // the difference of the two centres, the shift word (int) minus that
            if src_layer.is_some()
                && let (Source::Image(si), Source::Image(img)) = (&p.source, &l.source)
            {
                let sp = p.stage_pivot.unwrap_or(si.anchor());
                let moved = [sp[0] - si.center_x + img.center_x, sp[1] - si.center_y + img.center_y];
                l.stage_pivot = Some(moved);
                l.stage_shift = Some([-moved[0].trunc(), -moved[1].trunc()]);
                l.pivot = l.stage_pivot;
                l.shift = l.stage_shift;
            }
            // the commit reloads the texture only when the layer's image changes
            if src_layer.is_none() && p.name == l.name && (!p.pending || p.keeps_texture) {
                l.keeps_texture = true;
                l.source = p.source.clone();
                l.fill = p.fill;
                l.shade = p.shade;
                l.bake = p.bake;
            }
            // sub_32930 clears the haze id and phase; the fade words stay
            l.haze = crate::stage::Haze { id: 0, phase: 0, entry: None, ..p.haze };
            if src_layer.is_some() {
                l.fill = p.fill;
                l.shade = p.shade;
                l.stec_mode = p.stec_mode;
                l.stec_color = p.stec_color;
                l.snx = p.snx;
                l.snx4 = p.snx4;
                if p.bake.cpu()
                    && let Source::Image(_) = l.source
                {
                    l.bake = p.bake;
                    l.regrade = true;
                }
            }
        }
        // cmd_STCH_impl LABEL_296: the STBR byte (stage +124) becomes F982 when that is >= 1 and the image (arg 1,
        // sub_55D30) lacks ImageInfo +65 bit 0, else -1 (none), also over a source layer's copied byte
        let f982 = self.var("F982");
        let own_blend = self.assets.image_index_of(&upper).and_then(|i| self.assets.tables.images.get(i)).is_some_and(|i| i.own_blend);
        l.stbr = if f982 >= 1 && !own_blend { f982 as i8 as i32 } else { -1 };
        l.blend = if l.stbr >= 0 { blend_mode(l.stbr).0 } else { Blend::Alpha };
        if let Some(z) = c.int(4) {
            l.zoom_x = Tween::fixed(z as f32 / 1000.0);
            l.zoom_y = Tween::fixed(z as f32 / 1000.0);
        }
        match c.int(5) {
            Some(a) => l.alpha = Tween::fixed(a.clamp(0, 255) as f32 / 255.0),
            None if fresh => l.alpha = Tween::fixed(1.0),
            None => {}
        }
        if let Some(p) = c.int(11) {
            l.parent = (p >= 0).then_some(p as usize);
        }
        // args 12..15 each replace one clip word
        let clip_args = [c.int(12), c.int(13), c.int(14), c.int(15)];
        if clip_args.iter().any(Option::is_some) {
            let mut cr = l.clip.unwrap_or_default();
            for (v, a) in [&mut cr.x, &mut cr.y, &mut cr.w, &mut cr.h].into_iter().zip(clip_args) {
                if let Some(a) = a {
                    *v = a as f32;
                }
            }
            l.clip = Some(cr);
        }
        // arg 16: scissor inset (stage word 1462), arg 17: its mode (byte 3004, 0 when absent)
        if let Some(v) = c.int(16) {
            l.clip_inset = v as i16;
        }
        l.clip_mode = c.int_or(17, 0) as i8;
        // LABEL_245: a layer with a clip parent takes the parent slot's clip words, inset and mode (an SACL child
        // keeps them; sub_37F50 overwrites the others' at every draw)
        if let Some(b) = l.parent.filter(|&b| b != li as usize && b < crate::stage::LAYERS) {
            (l.clip, l.clip_inset, l.clip_mode) = match &self.stage.layers[b] {
                Some(p) => (p.clip, p.clip_inset, p.clip_mode),
                None => (None, 0, 0),
            };
        }
        // arg 19: the STRR clip rotation (stage word 1522), wrapped to +-180 degrees
        if let Some(v) = c.int(19) {
            l.rot2 = wrap_rot2(v);
        }
        // jp_60840: STCH (and SCH2 with an alpha) sets the STGS words to (alpha, 0) and the curve byte to 0, the
        // alpha being argument 5 or 255; SCH2 without one keeps the slot's
        if c.name == "STCH" || c.int(5).is_some() {
            let a = c.int(5).map_or(255, |v| v.clamp(0, 255)) as i16;
            (l.wfade, l.wfade_ease) = ([a, 0], 0);
        } else if let Some(d) = src_prev.as_ref().or(dest_prev.as_ref()) {
            // a source layer's words come along (jp_60840 LABEL_52)
            (l.wfade, l.wfade_ease) = (d.wfade, d.wfade_ease);
        }
        self.stage.set(li, l);
        // script_dispatch 0x7b274: a done STCH / SCH2 sets words 0x7C8 / 0x7CA
        self.stage_dirty = true;
    }

    /// The clock hands' rotations in 1/100 degrees, wrapped to -18000..18000: the hour hand 30 degrees an hour and
    /// 0.5 a minute, the minute hand 6 degrees a minute (sub_73A0 / sub_7BB0).
    fn clock_hands(h: i32, m: i32) -> (i32, i32) {
        (-18000 * (h / 6) + 3000 * (h % 6) + 50 * m, 600 * (m % 30) - 18000 * (m / 30))
    }

    /// CKDP(layer, hour, minute, x, y, zoom = 1000, mode) (Witch on the Holy Night, sub_73A0): a clock on layers
    /// layer..layer + 4 (layer <= 34), written to the stage only like STCH: the face img1242 centred at (x + 820,
    /// y + 635), the hour hand img1244 92 stage units (640x480) up and the minute hand img1246 124 up, each
    /// turning about its fixed pivot, and under each hand its `_02_02` shadow at alpha 64, a black fill (STEC
    /// mode 1, colour 0) offset by (9 * 480 / 1024, 13 * 272 / 576) stage units times the zoom. Arg 6 is the
    /// STEC mode of the face and hands (a grade; 1 makes them black too). Priorities 100, 103 / 101, 104 / 102;
    /// the STBR byte -1.
    fn cmd_ckdp(&mut self, r: &Renderer, c: &Command) {
        let Some(li) = c.int(0).filter(|&l| (0..=34).contains(&l)) else { return };
        let (hour, minute) = Self::clock_hands(c.int_or(1, 0), c.int_or(2, 0));
        // stage units (x / 1280 * 640, y / 720 * 480); the layer keeps script units (x 2, y 1.5 of them)
        let x = (c.int_or(3, 0) + 820) as f32 * 640.0 / 1280.0;
        let y = (c.int_or(4, 0) + 635) as f32 * 480.0 / 720.0;
        let zoom = c.int_or(5, 1000);
        let mode = c.int_or(6, 0);
        let z = zoom as f32 / 1000.0;
        let dx = z * 9.0 * 480.0 * 0.00097656 * 640.0 / 1280.0;
        let dy = z * 13.0 * 272.0 / 576.0 * 480.0 / 720.0;
        // image, position, alpha, priority, pivot / shift (None: the face's half size), rotation, STEC mode
        type Part = (&'static str, f32, f32, i32, i32, Option<([f32; 2], [f32; 2])>, i32, i32);
        let parts: [Part; 5] = [
            ("img1242", x, y, 255, 100, None, 0, mode),
            ("img1244", x, y - 92.0, 255, 103, Some(([41.5, 388.0], [-41.0, -388.0])), hour, mode),
            ("img1244_02_02", x + dx, y - 93.0 + dy, 64, 101, Some(([46.2, 394.0], [-46.0, -394.0])), hour, 1),
            ("img1246", x, y - 124.0, 255, 104, Some(([37.5, 513.0], [-37.0, -513.0])), minute, mode),
            ("img1246_02_02", x + dx, y - 124.0 + dy, 64, 102, Some(([42.5, 517.5], [-42.0, -517.0])), minute, 1),
        ];
        for (k, (name, px, py, alpha, prio, pivot, rot, stec)) in parts.into_iter().enumerate() {
            let slot = li + k as i32;
            // sub_42250: the image, else BG_ERROR
            // (the commit marks the images seen, `commit_stage`)
            let Some(img) = self.assets.image(r, name).or_else(|| self.assets.image(r, "BG_ERROR")) else { continue };
            let (pivot, shift) = pivot.unwrap_or_else(|| {
                // the ImageInfo size (floats 11 / 12), not the texture's that the draw uses
                let (w, h) = self.assets.image_info(name).map_or((img.width, img.height), |i| (i.width, i.height));
                let (w, h) = (w * 0.5, h * 0.5);
                ([w, h], [(-w).trunc(), (-h).trunc()])
            });
            let prev = self.stage.layers.get(slot as usize).cloned().flatten();
            let mut l = match prev {
                Some(p) => {
                    let keep = p.name == name && (!p.pending || p.keeps_texture);
                    let mut l = Layer { name: name.to_string(), pending: true, keeps_texture: keep, ..p };
                    if !keep {
                        l.source = Source::Image(img);
                        l.fill = None;
                        l.shade = Shade::Sprite;
                        l.bake = crate::grade::Bake::NONE;
                        l.regrade = false;
                    }
                    l
                }
                None => Layer::new(name.to_string(), Source::Image(img), 0.0, 0.0, prio),
            };
            l.x = Tween::fixed(px * 2.0);
            l.y = Tween::fixed(py * 1.5);
            l.zoom_x = Tween::fixed(z);
            l.zoom_y = Tween::fixed(z);
            l.alpha = Tween::fixed(alpha as f32 / 255.0);
            (l.prio, l.stbr, l.blend) = (prio, -1, Blend::Alpha);
            (l.staged.prio, l.staged.stbr) = (None, None);
            l.stage_pivot = Some(pivot);
            l.stage_shift = Some(shift);
            l.stage_rot = Some(rot as f32 / 100.0);
            l.stec_mode = stec;
            l.stec_color = if stec == 1 { [0.0; 3] } else { [1.0; 3] };
            self.stage.set(slot, l);
            self.rebake(slot);
        }
        self.stage_dirty = true;
    }

    /// CKTT(layer, hour, minute) (Witch on the Holy Night, sub_7BB0): the hands of a CKDP clock turn to the new
    /// time over 18 frames, linear (kind 70), from the hour / minute hand's stage rotation (layers + 1 / + 3);
    /// the render tweens start at once and the stage rotations take the targets.
    fn cmd_cktt(&mut self, c: &Command) {
        let Some(li) = c.int(0).filter(|&l| (0..=34).contains(&l)) else { return };
        let (hour, minute) = Self::clock_hands(c.int_or(1, 0), c.int_or(2, 0));
        let stage_rot = |l: &Option<Layer>| {
            l.as_ref().map_or(0, |l| (l.stage_rot.unwrap_or(l.rotation.to) * 100.0).round() as i32)
        };
        let from_hour = stage_rot(&self.stage.layers[li as usize + 1]);
        let from_minute = stage_rot(&self.stage.layers[li as usize + 3]);
        for (k, from, to) in [(1, from_hour, hour), (2, from_hour, hour), (3, from_minute, minute), (4, from_minute, minute)] {
            if let Some(l) = self.stage.get(li + k) {
                // sub_326E0 keeps the start as an i16
                l.rotation = Tween::new(from as i16 as f32 / 100.0, to as f32 / 100.0, 18, 70);
                l.stage_rot = None;
            }
        }
    }

    /// sub_63520 on one layer, or for a negative one on every layer with an image (sub_91540 / sub_91900).
    fn start_quake(&mut self, li: i32, slot: usize, q: crate::stage::Quake) {
        for (k, l) in self.stage.layers.iter_mut().enumerate() {
            if (li < 0 || k as i32 == li)
                && let Some(l) = l
            {
                l.quake[slot] = q;
            }
        }
    }

    /// sub_6E0D0: a voice plays unless voices are off or its speaker's row is muted, at that row's volume.
    /// Neither a message start nor VPL2 (0x7a450) plays one during the next-scene seek (off_AE0568).
    /// `replay`: it becomes the voice the replay button plays (AE06A8), as at a message start, which first stops
    /// both voice players (sub_9C3B0); VPL2 leaves the last voice playing.
    fn play_voice(&mut self, n: &str, replay: bool) {
        if self.skip || self.seek_node || !self.settings.voice_on {
            return;
        }
        tracing::debug!("voice {n}");
        if replay {
            self.last_voice = Some(n.to_string());
        }
        let row = voice_row(n);
        if self.speaker_voice_on(n) {
            let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
            match self.assets.voice(n) {
                Ok(pcm) => self.sound.play_voice_gain(Arc::new(pcm), gain, replay),
                Err(e) => tracing::warn!("voice {n}: {e:#}"),
            }
        }
    }

    /// The voice's config row is not muted (off_AE03D8 + 0x24 / WoH + 0x3C: the row's enable bit).
    fn speaker_voice_on(&self, n: &str) -> bool {
        self.settings.char_mute >> voice_row(n) & 1 == 0
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

    /// `name` as the scene voice at voice row `row`'s volume, both voice players stopped first (sub_4E170:
    /// sub_AA9E0 = sub_9C3B0, then sub_AA970).
    pub fn play_test_voice(&mut self, name: &str, row: usize) {
        let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
        match self.assets.voice(name) {
            Ok(pcm) => self.sound.play_voice_gain(Arc::new(pcm), gain, true),
            Err(e) => tracing::warn!("voice {name}: {e:#}"),
        }
    }

    /// Config voice test (sub_2B930 case 1): `name` on sound player 4 at voice row `row`'s volume, leaving the
    /// scene voice playing.
    pub fn conf_test_voice(&mut self, name: &str, row: usize) {
        tracing::debug!("config voice test {name} (row {row})");
        let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
        self.sound.stop_test_voice();
        match self.assets.voice(name) {
            Ok(pcm) => self.sound.play_test_voice(Arc::new(pcm), gain),
            Err(e) => tracing::warn!("voice {name}: {e:#}"),
        }
    }

    /// sub_2A9B0 with a voice test row held: a muted row stops the scene voice (sub_9C3B0: players 2, 3).
    /// sub_9D240 then sets the row's volume on voice player sub_9B5A0() (0 / 1) of the voice group, which no
    /// voice plays on (scene voices are 2 / 3, sub_9C300; the config test 4), so the scene voice keeps its
    /// volume; the volume byte it writes (off_AE0350 + 1) is set again before every voice plays.
    pub fn conf_test_row_volume(&mut self, row: usize) {
        if self.settings.char_mute >> row & 1 != 0 {
            self.sound.stop_voice();
        }
    }

    /// Config SE test (sub_29140): the last test SE stops (sub_9CF90), then one of the 14 test effects
    /// (off_46B138, sub_A8F90(0, 13)) plays once on the first idle SE channel 0..19 at the SE volume without a
    /// fade (sub_9CAA0). Returns the channel (qword_46F810), or None when every channel is busy.
    pub fn conf_test_se(&mut self, last: Option<i32>) -> Option<i32> {
        if let Some(ch) = last {
            self.sound.stop_se(ch);
        }
        let ch = (0..20).find(|&c| !self.sound.se_busy(c))?;
        let name = TEST_SES[self.app_rng.range(0, 13) as usize];
        tracing::debug!("config SE test {name} on channel {ch}");
        let pcm = self.assets.sound_effect(name)?;
        self.sound.play_se(ch, name, pcm, 1.0, 0.0, false, 0.0);
        Some(ch)
    }

    // ---- messages -----------------------------------------------------------------

    /// Start a new page; `finished`: the old one goes to the backlog (not when a load, rewind or reset
    /// replaces it).
    fn clear_page(&mut self, finished: bool) {
        let mut texts = std::mem::take(&mut self.page.texts);
        texts.append(&mut self.page.choice_texts);
        let mut extras = std::mem::take(&mut self.page.extras);
        extras.append(&mut self.page.choice_extras);
        let (first, last) = std::mem::take(&mut self.page_ring);
        if finished && !texts.is_empty() {
            let bubble = self.assets.build == Build::Overseas && self.var("F1105") >= 3;
            self.backlog_pages.push(BacklogPage { texts, extras, first, last, bubble });
        }
        self.page.chars.clear();
        self.page.texts.clear();
        self.page.msgs.clear();
        self.page.zs.clear();
        self.page.lifted = None;
        self.page.order = 0;
        self.page.revealed = 0.0;
        let (lay, top) = self.text_layout();
        self.page.pen = (lay.left, top);
    }

    /// Text colour of the page's messages (sub_391A0 draws history lines with it): hi / lo byte of 0xDC6D
    /// (read text) when config u16 24 is 1, else 0xFFFF. Only a whole-config copy writes that word, so it is
    /// 0 on the Switch and every colour is 0xFFFF.
    const TEXT_COLOUR: u16 = 0xFFFF;

    /// Witch on the Holy Night's bubble text (jp_39010 false: window type F1105, or F1109 > 0 in English, at
    /// least 3): colour 31200 and text flags 0x180000 (the bubble font, no edge or shadow).
    fn witch_bubble_text(&self) -> bool {
        self.assets.build == Build::Witch && self.window_type().is_some_and(|t| t >= 3)
    }

    /// The current message's colour (word_459424, jp_6E0D0 / MSAD jp_6E950): Witch on the Holy Night's mode 1
    /// tints read text (sub_53A60) 0xDC6D, mode 2 takes the speaker table (every entry 0xFFFF, bubbles too);
    /// bubble text is 31200 otherwise.
    fn msg_colour(&self) -> u16 {
        if self.assets.build != Build::Witch {
            return Self::TEXT_COLOUR;
        }
        let bubble = self.witch_bubble_text();
        match self.settings.text_colour {
            1 if self.line_read && !bubble => 0xDC6D,
            2 => 0xFFFF,
            _ if bubble => 31200,
            _ => 0xFFFF,
        }
    }

    /// The value `reset_colour_words` gives the per-message colour words: mode 1 makes the page's messages read
    /// (0xDC6D), bubbles 31200.
    fn line_reset_colour(&self) -> u16 {
        match self.assets.build {
            Build::Witch if self.witch_bubble_text() => 31200,
            Build::Witch if self.settings.text_colour == 1 => 0xDC6D,
            _ => Self::TEXT_COLOUR,
        }
    }

    /// Colour of history message `i` on the page (jp_391A0): mode 1 tints them all 0xDC6D (bubbles 31200) while
    /// the current message is read, else each keeps its words F(800 + i) / F(811 + i); otherwise 0xFFFF,
    /// bubbles 31200.
    fn history_colour(&self, i: usize) -> u16 {
        if self.assets.build != Build::Witch {
            return Self::TEXT_COLOUR;
        }
        let bubble = self.witch_bubble_text();
        match self.settings.text_colour {
            1 if self.line_read => if bubble { 31200 } else { 0xDC6D },
            1 => (self.var(&format!("F{}", 800 + i.min(10))) << 8 | self.var(&format!("F{}", 811 + i.min(10))) & 0xFF) as u16,
            _ if bubble => 31200,
            _ => 0xFFFF,
        }
    }

    /// Page mode (F822 == 1, set by START_SCRIPT): a page keeps its earlier messages (sub_391A0).
    fn page_mode(&self) -> bool {
        self.var("F822") == 1
    }

    /// The message layout and its first baseline (sub_391A0: page mode, else LABEL_135). Window types 3..32
    /// put the text at the bubble's text origin (unk_34DAD8, plus the F1106 / F1107 offsets), flt_34DEFC wide.
    fn text_layout(&self) -> (Layout, f32) {
        let base = msg_layout();
        if !self.page_mode() {
            let right = (WINDOW_LEFT + self.msg_width()) * 3.0;
            return (Layout { left: WINDOW_LEFT * 3.0, right, ..base }, WINDOW_TOP_BASELINE - ASCENT + base.ascent);
        }
        match self.window_type() {
            Some(t) if self.is_bubble(t) => {
                let (left, top) = self.bubble_text_origin(t, self.window_offset());
                let right = left + self.bubble_width(t) * UX;
                // Witch on the Holy Night's English bubbles scale the glyphs 0.85 wide and 0.95 tall (jp_6E0D0,
                // jp_391A0); the line feed follows the height
                let (k, sx) = if self.assets.build == Build::Witch && text::english() { (0.95, 0.85 / 0.95) } else { (1.0, 1.0) };
                let size = base.size * k;
                let mut line_height = base.line_height + (k * text::bubble_line_feed() - text::line_feed()) * base.size / text::BASE_SIZE;
                // Witch on the Holy Night's bubble flag 0x100000 (jp_A7790): line spacing a8 - 14 + 6, 8 px of 1080
                // closer (Switch: TeaTime bubble lines 82 px apart in the shared font's line feed 60)
                if self.assets.build == Build::Witch {
                    line_height -= 8.0 * CANVAS_H as f32 / 1080.0;
                }
                if self.z_cur == Some(true) {
                    line_height -= 8.0 * CANVAS_H as f32 / 1080.0;
                }
                // the bubble font's own pair ascent (`text::bubble_ascent`)
                let ascent = base.ascent * k * text::bubble_ascent();
                (Layout { left, right, ascent, size, sx, line_height, ..base }, top + ascent)
            }
            _ => match self.z_cur {
                // `@z` in a page slot: the shared font's line feed and pair ascent (`text::bubble_ascent`), as
                // a bubble's
                Some(tight) => {
                    let mut line_height = base.line_height + (text::bubble_line_feed() - text::line_feed()) * base.size / text::BASE_SIZE;
                    if tight {
                        line_height -= 8.0 * CANVAS_H as f32 / 1080.0;
                    }
                    let ascent = base.ascent * text::bubble_ascent();
                    (Layout { ascent, line_height, ..base }, top_baseline() - base.ascent + ascent)
                }
                None => (base, top_baseline()),
            },
        }
    }

    /// The window types of the build (unk_34D94C; the overseas build's flt_231D74 has 56; Witch on the Holy
    /// Night's 99, `witch::WINDOWS`, origins converted to 1280x720 units).
    fn windows(&self) -> &'static [(u16, f32, f32)] {
        static WITCH_WINDOWS: std::sync::LazyLock<Vec<(u16, f32, f32)>> = std::sync::LazyLock::new(|| {
            crate::witch::WINDOWS.iter().map(|&(slot, x, y, _, _)| (slot, x * 2.0, y * 720.0 / 448.0)).collect()
        });
        match self.assets.build {
            Build::Japanese => &WINDOWS,
            Build::Overseas => &EN_WINDOWS,
            Build::Witch => &WITCH_WINDOWS,
        }
    }

    /// Window type `t`'s image slot and x / y scale (overseas sub_3A070): with English text, types 0..2 take
    /// slots 63..65 (dword_232014; also any language outside the 1920x1080 mode), and types 33..55 stretch
    /// their reused bubble images from the top left.
    fn window_image(&self, t: usize) -> Option<(u16, f32, f32)> {
        let &(slot, _, _) = self.windows().get(t)?;
        if self.assets.build == Build::Japanese {
            return Some((slot, 1.0, 1.0));
        }
        if self.assets.build == Build::Witch {
            // sub_1AF00: types 0..2 take slots 62..64 with English text (or outside the 1920x1080 mode)
            let &(_, _, _, sx, sy) = crate::witch::WINDOWS.get(t)?;
            let slot = if t <= 2 && text::english() { 62 + t as u16 } else { slot };
            return Some((slot, sx, sy));
        }
        let slot = if t <= 2 && text::english() { 63 + t as u16 } else { slot };
        let (sx, sy) = match t {
            33 | 50 | 51 => (1.12, 1.0),
            34 => (1.12, 1.09),
            35 | 37 => (1.07, 1.0),
            36 => (1.0, 1.09),
            39 | 52 | 53 | 55 => (1.11, 1.0),
            40 => (1.4, 1.0),
            41 | 42 => (1.14, 1.0),
            43 | 46 => (1.01, 1.0),
            47 | 48 => (1.1, 1.0),
            49 => (1.3, 1.0),
            54 => (1.06, 1.0),
            _ => (1.0, 1.0),
        };
        Some((slot, sx, sy))
    }

    /// The image slot of DISP_PARTS entry 0.
    fn disp_slot0(&self) -> u16 {
        if self.assets.build == Build::Witch { WITCH_DISP_SLOT0 } else { DISP_SLOT0 }
    }

    /// Window types 3.. are the QA corner bubbles.
    fn is_bubble(&self, t: usize) -> bool {
        (3..self.windows().len()).contains(&t)
    }

    /// The message slot's width in 640-wide units (sub_39050: English 573; Witch on the Holy Night sub_1AE20: 436,
    /// English 556).
    fn msg_width(&self) -> f32 {
        match (self.assets.build == Build::Witch, text::english()) {
            (true, true) => 556.0,
            (true, false) => 436.0,
            (false, true) => EN_WIDTH,
            (false, false) => 464.0,
        }
    }

    /// Text width of bubble type `t` in 640-wide units (flt_34DEFC; overseas sub_39050).
    fn bubble_width(&self, t: usize) -> f32 {
        match self.assets.build {
            Build::Japanese => BUBBLE_WIDTH[(t - 3) % 6 / 2],
            Build::Overseas if t - 3 < 40 && (EN_BUBBLE_WIDTH_MASK >> (t - 3)) & 1 != 0 => EN_BUBBLE_WIDTH[t - 3],
            Build::Overseas => self.msg_width(),
            Build::Witch => crate::witch::BUBBLE_WIDTH.get(t - 3).copied().unwrap_or_else(|| self.msg_width()),
        }
    }

    /// Text origin of bubble type `t` in canvas pixels: unk_34D94C + unk_34DAD8 + the window offset.
    fn bubble_text_origin(&self, t: usize, (dx, dy): (f32, f32)) -> (f32, f32) {
        if self.assets.build == Build::Witch {
            // WoH jp_391A0: flt_25E588[type] in 1024x576 units, centred on the line count F1108; the offsets are
            // 1280x720-unit values here (`window_offset`)
            let (tx, ty) = crate::witch::BUBBLE_TEXT.get(t).copied().unwrap_or((0.0, 0.0));
            let lines = self.var("F1108") as f32;
            let x = tx * 0.625 + dx * 0.5;
            let y = ty * 0.77778 - ((lines - 1.0) * 41.0 + 26.0) * 0.41481 + dy * 448.0 / 720.0;
            return (x * UX, y * UY);
        }
        let (_, wx, wy) = self.windows()[t];
        let (tx, ty) = match self.assets.build {
            Build::Japanese => BUBBLE_TEXT[(t - 3) / 2],
            Build::Overseas | Build::Witch => EN_BUBBLE_TEXT[t],
        };
        // 1280x720 units -> 640x448 -> canvas pixels
        ((wx + tx + dx) * 640.0 / 1280.0 * UX, (wy + ty + dy) * 448.0 / 720.0 * UY)
    }

    /// The second bubble (F1111 a bubble type, page mode): sub_391A0 draws it like the F1105 window with the
    /// F1112 / F1113 offsets. Witch on the Holy Night's jp_391A0 has none.
    fn second_bubble(&self) -> Option<usize> {
        if self.assets.build == Build::Witch {
            return None;
        }
        usize::try_from(self.var("F1111")).ok().filter(|&t| self.is_bubble(t) && self.page_mode())
    }

    /// Where sub_391A0 moves the current message with a second bubble: to the second bubble's text origin plus
    /// the message's own drop below the page's first line, one line (AFECD0 - AFECCC) up. Earlier messages
    /// stay at the F1105 layout. The width stays the F1105 one (flt_34DEFC[F1105 - 3], else 464).
    fn second_bubble_shift(&self) -> Option<(f32, f32)> {
        let t = self.second_bubble()?;
        let at = |t: &[f32], w: &str| usize::try_from(self.var(w)).ok().and_then(|i| t.get(i)).copied().unwrap_or(0.0);
        let (left, top) = self.bubble_text_origin(t, (at(&WINDOW_DX, "F1112"), at(&WINDOW_DY, "F1113")));
        let (lay, top1) = self.text_layout();
        Some((left - lay.left, top + ASCENT - top1 - lay.line_height))
    }

    /// A font of the language's list that load_ttf refuses (`tt::refused`: the SDK's stroke and compressed
    /// formats): nn::font loads every font of the list (sub_CA500) without checking the engine's error, and such a
    /// font stops the game (Switch, Astris: Times New Roman with head.glyphDataFormat 38485 as the message font
    /// stopped it at boot).
    fn refused_font(assets: &Assets) -> Option<std::path::PathBuf> {
        ["", "2", "3"]
            .into_iter()
            .filter_map(|s| assets.font_path(s))
            .find(|p| std::fs::read(p).is_ok_and(|d| knife_core::itype::tt::refused(&d)))
    }

    /// sub_AB0F0: the message font of the game language (the first file of its folder) and the QA bubble font
    /// (of `<folder>3`), each with the exfont runs (exfont_nx01.bffnt).
    pub fn load_fonts(assets: &Assets, r: &Renderer) -> anyhow::Result<(FontAtlas, Option<FontAtlas>)> {
        use anyhow::Context as _;
        let exfont = std::fs::read(assets.path("exfont_nx01.bffnt"));
        if let Err(e) = &exfont {
            tracing::warn!("exfont: {e}");
        }
        let with_exfont = |mut f: FontAtlas| {
            if let Ok(d) = &exfont {
                f.load_exfont(r, d).unwrap_or_else(|e| tracing::warn!("exfont: {e:#}"));
            }
            f
        };
        if let Some(p) = Self::refused_font(assets) {
            anyhow::bail!("{}: a font format the Switch's font engine refuses (load_ttf), which stops the game", p.display());
        }
        let path = assets.font_path("").with_context(|| format!("no font in {}", assets.language_folder()))?;
        let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut font = with_exfont(FontAtlas::new(data)?);
        // the rest of the font list (sub_CA500): `<folder>2`, `<folder>3`
        for (suffix, lang) in [("2", 1), ("3", 2)] {
            if let Some(d) = assets.font_path(suffix).and_then(|p| std::fs::read(p).ok()) {
                font.add_font(d, lang).unwrap_or_else(|e| tracing::warn!("font {suffix}: {e:#}"));
            }
        }
        font.set_otf_kerning(assets.otf_kerning());
        // the bubble variant takes a glyph from `<folder>3`, else from the message list's other fonts in order
        // (Switch, French fan fonts: FOT-Seurat's missing space and U+3000 blank, ⼀ ⼝ from TsukuMin, ～ – ⋯
        // from TsukuGo)
        let bubble_list = |mut f: FontAtlas| {
            f.set_first_lang(2);
            for (suffix, lang) in [("", 0), ("2", 1)] {
                if let Some(d) = assets.font_path(suffix).and_then(|p| std::fs::read(p).ok()) {
                    f.add_font(d, lang).unwrap_or_else(|e| tracing::warn!("font {suffix}: {e:#}"));
                }
            }
            f.set_otf_kerning(assets.otf_kerning());
            f
        };
        let bubble = match assets.font_path("3").map(|p| std::fs::read(&p).with_context(|| format!("reading {}", p.display()))) {
            Some(Ok(d)) => FontAtlas::new(d).map(bubble_list).map(with_exfont).map_err(|e| tracing::warn!("bubble font: {e:#}")).ok(),
            Some(Err(e)) => {
                tracing::warn!("bubble font: {e:#}");
                None
            }
            // Witch on the Holy Night loads one font per language (sub_85A90: the folder's file and the
            // exfont); its bubble text (flag 0x100000: slot +101793, font variant 3 in jp_C406C) draws in the
            // shared system font (Switch: the TeaTime bubbles and the save list's header in gothic)
            None if assets.build == Build::Witch => crate::ui::shared_font_atlas(assets.shared_fonts()).map(with_exfont),
            None => {
                tracing::warn!("no bubble font");
                None
            }
        };
        Ok((font, bubble))
    }

    /// The overseas config's language change (sub_2D910 states 10 / 11): the script text, SYSMES and the
    /// language's atlases and fonts (sub_9A940 / sub_AB0F0), then the shown page laid out again in the new
    /// language at once (sub_7FE20: flag 0x1000; sub_391A0 resolves the earlier history lines per frame) and
    /// the shown choice (sub_7D0C0). Returns whether the language changed.
    pub fn change_language(&mut self, r: &Renderer, word: i16) -> bool {
        match self.assets.set_language(word) {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                tracing::warn!("language {word}: {e:#}");
                return false;
            }
        }
        if let Some(p) = Self::refused_font(&self.assets) {
            tracing::error!("{}: a font format the Switch's font engine refuses (load_ttf), which stops the game", p.display());
            std::process::exit(1);
        }
        match Self::load_fonts(&self.assets, r) {
            Ok((font, bubble)) => {
                self.font = font;
                self.font_bubble = bubble;
                self.glyphs.clear();
            }
            Err(e) => tracing::warn!("fonts: {e:#}"),
        }
        self.disp = self.assets.ui_textures(r, &self.assets.ui_atlas("DISP_PARTS")).unwrap_or_default().into_iter().map(|(_, t)| t).collect();
        self.sysmes = None;
        self.relayout_page();
        self.relayout_choice();
        true
    }

    /// The page's messages laid out again from their script text (each at the start the page gives it, as
    /// sub_391A0 places them), fully shown; their read fades stay.
    fn relayout_page(&mut self) {
        let texts = std::mem::take(&mut self.page.texts);
        self.page.extras.clear();
        let msgs = std::mem::take(&mut self.page.msgs);
        self.relay_zs = std::mem::take(&mut self.page.zs);
        let (waiting, visible, msg_pc) = (self.page.waiting, self.page.visible, self.msg_pc);
        self.relaying = true;
        let old_chars = std::mem::take(&mut self.page.chars);
        self.page.order = 0;
        let (lay, top) = self.text_layout();
        self.page.pen = (lay.left, top);
        for t in &texts {
            self.show_message(t, false);
        }
        for (m, old) in self.page.msgs.iter_mut().zip(&msgs) {
            m.2 = old.2;
            m.3 = old.3;
        }
        // each message's slot keeps its glyph records through a new layout (`Wobble::arm`)
        for (pc, old) in self.page.chars.iter_mut().zip(&old_chars) {
            if let (Some(w), Some(o)) = (&mut pc.wobble, old.wobble)
                && o.kind == w.kind
            {
                *w = o;
            }
        }
        self.relaying = false;
        self.msg_pc = msg_pc;
        self.page.revealed = self.page.order as f32;
        self.page.waiting = waiting;
        self.page.visible = visible;
    }

    /// sub_A2550: capture this frame and show it over everything at full alpha until `thaw_screen`.
    pub fn freeze_screen(&mut self) {
        self.freeze = Some(Freeze { capture: true, alpha: 1.0, step: 0.0 });
    }

    /// sub_A27F0(ms): the capture fades out linearly over `ms` (a step of 1000 / ms / 60 per frame).
    pub fn thaw_screen(&mut self, ms: f32) {
        if let Some(f) = &mut self.freeze
            && f.alpha > 0.0
        {
            f.step = 1000.0 / ms / 60.0;
        }
    }

    /// sub_A2860: false while a capture shows.
    pub fn screen_clear(&self) -> bool {
        self.freeze.is_none()
    }

    /// The QA bubble font (ja3 FOT-Seurat), set once at startup.
    pub fn set_bubble_font(&mut self, mut font: FontAtlas) {
        font.bubble = true;
        self.font_bubble = Some(font);
    }

    /// The message text font. sub_391A0 sets text flag 0x800000 while the window type (F1105) is 3 or more (the
    /// QA bubbles); sub_A7790 turns it into style bit 0x40, for which the text parse (sub_BDC2C) wraps the text
    /// in nn::font tags 24 ... 21. sub_C445C files the chars after tag 24 under glyph class 2 and sub_C406C
    /// rasterises those with font variant 2 (classes 0 / 1 use variants 0 / 1). The exe names no font files;
    /// the Switch draws that variant in FOT-Seurat (ja3); variant 3 is the shared system font (`Ui::sys_font`).
    fn msg_font(&mut self) -> &mut FontAtlas {
        let bubble = self.window_type().is_some_and(|t| t >= 3) || self.z_cur.is_some();
        match &mut self.font_bubble {
            Some(f) if bubble => f,
            _ => &mut self.font,
        }
    }

    /// F824, line 0's top in page units: 65 in Tsukihime (sub_391A0). Witch on the Holy Night's jp_391A0 (docked,
    /// jp_A59D0): 65 (English (int)(65 - 9.8824) = 55) for the docked windows; a bubble type 3..98 its text origin
    /// y flt_25E588 x 0.77778 less ((F1108 - 1) x 41 + 26) x 0.41481, plus the F1107 offset x 0.77778 (F1107
    /// 0..20).
    fn witch_f824(&self) -> i32 {
        if self.assets.build != Build::Witch {
            return 65;
        }
        let f1109 = self.var("F1109");
        let t = if text::english() && f1109 > 0 { f1109 } else { self.var("F1105") };
        let docked = if text::english() { (65.0f32 - 9.8824) as i32 } else { 65 };
        let Some(&(_, ty)) = usize::try_from(t).ok().filter(|&t| (3..=98).contains(&t)).and_then(|t| crate::witch::BUBBLE_TEXT.get(t))
        else {
            return docked;
        };
        let lines = self.var("F1108") as f32;
        let mut y = ty * 0.77778 - ((lines - 1.0) * 41.0 + 26.0) * 0.41481;
        if let Some(dy) = usize::try_from(self.var("F1107")).ok().and_then(|i| crate::witch::WINDOW_DY.get(i)) {
            y += dy * 0.77778;
        }
        y as i32
    }

    /// Message window type (stage word F1105; for English F1109 instead when it is above 0, sub_2DF20).
    fn window_type(&self) -> Option<usize> {
        let f1109 = self.var("F1109");
        usize::try_from(if text::english() && f1109 > 0 { f1109 } else { self.var("F1105") }).ok()
    }

    /// Window offset in 1280x720 units: flt_34D84C[F1106], flt_34D8B8[F1107] (0 out of range; Witch on the Holy
    /// Night's 1024x576-unit tables, `witch::WINDOW_DX` / `WINDOW_DY`).
    fn window_offset(&self) -> (f32, f32) {
        let at = |t: &[f32], w: &str| usize::try_from(self.var(w)).ok().and_then(|i| t.get(i)).copied().unwrap_or(0.0);
        if self.assets.build == Build::Witch {
            return (at(&crate::witch::WINDOW_DX, "F1106") * 1.25, at(&crate::witch::WINDOW_DY, "F1107") * 1.25);
        }
        (at(&WINDOW_DX, "F1106"), at(&WINDOW_DY, "F1107"))
    }

    /// Lay out a message (`text` as the script has it: `$` references resolve in the game language, and the
    /// page keeps it unresolved, as the exe's history records do) at the page pen.
    /// A raw ruby the tag processor can't take (`text::Run::RubyAt` without a base) stops the game: sub_DDA1C reads
    /// a base pointer the slot never set (address 0) or an earlier text's, or case 1 / 3 scans on past the text
    /// (Switch, Astris: a 0x03 with no 0x01 before it crashed the game in a fresh slot and froze it in another).
    fn check_rubies(runs: &[text::Run], shown: &str) {
        if runs.iter().any(|r| matches!(r, text::Run::RubyAt { base: None, .. })) {
            tracing::error!("{shown:?}: a raw ruby tag (0x03 with no 0x01 base before it, or an unclosed 0x01 / 0x03) that stops the game");
            std::process::exit(1);
        }
    }

    fn show_message(&mut self, text: &str, wait: bool) {
        self.slot_flip = false;
        let shown = self.resolve_text(text);
        let runs = text::parse_markup(&shown);
        Self::check_rubies(&runs, &shown);
        self.page.visible = true;
        let i = self.page.msgs.len();
        self.z_cur = if self.relaying {
            self.relay_zs.get(i).copied().flatten()
        } else if self.assets.build == Build::Witch && shown.contains("@z") {
            let bit = 1u64 << (4 + i).min(63);
            let fresh = self.z_latch & bit == 0;
            self.z_latch |= bit;
            Some(fresh)
        } else {
            None
        };
        // sub_391A0 lays the page's earlier messages out from their history slots (at 768), where a line above
        // line 0 is line 0 (the line index clamped): a message drawn there while it was the current one moves
        // down (Switch: `@x` text alone on a page at the screen's top, on line 0 once the next one shows)
        if let Some((first, dy)) = self.page.lifted.take() {
            for pc in self.page.chars.iter_mut().skip(first) {
                pc.baseline += dy;
                if let Some(bar) = &mut pc.bar {
                    bar.top += dy;
                }
            }
            self.page.pen.1 += dy;
        }
        // lay out on a scratch copy; restart the page if it overflows
        let (lay, top) = self.text_layout();
        // sub_391A0: in page mode message i of the page starts F(847 + i) lines further down (over 32 counts as
        // that minus 63, i.e. up; Witch on the Holy Night's jp_391A0 minus 64), never above the first line; the
        // scripts' WKAD(C847,1) after a message (and the first SELR of a choice) leave a blank line before the
        // next
        let witch = self.assets.build == Build::Witch;
        let f = if self.page_mode() { self.var(&format!("F{}", 847 + self.page.msgs.len())) } else { 0 };
        let extra = match f {
            v if v > 32 => v - if witch { 64 } else { 63 },
            v if v >= 1 => v,
            _ => 0,
        };
        // `@x` or a 0x0C (sub_391A0 / jp_391A0: the slot's tagged text has tag 12, sub_A88B0 -> sub_C0DB0): the
        // message starts at the left a line up (sub_A80D0), and the tag moves the cursor to the slot's base x
        // (sub_A8190): the end cursor of the message before (+101024) + 2 units (0 before the page's first, so
        // short of the left edge, which makes it 0 and sub_C3710 the slot's x)
        let indent = runs.iter().any(|r| matches!(r, text::Run::Indent));
        // Tsukihime's line up from line 0 reads the line table's entry -1, the quake's frame count dword_AFECC8
        // (an int, read as a float about 0): the line's box top at the screen's top (Switch: a page's first
        // message with a 0x0C at y 19 of 1080, 157 px above line 0's). Witch on the Holy Night stays on line 0.
        // A bubble keeps its own origin.
        let bubble = self.window_type().is_some_and(|t| self.is_bubble(t));
        let up = |y: f32| match y - lay.line_height {
            v if v < top - 0.5 && !witch && !bubble => lay.ascent,
            v => v.max(top),
        };
        let (start, mut base) = if self.page.chars.is_empty() {
            let y = top + extra.max(0) as f32 * lay.line_height;
            if indent && !witch { ((lay.left, up(y)), lay.left) } else { ((lay.left, y), lay.left) }
        } else {
            let y = (self.page.pen.1 + lay.line_height * (1 + extra) as f32 + PARAGRAPH_GAP).max(top);
            if indent {
                // on the message before's last line (Tsukihime: line - 1; Witch on the Holy Night: line - 1 while
                // F(847 + i) is 0, else its own line). From the box's right edge on the base is the slot's x on
                // the line after (Tsukihime: its own line; Witch on the Holy Night: the line after it). Switch:
                // 1_0's 「数分後。」 and the message after it on one line.
                let x = self.page.pen.0 - self.pen_gap(&lay) + 2.0 * UX;
                // sub_AB9A0 reads the cursor the click icon does, which for English text is half a glyph cell
                // further (overseas sub_C5FB4, jp_C5FB4). Switch: an English line's xyz 31 px after its x.
                let x = if (witch || self.assets.build == Build::Overseas) && text::english() { x + lay.size * 0.5 } else { x };
                if x >= lay.right {
                    ((lay.left, if witch { y + lay.line_height } else { y }), lay.left)
                } else {
                    // below the left edge base 0 (the slot's x); a NaN cursor passes as it is (jp_391A0)
                    let x = if x < lay.left { lay.left } else { x };
                    if !witch || f == 0 { ((lay.left, up(y)), x) } else { ((lay.left, y), x) }
                }
            } else {
                ((lay.left, y), lay.left)
            }
        };
        let mut placed = Vec::new();
        let mut from = (start, self.page.order);
        let order0 = self.page.order;
        let mut first = self.page.chars.len();
        let font = self.glyph_start(&runs, false);
        let (x, b, order) = text::layout_from(self.msg_font(), &lay, &runs, start.0, start.1, base, order0, &mut placed, font);
        self.msg_pc = self.pc.saturating_sub(1);
        let mut lifted = start.1 < top - 0.5;
        if b > BOTTOM_LIMIT && !self.page.chars.is_empty() && !self.relaying {
            self.clear_page(true);
            placed.clear();
            let y = if indent && !witch && !bubble { lay.ascent } else { top };
            lifted = y < top - 0.5;
            first = 0;
            base = lay.left;
            from = ((lay.left, y), 0);
            let font = self.glyph_start(&runs, false);
            let (x, b, order) = text::layout_from(self.msg_font(), &lay, &runs, lay.left, y, base, 0, &mut placed, font);
            self.page.pen = (x, b);
            self.page.order = order;
        } else {
            self.page.pen = (x, b);
            self.page.order = order;
        }
        if lifted {
            self.page.lifted = Some((first, top - lay.ascent));
        }
        self.page.last_start = from;
        self.page.last_base = base;
        // the page's earlier messages are drawn whole from their ring entries (sub_391A0 at 768 / jp_391A0 at 800),
        // only the new message's slot types out (a rewound message restored at head 0 included)
        self.page.revealed = self.page.revealed.max(from.1 as f32);
        self.arm_wobbles(&mut placed, &[]);
        let fade_frames = self.read_fade_frames();
        self.page.msgs.push((self.page.chars.len(), shown.trim_end().ends_with("@k"), 0.0, fade_frames));
        self.page.zs.push(self.z_cur);
        self.page.chars.extend(placed);
        let extra = self.var(&format!("F{}", 847 + self.page.texts.len()));
        self.page.extras.push(extra);
        self.page.texts.push(text.to_string());
        self.blank_glyphs(lay.size);
        if wait {
            self.page.waiting = true;
            self.wait = Wait::Click;
            self.auto_timer = 0.0;
        }
    }

    /// MSAD (sub_6E950): append to the current message's text (its trailing `@k` dropped) and lay the whole
    /// message out again in its slot, so the addition continues where the message ended.
    fn append_message(&mut self, add: &str, wait: bool) {
        let (Some(last), Some(&(first, _, _, _))) = (self.page.texts.last(), self.page.msgs.last()) else {
            self.show_message(add, wait);
            return;
        };
        let base = last.strip_suffix("@k").unwrap_or(last);
        let text = format!("{base}{add}");
        let shown = self.resolve_text(&text);
        let runs = text::parse_markup(&shown);
        Self::check_rubies(&runs, &shown);
        let (lay, _) = self.text_layout();
        let ((x0, y0), order0) = self.page.last_start;
        let old = self.page.chars.split_off(first.min(self.page.chars.len()));
        let mut placed = Vec::new();
        let base = self.page.last_base;
        let font = self.glyph_start(&runs, true);
        let (x, b, order) = text::layout_from(self.msg_font(), &lay, &runs, x0, y0, base, order0, &mut placed, font);
        self.arm_wobbles(&mut placed, &old);
        self.page.pen = (x, b);
        self.page.order = order;
        self.page.chars.extend(placed);
        if let Some(m) = self.page.msgs.last_mut() {
            m.1 = shown.trim_end().ends_with("@k");
        }
        *self.page.texts.last_mut().unwrap() = text;
        self.blank_glyphs(lay.size);
        self.page.visible = true;
        if wait {
            self.page.waiting = true;
            self.wait = Wait::Click;
            self.auto_timer = 0.0;
        }
    }

    /// The page's texts as their slots print them.
    fn page_runs(&self) -> Vec<Vec<text::Run>> {
        self.page.texts.iter().map(|t| text::parse_markup(&self.resolve_text(t))).collect()
    }

    /// The font order a text added to the page (`runs`; in place of the page's last when `replace`) starts
    /// drawing in (`text::GlyphCache`).
    fn glyph_start(&mut self, runs: &[text::Run], replace: bool) -> text::FontOrder {
        let mut page = self.page_runs();
        if replace {
            page.pop();
        }
        let mut texts: Vec<&[text::Run]> = page.iter().map(Vec::as_slice).collect();
        texts.push(runs);
        let mut glyphs = std::mem::take(&mut self.glyphs);
        let start = glyphs.starts(self.msg_font(), &texts, true).pop();
        let regs = glyphs.first_print(self.msg_font(), runs);
        self.glyphs = glyphs;
        self.glyph_regs.push((texts.len() - 1, regs));
        start.unwrap_or_default()
    }

    /// Mark the page's glyphs the glyph cache draws as the space glyph (`text::GlyphCache::blank`).
    fn blank_glyphs(&mut self, size: f32) {
        let page = self.page_runs();
        let texts: Vec<&[text::Run]> = page.iter().map(Vec::as_slice).collect();
        let firsts: Vec<usize> = self.page.msgs.iter().map(|m| m.0).collect();
        let (mut glyphs, mut chars) = (std::mem::take(&mut self.glyphs), std::mem::take(&mut self.page.chars));
        glyphs.blank(self.msg_font(), &texts, &mut chars, &firsts, size, true);
        (self.glyphs, self.page.chars) = (glyphs, chars);
    }

    /// jp_BDC2C's glyph records for a message's new layout: each wobbling glyph draws its phases and rates
    /// unless the glyph at its index in the message's last layout (`old`) had a record of the same type.
    fn arm_wobbles(&mut self, placed: &mut [text::PlacedChar], old: &[text::PlacedChar]) {
        let rng = &mut self.app_rng;
        for (k, pc) in placed.iter_mut().enumerate() {
            if let Some(w) = &mut pc.wobble {
                w.arm(old.get(k).and_then(|o| o.wobble), |a, b| rng.uniform(a, b));
            }
        }
    }

    // ---- rendering ------------------------------------------------------------------

    /// Pages the backlog can show: the finished pages plus the current one.
    /// Finished pages in the backlog. sub_27EB0 lists the history newest first and drops every entry up to the
    /// last one of the previous page (F799, the page-break flag each entry keeps), so the page on screen is
    /// never in it and the backlog only opens once a page has been read.
    pub fn backlog_len(&self) -> usize {
        self.backlog_pages.len()
    }

    /// Text quads of backlog page `back` (0 = the newest finished page), laid out like the message window
    /// (sub_28470 lays the history out with the message text style at x 88 units, English 32, the first line's
    /// box at 65.059 units, English 55.176, in a 464-unit slot, English 573).
    pub fn backlog_quads(&mut self, r: &Renderer, back: usize) -> Vec<Quad> {
        let (texts, extras, bubble) = match self.backlog_pages.len().checked_sub(back + 1) {
            Some(i) => {
                let p = &self.backlog_pages[i];
                (p.texts.clone(), p.extras.clone(), p.bubble)
            }
            None => return Vec::new(),
        };
        let mut lay = msg_layout();
        // bubble entries in the bubble font take its pair ascent (`text::bubble_ascent`)
        if bubble && self.font_bubble.is_some() {
            lay.ascent *= text::bubble_ascent();
        }
        let top = if text::english() { 55.176 } else { 65.059 } * UY + lay.ascent;
        let font = match &mut self.font_bubble {
            Some(f) if bubble => f,
            _ => &mut self.font,
        };
        let page: Vec<_> = texts.iter().map(|t| text::parse_markup(&resolve_refs(&self.assets, t))).collect();
        let entries: Vec<&[text::Run]> = page.iter().map(Vec::as_slice).collect();
        // the entries' slots print in turn every frame (`text::GlyphCache`)
        let mut glyphs = self.glyphs.clone();
        let starts = glyphs.starts(font, &entries, false);
        let mut placed = Vec::new();
        let (mut pen_y, mut order) = (None, 0);
        // the end cursor of the entry before (`text::trailing_gap`)
        let mut end_x = None;
        let english_cursor = matches!(self.assets.build, Build::Overseas | Build::Witch) && text::english();
        // each entry's first glyph and edge colour (its own slot's)
        let mut edges = Vec::new();
        for (i, t) in texts.iter().enumerate() {
            edges.push((placed.len(), text::text_edge(t)));
            let runs = &page[i];
            // sub_28470: entry i starts a line pitch below the last one (the first at the top line), then
            // F(847 + i) of its record more lines down when 1..32, or (that - 64) lines when over 32, not above
            // the top line (a choice's first option: the blank line its first SELR asked for)
            let y = pen_y.map_or(top, |y: f32| y + lay.line_height + PARAGRAPH_GAP);
            let extra = extras.get(i).copied().unwrap_or(0);
            let y = match extra {
                v @ 1..=32 => y + v as f32 * lay.line_height,
                v if v > 32 => (y + (v - 64) as f32 * lay.line_height).max(top),
                _ => y,
            };
            // `@x` or a 0x0C (sub_A88B0): the entry starts at the left and the tag moves the cursor to the base x,
            // the end cursor of the entry before + 2 units (that cursor 0 before the first: no move; for English
            // text half a glyph cell further, as the click icon's), a line up while F(847 + i) is below 32; from
            // the box's right edge on the left of the line after
            let (y, base) = match end_x {
                Some(e) if runs.iter().any(|r| matches!(r, text::Run::Indent)) => {
                    let x: f32 = e + 2.0 * UX + if english_cursor { lay.size * 0.5 } else { 0.0 };
                    if x >= lay.right {
                        (y + lay.line_height, lay.left)
                    } else if extra < 32 {
                        (y - lay.line_height, x)
                    } else {
                        (y, x)
                    }
                }
                _ => (y, lay.left),
            };
            let (px, b, o) = text::layout_from(font, &lay, runs, lay.left, y, base, order, &mut placed, starts[i]);
            end_x = Some(px - text::trailing_gap(runs, lay.spacing));
            pen_y = Some(b);
            order = o;
        }
        let firsts: Vec<usize> = edges.iter().map(|e| e.0).collect();
        glyphs.blank(font, &entries, &mut placed, &firsts, lay.size, false);
        let regs = glyphs.registrations(font, &entries, false);
        font.register(r, &regs, true);
        let mut out = Vec::new();
        // each entry is its own draw buffer (`text::Batches`)
        let mut batches = text::Batches::default();
        for (i, pc) in placed.iter().enumerate() {
            if edges.iter().any(|e| e.0 == i) {
                batches.flush(&mut out);
            }
            let edge = edges.iter().rev().find(|e| e.0 <= i).map_or([0; 3], |e| e.1);
            let mut q = Vec::new();
            Self::rimmed_glyph_edge(font, r, &mut q, pc, edge, 1.0);
            let ex = font.is_ex(pc);
            q.into_iter().for_each(|q| batches.push(ex, q));
        }
        batches.flush(&mut out);
        out.extend(placed.iter().filter_map(|pc| self.bar_quad(pc, None, [1.0; 4], true)));
        out
    }

    /// The bar of a bar run's first character (`text::Bar`, drawn by sub_C6A58 after the slot's glyphs) when
    /// `count` of its characters show (all when None), tinted `color` (the slot's text colour times its alpha):
    /// the bar texture pair sub_A7790 gives every slot (sub_C3B48: ids 143 / 144, the overseas build 146 /
    /// 147), DISP_PARTS bf_no_edge68x68_2 / bf_with_edge68x68_2 (image slots 79 / 80, overseas 82 / 83), the
    /// second for an edged slot (+101836 + 4 * +101003).
    pub fn bar_quad(&self, pc: &text::PlacedChar, count: Option<usize>, color: [f32; 4], edge: bool) -> Option<Quad> {
        let bar = pc.bar?;
        let n = count.map_or(bar.len, |c| c.min(usize::from(bar.len)) as u8);
        if n == 0 {
            return None;
        }
        let slot: u16 = if self.assets.build == Build::Overseas { 82 } else { 79 } + u16::from(edge);
        let tex = self.disp.get(usize::from(slot - self.disp_slot0()))?;
        let (rect, [u0, v0, u1, v1]) = bar.rects(pc.x, pc.size, n);
        Some(Quad::rect(tex.clone(), rect, color).with_uv_rect(u0, v0, u1, v1))
    }

    /// A message glyph: white (or its fill code's colour) with a dark rim all around.
    fn rimmed_glyph(font: &mut FontAtlas, r: &Renderer, out: &mut Vec<Quad>, pc: &text::PlacedChar, alpha: f32) {
        Self::rimmed_glyph_edge(font, r, out, pc, [0; 3], alpha);
    }

    /// `rimmed_glyph` with the rim in `edge` (the text's edge code's colour, `text::text_edge`).
    fn rimmed_glyph_edge(font: &mut FontAtlas, r: &Renderer, out: &mut Vec<Quad>, pc: &text::PlacedChar, edge: [u8; 3], alpha: f32) {
        // the edged cache of font index 1 scaled with the glyph, edge and fill in one texel: sub_391A0 fades a
        // message by one alpha on the whole bordered glyph (Switch fire page: faded line = B + 0.39 (full - B)
        // per pixel), not by fading separately drawn edge and fill quads
        let rgb = text::fill_rgb(pc, [1.0; 3]);
        if let Some(q) = font.rimmed_quad_edge(r, pc, 1, rgb, edge, alpha) {
            out.push(q);
        }
    }

    /// The MSQK offset of the message layer in canvas pixels.
    fn msg_offset(&self) -> (f32, f32) {
        (self.msg_quake.offset.0 * CANVAS_W as f32 / 640.0, self.msg_quake.offset.1 * CANVAS_H as f32 / 448.0)
    }

    /// The message window (layer 29) at window alpha `a`: the page-mode window image and a second bubble.
    fn window_quads(&self, a: f32, out: &mut Vec<Quad>) {
        if a <= 0.0 {
            return;
        }
        // the window (F1104 == 0, page mode): image slot WINDOWS[F1105] at its origin + the F1106 / F1107
        // offsets + the MSQK offset, natively sized (0.333 x 0.4148 units per 1920x1080 pixel). Types 0..2
        // cover the screen at the page alpha * the window opacity (Witch on the Holy Night's config word 0x10, 255
        // at first; 1 in Tsukihime); type 0 = txtwindow01 (alpha 102/255 in
        // the middle, falling off to the left and right edges). The bubbles draw at 0.78431 * the page alpha
        // (Witch on the Holy Night's jp_391A0: 1, Switch: the TeaTime bubbles opaque).
        // F822 != 1 draws image slots 10 / 2 instead (sub_B6DE0), which nothing ever fills, so no window
        // shows there (trainscene.mov)
        let (ox, oy) = self.msg_offset();
        // sub_391A0 draws the window into layer 29 and the text / icon into layer 30, then composites both
        // (sub_B06C0 -> sub_9FAD0) at window alpha * 2 * max(content - 0.5, 0): CLO4 fades the frame too
        let content = 1.0 - self.page.dim;
        let layer = a * (2.0 * (content - 0.5)).clamp(0.0, 1.0);
        if self.page_mode() && self.var("F1104") == 0 {
            let w = self.window_type().and_then(|t| Some((t, self.windows().get(t)?, self.window_image(t)?)));
            if let Some((t, &(_, wx, wy), (slot, sx, sy))) = w
                && let Some(tex) = self.disp.get(usize::from(slot - self.disp_slot0()))
            {
                let (dx, dy) = self.window_offset();
                // jp_391A0: types 0..2 at the config's window opacity
                let bubble_alpha = if self.assets.build == Build::Witch { 1.0 } else { 0.78431 };
                let alpha = if t > 2 { bubble_alpha } else { self.settings.window_alpha } * a * layer;
                let size = tex.texture.size();
                let rect = [(wx + dx) * 1.5 + ox, (wy + dy) * 1.5 + oy, size.width as f32 * sx, size.height as f32 * sy];
                out.push(Quad::rect(tex.clone(), rect, [1.0, 1.0, 1.0, alpha]));
            }
            // F1111 >= 3: a second bubble at its origin + the F1112 / F1113 offsets, also at 0.78431 * alpha
            if let Some(t) = self.second_bubble()
                && let Some((slot, sx, sy)) = self.window_image(t)
                && let Some(tex) = self.disp.get(usize::from(slot - self.disp_slot0()))
            {
                let at = |t: &[f32], w: &str| usize::try_from(self.var(w)).ok().and_then(|i| t.get(i)).copied().unwrap_or(0.0);
                let (_, wx, wy) = self.windows()[t];
                let (dx, dy) = (at(&WINDOW_DX, "F1112"), at(&WINDOW_DY, "F1113"));
                let size = tex.texture.size();
                let rect = [(wx + dx) * 1.5 + ox, (wy + dy) * 1.5 + oy, size.width as f32 * sx, size.height as f32 * sy];
                out.push(Quad::rect(tex.clone(), rect, [1.0, 1.0, 1.0, 0.78431 * a * layer]));
            }
        }
    }

    fn ui_quads(&mut self, r: &Renderer) -> Vec<Quad> {
        let mut out = Vec::new();
        // the overseas sub_391A0 returns at once while the system menu shows (sub_4EC00)
        if self.assets.build == Build::Overseas && self.system_menu_shown {
            return out;
        }
        let a = if self.hide_text { 0.0 } else { self.page.alpha };
        if a > 0.0 {
            let content = 1.0 - self.page.dim;
            let layer = a * (2.0 * (content - 0.5)).clamp(0.0, 1.0);
            // sub_391A0 composites target 29 (the window) only while F1103 is 0, so CLO2 / CLO3 / FADS take the
            // window away at once and the 64 ms quick hide is not seen (Switch: CLO3 + FADS(1,0,0) in
            // 001_00_00, the page band gone in the frame the text goes)
            if self.var("F1103") == 0 {
                self.window_quads(a, &mut out);
            }
            let (ox, oy) = self.msg_offset();
            // sub_391A0 composites the window (target 29), then draws the choice (sub_84E50), then composites
            // the text (target 30): the page text stays above the choice backdrop
            self.choice_quads(r, &mut out, a);
            let revealed = self.page.revealed;
            // sub_391A0: no text while F1103; else alpha * 2 * max(content - 0.5, 0), times the layer 30 composite
            let text_a = if self.var("F1103") != 0 { 0.0 } else { layer * layer };
            // fade of each message, resolved through runs of @k messages to the message that ends them; with a
            // second bubble earlier messages fade to 255 instead of 100 (sub_391A0 v206), so they stay
            let read_alpha = if self.second_bubble().is_some() { 1.0 } else { READ_ALPHA };
            let shift = self.second_bubble_shift();
            let bubble_font = self.window_type().is_some_and(|t| t >= 3);
            // the current message in its own slot's colour (jp_6E0D0), the earlier ones in jp_391A0's; the
            // edge follows the config unless the bubble flags (0x80000) take it off
            let n_msgs = self.page.msgs.len();
            let colours: Vec<[f32; 3]> =
                (0..n_msgs).map(|i| text::rgb565(if i + 1 == n_msgs { self.msg_colour() } else { self.history_colour(i) })).collect();
            let edge = self.settings.text_edge && !self.witch_bubble_text();
            // each message's edge colour from an edge code in its text (its slot's +16; Switch, Witch on the Holy
            // Night's @c5 rhymes: dark blue rims on their own pages, black again on the next message), and whether
            // the code turns the edge on (Tsukihime's sub_BDC2C: +283 and the edge font index)
            let slot_edges: Vec<([u8; 3], bool)> = (0..n_msgs)
                .map(|i| {
                    let t = self.page.texts.get(i).map_or("", String::as_str);
                    let forced = text::edge_code(t).is_some() && !crate::assets::witch() && !self.witch_bubble_text();
                    (text::text_edge(t), forced)
                })
                .collect();
            // (an `@z` message in the shared font too, `Game::z_cur`)
            let regs = std::mem::take(&mut self.glyph_regs);
            let (font_main, font_bubble) = (&mut self.font, &mut self.font_bubble);
            let zs = &self.page.zs;
            // the messages laid out since the last draw register their glyphs first, in the cache they draw from
            for (i, regs) in &regs {
                let font = match font_bubble.as_mut() {
                    Some(f) if bubble_font || zs.get(*i).copied().flatten().is_some() => f,
                    _ => &mut *font_main,
                };
                font.register(r, regs, edge || slot_edges.get(*i).is_some_and(|e| e.1));
            }
            let msgs = &self.page.msgs;
            let cur = msgs.last().map_or(usize::MAX, |m| m.0);
            let fades: Vec<f32> = (0..msgs.len())
                .map(|i| {
                    let j = (i..msgs.len()).find(|&j| !msgs[j].1).unwrap_or(msgs.len() - 1);
                    1.0 - (1.0 - read_alpha) * msgs[j].2
                })
                .collect();
            let mut m = 0;
            let mut bars = Vec::new();
            // each message is its own draw buffer (`text::Batches`)
            let mut batches = text::Batches::default();
            for (ci, pc) in self.page.chars.iter().enumerate() {
                while m + 1 < msgs.len() && msgs[m + 1].0 <= ci {
                    m += 1;
                    batches.flush(&mut out);
                }
                let fade = fades.get(m).copied().unwrap_or(1.0);
                let (sx, sy) = shift.filter(|_| ci >= cur).unwrap_or((0.0, 0.0));
                // jp_C3C24: the glyph's wobble in 1920x1080 pixels
                let (wx, wy) = pc.wobble.map_or((0.0, 0.0), |w| w.offset());
                let (wx, wy) = (wx * CANVAS_W as f32 / 1920.0, wy * CANVAS_H as f32 / 1080.0);
                let pc = &text::PlacedChar { x: pc.x + ox + sx + wx, baseline: pc.baseline + oy + sy + wy, ..*pc };
                // sub_C6A58: the bar shows (int) revealed - its first character's index cells
                let rgb = colours.get(m).copied().unwrap_or([1.0; 3]);
                if pc.bar.is_some() && !self.rewinding {
                    let count = (revealed.floor() - pc.order as f32).max(0.0) as usize;
                    bars.push((*pc, count, text_a * fade, rgb));
                }
                let mut vis = (revealed - pc.order as f32).clamp(0.0, 1.0);
                if !self.settings.text_fade_in && vis > 0.0 {
                    vis = 1.0;
                }
                // (sub_391A0 draws no text while a rewind runs, off_AE05B8)
                if vis <= 0.0 || self.rewinding {
                    continue;
                }
                // the slot's alpha (sub_AB710: fade x the content factor) as the print's byte, the composite's
                // factor after it
                let alpha = if text_a > 0.0 { vis * text::slot_alpha(fade * layer) * layer } else { 0.0 };
                // a fill code's colour (jp_C3C24's node colour) in place of the slot's
                let rgb = text::fill_rgb(pc, rgb);
                let font = match font_bubble.as_mut() {
                    Some(f) if bubble_font || zs.get(m).copied().flatten().is_some() => f,
                    _ => &mut *font_main,
                };
                let q = if edge || slot_edges[m].1 {
                    font.rimmed_quad_edge(r, pc, 1, rgb, slot_edges[m].0, alpha)
                } else {
                    font.placed_quad(r, pc, 0.0, 0.0, [rgb[0], rgb[1], rgb[2], alpha])
                };
                if let Some(q) = q {
                    batches.push(font.is_ex(pc), q);
                }
            }
            batches.flush(&mut out);
            // the message text is edged (+101003) as the config says; the bar takes the slot alpha, not the
            // glyph fade-in
            for (pc, count, alpha, rgb) in bars {
                out.extend(self.bar_quad(&pc, Some(count), [rgb[0], rgb[1], rgb[2], alpha], edge));
            }
            // (sub_38EC0 draws nothing while a rewind runs)
            // (the overseas sub_38EC0 also draws nothing in auto mode, word_1916DEC bit 0)
            let auto_hidden = self.assets.build != Build::Japanese && self.auto;
            if self.assets.build == Build::Witch {
                self.witch_icon_quads(&mut out, shift, a * content * layer);
            } else if self.icon_fade > 0.0 && !self.rewinding && !auto_hidden {
                // sub_38EC0 at the pen + (2, 3.5) units for the F799 line icon (slot 54, lineBreak_a), (2, 5.5)
                // for the page icon (slot 55, pageBreak_a, at half alpha); F822 != 1 always the page icon at
                // (2, 6.5). Window types 3..32 draw slot 56 (pageBreak_ciel_a, 59 frames of 43x46) at (4, 4).
                // 30 frames a second, 0.36 x 0.45 units per texture pixel.
                let bubble = self.page_mode() && self.window_type().is_some_and(|t| self.is_bubble(t));
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
                // The pen is the slot's nn::font cursor (slot +101024, sub_AB8E0): its line box top, and no
                // character spacing after the last glyph (TextWriter adds it before each glyph but the first;
                // `pen_gap`), truncated to whole 640x448 units.
                let (lay, _) = self.text_layout();
                let (px, py) = self.page.pen;
                let px = if px > lay.left { px - self.pen_gap(&lay) } else { px };
                // The overseas text slot (sub_C5FB4, English text: slot +101009) stores the writer's cursor
                // plus half the glyph cell (slot +776, the 41.04 px em) as the cursor
                let px = if self.assets.build == Build::Overseas && text::english() { px + lay.size * 0.5 } else { px };
                // (int) as fcvtzs: a NaN cursor (an English raw 0x12) 0, an infinite one saturated
                let (ux, uy) = ((px / UX) as i32 as f32, ((py - lay.ascent) / UY) as i32 as f32);
                let (sx, sy) = shift.unwrap_or((0.0, 0.0));
                let (x, y) = ((ux + dx) * UX + sx, (uy + dy) * UY + sy);
                // one animation frame per 2 ticks of a counter that runs only while an icon draws (sub_B0190's
                // frame ticks, at most 2) and wraps at twice the kind's frames, so each icon starts where the
                // last one stopped
                self.icon_ticks = (self.icon_ticks + (self.frame_ms * 0.06).min(2.0)) % (2 * frames) as f32;
                if let Some(t) = self.disp.get(slot - usize::from(self.disp_slot0())) {
                    let frame = (self.icon_ticks / 2.0) as u32 % frames;
                    // the frame rects (dword_572B48, both builds) step one cell a frame, except the page icon's
                    // last, which repeats frame 103: pageBreak_a's 105th cell is empty
                    let frame = if slot == 55 && frame == 104 { 103.0 } else { frame as f32 };
                    let tw = t.texture.size().width as f32;
                    let th = t.texture.size().height as f32;
                    let (w, h) = (fw * ICON_SCALE.0, fh * ICON_SCALE.1);
                    out.push(
                        Quad::rect(t.clone(), [x, y, w, h], [1.0, 1.0, 1.0, alpha * self.icon_fade * a * content * layer])
                            .with_uv_rect(frame * fw / tw, 0.0, (frame + 1.0) * fw / tw, fh / th),
                    );
                }
            }
        }
        if a <= 0.0 {
            self.choice_quads(r, &mut out, a);
        }
        // the end of jp_391A0: SEWI's select over everything (sub_656A0)
        self.wik_quads(r, &mut out);
        self.teatime_quads(r, &mut out);
        // sub_1E770 -> sub_1DAE0 (Witch on the Holy Night): XSKR's hold bar in 960x540 units, a grey (0.5) frame
        // at 820..920 x 510..530 and black over 822 + 96 x held / 180 .. 918 x 512..528
        let held = self.xskr_hold().floor();
        if held >= 1.0 {
            let k = CANVAS_W as f32 / 960.0;
            let rect = |x0: f32, y0: f32, x1: f32, y1: f32, c: f32| {
                Quad::rect(r.white.clone(), [x0 * k, y0 * k, (x1 - x0) * k, (y1 - y0) * k], [c, c, c, 1.0])
            };
            out.push(rect(820.0, 510.0, 920.0, 530.0, 0.5));
            out.push(rect(822.0 + held * 96.0 / 180.0, 512.0, 918.0, 528.0, 0.0));
        }
        out
    }

    /// The text head's step per frame (sub_81B30), None for instant text: 16.667 / the text ms of the message's
    /// read state (sub_3B2D0). Witch on the Holy Night (jp_81B30) has one speed for read and unread text, the
    /// config word 0x20 (0..255, 127 at first), and steps 0.768 x (speed x 0.005 + 0.0768) characters, above 200
    /// 0.768 x ((speed - 200) / 16 + 1.0768); 255 is instant.
    fn chars_per_frame(&self, read: usize) -> Option<f32> {
        if self.assets.build == Build::Witch {
            let v = self.settings.text_ms[0];
            return match v {
                255.0.. => None,
                201.0.. => Some(0.768 * ((v - 200.0) * 0.0625 + 1.0 + 0.0768)),
                _ => Some(0.768 * (v * 0.005 + 0.0768)),
            };
        }
        let ms = self.settings.text_ms[read];
        (ms > 0.0).then(|| FRAME_MS / ms)
    }

    /// Frames an earlier message on the page takes to fade to its read alpha: the text speed of the current
    /// message's read state * 1.2 (stage word 870 + i); Witch on the Holy Night's sub_391A0 counts 30 frames
    /// (stage word 859 + i up to 30, alpha 1 - 155 / 255 * n / 30).
    fn read_fade_frames(&self) -> f32 {
        if self.assets.build == Build::Witch {
            30.0
        } else {
            self.settings.text_ms[usize::from(self.line_read)] * 1.2
        }
    }

    /// Witch on the Holy Night's click icon (WoH jp_38EC0, called by jp_391A0 only for window types 0..2): the
    /// line icon (kind F799 0, slot 52 lineBreak_a, 45x45 cells) or the page icon (kind 1, slot 53 pageBreak_a,
    /// 60x45 cells; always outside page mode), 40 frames of 3 ticks each (dword_3AF4F8 % 120) whose rects
    /// (dword_3AF4E8 + 262 / + 182 ints) run two rows of 20 cells, at 0.333 x 0.333 units per texture pixel from
    /// the pen + (2, 6.5) units; nothing in auto mode (byte AE0660 bit 0) or during a rewind.
    fn witch_icon_quads(&mut self, out: &mut Vec<Quad>, shift: Option<(f32, f32)>, alpha: f32) {
        if self.icon_fade <= 0.0 || self.rewinding || self.auto {
            return;
        }
        if self.page_mode() && self.window_type().is_some_and(|t| t > 2) {
            return;
        }
        let page_icon = !self.page_mode() || self.wnty == 1;
        let (slot, (fw, fh), row_gap) = if page_icon { (53, (60.0, 45.0), 50.0) } else { (52, (45.0, 45.0), 56.0) };
        let (lay, _) = self.text_layout();
        let (px, py) = self.page.pen;
        let px = if px > lay.left { px - self.pen_gap(&lay) } else { px };
        // the cursor jp_C5FB4 stores for English text is half a glyph cell (slot +776) further, as in the
        // overseas Tsukihime
        let px = if text::english() { px + lay.size * 0.5 } else { px };
        // (int) as fcvtzs (jp_AB8E0), as Tsukihime's
        let (ux, uy) = ((px / UX) as i32 as f32, ((py - lay.ascent) / UY) as i32 as f32);
        let (sx, sy) = shift.unwrap_or((0.0, 0.0));
        let (x, y) = ((ux + 2.0) * UX + sx, (uy + 6.5) * UY + sy);
        self.icon_ticks = (self.icon_ticks + (self.frame_ms * 0.06).min(2.0)) % 120.0;
        let Some(t) = self.disp.get(usize::from(slot - self.disp_slot0())) else { return };
        let frame = (self.icon_ticks as u32 / 3) % 40;
        let (col, row) = ((frame % 20) as f32, (frame / 20) as f32);
        // the line icon's second row is 44 px high (56..100)
        let ch = if !page_icon && row > 0.0 { 44.0 } else { fh };
        let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
        let (u0, v0) = (col * fw / tw, row * row_gap / th);
        let (w, h) = (fw * 0.333 * UX, ch * 0.333 * UY);
        out.push(
            Quad::rect(t.clone(), [x, y, w, h], [1.0, 1.0, 1.0, self.icon_fade * alpha])
                .with_uv_rect(u0, v0, u0 + fw / tw, v0 + ch / th),
        );
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

    /// The layers into the stage target, through the layer group surfaces where stage::Draw says so.
    fn render_stage(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) {
        use crate::stage::Draw;
        let draws = self.stage.draws(&r.white, &self.border);
        let mut run: Vec<Quad> = Vec::new();
        let b = if self.stage.backdrop_drawn == 1 { 1.0 } else { 0.0 };
        let mut clear = Some([b, b, b, 1.0]);
        for d in draws {
            let quad = match d {
                Draw::Stage(q) => {
                    run.push(q);
                    continue;
                }
                Draw::Composite { blend, alpha } => {
                    // drawn now: the scratch target is reused by the next composite
                    r.draw(enc, &self.stage_rt, clear.take(), &std::mem::take(&mut run));
                    let [px, mask, tmp] = self.group_targets(r, enc);
                    // exalpha into target 3: the pixel surface's rgb, its alpha times the mask's
                    r.copy_target(enc, px, tmp);
                    let full = [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32];
                    let m = Quad { blend: Blend::MaskAlpha, ..Quad::rect(mask.tex.clone(), full, [1.0; 4]) };
                    r.draw(enc, tmp, None, &[m]);
                    run.push(Quad { blend, ..Quad::rect(tmp.tex.clone(), full, [1.0, 1.0, 1.0, alpha]) });
                    continue;
                }
                Draw::Pixel { quad, clear } => (0, quad, clear),
                Draw::Mask { quad, clear } => (1, quad, clear),
            };
            r.draw(enc, &self.stage_rt, clear.take(), &std::mem::take(&mut run));
            let (k, q, c) = quad;
            let t = &self.group_targets(r, enc)[k];
            r.draw(enc, t, c.then_some([0.0; 4]), &[q]);
        }
        r.draw(enc, &self.stage_rt, clear, &run);
    }

    fn group_targets(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) -> &[RenderTarget; 3] {
        if self.group_rt.is_none() {
            let t = ["group pixel", "group mask", "group composite"].map(|n| r.create_canvas_target(CANVAS_W, CANVAS_H, n));
            for rt in &t {
                r.draw(enc, rt, Some([0.0; 4]), &[]);
            }
            self.group_rt = Some(t);
        }
        self.group_rt.as_ref().unwrap()
    }

    /// The canvas targets again at the render scale, if it has changed (the window resized), their pictures
    /// kept.
    fn refit_targets(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) {
        for (rt, l) in [(&mut self.stage_rt, "stage"), (&mut self.scene_rt, "scene"), (&mut self.old_rt, "old frame")] {
            r.refit(enc, rt, l);
        }
        r.refit(enc, &mut self.blur_rt, "blur");
        r.refit(enc, &mut self.canvas, "canvas");
        let slots = [
            (self.menu_rt.as_mut(), "menu blur"),
            (self.freeze_rt.as_mut(), "screen capture"),
            (self.noise_rt.as_mut(), "noise rt40"),
            (self.choice_text_rt.as_mut(), "choice text"),
            (self.choice_glow.as_mut(), "choice glow"),
        ];
        for (rt, l) in slots {
            if let Some(rt) = rt {
                r.refit(enc, rt, l);
            }
        }
        let sets = [
            self.menu_half.as_mut().map(|t| t.as_mut_slice()),
            self.menu_quarter.as_mut().map(|t| t.as_mut_slice()),
            self.ghost_rt.as_mut().map(|t| t.as_mut_slice()),
            self.group_rt.as_mut().map(|t| t.as_mut_slice()),
        ];
        for rt in sets.into_iter().flatten().flatten() {
            r.refit(enc, rt, "scratch");
        }
        if let Some(rt) = &self.noise_rt {
            self.stage.noise_rt = Some(rt.tex.clone());
        }
        // the previews are drawn again at the new scale
        let (tw, th) = PREVIEW_TARGET;
        self.preview_rt.take_if(|t| !r.canvas_fits(&t.tex, tw, th));
        let (pw, ph) = PREVIEW_SIZE;
        self.previews.retain(|_, t| r.canvas_fits(t, pw, ph));
    }

    pub fn render(&mut self, r: &mut Renderer, overlay: &mut dyn FnMut(&mut Self, &Renderer, &mut Vec<Quad>)) {
        let mut enc = r.encoder();
        self.refit_targets(r, &mut enc);
        self.render_previews(r, &mut enc);
        if self.snapshot {
            r.copy_target(&mut enc, &self.scene_rt, &self.old_rt);
            self.snapshot = false;
        }
        if let Some(q) = self.stage.noise_pass() {
            let fresh = self.noise_rt.is_none();
            let rt = self.noise_rt.get_or_insert_with(|| r.create_canvas_target(CANVAS_W, CANVAS_H, "noise rt40"));
            if fresh {
                // nothing earlier to show through the uncovered strip yet: start from a full-screen copy
                let full = Quad::rect(q.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]);
                r.draw(&mut enc, rt, Some([0.0, 0.0, 0.0, 1.0]), &[full]);
            }
            r.draw(&mut enc, rt, None, &[q]);
            self.stage.noise_rt = Some(rt.tex.clone());
        }
        self.render_choice_glow(r, &mut enc);
        self.render_stage(r, &mut enc);
        // Target 0 draws in bucket order (sprite +16, sub_D2378), each bucket in queue order: bucket 1 takes the
        // stage (sub_3BBA0, task 20), the FADE(40) effect (sub_2FC50, task 60) and the text pass's FCLN effect
        // (task 80: sub_391A0 queues it before its first sub_B04E0(9)); a FADS's capture sprite (sub_A46F0, task
        // 50) is bucket 9, so it lies over all three, and what it shows is target 0 as it was, effects included
        // (035 ARC02_1's crossfade under FADE(40, -250): Switch region means 0.31 from this order, 1.38 with the
        // effect over the mixed frame, whose overlay of the frame with itself darkened the middle of the fade)
        r.copy_target(&mut enc, &self.stage_rt, &self.scene_rt);
        // sub_3C490 draws the FADE(40) effect over the composed frame, before the text
        if self.ghost.active() {
            let [src, out] = self.ghost_rt.get_or_insert_with(|| {
                [r.create_canvas_target(CANVAS_W, CANVAS_H, "ghost frame"), r.create_canvas_target(CANVAS_W, CANVAS_H, "ghost channels")]
            });
            let trans = self.trans.as_ref().map(|t| (t.t / t.dur).clamp(0.0, 1.0));
            self.ghost.render(r, &mut enc, &self.scene_rt, [src, &self.blur_rt, out], trans);
        }
        // sub_391A0 starts the text pass with the FCLN effect (sub_87A90(2)) unless F963 = 1
        if self.focus.active() && self.var("F963") != 1 {
            if self.focus_tex.is_none() {
                self.focus_tex = self.assets.ui_textures(r, "EFFECT_PARTS").ok().and_then(|t| {
                    let find = |n: &str| t.iter().find(|(name, _)| name == n).map(|(_, t)| t.clone());
                    Some([find("FallNote0")?, find("FallNote1")?, find("FallNote2")?, find("FocusLine")?])
                });
            }
            if let Some(tex) = &self.focus_tex {
                let quads = self.focus.quads(tex);
                r.draw(&mut enc, &self.scene_rt, None, &quads);
            }
        }
        match &self.trans {
            Some(t) => {
                let p = if t.hold { 0.0 } else { (t.t / t.dur).clamp(0.0, 1.0) };
                let rule = t.rule.clone();
                r.transition(&mut enc, &self.canvas, &self.old_rt.tex, &self.scene_rt.tex, rule.as_deref(), p, self.stage.rule_soft);
            }
            None => r.copy_target(&mut enc, &self.scene_rt, &self.canvas),
        }
        let witch = crate::assets::witch();
        let mut ui = Vec::new();
        if witch {
            // WoH's sub_7FCA0 captures target 0 after the text pass: the window and text are blurred with the scene
            // (Switch: the page's lines show through the system menu's backdrop, blurred)
            if !self.movie_by_ui {
                ui.extend(self.movie_quad());
            }
            ui.extend(self.story_movie.as_ref().and_then(|t| t.cover_quad(r)));
            ui.extend(self.ui_quads(r));
            if !ui.is_empty() {
                r.draw(&mut enc, &self.canvas, None, &ui);
                ui.clear();
            }
        }
        if self.menu_blur.is_some() && !self.menu_captured && !witch {
            // sub_A46F0 copies target 0 in the frame the blur starts, before the composites that frame queued:
            // sub_391A0 draws the window and text into layers 29 / 30 and composites them later (sub_B06C0 ->
            // sub_9FAD0's list), but sub_84E50 draws a choice's backdrop straight into target 0. So the capture is
            // the scene with the choice backdrop and no window or text (Switch EN: the system menu over a page
            // mean abs 1.41 without the window against 28.6 with it; over the first choice 1.44 with the
            // backdrop alone, 6.43 with the window instead; the backlog's 0.75 black leaves 0.417 =
            // 0.25 / (1 - 102/255) of the windowed page)
            let a = if self.touch_hidden { 0.0 } else { self.page.alpha };
            if let Some(q) = self.choice_backdrop(r, a) {
                r.draw(&mut enc, &self.canvas, None, &[q]);
            }
        }
        match self.menu_blur {
            Some(a) => {
                let rt = self.menu_rt.get_or_insert_with(|| r.create_canvas_target(CANVAS_W, CANVAS_H, "menu blur"));
                if !self.menu_captured {
                    if witch {
                        // sub_7FDB0: the capture drawn at 0.25 into the Defocus buffer (jp_A5330(-2)), sub_78810(1.25)'s
                        // meshes: 2 ceil(3.75) + 1 = 9 copies of the 480 x 270 image shifted by whole pixels, each at
                        // its gaussian weight, added horizontally then vertically (sub_7C8D0), and the quarter drawn
                        // back over the screen (jp_A5690, uv 0..0.25)
                        let (qw, qh) = (CANVAS_W / 4, CANVAS_H / 4);
                        let [a, b] = self.menu_quarter.get_or_insert_with(|| {
                            [r.create_canvas_target(qw, qh, "backbuffer defocus"), r.create_canvas_target(qw, qh, "defocus pass")]
                        });
                        let full = Quad::rect(self.canvas.tex.clone(), [0.0, 0.0, qw as f32, qh as f32], [1.0; 4]);
                        r.draw(&mut enc, a, Some([0.0, 0.0, 0.0, 1.0]), &[full]);
                        r.blur(&mut enc, &a.tex, b, [1.0, 0.0], 1.25, BlurMode::FullAdd);
                        r.blur(&mut enc, &b.tex, a, [0.0, 1.0], 1.25, BlurMode::FullAdd);
                        let up = Quad::rect(a.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]);
                        r.draw(&mut enc, rt, Some([0.0, 0.0, 0.0, 1.0]), &[up]);
                    } else if self.assets.build == knife_core::exe::Build::Overseas {
                        // sub_9EC50 (its sigma argument unused): the capture scaled to the half-size "filter
                        // Defocus" buffer, then sub_9CCD0(1.25)'s meshes, 2 ceil(3.75) + 1 = 9 quads offset
                        // sigma sqrt(2 pi) k / 1920 (/ 1080) in UV, each at its weight, horizontally and then
                        // vertically (shader 55, through the backbuffer and copied back), then scaled up 2x
                        let (hw, hh) = (CANVAS_W / 2, CANVAS_H / 2);
                        let [a, b] = self.menu_half.get_or_insert_with(|| {
                            [r.create_canvas_target(hw, hh, "filter defocus"), r.create_canvas_target(hw, hh, "defocus pass")]
                        });
                        let full = Quad::rect(self.canvas.tex.clone(), [0.0, 0.0, hw as f32, hh as f32], [1.0; 4]);
                        r.draw(&mut enc, a, Some([0.0, 0.0, 0.0, 1.0]), &[full]);
                        let step = 1.25 * (2.0 * std::f32::consts::PI).sqrt() * hw as f32 / 1920.0;
                        r.blur(&mut enc, &a.tex, b, [step, 0.0], 1.25, BlurMode::Uv);
                        r.blur(&mut enc, &b.tex, a, [0.0, step], 1.25, BlurMode::Uv);
                        let up = Quad::rect(a.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]);
                        r.draw(&mut enc, rt, Some([0.0, 0.0, 0.0, 1.0]), &[up]);
                    } else {
                        // sub_A0A90(0, 0, 2.25): sigma 2.25 taps spaced 1.75 px (of 1920), horizontally, vertically,
                        // horizontally again and vertically again (targets +107, +214, +107, +321)
                        let step = 1.75 * CANVAS_W as f32 / 1920.0;
                        r.blur(&mut enc, &self.canvas.tex, &self.blur_rt, [step, 0.0], 2.25, BlurMode::FullAdd);
                        r.blur(&mut enc, &self.blur_rt.tex, rt, [0.0, step], 2.25, BlurMode::FullAdd);
                        r.blur(&mut enc, &rt.tex, &self.blur_rt, [step, 0.0], 2.25, BlurMode::FullAdd);
                        r.blur(&mut enc, &self.blur_rt.tex, rt, [0.0, step], 2.25, BlurMode::FullAdd);
                    }
                    self.menu_captured = true;
                }
                // sub_A2680: shader 17 forcealpha, the fade as a colour byte ((unsigned)(a * 255))
                let mut q = Quad::rect(rt.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0, 1.0, 1.0, (a.min(1.0) * 255.0).floor() / 255.0]);
                q.shade = Shade::ForceAlpha;
                r.draw(&mut enc, &self.canvas, None, &[q]);
            }
            None => self.menu_captured = false,
        }
        if !witch {
            if !self.movie_by_ui {
                ui.extend(self.movie_quad());
            }
            ui.extend(self.story_movie.as_ref().and_then(|t| t.cover_quad(r)));
            ui.extend(self.ui_quads(r));
        }
        overlay(self, r, &mut ui);
        for (rt, clear, quads) in std::mem::take(&mut self.ui_passes) {
            r.draw(&mut enc, &rt, clear, &quads);
        }
        if !ui.is_empty() {
            r.draw(&mut enc, &self.canvas, None, &ui);
        }
        if let Some(f) = &mut self.freeze {
            let rt = self.freeze_rt.get_or_insert_with(|| r.create_canvas_target(CANVAS_W, CANVAS_H, "screen capture"));
            if f.capture {
                // sub_A2550 takes the frame being finished
                r.copy_target(&mut enc, &self.canvas, rt);
                f.capture = false;
            } else {
                let q = Quad::rect(rt.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0, 1.0, 1.0, f.alpha.clamp(0.0, 1.0)]);
                r.draw(&mut enc, &self.canvas, None, &[q]);
            }
        }
        r.present(enc, &self.canvas);
    }

    // ---- save / load --------------------------------------------------------------

    /// Whether the game is at a point that can be saved (a message or a choice).
    pub fn can_save(&self) -> bool {
        // a SELS / SELA question is not a resumable point (knife keeps no record kind for it)
        // (Witch on the Holy Night writes its snapshot at the WIK select and the TeaTime menu too: sub_65380's
        // quick save, sub_745E0's save and title buttons)
        let choice_ok = self.choice.as_ref().is_some_and(|c| c.kind == ChoiceKind::Page || c.kind == ChoiceKind::Wik);
        (self.wait == Wait::Click || (self.wait == Wait::Choice && choice_ok) || self.wait == Wait::TeaTime) && !self.ended
    }

    /// The snapshot a save writes: the save point (see `save_point`). The exe's record (word_F17FF0) is only
    /// emptied at boot (sub_4EF00), so it outlives a stage reset; while it is empty no save is written
    /// (sub_50FC0 0x52274, sub_52860, sub_52AA0 check word_F17FF2).
    pub fn save_data(&self) -> Option<crate::save::SaveData> {
        if !self.can_save() {
            return None;
        }
        self.save_point.as_ref().map(|s| crate::save::SaveData { timestamp: crate::save::now(), ..s.clone() })
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
                    // the stage rotation (dword 161)
                    rotation: l.stage_rot.unwrap_or(l.rotation.to),
                    prio: l.stage_prio(),
                    parent: l.parent,
                    base: l.stage_base(),
                    absolute: l.stage_absolute(),
                    // Witch on the Holy Night keeps its one slot's stage words in slot 0: mode = type + 1, delay,
                    // D, then ax, ay, P and the starting D
                    quake: if self.assets.build == Build::Witch {
                        let q = &l.wquake;
                        let [ax, ay, p, d, delay, e] = q.stage;
                        let set = q.stage != [0; 6] || q.stage_kind != 0;
                        [(if set { q.stage_kind + 1 } else { 0 }, delay, d, [ax, ay, p, e, 0, 0]), (0, 0, 0, [0; 6])]
                    } else {
                        l.quake.map(|q| (q.mode, q.delay, q.remaining, q.params))
                    },
                    rule: l.rule,
                    rule_flip: l.rule_flip,
                    noise: l.noise,
                    // the restore takes the blend from `stbr` when one is set (backdrop blends always have one)
                    blend: match l.stage_blend().1 {
                        Blend::Alpha | Blend::Over | Blend::Ps(_) | Blend::Footprint | Blend::MaskAlpha => 0,
                        Blend::Add => 1,
                        Blend::Sub => 2,
                        Blend::Multiply => 3,
                        Blend::Screen => 4,
                        Blend::Opaque => 5,
                    },
                    shade: l.shade as u8,
                    // the record keeps the stage's STEC state; a load builds the texture with it
                    fill: (l.stec_mode == 1).then_some(l.stec_color),
                    grade: if l.stec_mode >= 2 { l.stec_mode } else { 0 },
                    motion: l.motion.as_ref().map(|m| (m.id, m.t)),
                    clip: l.clip.map(|c| [c.x, c.y, c.w, c.h, f32::from(l.clip_inset)]),
                    clip_mode: l.clip_mode,
                    bright: Some(l.snx[0]),
                    contrast: Some(l.snx[1]),
                    stbr: Some(l.stage_blend().0),
                    rot2: l.stage_rot2(),
                    pivot: l.stage_pivot,
                    shift: l.stage_shift,
                    tint: (l.stec_mode != 1 && l.stec_color != [1.0; 3]).then_some(l.stec_color),
                    soxy: l.stage_soxy(),
                    snx4: l.snx4,
                    wfade: crate::assets::witch().then(|| [l.wfade[0], l.drawn_alpha(), l.wfade[1], i16::from(l.wfade_ease)]),
                })
            })
            .collect()
    }

    /// The current message as a page-ring entry (choices are never one: the exe's save record is not refreshed
    /// at them, and knife's rewind does not stop there).
    /// The state as a snapshot resuming at command `pc`.
    fn snapshot_at(
        &self,
        pc: usize,
        page_texts: Vec<String>,
    ) -> crate::save::SaveData {
        use crate::save::{SaveData, SeSave};
        let layers = self.layer_saves();
        let preview = self
            .page
            .texts
            .last()
            .map(|t| resolve_refs(&self.assets, t))
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
            focus: self.focus_saved,
            prev: None,
            page_texts,
            choice: None,
            bgm: self.sound.bgm_state(),
            bgm_word: Some(self.sound.bgm_word),
            loops: self
                .sound
                .looping_effects()
                .into_iter()
                .map(|(channel, name, volume)| SeSave { channel, name, volume })
                .collect(),
            se_words: self.sound.se_word.to_vec(),
            se_loops: self.sound.se_loop.to_vec(),
            se_pans: self.sound.se_pan.to_vec(),
            se_queue: self.se_queue_save(),
            timestamp: crate::save::now(),
            preview,
            kind: 0,
            voice: self.msg_voice.clone(),
            speaker: self.speaker,
            read_key: self.read_key,
            text: String::new(),
        }
    }

    /// sub_3ECC0 at the commit: a pending layer that loads its image builds the texture with the stage's STEC
    /// mode / colour and SNX2 / SNX3 tone (pre_alter); a layer keeping its texture is left as built.
    /// sub_97B80: write one sampled motion frame into layer `li`.
    /// The layer's current value of each motion field in motion units: what sub_97B80 passes sub_97710 as
    /// the fallback (pivot: render floats 25 / 26 x 2 / x 1.5; SNX2, SNX3, priority and colours: 0).
    fn motion_cur(&self, li: usize) -> [f32; crate::motion::FIELDS] {
        use crate::motion::field;
        let mut v = [0.0; crate::motion::FIELDS];
        let Some(l) = self.stage.layers.get(li).and_then(|l| l.as_ref()) else { return v };
        let centre = match &l.source {
            Source::Image(img) => img.anchor(),
            Source::Color(_) => [0.0; 2],
        };
        let piv = l.pivot.unwrap_or(centre);
        let c = l.clip.unwrap_or_default();
        let (va, vb) = self.variant_of(&l.name);
        v[field::X] = l.x.value_x();
        v[field::Y] = l.y.value();
        v[field::ALPHA] = l.alpha.value() * 255.0;
        v[field::PIVOT_X] = piv[0] * 1280.0 / 640.0;
        v[field::PIVOT_Y] = piv[1] * 720.0 / 480.0;
        v[field::ROT] = (l.rotation.value_rot() * 100.0) as i32 as f32;
        v[field::ZOOM_X] = f32::from((l.zoom_x.value() * 1000.0) as i16);
        v[field::ZOOM_Y] = f32::from((l.zoom_y.value() * 1000.0) as i16);
        v[11] = c.x;
        v[12] = c.y;
        v[13] = c.w;
        v[14] = c.h;
        v[field::CLIP_MODE] = f32::from(l.clip_mode);
        v[field::CLIP_INSET] = f32::from(l.clip_inset);
        v[field::BLEND] = l.stbr as f32;
        v[field::FILL] = l.stec_mode as f32;
        v[field::VARIANT_A] = va as f32;
        v[field::VARIANT_B] = vb as f32;
        v[field::GRAIN] = l.noise.unwrap_or(0) as f32;
        v
    }

    fn apply_motion(&mut self, r: &Renderer, li: usize, f: crate::motion::MotionFrame) {
        let variant = self.motion_variant(li, &f);
        let pivot_info = self.stage.layers.get(li).and_then(|l| l.as_ref()).and_then(|l| match &l.source {
            Source::Image(img) => Some((img.level, self.variant_delta(&l.name))),
            Source::Color(_) => None,
        });
        if f.grain.is_some_and(|g| g > 0) {
            self.load_noise(r);
        }
        let Some(l) = self.stage.layers.get_mut(li).and_then(|l| l.as_mut()) else { return };
        if let Some(x) = f.x {
            l.x = Tween::fixed(x);
        }
        if let Some(y) = f.y {
            l.y = Tween::fixed(y);
        }
        if let Some(a) = f.alpha {
            l.alpha = Tween::fixed(a);
        }
        // fields 6 / 7: the pivot in script units (x 4/3, y 362/272 to 640x362 units), scaled by the image level
        // (sub_2DFD0); the render pivot also takes the variant's centre offset, the quad offset is minus the
        // pivot. A layer with an image (BLACK / WHITE included) also gets the stage anchor p and shift word
        // (int)-p, without the offset.
        if f.pivot.iter().any(Option::is_some) {
            let (k, (dx, dy)) = pivot_info.unwrap_or((1.0, (0.0, 0.0)));
            let centre = match &l.source {
                Source::Image(img) => img.anchor(),
                Source::Color(_) => [0.0; 2],
            };
            let mut piv = l.pivot.unwrap_or(centre);
            let mut sh = l.shift.unwrap_or([-centre[0], -centre[1]]);
            let mut spiv = l.stage_pivot.unwrap_or(centre);
            let mut ssh = l.stage_shift.unwrap_or([-centre[0], -centre[1]]);
            for (a, (v, d)) in [(f.pivot[0], dx), (f.pivot[1], dy)].into_iter().enumerate() {
                let Some(v) = v else { continue };
                let p = if a == 0 {
                    v * 1.875 / 0.46875 * k * 0.33333
                } else {
                    v * 1.875 * 576.0 / 272.0 * k * 362.0 / 1080.0
                };
                piv[a] = d + p;
                sh[a] = -p;
                spiv[a] = p;
                ssh[a] = (-p).trunc();
            }
            l.pivot = Some(piv);
            l.shift = Some(sh);
            l.stage_pivot = Some(spiv);
            l.stage_shift = Some(ssh);
        }
        if let Some(rot) = f.rot {
            // render dword 20 and stage dword 161
            l.rotation = Tween::fixed(rot);
            l.stage_rot = None;
        }
        // render word 164 and stage word 1522
        if let Some(v) = f.rot2 {
            l.rot2 = v as i16;
            l.staged.rot2 = None;
        }
        if let Some(z) = f.zoom_x {
            l.zoom_x = Tween::fixed(z);
        }
        if let Some(z) = f.zoom_y {
            l.zoom_y = Tween::fixed(z);
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
            l.clip = Some(c);
        }
        if let Some(v) = f.clip_mode {
            l.clip_mode = v as i8;
        }
        if let Some(v) = f.clip_inset {
            l.clip_inset = v as i16;
        }
        // field 17: 0 (or noalpha on a translucent layer) drops the blend, another mode replaces it
        if let Some(v) = f.blend {
            let mode = if v == 0 || (v == 8 && l.alpha.value() < 1.0) { -1 } else { v };
            if mode != l.stbr {
                l.stbr = mode;
                l.blend = blend_mode(mode).0;
                l.staged.stbr = None;
            }
        }
        // field 18 = 1 on a layer that is not a fill sets the render colour (fields 19..21) it is multiplied by
        if f.fill.is_some_and(|v| v == 1 && l.stec_mode != 1) {
            l.stec_color = rgb(f.fill_color);
        }
        if let Some(c) = f.color {
            l.stec_color = rgb(c);
        }
        // SNX2 / SNX3 reach the stage and the next texture build only
        if let Some(v) = f.snx2 {
            l.snx[0] = v;
        }
        if let Some(v) = f.snx3 {
            l.snx[1] = v;
        }
        if let Some(g) = f.grain
            && l.noise.unwrap_or(0) != g
        {
            l.noise = (g > 0).then_some(g);
        }
        // field 27 moves the layer only while its stage priority is not 0: STCP(layer, 0) hides it from a running
        // motion; it writes render dword 5883 and the stage word
        if let Some(p) = f.prio
            && p != 0
            && l.stage_prio() != 0
        {
            l.prio = p;
            l.staged.prio = None;
        }
        if let Some(name) = variant {
            let (bake, cur) = (l.bake, l.name.clone());
            if let Some(img) = self.assets.image_graded(r, &name, bake) {
                let delta = self.variant_delta(&name);
                let old = self.variant_delta(&cur);
                let l = self.stage.layers[li].as_mut().unwrap();
                // sub_97B80 sets the render pivot and shift from the new image's exact centre (render floats 25 /
                // 26 and 18 / 19), not the whole-unit stage anchor
                match &mut l.pivot {
                    Some(p) => {
                        p[0] += delta.0 - old.0;
                        p[1] += delta.1 - old.1;
                    }
                    None => l.pivot = Some([img.center_x, img.center_y]),
                }
                l.shift = Some([-img.center_x, -img.center_y]);
                // the stage anchor moves by the difference of the two images' centres and the shift word is
                // (int) minus it
                let spiv = match &l.source {
                    Source::Image(o) => {
                        let s = l.stage_pivot.unwrap_or(o.anchor());
                        [s[0] + img.center_x - o.center_x, s[1] + img.center_y - o.center_y]
                    }
                    Source::Color(_) => [img.center_x, img.center_y],
                };
                l.stage_pivot = Some(spiv);
                l.stage_shift = Some([(-spiv[0]).trunc(), (-spiv[1]).trunc()]);
                l.source = Source::Image(img);
                l.name = name;
            }
        }
    }

    /// The `_aa_bb` suffix numbers of an image name (sub_644A0), (0, 0) without one.
    fn variant_of(&self, name: &str) -> (i32, i32) {
        if !self.assets.image_info(name).is_some_and(|i| i.variant) || name.len() < 6 {
            return (0, 0);
        }
        let b = name.as_bytes();
        let n = b.len();
        let d = |i: usize| i32::from(b[i]) - i32::from(b'0');
        (d(n - 5) * 10 + d(n - 4), d(n - 2) * 10 + d(n - 1))
    }

    /// ImageInfo centre of a variant image minus its base image's (sub_97B80 v109 / v110).
    fn variant_delta(&self, name: &str) -> (f32, f32) {
        if self.variant_of(name) == (0, 0) {
            return (0.0, 0.0);
        }
        let base = &name[..name.len() - 6];
        match (self.assets.image_info(name), self.assets.image_info(base)) {
            (Some(v), Some(b)) => (v.center_x - b.center_x, v.center_y - b.center_y),
            _ => (0.0, 0.0),
        }
    }

    /// Fields 22 / 23: the image variant a motion frame switches the layer to, if it changes. The exe edits the
    /// name in place: `_00_00` is appended to a base name, each number writes its ones digit and, from 10, its
    /// tens digit (a smaller number keeps the old tens), and 0 / 0 strips the suffix.
    fn motion_variant(&self, li: usize, f: &crate::motion::MotionFrame) -> Option<String> {
        if f.variant.iter().all(Option::is_none) {
            return None;
        }
        let l = self.stage.layers.get(li)?.as_ref()?;
        if !matches!(l.source, Source::Image(_)) {
            return None;
        }
        let cur = self.variant_of(&l.name);
        let a = f.variant[0].unwrap_or(cur.0);
        let b = f.variant[1].unwrap_or(cur.1);
        if (a, b) == cur {
            return None;
        }
        let mut dest = l.name.clone().into_bytes();
        if (a | b) == 0 {
            dest.truncate(dest.len().saturating_sub(6));
        } else {
            if cur == (0, 0) {
                dest.extend_from_slice(b"_00_00");
            }
            let n = dest.len();
            for (v, ones) in [(a, n - 4), (b, n - 1)] {
                if v >= 0 {
                    dest[ones] = b'0' + (v % 10) as u8;
                    if v >= 10 {
                        dest[ones - 1] = b'0' + (v / 10 % 10) as u8;
                    }
                }
            }
        }
        let name = String::from_utf8(dest).ok()?;
        self.assets.image_index_of(&name).map(|_| name)
    }

    fn rebake(&mut self, li: i32) {
        let Some(l) = self.stage.get(li) else { return };
        if !l.pending || l.keeps_texture {
            return;
        }
        let bake = crate::grade::Bake::new(l.stec_mode, l.snx[0], l.snx[1]);
        if l.stec_mode == 1 {
            l.fill = Some(bake.fill(l.stec_color));
        }
        if bake != l.bake {
            l.bake = bake;
            // the texture is built with it at the commit (sub_3C970 -> sub_3ECC0), after any further STEC /
            // SNX2 / SNX3 on the layer
            l.regrade = matches!(l.source, Source::Image(_));
        }
    }

    /// The commit's texture builds (sub_3C970 -> sub_3ECC0): each layer an STEC / SNX2 / SNX3 regraded gets its
    /// image built with the grade, the images decoding side by side.
    fn build_textures(&mut self, r: &Renderer) {
        let want: Vec<(usize, String, crate::grade::Bake)> = (self.stage.layers.iter().enumerate())
            .filter_map(|(i, l)| l.as_ref().filter(|l| l.regrade).map(|l| (i, l.name.clone(), l.bake)))
            .collect();
        for (_, name, bake) in &want {
            self.assets.prefetch(name, *bake);
        }
        for (i, name, bake) in want {
            let img = self.assets.image_graded(r, &name, bake);
            if let Some(l) = self.stage.layers[i].as_mut() {
                l.regrade = false;
                if let Some(img) = img {
                    l.source = Source::Image(img);
                }
            }
        }
    }

    /// A stage layer as a snapshot describes it (restores and save list previews).
    fn layer_from_save(&mut self, r: &Renderer, ls: &crate::save::LayerSave) -> Option<Layer> {
        let source = match (&ls.image, ls.color) {
            (Some(name), _) => {
                let img = self.assets.image_graded(r, name, ls.bake())?;
                Source::Image(img)
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
        // only the stage word: no load writes render +280
        l.rule = ls.rule;
        l.rule_flip = ls.rule_flip;
        if self.assets.build == Build::Witch {
            let (mode, delay, d, [ax, ay, p, e, ..]) = ls.quake[0];
            if mode >= 1 {
                l.wquake = WitchQuake::restore(mode - 1, [ax, ay, p, d, delay, e]);
            }
        } else {
            l.quake = ls.quake.map(|(mode, delay, remaining, params)| crate::stage::Quake::restore(mode, delay, remaining, params));
        }
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
        l.rot2 = ls.rot2;
        // sub_5D3A0 gives the render pivot and shift the stage's
        l.pivot = ls.pivot;
        l.stage_pivot = ls.pivot;
        l.shift = ls.shift;
        l.stage_shift = ls.shift;
        l.clip_mode = ls.clip_mode;
        l.soxy = ls.soxy;
        l.shade = match ls.shade {
            3 => Shade::Mono,
            4 => Shade::Nega,
            _ => Shade::Sprite,
        };
        l.bake = ls.bake();
        l.fill = ls.fill.map(|c| l.bake.fill(c));
        (l.stec_mode, l.stec_color) = match ls.fill {
            Some(c) => (1, c),
            None => (ls.grade, ls.tint.unwrap_or([1.0; 3])),
        };
        l.motion = ls.motion.map(|(id, t)| crate::motion::MotionState::new(id, t));
        l.clip = ls.clip.map(|[x, y, w, h, _]| crate::stage::ClipRect { x, y, w, h, ..Default::default() });
        l.clip_inset = ls.clip.map_or(0, |c| c[4] as i16);
        l.snx = [ls.bright.unwrap_or(1.0), ls.contrast.unwrap_or(1.0)];
        l.snx4 = ls.snx4;
        // the commit that presents the restored stage goes on with an STGS fade from the saved drawn alpha
        if let Some([f, c, n, e]) = ls.wfade {
            (l.wfade, l.wfade_ease) = ([f, n], e as u8);
            l.rebuild_fade(c);
        } else {
            l.wfade = [(ls.alpha * 255.0).round() as i16, 0];
        }
        l.pending = false;
        Some(l)
    }

    /// The preview of save slot `slot` showing record `d`, or None until `render` has drawn it.
    pub fn preview(&mut self, slot: usize, d: &crate::save::SaveData) -> Option<TexRef> {
        if let Some(t) = self.previews.get(&slot) {
            return Some(t.clone());
        }
        if !self.preview_queue.iter().any(|(s, ..)| *s == slot) {
            let pic = self.preview_picture(d);
            self.preview_queue.push((slot, d.layers.to_vec(), pic));
        }
        None
    }

    /// Overseas sub_86840 (from sub_884F0): a record of one of these scenes whose F1122 (the PGST id, read
    /// through sub_55300) is below 1 or the scene's value shows the picture alone instead of its stage.
    fn preview_picture(&self, d: &crate::save::SaveData) -> Option<String> {
        if self.assets.build != knife_core::exe::Build::Overseas {
            return None;
        }
        let (n, want) = match d.scene {
            4 => (0, 10000),
            35 => (1, 10000),
            60 => (2, 10000),
            81 | 109 => (3, 10000),
            116 => (4, 10000),
            157 => (5, 10015),
            169 => (6, 10000),
            183 => (7, 10000),
            197 => (8, 10000),
            218 => (9, 10000),
            239 => (10, 10000),
            266 => (11, 10000),
            281 => (12, 10000),
            298 => (13, 10000),
            323 => (14, 10000),
            348 => (15, 10000),
            322 => (16, 10029),
            376 => (17, 10000),
            385 => (18, 10000),
            391 => (19, 10000),
            398 => (20, 10000),
            416 => (21, 10000),
            424 => (22, 10000),
            454 => (23, 10000),
            490 => (24, 10000),
            493 => (25, 10000),
            494 => (26, 10000),
            _ => return None,
        };
        let pgst = d.vars.iter().find(|(k, _)| k == "F1122").map_or(0, |v| v.1);
        // sub_62110 (sub_56D40 on the image table): an index below 1 keeps the stage
        let name = format!("IMG60{n:02}");
        (pgst < 1 || pgst == want).then_some(name).filter(|n| self.assets.image_index_of(n).is_some_and(|i| i >= 1))
    }

    /// Forget the previews (of one slot, or all) after their records changed.
    pub fn clear_previews(&mut self, slot: Option<usize>) {
        match slot {
            Some(s) => {
                self.previews.remove(&s);
                self.preview_queue.retain(|(q, ..)| *q != s);
            }
            None => {
                self.previews.clear();
                self.preview_queue.clear();
            }
        }
    }

    /// Draw the queued previews: the layers as a stage on black into a `PREVIEW_TARGET` surface, then scaled to
    /// `PREVIEW_SIZE`.
    /// A preview waits until the worker threads have decoded the images it is missing.
    fn render_previews(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) {
        self.assets.finish_prefetched(r);
        if self.preview_queue.is_empty() {
            return;
        }
        let mut ready = Vec::new();
        for (slot, mut layers, pic) in std::mem::take(&mut self.preview_queue) {
            // the previews draw the thumbnail copies of the art (sub_895A0)
            for ls in layers.iter_mut().flatten() {
                if let Some(name) = ls.image.as_mut().filter(|n| !n.starts_with(crate::assets::THUMB)) {
                    *name = self.assets.thumb_name(name);
                }
            }
            let pic = pic.map(|p| self.assets.thumb_name(&p));
            let mut waiting = false;
            let none = crate::grade::Bake::new(0, 1.0, 1.0);
            let wanted = layers.iter().flatten().filter_map(|ls| ls.image.as_ref().map(|n| (n, ls.bake())));
            let wanted: Vec<(&String, crate::grade::Bake)> = match &pic {
                Some(p) => vec![(p, none)],
                None => wanted.collect(),
            };
            for (name, bake) in wanted {
                if self.assets.image_cached(name, bake) {
                    continue;
                }
                waiting = true;
                self.assets.prefetch(name, bake);
            }
            if waiting {
                self.preview_queue.push((slot, layers, pic));
            } else {
                ready.push((slot, layers, pic));
            }
        }
        if ready.is_empty() {
            return;
        }
        // the "thumbnail surface" sub_89AD0 draws into (sub_A5C10)
        let (tw, th) = PREVIEW_TARGET;
        let rt = self.preview_rt.get_or_insert_with(|| r.create_canvas_target(tw, th, "preview")).clone();
        for (slot, layers, pic) in ready {
            let scale = tw as f32 / CANVAS_W as f32;
            let quads: Vec<Quad> = if let Some(p) = pic {
                // the one layer sub_884F0 then sets up: the picture at 0, 0, zoom 2 / 3, white, no parent, its
                // ImageInfo centre (+124 / +128) 0 so the anchor is the top left. sub_89AD0 draws a texture at
                // its full size (the 1/6 thumbnail / 0.16667) times 1.5 (stage art is defined at half its pixels
                // in 640-wide units, drawn 3x) times the zoom: the 1920x1080 picture strip fills the surface
                let none = crate::grade::Bake::new(0, 1.0, 1.0);
                match self.assets.image_graded(r, &p, none) {
                    Some(img) => {
                        let k = 6.0 * 1.5 * 2.0 / 3.0;
                        let (w, h) = (img.tex.width as f32 * k, img.tex.height as f32 * k);
                        vec![Quad::rect(img.tex.clone(), [0.0, 0.0, w, h], [1.0; 4]).scaled(scale)]
                    }
                    None => Vec::new(),
                }
            } else {
                self.preview_stage_quads(r, &layers).into_iter().map(|q| q.scaled(scale)).collect()
            };
            r.draw(enc, &rt, Some([0.0, 0.0, 0.0, 1.0]), &quads);
            // sub_4F240 draws the target bilinearly into the 248x140 slot picture (82.7 x 57.9 of 640x448)
            let (pw, ph) = PREVIEW_SIZE;
            let out = r.create_canvas_target(pw, ph, "preview");
            r.draw(enc, &out, Some([0.0, 0.0, 0.0, 1.0]), &[Quad::rect(rt.tex.clone(), [0.0, 0.0, pw as f32, ph as f32], [1.0; 4])]);
            self.previews.insert(slot, out.tex);
        }
    }

    /// The record's stage as sub_884F0 keeps it, in canvas units.
    fn preview_stage_quads(&mut self, r: &Renderer, layers: &[Option<crate::save::LayerSave>]) -> Vec<Quad> {
        {
            let mut stage = Stage::new();
            // sub_884F0 keeps a layer with an image, alpha and priority >= 1, a clip that is unset or has a
            // size, no visible parent with a broken clip, and no STNS grain
            let shown = |ls: &crate::save::LayerSave| (ls.alpha * 255.0).round() >= 1.0 && ls.prio >= 1;
            let clip_ok = |ls: &crate::save::LayerSave| ls.clip.is_none_or(|c| c[2] != 0.0 && c[3] != 0.0);
            let keep = |ls: &crate::save::LayerSave| {
                let parent_bad = ls
                    .parent
                    .and_then(|p| layers.get(p).and_then(|l| l.as_ref()))
                    .is_some_and(|p| shown(p) && !clip_ok(p));
                shown(ls) && clip_ok(ls) && !parent_bad && ls.noise.is_none_or(|n| n == 0)
            };
            for (i, ls) in layers.iter().enumerate() {
                if let Some(l) = ls.as_ref().filter(|ls| keep(ls)).and_then(|ls| self.layer_from_save(r, ls)) {
                    stage.set(i as i32, l);
                }
            }
            stage.quads(&r.white)
        }
    }

    /// A load (sub_527A0 -> sub_82330): the record is copied in as the save point, the page ring starts
    /// over (the backlog with it), and the scene ring holds the record's previous entry (its text cleared)
    /// and the record; sub_5D950 restores the record (sub_5BAF0 LABEL_137 makes a qualifying one the save
    /// point again, with itself as the previous entry) and drops it from the scene ring. The message count
    /// is 0 afterwards.
    pub fn restore(&mut self, r: &Renderer, d: &crate::save::SaveData) {
        self.save_point = Some(d.clone());
        self.menu_lock = true;
        self.history.clear();
        self.backlog_pages.clear();
        self.page_ring = (None, None);
        // sub_5D950 starts the page ring over with the record in slot 0 (sub_5BAF0(0, 0) makes it the newest);
        // sub_82330 then sets AE0460 = AE04D0 = 0, so the replayed message's push takes slot 0 again and the
        // ring (and the backlog) holds the page once
        let key = (d.scene, d.pc, usize::MAX - 1);
        self.push_ring(crate::save::SaveData { prev: None, ..d.clone() }, key, false, d.kind);
        self.scene_ring.clear();
        if let Some(p) = d.prev.as_deref() {
            let mut p = p.clone();
            p.text.clear();
            self.scene_ring.push(ScenePoint { data: p, serial: None });
        }
        self.scene_ring.push(ScenePoint { data: crate::save::SaveData { prev: None, ..d.clone() }, serial: None });
        // sub_82330 -> sub_3C550 resets the render layers: no rule survives a load
        self.stage.rules.fill(None);
        self.restore_with(r, d, false);
        self.restore_save_point(d, false, d.kind);
        self.scene_ring.pop();
        self.scene_msgs = 0;
        self.ring_reuse = true;
    }

    /// `rewind`: the BGM keeps playing when the snapshot has the same one, the window stays hidden until the
    /// replay and the voice waits for the replayed message; otherwise (a load / scene point, sub_5D950) the
    /// voice plays at once.
    fn restore_with(&mut self, r: &Renderer, d: &crate::save::SaveData, rewind: bool) {
        // sub_5BAF0 ends the engine tasks (sub_3F4A0 on slots 0..2) before the replay
        self.end_staff_task();
        // byte_146EC28 comes back from the entry; `@n` holds end
        self.msg_text = d.text.clone();
        self.ring_hold = false;
        self.stop_movie();
        // a load stops all sound at once (sub_82330 -> sub_5EF10(1)); a rewind (sub_5D710) keeps the BGM when
        // it is the same track. Both then resume the saved words (sub_5D7E0).
        self.seek_audio = None;
        if rewind && self.sound.bgm_state().map(|b| b.0) == d.bgm.map(|b| b.0) {
            self.sound.stop_se(-1);
            self.sound.stop_voice();
            self.sound.stop_movie_audio();
        } else {
            self.sound.stop_all();
        }
        self.choice = None;
        self.teatime = None;
        self.sel_opts.clear();
        self.trans = None;
        // sub_2EA70(-1) / sub_3C550: the screen shake and the message quake stop
        self.shake.stop();
        self.msg_quake.stop();
        let noise = self.stage.noise.take();
        // sub_5BAF0 / sub_5D3A0 never write the render rules (+280): a rewind or the seek keeps them
        let rules = std::mem::take(&mut self.stage.rules);
        let soft = self.stage.rule_soft;
        self.stage = Stage::new();
        self.stage.noise = noise;
        self.stage.rules = rules;
        self.stage.rule_soft = soft;
        for (k, v) in self.stage.kept.iter_mut().zip(&d.kept) {
            *k = *v;
        }
        self.stage_backup = d.backup.clone();
        self.vars.retain(|k, _| k.starts_with('G'));
        for (k, v) in &d.vars {
            self.vars.insert(k.clone(), *v);
        }
        // the replayed message's icon kind: the page-break flag WNTY kept in F799 (sub_38EA0)
        self.wnty = self.var("F799");
        // sub_5BAF0: sub_2E430(F1100) restarts the screen effect from the restored words
        self.ghost.set_mode(self.var("F1100"), self.var("F1120") - 127, self.var("F1121"));
        // and sub_86470(stage byte 6569, 0) the FCLN effect
        self.focus_saved = d.focus;
        self.focus.start(i32::from(d.focus as i8), 0);
        for (i, ls) in d.layers.iter().enumerate() {
            let Some(ls) = ls else { continue };
            if let Some(l) = self.layer_from_save(r, ls) {
                self.stage.set(i as i32, l);
            }
        }
        // sub_5D3A0(0, -1) presents the restored stage, with the record's backdrop word
        if self.assets.build == Build::Witch {
            self.stage.backdrop = u8::from(!crate::savedata::witch_dark_scene(d.scene));
        }
        self.commit_stage(r);
        // sub_5BAF0: the SE channels' volume and pan words and their queued key-68 ops come back (a record
        // without them: the reset words, 100 / centre, with the loops' volumes)
        self.sound.se_word = [100; 20];
        self.sound.se_pan = [0; 20];
        self.sound.se_loop = [false; 20];
        for se in &d.loops {
            if let Some(f) = usize::try_from(se.channel).ok().and_then(|c| self.sound.se_loop.get_mut(c)) {
                *f = true;
            }
            if let Some(w) = usize::try_from(se.channel).ok().and_then(|c| self.sound.se_word.get_mut(c)) {
                *w = (se.volume * 100.0).round() as i16;
            }
        }
        for (w, &v) in self.sound.se_word.iter_mut().zip(&d.se_words) {
            *w = v;
        }
        for (w, &v) in self.sound.se_pan.iter_mut().zip(&d.se_pans) {
            *w = v;
        }
        for (f, &v) in self.sound.se_loop.iter_mut().zip(&d.se_loops) {
            *f = v;
        }
        // sub_82330: the BGM volume word comes back with the record
        if let Some(w) = d.bgm_word.or_else(|| d.bgm.map(|b| (b.1 * if crate::assets::witch() { 255.0 } else { 100.0 }).round() as i16)) {
            self.sound.bgm_word = w;
        }
        self.se_delayed = d.se_queue.iter().map(|q| (q.clone(), f32::from(q.frames - 1).max(0.0) * 1000.0 / 60.0, true)).collect();
        self.resume_sound(&SoundState {
            bgm: d.bgm,
            loops: d.loops.iter().map(|se| (se.channel, (se.name.clone(), se.volume))).collect(),
        });
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
        self.clear_page(false);
        // (a rewind keeps the window: sub_391A0 only leaves the text out while it runs)
        if !rewind {
            self.page.visible = false;
            self.page.alpha = 0.0;
        }
        self.page.quick = false;
        self.page.dim = 0.0;
        self.page.dim_vel = 0.0;
        for t in &d.page_texts {
            self.show_message(t, false);
        }
        // sub_5BAF0 case 7: an ATSV(1) entry shows its message text again (sub_5AFC0)
        if d.kind == 7 && !d.text.is_empty() {
            self.show_message(&message_end(&d.text).0, false);
        }
        self.page.revealed = self.page.order as f32;
        for m in &mut self.page.msgs {
            m.2 = 1.0;
        }
        self.pc = d.pc;
        self.wait = Wait::None;
        self.click_ms = 0.0;
        // sub_5BAF0 makes the entry's voice pending (AE04C8, AE08B0) for both a load and a rewind: the script
        // replays the message (a load resumes at the entry's offset; sub_5BAF0's own layout of the message runs
        // for a rewind only), whose start (sub_6E0D0) plays it and makes it the message's voice. A load only
        // sets the speaker's voice level from it (sub_5D950 0x5ee6c).
        self.pending_voice = None;
        self.stage_dirty = false;
        self.voice_to_play = None;
        self.msg_voice = d.voice.clone();
        self.speaker = d.speaker;
        // the entry's read key is current again (sub_5BAF0), so the next `_Z` command marks it read
        self.read_key = d.read_key;
        self.last_voice = None;
        self.pending_voice = d.voice.clone();
        self.voice_to_play = d.voice.clone();
        self.log_voice = false;
        self.since_restore = 0.0;
        if let Some((var, opts)) = &d.choice {
            let opts = opts.iter().map(|(v, t)| ChoiceOpt { value: *v, text: t.clone(), disabled: false }).collect();
            self.start_choice(Choice::new(var.clone(), ChoiceKind::Page, opts));
        }
        self.snapshot = false;
        self.fads = None;
    }

    /// cGallery replay (sub_5040, then sub_48AC0 case 4): the BGM fades out at 3.33 / frame (sub_9C9B0: 0.5 s
    /// from full volume), G008 =
    /// the scene id, and 528_ARCHIVE (id 3000) starts with the gallery-start flag set; under `IFSC` it sets
    /// F024 / F025 (no saves) and F822 (page mode), then jumps by G008 through the JUMPxx / JUMPQA scenes.
    pub fn start_archive(&mut self, scene_id: i32) -> bool {
        let Some(idx) = self.assets.scene_by_id(3000) else { return false };
        self.sound.stop_bgm_rate(10.0 / 3.0);
        self.reset();
        self.set_var("G008", scene_id);
        self.archive = true;
        self.start_scene(idx, None);
        self.wait = Wait::None;
        true
    }

    /// `reset` that leaves the BGM playing (sub_47A90 keeps BGM 2 when the gallery comes back).
    /// The story ended (sub_48AC0 case 2 state 1 on sub_40ED0 returning 1 / 2): the window's shown flag goes
    /// (sub_2E120(0)) and AE05A0 = 1 keeps it fading out at the 128 ms rate; an RDED end of a replay with F0 == 0
    /// also empties the stage at once (sub_3EFF0)
    /// sub_4D7E0's end (the system menu has faded out to leave the story): sub_5F200 drops the text slots
    /// and the message, so the window does not come back while the story fades to black.
    pub fn drop_messages(&mut self) {
        self.clear_page(false);
        self.page.visible = false;
        self.page.alpha = 0.0;
        self.page.quick = false;
    }

    pub fn end_story_window(&mut self) {
        self.page.visible = false;
        self.page.quick = false;
        if self.replay_done && self.var("F0") == 0 {
            self.stage.clear(-1);
            self.trans = None;
        }
    }

    pub fn reset_keep_bgm(&mut self) {
        let bgm = self.sound.take_bgm();
        self.reset();
        self.sound.put_bgm(bgm);
    }

    pub fn reset(&mut self) {
        self.menu_lock = true;
        self.white = 0.0;
        self.resume_ms = 0.0;
        // sub_57240 clears the queued SE commands
        self.se_delayed.clear();
        self.history.clear();
        self.ring_reuse = false;
        self.backlog_pages.clear();
        self.read_key = None;
        self.node_seq = 0;
        self.seek_node = false;
        self.seek_ended = false;
        self.seek_defer = false;
        self.ring_restored = false;
        self.seek_audio = None;
        self.speed = 1;
        self.ghost.set_mode(0, 0, 0);
        // sub_5F200: sub_86470(0, 0)
        self.focus.start(0, 0);
        self.focus_saved = 0;
        self.stop_movie();
        self.sound.stop_all();
        // sub_57240: the SE channels' volume words read 100, their loop flags and pans 0
        self.sound.se_word = [100; 20];
        self.sound.se_loop = [false; 20];
        self.sound.se_pan = [0; 20];
        // sub_57450 resets every layer field but the anchor (sub_3D70 starts past it)
        let leftovers = self.reset_leftovers();
        let soft = self.stage.rule_soft;
        self.stage = Stage::new();
        self.stage.kept = leftovers;
        self.stage.rule_soft = soft;
        self.stage_backup = None;
        // sub_5A880: the page ring and the scene ring start over, the message text goes
        self.scene_ring.clear();
        self.msg_text.clear();
        self.ring_hold = false;
        self.keep_bgm_at_end = false;
        self.pending_voice = None;
        self.stage_dirty = false;
        self.voice_to_play = None;
        self.msg_voice = None;
        self.speaker = 0;
        self.vars.retain(|k, _| k.starts_with('G'));
        self.clear_page(false);
        self.page.visible = false;
        self.page.alpha = 0.0;
        self.page.quick = false;
        self.page.dim = 0.0;
        self.page.dim_vel = 0.0;
        self.choice = None;
        self.teatime = None;
        self.sel_opts.clear();
        self.trans = None;
        self.fads = None;
        // sub_2EA70(-1) / sub_3C550: the screen shake and the message quake stop
        self.shake.stop();
        self.msg_quake.stop();
        self.wait = Wait::Halt;
        self.ended = false;
        self.replay_done = false;
        self.auto_skip_hit = false;
        self.banner_away = false;
        self.hold = false;
        self.end_staff_task();
        self.archive = false;
    }

    /// Flowchart jump (sub_11A58 -> sub_95800 / sub_178C8, then sub_48AC0 case 2: sub_40CD0, sub_17AF0): the
    /// current playthrough rewinds to node `n`. The play bits of every later row are cleared, chapter words
    /// by the row (sub_95800), F026 = the node's scene id and F110 = n are set, and a subset of F words is
    /// kept through the scene load that clears all F words (sub_57200); the scene starts from its top, where
    /// the scripts' `IF__(F026, ne, 0)` blocks set up the jump entry. Nothing is stored per node.
    pub fn jump_node(&mut self, n: u16) -> bool {
        let Some((row, scene_id)) = crate::flow::node_entry(n) else { return false };
        self.jump_record(row, i32::from(n), scene_id)
    }

    /// `jump_node` for the record (table row, id, scene id) under the flowchart cursor. A filler record
    /// (scene -1, id 0) gets the same rewind, F110 = 0 and F026 = -1, but no scene starts (sub_17AD0 < 0): the
    /// playthrough stays where it is and the system menu stays open; returns false then.
    pub fn jump_record(&mut self, row: usize, n: i32, scene_id: i32) -> bool {
        let idx = self.assets.scene_by_id(scene_id);
        if scene_id >= 0 && idx.is_none() {
            return false;
        }
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
        self.set_var("F110", n);
        self.set_var("F026", scene_id);
        self.set_var("F1103", 1);
        for w in [823, 847, 1104, 1105, 1106, 1107, 1108, 1109, 1110, 1111, 1112, 1113] {
            self.set_var(&f(w), 0);
        }
        let Some(idx) = idx else { return false };
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
                    l.x.value_x(),
                    l.y.value(),
                    l.zoom_x.value(),
                    l.zoom_y.value(),
                    l.alpha.value(),
                    l.rotation.value_rot(),
                    l.blend,
                    l.shade,
                    l.fill,
                    l.motion.as_ref().map(|m| (m.id, m.t as i32)),
                    l.pending,
                    l.quake
                        .iter()
                        .filter(|q| q.active())
                        .map(|q| format!(" quake{} ({:.1},{:.1})", q.mode, q.offset.0, q.offset.1))
                        .collect::<String>()
                        + &if l.rot2 != 0 || l.clip.is_some() { format!(" rot2 {} clip {:?}", l.rot2, l.clip) } else { String::new() }
                        + &l.noise.map_or(String::new(), |n| format!(" noise {n}{}", if self.stage.noise.is_some() { "" } else { " (no tex)" }))
                );
            }
        }
        s
    }

    /// The canvas's pixel size (`screenshot`'s)
    pub fn canvas_size(&self) -> (u32, u32) {
        (self.canvas.tex.width, self.canvas.tex.height)
    }

    /// RGBA8 copy of the last presented canvas as the screen shows it (through the output curve).
    pub fn screenshot(&self, r: &Renderer) -> Vec<u8> {
        let lut: Vec<u8> = (0..=255).map(knife_render::target_level).collect();
        let mut px = r.read_target(&self.canvas);
        for c in px.chunks_mut(4) {
            for v in &mut c[..3] {
                *v = lut[usize::from(*v)];
            }
        }
        px
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
/// from F201 (sub_92340) and edge bits from F600 (sub_92380), 8 per word; F110 is the current node.
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
    }

    /// The playthrough's current node, F110 (byte 0xDC of the F words: set by RTST for nodes 533..557 or
    /// while F24 is set, and to the target node at every scene JUMP, sub_954B0). The in-game flowchart
    /// opens with its cursor there (sub_1767C).
    pub fn current_node(&self) -> i32 {
        self.var("F110")
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
/// sub_7EDA0 (Witch on the Holy Night): the rule wipe edge width, 64 for <= 0 or >= 255.
fn soft_width(v: f32) -> f32 {
    if v >= 255.0 || v <= 0.0 { 64.0 } else { v }
}

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

/// sub_53CF0: `$NNNNNN` references a script_text line of the game language, anywhere in the text (the QA
/// scenes join two with `^`, a line break: `$042766^$042767`); scripts have no other `$`, so resolved text
/// passes through unchanged.
fn resolve_refs(assets: &Assets, s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let n = tail.bytes().take_while(u8::is_ascii_digit).count();
        match tail[..n].parse::<usize>() {
            Ok(i) => out.push_str(assets.line(i)),
            Err(_) => out.push('$'),
        }
        rest = &tail[n..];
    }
    out.push_str(rest);
    out
}

/// sub_6E0D0 / sub_6E950: the raw message text in AE0910 (768 bytes): `base` followed by `raw` up to its end
/// code, and the code (`e` / `n`).
fn raw_message(raw: &str, base: &str) -> (String, Option<u8>) {
    let bytes = raw.as_bytes();
    let end = (0..bytes.len().saturating_sub(1)).find(|&i| bytes[i] == b'@' && matches!(bytes[i + 1], b'e' | b'n'));
    let body = end.map_or(raw, |i| &raw[..i]);
    let mut t = format!("{base}{body}");
    if t.len() > 767 {
        let mut n = 767;
        while !t.is_char_boundary(n) {
            n -= 1;
        }
        t.truncate(n);
    }
    (t, end.map(|i| bytes[i + 1]))
}

/// The F823 word a ring entry recorded (its page message count).
fn entry_f823(d: &crate::save::SaveData) -> i32 {
    d.vars.iter().find(|(k, _)| k == "F823").map_or(0, |v| v.1)
}

/// The entry's copy of the text (+13956, 0x1FE bytes).
fn entry_text(t: &str) -> String {
    let mut n = t.len().min(ENTRY_TEXT);
    while !t.is_char_boundary(n) {
        n -= 1;
    }
    t[..n].to_string()
}

/// A message's text up to its end code (sub_6E0D0 / sub_6E950 stop copying at `@e` or `@n`) and whether the
/// script then waits for a click: only messages without an end code do (flag 0x400). `@n` / `@e` go on once
/// their text is out (and an `@e` message's voice has ended, `Wait::Message`); an `@k` before the code is no
/// wait (the slot text skips it, sub_BDC2C) and only groups the message's read fade (sub_391A0).
fn message_end(text: &str) -> (String, bool) {
    let bytes = text.as_bytes();
    let end = (0..bytes.len().saturating_sub(1)).find(|&i| bytes[i] == b'@' && matches!(bytes[i + 1], b'e' | b'n'));
    match end {
        Some(i) => (text[..i].to_string(), false),
        None => (text.to_string(), true),
    }
}
