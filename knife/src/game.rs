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

pub use choice::{ChoiceKind, ChoiceOpt, DIALOG_BUTTON_Y, DIALOG_BUTTONS};
use choice::Choice;
use story_movie::StoryMovie;

use knife_core::exe::Build;
use knife_core::vm::parse::{Command, Op, Scene};
use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, RenderTarget, Renderer, Shade, TexRef};

use crate::{
    assets::Assets,
    motion::MotionTable,
    sound::Sound,
    stage::{Layer, Source, Stage, Tween},
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
    /// per message: F(847 + its index on the page) when it was written, which its history record keeps
    /// (sub_28470 spaces the backlog entries with it)
    extras: Vec<i32>,
    pen: (f32, f32),
    order: usize,
    /// where the last message's layout started (pen, order): MSAD lays the grown message out again from here
    last_start: ((f32, f32), usize),
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
    /// SELD options listed after the page's messages (history entries of type 12, shown in the backlog)
    choice_texts: Vec<String>,
    /// their F(847 + index), as `extras`
    choice_extras: Vec<i32>,
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
    line_height: 0.0,
    ascent: ASCENT,
};
/// English (overseas build, sub_391A0 with sub_2DF20, docked): page text from x 32 units, line 0's box at
/// (int)(65 - 9.8824) = 55 units, the slot sub_39050 widens from 464 to 573 units, and every text slot's
/// glyphs at 0.9 of the W / 32 size (sub_A7790); the pitch is English's 53 px line feed at that size plus 24.
pub const EN_LAYOUT: Layout = Layout {
    left: 32.0 * 3.0,
    right: (32.0 + 573.0) * 3.0,
    size: EN_SIZE,
    spacing: 4.0,
    line_height: 0.0,
    // the font's ascent applies at run time (`msg_layout`)
    ascent: ASCENT * 0.9,
};
const EN_SIZE: f32 = 60.0 * 0.9 * 0.76;
/// English message slot width in 640-wide units (sub_39050: 464 -> 573 for non-bubble windows)
const EN_WIDTH: f32 = 573.0;
/// F822 != 1 (LABEL_135): the message alone (no earlier history entries) from x 169.58 units, line 0's box
/// at 43.141 units (trainscene.mov: glyph tops y 116, first glyph x 562 after the indent).
const WINDOW_LEFT: f32 = 169.58;
/// The overseas build's message layout keeps the Japanese one for Japanese and Chinese; the docked game
/// (sub_A59D0: the 1920x1080 display mode) never takes its handheld branch (box 46, 0.85 glyphs, 508 wide).
pub fn msg_layout() -> Layout {
    let lay = if text::english() { EN_LAYOUT } else { LAYOUT };
    // the line spacing (a8 + 14) * H / 1080 = 24 px after the font set's line feed at the glyph size; the
    // baseline at the pair font's ascent
    Layout { line_height: text::line_feed() * lay.size / text::BASE_SIZE + 24.0, ascent: text::msg_ascent(ASCENT), ..lay }
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
/// Image slots 52.. are DISP_PARTS_JA in order.
const DISP_SLOT0: u16 = 52;
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
    /// ja3 FOT-Seurat: the message text while the window is a bubble (see `msg_font`)
    font_bubble: Option<FontAtlas>,
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
    auto_timer: f32,
    /// the message being shown was already read (selects the read text speed)
    line_read: bool,
    pub ended: bool,
    /// the script stopped with sub_802C0 result 2 (RDED of the replayed node while F024 is set): the
    /// system data is written before the gallery comes back (sub_48AC0 case 2 states 7 / 5)
    pub replay_done: bool,
    /// RDAS hit a completed node with "既読シーンのオートスキップ" on (sub_92580 -> off_AE06B0 = 1)
    pub auto_skip_hit: bool,
    /// a next / previous scene countdown runs (sub_40ED0 states 3 / 6 return before the script)
    pub hold: bool,
    /// off_AE05B8: a held rewind runs sub_814C0 instead of the script step (sub_40ED0), so the restored
    /// messages are not shown until it stops
    pub rewinding: bool,
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
    /// dword_AFFCA4: set by a story start (sub_40CD0) or a load (sub_82330), cleared when a message starts
    /// (sub_6E0D0 / sub_6E950); while set Y does not open the system menu (sub_40ED0)
    pub menu_lock: bool,
    /// SE commands waiting out their key 68 delay: (channel, ms left, command) (byte_146E640 + 1112 + 20 * ch)
    se_delayed: Vec<(i32, f32, Command)>,
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
    /// script / effect steps per frame (sub_3FBD0 runs its effect steps this many times): the touch
    /// gesture and mode panel skip and rewind speeds, 1 normally
    pub speed: u32,
    /// per-message rewind history (sub_814C0 walks an 800-entry page ring, which the backlog lists too)
    history: std::collections::VecDeque<RingEntry>,
    /// ms the current click wait has been up (a skip passes it after `SKIP_FRAMES` 60 Hz frames)
    click_ms: f32,
    /// ms since the script last ran under a skip (one dispatch per 60 Hz frame)
    dispatch_ms: f32,
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
    /// voice of the message being shown (mode panel voice replay)
    last_voice: Option<String>,
    /// VPLY's voice until the next message takes it (off_AE08B0)
    pending_voice: Option<String>,
    /// Word 0x7CA of the script block: an STCH / SCH2 changed the stage since the last FADS. sub_802C0 then
    /// presents the stage without a transition (sub_5D3A0(0, 0)) once script_dispatch returns at a wait.
    stage_dirty: bool,
    /// VPLY's voice waiting for the next message start (AE04C8)
    voice_to_play: Option<String>,
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
    /// images the queued previews need, read and decoded on worker threads so opening the save list does
    /// not stall on large stage images (cache keys in flight, and where they arrive)
    preview_decoding: std::collections::HashSet<String>,
    preview_decoded: (std::sync::mpsc::Sender<crate::assets::Decoded>, std::sync::mpsc::Receiver<crate::assets::Decoded>),
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
        let disp = assets.ui_textures(r, &assets.ui_atlas("DISP_PARTS")).unwrap_or_default();
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
            font_bubble: None,
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
            hold: false,
            rewinding: false,
            staff_task: false,
            task_modes: (false, false),
            task_lock: (false, false),
            archive: false,
            menu_lock: true,
            se_delayed: Vec::new(),
            seek_audio: None,
            mouse: (0.0, 0.0),
            time: 0.0,
            warned: HashSet::new(),
            backlog_pages: Vec::new(),
            click_ms: 0.0,
            dispatch_ms: 0.0,
            ring_serial: 0,
            page_ring: (None, None),
            hide_text: false,
            touch_hidden: false,
            system_menu_shown: false,
            speed: 1,
            history: Default::default(),
            save_point: None,
            stage_backup: None,
            scene_ring: Vec::new(),
            scene_msgs: 0,
            ring_hold: false,
            keep_bgm_at_end: false,
            msg_text: String::new(),
            node_seq: 0,
            seek_node: false,
            seek_ended: false,
            seek_defer: false,
            ring_restored: false,
            last_voice: None,
            pending_voice: None,
            stage_dirty: false,
            voice_to_play: None,
            msg_voice: None,
            speaker: 0,
            menu_blur: None,
            menu_rt: None,
            menu_half: None,
            freeze: None,
            freeze_rt: None,
            menu_captured: false,
            movie: None,
            msg_pc: 0,
            stuck: (0, 0.0),
            wnty: 0,
            disp: Vec::new(),
            relaying: false,
            stage_rt: r.create_target(CANVAS_W, CANVAS_H, "stage"),
            noise_rt: None,
            previews: Default::default(),
            preview_queue: Vec::new(),
            preview_rt: None,
            preview_decoding: Default::default(),
            preview_decoded: std::sync::mpsc::channel(),
            old_rt: r.create_target(CANVAS_W, CANVAS_H, "old frame"),
            blur_rt: r.create_target(CANVAS_W, CANVAS_H, "blur"),
            ghost: ghost::Ghost::default(),
            ghost_rt: None,
            group_rt: None,
            focus: focus::Focus::default(),
            focus_saved: 0,
            focus_tex: None,
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

    fn finish_click(&mut self) {
        self.click_ms = 0.0;
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

    /// `choice`: the choice reads the pointer (not while the mode panel is up: sub_83E50 returns while
    /// sub_4E170 reports it busy, AE06C0).
    pub fn mouse_move(&mut self, x: f32, y: f32, choice: bool) {
        self.mouse = (x, y);
        if choice {
            self.choice_hover_at(x, y);
        }
    }

    // ---- per-frame update ---------------------------------------------------------

    pub fn update(&mut self, r: &Renderer, dt: f32) {
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
        // the next-scene seek steps nothing: it runs whole in the dispatch below, its waits zeroed
        let fast = self.skip;
        // the speed already repeats the steps (sub_3FBD0); a skip at speed 1 (the debug skip) runs time 8x
        let step = if fast && self.speed <= 1 { dt * 8.0 } else { dt };
        let cut = fast && self.speed <= 1;

        // sub_65E50 (from the sound update) counts the queued SE commands down
        self.tick_se_delayed(step);
        // layer tweens and motions
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
        if let Some(t) = &mut self.trans {
            t.t += step;
            if t.t >= t.dur || cut {
                self.trans = None;
            }
        }
        // earlier messages fade towards READ_ALPHA (the trailing run of @k messages counts as current); sub_391A0
        // only counts while F1103 is 0 and the content fade (off_AE05B0) is at 1, so a CLO4 / message-start
        // fade holds them where they are
        let mut i = self.page.msgs.len();
        while i > 0 && (i == self.page.msgs.len() || self.page.msgs[i - 1].1) {
            i -= 1;
        }
        if self.var("F1103") != 0 || self.page.dim != 0.0 {
            i = 0;
        }
        // the current message's fade length follows the text speed of its read state (dword_572B64, set by
        // sub_6E0D0 / a rewind's layout); config byte 98 (fade out off) ends every fade at once
        let fade_frames = self.settings.text_ms[usize::from(self.line_read)] * 1.2;
        if let Some(m) = self.page.msgs.last_mut() {
            m.3 = fade_frames;
        }
        let instant = !self.settings.text_fade_out;
        for m in &mut self.page.msgs[..i] {
            m.2 = if instant || m.3 <= 0.0 { 1.0 } else { (m.2 + dt * 0.06 / m.3).min(1.0) };
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
            // under a skip at speed n the exe's waits and effects run n steps a frame (the wait budget
            // sub_802C0 gets is speed x frames); knife cuts them short only for the debug skip at speed 1
            Wait::Time { left, click } => {
                let left = left - step;
                self.wait = if left <= 0.0 || cut { Wait::None } else { Wait::Time { left, click } };
            }
            Wait::Transition => {
                if self.trans.is_none() {
                    self.wait = Wait::None;
                }
            }
            Wait::Layer(l, mask) => {
                if cut {
                    for layer in self.stage.layers.iter_mut().flatten() {
                        layer.finish();
                    }
                }
                if !self.stage.animating(l, mask) || cut {
                    self.wait = Wait::None;
                }
            }
            Wait::Voice => {
                if !self.sound.voice_playing() || fast {
                    self.wait = Wait::None;
                }
            }
            Wait::Message { voice } => {
                if self.page.revealed as usize >= self.page.order && (!voice || fast || !self.sound.voice_playing()) {
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
        self.set_var("F824", 65);

        // run the script. sub_802C0 dispatches once per 60 Hz frame, so under a skip each wait the script stops
        // at costs a frame (knife resolves the waits within the frame's speed steps); the next-scene seek
        // runs in one frame
        // overseas sub_40ED0: while off_3516E0 is set, sub_802C0 is not called; its last frame runs sub_7DDA0
        if std::mem::take(&mut self.seek_defer) {
            self.seek_finish();
            return;
        }
        if self.skip && !self.seek_node && self.wait == Wait::None && !self.ended {
            if self.dispatch_ms < 1000.0 / 60.0 - 1.0 {
                return;
            }
            self.dispatch_ms = 0.0;
        }
        let mut budget = 20000;
        while self.wait == Wait::None && !self.ended && !self.auto_skip_hit && !self.hold && !self.rewinding && budget > 0 {
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
            if self.seek_node {
                // a pass of the seek loop ends where script_dispatch returns
                if self.wait != Wait::None {
                    self.seek_pass_end(r);
                }
                self.seek_clear_wait();
            }
        }
        // the pass that ended the seek: script_dispatch went on to the next wait first
        if self.seek_ended && !self.seek_node {
            self.seek_pass_end(r);
        }
        // sub_802C0: a stage an STCH / SCH2 changed with no FADS after it is presented as the dispatch returns
        // (sub_5D3A0(0, 0): sub_2E1B0(0, 0) ends any transition)
        if std::mem::take(&mut self.stage_dirty) && !self.seek_node {
            self.stage.commit();
            self.trans = None;
            if self.wait == Wait::Transition {
                self.wait = Wait::None;
            }
        }
        if std::mem::take(&mut self.seek_ended) {
            if self.assets.build == knife_core::exe::Build::Overseas {
                self.seek_defer = true;
            } else {
                self.seek_finish();
            }
        }
        // sub_391A0 (the message layer's draw) clears the page (sub_53AB0(1)) in page mode while F823 == 0: the
        // scripts zero it right after a page's last click wait, so the text goes then, not at the next message
        if self.page_mode() && self.var("F823") == 0 && !self.page.texts.is_empty() {
            self.clear_page(true);
        }
        self.assets.trim_images(400);
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
    fn seek_finish(&mut self) {
        for l in self.stage.layers.iter_mut().flatten() {
            l.x.diff = 0.0;
            l.y.diff = 0.0;
            l.alpha.diff = 0.0;
        }
        self.stage.commit();
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
        self.sound.stop_se(-1, 0.0);
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
    /// for the current one).
    pub fn settled(&self) -> bool {
        (self.wait == Wait::Click || self.rewinding) && self.trans.is_none()
    }

    /// The held rewind stopped: the text shows again (sub_391A0 draws none while off_AE05B8 is set), and a
    /// restored choice shows the page under it at once.
    pub fn end_rewind(&mut self) {
        self.rewinding = false;
        if self.wait == Wait::Choice && !self.page.texts.is_empty() {
            self.page.visible = true;
        }
    }

    /// Back one page (sub_814C0).
    pub fn rewind_step(&mut self, r: &Renderer) -> bool {
        if !self.can_rewind() {
            return false;
        }
        // sub_814C0: back to the newest earlier entry of a page's first message (type 0, F823 <= 1 when it was
        // recorded) taken outside a story movie / the staff roll (the entries between are undone on the way),
        // or the ring's oldest entry when there is none
        let cur = self.history.len() - 1;
        let target = (0..cur)
            .rev()
            .find(|&i| {
                let h = &self.history[i];
                h.kind == 0 && !h.locked && entry_f823(&h.data) <= 1
            })
            .unwrap_or(0);
        self.rewind_to(r, target, Back::Rewind);
        true
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
        while how != Back::Point && self.scene_ring.last().is_some_and(|p| p.serial.is_some_and(|s| s > serial)) {
            self.scene_ring.pop();
            if how == Back::Rewind {
                self.scene_msgs = 0;
            }
        }
        self.history.truncate(i + 1);
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
            self.lay_out_rewound(&d);
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
        let start = self.history.iter().take(i + 1).rev().find(|h| h.key.2 == 1).map(|h| h.serial);
        self.page_ring = (start.filter(|&s| kept.is_none_or(|l| s > l)), Some(serial));
    }

    /// sub_5BAF0 case 0 with a2 != 0: the entry's message is laid out in full at once (sub_53CF0 on its script
    /// text) with no voice (AE04C8 / AE08B0 = -1; the replay voice AE0790 stays the entry's), and the script
    /// resumes past it (+16). Without `@n` it waits for a click (AE04B8 0x500, 0x140 for `@e` or a full
    /// page), which the next held frame's sub_814C0 clears again, so a rewind held past its step goes on into
    /// the next message once released; an `@n` message goes on at once and holds the next ring push
    /// (byte_146E644).
    fn lay_out_rewound(&mut self, d: &crate::save::SaveData) {
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
        self.page.revealed = self.page.order as f32;
        self.ring_hold = code == Some(b'n');
        self.pending_voice = None;
        self.voice_to_play = None;
        self.last_voice = d.voice.clone();
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
        self.sound.stop_se(-1, 0.0);
    }

    /// sub_5D7E0: the BGM word plays at once (a track already playing keeps going, sub_9C470) at its volume
    /// (sub_9C870), each channel with a looping SE restarts it at once, the others stop.
    fn resume_sound(&mut self, st: &SoundState) {
        match st.bgm {
            Some((id, vol)) => self.play_bgm(id, vol, 0.0),
            None => self.sound.stop_bgm(0.0),
        }
        self.sound.stop_se(-1, 0.0);
        for (&ch, (name, vol)) in &st.loops {
            if let Some(pcm) = self.assets.sound_effect(name) {
                self.sound.play_se(ch, name, pcm, *vol, 0.0, true);
            }
        }
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
                self.set_var("F1103", 0);
                let raw = c.args.first().map(String::as_str).unwrap_or("");
                self.note_read(&c.name);
                // sub_6E0D0: VPLY's voice plays unless skipping (a fast skip into unread text with
                // read-only skip has stopped by now, so it plays there too) and becomes the replay voice
                let voiced = match self.voice_to_play.take() {
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
                    for (base, v) in [(799, Self::TEXT_COLOUR >> 8), (810, Self::TEXT_COLOUR & 0xFF), (824, f31), (835, f32)] {
                        self.set_var(&format!("F{}", base + n), v);
                    }
                }
                let (text, wait) = message_end(&text);
                self.show_message(&message_end(raw).0, wait);
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
            "IFNX" | "IFSC" | "IFUR" => {}
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
            // END_ (0x7f408, result 1) / RTM_ (0x789c8, result 2; RTMN first sets byte_146E64D, RTM2 only sets
            // it): the script ends (sub_5F200 at 0x7f5d4 / 0x7f9f8)
            "RTM2" => self.keep_bgm_at_end = true,
            "END_" | "RTM_" | "RTMN" => {
                if name == "RTMN" {
                    self.keep_bgm_at_end = true;
                }
                tracing::info!("{name}: scene finished");
                self.script_end();
            }
            // 0x7f4ac stores max(1, ms * 3 / 50) whole 60 Hz frames (AE04A8), so even WTTM(0) waits a frame
            "WTTM" => {
                let frames = (c.int_or(0, 0) * 3 / 50).max(1);
                self.wait = Wait::Time {
                    left: frames as f32 * 50.0 / 3.0,
                    click: c.int_or(1, 0) == 0,
                };
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
                    crate::text_stream::message(&text, (self.scene_index as u64) << 32 | self.pc as u64);
                    if !wait {
                        self.wait = Wait::None;
                    }
                    self.update_message_entry();
                    self.ring_hold = code == Some(b'n');
                }
            }

            // choices
            "SELR" => {
                // 0x78a6c: the group's first option pushes a type 13 entry and adds 1 to C847 (when F823 >= 1)
                if self.sel_opts.is_empty() {
                    self.push_ring_event(13);
                    if self.var("F823") >= 1 {
                        let v = self.var("C847") + 1;
                        self.set_var("C847", v);
                    }
                }
                let value = c.int_or(0, self.sel_opts.len() as i32);
                // the option keeps the script's text; its `$` reference resolves where it is laid out
                let text = c.arg(1).unwrap_or("").to_string();
                let disabled = c.int_or(2, 0) == 1;
                self.cmd_selr(value, &text, disabled);
            }
            "SELD" | "SELA" | "SELS" => {
                // 0x7f6d8 / sub_92800 / sub_92930 end a next-scene seek with the sound resumed
                if self.seek_node {
                    self.end_seek(true);
                }
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
            // SACL(layer = 0, on = 0) (inline 0x7af80): stage byte +0x54 bit 0x40
            "SACL" => {
                let on = c.int_or(1, 0) != 0;
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.absolute = on;
                }
            }
            // SBAS(layer = 0, base = -1) (inline 0x7afec): stage word 0x93C / 2 + layer
            "SBAS" => {
                let b = c.int_or(1, -1);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.base = usize::try_from(b).ok().filter(|&b| b < crate::stage::LAYERS);
                }
            }
            // STCP(layer = 0, prio = 0) (sub_62A40): stage word 282 + layer
            "STCP" => {
                let p = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.prio = p;
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
                    let from = c.int(1).map_or(l.rotation.to, |v| v as f32 / 100.0);
                    let to = c.int_or(2, 0) as f32 / 100.0;
                    l.rotation = Tween::new(from, to, frames, ease);
                }
            }
            // STGS(layer = 0, from = last target, to = 0, ms, ease) (sub_637A0 -> sub_32820): alpha 0..255,
            // both clamped to at most 255.
            "STGS" => {
                let frames = c.int(3).map_or(0, |ms| 60 * ms / 1000);
                let ease = c.int_or(4, 0);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    let from = c.int(1).unwrap_or((l.alpha.to * 255.0).round() as i32).min(255);
                    let to = c.int_or(2, 0).min(255);
                    l.alpha = Tween::new(from as f32 / 255.0, to as f32 / 255.0, frames, ease);
                }
            }
            // STPV(layer, px, py, rot) (sub_639A0): set the pivot (stage anchor, render floats 25 / 26) in script
            // units, scaled by the image level and offset by a variant's centre; the quad offset stays, so the
            // image does not move, but zoom and rotation turn about the new point. Arg 3 sets the rotation.
            "STPV" => {
                let li = c.int_or(0, -1);
                let delta = self.stage.get(li).map(|l| l.name.clone()).map_or((0.0, 0.0), |n| self.variant_delta(&n));
                if let Some(l) = self.stage.get(li) {
                    let (centre, k) = match &l.source {
                        Source::Image(img) => (img.anchor(), img.level),
                        Source::Color(_) => ([0.0; 2], 1.0),
                    };
                    let mut piv = l.pivot.unwrap_or(centre);
                    if let Some(v) = c.int(1) {
                        piv[0] = v as f32 * 1.875 / 0.46875 * k * 0.33333 + delta.0;
                    }
                    if let Some(v) = c.int(2) {
                        piv[1] = v as f32 * 1.875 * 576.0 / 272.0 * k * 362.0 / 1080.0 + delta.1;
                    }
                    l.pivot = Some(piv);
                    if let Some(rot) = c.int(3) {
                        l.rotation = Tween::fixed(rot as f32 / 100.0);
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
                if self.stage.animating(l, mask) {
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
                    self.rebake(r, c.int_or(0, -1));
                }
            }
            // STBR(layer = 0, mode = 0, override) (sub_62490): a non-negative arg 2 replaces the mode
            "STBR" => {
                let mode = c.int(2).filter(|&v| v >= 0).map_or(c.int_or(1, 0), |v| v & 0xff);
                let (blend, _) = blend_mode(mode);
                if let Some(l) = self.stage.get(c.int_or(0, 0)) {
                    l.blend = blend;
                    l.stbr = mode;
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
            "FADS" => {
                let mode = c.int_or(0, 1);
                let dur = c.int_or(1, 0) as f32;
                let rule = c.int_or(2, 0);
                self.stage.commit();
                self.stage_dirty = false;
                // sub_60610 copies the committed stage into the backup
                self.stage_backup = Some(self.stage_bytes());
                if mode == 1 || dur <= 0.0 || self.skip {
                    self.trans = None;
                } else {
                    // sub_47DD0: only mode 2 draws the old frame through the rule (sub_A48C0); mode 0 is a
                    // crossfade (sub_A46F0) even with a rule loaded
                    let rule = (mode == 2 && rule > 0)
                        .then(|| self.assets.image(r, &format!("RULE_{rule:02}")))
                        .flatten()
                        .map(|i| i.tex.clone());
                    self.snapshot = true;
                    self.trans = Some(Transition { dur, t: 0.0, rule });
                    self.wait = Wait::Transition;
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
                    l.soxy = off;
                }
            }
            // FCLN (sub_92120): only arg 5 counts, the effect (the others repeat its table entry); stage byte
            // 6569 keeps it for loads. FCSP (sub_921B0): off, fading over arg 0 ms (60 * ms / 1000 frames).
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
                    // cmd_STCH_impl stores (rule - 1) | (b << 12); sub_36520 passes (word >> 12) & 7
                    l.rule_flip = (c.int_or(2, 0) & 7) as u8;
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
                    self.rebake(r, li);
                }
            }
            // STRR(layer, r) (sub_633D0): the clip rotation its children are composited with; -99999 stores
            // -32768, which follows the child's own rotation
            "STRR" => {
                let v = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.rot2 = if c.int(1).is_none() { 0 } else if v == -99999 { i16::MIN } else { wrap_rot2(v) };
                }
            }
            "SNX4" | "FANX" | "STQK" | "TRPY" => {}
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
            "SEPL" | "SEFD" | "SESP" | "SEVL" => self.cmd_se(c, false),
            // MPLY(n, key 17 volume = 100, key 1 fade-in ms, key 11 fade-out ms, key 69 start sample) (sub_649E0):
            // a track already playing is left alone; with a fade-in the old track fades out at 1666.7 / (key 11
            // or key 1) volume points a frame while the new one rises at 1666.7 / key 1 (so key 1 ms at full
            // volume); without one the old track stops at once
            "MPLY" => {
                let n = c.int_or(0, 0);
                let vol = c.keyed_int(17).unwrap_or(100);
                let fade_in = c.keyed_int(1).unwrap_or(0);
                let fade_out = c.keyed_int(11).unwrap_or(0);
                let start = c.keyed_int(69).unwrap_or(0).max(0) as usize;
                // only MPLY marks a BGM heard (sub_649E0, n <= 0x417); the title and movie BGM do not
                if (0..=0x417).contains(&n) {
                    set_bit(&mut self.heard, n as usize);
                }
                if let Some(sa) = &mut self.seek_audio {
                    sa.bgm = (n >= 0).then_some((n, vol as f32 / 100.0));
                } else if n < 0 {
                    self.sound.stop_bgm(0.0);
                } else if self.sound.bgm_state().map(|b| b.0) != Some(n) {
                    let vol = vol as f32 / 100.0;
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
                let ms = c.keyed_int(1).unwrap_or(0);
                if let Some(sa) = &mut self.seek_audio {
                    sa.bgm = None;
                } else {
                    self.sound.stop_bgm(ms.max(0) as f64 / 1000.0);
                }
            }
            // MVOL(key 1 ms, key 17 volume = 0) (sub_653B0): to the volume over max(ms, 1) ms
            "MVOL" => {
                let ms = c.keyed_int(1).unwrap_or(0).max(1);
                let v = c.keyed_int(17).unwrap_or(0) as f32 / 100.0;
                if let Some(sa) = &mut self.seek_audio {
                    if let Some(b) = &mut sa.bgm {
                        b.1 = v;
                    }
                } else {
                    self.sound.set_bgm_volume(v, ms as f64 / 1000.0);
                }
            }
            "VPLY" | "VPL2" => {
                if let Some(n) = c.arg(0) {
                    self.note_voice(n);
                    self.pending_voice = Some(n.to_string());
                    // VPLY leaves the voice for the next message start (AE04C8, played by sub_6E0D0); VPL2
                    // plays it at once, without making it the replay voice, and drops it
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
            // RDAS(n) (0x79b78 -> sub_92580): with the auto skip of read scenes on, a completed node n asks for the
            // next scene (sub_40ED0 top state 1), unless skipping / seeking (off_AE0568, byte_146E638); the UI
            // drops it in a gallery flowchart replay (sub_50F0)
            "RDAS" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n))
                    && self.settings.auto_skip_read
                    && !self.seek_node
                    && !self.skip
                    && get_bit(&self.endings, n as usize)
                {
                    self.auto_skip_hit = true;
                }
            }
            "NOP_" | "WTSV" => {}
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
        if let Some(pcm) = SYS_SOUNDS.get(n).and_then(|&id| self.assets.sound_effect_id(id)) {
            self.sound.play_sys(0, pcm, vol);
        }
    }

    /// sub_ACFA0(n): system sound n on the SE volume through the sub_CA0 curve.
    fn play_se_sound(&mut self, n: usize) {
        let vol = volume_curve(self.sound.se_volume());
        if let Some(pcm) = SYS_SOUNDS.get(n).and_then(|&id| self.assets.sound_effect_id(id)) {
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
            self.end_mode_task();
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
                let path = self.assets.romfs.join(&s.path);
                let base = s.volume;
                let loop_range = s.loop_range;
                self.sound.play_bgm(n, vol, fade, start, move || {
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
    ///
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
            | "FLNT" | "STMA" | "SPMA" | "SGMA" | "SQK2" | "SQS2" | "SHK2" | "SHS2" | "SSW2" | "SSS2" => true,
            // sub_3DE0 / sub_4030 / sub_4120 back up once their layer passes (STEC 0..=39, SNX2 / SNX3 >= 0)
            "STEC" => stch_ok,
            "SNX2" | "SNX3" => arg(0).is_some_and(|l| l >= 0),
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
            "STZ4" | "STGS" | "SQS2" | "SHS2" | "SSS2" => signed(0),
            // the quakes skip a timed call (arg 4 / 4 / 6 >= 0) only during the load label scan
            "SQK2" => signed(0),
            "SHK2" => arg(0).is_none_or(|l| (-1..=39).contains(&l)),
            "SSW2" => unsigned(0),
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
    /// SEPL / SEFD / SEVL (sub_6A340 / sub_6B310 / sub_6AE10). With a delay (key 68, ms, past the first
    /// argument) outside a seek (off_AE0568 / AE0900), sub_66200 queues the command on its channel (all 20
    /// for -1) for ceil(ms * 60 / 1000) frames (at least 1); an op already queued there runs at once first. sub_65E50 counts
    /// the frames down and sub_65A60 runs it.
    fn cmd_se(&mut self, c: &Command, delayed: bool) {
        let ch = c.int_or(0, if c.name == "SEPL" { 0 } else { -1 });
        // the next-scene seek only records the channel words
        if let Some(sa) = &mut self.seek_audio {
            match c.name.as_str() {
                "SEPL" if c.int_or(1, 0) != 0 => {
                    if let Some(n) = c.arg(2) {
                        sa.loops.insert(ch, (n.to_string(), c.keyed_int(17).unwrap_or(100) as f32 / 100.0));
                    }
                }
                "SEVL" => {
                    let v = c.keyed_int(17).unwrap_or(0) as f32 / 100.0;
                    for (k, l) in sa.loops.iter_mut() {
                        if ch < 0 || *k == ch {
                            l.1 = v;
                        }
                    }
                }
                _ if ch < 0 => sa.loops.clear(),
                _ => {
                    sa.loops.remove(&ch);
                }
            }
            return;
        }
        if !delayed
            && c.name != "SESP"
            && let Some(ms) = c.keyed_int(68)
        {
            let frames = if (-32..=0).contains(&ms) { 1 } else { (3 * ms + 49) / 50 };
            let chs: Vec<i32> = if ch < 0 { (0..20).collect() } else { vec![ch % 20] };
            for k in chs {
                if let Some(i) = self.se_delayed.iter().position(|d| d.0 == k) {
                    let (_, _, old) = self.se_delayed.remove(i);
                    self.cmd_se(&old, true);
                }
                let mut one = c.clone();
                if ch < 0 {
                    one.args[0] = k.to_string();
                }
                self.se_delayed.push((k, frames as f32 * 1000.0 / 60.0, one));
            }
            return;
        }
        let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
        match c.name.as_str() {
            // sub_6A340: a looping SE already looping on the channel is left alone; under the fast skip
            // (AE0670) a one-shot SE does not play and stops the channel's loop
            "SEPL" => {
                let looping = c.int_or(1, 0) != 0;
                if let Some(n) = c.arg(2) {
                    let vol = c.keyed_int(17).unwrap_or(100) as f32 / 100.0;
                    if looping && self.sound.looping_on(ch).is_some_and(|l| l.0 == n && l.1 > 0.0) {
                        return;
                    }
                    if self.skip && !looping {
                        if self.sound.looping_on(ch).is_some() {
                            self.sound.stop_se(ch, 0.0);
                        }
                        return;
                    }
                    if let Some(pcm) = self.assets.sound_effect(n) {
                        self.sound.play_se(ch, n, pcm, vol, fade, looping);
                    }
                }
            }
            "SEVL" => {
                if let Some(v) = c.keyed_int(17) {
                    self.sound.set_se_volume(ch, v as f32 / 100.0, fade);
                }
            }
            _ => self.sound.stop_se(ch, fade),
        }
    }

    fn tick_se_delayed(&mut self, ms: f32) {
        if self.se_delayed.is_empty() {
            return;
        }
        for d in &mut self.se_delayed {
            d.1 -= ms;
        }
        let due: Vec<Command> = self
            .se_delayed
            .extract_if(.., |d| d.1 <= 0.0)
            .map(|d| d.2)
            .collect();
        for c in due {
            self.cmd_se(&c, true);
        }
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
        let x = c.int(2).map(|v| v as f32).or(prev.as_ref().map(|p| p.x.value_x())).unwrap_or(240.0);
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
            l.rotation = Tween::fixed(p.rotation.value_rot());
            l.parent = p.parent;
            l.rot2 = p.rot2;
            l.clip_inset = p.clip_inset;
            l.soxy = p.soxy;
            // a source layer's quake slots come along; otherwise cmd_STCH_impl clears them
            if src_layer.is_some() {
                l.quake = p.quake;
            }
            // cmd_STCH_impl resets the stage anchor on every call: without a source layer to the centre cut to
            // whole units (the defaults, `Image::anchor`); with one (SCH2 arg 18) to the source's pivot moved by
            // the difference of the two centres, the shift word (int) minus that
            if src_layer.is_some()
                && let (Source::Image(si), Source::Image(img)) = (&p.source, &l.source)
            {
                let sp = p.pivot.unwrap_or(si.anchor());
                let moved = [sp[0] - si.center_x + img.center_x, sp[1] - si.center_y + img.center_y];
                l.pivot = Some(moved);
                l.shift = Some([-moved[0].trunc(), -moved[1].trunc()]);
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
                if !p.bake.is_none()
                    && let Source::Image(_) = l.source
                    && let Some(img) = self.assets.image_graded(r, &l.name, p.bake)
                {
                    l.source = Source::Image(img);
                    l.bake = p.bake;
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
                ..Default::default()
            });
        } else if let Some(p) = &prev
            && src_layer.is_some()
        {
            l.clip = p.clip;
        }
        // arg 16: scissor inset (stage word 1462), arg 17: its mode (byte 3004, 0 when absent)
        if let Some(v) = c.int(16) {
            l.clip_inset = v as i16;
        }
        l.clip_mode = c.int_or(17, 0) as i8;
        // arg 19: the STRR clip rotation (stage word 1522), wrapped to +-180 degrees
        if let Some(v) = c.int(19) {
            l.rot2 = wrap_rot2(v);
        }
        self.stage.set(li, l);
        // script_dispatch 0x7b274: a done STCH / SCH2 sets words 0x7C8 / 0x7CA
        self.stage_dirty = true;
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
    /// `replay`: it becomes the voice the replay button plays (AE06A8), as at a message start.
    fn play_voice(&mut self, n: &str, replay: bool) {
        if self.skip || self.seek_node || !self.settings.voice_on {
            return;
        }
        tracing::debug!("voice {n}");
        if replay {
            self.last_voice = Some(n.to_string());
        }
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

    /// `name` as the scene voice at voice row `row`'s volume.
    pub fn play_test_voice(&mut self, name: &str, row: usize) {
        let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
        match self.assets.voice(name) {
            Ok(pcm) => self.sound.play_voice_gain(Arc::new(pcm), gain),
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

    /// sub_2A9B0 with a voice test row held: a muted row stops the scene voice (sub_9C3B0: players 2, 3),
    /// and the scene voice takes the row's volume (sub_9D240).
    pub fn conf_test_row_volume(&mut self, row: usize) {
        if self.settings.char_mute >> row & 1 != 0 {
            self.sound.stop_voice();
        }
        let gain = self.settings.char_voice.get(row).copied().unwrap_or(1.0);
        self.sound.set_voice_gain(gain);
    }

    /// Config SE test (sub_29140): the last test SE stops (sub_9CF90), then one of the 14 test effects
    /// (off_46B138, sub_A8F90(0, 13)) plays once on the first idle SE channel 0..19 at the SE volume without a
    /// fade (sub_9CAA0). Returns the channel (qword_46F810), or None when every channel is busy.
    pub fn conf_test_se(&mut self, last: Option<i32>) -> Option<i32> {
        if let Some(ch) = last {
            self.sound.stop_se(ch, 0.0);
        }
        let ch = (0..20).find(|&c| !self.sound.se_busy(c))?;
        let name = TEST_SES[self.app_rng.range(0, 13) as usize];
        tracing::debug!("config SE test {name} on channel {ch}");
        let pcm = self.assets.sound_effect(name)?;
        self.sound.play_se(ch, name, pcm, 1.0, 0.0, false);
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
        let base = msg_layout();
        if !self.page_mode() {
            let right = (WINDOW_LEFT + self.msg_width()) * 3.0;
            return (Layout { left: WINDOW_LEFT * 3.0, right, ..base }, WINDOW_TOP_BASELINE - ASCENT + base.ascent);
        }
        match self.window_type() {
            Some(t) if self.is_bubble(t) => {
                let (left, top) = self.bubble_text_origin(t, self.window_offset());
                let right = left + self.bubble_width(t) * UX;
                // the bubble font's own pair ascent (`text::bubble_ascent`)
                let ascent = base.ascent * text::bubble_ascent();
                (Layout { left, right, ascent, ..base }, top + ascent)
            }
            _ => (base, top_baseline()),
        }
    }

    /// The window types of the build (unk_34D94C; the overseas build's flt_231D74 has 56).
    fn windows(&self) -> &'static [(u16, f32, f32)] {
        match self.assets.build {
            Build::Japanese => &WINDOWS,
            Build::Overseas => &EN_WINDOWS,
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

    /// Window types 3.. are the QA corner bubbles.
    fn is_bubble(&self, t: usize) -> bool {
        (3..self.windows().len()).contains(&t)
    }

    /// The message slot's width in 640-wide units (sub_39050: English 573).
    fn msg_width(&self) -> f32 {
        if text::english() { EN_WIDTH } else { 464.0 }
    }

    /// Text width of bubble type `t` in 640-wide units (flt_34DEFC; overseas sub_39050).
    fn bubble_width(&self, t: usize) -> f32 {
        match self.assets.build {
            Build::Japanese => BUBBLE_WIDTH[(t - 3) % 6 / 2],
            Build::Overseas if t - 3 < 40 && (EN_BUBBLE_WIDTH_MASK >> (t - 3)) & 1 != 0 => EN_BUBBLE_WIDTH[t - 3],
            Build::Overseas => self.msg_width(),
        }
    }

    /// Text origin of bubble type `t` in canvas pixels: unk_34D94C + unk_34DAD8 + the window offset.
    fn bubble_text_origin(&self, t: usize, (dx, dy): (f32, f32)) -> (f32, f32) {
        let (_, wx, wy) = self.windows()[t];
        let (tx, ty) = match self.assets.build {
            Build::Japanese => BUBBLE_TEXT[(t - 3) / 2],
            Build::Overseas => EN_BUBBLE_TEXT[t],
        };
        // 1280x720 units -> 640x448 -> canvas pixels
        ((wx + tx + dx) * 640.0 / 1280.0 * UX, (wy + ty + dy) * 448.0 / 720.0 * UY)
    }

    /// The second bubble (F1111 a bubble type, page mode): sub_391A0 draws it like the F1105 window with the
    /// F1112 / F1113 offsets.
    fn second_bubble(&self) -> Option<usize> {
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

    /// sub_AB0F0: the message font of the game language (the first file of its folder) and the QA bubble font
    /// (of `<folder>3`), each with the exfont runs (exfont_nx01.bffnt).
    pub fn load_fonts(assets: &Assets, r: &Renderer) -> anyhow::Result<(FontAtlas, Option<FontAtlas>)> {
        use anyhow::Context as _;
        let exfont = std::fs::read(assets.romfs.join("exfont_nx01.bffnt"));
        if let Err(e) = &exfont {
            tracing::warn!("exfont: {e}");
        }
        let with_exfont = |mut f: FontAtlas| {
            if let Ok(d) = &exfont {
                f.load_exfont(r, d).unwrap_or_else(|e| tracing::warn!("exfont: {e:#}"));
            }
            f
        };
        let path = assets.font_path("").with_context(|| format!("no font in {}", assets.language_folder()))?;
        let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut font = with_exfont(FontAtlas::new(r, data)?);
        // the rest of the font list (sub_CA500): `<folder>2`, `<folder>3`
        for suffix in ["2", "3"] {
            if let Some(d) = assets.font_path(suffix).and_then(|p| std::fs::read(p).ok()) {
                font.add_fallback(d).unwrap_or_else(|e| tracing::warn!("font {suffix}: {e:#}"));
            }
        }
        let bubble = match assets.font_path("3").map(|p| std::fs::read(&p).with_context(|| format!("reading {}", p.display()))) {
            Some(Ok(d)) => FontAtlas::new(r, d).map(with_exfont).map_err(|e| tracing::warn!("bubble font: {e:#}")).ok(),
            Some(Err(e)) => {
                tracing::warn!("bubble font: {e:#}");
                None
            }
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
        match Self::load_fonts(&self.assets, r) {
            Ok((font, bubble)) => {
                self.font = font;
                self.font_bubble = bubble;
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
        let (waiting, visible, msg_pc) = (self.page.waiting, self.page.visible, self.msg_pc);
        self.relaying = true;
        self.page.chars.clear();
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
    pub fn set_bubble_font(&mut self, font: FontAtlas) {
        self.font_bubble = Some(font);
    }

    /// The message text font. sub_391A0 sets text flag 0x800000 while the window type (F1105) is 3 or more (the
    /// QA bubbles); sub_A7790 turns it into style bit 0x40, for which the text parse (sub_BDC2C) wraps the text
    /// in nn::font tags 24 ... 21. sub_C445C files the chars after tag 24 under glyph class 2 and sub_C406C
    /// rasterises those with font variant 2 (classes 0 / 1 use variants 0 / 1). The exe names no font files;
    /// the Switch draws that variant in FOT-Seurat (ja3); variant 3 is the shared system font (`Ui::sys_font`).
    fn msg_font(&mut self) -> &mut FontAtlas {
        let bubble = self.window_type().is_some_and(|t| t >= 3);
        match &mut self.font_bubble {
            Some(f) if bubble => f,
            _ => &mut self.font,
        }
    }

    /// Message window type (stage word F1105; for English F1109 instead when it is above 0, sub_2DF20).
    fn window_type(&self) -> Option<usize> {
        let f1109 = self.var("F1109");
        usize::try_from(if text::english() && f1109 > 0 { f1109 } else { self.var("F1105") }).ok()
    }

    /// Window offset in 1280x720 units: flt_34D84C[F1106], flt_34D8B8[F1107] (0 out of range).
    fn window_offset(&self) -> (f32, f32) {
        let at = |t: &[f32], w: &str| usize::try_from(self.var(w)).ok().and_then(|i| t.get(i)).copied().unwrap_or(0.0);
        (at(&WINDOW_DX, "F1106"), at(&WINDOW_DY, "F1107"))
    }

    /// Lay out a message (`text` as the script has it: `$` references resolve in the game language, and the
    /// page keeps it unresolved, as the exe's history records do) at the page pen.
    fn show_message(&mut self, text: &str, wait: bool) {
        let shown = self.resolve_text(text);
        let runs = text::parse_markup(&shown);
        self.page.visible = true;
        // lay out on a scratch copy; restart the page if it overflows
        let (lay, top) = self.text_layout();
        // sub_391A0: in page mode message i of the page starts F(847 + i) lines further down (over 32 counts as
        // that minus 63, i.e. up), never above the first line; the scripts' WKAD(C847,1) after a message (and
        // the first SELR of a choice) leave a blank line before the next
        let extra = if self.page_mode() {
            match self.var(&format!("F{}", 847 + self.page.msgs.len())) {
                v if v > 32 => v - 63,
                v if v >= 1 => v,
                _ => 0,
            }
        } else {
            0
        };
        let start = if self.page.chars.is_empty() {
            (lay.left, top + extra.max(0) as f32 * lay.line_height)
        } else {
            let y = self.page.pen.1 + lay.line_height * (1 + extra) as f32 + PARAGRAPH_GAP;
            (lay.left, y.max(top))
        };
        let mut placed = Vec::new();
        let mut from = (start, self.page.order);
        let order0 = self.page.order;
        let (x, b, order) = text::layout(self.msg_font(), &lay, &runs, start.0, start.1, order0, &mut placed);
        self.msg_pc = self.pc.saturating_sub(1);
        if b > BOTTOM_LIMIT && !self.page.chars.is_empty() && !self.relaying {
            self.clear_page(true);
            placed.clear();
            from = ((lay.left, top), 0);
            let (x, b, order) = text::layout(self.msg_font(), &lay, &runs, lay.left, top, 0, &mut placed);
            self.page.pen = (x, b);
            self.page.order = order;
        } else {
            self.page.pen = (x, b);
            self.page.order = order;
        }
        self.page.last_start = from;
        let fade_frames = self.settings.text_ms[usize::from(self.line_read)] * 1.2;
        self.page.msgs.push((self.page.chars.len(), shown.trim_end().ends_with("@k"), 0.0, fade_frames));
        self.page.chars.extend(placed);
        let extra = self.var(&format!("F{}", 847 + self.page.texts.len()));
        self.page.extras.push(extra);
        self.page.texts.push(text.to_string());
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
        let (lay, _) = self.text_layout();
        let ((x0, y0), order0) = self.page.last_start;
        self.page.chars.truncate(first);
        let mut placed = Vec::new();
        let (x, b, order) = text::layout(self.msg_font(), &lay, &runs, x0, y0, order0, &mut placed);
        self.page.pen = (x, b);
        self.page.order = order;
        self.page.chars.extend(placed);
        if let Some(m) = self.page.msgs.last_mut() {
            m.1 = shown.trim_end().ends_with("@k");
        }
        *self.page.texts.last_mut().unwrap() = text;
        self.page.visible = true;
        if wait {
            self.page.waiting = true;
            self.wait = Wait::Click;
            self.auto_timer = 0.0;
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
        let mut placed = Vec::new();
        let (mut pen_y, mut order) = (None, 0);
        for (i, t) in texts.iter().enumerate() {
            let runs = text::parse_markup(&resolve_refs(&self.assets, t));
            // sub_28470: entry i starts a line pitch below the last one (the first at the top line), then
            // F(847 + i) of its record more lines down when 1..32, or (that - 64) lines when over 32, not above
            // the top line (a choice's first option: the blank line its first SELR asked for)
            let y = pen_y.map_or(top, |y: f32| y + lay.line_height + PARAGRAPH_GAP);
            let y = match extras.get(i).copied().unwrap_or(0) {
                v @ 1..=32 => y + v as f32 * lay.line_height,
                v if v > 32 => (y + (v - 64) as f32 * lay.line_height).max(top),
                _ => y,
            };
            let (_, b, o) = text::layout(font, &lay, &runs, lay.left, y, order, &mut placed);
            pen_y = Some(b);
            order = o;
        }
        let mut out = Vec::new();
        for pc in &placed {
            Self::rimmed_glyph(font, r, &mut out, pc, 1.0);
        }
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
        let tex = self.disp.get(usize::from(slot - DISP_SLOT0))?;
        let (rect, [u0, v0, u1, v1]) = bar.rects(pc.x, pc.size, n);
        Some(Quad::rect(tex.clone(), rect, color).with_uv_rect(u0, v0, u1, v1))
    }

    /// A message glyph: white with a dark rim all around.
    fn rimmed_glyph(font: &mut FontAtlas, r: &Renderer, out: &mut Vec<Quad>, pc: &text::PlacedChar, alpha: f32) {
        // the edge of font index 1 (`text::edge_rim`) scaled with the glyph, composited with the fill in the
        // shader: sub_391A0 fades a message by one alpha on the whole bordered glyph (Switch fire page: faded
        // line = B + 0.39 (full - B) per pixel), not by fading separately drawn rim and fill quads
        if let Some(q) = font.rimmed_quad(r, pc, text::edge_rim(1, pc.size), text::RIM_ALPHA, alpha) {
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
        // cover the screen at the page alpha * config word 0 (255); type 0 = txtwindow01 (alpha 102/255 in
        // the middle, falling off to the left and right edges). The bubbles draw at 0.78431 * the page alpha.
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
                && let Some(tex) = self.disp.get(usize::from(slot - DISP_SLOT0))
            {
                let (dx, dy) = self.window_offset();
                let alpha = if t > 2 { 0.78431 } else { 1.0 } * a * layer;
                let size = tex.texture.size();
                let rect = [(wx + dx) * 1.5 + ox, (wy + dy) * 1.5 + oy, size.width as f32 * sx, size.height as f32 * sy];
                out.push(Quad::rect(tex.clone(), rect, [1.0, 1.0, 1.0, alpha]));
            }
            // F1111 >= 3: a second bubble at its origin + the F1112 / F1113 offsets, also at 0.78431 * alpha
            if let Some(t) = self.second_bubble()
                && let Some((slot, sx, sy)) = self.window_image(t)
                && let Some(tex) = self.disp.get(usize::from(slot - DISP_SLOT0))
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
            self.window_quads(a, &mut out);
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
            let font = match &mut self.font_bubble {
                Some(f) if bubble_font => f,
                _ => &mut self.font,
            };
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
            for (ci, pc) in self.page.chars.iter().enumerate() {
                while m + 1 < msgs.len() && msgs[m + 1].0 <= ci {
                    m += 1;
                }
                let fade = fades.get(m).copied().unwrap_or(1.0);
                let (sx, sy) = shift.filter(|_| ci >= cur).unwrap_or((0.0, 0.0));
                let pc = &text::PlacedChar { x: pc.x + ox + sx, baseline: pc.baseline + oy + sy, ..*pc };
                // sub_C6A58: the bar shows (int) revealed - its first character's index cells
                if pc.bar.is_some() && !self.rewinding {
                    let count = (revealed.floor() - pc.order as f32).max(0.0) as usize;
                    bars.push((*pc, count, text_a * fade));
                }
                let mut vis = (revealed - pc.order as f32).clamp(0.0, 1.0);
                if !self.settings.text_fade_in && vis > 0.0 {
                    vis = 1.0;
                }
                // (sub_391A0 draws no text while a rewind runs, off_AE05B8)
                if vis <= 0.0 || self.rewinding {
                    continue;
                }
                let alpha = vis * text_a * fade;
                Self::rimmed_glyph(font, r, &mut out, pc, alpha);
            }
            // the message text is edged (+101003); the bar takes the slot alpha, not the glyph fade-in
            for (pc, count, alpha) in bars {
                out.extend(self.bar_quad(&pc, Some(count), [1.0, 1.0, 1.0, alpha], true));
            }
            // (sub_38EC0 draws nothing while a rewind runs)
            // (the overseas sub_38EC0 also draws nothing in auto mode, word_1916DEC bit 0)
            let auto_hidden = self.assets.build == Build::Overseas && self.auto;
            if self.wait == Wait::Click && revealed as usize >= self.page.order && !self.rewinding && !auto_hidden {
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
                // character spacing after the last glyph (TextWriter adds it before each glyph but the first),
                // truncated to whole 640x448 units.
                let (lay, _) = self.text_layout();
                let (px, py) = self.page.pen;
                let px = if px > lay.left { px - lay.spacing } else { px };
                // The overseas text slot (sub_C5FB4, English text: slot +101009) stores the writer's cursor
                // plus half the glyph cell (slot +776, the 41.04 px em) as the cursor
                let px = if self.assets.build == Build::Overseas && text::english() { px + lay.size * 0.5 } else { px };
                let (ux, uy) = ((px / UX).trunc(), ((py - lay.ascent) / UY).trunc());
                let (sx, sy) = shift.unwrap_or((0.0, 0.0));
                let (x, y) = ((ux + dx) * UX + sx, (uy + dy) * UY + sy);
                if let Some(t) = self.disp.get(slot - usize::from(DISP_SLOT0)) {
                    let frame = ((self.time / 33.3) as u32 % frames) as f32;
                    let tw = t.texture.size().width as f32;
                    let th = t.texture.size().height as f32;
                    let (w, h) = (fw * ICON_SCALE.0, fh * ICON_SCALE.1);
                    out.push(
                        Quad::rect(t.clone(), [x, y, w, h], [1.0, 1.0, 1.0, alpha * a * content * layer])
                            .with_uv_rect(frame * fw / tw, 0.0, (frame + 1.0) * fw / tw, fh / th),
                    );
                }
            }
        }
        if a <= 0.0 {
            self.choice_quads(r, &mut out, a);
        }
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

    /// The layers into the stage target, through the layer group surfaces where stage::Draw says so.
    fn render_stage(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) {
        use crate::stage::Draw;
        let draws = self.stage.draws(&r.white);
        let mut run: Vec<Quad> = Vec::new();
        let mut clear = Some([0.0, 0.0, 0.0, 1.0]);
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
            let t = ["group pixel", "group mask", "group composite"].map(|n| r.create_target(CANVAS_W, CANVAS_H, n));
            for rt in &t {
                r.draw(enc, rt, Some([0.0; 4]), &[]);
            }
            self.group_rt = Some(t);
        }
        self.group_rt.as_ref().unwrap()
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
        self.render_stage(r, &mut enc);
        match &self.trans {
            Some(t) => {
                let p = (t.t / t.dur).clamp(0.0, 1.0);
                let rule = t.rule.clone();
                r.transition(&mut enc, &self.canvas, &self.old_rt.tex, &self.stage_rt.tex, rule.as_deref(), p);
            }
            None => r.copy_target(&mut enc, &self.stage_rt, &self.canvas),
        }
        // sub_3C490 draws the FADE(40) effect over the composed frame, before the text
        if self.ghost.active() {
            let [src, out] = self.ghost_rt.get_or_insert_with(|| {
                [r.create_target(CANVAS_W, CANVAS_H, "ghost frame"), r.create_target(CANVAS_W, CANVAS_H, "ghost channels")]
            });
            let trans = self.trans.as_ref().map(|t| (t.t / t.dur).clamp(0.0, 1.0));
            self.ghost.render(r, &mut enc, &self.canvas, [src, &self.blur_rt, out], trans);
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
                r.draw(&mut enc, &self.canvas, None, &quads);
            }
        }
        if self.menu_blur.is_some() && !self.menu_captured {
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
                let rt = self.menu_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "menu blur"));
                if !self.menu_captured {
                    if self.assets.build == knife_core::exe::Build::Overseas {
                        // sub_9EC50 (its sigma argument unused): the capture scaled to the half-size "filter
                        // Defocus" buffer, then sub_9CCD0(1.25)'s meshes, 2 ceil(3.75) + 1 = 9 quads offset
                        // sigma sqrt(2 pi) k / 1920 (/ 1080) in UV, each at its weight, horizontally and then
                        // vertically (shader 55, through the backbuffer and copied back), then scaled up 2x
                        let (hw, hh) = (CANVAS_W / 2, CANVAS_H / 2);
                        let [a, b] = self.menu_half.get_or_insert_with(|| {
                            [r.create_target(hw, hh, "filter defocus"), r.create_target(hw, hh, "defocus pass")]
                        });
                        let full = Quad::rect(self.canvas.tex.clone(), [0.0, 0.0, hw as f32, hh as f32], [1.0; 4]);
                        r.draw(&mut enc, a, Some([0.0, 0.0, 0.0, 1.0]), &[full]);
                        let step = 1.25 * (2.0 * std::f32::consts::PI).sqrt() * hw as f32 / 1920.0;
                        r.blur(&mut enc, &a.tex, b, [step, 0.0], 1.25);
                        r.blur(&mut enc, &b.tex, a, [0.0, step], 1.25);
                        let up = Quad::rect(a.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]);
                        r.draw(&mut enc, rt, Some([0.0, 0.0, 0.0, 1.0]), &[up]);
                    } else {
                        // sub_A0A90(.., 2.25): sigma 2.25 taps spaced 1.75 px (of 1920)
                        let step = 1.75 * CANVAS_W as f32 / 1920.0;
                        r.blur(&mut enc, &self.canvas.tex, &self.blur_rt, [step, 0.0], 2.25);
                        r.blur(&mut enc, &self.blur_rt.tex, rt, [0.0, step], 2.25);
                    }
                    self.menu_captured = true;
                }
                let q = Quad::rect(rt.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0, 1.0, 1.0, a.min(1.0)]);
                r.draw(&mut enc, &self.canvas, None, &[q]);
            }
            None => self.menu_captured = false,
        }
        let mut ui = Vec::new();
        if !self.movie_by_ui {
            ui.extend(self.movie_quad());
        }
        ui.extend(self.story_movie.as_ref().and_then(|t| t.cover_quad(r)));
        ui.extend(self.ui_quads(r));
        overlay(self, r, &mut ui);
        if !ui.is_empty() {
            r.draw(&mut enc, &self.canvas, None, &ui);
        }
        if let Some(f) = &mut self.freeze {
            let rt = self.freeze_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "screen capture"));
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
        let choice_ok = self.choice.as_ref().is_some_and(|c| c.kind == ChoiceKind::Page);
        (self.wait == Wait::Click || (self.wait == Wait::Choice && choice_ok)) && !self.ended
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
                    rotation: l.rotation.to,
                    prio: l.prio,
                    parent: l.parent,
                    base: l.base,
                    absolute: l.absolute,
                    quake: l.quake.map(|q| (q.mode, q.delay, q.remaining, q.params)),
                    rule: l.rule.as_ref().map(|(n, _)| *n),
                    rule_flip: l.rule_flip,
                    noise: l.noise,
                    // the restore takes the blend from `stbr` when one is set (backdrop blends always have one)
                    blend: match l.blend {
                        Blend::Alpha | Blend::Ps(_) | Blend::Footprint | Blend::MaskAlpha => 0,
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
                    stbr: Some(l.stbr),
                    rot2: l.rot2,
                    pivot: l.pivot,
                    tint: (l.stec_mode != 1 && l.stec_color != [1.0; 3]).then_some(l.stec_color),
                    soxy: l.soxy,
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
            .map(|t| text::note_text(&resolve_refs(&self.assets, t)))
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
            loops: self
                .sound
                .looping_effects()
                .into_iter()
                .map(|(channel, name, volume)| SeSave { channel, name, volume })
                .collect(),
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
        // (sub_2DFD0); the pivot also takes the variant's centre offset, the quad offset is minus the pivot
        if f.pivot.iter().any(Option::is_some) {
            let (k, (dx, dy)) = pivot_info.unwrap_or((1.0, (0.0, 0.0)));
            let centre = match &l.source {
                Source::Image(img) => img.anchor(),
                Source::Color(_) => [0.0; 2],
            };
            let mut piv = l.pivot.unwrap_or(centre);
            let mut sh = l.shift.unwrap_or([-centre[0], -centre[1]]);
            if let Some(v) = f.pivot[0] {
                let p = v * 1.875 / 0.46875 * k * 0.33333;
                piv[0] = dx + p;
                sh[0] = -p;
            }
            if let Some(v) = f.pivot[1] {
                let p = v * 1.875 * 576.0 / 272.0 * k * 362.0 / 1080.0;
                piv[1] = dy + p;
                sh[1] = -p;
            }
            l.pivot = Some(piv);
            l.shift = Some(sh);
        }
        if let Some(rot) = f.rot {
            l.rotation = Tween::fixed(rot);
        }
        if let Some(v) = f.rot2 {
            l.rot2 = v as i16;
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
        // field 27 moves the layer only while its priority is not 0: STCP(layer, 0) hides it from a running motion
        if let Some(p) = f.prio
            && p != 0
            && l.prio != 0
        {
            l.prio = p;
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

    fn rebake(&mut self, r: &Renderer, li: i32) {
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
            if let Source::Image(_) = l.source
                && let Some(img) = self.assets.image_graded(r, &l.name, bake)
            {
                l.source = Source::Image(img);
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
        l.rule = ls
            .rule
            .and_then(|n| self.assets.image(r, &format!("RULE_{n:02}")).map(|i| (n, i.tex.clone())));
        l.rule_flip = ls.rule_flip;
        l.quake = ls.quake.map(|(mode, delay, remaining, params)| crate::stage::Quake::restore(mode, delay, remaining, params));
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
        l.pivot = ls.pivot;
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
        while let Ok(d) = self.preview_decoded.1.try_recv() {
            self.preview_decoding.remove(d.key());
            self.assets.finish_decoded(r, d);
        }
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
                if self.preview_decoding.insert(crate::assets::Assets::image_key(name, bake).2) {
                    self.assets.decode_async(name, bake, self.preview_decoded.0.clone());
                }
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
        let rt = self.preview_rt.get_or_insert_with(|| r.create_target(tw, th, "preview")).tex.clone();
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
            let full = RenderTarget { tex: rt.clone() };
            r.draw(enc, &full, Some([0.0, 0.0, 0.0, 1.0]), &quads);
            // sub_4F240 draws the target bilinearly into the 248x140 slot picture (82.7 x 57.9 of 640x448)
            let (pw, ph) = PREVIEW_SIZE;
            let out = r.create_target(pw, ph, "preview");
            r.draw(enc, &out, Some([0.0, 0.0, 0.0, 1.0]), &[Quad::rect(rt.clone(), [0.0, 0.0, pw as f32, ph as f32], [1.0; 4])]);
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
        self.scene_ring.clear();
        if let Some(p) = d.prev.as_deref() {
            let mut p = p.clone();
            p.text.clear();
            self.scene_ring.push(ScenePoint { data: p, serial: None });
        }
        self.scene_ring.push(ScenePoint { data: crate::save::SaveData { prev: None, ..d.clone() }, serial: None });
        self.restore_with(r, d, false);
        self.restore_save_point(d, false, d.kind);
        self.scene_ring.pop();
        self.scene_msgs = 0;
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
            self.sound.stop_se(-1, 0.0);
            self.sound.stop_voice();
            self.sound.stop_movie_audio();
        } else {
            self.sound.stop_all();
        }
        self.choice = None;
        self.sel_opts.clear();
        self.trans = None;
        // sub_2EA70(-1) / sub_3C550: the screen shake and the message quake stop
        self.shake.stop();
        self.msg_quake.stop();
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
        if let Some((var, opts)) = &d.choice {
            let opts = opts.iter().map(|(v, t)| ChoiceOpt { value: *v, text: t.clone(), disabled: false }).collect();
            self.start_choice(Choice::new(var.clone(), ChoiceKind::Page, opts));
        }
        self.snapshot = false;
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
        // sub_57240 clears the queued SE commands
        self.se_delayed.clear();
        self.history.clear();
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
        // sub_57450 resets every layer field but the anchor (sub_3D70 starts past it)
        let leftovers = self.reset_leftovers();
        self.stage = Stage::new();
        self.stage.kept = leftovers;
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
        self.sel_opts.clear();
        self.trans = None;
        // sub_2EA70(-1) / sub_3C550: the screen shake and the message quake stop
        self.shake.stop();
        self.msg_quake.stop();
        self.wait = Wait::Halt;
        self.ended = false;
        self.replay_done = false;
        self.auto_skip_hit = false;
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
                        + &l.noise.map_or(String::new(), |n| format!(" noise {n}{}", if self.stage.noise.is_some() { "" } else { " (no tex)" }))
                );
            }
        }
        s
    }

    /// RGBA8 copy of the last presented canvas as the screen shows it (through the output curve).
    pub fn screenshot(&self, r: &Renderer) -> Vec<u8> {
        let lut: Vec<u8> = (0..=255).map(knife_render::output_level).collect();
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
