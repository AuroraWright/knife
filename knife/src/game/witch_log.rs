//! Witch on the Holy Night's backlog entries (the UI is `ui::witch::backlog`): sub_A180 lists the page ring's
//! message entries (type +36 == 0) newest first, sub_A2A0 / sub_A700 lay each out in a text slot from its
//! record (+14016 the text, +13264 the voice, +13952 the name, F1105 the window type), and state 9 restores the
//! quick jump's entry like a rewind (jp_5BAF0).

use knife_render::{Quad, Renderer, TexRef};

use super::{Back, Game, UX, UY, entry_f823, resolve_refs, voice_row};
use crate::text::{self, Layout};

/// The preview key of the quick jump's thumbnail (jp_89590 / jp_89AD0 draw the entry's stage into the
/// thumbnail surface, as the save list's previews).
const JUMP_PREVIEW: usize = usize::MAX - 1;

/// A Witch on the Holy Night text slot's jp_AB4D0 parameters (`Game::witch_slot_layout`).
#[derive(Clone, Copy)]
pub struct WitchSlot {
    /// the box width in 640-wide units
    pub width: f32,
    /// the glyph scales (a9, a10)
    pub kx: f32,
    pub ky: f32,
    /// the line spacing (a8), px of 1080 less 14
    pub a8: f32,
    /// flag 0x100000: the bubble font (slot +101793, font variant 3) and line spacing 8 px less
    pub bubble: bool,
    /// `@z` in the text (`Game::slot_z`): the bubble font too, and Some(true) 8 px less again
    pub z: Option<bool>,
    /// the ruby's extra y (sub_861A0)
    pub ruby_y: f32,
}

/// A slot's glyphs with the line of each.
pub struct WitchPlaced {
    pub chars: Vec<text::PlacedChar>,
    rows: Vec<i32>,
    /// the lines the text takes (jp_A8780)
    pub lines: i32,
}

/// A backlog entry (dword_6379D0 lists them by ring slot).
#[derive(Clone, Debug)]
pub struct LogEntry {
    /// the ring entry's serial
    pub serial: u64,
    pub text: String,
    /// +13264 >= 0: a voice the entry replays (A, or a tap)
    pub voice: Option<String>,
    /// quick-jumpable (sub_A2A0: F823 == 1, +13272 == 0 (not taken during a story movie or the staff roll)
    /// and a type other than 8 / 11)
    pub jump: bool,
    /// F1105, the message window type: 3 and up are bubbles (sub_1ADD0)
    pub window: i32,
}

impl Game {
    /// sub_A180's count: the ring's message entries.
    pub fn witch_log_len(&self) -> usize {
        self.history.iter().filter(|h| h.kind == 0).count()
    }

    /// sub_A180: the ring's message entries, newest first.
    pub fn witch_log(&self) -> Vec<LogEntry> {
        self.history
            .iter()
            .rev()
            .filter(|h| h.kind == 0)
            .map(|h| {
                let d = &h.data;
                LogEntry {
                    serial: h.serial,
                    text: d.text.clone(),
                    voice: d.voice.clone(),
                    jump: entry_f823(d) == 1 && !h.locked && !matches!(h.kind, 8 | 11),
                    window: d.vars.iter().find(|(k, _)| k == "F1105").map_or(0, |v| v.1),
                }
            })
            .collect()
    }

    /// sub_A700 on A (or a tap on the list): the entry's voice at its speaker row's volume (AE0350 + 9 =
    /// sub_FF70(1, 255) x config row / 255), both voice players stopped first (jp_AA9F0, jp_AA970), while
    /// config 0x36 (the voices) is on.
    pub fn witch_log_voice(&mut self, name: &str) {
        if self.settings.voice_on {
            self.play_test_voice(name, voice_row(name));
        }
    }

    /// jp_AA9F0: the replayed voice stops (the backlog closing over it).
    pub fn stop_voice(&mut self) {
        self.sound.stop_voice();
    }

    /// The quick jump's thumbnail of entry `serial` (jp_89590 sets it up, jp_89AD0 draws it, ready a frame
    /// or more later), None until drawn.
    pub fn witch_log_preview(&mut self, serial: u64, fresh: bool) -> Option<TexRef> {
        if fresh {
            self.clear_previews(Some(JUMP_PREVIEW));
        }
        let d = self.history.iter().find(|h| h.serial == serial)?.data.clone();
        self.preview(JUMP_PREVIEW, &d)
    }

    /// State 9: the quick jump restores the entry (jp_5BAF0(entry, 0), sub_3E570, sub_6A7B0), as Tsukihime's
    /// sub_28D50 does. False when the entry has left the ring.
    pub fn witch_log_jump(&mut self, r: &Renderer, serial: u64) -> bool {
        let Some(i) = self.history.iter().position(|h| h.serial == serial) else {
            return false;
        };
        self.rewind_to(r, i, Back::Jump);
        true
    }

    /// An entry's text in its slot (sub_A700 -> jp_AB4D0, drawn by jp_AB710 at (x, y) of 640x448 at `alpha`):
    /// glyphs 60 x (0.65, 0.702) of W / 32 (English 0.9 x (0.57, 0.6588)), character spacing (0 + 4) and line
    /// spacing (-6 + 14) px of 1080, 392 units wide (English 448); a bubble entry (sub_1ADD0: window type 3
    /// and up, flag 0x100000) takes the bubble font, line spacing 8 px less (jp_A7790: -14 + 6) and, in
    /// Japanese, its type's width (sub_1AE20) x 0.87838. Five lines or more are laid out again with line
    /// spacing (-10 + 14) at 0.56 x 0.588 (English 0.75 of the x scale). The ruby rides 6.75 px lower
    /// (sub_861A0(30..37, -6.75)), the edge follows the config, the colour is white (config 0x2A is never 2,
    /// and the speaker table off_632380 is all 0xFFFF anyway). The name slot (40 + k, record +13952) stays
    /// empty: nothing writes the record's name.
    pub fn witch_log_quads(&mut self, r: &Renderer, e: &LogEntry, panel: usize, x: f32, y: f32, alpha: f32) -> Vec<Quad> {
        let en = text::english();
        let bubble = e.window >= 3;
        let width = match usize::try_from(e.window - 3).ok().and_then(|t| crate::witch::BUBBLE_WIDTH.get(t)) {
            _ if en => 448.0,
            Some(&w) => ((w * 0.87838) as i32) as f32,
            None => 392.0,
        };
        let (kx, ky) = if en { (0.57, 0.6588) } else { (0.65, 0.702) };
        let text = resolve_refs(&self.assets, &e.text);
        // panel i is text slot 30 + i
        let z = self.slot_z(30 + panel, &text);
        let slot = WitchSlot { width, kx, ky, a8: -6.0, bubble, z, ruby_y: -6.75 };
        let mut placed = self.witch_slot_layout(&text, x, y, slot);
        // jp_A8780: the slot's line count
        if placed.lines >= 5 {
            let (kx, ky) = if en { (kx * 0.75, ky) } else { (0.56, 0.588) };
            placed = self.witch_slot_layout(&text, x, y, WitchSlot { kx, ky, a8: -10.0, ..slot });
        }
        self.witch_slot_quads(r, &placed, bubble || z.is_some(), &e.text, usize::MAX, alpha)
    }

    /// `@z` in the text set in text slot `slot` (jp_BDC2C, `Game::z_latch`): Some(whether the slot's line
    /// spacing took the 8 px) when `text` has the code. jp_A7790 lays a slot's text out again only when it
    /// changed (sub_97558), so a slot reads its latch once per new text.
    pub fn slot_z(&mut self, slot: usize, text: &str) -> Option<bool> {
        if let Some((t, z)) = self.slot_texts.get(&slot)
            && t == text
        {
            return *z;
        }
        let z = text.contains("@z").then(|| {
            let bit = 1u64 << slot.min(63);
            let fresh = self.z_latch & bit == 0;
            self.z_latch |= bit;
            fresh
        });
        self.slot_texts.insert(slot, (text.to_string(), z));
        z
    }

    /// A text slot of Witch on the Holy Night laid out as jp_AB4D0 -> jp_A7790 does at (x, y) of 640x448: the
    /// glyphs W / 32 (English x 0.9) at the slot's scales, character spacing (0 + 4) px of 1080, the box
    /// (int)(W x width / 640) px wide.
    pub fn witch_slot_layout(&mut self, s: &str, x: f32, y: f32, slot: WitchSlot) -> WitchPlaced {
        let px = |v: f32| v * super::CANVAS_H as f32 / 1080.0;
        let bubble_font = (slot.bubble || slot.z.is_some()) && self.font_bubble.is_some();
        let size = 60.0 * text::glyph_scale() * slot.ky;
        let left = x * UX;
        let mut ascent = text::msg_ascent((text::JA_ASCENT + 1.0) * size / text::BASE_SIZE / text::glyph_scale());
        if bubble_font {
            ascent *= text::bubble_ascent();
        }
        // flag 0x100000: line spacing a8 - 14 + 6; `@z` once more the first time
        let a8 = slot.a8 - if slot.bubble { 8.0 } else { 0.0 } - if slot.z == Some(true) { 8.0 } else { 0.0 };
        let lay = Layout {
            left,
            right: left + ((super::CANVAS_W as f32 * slot.width / 640.0) as i32) as f32,
            size,
            spacing: 4.0 * super::CANVAS_W as f32 / 1920.0,
            sx: slot.kx / slot.ky,
            line_height: if bubble_font { text::bubble_line_feed() } else { text::line_feed() } * size / text::BASE_SIZE + px(a8 + 14.0),
            ascent,
            ruby_y: slot.ruby_y,
        };
        let runs = text::parse_markup(s);
        let font = match &mut self.font_bubble {
            Some(f) if bubble_font => f,
            _ => &mut self.font,
        };
        let top = y * UY + lay.ascent;
        let mut placed = Vec::new();
        text::layout(font, &lay, &runs, lay.left, top, 0.0, 0, &mut placed);
        let line = |c: &text::PlacedChar| ((c.baseline - top) / lay.line_height).round() as i32;
        let lines = placed.iter().filter(|c| c.size == lay.size).map(line).max().map_or(0, |n| n + 1);
        let rows = placed.iter().map(line).collect();
        WitchPlaced { chars: placed, rows, lines }
    }

    /// The glyphs of a laid out slot (`witch_slot_layout`) on its first `lines` lines (jp_A8330), white: the
    /// edge follows the config, an edge code in `text` colours the rim and a fill code the glyphs
    /// (`text::fill_rgb`).
    pub fn witch_slot_quads(&mut self, r: &Renderer, p: &WitchPlaced, bubble: bool, text: &str, lines: usize, alpha: f32) -> Vec<Quad> {
        let edge = self.settings.text_edge;
        let rim = text::text_edge(text);
        let shown: Vec<&text::PlacedChar> = p.chars.iter().zip(&p.rows).filter(|(_, row)| **row < i32::try_from(lines).unwrap_or(i32::MAX)).map(|(c, _)| c).collect();
        let font = match &mut self.font_bubble {
            Some(f) if bubble => f,
            _ => &mut self.font,
        };
        let mut out = Vec::new();
        // the bars keep the float alpha (`text::slot_alpha`)
        let glyph_a = text::slot_alpha(alpha);
        for pc in &shown {
            let [cr, cg, cb] = text::fill_rgb(pc, [1.0; 3]);
            if edge {
                out.extend(font.rimmed_quad_edge(r, pc, 1, [cr, cg, cb], rim, glyph_a));
            } else {
                out.extend(font.placed_quad(r, pc, 0.0, 0.0, [cr, cg, cb, glyph_a]));
            }
        }
        out.extend(shown.iter().filter_map(|pc| self.bar_quad(pc, None, [1.0, 1.0, 1.0, alpha], edge)));
        out
    }
}
