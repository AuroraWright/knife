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

use crate::save_tables::{F_BITS, F_BYTES, F_WORDS, WITCH_F_BITS, WITCH_F_BYTES, WITCH_F_WORDS};

/// A build's save file layout (`lay()`). Witch on the Holy Night (sub_2F560 writes, sub_2F280 reads) keeps
/// 5976 bytes of system data in a 7168-byte block, then 138 records of 28112 bytes at 7168 + 30208 n (u16 1000,
/// u16 1, i32 count, a 14528-byte page entry (jp_5A910), the first 0x34C0 bytes of the previous one, the date
/// at +28040): 0..119 the save list (6 pages of 20, sub_353B0), 120 the quick save (sub_358C0), 121 only
/// compared (sub_35970), 122 the title's resume record and auto save (sub_35790); nothing uses 123..137.
pub struct Layout {
    pub file_size: usize,
    pub slots: usize,
    pub list_slots: usize,
    /// the auto save record(s): Tsukihime's ring of 39 from 100 (newest first), Witch on the Holy Night's 122
    pub auto_first: usize,
    pub auto_count: usize,
    pub quick_slot: usize,
    rec_base: usize,
    rec_stride: usize,
    pub rec_size: usize,
    pub magic: u16,
    pub page_size: usize,
    prev_size: usize,
    date_at: usize,
    pub p_type: usize,
    pub p_bgm: usize,
    pub p_bgm_vol: usize,
    pub p_se_id: usize,
    pub p_stage: usize,
    pub p_stage_prev: usize,
    pub stage_size: usize,
    pub p_f_words: usize,
    pub p_f_bits: usize,
    pub p_f_bytes: usize,
    pub p_voice: usize,
    pub p_voice_speaker: usize,
    pub p_text: usize,
    pub p_se_vol: usize,
    pub p_se_loop: usize,
    pub p_se_pan: usize,
    pub p_se_state: usize,
    f_words: &'static [u16],
    f_bits: &'static [u16],
    f_bytes: &'static [u16],
    /// system block (`rec_base` bytes): config, progress words (G), seen bitmap, read keys, the CONTINUE / save
    /// list cursor
    pub sys_config: usize,
    pub config_size: usize,
    pub sys_g: usize,
    pub g_words: usize,
    pub sys_seen: usize,
    pub seen_bytes: usize,
    pub sys_read: usize,
    pub sys_continue: usize,
    pub st: StageLayout,
}

/// The stage block (struct of arrays over 40 layers; STCC resets a layer).
pub struct StageLayout {
    /// i16 image table index (-1 none)
    pub image: usize,
    /// u8 flags: 0x40 SACL absolute
    pub flags: usize,
    /// i8 STBR blend mode (-1 none)
    pub blend: usize,
    /// f32 position in 640x480 units (script x * 0.5, y * 2/3)
    pub x: usize,
    pub y: usize,
    /// i16 alpha 0..255
    pub alpha: usize,
    /// i16 STCP priority
    pub prio: usize,
    /// i32 STRT rotation (1/100 degree)
    pub rot: usize,
    /// i16 STCH zoom x1000
    pub zoom: usize,
    /// f32 image anchor: ImageInfo fields 13 / 14 truncated toward zero (cmd_STCH_impl)
    pub anchor_x: usize,
    pub anchor_y: usize,
    /// i16 words a reset leaves at 1000 and -1
    pub w_1000: usize,
    pub w_neg: usize,
    /// i16 STZ4 zoom x / y x1000
    pub zoom_x: usize,
    pub zoom_y: usize,
    /// i16 STCH arg 11 parent (-1 none)
    pub parent: usize,
    /// i16 STTR (rule - 1) | (b << 12) (-1 none)
    pub rule: usize,
    /// i16 SBAS transform base (-1 none); Witch on the Holy Night stubs SBAS and has no such word
    pub base: Option<usize>,
    /// i16 SOXY draw offset x (1920-wide pixels / 3) and y (x 0.41481)
    pub soxy_x: usize,
    pub soxy_y: usize,
    /// i16 x 4 clip x, y, w, h (STCH args 12..15)
    pub clip: usize,
    /// i16 scissor inset (STCH arg 16, motion field 16)
    pub clip_inset: usize,
    /// i8 inset mode (STCH arg 17, motion field 15)
    pub clip_mode: usize,
    /// i16 STRR / STCH arg 19 clip rotation (1/100 degree, -32768 = follow the layer rotation)
    pub rot2: usize,
    /// 12 bytes motion state: u16 id, u16, i32 frame, u8 paused
    pub motion: usize,
    /// Tsukihime's quake slots (sub_63520; sub_5D3A0 restores both): slot 0 SQK2 (mode 1), slot 1 SSW2
    /// (mode 3) / mode 2. u8 mode per slot, i16 fade frames, i16 remaining frames (-1 forever), 6 x i16
    /// parameters.
    pub qk_mode: usize,
    pub qk_fade: usize,
    pub qk_dur: usize,
    pub qk_param: usize,
    /// Witch on the Holy Night's one quake slot (`stage::WitchQuake`): i16 ax, ay, P, D, delay, starting D
    /// (40 words apart), u8 type
    pub wq: usize,
    pub wq_type: usize,
    /// STNS grain value and its draw mode (Tsukihime: 1 when n > 2, else 3; Witch on the Holy Night sub_10640)
    pub noise: usize,
    pub noise_mode: usize,
    /// i16 STEC mode (1 = colour fill)
    pub effect: usize,
    /// f32 x 4: SNX2 brightness (1 + v / 255), SNX3 contrast (1 + v / 100), SNX4's two values (Witch on the
    /// Holy Night; 0 in Tsukihime)
    pub tone: usize,
    /// 18 bytes: i16 0, i16 -anchor x, i16 -anchor y, rgb fill (STEC colour, ff = none), rgb, 6 x 0
    pub extra: usize,
    /// Witch on the Holy Night's STGS words: i16 start and current alpha, i16 frames (sub_13ED0 rebuilds the
    /// fade from them on a load; STCH sets both alphas to the layer's and 0 frames)
    pub alpha_fade: Option<usize>,
    /// layer-independent tail: +0 the backdrop colour index (Witch on the Holy Night: 0 black for the scenes
    /// of `witch_dark_scene`, else 1 white; Tsukihime: always 0), the FCLN byte, the 0x78 byte (sub_1DC40)
    pub globals: usize,
    /// i8 FCLN effect (sub_92120; 0 after FCSP), restarted by the load replay
    pub focus: usize,
    pub glob_78: usize,
}

pub const LAYERS: usize = 40;
pub const SE_STATE_IDLE: [u8; 20] = [0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
pub const SE_CHANNELS: usize = 20;

// ---- page entry fields both builds share (sub_5A910 / sub_5BAF0) ---------------------------------
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
/// i32 rewind-ring count min(ring index + 1, 1) and the ring entry it counts
const COUNT_AT: usize = 4;
/// page-ring entry inside a record
pub const PAGE_AT: usize = 8;

/// Tsukihime: a 0x3E7000-byte image: 6144 bytes of system data at 0 (5952 used; sub_4EC60 writes, sub_4EA30
/// loads), then 140 records of 0x7200 bytes (0x7140 used) at 6144 + 0x7200 n. A record is a copy of the save
/// snapshot word_F17FF0: u16 1030 (1.0.2 and the overseas build 1010; `record_magic`), u16 1 (non-zero = slot in
/// use), i32 count of choice-ring entries, the 15016-byte page-ring entry being saved (sub_5A910 layout), the first 0x3688 bytes of the previous choice-ring
/// entry, and the date at +0x7138. Records 0..99 are the save list, 100..138 the auto save ring (newest at
/// 100), 139 the quick save.
pub static JAPANESE: Layout = Layout {
    file_size: 0x3E7000,
    slots: 140,
    list_slots: 100,
    auto_first: 100,
    auto_count: 39,
    quick_slot: 139,
    rec_base: 6144,
    rec_stride: 0x7200,
    rec_size: 0x7140,
    magic: 1010,
    page_size: 15016,
    prev_size: 0x3688,
    date_at: 0x7138,
    // u16 entry type: 0 message, 1 stage change, 2 BGM, 3 SE, 6 auto save, 12 / 13 choice
    p_type: 32,
    // i16 BGM number (-1 none) and volume (`017, 0..100) (word_146E654 / word_146E650, MPLY sub_649E0)
    p_bgm: 34,
    p_bgm_vol: 36,
    // i16 x 20: looping sound effect of each channel, SE_NAMES index (-1 none) (off_AE0960)
    p_se_id: 38,
    // stage block (off_AE0250) and the copy taken before the first change since the last commit (off_AE08D0)
    p_stage: 80,
    p_stage_prev: 6664,
    stage_size: 0x19B8,
    p_f_words: 13314,
    p_f_bits: 13442,
    p_f_bytes: 13449,
    // i32 voice id of the message (sub_23C0, -1 none; sub_6E0D0 from off_AE08B0) and i16 its speaker (-1 in
    // entries that are not messages)
    p_voice: 13752,
    p_voice_speaker: 13756,
    // 0x200 bytes: raw text argument of the current message (byte_146EC28)
    p_text: 13956,
    // u8 x 20: SE channel volumes (0..100), then u8 x 20 SE channel loop flags
    p_se_vol: 14468,
    p_se_loop: 14489,
    // i16 x 20: SE channel pans (x 1000, byte_146E640 + 220)
    p_se_pan: 14510,
    // 20 x 20 bytes: each SE channel's queued key-68 command (byte_146E640 + 1112 + 20 ch, sub_66200; reset
    // pattern SE_STATE_IDLE): i16 frames, i16 sound id, i32 key 1, u8 type, i8 key 17, u8, u8 loop, i16 key 18,
    // i16, i32 key 70
    p_se_state: 14612,
    f_words: &F_WORDS,
    f_bits: &F_BITS,
    f_bytes: &F_BYTES,
    sys_config: 4,
    config_size: 104,
    // 500 i16 progress words (off_AE04E0): word 5 movie flags, 110+ RDST, 300+ RDED, 400+ edges
    sys_g: 108,
    g_words: 500,
    // seen bitmap (off_AE07A8, 531 bytes): images by sub_774F0 bit, BGM n at bit 3200 + n
    sys_seen: 1108,
    seen_bytes: 531,
    // read keys (off_AE0350 + 13, 4020 bytes): highest message sub-index read per `_ZMsss..`
    sys_read: 1639,
    sys_continue: 5662,
    st: StageLayout {
        image: 4,
        flags: 84,
        blend: 124,
        x: 164,
        y: 324,
        alpha: 484,
        prio: 564,
        rot: 644,
        zoom: 804,
        anchor_x: 884,
        anchor_y: 1044,
        w_1000: 1204,
        w_neg: 1924,
        zoom_x: 2004,
        zoom_y: 2084,
        parent: 2204,
        rule: 2284,
        base: Some(2364),
        soxy_x: 2444,
        soxy_y: 2524,
        clip: 2604,
        clip_inset: 2924,
        clip_mode: 3004,
        rot2: 3044,
        motion: 3124,
        qk_mode: 3604,
        qk_fade: 3684,
        qk_dur: 3844,
        qk_param: 4004,
        wq: 0,
        wq_type: 0,
        noise: 5044,
        noise_mode: 5084,
        effect: 5124,
        tone: 5204,
        extra: 5844,
        alpha_fade: None,
        globals: 6564,
        focus: 6569,
        glob_78: 6572,
    },
};

/// Witch on the Holy Night (see `Layout`): the page entry moves the type and the sound words 4 bytes on (the
/// ring serial at +4 is an i32), keeps the BGM volume as 255 x percent / 100 at +13276, and its stage block
/// (0x18C0 bytes) has its own field order.
pub static WITCH: Layout = Layout {
    file_size: 0x3FB800,
    slots: 138,
    list_slots: 120,
    auto_first: 122,
    auto_count: 1,
    quick_slot: 120,
    rec_base: 7168,
    rec_stride: 30208,
    rec_size: 28112,
    magic: 1000,
    page_size: 14528,
    prev_size: 0x34C0,
    date_at: 28040,
    p_type: 36,
    p_bgm: 38,
    p_bgm_vol: 13276,
    p_se_id: 40,
    p_stage: 84,
    p_stage_prev: 6420,
    stage_size: 0x18C0,
    p_f_words: 12826,
    p_f_bits: 12954,
    p_f_bytes: 12961,
    p_voice: 13264,
    p_voice_speaker: 13268,
    p_text: 14016,
    p_se_vol: 13282,
    p_se_loop: 13303,
    p_se_pan: 13324,
    p_se_state: 13424,
    f_words: &WITCH_F_WORDS,
    f_bits: &WITCH_F_BITS,
    f_bytes: &WITCH_F_BYTES,
    sys_config: 4,
    config_size: 320,
    // 330 words; sub_2F280 ORs the words of off_632680 into the file's and the game's
    sys_g: 324,
    g_words: 330,
    // images below 3200 by jp_644A0's base image, BGM n <= 0x207 at bit 3200 + n (jp_77740)
    sys_seen: 984,
    seen_bytes: 524,
    sys_read: 1508,
    // u16 save list page and index of the newest list record (sub_2F560 mode 2; the save screen's cursor)
    sys_continue: 5528,
    st: StageLayout {
        image: 4,
        flags: 84,
        blend: 124,
        x: 164,
        y: 324,
        alpha: 484,
        prio: 724,
        rot: 804,
        zoom: 964,
        anchor_x: 1044,
        anchor_y: 1204,
        w_1000: 1684,
        w_neg: 2484,
        zoom_x: 2564,
        zoom_y: 2644,
        parent: 2764,
        rule: 2844,
        base: None,
        soxy_x: 2924,
        soxy_y: 3004,
        clip: 3084,
        clip_inset: 3404,
        clip_mode: 3484,
        rot2: 3524,
        motion: 3604,
        qk_mode: 0,
        qk_fade: 0,
        qk_dur: 0,
        qk_param: 0,
        wq: 4084,
        wq_type: 4564,
        noise: 4684,
        noise_mode: 4724,
        effect: 4764,
        tone: 4844,
        extra: 5484,
        alpha_fade: Some(2244),
        globals: 6204,
        focus: 6236,
        glob_78: 6244,
    },
};

/// The save layout of the loaded build.
pub fn lay() -> &'static Layout {
    if crate::assets::witch() { &WITCH } else { &JAPANESE }
}

/// The stage layout of the loaded build.
pub fn st() -> &'static StageLayout {
    &lay().st
}

/// Witch on the Holy Night's scenes that keep the black backdrop (jp_48AC0 sets stage word 0x183C to 0 for
/// them, 1 for every other scene, each story frame; sub_3E130 hands it to sub_13A60: clear colour off_6327E0
/// entry 0 black / 1 white).
pub fn witch_dark_scene(scene: usize) -> bool {
    matches!(
        scene,
        6..=11 | 0x3B | 0x42 | 0x49 | 0x4A | 0x81..=0x87 | 0x89 | 0x8A | 0xA3 | 0xBB | 0xC0 | 0xC5 | 0xD0 | 0xD2 | 0xD5 | 0xD8 | 0xF7
    )
}

/// Default stage block: every layer as a stage reset leaves it (Tsukihime sub_57450, Witch on the Holy Night
/// jp_57450; the backup copy's backdrop word is 0, the stage's 1).
pub fn empty_stage() -> Vec<u8> {
    let (l, st) = (lay(), st());
    let mut s = vec![0u8; l.stage_size];
    s[0..2].copy_from_slice(&(-1i16).to_le_bytes());
    for i in 0..LAYERS {
        put_i16(&mut s, st.image + 2 * i, -1);
        s[st.blend + i] = 0xff;
        for (o, v) in [(st.zoom, 1000), (st.w_1000, 1000), (st.w_neg, -1), (st.zoom_x, 1000), (st.zoom_y, 1000)] {
            put_i16(&mut s, o + 2 * i, v);
        }
        for o in [st.parent, st.rule].into_iter().chain(st.base) {
            put_i16(&mut s, o + 2 * i, -1);
        }
        put_f32(&mut s, st.tone + 16 * i, 1.0);
        put_f32(&mut s, st.tone + 16 * i + 4, 1.0);
        s[st.extra + 18 * i + 6..st.extra + 18 * i + 9].fill(0xff);
    }
    s[st.glob_78] = 0x78;
    if crate::assets::witch() {
        // the global zoom words 0x183E..0x1850: 0, 0, 1000, 320, 224, 0 x 3, 1000
        for (o, v) in [(6, 1000), (8, 320), (10, 224), (18, 1000)] {
            put_i16(&mut s, st.globals + o, v);
        }
    }
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

/// The F variables a page entry keeps, as (index, value), in the order the restore writes them (sub_82330 /
/// jp_82330: words, low bytes, flags), each F once with the value it ends with.
pub fn read_f(page: &[u8]) -> Vec<(u16, i16)> {
    let l = lay();
    let mut v: Vec<(u16, i16)> = Vec::with_capacity(l.f_words.len() + l.f_bits.len() + l.f_bytes.len());
    let mut put = |n: u16, x: i16| match v.iter_mut().find(|e| e.0 == n) {
        Some(e) => e.1 = x,
        None => v.push((n, x)),
    };
    for (k, &n) in l.f_words.iter().enumerate() {
        put(n, get_i16(page, l.p_f_words + 2 * k));
    }
    for (k, &n) in l.f_bytes.iter().enumerate() {
        put(n, i16::from(page[l.p_f_bytes + k]));
    }
    for (k, &n) in l.f_bits.iter().enumerate() {
        put(n, i16::from(page[l.p_f_bits + k / 8] >> (k % 8) & 1));
    }
    v
}

/// Set or clear flag `n` of a page's F subset (sub_53F00; no-op for words outside the flag list).
pub fn set_f_bit(page: &mut [u8], n: u16, on: bool) {
    let l = lay();
    if let Some(k) = l.f_bits.iter().position(|&b| b == n) {
        let m = 1 << (k % 8);
        if on {
            page[l.p_f_bits + k / 8] |= m;
        } else {
            page[l.p_f_bits + k / 8] &= !m;
        }
    }
}

/// Store the F subset (sub_5A910): words whole, flags as bits (non-zero), the rest as their low byte.
pub fn write_f(page: &mut [u8], f: impl Fn(u16) -> i16) {
    let l = lay();
    for (k, &n) in l.f_words.iter().enumerate() {
        put_i16(page, l.p_f_words + 2 * k, f(n));
    }
    page[l.p_f_bits..l.p_f_bytes].fill(0);
    for (k, &n) in l.f_bits.iter().enumerate() {
        if f(n) != 0 {
            page[l.p_f_bits + k / 8] |= 1 << (k % 8);
        }
    }
    for (k, &n) in l.f_bytes.iter().enumerate() {
        page[l.p_f_bytes + k] = f(n) as u8;
    }
}

/// A fresh page entry with the reset values of sub_82330 (Witch on the Holy Night: the BGM volume word 255).
pub fn empty_page() -> Vec<u8> {
    let l = lay();
    let mut p = vec![0u8; l.page_size];
    put_i16(&mut p, l.p_bgm, -1);
    if crate::assets::witch() {
        put_i16(&mut p, l.p_bgm_vol, 255);
    }
    for ch in 0..SE_CHANNELS {
        put_i16(&mut p, l.p_se_id + 2 * ch, -1);
        p[l.p_se_vol + ch] = 100;
        p[l.p_se_state + 20 * ch..l.p_se_state + 20 * (ch + 1)].copy_from_slice(&SE_STATE_IDLE);
    }
    let stage = empty_stage();
    p[l.p_stage..l.p_stage + l.stage_size].copy_from_slice(&stage);
    p[l.p_stage_prev..l.p_stage_prev + l.stage_size].copy_from_slice(&stage);
    put_i32(&mut p, l.p_voice, -1);
    p
}

/// Wrap a page entry into a record dated `ts` (local seconds, `save::now`).
/// `prev`: the newest rewind-ring entry before this page (sub_527F0 stores count 1 and its first bytes; count 0
/// and no entry when the ring is empty).
pub fn record(page: &[u8], prev: Option<&[u8]>, ts: u64) -> Vec<u8> {
    let l = lay();
    let prev_at = PAGE_AT + l.page_size;
    let mut r = vec![0u8; l.rec_size];
    r[0..2].copy_from_slice(&record_magic().to_le_bytes());
    r[2..4].copy_from_slice(&1u16.to_le_bytes());
    r[PAGE_AT..PAGE_AT + l.page_size].copy_from_slice(page);
    if let Some(p) = prev {
        r[COUNT_AT..COUNT_AT + 4].copy_from_slice(&1i32.to_le_bytes());
        r[prev_at..prev_at + l.prev_size].copy_from_slice(&p[..l.prev_size]);
    }
    let (y, mo, d, h, mi, s) = civil(ts);
    r[l.date_at..l.date_at + 2].copy_from_slice(&(y as u16).to_le_bytes());
    r[l.date_at + 2..l.date_at + 7].copy_from_slice(&[mo as u8, d as u8, h as u8, mi as u8, s as u8]);
    r
}

/// The magic a record is written with: 1030 in the Japanese 1.0.3 update (sub_B9020; sub_83260 resets the game
/// for any other snapshot), 1010 in 1.0.2 and the overseas build, 1000 in Witch on the Holy Night.
pub fn record_magic() -> u16 {
    if !crate::assets::witch() && !crate::assets::overseas() { JAPANESE_103_MAGIC } else { lay().magic }
}

const JAPANESE_103_MAGIC: u16 = 1030;

/// The page entry of a record in use (`record_magic`; the Japanese build also takes the 1010 records knife
/// wrote for 1.0.2).
pub fn page_of(rec: &[u8]) -> Option<&[u8]> {
    let l = lay();
    let m = get_u16(rec, 0);
    ((m == record_magic() || m == l.magic) && get_u16(rec, 2) != 0).then(|| &rec[PAGE_AT..PAGE_AT + l.page_size])
}

/// The previous rewind-ring entry of a record as a full page entry, as a load puts it back (sub_82330): the
/// stored bytes with the text cleared; the rest (SE state) is the reset page's.
pub fn prev_of(rec: &[u8]) -> Option<Vec<u8>> {
    page_of(rec)?;
    if get_i32(rec, COUNT_AT) < 1 {
        return None;
    }
    let l = lay();
    let prev_at = PAGE_AT + l.page_size;
    let mut p = empty_page();
    p[..l.prev_size].copy_from_slice(&rec[prev_at..prev_at + l.prev_size]);
    p[l.p_text] = 0;
    Some(p)
}

/// Record date as local seconds (`save::now`).
/// A record's date as stored at +28040: u16 year, then month, day, hour, minute, second.
pub fn record_date(rec: &[u8]) -> [u16; 6] {
    let at = lay().date_at;
    let f = &rec[at + 2..at + 7];
    [get_u16(rec, at), f[0].into(), f[1].into(), f[2].into(), f[3].into(), f[4].into()]
}

pub fn record_time(rec: &[u8]) -> u64 {
    let at = lay().date_at;
    let y = get_u16(rec, at) as i64;
    let f = &rec[at + 2..at + 7];
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
/// read keys (off_AE0350 + 13, 4020 bytes): highest message sub-index read per `_ZMsss..`, both builds
pub const READ_KEYS: usize = 4020;
pub const CONFIG_SIZE: usize = 104;
/// static config defaults (0x46B080, written by sub_2C810)
pub const CONFIG_DEFAULTS: [u8; CONFIG_SIZE] = [
    0xff, 0, 0, 0, 40, 0, 100, 0, 40, 0, 10, 0, 0, 0, 35, 0, 15, 0, 0xff, 0xff, 1, 0, 0, 0, 0, 0, 0, 0, 0x98, 0x08, 1, 0,
    1, 0, 0, 0, 0xff, 0xff, 0xff, 0x3f, 3, 0, 0, 0, 0, 0, 1, 0, 40, 0, 0, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0,
    50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 50, 0, 0, 0, 0xb8, 0x0b, 0, 0,
    0, 0, 0, 1, 0, 0, 0, 0,
];
/// Witch on the Holy Night's config defaults (0x2EB0B0, 320 bytes; offsets in `wcfg`)
#[rustfmt::skip]
pub const WITCH_CONFIG_DEFAULTS: [u8; 320] = [
    0xff, 0, 2, 0, 0, 0, 0x7f, 0, 1, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0, 0, 0x7f, 0, 0x7f, 0, 0x7f, 0, 0x7f, 0, 0, 0, 0, 0,
    0x7f, 0, 0x7f, 0, 0xff, 0xff, 1, 0, 0, 0, 0, 0, 0, 0, 0x7f, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x3f,
    3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0x7f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0,
    0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x64, 0, 0x80, 0x3c, 0x36, 0xfe, 0, 0, 1, 0, 0, 0, 1, 0,
    0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// The build's initial config block.
pub fn config_defaults() -> Vec<u8> {
    if crate::assets::witch() { WITCH_CONFIG_DEFAULTS.to_vec() } else { CONFIG_DEFAULTS.to_vec() }
}

/// The config language word's offset in the build's config block.
pub fn language_at() -> usize {
    if crate::assets::witch() { wcfg::LANGUAGE } else { cfg::LANGUAGE }
}

/// Witch on the Holy Night's config words (sub_10130 applies them, sub_10470 / sub_D700 store them).
pub mod wcfg {
    /// i16 0..255 sliders through sub_FF70's curve (k at 127, 255 at 255): BGM (k 102), voice (96), SE (102),
    /// system SE (32), movie (0x54, 102); sub_10130 keeps 100 x curve / 255 percent (AE0350 + 8 / + 9 / + 10
    /// / + 11 / + 7)
    pub const BGM: usize = 0x14;
    pub const VOICE: usize = 0x16;
    pub const SE: usize = 0x18;
    pub const SYS_SE: usize = 0x1A;
    pub const MOVIE: usize = 0x54;
    /// i16 text speed 0..255 (255 instant; jp_81B30 / jp_82170)
    pub const TEXT_SPEED: usize = 0x20;
    /// i16 game language (-1 the system's)
    pub const LANGUAGE: usize = 0x24;
    /// i16 auto mode wait level 0..255: 510 - 2 v frames at a click wait (jp_81B30)
    pub const AUTO_WAIT: usize = 0x2E;
    /// i16 skip only read text (jp_40ED0)
    pub const SKIP_READ_ONLY: usize = 0x30;
    /// u32 voice rows on (bit r, 15 rows; sub_76440)
    pub const VOICE_BITS: usize = 0x3C;
    /// i16 the voice stops on advance (jp_802C0)
    pub const VOICE_STOP: usize = 0x4E;
    /// i16 the message glyph edge (sub_A7790 -> sub_9D6DC / sub_9D704; config page case 3)
    pub const TEXT_EDGE: usize = 0x26;
    /// i16 the message colour mode (jp_6E0D0, jp_391A0, jp_40ED0; config page case 5): 1 read text 0xDC6D
    pub const TEXT_COLOUR: usize = 0x2A;
    /// i16 the message window's opacity 0..255 (jp_391A0: x / 255 for window types 0..2; the config's
    /// transparency slider is 255 - it)
    pub const WINDOW_ALPHA: usize = 0x10;
    /// i16 BGM auto adjust: the BGM ducks while a voice plays (jp_48AC0)
    pub const BGM_AUTO: usize = 0x1C;
    /// i16 the mode panel's previous / next scene buttons (jp_4E170); only the defaults write it (1)
    pub const SCENE_JUMP: usize = 0x32;
    /// i16 the button help balloons (jp_ADC40)
    pub const HELP: usize = 0x34;
    /// i16 the voices' master switch: 0 mutes every scene voice (jp_6E0D0 / jp_6E950) and the backlog's
    /// replay (sub_A700); nothing in the config pages shows it, only the voice page's reset writes it (1)
    pub const VOICE_ALL: usize = 0x36;
    /// i16 skip mode stops at a choice (jp_83E50)
    pub const CHOICE_SKIP_STOP: usize = 0x44;
    /// i16 x 15 voice row volumes, percent (sub_76440: x the voice slider / 255)
    pub const CHAR_VOICE: usize = 0x76;
    pub const VOICE_ROWS: usize = 15;
}

/// sub_FF70's slider curve: 0..127 -> 0..k, 127..255 -> k..255.
pub fn witch_curve(v: i16, k: i32) -> i32 {
    let v = i32::from(v);
    if v > 127 { k + (v - 127) * (k ^ 0xff) / 128 } else { k * v / 127 }
}

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
    /// u16 the message glyph edge and shadow (sub_A7790 -> sub_C1060 / sub_C1074)
    pub const TEXT_EDGE: usize = 20;
    /// u16 x 19 character voice volumes 0..100 (objects 99 + 5i), u32 voice enable bits (mute 97 + 5i clears)
    pub const CHAR_VOICE: usize = 52;
    pub const VOICE_BITS: usize = 36;
}

pub struct SaveFile {
    pub buf: Vec<u8>,
}

/// sub_2DE00's language of the console (`assets::system_language`), which sub_4EA30 writes into a negative
/// config language word.
static SYSTEM_LANGUAGE: std::sync::atomic::AtomicI16 = std::sync::atomic::AtomicI16::new(0);

pub fn set_system_language(language: i16) {
    SYSTEM_LANGUAGE.store(language, std::sync::atomic::Ordering::Relaxed);
}

/// `--language`: the config language word every load of the file starts with (-1: the file's own).
static LANGUAGE_OVERRIDE: std::sync::atomic::AtomicI16 = std::sync::atomic::AtomicI16::new(-1);

pub fn set_language_override(language: i16) {
    LANGUAGE_OVERRIDE.store(language, std::sync::atomic::Ordering::Relaxed);
}

fn system_language() -> i16 {
    SYSTEM_LANGUAGE.load(std::sync::atomic::Ordering::Relaxed)
}

impl SaveFile {
    /// An empty file: no records, default config, no CONTINUE record.
    pub fn new() -> Self {
        let l = lay();
        let mut buf = vec![0u8; l.file_size];
        buf[l.sys_config..l.sys_config + l.config_size].copy_from_slice(&config_defaults());
        put_i16(&mut buf, 2, 1);
        // sub_4EA30 (jp_2F280) fills a negative language word from the system language (sub_2DE00)
        put_i16(&mut buf, l.sys_config + language_at(), system_language());
        Self { buf }
    }

    /// The config language word (sub_2DE70 resolves it, `Assets::resolve_language`).
    pub fn language(&self) -> i16 {
        get_i16(&self.buf, lay().sys_config + language_at())
    }

    /// Set the config language word (the overseas config's language buttons, sub_2D910 state 10).
    pub fn set_language(&mut self, word: i16) {
        put_i16(&mut self.buf, lay().sys_config + language_at(), word);
    }

    pub fn path() -> PathBuf {
        crate::save::file()
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let buf = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let l = lay();
        if buf.len() != l.file_size {
            bail!("{}: {} bytes, expected {}", path.display(), buf.len(), l.file_size);
        }
        let mut f = Self { buf };
        // the system block counts only when u16 +2 is set (sub_4EA30)
        if get_u16(&f.buf, 2) == 0 {
            let fresh = Self::new();
            f.buf[..l.rec_base].copy_from_slice(&fresh.buf[..l.rec_base]);
        }
        if f.language() < 0 {
            f.set_language(system_language());
        }
        Ok(f)
    }

    /// The save file, or an empty file when there is none (or it is unreadable).
    pub fn load() -> Self {
        let path = Self::path();
        let mut f = if path.exists() {
            Self::load_from(&path).unwrap_or_else(|e| {
                tracing::warn!("{e:#}");
                Self::new()
            })
        } else {
            Self::new()
        };
        let language = LANGUAGE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
        if language >= 0 {
            f.set_language(language);
        }
        f
    }

    pub fn store(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &self.buf)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }

    fn rec_range(n: usize) -> std::ops::Range<usize> {
        let l = lay();
        let o = l.rec_base + l.rec_stride * n;
        o..o + l.rec_size
    }

    /// Record n when its slot is in use (u16 +2 non-zero, sub_52BF0).
    pub fn record(&self, n: usize) -> Option<&[u8]> {
        (n < lay().slots)
            .then(|| &self.buf[Self::rec_range(n)])
            .filter(|r| get_u16(r, 0) != 0 && get_u16(r, 2) != 0)
    }

    /// sub_4EEB0: the manual records (0..100) whose u16 +2 is set; the title opens on LOAD GAME when any is.
    pub fn manual_count(&self) -> usize {
        (0..lay().list_slots).filter(|&n| get_u16(&self.buf[Self::rec_range(n)], 2) != 0).count()
    }

    pub fn set_record(&mut self, n: usize, rec: &[u8]) {
        let r = Self::rec_range(n);
        self.buf[r].copy_from_slice(&rec[..lay().rec_size]);
    }

    /// sub_50FC0 state 23 (overseas): a deleted record's magic, used word and date (u32 at +0x7138 and
    /// +0x713B) are zeroed; the rest stays.
    pub fn delete_record(&mut self, n: usize) {
        let r = Self::rec_range(n);
        self.buf[r.start..r.start + 4].fill(0);
        let at = r.start + lay().date_at;
        self.buf[at..at + 7].fill(0);
    }

    /// sub_52AA0: records 100..137 move up one (138 drops out) to make room for a new auto save at 100
    /// (Witch on the Holy Night overwrites its one auto save record).
    pub fn shift_auto(&mut self) {
        let l = lay();
        for k in (l.auto_first..l.auto_first + l.auto_count - 1).rev() {
            let (src, dst) = (Self::rec_range(k), Self::rec_range(k + 1));
            self.buf.copy_within(src, dst.start);
        }
    }

    pub fn config(&self) -> &[u8] {
        let l = lay();
        &self.buf[l.sys_config..l.sys_config + l.config_size]
    }

    pub fn config_mut(&mut self) -> &mut [u8] {
        let l = lay();
        &mut self.buf[l.sys_config..l.sys_config + l.config_size]
    }

    pub fn g(&self, n: usize) -> i16 {
        get_i16(&self.buf, lay().sys_g + 2 * n)
    }

    pub fn set_g(&mut self, n: usize, v: i16) {
        put_i16(&mut self.buf, lay().sys_g + 2 * n, v);
    }

    pub fn seen(&self) -> &[u8] {
        let l = lay();
        &self.buf[l.sys_seen..l.sys_seen + l.seen_bytes]
    }

    pub fn seen_mut(&mut self) -> &mut [u8] {
        let l = lay();
        &mut self.buf[l.sys_seen..l.sys_seen + l.seen_bytes]
    }

    pub fn read_keys(&self) -> &[u8] {
        let at = lay().sys_read;
        &self.buf[at..at + READ_KEYS]
    }

    pub fn read_keys_mut(&mut self) -> &mut [u8] {
        let at = lay().sys_read;
        &mut self.buf[at..at + READ_KEYS]
    }

    /// sub_4EC60 mode 2: CONTINUE resumes the newest record of 0..138 (never the quick save; the first of
    /// equal dates), 0 when there is none. Witch on the Holy Night's sub_2F560 mode 2 keeps the newest of the
    /// list records 0..119 as a save screen page (0..5) and index (0..19), 0 / 0 when there is none.
    pub fn update_continue(&mut self) {
        let l = lay();
        let end = if crate::assets::witch() { l.list_slots } else { l.quick_slot };
        let newest = (0..end)
            .filter_map(|n| self.record(n).map(|r| (record_time(r), std::cmp::Reverse(n))))
            .max()
            .map_or(0, |(_, n)| n.0);
        if crate::assets::witch() {
            put_i16(&mut self.buf, l.sys_continue, (newest / 20) as i16);
            put_i16(&mut self.buf, l.sys_continue + 2, (newest % 20) as i16);
        } else {
            put_i16(&mut self.buf, l.sys_continue, newest as i16);
        }
    }

    /// Witch on the Holy Night's save screen cursor at install (sub_2F280(0)): the newest list record's page
    /// and index as sub_2F560 mode 2 keeps them.
    pub fn continue_cursor(&self) -> (i16, i16) {
        let at = lay().sys_continue;
        (get_i16(&self.buf, at).clamp(0, 5), get_i16(&self.buf, at + 2).clamp(0, 19))
    }

    /// The record the title's CONTINUE resumes: Tsukihime's newest (`update_continue`), Witch on the Holy
    /// Night's auto save record 122 (sub_35790).
    pub fn continue_slot(&self) -> Option<usize> {
        let l = lay();
        let n = if crate::assets::witch() { l.auto_first as i16 } else { get_i16(&self.buf, l.sys_continue) };
        usize::try_from(n).ok().filter(|&n| self.record(n).is_some())
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
        assert_eq!(&r[JAPANESE.date_at..JAPANESE.date_at + 7], &[0xEA, 0x07, 10, 19, 6, 8, 24]);
        assert_eq!(record_time(&r), ts);
        assert_eq!(get_i32(&r, COUNT_AT), 0);
        assert!(prev_of(&r).is_none());
    }

    #[test]
    fn prev_entry_round_trip() {
        let page = empty_page();
        let mut prev = empty_page();
        put_i16(&mut prev, P_SCENE, 12);
        prev[JAPANESE.p_text] = b'x';
        let r = record(&page, Some(&prev), 0);
        assert_eq!(get_i32(&r, COUNT_AT), 1);
        let p = prev_of(&r).unwrap();
        assert_eq!((get_i16(&p, P_SCENE), p[JAPANESE.p_text]), (12, 0));
    }

    #[test]
    fn fresh_file_and_continue() {
        let mut f = SaveFile::new();
        // a fresh Switch profile (savedata000_introvideo): CONTINUE 0, word_F1F43C 0, language 0
        assert_eq!((get_i16(&f.buf, JAPANESE.sys_continue), get_i16(&f.buf, JAPANESE.sys_continue + 4)), (0, 0));
        assert_eq!(f.language(), 0);
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

    /// Runs `f` with the Witch on the Holy Night layout selected.
    fn witch<T>(f: impl FnOnce() -> T) -> T {
        crate::assets::TEST_WITCH.set(true);
        let r = f();
        crate::assets::TEST_WITCH.set(false);
        r
    }

    #[test]
    fn witch_layout_fits() {
        for l in [&JAPANESE, &WITCH] {
            assert!(l.rec_base + l.rec_stride * (l.slots - 1) + l.rec_size <= l.file_size);
            assert!(PAGE_AT + l.page_size + l.prev_size <= l.date_at && l.date_at + 7 <= l.rec_size);
            assert!(l.p_stage + l.stage_size <= l.p_stage_prev && l.p_stage_prev + l.stage_size <= l.prev_size);
            assert!(l.p_text + 0x200 <= l.page_size);
        }
        witch(|| {
            let f = SaveFile::new();
            assert_eq!(f.buf.len(), 0x3FB800);
            // the BGM volume word of a fresh entry, and the WoH extras of an empty stage
            assert_eq!(get_i16(&empty_page(), WITCH.p_bgm_vol), 255);
            assert_eq!(get_i16(&empty_stage(), WITCH.st.globals + 6), 1000);
        });
    }

    #[test]
    fn witch_record_and_continue() {
        witch(|| {
            let page = empty_page();
            let r = record(&page, Some(&page), 1_792_390_104);
            assert_eq!(&r[WITCH.date_at..WITCH.date_at + 7], &[0xEA, 0x07, 10, 19, 6, 8, 24]);
            assert_eq!((get_i32(&r, COUNT_AT), record_time(&r)), (1, 1_792_390_104));
            let mut f = SaveFile::new();
            f.set_record(45, &record(&page, None, 100));
            f.set_record(3, &record(&page, None, 50));
            // a quick / auto record does not move the save list cursor
            f.set_record(WITCH.quick_slot, &record(&page, None, 200));
            f.update_continue();
            assert_eq!((get_i16(&f.buf, WITCH.sys_continue), get_i16(&f.buf, WITCH.sys_continue + 2)), (2, 5));
            // CONTINUE resumes the auto save record
            assert_eq!(f.continue_slot(), None);
            f.set_record(WITCH.auto_first, &record(&page, None, 10));
            assert_eq!(f.continue_slot(), Some(122));
        });
    }

    #[test]
    fn witch_f_subset_round_trip() {
        witch(|| {
            let mut page = empty_page();
            write_f(&mut page, |n| match n {
                3 => 486,
                298 => 7,
                998 => 1,
                _ => 0,
            });
            let f: std::collections::HashMap<u16, i16> = read_f(&page).into_iter().collect();
            assert_eq!((f[&3], f[&298], f[&998], f[&110]), (486, 7, 1, 0));
            assert_eq!(get_i16(&page, WITCH.p_f_words + 2 * 3), 486);
        });
    }
}
