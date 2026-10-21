//! The Switch save file, `save:/savedata.bin` (nexl_Save, sub_B6330 / sub_F7504; buffer sub_A8E30).
//!
//! A 0x3E7000-byte image: 6144 bytes of system data at 0 (5952 used; sub_4EC60 writes, sub_4EA30 loads),
//! then 140 records of 0x7200 bytes (0x7140 used) at 6144 + 0x7200 n. A record is a copy of the save
//! snapshot word_F17FF0: u16 1010, u16 1 (non-zero = slot in use), i32 count of choice-ring entries, the
//! 15016-byte page-ring entry being saved (sub_5A910 layout, below), the first 0x3688 bytes of the previous
//! choice-ring entry, and the date at +0x7138. Records 0..99 are the save list, 100..138 the auto save ring
//! (newest at 100), 139 the quick save.
//!
//! This module only knows the byte layout; `Game` converts between it and its own state.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::save_tables::{F_BITS, F_BYTES, F_WORDS};

pub const FILE_SIZE: usize = 0x3E7000;
pub const SLOTS: usize = 140;
pub const LIST_SLOTS: usize = 100;
pub const AUTO_FIRST: usize = 100;
pub const AUTO_COUNT: usize = 39;
pub const QUICK_SLOT: usize = 139;

const REC_BASE: usize = 6144;
const REC_STRIDE: usize = 0x7200;
pub const REC_SIZE: usize = 0x7140;
pub const RECORD_MAGIC: u16 = 1010;
/// page-ring entry (off_AE0478, 800 x 15016 bytes) inside a record
pub const PAGE_AT: usize = 8;
pub const PAGE_SIZE: usize = 15016;
/// i32 rewind-ring count min(ring index + 1, 1) and the ring entry it counts (first 0x3688 bytes)
const COUNT_AT: usize = 4;
const PREV_AT: usize = PAGE_AT + PAGE_SIZE;
const PREV_SIZE: usize = 0x3688;
const DATE_AT: usize = 0x7138;

// ---- page entry fields (sub_5A910 / sub_5BAF0) --------------------------------------------------
/// i64 script offset of the `;` before the command the entry was taken at
pub const P_OFFSET: usize = 8;
/// i64 script offset just past that command
pub const P_NEXT: usize = 16;
/// u16 scene number (allscr.mrg entry - 3, the same as knife's scene index)
pub const P_SCENE: usize = 24;
/// i16 read key of the current message (`_ZMsssmm`: sss and mm in hex, off_AE0810 / off_AE0818)
pub const P_READ_IDX: usize = 26;
pub const P_READ_SUB: usize = 28;
/// u8 speaker of the last message (off_AE0590) and of the pending voice (off_AE08B8; sub_2360 index)
pub const P_SPEAKER_LAST: usize = 30;
pub const P_SPEAKER_NEXT: usize = 31;
/// u16 entry type: 0 message, 1 stage change, 2 BGM, 3 SE, 6 auto save, 12 / 13 choice
pub const P_TYPE: usize = 32;
/// i16 BGM number (-1 none) and volume (`017, 0..100) (word_146E654 / word_146E650, MPLY sub_649E0)
pub const P_BGM: usize = 34;
pub const P_BGM_VOL: usize = 36;
/// i16 x 20: looping sound effect of each channel, SE_NAMES index (-1 none) (off_AE0960)
pub const P_SE_ID: usize = 38;
/// stage block (off_AE0250) and the copy taken before the first change since the last commit (off_AE08D0)
pub const P_STAGE: usize = 80;
pub const P_STAGE_PREV: usize = 6664;
pub const STAGE_SIZE: usize = 0x19B8;
pub const P_F_WORDS: usize = 13314;
pub const P_F_BITS: usize = 13442;
pub const P_F_BYTES: usize = 13449;
/// i32 voice id of the message (sub_23C0, -1 none; sub_6E0D0 from off_AE08B0) and i16 its speaker (-1 in
/// entries that are not messages)
pub const P_VOICE: usize = 13752;
pub const P_VOICE_SPEAKER: usize = 13756;
/// 0x200 bytes: raw text argument of the current message (byte_146EC28)
pub const P_TEXT: usize = 13956;
/// u8 x 20: SE channel volumes (0..100), then u8 x 20 SE channel loop flags
pub const P_SE_VOL: usize = 14468;
pub const P_SE_LOOP: usize = 14489;
/// 20 x 20 bytes of SE channel playback state (reset pattern below)
pub const P_SE_STATE: usize = 14612;
const SE_STATE_IDLE: [u8; 20] = [0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
pub const SE_CHANNELS: usize = 20;

// ---- stage block (struct of arrays over 40 layers; STCC sub_62140 resets a layer) ---------------
pub const LAYERS: usize = 40;
pub mod st {
    /// i16 image table index (-1 none)
    pub const IMAGE: usize = 4;
    /// u8 flags: 0x40 SACL absolute
    pub const FLAGS: usize = 84;
    /// i8 STBR blend mode (-1 none)
    pub const BLEND: usize = 124;
    /// f32 position in 640x480 units (script x * 0.5, y * 2/3)
    pub const X: usize = 164;
    pub const Y: usize = 324;
    /// i16 alpha 0..255
    pub const ALPHA: usize = 484;
    /// i16 STCP priority
    pub const PRIO: usize = 564;
    /// i32 STRT rotation (1/100 degree)
    pub const ROT: usize = 644;
    /// i16 STCH zoom x1000
    pub const ZOOM: usize = 804;
    /// f32 image anchor: ImageInfo fields 13 / 14 truncated toward zero (cmd_STCH_impl)
    pub const ANCHOR_X: usize = 884;
    pub const ANCHOR_Y: usize = 1044;
    pub const W1204: usize = 1204;
    pub const W1924: usize = 1924;
    /// i16 STZ4 zoom x / y x1000
    pub const ZOOM_X: usize = 2004;
    pub const ZOOM_Y: usize = 2084;
    /// i16 STCH arg 11 parent (-1 none)
    pub const PARENT: usize = 2204;
    /// i16 STTR (rule - 1) | (b << 12) (-1 none)
    pub const RULE: usize = 2284;
    /// i16 SBAS transform base (-1 none)
    pub const BASE: usize = 2364;
    /// i16 SOXY args 3 / 4: draw offset x (1920-wide pixels / 3) and y (x 0.41481)
    pub const SOXY_X: usize = 2444;
    pub const SOXY_Y: usize = 2524;
    /// i16 x 4 clip x, y, w, h (STCH args 12..15)
    pub const CLIP: usize = 2604;
    /// i16 scissor inset (STCH arg 16, motion field 16)
    pub const CLIP_INSET: usize = 2924;
    /// i8 inset mode (STCH arg 17, motion field 15)
    pub const CLIP_MODE: usize = 3004;
    /// i16 STRR / STCH arg 19 clip rotation (1/100 degree, -32768 = follow the layer rotation)
    pub const ROT2: usize = 3044;
    /// 12 bytes motion state: u16 id, u16, i32 frame, u8 paused
    pub const MOTION: usize = 3124;
    /// quake slots (sub_63520; sub_5D3A0 restores both): slot 0 SQK2 (mode 1), slot 1 SSW2 (mode 3) /
    /// mode 2. u8 mode per slot, i16 fade frames, i16 remaining frames (-1 forever), 6 x i16 parameters.
    pub const QK_MODE: usize = 3604;
    pub const QK_FADE: usize = 3684;
    pub const QK_DUR: usize = 3844;
    pub const QK_PARAM: usize = 4004;
    /// STNS grain value and its draw mode (1 when n > 2, else 3)
    pub const NOISE: usize = 5044;
    pub const NOISE_MODE: usize = 5084;
    /// i16 STEC mode (1 = colour fill)
    pub const EFFECT: usize = 5124;
    /// f32 x 4: SNX2 brightness (1 + v / 255), SNX3 contrast (1 + v / 100), 0, 0
    pub const TONE: usize = 5204;
    /// 18 bytes: i16 0, i16 -anchor x, i16 -anchor y, rgb fill (STEC colour, ff = none), rgb, 6 x 0
    pub const EXTRA: usize = 5844;
    /// layer-independent tail (tone table, FCLN, quake): 6564..6584
    pub const GLOBALS: usize = 6564;
    /// i8 FCLN effect (sub_92120; 0 after FCSP), restarted by the load replay
    pub const FOCUS: usize = 6569;
}

/// Default stage block: every layer as STCC leaves it.
pub fn empty_stage() -> Vec<u8> {
    let mut s = vec![0u8; STAGE_SIZE];
    s[0..2].copy_from_slice(&(-1i16).to_le_bytes());
    for l in 0..LAYERS {
        put_i16(&mut s, st::IMAGE + 2 * l, -1);
        s[st::BLEND + l] = 0xff;
        for (o, v) in [(st::ZOOM, 1000), (st::W1204, 1000), (st::W1924, -1), (st::ZOOM_X, 1000), (st::ZOOM_Y, 1000)] {
            put_i16(&mut s, o + 2 * l, v);
        }
        for o in [st::PARENT, st::RULE, st::BASE] {
            put_i16(&mut s, o + 2 * l, -1);
        }
        put_f32(&mut s, st::TONE + 16 * l, 1.0);
        put_f32(&mut s, st::TONE + 16 * l + 4, 1.0);
        s[st::EXTRA + 18 * l + 6..st::EXTRA + 18 * l + 9].fill(0xff);
    }
    s[st::GLOBALS + 8] = 0x78;
    s
}

pub fn get_i16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}
pub fn get_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn get_i32(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
pub fn get_i64(b: &[u8], o: usize) -> i64 {
    i64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
pub fn get_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
pub fn put_i16(b: &mut [u8], o: usize, v: i16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
pub fn put_i32(b: &mut [u8], o: usize, v: i32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
pub fn put_i64(b: &mut [u8], o: usize, v: i64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
pub fn put_f32(b: &mut [u8], o: usize, v: f32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Clamp to the exe's i16 variable range.
pub fn clamp16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// The F variables a page entry keeps, as (index, value).
pub fn read_f(page: &[u8]) -> Vec<(u16, i16)> {
    let mut v = Vec::with_capacity(F_WORDS.len() + F_BITS.len() + F_BYTES.len());
    for (k, &n) in F_WORDS.iter().enumerate() {
        v.push((n, get_i16(page, P_F_WORDS + 2 * k)));
    }
    for (k, &n) in F_BITS.iter().enumerate() {
        v.push((n, i16::from(page[P_F_BITS + k / 8] >> (k % 8) & 1)));
    }
    for (k, &n) in F_BYTES.iter().enumerate() {
        v.push((n, i16::from(page[P_F_BYTES + k])));
    }
    v
}

/// Set or clear flag `n` of a page's F subset (sub_53F00; no-op for words outside the flag list).
pub fn set_f_bit(page: &mut [u8], n: u16, on: bool) {
    if let Some(k) = F_BITS.iter().position(|&b| b == n) {
        let m = 1 << (k % 8);
        if on {
            page[P_F_BITS + k / 8] |= m;
        } else {
            page[P_F_BITS + k / 8] &= !m;
        }
    }
}

/// Store the F subset (sub_5A910): words whole, flags as bits (non-zero), the rest as their low byte.
pub fn write_f(page: &mut [u8], f: impl Fn(u16) -> i16) {
    for (k, &n) in F_WORDS.iter().enumerate() {
        put_i16(page, P_F_WORDS + 2 * k, f(n));
    }
    page[P_F_BITS..P_F_BYTES].fill(0);
    for (k, &n) in F_BITS.iter().enumerate() {
        if f(n) != 0 {
            page[P_F_BITS + k / 8] |= 1 << (k % 8);
        }
    }
    for (k, &n) in F_BYTES.iter().enumerate() {
        page[P_F_BYTES + k] = f(n) as u8;
    }
}

/// A fresh page entry with the reset values of sub_82330.
pub fn empty_page() -> Vec<u8> {
    let mut p = vec![0u8; PAGE_SIZE];
    put_i16(&mut p, P_BGM, -1);
    for ch in 0..SE_CHANNELS {
        put_i16(&mut p, P_SE_ID + 2 * ch, -1);
        p[P_SE_VOL + ch] = 100;
        p[P_SE_STATE + 20 * ch..P_SE_STATE + 20 * (ch + 1)].copy_from_slice(&SE_STATE_IDLE);
    }
    let stage = empty_stage();
    p[P_STAGE..P_STAGE + STAGE_SIZE].copy_from_slice(&stage);
    p[P_STAGE_PREV..P_STAGE_PREV + STAGE_SIZE].copy_from_slice(&stage);
    put_i32(&mut p, P_VOICE, -1);
    p
}

/// Wrap a page entry into a record dated `ts` (local seconds, `save::now`).
/// `prev`: the newest rewind-ring entry before this page (sub_527F0 stores count 1 and its first 0x3688
/// bytes; count 0 and no entry when the ring is empty).
pub fn record(page: &[u8], prev: Option<&[u8]>, ts: u64) -> Vec<u8> {
    let mut r = vec![0u8; REC_SIZE];
    r[0..2].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
    r[2..4].copy_from_slice(&1u16.to_le_bytes());
    r[PAGE_AT..PAGE_AT + PAGE_SIZE].copy_from_slice(page);
    if let Some(p) = prev {
        r[COUNT_AT..COUNT_AT + 4].copy_from_slice(&1i32.to_le_bytes());
        r[PREV_AT..PREV_AT + PREV_SIZE].copy_from_slice(&p[..PREV_SIZE]);
    }
    let (y, mo, d, h, mi, s) = civil(ts);
    r[DATE_AT..DATE_AT + 2].copy_from_slice(&(y as u16).to_le_bytes());
    r[DATE_AT + 2..DATE_AT + 7].copy_from_slice(&[mo as u8, d as u8, h as u8, mi as u8, s as u8]);
    r
}

/// The page entry of a record in use (u16 1010 header).
pub fn page_of(rec: &[u8]) -> Option<&[u8]> {
    (get_u16(rec, 0) == RECORD_MAGIC && get_u16(rec, 2) != 0).then(|| &rec[PAGE_AT..PAGE_AT + PAGE_SIZE])
}

/// The previous rewind-ring entry of a record as a full page entry, as a load puts it back (sub_82330): the
/// stored 0x3688 bytes with the text cleared; the rest (SE state) is the reset page's.
pub fn prev_of(rec: &[u8]) -> Option<Vec<u8>> {
    page_of(rec)?;
    if get_i32(rec, COUNT_AT) < 1 {
        return None;
    }
    let mut p = empty_page();
    p[..PREV_SIZE].copy_from_slice(&rec[PREV_AT..PREV_AT + PREV_SIZE]);
    p[P_TEXT] = 0;
    Some(p)
}

/// Record date as local seconds (`save::now`).
pub fn record_time(rec: &[u8]) -> u64 {
    let y = get_u16(rec, DATE_AT) as i64;
    let f = &rec[DATE_AT + 2..DATE_AT + 7];
    let (mo, d) = (f[0] as i64, f[1] as i64);
    // days-from-civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days * 86400 + f[2] as i64 * 3600 + f[3] as i64 * 60 + f[4] as i64).max(0) as u64
}

fn civil(ts: u64) -> (i64, i64, i64, i64, i64, i64) {
    let days = (ts / 86400) as i64;
    let secs = (ts % 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

// ---- system block ----------------------------------------------------------------------------------
pub const SYS_CONFIG: usize = 4;
pub const CONFIG_SIZE: usize = 104;
/// 500 i16 progress words (off_AE04E0): word 5 movie flags, 110+ RDST, 300+ RDED, 400+ edges
pub const SYS_G: usize = 108;
pub const G_WORDS: usize = 500;
/// seen bitmap (off_AE07A8, 531 bytes): images by sub_774F0 bit, BGM n at bit 3200 + n
pub const SYS_SEEN: usize = 1108;
pub const SEEN_BYTES: usize = 531;
/// read keys (off_AE0350 + 13, 4020 bytes): highest message sub-index read per `_ZMsss..`
pub const SYS_READ: usize = 1639;
pub const READ_KEYS: usize = 4020;
pub const SYS_CONTINUE: usize = 5662;
/// static config defaults (0x46B080, written by sub_2C810)
pub const CONFIG_DEFAULTS: [u8; CONFIG_SIZE] = [
    0xff, 0, 0, 0, 40, 0, 100, 0, 40, 0, 10, 0, 0, 0, 35, 0, 15, 0, 0xff, 0xff, 1, 0, 0, 0, 0, 0, 0, 0, 0x98, 0x08, 1, 0,
    1, 0, 0, 0, 0xff, 0xff, 0xff, 0x3f, 3, 0, 0, 0, 0, 0, 1, 0, 40, 0, 0, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0,
    50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 0, 0, 0xb8, 0x0b, 0, 0,
    0, 0, 0, 1, 0, 0, 0, 0,
];

/// Config byte offsets (sub_2A730 stores the config screen objects; sub_2A550 loads them).
pub mod cfg {
    /// i16 text language (sub_2DE00 from the system language: 0 Japanese, 1 English, 2 / 3 Chinese)
    pub const LANGUAGE: usize = 18;
    /// u16 text speed unread / read (object 58 / 62), u8 page at once unread / read (57 / 61)
    pub const TEXT_UNREAD: usize = 14;
    pub const TEXT_READ: usize = 16;
    pub const PAGE_UNREAD: usize = 90;
    pub const PAGE_READ: usize = 91;
    /// u16 auto mode wait at a page end / at a click wait (65 / 68)
    pub const AUTO_PAGE: usize = 92;
    pub const AUTO_CLICK: usize = 28;
    /// u16 auto skip read scenes (radio 69 / 70, 69 = on), u8 its confirmation (71)
    pub const AUTO_SKIP: usize = 94;
    pub const SKIP_CONFIRM: usize = 96;
    /// u16 skip read text only (radio 72 / 73, 73 = read only; sub_40ED0 stops at unread text when set)
    pub const SKIP_READ_ONLY: usize = 30;
    /// u8 text fade in / page fade out off (radios 74 / 75, 76 / 77: the second option sets them)
    pub const NO_FADE_IN: usize = 97;
    pub const NO_FADE_OUT: usize = 98;
    /// u16 BGM / SE / system SE volume 0..100 (81 / 86 / 90) and u8 mutes (80 / 84 / 89)
    pub const BGM: usize = 4;
    pub const SE: usize = 8;
    pub const SYS_SE: usize = 10;
    pub const BGM_MUTE: usize = 100;
    pub const SE_MUTE: usize = 101;
    pub const SYS_SE_MUTE: usize = 102;
    /// copy of the BGM volume written with it
    pub const BGM_COPY: usize = 48;
    /// u16 all voices (radio 91: 100, else 0), u16 stop voice on advance (radio 93)
    pub const VOICE_ALL: usize = 6;
    pub const VOICE_STOP: usize = 46;
    /// u16 x 19 character voice volumes 0..100 (objects 99 + 5i), u32 voice enable bits (mute 97 + 5i clears)
    pub const CHAR_VOICE: usize = 52;
    pub const VOICE_BITS: usize = 36;
}

pub struct SaveFile {
    pub buf: Vec<u8>,
}

impl SaveFile {
    /// An empty file: no records, default config, no CONTINUE record.
    pub fn new() -> Self {
        let mut buf = vec![0u8; FILE_SIZE];
        buf[SYS_CONFIG..SYS_CONFIG + CONFIG_SIZE].copy_from_slice(&CONFIG_DEFAULTS);
        put_i16(&mut buf, 2, 1);
        // sub_4EA30 fills a negative language word from the system language (sub_2DE00: 0 Japanese)
        put_i16(&mut buf, SYS_CONFIG + cfg::LANGUAGE, 0);
        Self { buf }
    }

    pub fn path() -> PathBuf {
        crate::save::dir().join("savedata.bin")
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let buf = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if buf.len() != FILE_SIZE {
            bail!("{}: {} bytes, expected {FILE_SIZE}", path.display(), buf.len());
        }
        let mut f = Self { buf };
        // the system block counts only when u16 +2 is set (sub_4EA30)
        if get_u16(&f.buf, 2) == 0 {
            let fresh = Self::new();
            f.buf[..REC_BASE].copy_from_slice(&fresh.buf[..REC_BASE]);
        }
        if get_i16(&f.buf, SYS_CONFIG + cfg::LANGUAGE) < 0 {
            put_i16(&mut f.buf, SYS_CONFIG + cfg::LANGUAGE, 0);
        }
        Ok(f)
    }

    /// The save directory's savedata.bin, or an empty file when there is none (or it is unreadable).
    pub fn load() -> Self {
        let path = Self::path();
        if !path.exists() {
            return Self::new();
        }
        Self::load_from(&path).unwrap_or_else(|e| {
            tracing::warn!("{e:#}");
            Self::new()
        })
    }

    pub fn store(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(crate::save::dir())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &self.buf)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }

    fn rec_range(n: usize) -> std::ops::Range<usize> {
        let o = REC_BASE + REC_STRIDE * n;
        o..o + REC_SIZE
    }

    /// Record n when its slot is in use (u16 +2 non-zero, sub_52BF0).
    pub fn record(&self, n: usize) -> Option<&[u8]> {
        (n < SLOTS)
            .then(|| &self.buf[Self::rec_range(n)])
            .filter(|r| get_u16(r, 0) != 0 && get_u16(r, 2) != 0)
    }

    /// sub_4EEB0: the manual records (0..100) whose u16 +2 is set; the title opens on LOAD GAME when any is.
    pub fn manual_count(&self) -> usize {
        (0..AUTO_FIRST).filter(|&n| get_u16(&self.buf[Self::rec_range(n)], 2) != 0).count()
    }

    pub fn set_record(&mut self, n: usize, rec: &[u8]) {
        let r = Self::rec_range(n);
        self.buf[r].copy_from_slice(&rec[..REC_SIZE]);
    }

    /// sub_52AA0: records 100..137 move up one (138 drops out) to make room for a new auto save at 100.
    pub fn shift_auto(&mut self) {
        for k in (AUTO_FIRST..AUTO_FIRST + AUTO_COUNT - 1).rev() {
            let (src, dst) = (Self::rec_range(k), Self::rec_range(k + 1));
            self.buf.copy_within(src, dst.start);
        }
    }

    pub fn config(&self) -> &[u8] {
        &self.buf[SYS_CONFIG..SYS_CONFIG + CONFIG_SIZE]
    }

    pub fn config_mut(&mut self) -> &mut [u8] {
        &mut self.buf[SYS_CONFIG..SYS_CONFIG + CONFIG_SIZE]
    }

    pub fn g(&self, n: usize) -> i16 {
        get_i16(&self.buf, SYS_G + 2 * n)
    }

    pub fn set_g(&mut self, n: usize, v: i16) {
        put_i16(&mut self.buf, SYS_G + 2 * n, v);
    }

    pub fn seen(&self) -> &[u8] {
        &self.buf[SYS_SEEN..SYS_SEEN + SEEN_BYTES]
    }

    pub fn seen_mut(&mut self) -> &mut [u8] {
        &mut self.buf[SYS_SEEN..SYS_SEEN + SEEN_BYTES]
    }

    pub fn read_keys(&self) -> &[u8] {
        &self.buf[SYS_READ..SYS_READ + READ_KEYS]
    }

    pub fn read_keys_mut(&mut self) -> &mut [u8] {
        &mut self.buf[SYS_READ..SYS_READ + READ_KEYS]
    }

    /// sub_4EC60 mode 2: CONTINUE resumes the newest record of 0..138 (never the quick save; the first of
    /// equal dates), 0 when there is none.
    pub fn update_continue(&mut self) {
        let newest = (0..QUICK_SLOT)
            .filter_map(|n| self.record(n).map(|r| (record_time(r), std::cmp::Reverse(n))))
            .max()
            .map_or(0, |(_, n)| n.0);
        put_i16(&mut self.buf, SYS_CONTINUE, newest as i16);
    }

    pub fn continue_slot(&self) -> Option<usize> {
        usize::try_from(get_i16(&self.buf, SYS_CONTINUE)).ok().filter(|&n| self.record(n).is_some())
    }
}

impl Default for SaveFile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_round_trip() {
        let ts = 1_792_390_104; // 2026-10-19 06:08:24 UTC
        let page = empty_page();
        let r = record(&page, None, ts);
        assert_eq!(&r[DATE_AT..DATE_AT + 7], &[0xEA, 0x07, 10, 19, 6, 8, 24]);
        assert_eq!(record_time(&r), ts);
        assert_eq!(get_i32(&r, COUNT_AT), 0);
        assert!(prev_of(&r).is_none());
    }

    #[test]
    fn prev_entry_round_trip() {
        let page = empty_page();
        let mut prev = empty_page();
        put_i16(&mut prev, P_SCENE, 12);
        prev[P_TEXT] = b'x';
        let r = record(&page, Some(&prev), 0);
        assert_eq!(get_i32(&r, COUNT_AT), 1);
        let p = prev_of(&r).unwrap();
        assert_eq!((get_i16(&p, P_SCENE), p[P_TEXT]), (12, 0));
    }

    #[test]
    fn fresh_file_and_continue() {
        let mut f = SaveFile::new();
        // a fresh Switch profile (savedata000_introvideo): CONTINUE 0, word_F1F43C 0, language 0
        assert_eq!((get_i16(&f.buf, SYS_CONTINUE), get_i16(&f.buf, SYS_CONTINUE + 4)), (0, 0));
        assert_eq!(get_i16(&f.buf, SYS_CONFIG + cfg::LANGUAGE), 0);
        assert_eq!(f.continue_slot(), None);
        let page = empty_page();
        f.set_record(5, &record(&page, None, 100));
        f.set_record(3, &record(&page, None, 100));
        f.set_record(7, &record(&page, None, 50));
        f.update_continue();
        assert_eq!(f.continue_slot(), Some(3));
    }

    #[test]
    fn set_f_bit_marks_flag() {
        let mut page = empty_page();
        set_f_bit(&mut page, 1118, true);
        set_f_bit(&mut page, 22, true); // a word, not a flag: ignored
        let f: std::collections::HashMap<u16, i16> = read_f(&page).into_iter().collect();
        assert_eq!((f[&1118], f[&22]), (1, 0));
    }

    #[test]
    fn f_subset_round_trip() {
        let mut page = empty_page();
        write_f(&mut page, |n| match n {
            22 => 7,
            110 => 486,
            998 => 1,
            201 => 0x1ff,
            _ => 0,
        });
        let f: std::collections::HashMap<u16, i16> = read_f(&page).into_iter().collect();
        assert_eq!((f[&22], f[&110], f[&998], f[&201], f[&23]), (7, 486, 1, 0xff, 0));
    }
}
