//! Save data types: in-memory game snapshots (saves, rewind history) and the save directory.
//!
//! Saves and system data live in the Switch format (`savedata.rs`, `savedata.bin` in the save directory), and
//! nothing else is kept. Save list previews are rendered from each record's stage block, as on the Switch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Slots 0..140 are the exe's 140 save records: 0..100 the save list (sub_50790), 100..139 the auto save ring
/// (sub_52AA0, newest at 100, reached through CONTINUE only), 139 the quick save (sub_52860).
pub use crate::savedata::{AUTO_FIRST, LIST_SLOTS, QUICK_SLOT, SLOTS};

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
    /// SSW2: amp min/max, period min/max, strength, remaining ms
    #[serde(default)]
    pub sway: Option<[f32; 6]>,
    /// SQK2: amp x / y, remaining ms (negative = forever), elapsed ms, then the SQK2 arguments 3..7
    #[serde(default)]
    pub quake: Option<([f32; 4], [i32; 5])>,
    #[serde(default)]
    pub rule: Option<i32>,
    #[serde(default)]
    pub noise: Option<i32>,
    pub blend: u8,
    pub shade: u8,
    pub fill: Option<[f32; 3]>,
    /// STEC colour grade (0 none)
    #[serde(default)]
    pub grade: i32,
    pub motion: Option<(i32, f32)>,
    #[serde(default)]
    pub clip: Option<[f32; 5]>,
    #[serde(default)]
    pub bright: Option<f32>,
    /// STBR mode as given (-1 none)
    #[serde(default)]
    pub stbr: Option<i32>,
    /// STRR rotation, degrees
    #[serde(default)]
    pub motion_rot: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SeSave {
    pub channel: i32,
    pub name: String,
    pub volume: f32,
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
    pub loops: Vec<SeSave>,
    /// unix seconds
    pub timestamp: u64,
    /// text shown in the slot list
    pub preview: String,
    /// FADE 40 full-screen blur active
    #[serde(default)]
    pub blur: bool,
    /// per empty layer: what STCC left of it (see `Stage::kept`)
    #[serde(default)]
    pub kept: Vec<Option<Kept>>,
    /// the stage backup block (record +6664, see `Game::stage_backup`)
    #[serde(default)]
    pub backup: Option<Vec<u8>>,
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
}

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("KNIFE_SAVE_DIR") {
        return PathBuf::from(d);
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("knife").join("saves")
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
