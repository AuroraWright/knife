//! Save data types: in-memory game snapshots (saves, rewind history) and the save directory.
//!
//! Saves and system data live in the Switch format (`savedata.rs`, `savedata.bin`, or the `--save-file`), and
//! nothing else is kept. Save list previews are rendered from each record's stage block, as on the Switch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Slots 0..140 are the exe's 140 save records: 0..100 the save list (sub_50790), 100..139 the auto save ring
/// (sub_52AA0, newest at 100, reached through CONTINUE only), 139 the quick save (sub_52860).
/// The build's record slots (`savedata::Layout`): all of them, the save list, the quick save, the (first) auto
/// save.
pub fn slots() -> usize {
    crate::savedata::lay().slots
}
pub fn list_slots() -> usize {
    crate::savedata::lay().list_slots
}
pub fn quick_slot() -> usize {
    crate::savedata::lay().quick_slot
}
pub fn auto_first() -> usize {
    crate::savedata::lay().auto_first
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LayerSave {
    pub image: Option<String>,
    pub color: Option<[f32; 3]>,
    pub x: f32,
    pub y: f32,
    pub zoom_x: f32,
    pub zoom_y: f32,
    pub alpha: f32,
    pub rotation: f32,
    pub prio: i32,
    pub parent: Option<usize>,
    #[serde(default)]
    pub base: Option<usize>,
    #[serde(default)]
    pub absolute: bool,
    /// quake slots 0 / 1 as the stage block keeps them: mode, delay frames, remaining frames, parameters
    #[serde(default)]
    pub quake: [(u8, i16, i16, [i16; 6]); 2],
    #[serde(default)]
    pub rule: Option<i32>,
    /// STTR b
    #[serde(default)]
    pub rule_flip: u8,
    #[serde(default)]
    pub noise: Option<i32>,
    pub blend: u8,
    pub shade: u8,
    pub fill: Option<[f32; 3]>,
    /// STEC colour grade (0 none)
    #[serde(default)]
    pub grade: i32,
    pub motion: Option<(i32, f32)>,
    /// clip window x, y, w, h (px) and the scissor inset (STCH arg 16)
    #[serde(default)]
    pub clip: Option<[f32; 5]>,
    /// STCH arg 17: the inset's mode
    #[serde(default)]
    pub clip_mode: i8,
    /// SNX2 brightness (1 + v / 255)
    #[serde(default)]
    pub bright: Option<f32>,
    /// SNX3 contrast (1 + v / 100)
    #[serde(default)]
    pub contrast: Option<f32>,
    /// STBR mode as given (-1 none)
    #[serde(default)]
    pub stbr: Option<i32>,
    /// STRR clip rotation (stage word 1522: 1/100 degree, -32768 = the child's own rotation)
    #[serde(default)]
    pub rot2: i16,
    /// STPV / motion pivot (stage anchor, 640x362 units) when not the image centre
    #[serde(default)]
    pub pivot: Option<[f32; 2]>,
    /// stage shift word (2923 / 2924, whole units) when not minus the image centre
    #[serde(default)]
    pub shift: Option<[f32; 2]>,
    /// render colour a motion set on a layer that is not a fill (stage colour at +5850)
    #[serde(default)]
    pub tint: Option<[f32; 3]>,
    /// SOXY draw offset (stage words 1222 / 1262)
    #[serde(default)]
    pub soxy: (i16, i16),
    /// Witch on the Holy Night's SNX4 values (`Layer::snx4`)
    #[serde(default)]
    pub snx4: [f32; 2],
    /// Witch on the Holy Night's STGS words: start and current alpha, frames, curve (`Layer::wfade`)
    #[serde(default)]
    pub wfade: Option<[i16; 4]>,
}

impl LayerSave {
    /// What a load builds the layer texture with (the STEC grade and SNX tone the record keeps).
    pub fn bake(&self) -> crate::grade::Bake {
        crate::grade::Bake::new(self.grade, self.bright.unwrap_or(1.0), self.contrast.unwrap_or(1.0))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SeSave {
    pub channel: i32,
    pub name: String,
    pub volume: f32,
}

/// A channel's queued key-68 SE command (sub_66200: byte_146E640 + 1112 + 20 ch; saved at entry +14612).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeQueued {
    pub channel: i32,
    /// frames left on the exe's counter (+1112), before this frame's count-down
    pub frames: i16,
    /// +1120: 1 SEPL, 2 SEFD, 3 SEVL, 4 SEAC
    pub kind: u8,
    /// +1114: SEPL's sound (sub_900; none when not in the table)
    pub name: Option<String>,
    /// +1116: key 1, ms (-1 none)
    pub fade: i32,
    /// +1121: key 17 (-1 none)
    pub volume: i8,
    /// +1123: SEPL's loop argument
    pub looping: bool,
    /// +1124: key 18 (-1 none)
    pub pan: i16,
    /// +1128: key 70 (0 none)
    pub speaker: i32,
    /// +1122: SEAC's end volume (-1 none)
    #[serde(default)]
    pub vol_end: i8,
    /// +1126: SEAC's end pan, 0..128 (-1 none)
    #[serde(default)]
    pub pan_end: i16,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveData {
    pub version: u32,
    pub scene: usize,
    pub scene_name: String,
    /// command index to resume at (the message being displayed)
    pub pc: usize,
    pub vars: Vec<(String, i32)>,
    pub layers: Vec<Option<LayerSave>>,
    pub page_texts: Vec<String>,
    pub choice: Option<(String, Vec<(i32, String)>)>,
    pub bgm: Option<(i32, f32)>,
    /// the BGM volume word (see `Sound::bgm_word`); None in old saves
    #[serde(default)]
    pub bgm_word: Option<i16>,
    pub loops: Vec<SeSave>,
    /// unix seconds
    pub timestamp: u64,
    /// text shown in the slot list
    pub preview: String,
    /// per empty layer: what STCC left of it (see `Stage::kept`)
    #[serde(default)]
    pub kept: Vec<Option<Kept>>,
    /// the stage backup block (record +6664, see `Game::stage_backup`)
    #[serde(default)]
    pub backup: Option<Vec<u8>>,
    /// the FCLN effect (stage byte 6569; 0 = none)
    #[serde(default)]
    pub focus: u8,
    /// the rewind-ring entry before this one, kept by a save (record count / previous entry)
    #[serde(default)]
    pub prev: Option<Box<SaveData>>,
    /// page-ring entry type when not a message / choice (6: ATSV)
    #[serde(default)]
    pub kind: u16,
    /// the message's voice (entry +13752, played again on load / rewind) and speaker (+13756 / +30)
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub speaker: i16,
    /// the current `_Z` read key (entry +26 / +28: the message's own, or the last marker's)
    #[serde(default)]
    pub read_key: Option<(usize, u8)>,
    /// the entry's raw text (+13956, byte_146EC28): a message's text up to its end code with MSAD's additions,
    /// ATSV(1)'s copy of the last message's, empty otherwise
    #[serde(default)]
    pub text: String,
    /// each SE channel's volume word (byte_146E640 + 140, saved as a byte at +14468); empty in old saves
    #[serde(default)]
    pub se_words: Vec<i16>,
    /// each SE channel's loop flag (byte_146E640 + 180; +14489); empty in old saves
    #[serde(default)]
    pub se_loops: Vec<bool>,
    /// each SE channel's pan word (byte_146E640 + 220, x 1000; +14510)
    #[serde(default)]
    pub se_pans: Vec<i16>,
    /// the queued key-68 SE commands
    #[serde(default)]
    pub se_queue: Vec<SeQueued>,
}

/// What an empty layer keeps in the stage block: the negated anchor at +5844+2 / +4 of the last image shown
/// there. STCC (sub_62140, `stcc`) also keeps the STBR blend byte and sets alpha 255; a stage reset (sub_57450:
/// NEW GAME, flowchart jump) clears everything else, alpha 0 and blend -1.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Kept {
    pub neg_anchor: (i16, i16),
    pub blend: u8,
    #[serde(default)]
    pub stcc: bool,
    /// the SACL bit (stage byte +0x54 bit 0x40): STCC keeps it, STCH and a reset clear it, and SACL sets it on
    /// an empty slot too
    #[serde(default)]
    pub absolute: bool,
}

static SAVE_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Use this save file (`--save-file`) instead of the default; call before the first save access.
pub fn set_file(path: PathBuf) {
    let _ = SAVE_FILE.set(path);
}

/// The default save file of a build: `knife/savedata000` (the overseas data: `knife/overseas/savedata000`, a
/// separate title on the Switch; Witch on the Holy Night: `knife/witch/savedata000`) in the OS application data
/// directory (the working directory when the OS has none), as shin does.
pub fn default_file(build: knife_core::exe::Build) -> PathBuf {
    let dir = crate::platform::data_dir().unwrap_or_default().join("knife");
    match build {
        knife_core::exe::Build::Japanese => dir.join("savedata000"),
        knife_core::exe::Build::Overseas => dir.join("overseas").join("savedata000"),
        knife_core::exe::Build::Witch => dir.join("witch").join("savedata000"),
    }
}

/// The save file: `--save-file`, else `default_file`.
pub fn file() -> PathBuf {
    SAVE_FILE.get_or_init(|| default_file(knife_core::exe::Build::Japanese)).clone()
}

/// The local wall-clock time as seconds since 1970-01-01 00:00 of the same clock: save records store the date
/// fields of the console's local time (sdk time zone conversion before sub_5A910), so timestamps are kept in
/// local time throughout.
pub fn now() -> u64 {
    let t = chrono::Local::now();
    (t.timestamp() + i64::from(t.offset().local_minus_utc())).max(0) as u64
}

/// `YYYY/MM/DD HH:MM` of a local timestamp (`now`).
pub fn format_time(ts: u64) -> String {
    let days = (ts / 86400) as i64;
    let secs = ts % 86400;
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}/{m:02}/{d:02} {:02}:{:02}", secs / 3600, secs / 60 % 60)
}
