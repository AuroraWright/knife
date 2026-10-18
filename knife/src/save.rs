//! Save data: per-slot game snapshots and persistent global data (G flags, settings).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SLOTS: usize = 100;
pub const QUICK_SLOT: usize = 0;

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
    #[serde(default)]
    pub rule: Option<i32>,
    #[serde(default)]
    pub noise: Option<i32>,
    pub blend: u8,
    pub shade: u8,
    pub fill: Option<[f32; 3]>,
    pub motion: Option<(i32, f32)>,
    #[serde(default)]
    pub clip: Option<[f32; 5]>,
    #[serde(default)]
    pub bright: Option<f32>,
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
}

/// Flowchart node: where it starts and the (non-global) flags when it was last reached.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeRecord {
    pub scene: usize,
    pub pc: usize,
    pub vars: Vec<(String, i32)>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct GlobalData {
    pub vars: Vec<(String, i32)>,
    pub text_speed: Option<f32>,
    pub auto_delay_ms: Option<f32>,
    pub master: Option<f32>,
    pub bgm: Option<f32>,
    pub se: Option<f32>,
    pub voice: Option<f32>,
    /// script_text lines already read (for skip-read-only), as a bitmap
    #[serde(default)]
    pub read: Vec<u64>,
    /// flowchart nodes reached (RDST) and endings seen (RDED), as bitmaps
    #[serde(default)]
    pub nodes: Vec<u64>,
    #[serde(default)]
    pub endings: Vec<u64>,
    #[serde(default)]
    pub skip_unread: Option<bool>,
    #[serde(default)]
    pub node_states: std::collections::BTreeMap<u16, NodeRecord>,
    /// images shown at least once (CG gallery)
    #[serde(default)]
    pub seen: Vec<String>,
    #[serde(default)]
    pub heard: Vec<u64>,
    #[serde(default)]
    pub movie_flags: u16,
}

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("KNIFE_SAVE_DIR") {
        return PathBuf::from(d);
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("knife").join("saves")
}

fn slot_path(slot: usize) -> PathBuf {
    dir().join(format!("slot{slot:03}.json"))
}

pub fn thumb_path(slot: usize) -> PathBuf {
    dir().join(format!("slot{slot:03}.png"))
}

pub fn write_slot(slot: usize, data: &SaveData, thumb: Option<(&[u8], u32, u32)>) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    let json = serde_json::to_vec_pretty(data)?;
    let tmp = slot_path(slot).with_extension("tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, slot_path(slot))?;
    if let Some((rgba, w, h)) = thumb {
        image::save_buffer(thumb_path(slot), rgba, w, h, image::ExtendedColorType::Rgba8)?;
    }
    Ok(())
}

pub fn read_slot(slot: usize) -> Option<SaveData> {
    let data = std::fs::read(slot_path(slot)).ok()?;
    serde_json::from_slice(&data).ok()
}

pub fn read_global() -> GlobalData {
    std::fs::read(dir().join("global.json"))
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

pub fn write_global(g: &GlobalData) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    let json = serde_json::to_vec(g)?;
    std::fs::write(dir().join("global.json"), json).context("writing global.json")
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY/MM/DD HH:MM` (UTC offset ignored: local time is not available without extra deps).
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
