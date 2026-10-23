//! Font rasterisation into an R8 atlas, and NVL page layout.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use knife_core::format::bffnt;
use knife_core::linebreak::LineBreak;
use knife_render::{Blend, Quad, Renderer, Shade, TexRef};

const ATLAS: u32 = 2048;
const PAD: u32 = 2;

#[derive(Clone, Copy)]
struct Glyph {
    /// atlas rect in pixels
    rect: [u32; 4],
    /// offset of the bitmap's top-left from the pen position on the baseline
    off: [f32; 2],
}

/// exfont_nx01.bffnt, the bitmap font sub_CAE88 pairs with the message font (nn::font PairFont), and its sheet
/// as an upright R8 texture.
struct ExFont {
    font: bffnt::Font,
    tex: TexRef,
}

/// The game language (sub_2DE70): 0 Japanese, 1 English, 2 / 3 Simplified / Traditional Chinese. The text
/// engine's slot setup (sub_A7790) reads it for every slot.
static LANGUAGE: AtomicUsize = AtomicUsize::new(0);
/// nn::font's line breaker over the executable's tables (sub_148988), for English text.
static LINE_BREAK: OnceLock<Arc<LineBreak>> = OnceLock::new();

pub fn set_language(language: usize) {
    LANGUAGE.store(language, Ordering::Relaxed);
}

pub fn set_line_break(lb: Arc<LineBreak>) {
    let _ = LINE_BREAK.set(lb);
}

pub fn language() -> usize {
    LANGUAGE.load(Ordering::Relaxed)
}

/// English text (sub_2DF20)
pub fn english() -> bool {
    language() == 1
}

/// The edge width (glyph cache px) of each font index of a text slot (+55436, `jp_C1074`): the font manager
/// builds the message and system glyph caches (sub_CA500 / sub_CAA50) as four sub-sets with outline 0, 2,
/// 3 and 4, and `jp_CBC68` picks the sub-set by the index. sub_A7790 gives index 1 to a slot with flag
/// 0x200000 (2 with 0x2000000 too); otherwise it keeps the index set when the slot was made (1 while the
/// text edge setting, 0x46B080 + 0x14, is on).
pub const OUTLINE: [f32; 4] = [0.0, 2.0, 3.0, 4.0];

/// The rim shader's taps per cache px of edge and their alpha (`FontAtlas::rim`), fitted on the overseas Switch's
/// message text, Japanese and English pages (mean abs 4.10 -> 3.22 and 6.37 -> 5.43 against fixed 2 px taps at
/// 0.55; dark pixels 12175 / 4628 against the Switch's 14593 / 4633).
pub const RIM_TAP: f32 = 1.1;
pub const RIM_ALPHA: f32 = 0.65;

/// The rim width (`FontAtlas::rim`) of font index `index` at glyph height `px`: `OUTLINE[index]` cache px scaled
/// from the 40 px base.
pub fn edge_rim(index: usize, px: f32) -> f32 {
    OUTLINE[index] * RIM_TAP * px / BASE_SIZE
}

/// sub_A7790: every text slot's glyph size is W / 32, times 0.9 for English (overseas build).
pub fn glyph_scale() -> f32 {
    if english() { 0.9 } else { 1.0 }
}

/// The message font's first baseline below a slot's y, from the offset `jp` the Japanese fonts give at the
/// Japanese glyph size: it scales with the glyph (0.9 for English) and with the pair font's ascent at the 40 px
/// base (`knife_core::format::sfnt::pair_metrics`; Japanese 41) plus `GLYPH_DROP`. Switch (Astris, sub-pixel
/// fits of the prologue's first page and the config previews): baselines 42.03 / 44.07 / 39.89 / 39.95 base px
/// below the line box for ascents 41 / 43 / 39 / 39 (Japanese, English, Simplified and Traditional Chinese).
pub fn msg_ascent(jp: f32) -> f32 {
    jp * glyph_scale() * ascent_ratio(MSG_ASCENT.load(Ordering::Relaxed))
}

/// The bubble font's (`<folder>3`) baseline against the message font's at the same glyph size: its own pair
/// font's ascent (English: FOT-Seurat's 41 against the message set's 43).
pub fn bubble_ascent() -> f32 {
    ascent_ratio(BUBBLE_ASCENT.load(Ordering::Relaxed)) / ascent_ratio(MSG_ASCENT.load(Ordering::Relaxed))
}

fn ascent_ratio(ascent: usize) -> f32 {
    (ascent as f32 + GLYPH_DROP) / (JA_ASCENT + GLYPH_DROP)
}

/// How far a glyph's ink baseline sits below the writer's cursor, in 40 px cache pixels at the writer's scale:
/// nn::font's CharWriter puts the cell at cursor - (baselineDifference + the font's GetBaselinePos), and the
/// glyph cache's cell baseline (sub_143070: (int)(rec[6] + round(rec[2] * 40) * rec[5])) is one pixel below the
/// font's (Switch: the message baselines above, the ruby below).
const GLYPH_DROP: f32 = 1.0;

/// The Japanese message fonts' pair ascent at 40 px (FOT-Seurat's 1029 of 1000), which `game::ASCENT` is.
pub const JA_ASCENT: f32 = 41.0;
static MSG_ASCENT: AtomicUsize = AtomicUsize::new(41);
static MSG_BASELINE: AtomicUsize = AtomicUsize::new(41);
static BUBBLE_ASCENT: AtomicUsize = AtomicUsize::new(41);

/// The fonts' base size (the font manager's +124, from sub_AB0F0's init struct): glyph scale = size / 40.
pub const BASE_SIZE: f32 = 40.0;
/// The pair font's (exfont + the system shared font at the base size) width, in base px: the shared font's 50
/// (the exfont's is 41). Switch: the 3- and 6-dash runs 140.0 / 285.9 px wide (the 0xD stretch gives 141.6 /
/// 286.4 with 50).
pub const PAIR_METRIC: f32 = 50.0;

/// The message pair font's line feed at the base size (`knife_core::format::sfnt::pair_metrics` over the
/// language's three fonts and the exfont), in base px: 50 for Japanese (Switch: line pitch 50 * 1.14 + 24 = 81
/// px), 53 for English (Switch: choice rows 78.4 px apart, 53 * 1.026 + 24), 49 / 47 for Simplified /
/// Traditional Chinese.
static LINE_FEED: AtomicUsize = AtomicUsize::new(50);

/// The language's font metrics: the message pair font's line feed, ascent and baseline position and the bubble
/// pair font's ascent, in base px.
pub fn set_font_metrics(line_feed: i32, msg_ascent: i32, msg_baseline: i32, bubble_ascent: i32) {
    LINE_FEED.store(line_feed.max(1) as usize, Ordering::Relaxed);
    MSG_ASCENT.store(msg_ascent.max(0) as usize, Ordering::Relaxed);
    MSG_BASELINE.store(msg_baseline.max(0) as usize, Ordering::Relaxed);
    BUBBLE_ASCENT.store(bubble_ascent.max(0) as usize, Ordering::Relaxed);
}

pub fn line_feed() -> f32 {
    LINE_FEED.load(Ordering::Relaxed) as f32
}

/// An exfont cell's top above the line's baseline, in base-size px. Switch (Astris): the dash ink centre
/// (cell row 23) 18.35 px above the baseline at 45.6 px, so the cell's glyph baseline (TGLP 37) sits 0.8
/// base px above the line's.
const EX_TOP: f32 = 37.8;

pub struct FontAtlas {
    font: FontArc,
    /// the rest of the language's font list (`add_fallback`)
    fallback: Vec<FontArc>,
    /// the font file, for the GPOS pair kerning ab_glyph does not read (`kern`)
    data: Arc<Vec<u8>>,
    gpos_kern: std::cell::RefCell<HashMap<(u16, u16), i16>>,
    ex: Option<ExFont>,
    pub tex: TexRef,
    glyphs: HashMap<(char, u32), Option<Glyph>>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

impl FontAtlas {
    pub fn new(r: &Renderer, mut data: Vec<u8>) -> anyhow::Result<Self> {
        // the glyphs with bytes after their endchar, which nn::fontll draws (`cff::normalize`)
        knife_core::format::cff::normalize(&mut data);
        let font = FontArc::try_from_vec(data.clone())?;
        let tex = r.create_writable(ATLAS, ATLAS, true, "font atlas");
        Ok(Self {
            font,
            fallback: Vec::new(),
            data: Arc::new(data),
            gpos_kern: Default::default(),
            ex: None,
            tex,
            glyphs: HashMap::new(),
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
        })
    }

    /// Load the exfont (`exfont_nx01.bffnt`) for the run glyphs.
    pub fn load_exfont(&mut self, r: &Renderer, data: &[u8]) -> anyhow::Result<()> {
        let font = bffnt::Font::parse(data)?;
        let (w, h) = (font.sheet.width, font.sheet.height);
        let rgba = knife_core::format::bntx_decode::to_rgba8(&font.sheet)?;
        // one channel, rows flipped upright (the sheet is stored bottom-up)
        let mut r8 = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                r8[(y * w + x) as usize] = rgba[(((h - 1 - y) * w + x) * 4) as usize];
            }
        }
        let tex = r.create_writable(w, h, true, "exfont");
        r.write_region(&tex, [0, 0, w, h], 1, &r8);
        self.ex = Some(ExFont { font, tex });
        Ok(())
    }

    pub fn has_ex(&self, c: char) -> bool {
        self.ex.as_ref().is_some_and(|e| e.font.glyph(c).is_some())
    }

    /// Add the next font of the language's font list: sub_CA500 gives each of the message glyph cache's
    /// faces the language's three font files as one nn::font font list, and the cache takes a glyph from the
    /// first font that has it (English: Helvetica Neue has no U+3000 or ■, which come from FOT-Seurat, en3).
    pub fn add_fallback(&mut self, mut data: Vec<u8>) -> anyhow::Result<()> {
        knife_core::format::cff::normalize(&mut data);
        self.fallback.push(FontArc::try_from_vec(data)?);
        Ok(())
    }

    /// The font of the list that has `c`, else the first.
    fn face(&self, c: char) -> &FontArc {
        if self.font.glyph_id(c).0 != 0 {
            return &self.font;
        }
        self.fallback.iter().find(|f| f.glyph_id(c).0 != 0).unwrap_or(&self.font)
    }

    /// A single exfont glyph (the pair font's first font, sub_CAE88, so the inline glyph tags' characters come
    /// from it): its cell scaled by size / 40 at the pen plus its left bearing, the cell top `EX_TOP` above the
    /// baseline. The edged set (`EX_TAGS`) carries its own border at half coverage, which nn::font draws in the
    /// edge colour (Switch, QA bubble `[heart]`: a white heart in a black rim; fs_text's bordered mode).
    fn ex_quad(&self, c: char, size: f32, (x, baseline): (f32, f32), color: [f32; 4]) -> Option<Quad> {
        let ex = self.ex.as_ref()?;
        let g = ex.font.glyph(c)?;
        let sx = size / BASE_SIZE;
        let (tw, th) = (ex.font.sheet.width as f32, ex.font.sheet.height as f32);
        let ch = ex.font.cell_h as f32;
        let q = Quad::rect(ex.tex.clone(), [x + f32::from(g.left) * sx, baseline - EX_TOP * sx, f32::from(g.width) * sx, ch * sx], color)
            .with_uv_rect(g.x as f32 / tw, g.y as f32 / th, (g.x as f32 + f32::from(g.width)) / tw, (g.y as f32 + ch) / th);
        let bordered = if EX_TAGS.iter().any(|t| t.2 == c) { 1.0 } else { 0.0 };
        Some(Quad { blend: Blend::Alpha, shade: Shade::Text, haze: [[0.0; 4], [0.0, 0.0, bordered, 1.0]], ..q })
    }

    /// Quad for a placed char (a font glyph, or an exfont glyph stretched over a run) offset by (dx, dy).
    pub fn placed_quad(&mut self, r: &Renderer, pc: &PlacedChar, dx: f32, dy: f32, color: [f32; 4]) -> Option<Quad> {
        match pc.run {
            Some((n, spacing)) => self.run_quad(pc.c, (n, spacing), pc.size, (pc.x + dx, pc.baseline + dy), color),
            None if self.has_ex(pc.c) => self.ex_quad(pc.c, pc.size, (pc.x + dx, pc.baseline + dy), color),
            None => self.quad_stretched(r, pc.c, pc.size, (pc.x + dx, pc.baseline + dy), pc.stretch, color),
        }
    }

    /// The placed char as one white glyph over its black rim (`Shade::RimText`): the quad grows by the
    /// `rim` px the taps reach, each tap at `rim_alpha`, and `alpha` fades the composite once.
    pub fn rimmed_quad(&mut self, r: &Renderer, pc: &PlacedChar, rim: f32, rim_alpha: f32, alpha: f32) -> Option<Quad> {
        let q = self.placed_quad(r, pc, 0.0, 0.0, [1.0, 1.0, 1.0, alpha])?;
        // a single exfont glyph is a sheet bitmap, not a glyph of the outlined cache sub-set: no rim of its own
        if pc.run.is_none() && self.has_ex(pc.c) {
            return Some(q);
        }
        Some(Self::rim(q, rim, rim_alpha))
    }

    /// A glyph quad grown by `rim` px and drawn with `Shade::RimText`: the shader's taps (2 px orthogonal,
    /// 1.5 px diagonal) scaled to `rim` / 2.
    pub fn rim(q: Quad, rim: f32, rim_alpha: f32) -> Quad {
        let ([x0, y0], [x1, y1]) = (q.pos[0], q.pos[2]);
        let ([u0, v0], [u1, v1]) = (q.uv[0], q.uv[2]);
        let (du, dv) = ((u1 - u0) / (x1 - x0), (v1 - v0) / (y1 - y0));
        let k = rim / 2.0;
        let q = Quad::rect(q.tex.clone(), [x0 - rim, y0 - rim, x1 - x0 + 2.0 * rim, y1 - y0 + 2.0 * rim], q.color)
            .with_uv_rect(u0 - rim * du, v0 - rim * dv, u1 + rim * du, v1 + rim * dv);
        Quad { shade: Shade::RimText, haze: [[u0, v0, u1, v1], [du * k, dv * k, rim_alpha, 0.0]], ..q }
    }

    /// An exfont glyph stretched over n cells (nn::font tag 0xD in sub_DD334): x scale
    /// ((W n + sp (n - 1)) sx / W) * 0.965 (the overseas build's sub_DB650: 0.98) with W the pair font's
    /// scaled width (sub_13BCF0) and sp the character spacing,
    /// centred over the n cells the pen crosses (Switch: run centres at pen + n * 49.6 / 2). The overseas
    /// tag processor centres only while its +92 is set, which the writers (sub_C5FB4, sub_C406C, sub_BF0F8)
    /// set to "not English": English runs start at the pen (Switch, Seurat bubble "■■■■ ■■■■■.": the bars'
    /// left edges at the pen within half a pixel, 175 / 219 px wide).
    fn run_quad(&self, c: char, (n, spacing): (u8, f32), size: f32, (x, baseline): (f32, f32), color: [f32; 4]) -> Option<Quad> {
        let ex = self.ex.as_ref()?;
        let g = ex.font.glyph(c)?;
        let n = n as f32;
        let sx = size / BASE_SIZE;
        let w = PAIR_METRIC * sx;
        let stretch = (w * n + spacing * (n - 1.0)) * sx / w * if crate::assets::overseas() { 0.98 } else { 0.965 };
        let qw = g.width as f32 * stretch;
        let cells = if english() { qw } else { n * (size + spacing) };
        let qh = ex.font.cell_h as f32 * sx;
        let (tw, th) = (ex.font.sheet.width as f32, ex.font.sheet.height as f32);
        let u0 = (g.x as f32 + g.left.max(0) as f32) / tw;
        let u1 = u0 + g.width as f32 / tw;
        let q = Quad::rect(ex.tex.clone(), [x + (cells - qw) / 2.0, baseline - EX_TOP * sx, qw, qh], color)
            .with_uv_rect(u0, g.y as f32 / th, u1, (g.y as f32 + ex.font.cell_h as f32) / th);
        // haze[1][3] = 1: fs_text box-filters the sheet over each pixel's footprint (the sheet's hard 2-texel
        // dash drawn below its 40 px base size)
        Some(Quad { blend: Blend::Alpha, shade: Shade::Text, haze: [[0.0; 4], [0.0, 0.0, 0.0, 1.0]], ..q })
    }

    /// nn::font sizes a scalable font by its em (Switch, English: Helvetica Neue's caps 27 px tall in a 37.8 px
    /// dialog slot = 0.714 em, 29 px in the 40 px config preview); ab_glyph's PxScale is ascent - descent, which
    /// is the em for the Japanese and shared fonts (1000 / 1024) but 1.207 em for Helvetica Neue, 1.331 for
    /// Demos Next and 1.337 for SungASC.
    fn px(font: &FontArc, size: f32) -> PxScale {
        let em = font.units_per_em().unwrap_or(1000.0);
        PxScale::from(size * font.height_unscaled() / em)
    }

    /// A glyph's advance: nn::font caches a scalable font's glyphs at the base size with whole-pixel char
    /// widths and scales those (Switch, English lines: 3 / 7 / 9 px narrower than fractional advances over
    /// 650 / 760 / 870 px, rounding at 40 px gives 2.5 / 5.7 / 6.3).
    /// An exfont glyph advances by its CWDH advance at size / 40.
    pub fn advance(&self, c: char, size: f32) -> f32 {
        if let Some(g) = self.ex.as_ref().and_then(|e| e.font.glyph(c)) {
            return f32::from(g.advance) * size / BASE_SIZE;
        }
        let font = self.face(c);
        let base = font.as_scaled(Self::px(font, BASE_SIZE)).h_advance(font.glyph_id(c));
        base.round() * size / BASE_SIZE
    }

    /// The font's kerning between `a` and `b` (nn::font adds it before `b`): the `kern` table, else the GPOS
    /// `kern` feature's pair adjustments (Helvetica Neue has only those; Switch, English lines: 4 / 1 / 3 px
    /// narrower over 870 / 760 / 650 px, as its GPOS kerning gives). Like the advances, in whole pixels at the
    /// base size (Switch, FOT-Seurat bubble: "with a Dead End. This time it was our heroine." has +18 / +2
    /// units of kerning, under half a pixel each at 40 px, and is 0.8 px narrower than with them).
    /// Pairs across fonts (the exfont, a fallback font) have none.
    pub fn kern(&self, a: Option<char>, b: char, size: f32) -> f32 {
        a.filter(|&a| ![a, b].iter().any(|&c| self.has_ex(c) || self.font.glyph_id(c).0 == 0)).map_or(0.0, |a| {
            let (ga, gb) = (self.font.glyph_id(a), self.font.glyph_id(b));
            let k = self.font.as_scaled(Self::px(&self.font, BASE_SIZE)).kern(ga, gb);
            let base = if k != 0.0 {
                k
            } else {
                let units = *self.gpos_kern.borrow_mut().entry((ga.0, gb.0)).or_insert_with(|| self.gpos_pair(ga.0, gb.0));
                f32::from(units) * BASE_SIZE / self.font.units_per_em().unwrap_or(1000.0)
            };
            base.round() * size / BASE_SIZE
        })
    }

    /// The x advance adjustment of the first glyph of a pair in the GPOS `kern` feature (pair adjustment
    /// lookups, the first subtable that covers the pair).
    fn gpos_pair(&self, a: u16, b: u16) -> i16 {
        use ttf_parser::gpos::{PairAdjustment, PositioningSubtable};
        let Ok(face) = ttf_parser::Face::parse(&self.data, 0) else { return 0 };
        let Some(gpos) = face.tables().gpos else { return 0 };
        let (a, b) = (ttf_parser::GlyphId(a), ttf_parser::GlyphId(b));
        let mut lookups: Vec<u16> =
            gpos.features.into_iter().filter(|f| &f.tag.to_bytes() == b"kern").flat_map(|f| f.lookup_indices).collect();
        lookups.sort_unstable();
        lookups.dedup();
        for li in lookups {
            let Some(lookup) = gpos.lookups.get(li) else { continue };
            for st in lookup.subtables.into_iter::<PositioningSubtable>() {
                let PositioningSubtable::Pair(pair) = st else { continue };
                let found = match pair {
                    PairAdjustment::Format1 { coverage, sets } => {
                        coverage.get(a).and_then(|i| sets.get(i)).and_then(|set| set.get(b)).map(|(v, _)| v.x_advance)
                    }
                    PairAdjustment::Format2 { coverage, classes, matrix } => coverage
                        .get(a)
                        .and_then(|_| matrix.get((classes.0.get(a), classes.1.get(b))))
                        .map(|(v, _)| v.x_advance),
                };
                if let Some(k) = found {
                    return k;
                }
            }
        }
        0
    }

    fn reset(&mut self) {
        self.glyphs.clear();
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_h = 0;
    }

    /// The glyph as nn::font's cache holds it: rasterized at the base size (font manager +124, 40 px) with its
    /// box on whole pixels, which the draw scales to the slot size.
    fn glyph(&mut self, r: &Renderer, c: char) -> Option<Glyph> {
        let key = (c, (BASE_SIZE * 4.0) as u32);
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let font = self.face(c);
        let id = font.glyph_id(c);
        let outlined = font.outline_glyph(id.with_scale_and_position(Self::px(font, BASE_SIZE), ab_glyph::point(0.0, 0.0)));
        let g = outlined.map(|o| {
            let b = o.px_bounds();
            let (w, h) = (b.width().ceil() as u32 + 1, b.height().ceil() as u32 + 1);
            if self.shelf_x + w + PAD > ATLAS {
                self.shelf_x = 0;
                self.shelf_y += self.shelf_h + PAD;
                self.shelf_h = 0;
            }
            if self.shelf_y + h + PAD > ATLAS {
                // atlas full: start over (glyphs already queued this frame stay valid
                // until overwritten, which is acceptable for a rare event)
                self.reset();
            }
            let (x, y) = (self.shelf_x, self.shelf_y);
            self.shelf_x += w + PAD;
            self.shelf_h = self.shelf_h.max(h);
            let mut buf = vec![0u8; (w * h) as usize];
            o.draw(|px, py, cov| {
                let i = (py * w + px) as usize;
                if i < buf.len() {
                    buf[i] = (cov.clamp(0.0, 1.0) * 255.0) as u8;
                }
            });
            r.write_region(&self.tex, [x, y, w, h], 1, &buf);
            Glyph {
                rect: [x, y, w, h],
                off: [b.min.x, b.min.y],
            }
        });
        let g = g.or(Some(Glyph {
            rect: [0, 0, 0, 0],
            off: [0.0, 0.0],
        }));
        self.glyphs.insert(key, g);
        g
    }

    /// Quad for one glyph with its pen at (x, baseline).
    pub fn quad(&mut self, r: &Renderer, c: char, size: f32, x: f32, baseline: f32, color: [f32; 4]) -> Option<Quad> {
        self.quad_stretched(r, c, size, (x, baseline), 1.0, color)
    }

    /// A glyph quad with its x scaled by `sx` about the middle of its em (Switch, train page ……: the stretched
    /// dots stay centred on the unstretched glyph).
    fn quad_stretched(&mut self, r: &Renderer, c: char, size: f32, (x, baseline): (f32, f32), sx: f32, color: [f32; 4]) -> Option<Quad> {
        let g = self.glyph(r, c)?;
        if g.rect[2] == 0 {
            return None;
        }
        let [gx, gy, gw, gh] = g.rect;
        let a = ATLAS as f32;
        let k = size / BASE_SIZE;
        let q = Quad::rect(
            self.tex.clone(),
            [x + g.off[0] * k * sx - (sx - 1.0) * size / 2.0, baseline + g.off[1] * k, gw as f32 * k * sx, gh as f32 * k],
            color,
        )
        .with_uv_rect(gx as f32 / a, gy as f32 / a, (gx + gw) as f32 / a, (gy + gh) as f32 / a);
        Some(Quad {
            blend: Blend::Alpha,
            shade: Shade::Text,
            ..q
        })
    }

    /// Draw a text slot line (sub_AB4D0 -> sub_A7790): `w` x `h` cells from the pen at `x`, `spacing` after each;
    /// returns the advance. sub_AB4D0 turns each double space into U+3000. For Japanese (+101784, sub_C3698)
    /// sub_BDC2C wraps ASCII runs in tags 0x10 / 0x11, and sub_DD0D8 case 0x10 sets the writer's fixed width to
    /// half (+100928 = 0.5, sub_C36AC), its character space to half (+100932 = 0.5, sub_C36D8) and its x scale to
    /// 0.725: each glyph is centred in its fixed cell (w, or w / 2 for ASCII). Switch (music panel, system
    /// font): kana 24 + 4 px apart, ASCII 12 + 2 px with the narrow glyphs centred.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_str_slot(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: [f32; 2], spacing: f32, pos: [f32; 2], color: [f32; 4]) -> f32 {
        self.slot_line(r, out, s, size, spacing, pos, color, None, english())
    }

    /// `draw_str_slot` with the text engine's dark rim (`rimmed_quad`; the slots share sub_A7790 with the
    /// messages) of the slot's font index (`OUTLINE`): the glyph caches render each font at the base size
    /// (font manager +124, 40 px), so the edge is `OUTLINE[index]` cache px scaled to the glyph height.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_str_slot_rimmed(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: [f32; 2], spacing: f32, pos: [f32; 2], color: [f32; 4], index: usize) -> f32 {
        let rim = edge_rim(index, size[1] * glyph_scale());
        self.slot_line(r, out, s, size, spacing, pos, color, Some(rim), english())
    }

    #[allow(clippy::too_many_arguments)]
    /// `free`: the writer's +289 (English or flag 0x10000): own advances and kerning, no fixed cells.
    fn slot_line(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, [w, h]: [f32; 2], spacing: f32, [x, baseline]: [f32; 2], color: [f32; 4], rim: Option<f32>, free: bool) -> f32 {
        let (w, h) = (w * glyph_scale(), h * glyph_scale());
        let mut pen = x;
        let mut prev = None;
        let chars: Vec<char> = slot_chars(s).collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            // the writer parses slot text like the messages (sub_BD7EC): a dash run is one exfont glyph over
            // its n cells (tag 0xD)
            if let Some(g) = run_kind(c).filter(|&g| g == EX_DASH && self.has_ex(g)) {
                let n = 1 + chars[i..].iter().take_while(|&&d| run_kind(d) == Some(g)).count().min(254);
                i += n - 1;
                if let Some(q) = self.run_quad(g, (n as u8, spacing), w, (pen, baseline), color) {
                    out.push(match rim {
                        Some(rim) => Self::rim(q, rim, RIM_ALPHA),
                        None => q,
                    });
                }
                // English: no spacing after the run, as in the messages
                pen += n as f32 * (w + spacing) - if free { spacing } else { 0.0 };
                continue;
            }
            if free {
                pen += self.kern(prev.replace(c), c, h);
            }
            let (cell, sx) = self.slot_cell(c, w, h, free);
            let gx = pen + (cell - self.advance(c, h) * sx) / 2.0;
            // quad_stretched centres its stretch on the em; shift it back so the glyph starts at gx
            if let Some(q) = self.quad_stretched(r, c, h, (gx + (sx - 1.0) * h / 2.0, baseline), sx, color) {
                out.push(match rim {
                    Some(rim) => Self::rim(q, rim, RIM_ALPHA),
                    None => q,
                });
            }
            pen += cell + if free { spacing } else { slot_space(c, spacing) };
        }
        pen - x
    }

    /// Width of a text slot line: the advances plus the spacing between glyphs.
    pub fn measure_slot(&self, s: &str, [w, h]: [f32; 2], spacing: f32) -> f32 {
        let (w, h) = (w * glyph_scale(), h * glyph_scale());
        let chars: Vec<char> = slot_chars(s).collect();
        let last = chars.last().map_or(0.0, |&c| slot_space(c, spacing));
        let mut prev = None;
        chars
            .iter()
            .map(|&c| {
                let (cell, _) = self.slot_cell(c, w, h, english());
                let k = if english() { self.kern(prev.replace(c), c, h) } else { 0.0 };
                k + cell + slot_space(c, spacing)
            })
            .sum::<f32>()
            - last
    }

    /// Split a text slot paragraph into the lines its writer shows in a `width` px box: a line ends before
    /// the character that would run past it; English breaks at nn::font's line break opportunities instead
    /// (sub_BF0F8 / sub_148988), a line keeping its trailing spaces.
    pub fn wrap_slot(&self, s: &str, size: [f32; 2], spacing: f32, width: f32) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        let mut lines = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let fits = |end: usize| self.measure_slot(&chars[start..=end].iter().collect::<String>(), size, spacing) <= width;
            let overflow = (start + 1..chars.len()).find(|&j| !fits(j));
            let last = match (overflow, english().then(|| LINE_BREAK.get()).flatten()) {
                (None, _) => chars.len() - 1,
                (Some(o), Some(lb)) => lb.last_on_line(&chars, start, Some(o)),
                (Some(o), None) => o - 1,
            };
            lines.push(chars[start..=last].iter().collect());
            start = last + 1;
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }

    /// A text slot glyph's cell width and x scale. Japanese and Chinese (+101784 set, sub_C3698): sub_BDC2C
    /// wraps ASCII runs in tags 0x10 / 0x11, and sub_DD0D8 case 0x10 halves the writer's fixed width (+100928
    /// = 0.5) at 0.725 of the x scale; other glyphs take the full fixed cell `w`. English (the overseas build
    /// clears it): no ASCII runs and no fixed width, each glyph its own advance.
    fn slot_cell(&self, c: char, w: f32, h: f32, free: bool) -> (f32, f32) {
        let k = w / h;
        if free {
            (self.advance(c, h) * k, k)
        } else if (' '..='~').contains(&c) {
            (w * 0.5, k * 0.725)
        } else {
            (w, k)
        }
    }
}

/// A text slot string as sub_AB4D0 hands it on: each double space becomes U+3000 (not for English).
fn slot_chars(s: &str) -> impl Iterator<Item = char> + '_ {
    let mut it = s.chars().peekable();
    let english = english();
    std::iter::from_fn(move || {
        let c = it.next()?;
        if !english && c == ' ' && it.peek() == Some(&' ') {
            it.next();
            return Some('\u{3000}');
        }
        Some(c)
    })
}

/// The character space after `c`: inside sub_BDC2C's ASCII runs (tag 0x10) +100932 (sub_C36D8: 0.5 for
/// Japanese, 1.0 for Chinese); English has no ASCII runs.
fn slot_space(c: char, spacing: f32) -> f32 {
    if language() == 0 && (' '..='~').contains(&c) { spacing * 0.5 } else { spacing }
}

/// One laid-out character of a page.
#[derive(Clone, Copy, Debug)]
pub struct PlacedChar {
    pub c: char,
    pub x: f32,
    pub baseline: f32,
    pub size: f32,
    /// reveal order index (ruby shares its base's index)
    pub order: usize,
    /// an exfont glyph stretched over a run of this many cells (sub_C1080's tag 13), with the layout's
    /// character spacing; `c` is the exfont glyph
    pub run: Option<(u8, f32)>,
    /// glyph x scale: sub_BDC2C wraps each … in tags 0x12 / 0x13, and sub_DD334's 0x12 widens the writer's x
    /// scale by (W + spacing) / W, so the dots spread evenly over the cell including the character spacing
    pub stretch: f32,
    /// the first character of a bar run (`Run::Bar`) carries the bar
    pub bar: Option<Bar>,
}

/// A bar over a run of bar tags (sub_C6A58): one quad from the bar sheet (DISP_PARTS bf_with_edge68x68_2 for
/// an edged slot, bf_no_edge68x68_2 otherwise: 21 x 29 cells of 68 px), as many cells wide as the run has
/// characters revealed. The tag processor (sub_DB650 cases 0x16 / 0x19) puts it at the writer's cursor plus
/// W / 4 in x and y (W = writer +32, the glyph size): the pen x at the first character and its line top.
#[derive(Clone, Copy, Debug)]
pub struct Bar {
    /// the sheet row of a one-cell bar: 0 for ber, 21 for zap (+101868)
    pub row: u8,
    /// characters in the run
    pub len: u8,
    /// the line top at the first character
    pub top: f32,
    /// a cell's width: glyph size + character spacing (+776 + +100912)
    pub cell: f32,
}

impl Bar {
    /// The bar's rect and uv rect (x0, y0, x1, y1) when `count` (1..=len) of its characters show:
    /// row count - 1 + `row` of the sheet, count cells from the left; `x` is the first character's pen x.
    pub fn rects(&self, x: f32, size: f32, count: u8) -> ([f32; 4], [f32; 4]) {
        const CELL: f32 = 68.0;
        let (sw, sh) = (1428.0, 1972.0);
        let n = f32::from(count.clamp(1, self.len));
        let v0 = CELL * (n + f32::from(self.row) - 1.0) / sh;
        ([x + size / 4.0, self.top + size / 4.0, n * self.cell, size], [0.0, v0, n * CELL / sw, v0 + CELL / sh])
    }
}

impl PlacedChar {
    /// The full-width extent the char covers (a run: its n cells less the trailing spacing).
    pub fn width(&self) -> f32 {
        self.run.map_or(self.size, |(n, sp)| n as f32 * (self.size + sp) - sp)
    }
}

/// A parsed run of message text.
#[derive(Clone, Debug)]
pub enum Run {
    Text(String),
    Ruby { base: String, ruby: String },
    Newline,
    /// a run of bar tags (`[ber..]` / `[zap00]`): its text (a U+3000 per tag) under one bar from the sheet
    /// row `row` on (`Bar`)
    Bar { row: u8, text: String },
}

/// The inline glyph tags (overseas sub_A6220 / sub_A62C0 over off_34A910, off_34ABB0 and off_34AA60; the
/// Japanese sub_A7790 inline over off_AC06C0, off_AC0930 and off_AC07F8, without the last three): the exfont
/// character of the plain set and of the edged set, which sub_BDC2C picks by the slot's edge (+101003, the
/// rimmed message text).
const EX_TAGS: [(&str, char, char); 10] = [
    ("heart", '²', '†'),
    ("ageage", '³', '‡'),
    ("ansz", '¶', 'ˆ'),
    ("ingz", '¨', '‰'),
    ("eywz", '¸', '˜'),
    ("swel", '¹', '™'),
    ("onpu", 'ž', 'Ž'),
    ("star2", '³', '‡'),
    ("heart2", '¶', 'ˆ'),
    ("edia", '¨', '‰'),
];

/// The Unicode look-alike of an inline glyph tag's exfont character (for knife's text stream), else `c`.
pub fn tag_symbol(c: char) -> char {
    const SYMBOLS: [char; 10] = ['♥', '☆', '♡', 'ë', '⚡', '⚡', '♪', '☆', '♡', 'ë'];
    EX_TAGS.iter().zip(SYMBOLS).find(|(t, _)| t.1 == c || t.2 == c).map_or(c, |(_, s)| s)
}

/// The bar sheet's first row of each bar tag kind (sub_BDC2C: `[ber00]`..`[ber30]` give U+3000 and +101868 =
/// 0, `[zap00]` ⁂ and 21).
fn bar_tag(tag: &str) -> Option<u8> {
    if tag == "zap00" {
        return Some(21);
    }
    let n: u32 = tag.strip_prefix("ber").filter(|d| d.len() == 2)?.parse().ok()?;
    (n <= 30).then_some(0)
}

/// A message's save note text (the save list's text slot draws it again, without an edge): ruby readings and
/// line breaks dropped, inline glyphs as the plain set's characters, bar runs kept as their tags.
pub fn note_text(s: &str) -> String {
    let mut out = String::new();
    for run in parse_markup_edge(s, false) {
        match run {
            Run::Text(t) => out.push_str(&t),
            Run::Ruby { base, .. } => out.push_str(&base),
            Run::Newline => {}
            Run::Bar { row, text } => {
                for c in text.chars() {
                    match c {
                        '\u{3000}' => out.push_str(if row == 0 { "[ber00]" } else { "[zap00]" }),
                        c => out.push(c),
                    }
                }
            }
        }
    }
    out
}

/// Parse the message markup used in script_text with the slot's edge (`parse_markup_edge`): the rimmed text.
pub fn parse_markup(s: &str) -> Vec<Run> {
    parse_markup_edge(s, true)
}

/// Parse the message markup used in script_text: `<base|ruby>`, `@g` (style marker),
/// `^` (newline), the inline glyph tags and `@k`/`@e`/`@n` control codes. sub_BDC2C matches the tags by name
/// (sub_A6220) and puts the replacement in: an exfont character (`EX_TAGS`, the edged set when `edge`) or, for
/// the bar tags, U+3000 inside nn::font tags 22 (ber) / 25 (zap) ... 23 that mark the bar's first and last
/// character (+101860 / +101864) and its first sheet row (+101868); tags after the first extend the bar.
/// Unknown tags stay as text.
pub fn parse_markup_edge(s: &str, edge: bool) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars().peekable();
    // the bar is open while tags follow each other
    let mut bar_open = false;
    while let Some(c) = it.next() {
        if c != '[' {
            bar_open = false;
        }
        match c {
            '<' => {
                let mut body = String::new();
                for d in it.by_ref() {
                    if d == '>' {
                        break;
                    }
                    body.push(d);
                }
                if let Some((b, r)) = body.split_once('|') {
                    if !cur.is_empty() {
                        runs.push(Run::Text(std::mem::take(&mut cur)));
                    }
                    runs.push(Run::Ruby {
                        base: b.to_string(),
                        ruby: r.to_string(),
                    });
                } else {
                    cur.push_str(&body);
                }
            }
            '@' => {
                // control codes are one letter (plus optional digits for colours)
                if let Some(&n) = it.peek() {
                    it.next();
                    if n == 'n' || n == 'r' {
                        if !cur.is_empty() {
                            runs.push(Run::Text(std::mem::take(&mut cur)));
                        }
                        runs.push(Run::Newline);
                    } else if n == 'c' {
                        while it.peek().is_some_and(|d| d.is_ascii_digit()) {
                            it.next();
                        }
                    }
                }
            }
            '^' => {
                if !cur.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut cur)));
                }
                runs.push(Run::Newline);
            }
            '[' => {
                let mut tag = String::new();
                let mut closed = false;
                while let Some(&d) = it.peek() {
                    if d == '[' {
                        break;
                    }
                    it.next();
                    if d == ']' {
                        closed = true;
                        break;
                    }
                    tag.push(d);
                }
                let overseas = crate::assets::overseas();
                let ex = EX_TAGS[..if overseas { 10 } else { 7 }].iter().find(|t| t.0 == tag).map(|t| if edge { t.2 } else { t.1 });
                let bar = bar_tag(&tag);
                match (closed, ex, bar) {
                    (true, _, Some(_)) | (true, Some(_), _) if bar_open => {
                        if let Some(Run::Bar { text, .. }) = runs.last_mut() {
                            text.push(ex.unwrap_or('\u{3000}'));
                        }
                    }
                    (true, _, Some(row)) => {
                        if !cur.is_empty() {
                            runs.push(Run::Text(std::mem::take(&mut cur)));
                        }
                        runs.push(Run::Bar { row, text: '\u{3000}'.to_string() });
                        bar_open = true;
                    }
                    (true, Some(e), _) => cur.push(e),
                    _ => {
                        bar_open = false;
                        cur.push('[');
                        cur.push_str(&tag);
                        if closed {
                            cur.push(']');
                        }
                    }
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        runs.push(Run::Text(cur));
    }
    runs
}

/// The exfont glyphs sub_C1080 / sub_C0BB0 put in place of runs: for ―, ─ and — a line, for ■ a box, in the
/// thin variants œ / ½ while the slot's +101003 switch (TsukuGo text, flag 0x200000) is off, as for message
/// text (the thick Œ / ¼ go with the gothic font).
const EX_DASH: char = 'œ';
const EX_BOX: char = '½';

fn run_kind(c: char) -> Option<char> {
    match c {
        '―' | '─' | '—' => Some(EX_DASH),
        '■' => Some(EX_BOX),
        _ => None,
    }
}

/// Ruby scale (dword_3C9430[language]: Japanese 0.52, English 0.55, Chinese 0.51) of the text's glyph scale
/// and spacing, and the ruby cursor's extra y (slot +101748 = -2 * H / 1080 from the slot init's +100924).
const RUBY_SCALE: [f32; 5] = [0.52, 0.55, 0.51, 0.51, 0.45];
const RUBY_Y: f32 = -2.0;

/// Characters that must not start a line (kinsoku).
fn no_line_start(c: char) -> bool {
    "、。，．・？！゛゜ヽヾゝゞ々ー）］｝」』!),.:;?]}…―ぁぃぅぇぉっゃゅょァィゥェォッャュョ〟’”".contains(c)
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub left: f32,
    pub right: f32,
    /// glyph em in px
    pub size: f32,
    /// nn::font character spacing added after every glyph's advance (text slot +100912)
    pub spacing: f32,
    pub line_height: f32,
    /// the scaled pair-font ascent: a line's baseline below its box top
    pub ascent: f32,
}

/// Lay out runs starting at pen (x, baseline). Returns the end pen and the next order index.
pub fn layout(
    atlas: &FontAtlas,
    lay: &Layout,
    runs: &[Run],
    mut x: f32,
    mut baseline: f32,
    mut order: usize,
    out: &mut Vec<PlacedChar>,
) -> (f32, f32, usize) {
    // `n` > 0: a run of n source chars `c` drawn as one stretched exfont glyph (`ex`) over n cells
    let push = |c: char, ex: Option<(char, u8)>, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| {
        let n = ex.map_or(1, |e| e.1) as f32;
        // a run crosses n full-width cells
        let adv = if ex.is_some() { lay.size } else { atlas.advance(c, lay.size) };
        let width = n * (adv + lay.spacing) - lay.spacing;
        // nn::font sizes a line in characters (sub_BD7EC): N = floor((W - g / 2) / (g + spacing)) + 1 for the
        // slot width W and the glyph width g, i.e. a character fits while its cell starts at most W - g / 2
        // from the left (28 full-width glyphs in the 464-unit message slot, 17 in the save note's 130 units);
        // kinsoku chars never start a line, so they overhang the right edge
        if *x + (n - 1.0) * (adv + lay.spacing) + lay.size / 2.0 > lay.right && !no_line_start(c) {
            *x = lay.left;
            *baseline += lay.line_height;
        }
        out.push(PlacedChar {
            c: ex.map_or(c, |e| e.0),
            x: *x,
            baseline: *baseline,
            size: lay.size,
            order: *order,
            run: ex.map(|e| (e.1, lay.spacing)),
            stretch: if ex.is_none() && c == '…' { (lay.size + lay.spacing) / lay.size } else { 1.0 },
            bar: None,
        });
        // the run is one character to the reveal
        *order += 1;
        *x += width + lay.spacing;
    };
    let push_char = |c: char, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| push(c, None, x, baseline, order, out);
    let text = |t: &str, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| {
        let chars: Vec<char> = t.chars().collect();
        if english() {
            english_text(atlas, lay, &chars, (x, baseline), order, out);
            return;
        }
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let n = chars[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
            match run_kind(c) {
                // runs of dashes (any length) and of two or more boxes (sub_C1080)
                Some(g) if atlas.has_ex(g) && (g == EX_DASH || n >= 2) => {
                    push(c, Some((g, n as u8)), x, baseline, order, out);
                    i += n;
                }
                _ => {
                    push_char(c, x, baseline, order, out);
                    i += 1;
                }
            }
        }
    };
    for run in runs {
        match run {
            Run::Newline => {
                x = lay.left;
                baseline += lay.line_height;
            }
            Run::Text(t) => text(t, &mut x, &mut baseline, &mut order, out),
            Run::Bar { row, text: t } => {
                let first = out.len();
                text(t, &mut x, &mut baseline, &mut order, out);
                if let Some(pc) = out.get_mut(first) {
                    let len = t.chars().count().min(usize::from(u8::MAX)) as u8;
                    pc.bar = Some(Bar { row: *row, len, top: pc.baseline - lay.ascent, cell: lay.size + lay.spacing });
                }
            }
            Run::Ruby { base, ruby } => {
                let bw: f32 = base.chars().map(|c| atlas.advance(c, lay.size) + lay.spacing).sum();
                // the base's last character must fit (the line size in characters above)
                let last = base.chars().last().map_or(0.0, |c| atlas.advance(c, lay.size) + lay.spacing);
                if x + bw - last + lay.size / 2.0 > lay.right {
                    x = lay.left;
                    baseline += lay.line_height;
                }
                let start_x = x;
                let first = order;
                for c in base.chars() {
                    push_char(c, &mut x, &mut baseline, &mut order, out);
                }
                // sub_DDA1C: the ruby writer at 0.52 of the text's scale and spacing, centred over the base (the
                // spread flag +101008 is 0 in sub_A7790's slot init); nn::font string widths have no trailing
                // spacing. Its cursor y is the base writer's less the scaled pair font's GetBaselinePos (vtable
                // +200: the message pair's, `sfnt::SetMetrics::baseline`) and +101748 (-2 px); both inks sit
                // `GLYPH_DROP` cache pixels below their cursors at their own scales. Switch: the ruby 45.37 px
                // above the base on the Japanese train page (まえぶ, B 41 -> 45.29), Traditional Chinese 41.15
                // (約定, the exfont's 37 -> 40.73).
                let rk = RUBY_SCALE.get(language()).copied().unwrap_or(RUBY_SCALE[0]);
                let (rs, rsp) = (lay.size * rk, lay.spacing * rk);
                let bw = bw - lay.spacing;
                let rw: f32 = ruby.chars().map(|c| atlas.advance(c, rs) + rsp).sum::<f32>() - rsp;
                let n = ruby.chars().count().max(1) as f32;
                let mut rx = start_x + (bw - rw) / 2.0;
                let b = MSG_BASELINE.load(Ordering::Relaxed) as f32 + (1.0 - rk) * GLYPH_DROP;
                let rb = baseline - b * lay.size / BASE_SIZE - RUBY_Y;
                let span = (order - first).max(1);
                for (k, c) in ruby.chars().enumerate() {
                    out.push(PlacedChar {
                        c,
                        x: rx,
                        baseline: rb,
                        size: rs,
                        order: first + (k * span / n as usize).min(span - 1),
                        run: None,
                        stretch: 1.0,
                        bar: None,
                    });
                    rx += atlas.advance(c, rs) + rsp;
                }
            }
        }
    }
    (x, baseline, order)
}

/// English message text (the overseas build's +101009 = English, sub_A7790): no per-line character count
/// (sub_BD7EC), but nn::font's line breaking. For each line sub_BF0F8 finds the first character that runs past
/// the slot width (sub_C7240 / sub_145334: pen + spacing + kerning + advance > width; a line's first character
/// always fits) and asks sub_148988 for the line's last character; sub_C471C breaks right after it, so a line
/// keeps its trailing spaces. Glyphs advance by their own width plus the kerning before them and the spacing.
fn english_text(atlas: &FontAtlas, lay: &Layout, chars: &[char], (x, baseline): (&mut f32, &mut f32), order: &mut usize, out: &mut Vec<PlacedChar>) {
    // sub_C1080 turns runs of dashes and of two or more boxes into one stretched exfont glyph (tag 13) for
    // English text too: units of (first char index, char count, exfont glyph)
    let mut units: Vec<(usize, usize, Option<char>)> = Vec::new();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        let n = chars[k..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
        match run_kind(c) {
            Some(g) if atlas.has_ex(g) && (g == EX_DASH || n >= 2) => {
                units.push((k, n, Some(g)));
                k += n;
            }
            _ => {
                units.push((k, 1, None));
                k += 1;
            }
        }
    }
    // a unit's kerning (none next to a run) and width
    let width = |prev: Option<char>, u: (usize, usize, Option<char>)| match u.2 {
        Some(_) => (0.0, u.1 as f32 * (lay.size + lay.spacing) - lay.spacing),
        None => {
            let c = chars[u.0];
            (atlas.kern(prev, c, lay.size), atlas.advance(c, lay.size))
        }
    };
    let next_prev = |u: (usize, usize, Option<char>)| if u.2.is_some() { None } else { Some(chars[u.0]) };
    // the spacing after a unit: none after a run (Switch, Seurat bubble "■■■■ ■■■■■.": the space and the
    // full stop follow the runs' n cells less the trailing spacing within a pixel)
    let after = |u: (usize, usize, Option<char>)| if u.2.is_some() { 0.0 } else { lay.spacing };
    let lb = LINE_BREAK.get();
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < units.len() {
        // the first unit past the width
        let mut px = *x;
        let mut kp = prev;
        let mut overflow = None;
        for (j, &u) in units.iter().enumerate().skip(i) {
            let (kern, w) = width(kp, u);
            if (j > i || *x > lay.left) && px + kern + w > lay.right {
                overflow = Some(j);
                break;
            }
            px += kern + w + after(u);
            kp = next_prev(u);
        }
        let last = match overflow {
            None => units.len() - 1,
            Some(o) => {
                let c = match lb {
                    Some(lb) => lb.last_on_line(chars, units[i].0, Some(units[o].0)),
                    None => units[o].0.saturating_sub(1).max(units[i].0),
                };
                // the unit holding that character
                units.iter().rposition(|u| u.0 <= c).unwrap_or(i).max(i)
            }
        };
        // text that continues a line whose first word no longer fits starts on the next one
        if *x > lay.left && overflow.is_some_and(|o| last >= o && o == i) {
            *x = lay.left;
            *baseline += lay.line_height;
            prev = None;
            continue;
        }
        for &u in &units[i..=last] {
            let (kern, w) = width(prev, u);
            *x += kern;
            let (c, run) = match u.2 {
                Some(g) => (g, Some((u.1 as u8, lay.spacing))),
                None => (chars[u.0], None),
            };
            out.push(PlacedChar { c, x: *x, baseline: *baseline, size: lay.size, order: *order, run, stretch: 1.0, bar: None });
            *order += 1;
            *x += w + after(u);
            prev = next_prev(u);
        }
        i = last + 1;
        if i < units.len() {
            *x = lay.left;
            *baseline += lay.line_height;
            prev = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use ab_glyph::{Font, FontRef};

    /// Every drawn glyph of the CFF fonts outlines once `cff::normalize` has moved the data after their
    /// endchar out of the way (needs switch_romfs and switch_romfs_en): the bubble text "Sometimes, ..." of
    /// the overseas bad ends lost o / e / t / s, the English message font ö, ’, ”, — and the rest.
    #[test]
    fn cff_glyphs_after_endchar_outline() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for (font, fixed) in [
            ("switch_romfs/ja3/FOT-SeuratPro-M.otf", 65),
            ("switch_romfs_en/en/HelveticaNeueLTGEO-55Roman.otf", 59),
            ("switch_romfs_en/en2/DemosNextPro-Regular.otf", 18),
            ("switch_romfs_en/en3/FOT-SeuratPro-M.otf", 18),
        ] {
            let Ok(mut data) = std::fs::read(root.join(font)) else { continue };
            assert_eq!(knife_core::format::cff::normalize(&mut data), fixed, "{font}");
            let f = FontRef::try_from_slice(&data).unwrap();
            for c in "Sometimes, it's not our hero öà’”—".chars() {
                if f.glyph_id(c).0 != 0 && c != ' ' {
                    assert!(f.outline(f.glyph_id(c)).is_some(), "{font}: {c}");
                }
            }
            // the glyphs are untouched otherwise: normalizing twice changes nothing
            assert_eq!(knife_core::format::cff::normalize(&mut data), 0, "{font}");
        }
    }
}
