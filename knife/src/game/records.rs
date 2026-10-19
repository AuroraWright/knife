//! Game state <-> the Switch save data (`savedata.rs`): save records (page-ring entries, sub_5A910 /
//! sub_5BAF0 / sub_82330) and the system block (sub_4EC60 / sub_4EA30).

use std::{collections::HashMap, sync::OnceLock};

use super::{Game, Settings, get_bit, read_key, set_bit};
use crate::{
    save::{Kept, LayerSave, SaveData, SeSave},
    stage::Source,
    save_tables::{SCENE_NODE, SE_NAMES, SEEN_ALIAS},
    savedata::{self as sd, SaveFile, cfg, get_f32, get_i16, get_i32, get_i64, get_u16, put_f32, put_i16, put_i32, put_i64, st},
    text,
};

/// Images below this table index have their own seen bit (sub_774F0); the rest go through SEEN_ALIAS.
pub(super) const SEEN_DIRECT: usize = 2701;
/// BGM n is heard at seen bit 3200 + n (MPLY sub_649E0, n <= 0x417).
const BGM_SEEN: usize = 3200;
const BGM_COUNT: usize = 1048;
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

fn alias_bits() -> &'static HashMap<&'static str, u16> {
    static MAP: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();
    MAP.get_or_init(|| SEEN_ALIAS.iter().copied().collect())
}

fn is_variant(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 6 && b[b.len() - 6] == b'_' && b[b.len() - 3] == b'_' && [1, 2, 4, 5].iter().all(|&k| b[b.len() - 6 + k].is_ascii_digit())
}

impl Settings {
    /// Settings from the exe's 104-byte config block (offsets in savedata::cfg).
    pub fn from_config(c: &[u8]) -> Self {
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
        }
    }

    /// Write these settings into a config block, keeping the fields knife does not use.
    pub fn to_config(&self, c: &mut [u8]) {
        let pw = |c: &mut [u8], o: usize, v: f32| put_i16(c, o, v.round().clamp(0.0, 32767.0) as i16);
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
    }
}

impl Game {
    /// Seen-bitmap bit of an image (sub_774F0 / sub_77610): expression variants count as their body, table
    /// indices below 2701 are their own bit, the rest map through the alias table (sub_77360).
    pub fn seen_bit(&self, name: &str) -> Option<usize> {
        let up = name.to_ascii_uppercase();
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
        let (kind, at) = if d.kind == TYPE_ATSV {
            (TYPE_ATSV, d.pc)
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
        let mut p = sd::empty_page();
        put_i64(&mut p, sd::P_OFFSET, c.offset as i64 - 1);
        let next = cmds.get(at + 1).map_or(c.offset + c.name.len() + 3, |n| n.offset);
        put_i64(&mut p, sd::P_NEXT, next as i64);
        put_i16(&mut p, sd::P_SCENE, d.scene as i16);
        put_i16(&mut p, sd::P_TYPE, kind as i16);
        // sub_5A910 marks every entry voiceless; sub_6E0D0 then gives a message its voice and speaker
        p[sd::P_SPEAKER_LAST] = d.speaker as u8;
        if kind == TYPE_MESSAGE {
            put_i32(&mut p, sd::P_VOICE, d.voice.as_deref().map_or(-1, crate::voice_ids::id));
            put_i16(&mut p, sd::P_VOICE_SPEAKER, d.speaker);
            p[sd::P_SPEAKER_NEXT] = d.speaker as u8;
        } else {
            put_i16(&mut p, sd::P_VOICE_SPEAKER, -1);
        }
        // sub_5A910 stores the current read key (off_AE0810 / off_AE0818) in every entry
        if let Some((i, s)) = d.read_key.or_else(|| (kind == TYPE_MESSAGE).then(|| read_key(&c.name)).flatten()) {
            put_i16(&mut p, sd::P_READ_IDX, i as i16);
            put_i16(&mut p, sd::P_READ_SUB, s.into());
        }
        if kind == TYPE_MESSAGE {
            let raw = c.args.first().map_or(&[][..], |a| a.as_bytes());
            let n = raw.len().min(0x1ff);
            p[sd::P_TEXT..sd::P_TEXT + n].copy_from_slice(&raw[..n]);
        }
        if let Some((id, vol)) = d.bgm {
            put_i16(&mut p, sd::P_BGM, sd::clamp16(id));
            put_i16(&mut p, sd::P_BGM_VOL, (vol * 100.0).round() as i16);
        }
        for se in &d.loops {
            let Ok(ch) = usize::try_from(se.channel) else { continue };
            let Some(id) = SE_NAMES.iter().position(|n| n.eq_ignore_ascii_case(&se.name)) else { continue };
            if ch < sd::SE_CHANNELS {
                put_i16(&mut p, sd::P_SE_ID + 2 * ch, id as i16);
                p[sd::P_SE_VOL + ch] = (se.volume * 100.0).round().clamp(0.0, 255.0) as u8;
                p[sd::P_SE_LOOP + ch] = 1;
            }
        }
        let vars: HashMap<&str, i32> = d.vars.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        sd::write_f(&mut p, |n| sd::clamp16(vars.get(format!("F{n:03}").as_str()).copied().unwrap_or(0)));
        let stage = self.encode_stage(&d.layers, &d.kept);
        p[sd::P_STAGE..sd::P_STAGE + sd::STAGE_SIZE].copy_from_slice(&stage);
        let backup = d.backup.as_deref().filter(|b| b.len() == sd::STAGE_SIZE).unwrap_or(&stage);
        p[sd::P_STAGE_PREV..sd::P_STAGE_PREV + sd::STAGE_SIZE].copy_from_slice(backup);
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
        let scene = get_u16(p, sd::P_SCENE) as usize;
        let script = self.assets.scene(scene)?;
        let target = get_i64(p, sd::P_OFFSET) + 1;
        let pc = script.commands.iter().position(|c| c.offset as i64 >= target)?;
        let mut vars: Vec<(String, i32)> =
            sd::read_f(p).into_iter().filter(|&(_, v)| v != 0).map(|(n, v)| (format!("F{n:03}"), i32::from(v))).collect();
        // knife's edge marking starts from the node of the scene being resumed (sub_92D00)
        if let Some(&n) = scene.checked_sub(1).and_then(|i| SCENE_NODE.get(i))
            && n >= 0
        {
            vars.push(("~LAST".into(), i32::from(n) + 1));
        }
        let raw = &p[sd::P_TEXT..sd::P_TEXT + 0x200];
        let raw = String::from_utf8_lossy(&raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())]).into_owned();
        let preview = if get_u16(p, sd::P_TYPE) == TYPE_MESSAGE && raw.starts_with('$') {
            text::parse_markup(&self.resolve_text(&raw))
                .into_iter()
                .filter_map(|r| match r {
                    text::Run::Text(t) => Some(t),
                    text::Run::Ruby { base, .. } => Some(base),
                    text::Run::Newline => None,
                })
                .collect()
        } else {
            String::new()
        };
        let bgm = get_i16(p, sd::P_BGM);
        let loops = (0..sd::SE_CHANNELS)
            .filter_map(|ch| {
                let id = usize::try_from(get_i16(p, sd::P_SE_ID + 2 * ch)).ok()?;
                Some(SeSave { channel: ch as i32, name: SE_NAMES.get(id)?.to_string(), volume: f32::from(p[sd::P_SE_VOL + ch]) / 100.0 })
            })
            .collect();
        Some(SaveData {
            version: 1,
            scene,
            scene_name: self.assets.scene_name(scene).to_string(),
            pc,
            vars,
            layers: self.decode_stage(&p[sd::P_STAGE..sd::P_STAGE + sd::STAGE_SIZE]),
            kept: Self::decode_kept(&p[sd::P_STAGE..sd::P_STAGE + sd::STAGE_SIZE]),
            backup: Some(p[sd::P_STAGE_PREV..sd::P_STAGE_PREV + sd::STAGE_SIZE].to_vec()),
            page_texts: Vec::new(),
            choice: None,
            bgm: (bgm >= 0).then(|| (i32::from(bgm), f32::from(get_i16(p, sd::P_BGM_VOL)) / 100.0)),
            loops,
            timestamp,
            preview,
            blur: false,
            prev: None,
            kind: if get_u16(p, sd::P_TYPE) == TYPE_ATSV { TYPE_ATSV } else { 0 },
            voice: crate::voice_ids::name(get_i32(p, sd::P_VOICE)).map(str::to_string),
            speaker: i16::from(p[sd::P_SPEAKER_LAST]),
            read_key: {
                let (i, s) = (get_i16(p, sd::P_READ_IDX), get_i16(p, sd::P_READ_SUB));
                (i > 0 || s > 0).then(|| (i.max(0) as usize, s as u8))
            },
        })
    }

    /// Stage block of a snapshot's layers (array layout in savedata::st).
    /// The current stage as a record's stage block.
    pub(crate) fn stage_bytes(&self) -> Vec<u8> {
        self.encode_stage(&self.layer_saves(), &self.stage.kept)
    }

    /// Image table index and anchor (ImageInfo centre truncated toward zero) of a layer's image.
    fn image_anchor(&self, name: &str) -> (Option<usize>, (f32, f32)) {
        let idx = self.assets.image_index_of(name);
        let a = idx.and_then(|i| self.assets.tables.images.get(i)).map_or((0.0, 0.0), |im| (im.center_x.trunc(), im.center_y.trunc()));
        (idx, a)
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
            };
        };
        let name = match &l.source {
            Source::Image(_) => l.name.clone(),
            Source::Color(c) => if *c == [1.0; 3] { "WHITE" } else { "BLACK" }.to_string(),
        };
        let (_, (ax, ay)) = self.image_anchor(&name);
        Kept { neg_anchor: (-(ax as i16), -(ay as i16)), blend: l.stbr as i8 as u8, stcc: true }
    }

    /// What a stage reset (sub_57450) leaves of every layer: only the anchor (sub_3D70 starts past it).
    pub(crate) fn reset_leftovers(&self) -> Vec<Option<Kept>> {
        (0..sd::LAYERS)
            .map(|i| {
                let k = self.kept_of(i);
                (k.neg_anchor != (0, 0)).then_some(Kept { neg_anchor: k.neg_anchor, blend: 0xff, stcc: false })
            })
            .collect()
    }

    /// Empty layers' leftovers: STCC set alpha 255 (an untouched layer has 0); a reset leaves only the anchor.
    fn decode_kept(s: &[u8]) -> Vec<Option<Kept>> {
        (0..sd::LAYERS)
            .map(|l| {
                let e = st::EXTRA + 18 * l;
                let neg_anchor = (get_i16(s, e + 2), get_i16(s, e + 4));
                let stcc = get_i16(s, st::ALPHA + 2 * l) == 255;
                (get_i16(s, st::IMAGE + 2 * l) < 0 && (stcc || neg_anchor != (0, 0)))
                    .then(|| Kept { neg_anchor, blend: if stcc { s[st::BLEND + l] } else { 0xff }, stcc })
            })
            .collect()
    }

    fn encode_stage(&self, layers: &[Option<LayerSave>], kept: &[Option<Kept>]) -> Vec<u8> {
        let mut s = sd::empty_stage();
        for (l, k) in kept.iter().enumerate().take(sd::LAYERS) {
            if let (Some(k), None) = (k, layers.get(l).and_then(|x| x.as_ref())) {
                if k.stcc {
                    put_i16(&mut s, st::ALPHA + 2 * l, 255);
                    s[st::BLEND + l] = k.blend;
                }
                put_i16(&mut s, st::EXTRA + 18 * l + 2, k.neg_anchor.0);
                put_i16(&mut s, st::EXTRA + 18 * l + 4, k.neg_anchor.1);
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
            let w = |s: &mut [u8], o: usize, v: f32| put_i16(s, o + 2 * l, v.round().clamp(-32768.0, 32767.0) as i16);
            put_i16(&mut s, st::IMAGE + 2 * l, idx.map_or(-1, |i| i as i16));
            s[st::FLAGS + l] = if ls.absolute { 0x40 } else { 0 };
            s[st::BLEND + l] = ls.stbr.unwrap_or(-1) as i8 as u8;
            put_f32(&mut s, st::X + 4 * l, ls.x * 0.5);
            put_f32(&mut s, st::Y + 4 * l, ls.y * 2.0 / 3.0);
            // a running motion's alpha is its value at the saved whole frame, truncated (the epsilon keeps
            // n / 255 at n); knife's motion clock runs between frames
            let frame = ls.motion.map(|(id, t)| (id, (t * 60.0 / 1000.0).round() * 1000.0 / 60.0));
            let alpha = frame.and_then(|(id, t)| self.motions.alpha_at(id, t)).unwrap_or(ls.alpha * 255.0);
            w(&mut s, st::ALPHA, (alpha + 1.0e-3).trunc());
            put_i16(&mut s, st::PRIO + 2 * l, sd::clamp16(ls.prio));
            put_i32(&mut s, st::ROT + 4 * l, (ls.rotation * 100.0).round() as i32);
            put_f32(&mut s, st::ANCHOR_X + 4 * l, ax);
            put_f32(&mut s, st::ANCHOR_Y + 4 * l, ay);
            w(&mut s, st::ZOOM_X, ls.zoom_x * 1000.0);
            w(&mut s, st::ZOOM_Y, ls.zoom_y * 1000.0);
            put_i16(&mut s, st::PARENT + 2 * l, ls.parent.map_or(-1, |p| p as i16));
            put_i16(&mut s, st::BASE + 2 * l, ls.base.map_or(-1, |b| b as i16));
            put_i16(&mut s, st::RULE + 2 * l, ls.rule.map_or(-1, |n| sd::clamp16(n - 1)));
            if let Some([x, y, cw, ch, rot]) = ls.clip {
                for (k, v) in [x, y, cw, ch].into_iter().enumerate() {
                    put_i16(&mut s, st::CLIP + 8 * l + 2 * k, v.round() as i16);
                }
                w(&mut s, st::CLIP_ROT, rot * 10.0);
            }
            w(&mut s, st::ROT2, ls.motion_rot * 100.0);
            if let Some((id, t)) = ls.motion {
                put_i16(&mut s, st::MOTION + 12 * l, sd::clamp16(id));
                put_i32(&mut s, st::MOTION + 12 * l + 4, (t * 60.0 / 1000.0).round() as i32);
            }
            if let Some([a0, a1, p0, p1, strength, left]) = ls.sway {
                // SSW2 (sub_91CF0 -> sub_63520 mode 3, slot 1): amplitudes, periods and duration in frames
                s[st::QK_MODE + 2 * l + 1] = 3;
                let dur = if left < 0.0 { -1.0 } else { left * 3.0 / 50.0 };
                put_i16(&mut s, st::QK_DUR + 4 * l + 2, dur.round() as i16);
                for (k, v) in [a0, a1, p0 * 3.0 / 50.0, p1 * 3.0 / 50.0, strength].into_iter().enumerate() {
                    put_i16(&mut s, st::QK_PARAM + 24 * l + 12 + 2 * k, v.round() as i16);
                }
            }
            if let Some(([ax, ay, left, _], [period, dur, fade, a6, a7])) = ls.quake {
                // SQK2 (sub_91540 -> sub_63520 mode 1, slot 0); arguments in ms become frames (3 / 50)
                s[st::QK_MODE + 2 * l] = 1;
                put_i16(&mut s, st::QK_FADE + 4 * l, sd::clamp16(3 * fade / 50));
                put_i16(&mut s, st::QK_DUR + 4 * l, if left < 0.0 { -1 } else { (left * 3.0 / 50.0).round() as i16 });
                let frames = match period {
                    i32::MIN => 2,
                    p if p < 0 => 1,
                    p if 3 * p + 49 < 99 => 2,
                    p => 3 * p / 50 + 1,
                };
                let total = if dur >= 0 && 3 * a7 <= 49 { 3 * dur / 50 } else { 0 };
                for (k, v) in [ax.round() as i32, ay.round() as i32, frames, 3 * a7 / 50, total, a6].into_iter().enumerate() {
                    put_i16(&mut s, st::QK_PARAM + 24 * l + 2 * k, sd::clamp16(v));
                }
            }
            if let Some(n) = ls.noise {
                s[st::NOISE + l] = n as u8;
                s[st::NOISE_MODE + l] = if n > 2 { 1 } else { 3 };
            }
            let e = st::EXTRA + 18 * l;
            if ls.grade != 0 {
                put_i16(&mut s, st::EFFECT + 2 * l, ls.grade as i16);
            }
            if let Some(rgb) = ls.fill {
                put_i16(&mut s, st::EFFECT + 2 * l, 1);
                for (k, v) in rgb.into_iter().enumerate() {
                    s[e + 6 + k] = (v * 255.0).round() as u8;
                }
            }
            put_f32(&mut s, st::TONE + 16 * l, ls.bright.unwrap_or(1.0));
            put_i16(&mut s, e + 2, -(ax as i16));
            put_i16(&mut s, e + 4, -(ay as i16));
        }
        s
    }

    fn decode_stage(&self, s: &[u8]) -> Vec<Option<LayerSave>> {
        (0..sd::LAYERS)
            .map(|l| {
                let idx = usize::try_from(get_i16(s, st::IMAGE + 2 * l)).ok()?;
                let name = self.assets.tables.images.get(idx)?.name.clone();
                let (image, color) = match name.to_ascii_uppercase().as_str() {
                    "BLACK" => (None, Some([0.0; 3])),
                    "WHITE" => (None, Some([1.0; 3])),
                    _ => (Some(name), None),
                };
                let h = |o: usize| f32::from(get_i16(s, o + 2 * l));
                let opt = |o: usize| usize::try_from(get_i16(s, o + 2 * l)).ok();
                let zoom = h(st::ZOOM) / 1000.0;
                let q = st::QK_PARAM + 24 * l + 12;
                let sway = (s[st::QK_MODE + 2 * l + 1] == 3).then(|| {
                    let p = |k: usize| f32::from(get_i16(s, q + 2 * k));
                    let dur = f32::from(get_i16(s, st::QK_DUR + 4 * l + 2));
                    [p(0), p(1), p(2) * 50.0 / 3.0, p(3) * 50.0 / 3.0, p(4), if dur < 0.0 { -1.0 } else { dur * 50.0 / 3.0 }]
                });
                let q0 = st::QK_PARAM + 24 * l;
                let quake = (s[st::QK_MODE + 2 * l] == 1).then(|| {
                    let p = |k: usize| i32::from(get_i16(s, q0 + 2 * k));
                    let dur = get_i16(s, st::QK_DUR + 4 * l);
                    let ms = |f: i32| f * 50 / 3;
                    let period = match p(2) {
                        f if f <= 1 => -1,
                        2 => 0,
                        f => ms(f - 1),
                    };
                    let total = if dur < 0 { -1 } else if p(4) > 0 { ms(p(4)) } else { ms(i32::from(dur)) };
                    let left = if dur < 0 { -1.0e10 } else { f32::from(dur) * 50.0 / 3.0 };
                    let fade = ms(i32::from(get_i16(s, st::QK_FADE + 4 * l)));
                    ([p(0) as f32, p(1) as f32, left, 0.0], [period, total, fade, p(5), ms(p(3))])
                });
                let e = st::EXTRA + 18 * l;
                let clip = [0, 1, 2, 3].map(|k| f32::from(get_i16(s, st::CLIP + 8 * l + 2 * k)));
                let rot2 = get_i16(s, st::ROT2 + 2 * l);
                let motion = get_i16(s, st::MOTION + 12 * l);
                let blend = s[st::BLEND + l] as i8;
                Some(LayerSave {
                    image,
                    color,
                    x: get_f32(s, st::X + 4 * l) * 2.0,
                    y: get_f32(s, st::Y + 4 * l) * 1.5,
                    zoom_x: zoom * h(st::ZOOM_X) / 1000.0,
                    zoom_y: zoom * h(st::ZOOM_Y) / 1000.0,
                    alpha: h(st::ALPHA) / 255.0,
                    rotation: get_i32(s, st::ROT + 4 * l) as f32 / 100.0,
                    prio: i32::from(get_i16(s, st::PRIO + 2 * l)),
                    parent: opt(st::PARENT),
                    base: opt(st::BASE),
                    absolute: s[st::FLAGS + l] & 0x40 != 0,
                    sway,
                    quake,
                    rule: (get_i16(s, st::RULE + 2 * l) >= 0).then(|| i32::from(get_i16(s, st::RULE + 2 * l) & 0xfff) + 1),
                    noise: (s[st::NOISE + l] > 0).then(|| i32::from(s[st::NOISE + l])),
                    blend: 0,
                    shade: 0,
                    fill: (get_i16(s, st::EFFECT + 2 * l) == 1).then(|| [0, 1, 2].map(|k| f32::from(s[e + 6 + k]) / 255.0)),
                    grade: Some(i32::from(get_i16(s, st::EFFECT + 2 * l))).filter(|&m| m >= 2).unwrap_or(0),
                    motion: (motion > 0).then(|| (i32::from(motion), get_i32(s, st::MOTION + 12 * l + 4) as f32 * 1000.0 / 60.0)),
                    clip: (clip[2] != 0.0 || clip[3] != 0.0).then(|| [clip[0], clip[1], clip[2], clip[3], h(st::CLIP_ROT) / 10.0]),
                    bright: Some(get_f32(s, st::TONE + 16 * l)),
                    stbr: (blend >= 0).then_some(i32::from(blend)),
                    // STRR -99999 is stored as 0x8000 and means no rotation
                    motion_rot: if rot2 == i16::MIN { 0.0 } else { f32::from(rot2) / 100.0 },
                })
            })
            .collect()
    }

    // ---- system block --------------------------------------------------------------------------

    /// Take settings and persistent progress from the system block (sub_4EA30).
    pub fn load_system(&mut self, f: &SaveFile) {
        let c = f.config();
        self.settings = Settings::from_config(c);
        let vol = &mut self.sound.vol;
        vol.bgm = f32::from(get_u16(c, cfg::BGM)) / 100.0;
        vol.se = f32::from(get_u16(c, cfg::SE)) / 100.0;
        vol.bgm_mute = self.settings.mute & 1 != 0;
        vol.se_mute = self.settings.mute & 2 != 0;
        self.sound.apply_volumes();
        self.vars.retain(|k, _| !k.starts_with('G'));
        for n in (0..sd::G_WORDS).filter(|&n| !Self::g_reserved(n)) {
            let v = f.g(n);
            if v != 0 {
                self.vars.insert(format!("G{n:03}"), i32::from(v));
            }
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
        let seen = f.seen();
        self.seen.clear();
        self.heard.clear();
        for i in (0..sd::SEEN_BYTES * 8).filter(|i| seen[i / 8] >> (i % 8) & 1 != 0) {
            match i.checked_sub(BGM_SEEN) {
                Some(n) => set_bit(&mut self.heard, n),
                None => set_bit(&mut self.seen, i),
            };
        }
        self.read.copy_from_slice(f.read_keys());
    }

    /// Write settings and persistent progress into the system block (sub_4EC60).
    pub fn store_system(&self, f: &mut SaveFile) {
        let c = f.config_mut();
        self.settings.to_config(c);
        let vol = &self.sound.vol;
        put_i16(c, cfg::BGM, (vol.bgm * 100.0).round() as i16);
        put_i16(c, cfg::BGM_COPY, (vol.bgm * 100.0).round() as i16);
        put_i16(c, cfg::SE, (vol.se * 100.0).round() as i16);
        for n in (0..sd::G_WORDS).filter(|&n| !Self::g_reserved(n)) {
            f.set_g(n, sd::clamp16(self.vars.get(&format!("G{n:03}")).copied().unwrap_or(0)));
        }
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
        let seen = f.seen_mut();
        seen.fill(0);
        for i in 0..BGM_SEEN {
            if get_bit(&self.seen, i) {
                seen[i / 8] |= 1 << (i % 8);
            }
        }
        for n in (0..BGM_COUNT).filter(|&n| get_bit(&self.heard, n)) {
            let i = BGM_SEEN + n;
            seen[i / 8] |= 1 << (i % 8);
        }
        f.read_keys_mut().copy_from_slice(&self.read);
        put_i16(&mut f.buf, 2, 1);
    }

    /// Progress words that hold bitmaps, not script G variables.
    fn g_reserved(n: usize) -> bool {
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
