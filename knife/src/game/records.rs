//! Game state <-> the Switch save data (`savedata.rs`): save records (page-ring entries, sub_5A910 /
//! sub_5BAF0 / sub_82330) and the system block (sub_4EC60 / sub_4EA30).

use std::{collections::HashMap, sync::OnceLock};

use super::{Game, Settings, get_bit, read_key, set_bit};
use crate::{
    save::{Kept, LayerSave, SaveData, SeQueued, SeSave},
    stage::Source,
    save_tables::{SE_NAMES, SEEN_ALIAS, WITCH_SE_NAMES},
    savedata::{self as sd, SaveFile, cfg, get_f32, get_i16, get_i32, get_i64, get_u16, put_f32, put_i16, put_i32, put_i64, wcfg},
};

/// Images below this table index have their own seen bit (sub_774F0); the rest go through SEEN_ALIAS.
pub(super) const SEEN_DIRECT: usize = 2701;
/// BGM n is heard at seen bit 3200 + n (MPLY sub_649E0, n <= 0x417; Witch on the Holy Night jp_77740, n <=
/// 0x207).
const BGM_SEEN: usize = 3200;
fn bgm_count() -> usize {
    if crate::assets::witch() { 520 } else { 1048 }
}
/// progress words (G) holding bitmaps / flags rather than script variables
const G_MOVIES: usize = 5;
const G_NODES: usize = 110;
const G_ENDINGS: usize = 300;
const G_EDGES: usize = 400;
const NODE_WORDS: usize = crate::flow::NODE_BITS / 16;
const EDGE_WORDS: usize = crate::flow::EDGE_BITS.div_ceil(16);
/// entry types (sub_5A910 third argument)
const TYPE_MESSAGE: u16 = 0;
const TYPE_CHOICE: u16 = 12;
const TYPE_ATSV: u16 = 6;
/// ATSV(1): the last message's text kept (0x78fb0)
const TYPE_ATSV_TEXT: u16 = 7;

/// The build's sound effect name table (sub_900 / jp_900).
fn se_names() -> &'static [&'static str] {
    if crate::assets::witch() { &WITCH_SE_NAMES } else { &SE_NAMES }
}

/// sub_900: a sound effect's index in the name table.
fn se_id(name: &str) -> Option<i16> {
    se_names().iter().position(|n| n.eq_ignore_ascii_case(name)).map(|i| i as i16)
}

fn alias_bits() -> &'static HashMap<&'static str, u16> {
    static MAP: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();
    MAP.get_or_init(|| SEEN_ALIAS.iter().copied().collect())
}

fn is_variant(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 6 && b[b.len() - 6] == b'_' && b[b.len() - 3] == b'_' && [1, 2, 4, 5].iter().all(|&k| b[b.len() - 6 + k].is_ascii_digit())
}

/// sub_10130's gain of a Witch on the Holy Night slider: the whole percent 100 x curve / 255.
fn witch_gain(level: i16, k: i32) -> f32 {
    (100 * sd::witch_curve(level, k) / 255) as f32 / 100.0
}

/// jp_48AC0's BGM volume (jp_AD090): the curve level at the ducking level (of 256) / 512, as a whole percent of
/// 128.
fn witch_bgm_gain(duck: i32, curve: i32) -> f32 {
    let a = (duck as f32 * curve as f32 * (1.0 / 512.0)) as i32;
    (100 * a / 128).clamp(0, 100) as f32 / 100.0
}

/// Store a gain as its slider level, keeping the level when it already gives that gain (levels below 127 share
/// gains).
fn put_witch_level(c: &mut [u8], at: usize, k: i32, gain: f32) {
    if (witch_gain(get_i16(c, at), k) - gain).abs() > 1.0e-4 {
        let v = (0..=255).min_by_key(|&v| ((witch_gain(v, k) - gain).abs() * 1.0e6) as i32);
        put_i16(c, at, v.unwrap_or(127));
    }
}

/// Witch on the Holy Night's auto mode wait at a click wait (jp_81B30): 510 - 2 x the config level frames.
fn witch_auto_ms(level: i16) -> f32 {
    f32::from(510 - 2 * level.clamp(0, 255)) * 1000.0 / 60.0
}

impl Settings {
    /// Settings from the build's config block (offsets in savedata::cfg / savedata::wcfg).
    pub fn from_config(c: &[u8]) -> Self {
        if crate::assets::witch() {
            return Self::from_witch_config(c);
        }
        let w = |o: usize| f32::from(get_u16(c, o));
        Self {
            text_ms: [w(cfg::TEXT_UNREAD), w(cfg::TEXT_READ)],
            page_at_once: [c[cfg::PAGE_UNREAD] != 0, c[cfg::PAGE_READ] != 0],
            auto_page_ms: w(cfg::AUTO_PAGE),
            auto_click_ms: w(cfg::AUTO_CLICK),
            auto_skip_read: get_u16(c, cfg::AUTO_SKIP) != 0,
            skip_confirm: c[cfg::SKIP_CONFIRM] != 0,
            skip_unread: get_u16(c, cfg::SKIP_READ_ONLY) == 0,
            text_fade_in: c[cfg::NO_FADE_IN] == 0,
            text_fade_out: c[cfg::NO_FADE_OUT] == 0,
            voice_on: get_u16(c, cfg::VOICE_ALL) != 0,
            voice_stop_on_advance: get_u16(c, cfg::VOICE_STOP) != 0,
            sys_se: w(cfg::SYS_SE) / 100.0,
            mute: u32::from(c[cfg::BGM_MUTE] != 0) | u32::from(c[cfg::SE_MUTE] != 0) << 1 | u32::from(c[cfg::SYS_SE_MUTE] != 0) << 2,
            char_voice: (0..19).map(|i| w(cfg::CHAR_VOICE + 2 * i) / 100.0).collect(),
            char_mute: !(get_i32(c, cfg::VOICE_BITS) as u32) & 0x7ffff,
            text_edge: get_u16(c, cfg::TEXT_EDGE) != 0,
            text_colour: 0,
            window_alpha: 1.0,
            bgm_auto: false,
            choice_skip_stop: false,
            help_text: true,
            scene_jump: true,
        }
    }

    /// Witch on the Holy Night's 320-byte config (sub_10130): one text speed level, the auto wait level, the
    /// read-only skip, the voice rows (bits 0x3C, percent words 0x76), the voice stop and the system SE slider
    /// through sub_FF70's curve. It has no page-at-once, text fade, auto skip or mute options.
    fn from_witch_config(c: &[u8]) -> Self {
        let w = |o: usize| get_i16(c, o);
        let speed = f32::from(w(wcfg::TEXT_SPEED));
        let auto = witch_auto_ms(w(wcfg::AUTO_WAIT));
        Self {
            text_ms: [speed, speed],
            page_at_once: [false, false],
            auto_page_ms: auto,
            auto_click_ms: auto,
            auto_skip_read: false,
            skip_confirm: false,
            skip_unread: w(wcfg::SKIP_READ_ONLY) == 0,
            text_fade_in: true,
            text_fade_out: true,
            voice_on: w(wcfg::VOICE_ALL) != 0,
            voice_stop_on_advance: w(wcfg::VOICE_STOP) != 0,
            sys_se: witch_gain(w(wcfg::SYS_SE), 32),
            mute: 0,
            char_voice: (0..wcfg::VOICE_ROWS).map(|i| f32::from(w(wcfg::CHAR_VOICE + 2 * i)) / 100.0).collect(),
            char_mute: !(get_i32(c, wcfg::VOICE_BITS) as u32) & 0x7fff,
            text_edge: w(wcfg::TEXT_EDGE) != 0,
            text_colour: w(wcfg::TEXT_COLOUR) as u16,
            window_alpha: f32::from(w(wcfg::WINDOW_ALPHA)) * 0.0039216,
            bgm_auto: w(wcfg::BGM_AUTO) != 0,
            choice_skip_stop: w(wcfg::CHOICE_SKIP_STOP) != 0,
            help_text: w(wcfg::HELP) != 0,
            scene_jump: w(wcfg::SCENE_JUMP) != 0,
        }
    }

    /// Write these settings into a config block, keeping the fields knife does not use.
    pub fn to_config(&self, c: &mut [u8]) {
        let pw = |c: &mut [u8], o: usize, v: f32| put_i16(c, o, v.round().clamp(0.0, 32767.0) as i16);
        if crate::assets::witch() {
            // the levels knife's settings hold as ms or gains go back to the nearest level that gives them
            pw(c, wcfg::TEXT_SPEED, self.text_ms[0].clamp(0.0, 255.0));
            let auto = (0..=255).min_by_key(|&v| (witch_auto_ms(v) - self.auto_click_ms).abs() as i32).unwrap_or(127);
            put_i16(c, wcfg::AUTO_WAIT, auto);
            put_i16(c, wcfg::SKIP_READ_ONLY, (!self.skip_unread).into());
            put_i16(c, wcfg::VOICE_STOP, self.voice_stop_on_advance.into());
            put_witch_level(c, wcfg::SYS_SE, 32, self.sys_se);
            for (i, v) in self.char_voice.iter().take(wcfg::VOICE_ROWS).enumerate() {
                pw(c, wcfg::CHAR_VOICE + 2 * i, v * 100.0);
            }
            let bits = get_i32(c, wcfg::VOICE_BITS) as u32 & !0x7fff | !self.char_mute & 0x7fff;
            put_i32(c, wcfg::VOICE_BITS, bits as i32);
            put_i16(c, wcfg::TEXT_EDGE, self.text_edge.into());
            put_i16(c, wcfg::TEXT_COLOUR, self.text_colour as i16);
            if (f32::from(get_i16(c, wcfg::WINDOW_ALPHA)) * 0.0039216 - self.window_alpha).abs() > 1.0e-6 {
                put_i16(c, wcfg::WINDOW_ALPHA, (self.window_alpha / 0.0039216).round().clamp(0.0, 255.0) as i16);
            }
            put_i16(c, wcfg::BGM_AUTO, self.bgm_auto.into());
            put_i16(c, wcfg::CHOICE_SKIP_STOP, self.choice_skip_stop.into());
            put_i16(c, wcfg::HELP, self.help_text.into());
            return;
        }
        pw(c, cfg::TEXT_UNREAD, self.text_ms[0]);
        pw(c, cfg::TEXT_READ, self.text_ms[1]);
        c[cfg::PAGE_UNREAD] = self.page_at_once[0].into();
        c[cfg::PAGE_READ] = self.page_at_once[1].into();
        pw(c, cfg::AUTO_PAGE, self.auto_page_ms);
        pw(c, cfg::AUTO_CLICK, self.auto_click_ms);
        put_i16(c, cfg::AUTO_SKIP, self.auto_skip_read.into());
        c[cfg::SKIP_CONFIRM] = self.skip_confirm.into();
        put_i16(c, cfg::SKIP_READ_ONLY, (!self.skip_unread).into());
        c[cfg::NO_FADE_IN] = (!self.text_fade_in).into();
        c[cfg::NO_FADE_OUT] = (!self.text_fade_out).into();
        put_i16(c, cfg::VOICE_ALL, if self.voice_on { 100 } else { 0 });
        put_i16(c, cfg::VOICE_STOP, self.voice_stop_on_advance.into());
        pw(c, cfg::SYS_SE, self.sys_se * 100.0);
        c[cfg::BGM_MUTE] = (self.mute & 1 != 0).into();
        c[cfg::SE_MUTE] = (self.mute & 2 != 0).into();
        c[cfg::SYS_SE_MUTE] = (self.mute & 4 != 0).into();
        for (i, v) in self.char_voice.iter().take(19).enumerate() {
            pw(c, cfg::CHAR_VOICE + 2 * i, v * 100.0);
        }
        let bits = get_i32(c, cfg::VOICE_BITS) as u32 & !0x7ffff | !self.char_mute & 0x7ffff;
        put_i32(c, cfg::VOICE_BITS, bits as i32);
        put_i16(c, cfg::TEXT_EDGE, self.text_edge.into());
    }
}

impl Game {
    /// Seen-bitmap bit of an image (sub_774F0 / sub_77610): expression variants count as their body, table
    /// indices below 2701 are their own bit, the rest map through the alias table (sub_77360).
    pub fn seen_bit(&self, name: &str) -> Option<usize> {
        let up = name.to_ascii_uppercase();
        if crate::assets::witch() {
            // jp_644A0: an image with ImageInfo +67 bit 0 counts as the name less its last 6 characters (BG_ERROR
            // when there is none); bits above 3199 are not kept
            let idx = self.assets.image_index_of(&up)?;
            let variant = self.assets.tables.images.get(idx).is_some_and(|im| im.variant) && up.len() >= 6;
            let bit = if variant {
                self.assets.image_index_of(&up[..up.len() - 6]).or_else(|| self.assets.image_index_of("BG_ERROR"))?
            } else {
                idx
            };
            return (bit < BGM_SEEN).then_some(bit);
        }
        let base = if is_variant(&up) { &up[..up.len() - 6] } else { up.as_str() };
        let idx = self.assets.image_index_of(base)?;
        if idx < SEEN_DIRECT {
            return Some(idx);
        }
        alias_bits().get(base).map(|&b| usize::from(b)).filter(|&b| b < BGM_SEEN)
    }

    pub fn is_seen(&self, name: &str) -> bool {
        self.seen_bit(name).is_some_and(|b| get_bit(&self.seen, b))
    }

    // ---- records -------------------------------------------------------------------------------

    /// A save record for snapshot `d`, or None when its scene / position cannot be expressed.
    pub fn encode_record(&mut self, d: &SaveData) -> Option<Vec<u8>> {
        let mut p = self.encode_page(d)?;
        // every save write marks its page with F1118 (sub_50FC0 / sub_52AA0 -> sub_53F00); the scripts' init
        // blocks set it too
        sd::set_f_bit(&mut p, 1118, true);
        let prev = d.prev.as_deref().and_then(|q| self.encode_page(q));
        Some(sd::record(&p, prev.as_deref(), d.timestamp))
    }

    /// The page-ring entry (sub_5A910 layout) of snapshot `d`.
    fn encode_page(&mut self, d: &SaveData) -> Option<Vec<u8>> {
        let script = self.assets.scene(d.scene)?;
        let cmds = &script.commands;
        // a choice resumes at its first option (conditional ones are `IF__` .. `ZYxxxxx` label pairs), so the
        // choice is built again on load
        let (kind, at) = if matches!(d.kind, TYPE_ATSV | TYPE_ATSV_TEXT) {
            (d.kind, d.pc)
        } else if d.choice.is_some() {
            let mut i = d.pc.checked_sub(1)?;
            while i > 0 && (matches!(cmds[i - 1].name.as_str(), "SELR" | "IF__") || cmds[i - 1].name.starts_with("ZY")) {
                i -= 1;
            }
            (TYPE_CHOICE, i)
        } else {
            (TYPE_MESSAGE, d.pc)
        };
        let c = cmds.get(at)?;
        let l = sd::lay();
        let mut p = sd::empty_page();
        put_i64(&mut p, sd::P_OFFSET, c.offset as i64 - 1);
        let next = cmds.get(at + 1).map_or(c.offset + c.name.len() + 3, |n| n.offset);
        put_i64(&mut p, sd::P_NEXT, next as i64);
        put_i16(&mut p, sd::P_SCENE, d.scene as i16);
        put_i16(&mut p, l.p_type, kind as i16);
        // sub_5A910 marks every entry voiceless; sub_6E0D0 then gives a message its voice and speaker
        p[sd::P_SPEAKER_LAST] = d.speaker as u8;
        if kind == TYPE_MESSAGE {
            put_i32(&mut p, l.p_voice, d.voice.as_deref().map_or(-1, crate::voice_ids::id));
            put_i16(&mut p, l.p_voice_speaker, d.speaker);
            p[sd::P_SPEAKER_NEXT] = d.speaker as u8;
        } else {
            put_i16(&mut p, l.p_voice_speaker, -1);
        }
        // sub_5A910 stores the current read key (off_AE0810 / off_AE0818) in every entry
        if let Some((i, s)) = d.read_key.or_else(|| (kind == TYPE_MESSAGE).then(|| read_key(&c.name)).flatten()) {
            put_i16(&mut p, sd::P_READ_IDX, i as i16);
            put_i16(&mut p, sd::P_READ_SUB, s.into());
        }
        // +13956 (+14016): the entry's text (0x1FE bytes and a 0)
        let raw = d.text.as_bytes();
        let n = raw.len().min(0x1fe);
        p[l.p_text..l.p_text + n].copy_from_slice(&raw[..n]);
        if let Some((id, vol)) = d.bgm {
            put_i16(&mut p, l.p_bgm, sd::clamp16(id));
            // Witch on the Holy Night keeps the MPLY volume as 255 x percent / 100 (jd_146E650)
            let scale = if crate::assets::witch() { 255.0 } else { 100.0 };
            put_i16(&mut p, l.p_bgm_vol, (vol * scale).round() as i16);
        }
        if let Some(w) = d.bgm_word {
            put_i16(&mut p, l.p_bgm_vol, w);
        }
        for se in &d.loops {
            let Ok(ch) = usize::try_from(se.channel) else { continue };
            let Some(id) = se_id(&se.name) else { continue };
            if ch < sd::SE_CHANNELS {
                put_i16(&mut p, l.p_se_id + 2 * ch, id);
                p[l.p_se_vol + ch] = (se.volume * 100.0).round().clamp(0.0, 255.0) as u8;
                p[l.p_se_loop + ch] = 1;
            }
        }
        // the loop flag words (+180) outlive their loops (SEFD clears only the loop id)
        for (ch, &f) in d.se_loops.iter().enumerate().take(sd::SE_CHANNELS) {
            p[l.p_se_loop + ch] |= u8::from(f);
        }
        // sub_5A910: every channel's volume word (its low byte) and pan
        for (ch, &w) in d.se_words.iter().enumerate().take(sd::SE_CHANNELS) {
            p[l.p_se_vol + ch] = w as u8;
        }
        for (ch, &w) in d.se_pans.iter().enumerate().take(sd::SE_CHANNELS) {
            put_i16(&mut p, l.p_se_pan + 2 * ch, w);
        }
        for q in &d.se_queue {
            let Ok(ch) = usize::try_from(q.channel) else { continue };
            if ch >= sd::SE_CHANNELS {
                continue;
            }
            let o = l.p_se_state + 20 * ch;
            put_i16(&mut p, o, q.frames);
            put_i16(&mut p, o + 2, q.name.as_deref().and_then(se_id).unwrap_or(-1));
            put_i32(&mut p, o + 4, q.fade);
            p[o + 8] = q.kind;
            p[o + 9] = q.volume as u8;
            if q.kind == 1 || q.kind == 4 {
                p[o + 11] = q.looping.into();
            }
            put_i16(&mut p, o + 12, q.pan);
            if q.kind == 4 {
                p[o + 10] = q.vol_end as u8;
                put_i16(&mut p, o + 14, q.pan_end);
            }
            put_i32(&mut p, o + 16, q.speaker);
        }
        let vars: HashMap<&str, i32> = d.vars.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        sd::write_f(&mut p, |n| sd::clamp16(vars.get(format!("F{n:03}").as_str()).copied().unwrap_or(0)));
        let stage = self.encode_stage(&d.layers, &d.kept, d.focus, d.scene);
        p[l.p_stage..l.p_stage + l.stage_size].copy_from_slice(&stage);
        let backup = d.backup.as_deref().filter(|b| b.len() == l.stage_size).unwrap_or(&stage);
        p[l.p_stage_prev..l.p_stage_prev + l.stage_size].copy_from_slice(backup);
        Some(p)
    }

    /// The snapshot a record restores (sub_82330 for 1010 records): the script resumes at the saved
    /// command, the saved F subset comes back as saved and all other F words read 0. F26 stays as saved:
    /// only the non-1010 path, which restarts the scene, sets it to the scene's id (0x82be0).
    pub fn decode_record(&mut self, rec: &[u8]) -> Option<SaveData> {
        let mut d = self.decode_page(sd::page_of(rec)?, sd::record_time(rec))?;
        d.prev = sd::prev_of(rec).and_then(|p| self.decode_page(&p, d.timestamp)).map(Box::new);
        Some(d)
    }

    /// The snapshot of one page-ring entry.
    fn decode_page(&mut self, p: &[u8], timestamp: u64) -> Option<SaveData> {
        let l = sd::lay();
        let scene = get_u16(p, sd::P_SCENE) as usize;
        let script = self.assets.scene(scene)?;
        // the exe goes on at the command after the record's offset (its `;`); a record of an earlier knife
        // version holds the command's own offset as that counted it (`Command::legacy_offset`)
        let target = get_i64(p, sd::P_OFFSET) + 1;
        let cmds = &script.commands;
        let pc = (!cmds.iter().any(|c| c.offset as i64 == target))
            .then(|| cmds.iter().position(|c| c.legacy_offset != c.offset && c.legacy_offset as i64 == target))
            .flatten()
            .or_else(|| cmds.iter().position(|c| c.offset as i64 >= target))?;
        let vars: Vec<(String, i32)> =
            sd::read_f(p).into_iter().filter(|&(_, v)| v != 0).map(|(n, v)| (format!("F{n:03}"), i32::from(v))).collect();
        let raw = &p[l.p_text..l.p_text + 0x200];
        let raw = String::from_utf8_lossy(&raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())]).into_owned();
        let preview = if raw.starts_with('$') {
            self.resolve_text(&raw)
        } else {
            String::new()
        };
        let bgm = get_i16(p, l.p_bgm);
        let names = se_names();
        let loops = (0..sd::SE_CHANNELS)
            .filter_map(|ch| {
                // sub_5D7E0 restarts a channel with a loop id (+100) and its loop flag (+180)
                if p[l.p_se_loop + ch] == 0 {
                    return None;
                }
                let id = usize::try_from(get_i16(p, l.p_se_id + 2 * ch)).ok()?;
                Some(SeSave { channel: ch as i32, name: names.get(id)?.to_string(), volume: f32::from(p[l.p_se_vol + ch]) / 100.0 })
            })
            .collect();
        // sub_5BAF0 reads the volume bytes back signed
        let se_words = (0..sd::SE_CHANNELS).map(|ch| i16::from(p[l.p_se_vol + ch] as i8)).collect();
        let se_pans = (0..sd::SE_CHANNELS).map(|ch| get_i16(p, l.p_se_pan + 2 * ch)).collect();
        let se_loops = (0..sd::SE_CHANNELS).map(|ch| p[l.p_se_loop + ch] != 0).collect();
        let se_queue = (0..sd::SE_CHANNELS)
            .filter_map(|ch| {
                let o = l.p_se_state + 20 * ch;
                let kind = p[o + 8];
                (kind != 0).then(|| SeQueued {
                    channel: ch as i32,
                    frames: get_i16(p, o),
                    kind,
                    name: usize::try_from(get_i16(p, o + 2)).ok().and_then(|i| names.get(i)).map(|n| n.to_string()),
                    fade: get_i32(p, o + 4),
                    volume: p[o + 9] as i8,
                    looping: p[o + 11] == 1,
                    pan: get_i16(p, o + 12),
                    speaker: get_i32(p, o + 16),
                    vol_end: p[o + 10] as i8,
                    pan_end: get_i16(p, o + 14),
                })
            })
            .collect();
        Some(SaveData {
            version: 1,
            scene,
            scene_name: self.assets.scene_name(scene).to_string(),
            pc,
            vars,
            layers: self.decode_stage(&p[l.p_stage..l.p_stage + l.stage_size]),
            kept: Self::decode_kept(&p[l.p_stage..l.p_stage + l.stage_size]),
            focus: p[l.p_stage + l.st.focus],
            backup: Some(p[l.p_stage_prev..l.p_stage_prev + l.stage_size].to_vec()),
            page_texts: Vec::new(),
            choice: None,
            bgm: (bgm >= 0).then(|| {
                let scale = if crate::assets::witch() { 255.0 } else { 100.0 };
                (i32::from(bgm), f32::from(get_i16(p, l.p_bgm_vol)) / scale)
            }),
            bgm_word: Some(get_i16(p, l.p_bgm_vol)),
            loops,
            timestamp,
            preview,
            prev: None,
            kind: match get_u16(p, l.p_type) {
                k @ (TYPE_ATSV | TYPE_ATSV_TEXT) => k,
                _ => 0,
            },
            voice: crate::voice_ids::name(get_i32(p, l.p_voice)).map(str::to_string),
            speaker: i16::from(p[sd::P_SPEAKER_LAST]),
            read_key: {
                let (i, s) = (get_i16(p, sd::P_READ_IDX), get_i16(p, sd::P_READ_SUB));
                (i > 0 || s > 0).then(|| (i.max(0) as usize, s as u8))
            },
            text: raw,
            se_words,
            se_loops,
            se_pans,
            se_queue,
        })
    }

    /// Stage block of a snapshot's layers (array layout in savedata::st).
    /// The current stage as a record's stage block.
    pub(crate) fn stage_bytes(&self) -> Vec<u8> {
        self.encode_stage(&self.layer_saves(), &self.stage.kept, self.focus_saved, self.scene_index)
    }

    /// Image table index and anchor (ImageInfo centre truncated toward zero) of a layer's image.
    /// Witch on the Holy Night's STCH (jp_60840) keeps the exact centre.
    fn image_anchor(&self, name: &str) -> (Option<usize>, (f32, f32)) {
        let idx = self.assets.image_index_of(name);
        (idx, idx.map_or((0.0, 0.0), |i| self.image_centre(i)))
    }

    fn image_centre(&self, idx: usize) -> (f32, f32) {
        let t = |v: f32| if crate::assets::witch() { v } else { v.trunc() };
        self.assets.tables.images.get(idx).map_or((0.0, 0.0), |im| (t(im.center_x), t(im.center_y)))
    }

    /// What STCC leaves of layer `i` (sub_62140): the negated anchor and blend of the layer it clears, or of
    /// whatever an earlier clear left there.
    pub(crate) fn kept_of(&self, i: usize) -> Kept {
        let Some(l) = self.stage.layers.get(i).and_then(|l| l.as_ref()) else {
            let k = self.stage.kept.get(i).copied().flatten();
            return Kept {
                neg_anchor: k.map_or((0, 0), |k| k.neg_anchor),
                blend: k.filter(|k| k.stcc).map_or(0xff, |k| k.blend),
                stcc: true,
                absolute: k.is_some_and(|k| k.absolute),
            };
        };
        let name = match &l.source {
            Source::Image(_) => l.name.clone(),
            Source::Color(c) => if *c == [1.0; 3] { "WHITE" } else { "BLACK" }.to_string(),
        };
        // the stage shift word (2923 / 2924): minus the anchor unless STPV or a motion pivot rewrote it (Witch on
        // the Holy Night's STCH never writes it: 0)
        let neg_anchor = match l.stage_shift {
            Some([x, y]) => (x as i16, y as i16),
            None if crate::assets::witch() => (0, 0),
            None => {
                let (_, (ax, ay)) = self.image_anchor(&name);
                (-(ax as i16), -(ay as i16))
            }
        };
        Kept { neg_anchor, blend: l.stage_blend().0 as i8 as u8, stcc: true, absolute: l.stage_absolute() }
    }

    /// What a stage reset (sub_57450) leaves of every layer: only the anchor (sub_3D70 starts past it).
    pub(crate) fn reset_leftovers(&self) -> Vec<Option<Kept>> {
        (0..sd::LAYERS)
            .map(|i| {
                let k = self.kept_of(i);
                (k.neg_anchor != (0, 0)).then_some(Kept { neg_anchor: k.neg_anchor, blend: 0xff, stcc: false, absolute: false })
            })
            .collect()
    }

    /// Empty layers' leftovers: STCC set alpha 255 (an untouched layer has 0); a reset leaves only the anchor.
    fn decode_kept(s: &[u8]) -> Vec<Option<Kept>> {
        let st = sd::st();
        (0..sd::LAYERS)
            .map(|l| {
                let e = st.extra + 18 * l;
                let neg_anchor = (get_i16(s, e + 2), get_i16(s, e + 4));
                let stcc = get_i16(s, st.alpha + 2 * l) == 255;
                let absolute = s[st.flags + l] & 0x40 != 0;
                (get_i16(s, st.image + 2 * l) < 0 && (stcc || absolute || neg_anchor != (0, 0)))
                    .then(|| Kept { neg_anchor, blend: if stcc { s[st.blend + l] } else { 0xff }, stcc, absolute })
            })
            .collect()
    }

    fn encode_stage(&self, layers: &[Option<LayerSave>], kept: &[Option<Kept>], focus: u8, scene: usize) -> Vec<u8> {
        let (st, witch) = (sd::st(), crate::assets::witch());
        let mut s = sd::empty_stage();
        // jp_48AC0 sets the backdrop colour word every story frame from the scene
        if witch {
            put_i16(&mut s, st.globals, if sd::witch_dark_scene(scene) { 0 } else { 1 });
        }
        s[st.focus] = focus;
        for (l, k) in kept.iter().enumerate().take(sd::LAYERS) {
            if let (Some(k), None) = (k, layers.get(l).and_then(|x| x.as_ref())) {
                if k.stcc {
                    put_i16(&mut s, st.alpha + 2 * l, 255);
                    s[st.blend + l] = k.blend;
                }
                // jp_62140 sets the STGS words to 255; jp_3C970 writes every slot's drawn alpha word each frame
                // from its render alpha, 1 after STCC and 0 after a reset (a slot that never showed anything)
                if k.stcc && let Some(f) = st.alpha_fade {
                    put_i16(&mut s, f + 2 * l, 255);
                    put_i16(&mut s, f + 80 + 2 * l, 255);
                }
                if k.absolute {
                    s[st.flags + l] |= 0x40;
                }
                put_i16(&mut s, st.extra + 18 * l + 2, k.neg_anchor.0);
                put_i16(&mut s, st.extra + 18 * l + 4, k.neg_anchor.1);
            }
        }
        for (l, ls) in layers.iter().enumerate().take(sd::LAYERS) {
            let Some(ls) = ls else { continue };
            let name = match (&ls.image, ls.color) {
                (Some(n), _) => n.clone(),
                (None, Some(c)) => if c == [1.0; 3] { "WHITE" } else { "BLACK" }.to_string(),
                (None, None) => continue,
            };
            let (idx, (ax, ay)) = self.image_anchor(&name);
            // STPV / motion pivots replace the anchor
            let (ax, ay) = ls.pivot.map_or((ax, ay), |[x, y]| (x, y));
            let w = |s: &mut [u8], o: usize, v: f32| put_i16(s, o + 2 * l, v.round().clamp(-32768.0, 32767.0) as i16);
            put_i16(&mut s, st.image + 2 * l, idx.map_or(-1, |i| i as i16));
            s[st.flags + l] = if ls.absolute { 0x40 } else { 0 };
            s[st.blend + l] = ls.stbr.unwrap_or(-1) as i8 as u8;
            put_f32(&mut s, st.x + 4 * l, ls.x * 0.5);
            put_f32(&mut s, st.y + 4 * l, ls.y * 2.0 / 3.0);
            // a running motion's alpha is its value at the saved whole frame, truncated (the epsilon keeps
            // n / 255 at n); knife's motion clock runs between frames
            let frame = ls.motion.map(|(id, t)| (id, (t * 60.0 / 1000.0).round() * 1000.0 / 60.0));
            let alpha = frame.and_then(|(id, t)| self.motions.alpha_at(id, t, ls.alpha * 255.0)).unwrap_or(ls.alpha * 255.0);
            w(&mut s, st.alpha, (alpha + 1.0e-3).trunc());
            put_i16(&mut s, st.prio + 2 * l, sd::clamp16(ls.prio));
            put_i32(&mut s, st.rot + 4 * l, (ls.rotation * 100.0).round() as i32);
            put_f32(&mut s, st.anchor_x + 4 * l, ax);
            put_f32(&mut s, st.anchor_y + 4 * l, ay);
            w(&mut s, st.zoom_x, ls.zoom_x * 1000.0);
            w(&mut s, st.zoom_y, ls.zoom_y * 1000.0);
            put_i16(&mut s, st.parent + 2 * l, ls.parent.map_or(-1, |p| p as i16));
            if let Some(b) = st.base {
                put_i16(&mut s, b + 2 * l, ls.base.map_or(-1, |b| b as i16));
            }
            put_i16(&mut s, st.rule + 2 * l, ls.rule.map_or(-1, |n| sd::clamp16(n - 1) | i16::from(ls.rule_flip & 7) << 12));
            if let Some([x, y, cw, ch, inset]) = ls.clip {
                for (k, v) in [x, y, cw, ch].into_iter().enumerate() {
                    put_i16(&mut s, st.clip + 8 * l + 2 * k, v.round() as i16);
                }
                w(&mut s, st.clip_inset, inset);
            }
            s[st.clip_mode + l] = ls.clip_mode as u8;
            put_i16(&mut s, st.rot2 + 2 * l, ls.rot2);
            put_i16(&mut s, st.soxy_x + 2 * l, ls.soxy.0);
            put_i16(&mut s, st.soxy_y + 2 * l, ls.soxy.1);
            if let Some((id, t)) = ls.motion {
                put_i16(&mut s, st.motion + 12 * l, sd::clamp16(id));
                put_i32(&mut s, st.motion + 12 * l + 4, (t * 60.0 / 1000.0).round() as i32);
            }
            if witch {
                // one slot: ax, ay, P, D, delay, starting D (40 words apart) and the type byte (save.rs packs
                // them in slot 0 as mode = type + 1)
                let (mode, delay, d, [ax, ay, p, e, ..]) = ls.quake[0];
                if mode >= 1 {
                    for (k, v) in [ax, ay, p, d, delay, e].into_iter().enumerate() {
                        put_i16(&mut s, st.wq + 80 * k + 2 * l, v);
                    }
                    s[st.wq_type + l] = mode - 1;
                }
                if let (Some(f), Some([from, cur, frames, ease])) = (st.alpha_fade, ls.wfade) {
                    // a running motion's alpha is the drawn one
                    let cur = if ls.motion.is_some() { (alpha + 1.0e-3).trunc() as i16 } else { cur };
                    put_i16(&mut s, f + 2 * l, from);
                    put_i16(&mut s, f + 80 + 2 * l, cur);
                    put_i16(&mut s, f + 160 + 2 * l, frames);
                    s[564 + l] = ease as u8;
                }
                put_f32(&mut s, st.tone + 16 * l + 8, ls.snx4[0]);
                put_f32(&mut s, st.tone + 16 * l + 12, ls.snx4[1]);
            }
            // quake slots (sub_63520): mode bytes +3604, delay +3684, remaining +3844, parameters +4004
            for (k, &(mode, delay, remaining, params)) in ls.quake.iter().enumerate() {
                if mode == 0 || witch {
                    continue;
                }
                s[st.qk_mode + 2 * l + k] = mode;
                put_i16(&mut s, st.qk_fade + 4 * l + 2 * k, delay);
                put_i16(&mut s, st.qk_dur + 4 * l + 2 * k, remaining);
                for (j, v) in params.into_iter().enumerate() {
                    put_i16(&mut s, st.qk_param + 24 * l + 12 * k + 2 * j, v);
                }
            }
            if let Some(n) = ls.noise {
                s[st.noise + l] = n as u8;
                // Witch on the Holy Night's sub_10640: 6 for 21 / 22 / 31 / 32, 9 for 11 / 12, else 3
                s[st.noise_mode + l] = match n {
                    _ if !witch => if n > 2 { 1 } else { 3 },
                    21 | 22 | 31 | 32 => 6,
                    11 | 12 => 9,
                    _ => 3,
                };
            }
            let e = st.extra + 18 * l;
            if ls.grade != 0 {
                put_i16(&mut s, st.effect + 2 * l, ls.grade as i16);
            }
            if ls.fill.is_some() {
                put_i16(&mut s, st.effect + 2 * l, 1);
            }
            if let Some(rgb) = ls.fill.or(ls.tint) {
                for (k, v) in rgb.into_iter().enumerate() {
                    s[e + 6 + k] = (v * 255.0).round() as u8;
                }
            }
            put_f32(&mut s, st.tone + 16 * l, ls.bright.unwrap_or(1.0));
            put_f32(&mut s, st.tone + 16 * l + 4, ls.contrast.unwrap_or(1.0));
            let [sx, sy] = ls.shift.unwrap_or(if witch { [0.0; 2] } else { [-ax.trunc(), -ay.trunc()] });
            put_i16(&mut s, e + 2, sx as i16);
            put_i16(&mut s, e + 4, sy as i16);
        }
        s
    }

    fn decode_stage(&self, s: &[u8]) -> Vec<Option<LayerSave>> {
        let (st, witch) = (sd::st(), crate::assets::witch());
        (0..sd::LAYERS)
            .map(|l| {
                let idx = usize::try_from(get_i16(s, st.image + 2 * l)).ok()?;
                let name = self.assets.tables.images.get(idx)?.name.clone();
                let (image, color) = match name.to_ascii_uppercase().as_str() {
                    "BLACK" => (None, Some([0.0; 3])),
                    "WHITE" => (None, Some([1.0; 3])),
                    _ => (Some(name), None),
                };
                let h = |o: usize| f32::from(get_i16(s, o + 2 * l));
                let opt = |o: usize| usize::try_from(get_i16(s, o + 2 * l)).ok();
                // Witch on the Holy Night's load sizes the layer from the STZ4 words alone (jp_31F30)
                let zoom = if witch { 1.0 } else { h(st.zoom) / 1000.0 };
                let quake = [0, 1].map(|k| {
                    if witch {
                        let w = |j: usize| get_i16(s, st.wq + 80 * j + 2 * l);
                        let (kind, words) = (s[st.wq_type + l], [w(0), w(1), w(2), w(3), w(4), w(5)]);
                        return match (k, kind != 0 || words != [0; 6]) {
                            (0, true) => (kind + 1, words[4], words[3], [words[0], words[1], words[2], words[5], 0, 0]),
                            _ => (0, 0, 0, [0; 6]),
                        };
                    }
                    let mode = s[st.qk_mode + 2 * l + k];
                    let params = [0, 1, 2, 3, 4, 5].map(|j| get_i16(s, st.qk_param + 24 * l + 12 * k + 2 * j));
                    match mode {
                        0 => (0, 0, 0, [0; 6]),
                        _ => (mode, get_i16(s, st.qk_fade + 4 * l + 2 * k), get_i16(s, st.qk_dur + 4 * l + 2 * k), params),
                    }
                });
                let e = st.extra + 18 * l;
                let clip = [0, 1, 2, 3].map(|k| f32::from(get_i16(s, st.clip + 8 * l + 2 * k)));
                let rot2 = get_i16(s, st.rot2 + 2 * l);
                let colour = [0, 1, 2].map(|k| f32::from(s[e + 6 + k]) / 255.0);
                let effect = get_i16(s, st.effect + 2 * l);
                let centre = self.image_centre(idx);
                let anchor = (get_f32(s, st.anchor_x + 4 * l), get_f32(s, st.anchor_y + 4 * l));
                let shift = (f32::from(get_i16(s, e + 2)), f32::from(get_i16(s, e + 4)));
                let motion = get_i16(s, st.motion + 12 * l);
                let blend = s[st.blend + l] as i8;
                Some(LayerSave {
                    image,
                    color,
                    x: get_f32(s, st.x + 4 * l) * 2.0,
                    y: get_f32(s, st.y + 4 * l) * 1.5,
                    zoom_x: zoom * h(st.zoom_x) / 1000.0,
                    zoom_y: zoom * h(st.zoom_y) / 1000.0,
                    alpha: h(st.alpha) / 255.0,
                    rotation: get_i32(s, st.rot + 4 * l) as f32 / 100.0,
                    prio: i32::from(get_i16(s, st.prio + 2 * l)),
                    parent: opt(st.parent),
                    base: st.base.and_then(opt),
                    absolute: s[st.flags + l] & 0x40 != 0,
                    quake,
                    rule: (get_i16(s, st.rule + 2 * l) >= 0).then(|| i32::from(get_i16(s, st.rule + 2 * l) & 0xfff) + 1),
                    rule_flip: (get_i16(s, st.rule + 2 * l).max(0) >> 12 & 7) as u8,
                    noise: (s[st.noise + l] > 0).then(|| i32::from(s[st.noise + l])),
                    blend: 0,
                    shade: 0,
                    fill: (effect == 1).then_some(colour),
                    grade: Some(i32::from(get_i16(s, st.effect + 2 * l))).filter(|&m| m >= 2).unwrap_or(0),
                    motion: (motion > 0).then(|| (i32::from(motion), get_i32(s, st.motion + 12 * l + 4) as f32 * 1000.0 / 60.0)),
                    clip: (clip[2] != 0.0 || clip[3] != 0.0).then(|| [clip[0], clip[1], clip[2], clip[3], h(st.clip_inset)]),
                    clip_mode: s[st.clip_mode + l] as i8,
                    bright: Some(get_f32(s, st.tone + 16 * l)),
                    contrast: Some(get_f32(s, st.tone + 16 * l + 4)),
                    stbr: (blend >= 0).then_some(i32::from(blend)),
                    rot2,
                    pivot: (anchor != centre).then_some([anchor.0, anchor.1]),
                    shift: (shift != if witch { (0.0, 0.0) } else { (-centre.0, -centre.1) }).then_some([shift.0, shift.1]),
                    // the stage colour is white for a layer that is not a fill unless a motion coloured it
                    tint: (effect != 1 && colour != [1.0; 3] && colour != [0.0; 3]).then_some(colour),
                    soxy: (get_i16(s, st.soxy_x + 2 * l), get_i16(s, st.soxy_y + 2 * l)),
                    snx4: [get_f32(s, st.tone + 16 * l + 8), get_f32(s, st.tone + 16 * l + 12)],
                    wfade: st.alpha_fade.map(|f| {
                        let w = |k: usize| get_i16(s, f + 80 * k + 2 * l);
                        [w(0), w(1), w(2), i16::from(s[564 + l])]
                    }),
                })
            })
            .collect()
    }

    // ---- system block --------------------------------------------------------------------------

    /// Take settings and persistent progress from the system block (sub_4EA30).
    /// Witch on the Holy Night's config block as sub_10130 applies it (and the config screen, row by row): the
    /// settings, SE / voice / movie as sub_FF70's curve, and the BGM level jp_48AC0 scales by its ducking
    /// (jp_47B40 forces that recompute).
    pub fn witch_config(&mut self, c: &[u8]) {
        self.settings = Settings::from_config(c);
        // the voice slider scales each row's volume (jp_6E0D0: sub_FF70(1, 255) x row / 255)
        let vol = &mut self.sound.vol;
        vol.se = witch_gain(get_i16(c, wcfg::SE), 102);
        vol.voice = sd::witch_curve(get_i16(c, wcfg::VOICE), 96) as f32 / 255.0;
        vol.movie = Some(witch_gain(get_i16(c, wcfg::MOVIE), 102));
        vol.bgm_mute = false;
        vol.se_mute = false;
        self.bgm_curve = sd::witch_curve(get_i16(c, wcfg::BGM), 102);
        self.duck_set = self.duck;
        self.sound.vol.bgm = witch_bgm_gain(self.duck, self.bgm_curve);
        self.apply_volumes();
    }

    /// jp_48AC0, once a 60 Hz frame: while a voice plays (jp_9C400) and BGM auto adjust is on (config 0x1C)
    /// the BGM level (dword_3CD650, of 256) falls by 4 to 128, else it rises by 2 to 256; a new level sets the
    /// BGM volume (jp_AD090).
    pub(crate) fn witch_duck(&mut self, dt: f32) {
        self.duck_ms += dt;
        while self.duck_ms >= 1000.0 / 60.0 {
            self.duck_ms -= 1000.0 / 60.0;
            let mut v = self.duck;
            if self.sound.voice_playing() && self.settings.bgm_auto {
                if v >= 129 {
                    v = v.max(132) - 4;
                }
            } else if v <= 255 {
                v = v.min(254) + 2;
            }
            self.duck = v;
            if self.duck_set != v {
                self.duck_set = v;
                self.sound.vol.bgm = witch_bgm_gain(v, self.bgm_curve);
                self.sound.apply_volumes();
            }
        }
    }

    pub fn load_system(&mut self, f: &SaveFile) {
        let c = f.config();
        self.settings = Settings::from_config(c);
        let witch = crate::assets::witch();
        if witch {
            self.witch_config(c);
        } else {
            let vol = &mut self.sound.vol;
            vol.bgm = f32::from(get_u16(c, cfg::BGM)) / 100.0;
            vol.se = f32::from(get_u16(c, cfg::SE)) / 100.0;
            vol.bgm_mute = self.settings.mute & 1 != 0;
            vol.se_mute = self.settings.mute & 2 != 0;
            self.apply_volumes();
        }
        self.vars.retain(|k, _| !k.starts_with('G'));
        for n in (0..sd::lay().g_words).filter(|&n| !Self::g_reserved(n)) {
            let v = f.g(n);
            if v != 0 {
                self.vars.insert(format!("G{n:03}"), i32::from(v));
            }
        }
        let seen = f.seen();
        self.seen.clear();
        self.heard.clear();
        for i in (0..seen.len() * 8).filter(|i| seen[i / 8] >> (i % 8) & 1 != 0) {
            match i.checked_sub(BGM_SEEN) {
                Some(n) => set_bit(&mut self.heard, n),
                None => set_bit(&mut self.seen, i),
            };
        }
        self.read.copy_from_slice(f.read_keys());
        if witch {
            return;
        }
        self.movie_flags = f.g(G_MOVIES) as u16;
        let bits = |first: usize, words: usize| {
            let mut v = Vec::new();
            for w in 0..words {
                let g = f.g(first + w) as u16;
                for b in (0..16).filter(|b| g >> b & 1 != 0) {
                    set_bit(&mut v, 16 * w + b);
                }
            }
            v
        };
        self.nodes = bits(G_NODES, NODE_WORDS);
        self.endings = bits(G_ENDINGS, NODE_WORDS);
        self.edges = bits(G_EDGES, EDGE_WORDS);
    }

    /// Write settings and persistent progress into the system block (sub_4EC60).
    pub fn store_system(&self, f: &mut SaveFile) {
        let c = f.config_mut();
        self.settings.to_config(c);
        let vol = &self.sound.vol;
        let witch = crate::assets::witch();
        // Witch on the Holy Night's sound levels live in the config block, which its config screen edits
        // (`witch_config` derives the volumes from it)
        if !witch {
            put_i16(c, cfg::BGM, (vol.bgm * 100.0).round() as i16);
            put_i16(c, cfg::BGM_COPY, (vol.bgm * 100.0).round() as i16);
            put_i16(c, cfg::SE, (vol.se * 100.0).round() as i16);
        }
        for n in (0..sd::lay().g_words).filter(|&n| !Self::g_reserved(n)) {
            f.set_g(n, sd::clamp16(self.vars.get(&format!("G{n:03}")).copied().unwrap_or(0)));
        }
        if !witch {
            f.set_g(G_MOVIES, self.movie_flags as i16);
            let mut put_bits = |first: usize, words: usize, bits: &[u64]| {
                for w in 0..words {
                    let v = (0..16).filter(|b| get_bit(bits, 16 * w + b)).fold(0u16, |a, b| a | 1 << b);
                    f.set_g(first + w, v as i16);
                }
            };
            put_bits(G_NODES, NODE_WORDS, &self.nodes);
            put_bits(G_ENDINGS, NODE_WORDS, &self.endings);
            put_bits(G_EDGES, EDGE_WORDS, &self.edges);
        }
        let seen = f.seen_mut();
        seen.fill(0);
        for i in 0..BGM_SEEN {
            if get_bit(&self.seen, i) {
                seen[i / 8] |= 1 << (i % 8);
            }
        }
        for n in (0..bgm_count()).filter(|&n| get_bit(&self.heard, n)) {
            let i = BGM_SEEN + n;
            seen[i / 8] |= 1 << (i % 8);
        }
        f.read_keys_mut().copy_from_slice(&self.read);
        put_i16(&mut f.buf, 2, 1);
    }

    /// Progress words that hold bitmaps, not script G variables.
    fn g_reserved(n: usize) -> bool {
        if crate::assets::witch() {
            return false;
        }
        n == G_MOVIES
            || (G_NODES..G_NODES + NODE_WORDS).contains(&n)
            || (G_ENDINGS..G_ENDINGS + NODE_WORDS).contains(&n)
            || (G_EDGES..G_EDGES + EDGE_WORDS).contains(&n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() {
        let s = Settings::from_config(&sd::CONFIG_DEFAULTS);
        assert_eq!(s.text_ms, [35.0, 15.0]);
        assert_eq!((s.auto_page_ms, s.auto_click_ms), (3000.0, 2200.0));
        assert!(!s.skip_unread && s.text_fade_in && s.voice_on && s.voice_stop_on_advance && s.char_mute == 0);
        let mut c = sd::CONFIG_DEFAULTS;
        s.to_config(&mut c);
        assert_eq!(c, sd::CONFIG_DEFAULTS);
    }

    #[test]
    fn variant_names() {
        assert!(is_variant("IMG1696_11_11"));
        assert!(!is_variant("IMG1696") && !is_variant("AOK_A_21_00_N01"));
    }
}
