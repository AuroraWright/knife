//! Font rasterisation into an R8 atlas, and NVL page layout.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use knife_core::format::bffnt;
use knife_core::itype::glyph::Hinter;
use knife_core::itype::raster::Graymap;
use knife_core::itype::tt;
use knife_core::kinsoku;
use knife_core::linebreak::LineBreak;
use knife_render::{Blend, Quad, Renderer, Shade, TexRef};

const ATLAS: u32 = 2048;
/// the empty texels around each atlas glyph, the first row and column too (its padded quad samples half of one)
const PAD: u32 = 2;

#[derive(Clone, Copy)]
struct Glyph {
    /// atlas rect in pixels
    rect: [u32; 4],
    /// offset of the bitmap's top-left from the pen position on the baseline
    off: [f32; 2],
    /// atlas pixels per base-size pixel (`FontAtlas::raster`)
    px: f32,
}

/// A face of the font list (TrueType or CFF) rasterized as nn::font's glyph caches do (`knife_core::itype`:
/// hinted, scan converted, outlined and trimmed bit for bit), with its hinting state at the atlas's raster size.
struct TtFace {
    data: Arc<Vec<u8>>,
    hinter: Option<(u16, Hinter)>,
}

impl TtFace {
    fn new(data: Arc<Vec<u8>>) -> Option<TtFace> {
        tt::Font::new(&data)?;
        Some(TtFace { data, hinter: None })
    }

    /// `c`'s cache map at `ppem` with outline width `ow` (`cache_cell`); None for a glyph that will not load.
    fn map(&mut self, c: char, ppem: u16, ow: u16) -> Option<Graymap> {
        let font = tt::Font::new(&self.data)?;
        if self.hinter.as_ref().is_none_or(|&(p, _)| p != ppem) {
            self.hinter = Some((ppem, Hinter::new(&font, ppem)));
        }
        let (_, h) = self.hinter.as_mut()?;
        h.cache_map(&font, font.glyph(c as u32), ow)
    }
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
/// nn::font's line breaker over the executable's tables (sub_148988): English lines, and each Japanese / Chinese
/// line's last character before the kinsoku (`plan_lines`).
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
pub const OUTLINE: [u16; 4] = [0, 2, 3, 4];

/// A glyph quad of an edged cache in fs_text's bordered mode: half coverage the edge in `edge` (RGB bytes),
/// full the fill in the vertex colour.
fn edged_glyph(q: Quad, edge: [u8; 3]) -> Quad {
    let packed = (u32::from(edge[0]) << 16 | u32::from(edge[1]) << 8 | u32::from(edge[2])) as f32;
    Quad { haze: [[packed, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], ..q }
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
/// The message font set's own baseline position (+36 of its ScalableFont, before the exfont pairing): its glyph
/// cache cells' baseline, a base px above their quads' italic origin (`italic_quad`).
static GLYPH_BASELINE: AtomicUsize = AtomicUsize::new(41);
static BUBBLE_ASCENT: AtomicUsize = AtomicUsize::new(41);

/// The fonts' base size (the font manager's +124, from sub_AB0F0's init struct): glyph scale = size / 40.
pub const BASE_SIZE: f32 = 40.0;
/// The writer font's width (sub_13BCF0: scale.x x PairFont::GetWidth, the larger of its fonts'): the exfont's
/// FINF width 41 against the ScalableFont's 40 (the base size), in every build. Switch: Witch on the Holy
/// Night's `@_` rubies 22.65 px right of their bases' start, half of 41 at the 44.4 px glyphs; the Japanese
/// build's three-dash run moves the pen on by 52.5 px at tag 0xD (`run_shifts` gives 52.3 with 41, 62.0 with
/// the shared font's 50).
pub fn pair_width() -> f32 {
    41.0
}

/// The message pair font's line feed at the base size (`knife_core::format::sfnt::pair_metrics` over the
/// language's three fonts and the exfont), in base px: 50 for Japanese (Switch: line pitch 50 * 1.14 + 24 = 81
/// px), 53 for English (Switch: choice rows 78.4 px apart, 53 * 1.026 + 24), 49 / 47 for Simplified /
/// Traditional Chinese.
static LINE_FEED: AtomicUsize = AtomicUsize::new(50);
static BUBBLE_LINE_FEED: AtomicUsize = AtomicUsize::new(50);

/// The language's font metrics: the message pair font's line feed, ascent and baseline position and the bubble
/// pair font's ascent and line feed, in base px.
pub fn set_font_metrics(line_feed: i32, msg_ascent: i32, msg_baseline: i32, bubble_ascent: i32, bubble_line_feed: i32, glyph_baseline: i32) {
    GLYPH_BASELINE.store(glyph_baseline.max(0) as usize, Ordering::Relaxed);
    LINE_FEED.store(line_feed.max(1) as usize, Ordering::Relaxed);
    BUBBLE_LINE_FEED.store(bubble_line_feed.max(1) as usize, Ordering::Relaxed);
    MSG_ASCENT.store(msg_ascent.max(0) as usize, Ordering::Relaxed);
    MSG_BASELINE.store(msg_baseline.max(0) as usize, Ordering::Relaxed);
    BUBBLE_ASCENT.store(bubble_ascent.max(0) as usize, Ordering::Relaxed);
}

pub fn line_feed() -> f32 {
    LINE_FEED.load(Ordering::Relaxed) as f32
}

/// The bubble font's line feed at the base size: the message font's in Tsukihime (its font list holds the
/// bubble font, `<folder>3`), the shared system font's pair in Witch on the Holy Night (Switch: TeaTime bubble
/// lines 82 px apart).
pub fn bubble_line_feed() -> f32 {
    BUBBLE_LINE_FEED.load(Ordering::Relaxed) as f32
}

/// An exfont cell's top above the line's baseline, in base-size px: the cell's glyph baseline (TGLP 37) sits a
/// base px above the line's. Switch (Astris with its Nearest scaling filter, exact game pixels; Japanese at
/// 45.6 px): the edged dash's rows (the sheet's 21..24) 14 166 255 155 19 over a 93 grey, as knife's.
const EX_TOP: f32 = 38.0;
/// An exfont glyph's italic origin above the line's baseline (base px): its quad's top, half a scaled pixel
/// above its cell (`italic_quad`), in every language. Switch (overseas Japanese and Simplified Chinese,
/// two-dash italic runs leaning over the text's and a ruby's height): both ends of each within 0.2 px.
const EX_SHEAR_TOP: f32 = EX_TOP + 0.5;

/// One nn::font draw buffer's glyph quads in draw order (DispStringBuffer, sub_13C5A4): the buffer draws one
/// texture at a time, in the order the textures first appear, and each texture's glyphs in their own order, so
/// a message's exfont glyphs (the sheet) all draw after its glyph cache glyphs, or all before them when the
/// first glyph is one. Switch, the Japanese build: an italic six-dash run over the い after it draws on top of
/// い, and a two-dash run's end beside い keeps its full white under い's rim (exact pixels, Astris Nearest).
#[derive(Default)]
pub struct Batches {
    ex_first: Option<bool>,
    font: Vec<Quad>,
    ex: Vec<Quad>,
}

impl Batches {
    pub fn push(&mut self, ex: bool, q: Quad) {
        self.ex_first.get_or_insert(ex);
        if ex { self.ex.push(q) } else { self.font.push(q) }
    }

    /// Append the buffer's quads to `out` and start a new buffer.
    pub fn flush(&mut self, out: &mut Vec<Quad>) {
        if self.ex_first == Some(true) {
            out.append(&mut self.ex);
        }
        out.append(&mut self.font);
        out.append(&mut self.ex);
        self.ex_first = None;
    }
}

pub struct FontAtlas {
    font: FontArc,
    /// the rest of the language's font list (`add_fallback`)
    fallback: Vec<FontArc>,
    /// the font file, for the GPOS pair kerning ab_glyph does not read (`kern`)
    data: Arc<Vec<u8>>,
    gpos_kern: std::cell::RefCell<HashMap<(u16, u16), i16>>,
    /// the font list's faces for iType (the font, then the fallbacks)
    tt: Vec<Option<TtFace>>,
    ex: Option<ExFont>,
    pub tex: TexRef,
    /// by char and outline width
    glyphs: HashMap<(char, u16), Option<Glyph>>,
    /// the atlas's side
    side: u32,
    /// the glyphs' raster scale over the base size: the render scale, so a canvas drawn above or below 1080p keeps
    /// the glyphs as sharp as the Switch's
    raster: f32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    /// Tsukihime's QA bubble font (`Game::msg_font`): its text carries style 0x40 (`plan_lines`)
    pub bubble: bool,
}

impl FontAtlas {
    pub fn new(r: &Renderer, data: Vec<u8>) -> anyhow::Result<Self> {
        let font = FontArc::try_from_vec(data.clone())?;
        let tex = r.create_writable(ATLAS, ATLAS, true, "font atlas");
        let data = Arc::new(data);
        Ok(Self {
            font,
            fallback: Vec::new(),
            tt: vec![TtFace::new(data.clone())],
            data,
            gpos_kern: Default::default(),
            ex: None,
            tex,
            glyphs: HashMap::new(),
            side: ATLAS,
            raster: 1.0,
            shelf_x: PAD,
            shelf_y: PAD,
            shelf_h: 0,
            bubble: false,
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

    /// Whether the placed char draws from the exfont sheet (a run, or an exfont glyph), not the glyph cache.
    pub fn is_ex(&self, pc: &PlacedChar) -> bool {
        pc.run.is_some() || self.has_ex(pc.c)
    }

    /// Add the next font of the language's font list: sub_CA500 gives each of the message glyph cache's
    /// faces the language's three font files as one nn::font font list, and the cache takes a glyph from the
    /// first font that has it (English: Helvetica Neue has no U+3000 or ■, which come from FOT-Seurat, en3).
    pub fn add_fallback(&mut self, data: Vec<u8>) -> anyhow::Result<()> {
        self.fallback.push(FontArc::try_from_vec(data.clone())?);
        self.tt.push(TtFace::new(Arc::new(data)));
        Ok(())
    }

    /// The character nn::font draws for `c`: ■ (U+25A0) when neither the font list nor the exfont has it
    /// (Switch, overseas Japanese: 杀 and 唧, which TT_TsukuMin-D lacks, draw as its filled box, 34 px wide at
    /// 45.6 px, in the text and in the ruby).
    fn shown(&self, c: char) -> char {
        let missing = |c: char| self.font.glyph_id(c).0 == 0 && self.fallback.iter().all(|f| f.glyph_id(c).0 == 0);
        if !c.is_control() && !self.has_ex(c) && missing(c) && !missing('■') { '■' } else { c }
    }

    /// The font of the list that has `c`, else the first.
    fn face(&self, c: char) -> &FontArc {
        match self.face_index(c) {
            0 => &self.font,
            i => &self.fallback[i - 1],
        }
    }

    /// `face`'s place in the list (0 the font, then the fallbacks).
    fn face_index(&self, c: char) -> usize {
        if self.font.glyph_id(c).0 != 0 {
            return 0;
        }
        self.fallback.iter().position(|f| f.glyph_id(c).0 != 0).map_or(0, |i| i + 1)
    }

    /// A single exfont glyph (the pair font's first font, sub_CAE88, so the inline glyph tags' characters come
    /// from it): its cell scaled by size / 40 at the pen plus its left bearing, the cell top `EX_TOP` above the
    /// baseline. The edged set (`EX_TAGS`) carries its own border at half coverage, which nn::font draws in the
    /// edge colour (Switch, QA bubble `[heart]`: a white heart in a black rim; fs_text's bordered mode).
    /// The pair font is padded (PrintGlyph jp_13BE3C with the font's +9 set): the quad and its uv rect reach
    /// half a texel past the cell on every side, so the sheet's empty gap blends in at the cell's edge (Switch,
    /// nearest-scaled capture: the edged dash's ends get a column of edge colour, 64 28 13 30 65).
    fn ex_quad(&self, c: char, size: f32, (x, baseline): (f32, f32), color: [f32; 4]) -> Option<Quad> {
        let ex = self.ex.as_ref()?;
        let g = ex.font.glyph(c)?;
        let sx = size / BASE_SIZE;
        let (tw, th) = (ex.font.sheet.width as f32, ex.font.sheet.height as f32);
        let (gw, ch) = (f32::from(g.width), ex.font.cell_h as f32);
        let q = Quad::rect(
            ex.tex.clone(),
            [x + (f32::from(g.left) - 0.5) * sx, baseline - (EX_TOP + 0.5) * sx, (gw + 1.0) * sx, (ch + 1.0) * sx],
            color,
        )
        .with_uv_rect((g.x as f32 - 0.5) / tw, (g.y as f32 - 0.5) / th, (g.x as f32 + gw + 0.5) / tw, (g.y as f32 + ch + 0.5) / th);
        let border = if bordered(c) { 1.0 } else { 0.0 };
        Some(Quad { blend: Blend::Alpha, shade: Shade::Text, haze: [[0.0; 4], [0.0, 0.0, border, 0.0]], ..q })
    }

    /// Quad for a placed char (a font glyph, or an exfont glyph stretched over a run) offset by (dx, dy).
    pub fn placed_quad(&mut self, r: &Renderer, pc: &PlacedChar, dx: f32, dy: f32, color: [f32; 4]) -> Option<Quad> {
        let q = self.upright_quad(r, pc, dx, dy, color, 0)?;
        let ex = pc.run.is_some() || self.has_ex(pc.c);
        Some(italic_quad(q, pc, dy, ex))
    }

    /// `placed_quad` before the italic shear, a font glyph from the cache of outline width `ow`.
    fn upright_quad(&mut self, r: &Renderer, pc: &PlacedChar, dx: f32, dy: f32, color: [f32; 4], ow: u16) -> Option<Quad> {
        let q = match pc.run {
            Some((n, spacing)) => self.run_quad(pc.c, (n, spacing), pc.size, (pc.x + dx, pc.baseline + dy), color),
            None if self.has_ex(pc.c) => self.ex_quad(pc.c, pc.size, (pc.x + dx, pc.baseline + dy), color),
            None => self.quad_stretched(r, pc.c, pc.size, (pc.x + dx, pc.baseline + dy), pc.stretch, color, ow),
        }?;
        if pc.sx == 1.0 {
            return Some(q);
        }
        // the writer's x scale squeezes the glyph about the pen
        let x0 = pc.x + dx;
        Some(Quad { pos: q.pos.map(|[x, y]| [x0 + (x - x0) * pc.sx, y]), ..q })
    }

    /// The placed char filled `rgb` over its edge in `edge` (RGB bytes, the slot's edge colour), from the edged
    /// cache of font index `index` (`OUTLINE`), `alpha` fading the composite once.
    #[allow(clippy::too_many_arguments)]
    pub fn rimmed_quad_edge(&mut self, r: &Renderer, pc: &PlacedChar, index: usize, rgb: [f32; 3], edge: [u8; 3], alpha: f32) -> Option<Quad> {
        let color = [rgb[0], rgb[1], rgb[2], alpha];
        // an exfont glyph (a run or not) is a sheet bitmap, not a glyph of the outlined cache sub-set: no rim of
        // its own, the edged slot's dash and box carrying their border (`edged`), drawn in the edge colour
        if pc.run.is_some() || self.has_ex(pc.c) {
            let mut q = self.upright_quad(r, &PlacedChar { c: edged(pc.c), ..*pc }, 0.0, 0.0, color, 0)?;
            q.haze[0][0] = (u32::from(edge[0]) << 16 | u32::from(edge[1]) << 8 | u32::from(edge[2])) as f32;
            return Some(italic_quad(q, pc, 0.0, true));
        }
        let q = self.upright_quad(r, pc, 0.0, 0.0, color, OUTLINE[index])?;
        Some(italic_quad(edged_glyph(q, edge), pc, 0.0, false))
    }

    /// How far right of the pen `run_quad` draws a run's glyph: centred over its n cells, English at the pen.
    pub fn run_inset(&self, c: char, (n, spacing): (u8, f32), size: f32) -> f32 {
        let Some(g) = self.ex.as_ref().and_then(|e| e.font.glyph(c)) else { return 0.0 };
        if english() {
            return 0.0;
        }
        (f32::from(n) * (size + spacing) - g.width as f32 * run_scale((n, spacing), size)) / 2.0
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
        let qw = g.width as f32 * run_scale((n, spacing), size);
        let sx = size / BASE_SIZE;
        let qh = ex.font.cell_h as f32 * sx;
        let (tw, th) = (ex.font.sheet.width as f32, ex.font.sheet.height as f32);
        // the padded quad (`ex_quad`) at the run's x scale
        let px = qw / g.width as f32 * 0.5;
        let u0 = g.x as f32 + g.left.max(0) as f32;
        let q = Quad::rect(
            ex.tex.clone(),
            [x + self.run_inset(c, (n, spacing), size) - px, baseline - (EX_TOP + 0.5) * sx, qw + 2.0 * px, qh + sx],
            color,
        )
        .with_uv_rect((u0 - 0.5) / tw, (g.y as f32 - 0.5) / th, (u0 + g.width as f32 + 0.5) / tw, (g.y as f32 + ex.font.cell_h as f32 + 0.5) / th);
        let border = if bordered(c) { 1.0 } else { 0.0 };
        Some(Quad { blend: Blend::Alpha, shade: Shade::Text, haze: [[0.0; 4], [0.0, 0.0, border, 0.0]], ..q })
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
        let c = self.shown(c);
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

    /// Start over on a new atlas texture (quads already queued keep the old one), sized for the raster scale.
    fn reset(&mut self, r: &Renderer) {
        self.side = (ATLAS * self.raster.ceil() as u32).min(8192);
        self.tex = r.create_writable(self.side, self.side, true, "font atlas");
        self.glyphs.clear();
        self.shelf_x = PAD;
        self.shelf_y = PAD;
        self.shelf_h = 0;
    }

    /// The glyph as nn::font's glyph cache holds it (sub_143070): rasterized at the base size (font manager +124,
    /// 40 px) with its box on whole pixels and, for an edged cache, grown by the outline width `ow`
    /// (`cache_cell`: half coverage the edge, full the fill); the draw scales it to the slot size. Above or below
    /// 1080p at the render scale times that, to the nearest whole ppem, the outline width scaled alike.
    /// The faces rasterize as the Switch's iType does, TrueType and CFF, bit for bit at 1080p.
    fn glyph(&mut self, r: &Renderer, c: char, ow: u16) -> Option<Glyph> {
        if r.scale() != self.raster {
            self.raster = r.scale();
            self.reset(r);
        }
        let c = self.shown(c);
        if let Some(g) = self.glyphs.get(&(c, ow)) {
            return *g;
        }
        let ppem = (BASE_SIZE * self.raster).round().max(1.0) as u16;
        let scaled_ow = (f32::from(ow) * f32::from(ppem) / BASE_SIZE).round() as u16;
        let i = self.face_index(c);
        let map = self.tt.get_mut(i).and_then(Option::as_mut).and_then(|face| face.map(c, ppem, scaled_ow));
        let g = map.filter(|m| m.width > 0 && m.height > 0).map(|m| {
            let (w, h) = (m.width as u32, m.height as u32);
            if self.shelf_x + w + PAD > self.side {
                self.shelf_x = PAD;
                self.shelf_y += self.shelf_h + PAD;
                self.shelf_h = 0;
            }
            if self.shelf_y + h + PAD > self.side {
                self.reset(r);
            }
            let (x, y) = (self.shelf_x, self.shelf_y);
            self.shelf_x += w + PAD;
            self.shelf_h = self.shelf_h.max(h);
            r.write_region(&self.tex, [x, y, w, h], 1, &m.bits);
            Glyph {
                rect: [x, y, w, h],
                off: [f32::from(m.lo_x), -f32::from(m.hi_y) - 1.0],
                px: f32::from(ppem) / BASE_SIZE,
            }
        });
        let g = g.or(Some(Glyph {
            rect: [0, 0, 0, 0],
            off: [0.0, 0.0],
            px: 1.0,
        }));
        self.glyphs.insert((c, ow), g);
        g
    }

    /// Quad for one glyph with its pen at (x, baseline) from the edged cache of font index `index` (`OUTLINE`):
    /// filled in the vertex colour over its edge in black (fs_text's bordered mode).
    #[allow(clippy::too_many_arguments)]
    pub fn edged_quad(&mut self, r: &Renderer, c: char, size: f32, x: f32, baseline: f32, color: [f32; 4], index: usize) -> Option<Quad> {
        self.quad_stretched(r, c, size, (x, baseline), 1.0, color, OUTLINE[index]).map(|q| edged_glyph(q, [0; 3]))
    }

    /// A glyph quad with its x scaled by `sx` about the middle of its em (Switch, train page ……: the stretched
    /// dots stay centred on the unstretched glyph).
    /// `ow`: the cache's outline width (`glyph`).
    #[allow(clippy::too_many_arguments)]
    fn quad_stretched(&mut self, r: &Renderer, c: char, size: f32, (x, baseline): (f32, f32), sx: f32, color: [f32; 4], ow: u16) -> Option<Quad> {
        let g = self.glyph(r, c, ow)?;
        if g.rect[2] == 0 {
            return None;
        }
        let [gx, gy, gw, gh] = g.rect;
        let a = self.side as f32;
        let k = size / BASE_SIZE / g.px;
        // padded by half a texel on every side as PrintGlyph pads every font's glyphs (the nn::font::Font
        // constructor, 0x13DD40, sets the +9 it reads): the atlas's empty gap blends in at the cell's edge
        let (x0, y0) = (x + (g.off[0] - 0.5) * k * sx - (sx - 1.0) * size / 2.0, baseline + (g.off[1] - 0.5) * k);
        let q = Quad::rect(self.tex.clone(), [x0, y0, (gw as f32 + 1.0) * k * sx, (gh as f32 + 1.0) * k], color)
            .with_uv_rect((gx as f32 - 0.5) / a, (gy as f32 - 0.5) / a, (gx + gw) as f32 / a + 0.5 / a, (gy + gh) as f32 / a + 0.5 / a);
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

    /// `draw_str_slot` with the text engine's dark edge (the slots share sub_A7790 with the messages): the
    /// glyphs of the edged cache of the slot's font index (`OUTLINE`), which the draw scales from the base size
    /// (font manager +124, 40 px) with the glyph.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_str_slot_rimmed(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: [f32; 2], spacing: f32, pos: [f32; 2], color: [f32; 4], index: usize) -> f32 {
        self.slot_line(r, out, s, size, spacing, pos, color, Some(index), english())
    }

    #[allow(clippy::too_many_arguments)]
    /// `free`: the writer's +289 (English or flag 0x10000): own advances and kerning, no fixed cells. `rim`: the
    /// edged font index.
    fn slot_line(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, [w, h]: [f32; 2], spacing: f32, [x, baseline]: [f32; 2], color: [f32; 4], rim: Option<usize>, free: bool) -> f32 {
        let (w, h) = (w * glyph_scale(), h * glyph_scale());
        let mut pen = x;
        let mut prev = None;
        let chars = lone_dashes(slot_chars(s).collect(), Pass::Text, |g| self.has_ex(g));
        let prop = prop_runs(&chars, Pass::Text);
        let mut batches = Batches::default();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            // the writer parses slot text like the messages (sub_BD7EC -> sub_BDC2C): two or more dashes or
            // boxes are one exfont glyph over their n cells (tag 0xD), a single dash the plain exfont glyph
            // (`makes_run`); the rimmed slot's glyphs are the edged set with their own border (`edged`)
            let n = run_kind(c).filter(|&g| self.has_ex(g)).map_or(0, |g| 1 + chars[i..].iter().take_while(|&&d| run_kind(d) == Some(g)).count().min(254));
            if let Some(g) = run_kind(c).filter(|&g| n > 0 && makes_run(g, n, Pass::Text)) {
                i += n - 1;
                let g = if rim.is_some() { edged(g) } else { g };
                if let Some(q) = self.run_quad(g, (n as u8, spacing), w, (pen, baseline), color) {
                    batches.push(true, q);
                }
                // English: no spacing after the run, as in the messages
                pen += n as f32 * (w + spacing) - spacing;
                if !free {
                    pen += slot_space(chars.get(i).copied().unwrap_or(c), spacing);
                }
                continue;
            }
            // only a free writer kerns (+289)
            if free {
                pen += self.kern(prev.replace(c), c, h);
            }
            let (cell, sx) = self.slot_cell(c, w, h, free, prop[i - 1]);
            let gx = pen + (cell - self.advance(c, h) * sx) / 2.0;
            if self.has_ex(c) {
                let g = if rim.is_some() { edged(c) } else { c };
                // the writer's x scale squeezes the glyph about its pen
                if let Some(q) = self.ex_quad(g, h, (gx, baseline), color) {
                    batches.push(true, Quad { pos: q.pos.map(|[x, y]| [gx + (x - gx) * sx, y]), ..q });
                }
                pen += cell + if free { spacing } else { slot_space(chars.get(i).copied().unwrap_or(c), spacing) };
                continue;
            }
            // quad_stretched centres its stretch on the em; shift it back so the glyph starts at gx
            let ow = rim.map_or(0, |i| OUTLINE[i]);
            if let Some(q) = self.quad_stretched(r, c, h, (gx + (sx - 1.0) * h / 2.0, baseline), sx, color, ow) {
                batches.push(false, if rim.is_some() { edged_glyph(q, [0; 3]) } else { q });
            }
            // nn::font's TextWriter spaces each glyph from the one before at its own spacing (`steps`)
            pen += cell + if free { spacing } else { slot_space(chars.get(i).copied().unwrap_or(c), spacing) };
        }
        batches.flush(out);
        pen - x
    }

    /// Width of a text slot line: the advances plus the spacing between glyphs.
    pub fn measure_slot(&self, s: &str, [w, h]: [f32; 2], spacing: f32) -> f32 {
        let (w, h) = (w * glyph_scale(), h * glyph_scale());
        let chars = lone_dashes(slot_chars(s).collect(), Pass::Text, |g| self.has_ex(g));
        let prop = prop_runs(&chars, Pass::Text);
        // the spacing goes before each glyph but the first (`steps`)
        let first = chars.first().map_or(0.0, |&c| slot_space(c, spacing));
        let mut prev = None;
        chars
            .iter()
            .zip(&prop)
            .map(|(&c, &p)| {
                let (cell, _) = self.slot_cell(c, w, h, english(), p);
                let k = if english() { self.kern(prev.replace(c), c, h) } else { 0.0 };
                k + cell + slot_space(c, spacing)
            })
            .sum::<f32>()
            - first
    }

    /// `measure_slot` of a slot with the writer's free layout (flag 0x10000): own advances, kerning and the
    /// full spacing after each glyph, as `slot_line` places them.
    pub fn measure_slot_as(&self, s: &str, [w, h]: [f32; 2], spacing: f32, free: bool) -> f32 {
        if !free {
            return self.measure_slot(s, [w, h], spacing);
        }
        let (w, h) = (w * glyph_scale(), h * glyph_scale());
        let chars = lone_dashes(slot_chars(s).collect(), Pass::Text, |g| self.has_ex(g));
        let mut prev = None;
        let total: f32 = chars.iter().map(|&c| self.kern(prev.replace(c), c, h) + self.slot_cell(c, w, h, true, false).0 + spacing).sum();
        total - if chars.is_empty() { 0.0 } else { spacing }
    }

    /// `draw_str_slot` with the writer's free layout given (the slot's flag 0x10000).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_str_slot_as(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: [f32; 2], spacing: f32, pos: [f32; 2], color: [f32; 4], free: bool) -> f32 {
        self.slot_line(r, out, s, size, spacing, pos, color, None, free || english())
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
    /// clears it): no ASCII runs and no fixed width, each glyph its own advance. `prop`: a glyph of one of Witch
    /// on the Holy Night's proportional ASCII runs (`prop_runs`), its own advance at the run's 0.725 x scale.
    fn slot_cell(&self, c: char, w: f32, h: f32, free: bool, prop: bool) -> (f32, f32) {
        let k = w / h;
        if free {
            (self.advance(c, h) * k, k)
        } else if prop {
            (self.advance(c, h) * k * 0.725, k * 0.725)
        } else if (' '..='~').contains(&c) {
            (w * 0.5, k * 0.725)
        } else {
            (w, k)
        }
    }
}

/// A text slot string as sub_AB4D0 (every text slot's setter, the messages' too) hands it on to sub_A7790:
/// each double space becomes U+3000 (not for English).
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

/// Which of a slot line's characters sit in a proportional ASCII run. Witch on the Holy Night's slots keep
/// sub_BCDB0's parameter byte 0x8E = 1 (Tsukihime's sub_A7790 stores 0 over it), which sub_BD550 copies to
/// the text object's +0x18A96 and sub_B697C hands the writer as +131; jp_DD0D8 then clears the fixed width
/// (+56) for a run opened by tag 16, as for tag 27. jp_BDC2C picks the tag from the run's first characters:
/// 27 for "!!!", 26 (fixed half cells kept) for "!" or "?" followed by "!" or "?", 16 otherwise. Switch
/// (WoH config help, 蒼崎 青子): the space 6.7 px wide, not the half cell's 11.75. A ruby's base (jp_C1080,
/// `Pass`) knows no tag 27; the ruby text's runs print proportional whatever their tag (`steps`).
fn prop_runs(chars: &[char], pass: Pass) -> Vec<bool> {
    let ascii = |c: char| (' '..='~').contains(&c);
    let mut prop = vec![false; chars.len()];
    if !crate::assets::witch() || english() {
        return prop;
    }
    let mut i = 0;
    while i < chars.len() {
        let n = chars[i..].iter().take_while(|&&c| ascii(c)).count();
        if n == 0 {
            i += 1;
            continue;
        }
        let run = &chars[i..i + n];
        let mark = |j: usize| run.get(j).is_some_and(|&c| c == '!' || c == '?');
        let bang3 = pass == Pass::Text && run.starts_with(&['!', '!', '!']);
        let fixed = !bang3 && mark(0) && mark(1);
        prop[i..i + n].fill(!fixed);
        i += n;
    }
    prop
}

/// Which of jp_BDC2C's passes tags a line's ASCII runs: the text, or jp_C1080's ruby base or ruby text.
#[derive(Clone, Copy, PartialEq)]
pub enum Pass {
    Text,
    Base,
    Ruby,
}

/// The character space after `c`: inside sub_BDC2C's ASCII runs (tag 0x10) scaled by `run_spacing`; English
/// has no ASCII runs.
fn slot_space(c: char, spacing: f32) -> f32 {
    if (' '..='~').contains(&c) { spacing * run_spacing() } else { spacing }
}

/// The character space's factor inside an ASCII run, +100932 (sub_C36D8): 0.5 for Japanese, 1.0 for Chinese.
fn run_spacing() -> f32 {
    if language() == 0 { 0.5 } else { 1.0 }
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
    /// the layout's x scale (`Layout::sx`): the whole glyph squeezed from its pen
    pub sx: f32,
    /// the first character of a bar run (`Run::Bar`) carries the bar
    pub bar: Option<Bar>,
    /// the writer's italic ratio (nn::font CharWriter +36: 0.25 after `@i`, tag 14 in jp_DD334; 0 again after
    /// `@r`, tag 11): the glyph's top leans right (`italic_quad`)
    pub italic: f32,
    /// the glyph's wobble (`@a1` / `@a2`, jp_C0E4C types 1 / 2), stepped per frame by `Wobble::step`
    pub wobble: Option<Wobble>,
    /// the glyph's fill colour index (`fill_colour`; 0 the slot's colour)
    pub fill: u8,
    /// the height the glyph's italic leans over, in px (`italic_quad`; 0 its own cell's)
    pub shear: f32,
}

/// A glyph's wobble (Witch on the Holy Night's `@a1` / `@a2`; the text object's 32-byte records at +55408):
/// jp_C0684 adds each rate to its phase every frame (wrapping at 6.2832, where type 1 draws a new rate in
/// 0.0125..0.04), and jp_C3C24 offsets the glyph by amp x sin(phase) in 1920x1080 pixels. Type 1: amplitudes
/// 2.25 / 3.5, both phases random, the first rates 4 x 0.0125..0.04; type 2: only y (amplitude 4, rate
/// 0.0125..0.04, no new rates). jp_BDC2C re-arms a record on every layout of its slot but keeps the phases
/// and rates of one already of that type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wobble {
    pub kind: u8,
    pub amp: [f32; 2],
    pub phase: [f32; 2],
    pub rate: [f32; 2],
}

impl Wobble {
    /// An unarmed record of `kind` (`arm` draws its phases and rates).
    pub fn new(kind: u8) -> Self {
        let amp = if kind == 1 { [2.25, 3.5] } else { [0.0, 4.0] };
        Self { kind, amp, phase: [0.0; 2], rate: [0.0; 2] }
    }

    /// jp_C0E4C / jp_BDC2C: the record's random phases and rates (`uniform(lo, hi)`), unless `old` is a
    /// record of the same type, whose phases and rates stay.
    #[allow(clippy::approx_constant)]
    pub fn arm(&mut self, old: Option<Wobble>, mut uniform: impl FnMut(f32, f32) -> f32) {
        if let Some(o) = old.filter(|o| o.kind == self.kind) {
            self.phase = o.phase;
            self.rate = o.rate;
            return;
        }
        if self.kind == 1 {
            self.phase = [uniform(0.0, 6.2832), uniform(0.0, 6.2832)];
            self.rate = [uniform(0.0125, 0.04) * 4.0, uniform(0.0125, 0.04) * 4.0];
        } else {
            self.phase = [0.0, uniform(0.0, 6.2832)];
            self.rate = [0.0, uniform(0.0125, 0.04)];
        }
    }

    /// One 60 Hz frame (jp_C0684).
    #[allow(clippy::approx_constant)]
    pub fn step(&mut self, mut uniform: impl FnMut(f32, f32) -> f32) {
        for k in 0..2 {
            self.phase[k] += self.rate[k];
            if self.phase[k] >= 6.2832 {
                self.phase[k] -= 6.2832;
                if self.kind == 1 {
                    self.rate[k] = uniform(0.0125, 0.04);
                }
            }
        }
    }

    /// The glyph's offset in 1920x1080 pixels (jp_C3C24).
    pub fn offset(&self) -> (f32, f32) {
        (self.amp[0] * self.phase[0].sin(), self.amp[1] * self.phase[1].sin())
    }
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
    /// `under`: the ruby text held `@_` (jp_C1080: the slot's ruby mode +100968 = 2, `layout`)
    Ruby { base: String, ruby: String, under: bool },
    Newline,
    /// a run of bar tags (`[ber..]` / `[zap00]`): its text (a U+3000 per tag) under one bar from the sheet
    /// row `row` on (`Bar`)
    Bar { row: u8, text: String },
    /// `@i` (jp_BDC2C tag 14, once per slot text): the italic ratio 0.25, and in Witch on the Holy Night
    /// ASCII runs at 0.85 of the x scale instead of 0.725 (jp_DD0D8's +132, kept to the end of the text;
    /// Tsukihime's sub_DD0D8 / sub_DD334 and the overseas sub_DB650 keep 0.725)
    Italic,
    /// `@a1` / `@a2` / `@a` (jp_BDC2C: `a4` flag 0x10 ignores it): the wobble of the glyphs that follow, 0 none
    Wobble(u8),
    /// `@r` (tag 11): the italic ratio back to 0 (jp_DD334 case 0xB), no wobble and the slot's own fill (a
    /// colour node of index 0); the 0.85 stays. No line break.
    Plain,
    /// `@c` and a fill digit (`fill_colour`): the fill of the glyphs that follow, 0 the slot's own
    Fill(u8),
    /// `@c` and an edge digit: the slot's edge colour (`edge_colour`), for all of its text
    Edge(u8),
    /// `@x` (jp_BDC2C tag 12; `a4` flag 0x10, slot flag 0x40, drops it): the writer's cursor x goes to the
    /// slot's base x (+101032, jp_DD334 case 0xC). Page text opens with it to go on after the message before:
    /// `Game::show_message` places the start, the layout takes it as no move.
    Indent,
}

/// The fill of `@c1`.. (jp_BDC2C: Witch on the Holy Night's digits 1..4 index jd_3C99D2 at 0x286E46, Tsukihime's
/// 1..5 dword_3C99D2) as RGBA bytes; jp_C3C24 gives each glyph from the code on the colour of its node (or,
/// with the code alone at the text's start and no `@r` after it, jp_C35B4 the whole slot's).
pub fn fill_colour(i: u8) -> Option<[u8; 4]> {
    const WITCH: [u32; 4] = [0xff0000ff, 0xff000000, 0xffff9d00, 0xff00ffff];
    const TSUKIHIME: [u32; 5] = [0xff0000ff, 0xffff8832, 0xff101010, 0xffaaaaaa, 0xffff0000];
    let t: &[u32] = if crate::assets::witch() { &WITCH } else { &TSUKIHIME };
    t.get(usize::from(i).checked_sub(1)?).map(|c| c.to_le_bytes())
}

/// A glyph's fill: its fill code's colour (`fill_colour`) or else `rgb`, the slot's.
pub fn fill_rgb(pc: &PlacedChar, rgb: [f32; 3]) -> [f32; 3] {
    fill_colour(pc.fill).map_or(rgb, |[r, g, b, _]| [r, g, b].map(|v| f32::from(v) / 255.0))
}

/// The edge colour (RGB bytes) of `text`'s markup: its last edge code's, else black.
pub fn text_edge(text: &str) -> [u8; 3] {
    let [r, g, b, _] = edge_code(text).map_or([0, 0, 0, 255], edge_colour);
    [r, g, b]
}

/// The edge colour of an edge digit (jp_BDC2C: Witch on the Holy Night's `@c5`..`@c9` index dword_286E64,
/// Tsukihime's `@c6`..`@c9` dword_34DCB0) as RGBA bytes: the slot's +16 (jp_C5FB4's edge colour), for the text
/// of the message that holds it.
pub fn edge_colour(i: u8) -> [u8; 4] {
    const WITCH: [u32; 5] = [0xff663300, 0xff000000, 0xff000000, 0xff000000, 0xffffffff];
    const TSUKIHIME: [u32; 4] = [0xff0000ff, 0xff282828, 0xff000000, 0xffffffff];
    let t: &[u32] = if crate::assets::witch() { &WITCH } else { &TSUKIHIME };
    t.get(usize::from(i)).copied().unwrap_or(0xff000000).to_le_bytes()
}

/// The last edge code of `s`'s markup (`Run::Edge`), which colours the edge of all of its text.
pub fn edge_code(s: &str) -> Option<u8> {
    parse_markup(s).iter().rev().find_map(|r| match r {
        Run::Edge(i) => Some(*i),
        _ => None,
    })
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

/// The spacing `layout`'s pen keeps after the text's last character: nn::font's cursor ends at the glyph (a
/// slot's end cursor, +101024), the layout's pen one character spacing on (its own: half for Japanese ASCII,
/// none after an English exfont run).
pub fn trailing_gap(runs: &[Run], spacing: f32) -> f32 {
    for r in runs.iter().rev() {
        match r {
            Run::Text(t) | Run::Bar { text: t, .. } | Run::Ruby { base: t, .. } => {
                if let Some(c) = t.chars().last() {
                    return if english() { if run_kind(c).is_some() { 0.0 } else { spacing } } else { slot_space(c, spacing) };
                }
            }
            Run::Newline => return 0.0,
            _ => {}
        }
    }
    0.0
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
/// Unknown tags stay as text. The slot gets the string from sub_AB4D0, each double space a U+3000 (`slot_chars`).
pub fn parse_markup_edge(s: &str, edge: bool) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut it = slot_chars(s).peekable();
    // the bar is open while tags follow each other
    let mut bar_open = false;
    while let Some(c) = it.next() {
        if c != '[' {
            bar_open = false;
        }
        match c {
            // the overseas sub_A7790 sets +101013 for English text, and its sub_BDC2C then takes `<` as a
            // character: no ruby (Switch, English: `The <sky|Jacob's Ladder> and` drawn as written)
            '<' if !(crate::assets::overseas() && english()) => {
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
                    // sub_BDC2C parses the ruby with sub_C1FDC (every slot's +295 is 2, set by sub_A7790).
                    // Witch on the Holy Night's and the overseas one skip `@` and the character after it in the
                    // ruby text; the Japanese one copies them (Switch: `<漢字|@_かんじ>` rubied "@_かんじ").
                    // With the ASCII tags on (not English) they also drop a space read right after a U+3000 when
                    // a U+3000 follows (Switch, WoH: 「かん　 　じ」 parses to "かん　　じ"); a skipped `@x` leaves
                    // no previous character, `@_` keeps it
                    let drop_space = (crate::assets::witch() || crate::assets::overseas()) && !english();
                    let mut ruby = String::new();
                    let mut under = false;
                    let mut rt = r.chars().peekable();
                    let mut prev = None;
                    while let Some(d) = rt.next() {
                        if d == '@' && (crate::assets::witch() || crate::assets::overseas()) {
                            match rt.next() {
                                Some('_') => under = true,
                                _ => prev = None,
                            }
                            continue;
                        }
                        if drop_space && d == ' ' && prev == Some('\u{3000}') && rt.peek() == Some(&'\u{3000}') {
                            prev = None;
                            continue;
                        }
                        ruby.push(d);
                        prev = Some(d);
                    }
                    runs.push(Run::Ruby {
                        base: b.to_string(),
                        ruby,
                        under,
                    });
                } else {
                    cur.push_str(&body);
                }
            }
            '@' => {
                // control codes are one letter (plus optional digits for colours)
                if let Some(&n) = it.peek() {
                    it.next();
                    if matches!(n, 'n' | 'r' | 'i' | 'a' | 'c' | 'x') && !cur.is_empty() {
                        runs.push(Run::Text(std::mem::take(&mut cur)));
                    }
                    if n == 'n' {
                        runs.push(Run::Newline);
                    } else if n == 'r' {
                        runs.push(Run::Plain);
                    } else if n == 'i' {
                        runs.push(Run::Italic);
                    } else if n == 'x' {
                        runs.push(Run::Indent);
                    } else if n == 'a' {
                        // jp_BDC2C reads the character after `@a` whatever it is
                        runs.push(Run::Wobble(match it.next() {
                            Some('1') => 1,
                            Some('2') => 2,
                            _ => 0,
                        }));
                    } else if n == 'c' {
                        // jp_BDC2C reads the one character after `@c`: Witch on the Holy Night's 1..4 fills and
                        // 5..9 edges, Tsukihime's 1..5 fills and 6..9 edges; any other a fill node of 0
                        let d = it.next().and_then(|d| d.to_digit(10)).unwrap_or(0) as u8;
                        let fills = if crate::assets::witch() { 4 } else { 5 };
                        runs.push(if (1..=fills).contains(&d) { Run::Fill(d) } else if d > fills { Run::Edge(d - fills - 1) } else { Run::Fill(0) });
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
/// plain variants œ / ½ while the slot's edge switch (+101003; Witch on the Holy Night's and the overseas
/// +101007) is off, else the edged Œ / ¼ (`edged`).
const EX_DASH: char = 'œ';
const EX_BOX: char = '½';

/// The edged exfont glyph for `c` (`EX_DASH` / `EX_BOX`), which sub_BDC2C and the ruby parsers pick while the
/// slot has the edge (the text edge setting, flags 0x200000 / 0x80000): the same cell with a border at half
/// coverage, which nn::font draws in the edge colour (`ex_quad`). Switch, overseas Japanese with the edge: the
/// tagged line holds Œ, and a dash, a run or not, has a 1 px dark edge above and below its 3 white rows.
fn edged(c: char) -> char {
    match c {
        EX_DASH => 'Œ',
        EX_BOX => '¼',
        c => c,
    }
}

/// Whether exfont glyph `c` carries its own border at half coverage (the edged dash and box, the inline glyph
/// tags' edged set).
fn bordered(c: char) -> bool {
    c == edged(EX_DASH) || c == edged(EX_BOX) || EX_TAGS.iter().any(|t| t.2 == c)
}

/// Whether `n` run characters of exfont glyph `g` (`run_kind`) in `pass` are one stretched run (tag 13). The
/// ruby parsers (sub_C1080 for a base, sub_C1FDC for ruby text) run dashes of any length and two or more boxes;
/// the message text's (sub_BDC2C; Witch on the Holy Night's jp_BDC2C, the overseas 0xBC8C0) runs only two or
/// more of either and copies a single dash as the plain exfont glyph (`lone_dashes`), leaving a single box ■.
fn makes_run(g: char, n: usize, pass: Pass) -> bool {
    n >= 2 || g == EX_DASH && pass != Pass::Text
}

/// `chars` with each single dash of the message text (`makes_run`) as the exfont glyph sub_BDC2C copies in
/// its place, drawn in its cell like any glyph. Switch, overseas Japanese: 「あ―い」's tagged line holds Œ with
/// no tag 13 and the dash is drawn 3 px left of a one-cell run's.
fn lone_dashes(mut chars: Vec<char>, pass: Pass, has_ex: impl Fn(char) -> bool) -> Vec<char> {
    if pass == Pass::Text && has_ex(EX_DASH) {
        for i in 0..chars.len() {
            let dash = |j: Option<usize>| j.and_then(|j| chars.get(j)).is_some_and(|&d| run_kind(d) == Some(EX_DASH));
            if dash(Some(i)) && !dash(i.checked_sub(1)) && !dash(Some(i + 1)) {
                chars[i] = EX_DASH;
            }
        }
    }
    chars
}

fn run_kind(c: char) -> Option<char> {
    match c {
        '―' | '─' | '—' => Some(EX_DASH),
        '■' => Some(EX_BOX),
        _ => None,
    }
}

/// Ruby scale (dword_3C9430[language]: Japanese 0.52, English 0.55, Chinese 0.51) of the text's glyph scale
/// and spacing, and the ruby cursor's usual extra y (slot +101748 = -2 * H / 1080 from the slot init's
/// +100924; `Layout::ruby_y`). Witch on the Holy Night's table (dword_2869F4) has Japanese at 0.45.
const RUBY_SCALE: [f32; 5] = [0.52, 0.55, 0.51, 0.51, 0.45];
const RUBY_SCALE_WITCH: [f32; 5] = [0.45, 0.55, 0.51, 0.51, 0.45];
pub const RUBY_Y: f32 = -2.0;

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
    /// the writer's x scale relative to `size` (Witch on the Holy Night's English bubbles: 0.85 / 0.95)
    pub sx: f32,
    pub line_height: f32,
    /// the scaled pair-font ascent: a line's baseline below its box top
    pub ascent: f32,
    /// the ruby cursor's extra y in px (slot +101748 / WoH +101756 from +100924: `RUBY_Y`, Witch on the Holy
    /// Night's backlog slots -6.75, sub_861A0)
    pub ruby_y: f32,
}

/// The height nn::font's italic leans a message's glyphs over when its last glyph is at `size`: the draw
/// buffer's +188 from PrintGlyph (jp_13CEF8: scale.y x the pair font's GetHeight, vtable +40, the line feed,
/// plus scale.y).
fn italic_height(size: f32) -> f32 {
    (line_feed() + 1.0) * size / BASE_SIZE
}

/// nn::font's italic (CharWriter::PrintGlyph): the glyph's record carries its shear, the whole pixels of the
/// italic ratio x the writer's x scale x the font's width (+48, an int; vtable +32: `pair_width`), and the
/// draw buffer one height (`italic_height`), which every glyph sets and the draw of the message's batch uses
/// for all of them: the top of a glyph's quad (half a scaled pixel above its glyph cache cell, whose baseline
/// is the ScalableFont's own baseline position + 1 down, `GLYPH_BASELINE`) moves right by its shear, less its
/// shear for every such height of the last glyph below that (`PlacedChar::shear`). A message that ends with a ruby leans every italic glyph over the ruby's height,
/// 0.52 of the text's (Switch, overseas: `@iあいう@rかき<漢字|かんじ>` leans あいう 1.94 times as steeply,
/// level 0.52 of a cell below its top; `@iあいう@r<漢字|かんじ>かき` does not). Switch, overseas Simplified
/// Chinese (line feed 49, ascent 39): `@i我們說――話了@r的人<漢字|中―文>` leans 0.378 px a row, 11 px over
/// 50 x the ruby's scale, level 40.8 base px above the baseline; English (ascent 43, baseline position 40):
/// italic text 0.5 px further right than from the ascent; Traditional Chinese (the pair's baseline position the
/// exfont's 37, SungASC's own 43): italic text 1.4 px left of the pair's, 2.7 px with a ruby at the end. An
/// exfont glyph (`ex`) leans from `EX_SHEAR_TOP`.
fn italic_quad(q: Quad, pc: &PlacedChar, dy: f32, ex: bool) -> Quad {
    if pc.italic == 0.0 {
        return q;
    }
    let k = pc.size / BASE_SIZE;
    // a run's glyph shears at the x scale tag 0xD stretched it to (Switch, `@i` over four dashes: the run
    // 18 px right of the unstretched shear's)
    let scale = match pc.run {
        Some(r) => run_scale(r, pc.size),
        None => k,
    };
    let off = (pc.italic * pair_width() * scale * pc.sx).trunc();
    let cell = GLYPH_BASELINE.load(Ordering::Relaxed) as f32 + 1.0;
    let top = pc.baseline + dy - if ex { EX_SHEAR_TOP } else { cell + 0.5 } * k;
    let h = if pc.shear > 0.0 { pc.shear } else { italic_height(pc.size) };
    Quad { pos: q.pos.map(|[x, y]| [x + off * (1.0 - (y - top) / h), y]), ..q }
}

/// nn::font's italic ratio after `@i` (jp_DD334 case 0xE: CharWriter +36 = 0.25).
const ITALIC_RATIO: f32 = 0.25;

/// The writer's markup state while a message is laid out.
#[derive(Default)]
struct Style {
    /// the italic ratio (`ITALIC_RATIO` after `@i`, 0 after `@r`)
    italic: f32,
    /// Witch on the Holy Night's +132 (jp_DD0D8 / jp_DD334) and the overseas +136 (sub_DB650): set by tag 14
    /// (`@i`) and kept to the end of the text, as `@r` is tag 11 (italic ratio 0) and only sub_DC364, before
    /// each pass of the writers, clears it. It scales Witch on the Holy Night's ASCII runs by 0.85 and moves
    /// both builds' exfont runs (`run_shifts`). Switch, overseas: 「かき■■すと@iか@rき■■す」 draws its second
    /// box run 4.5 px left and the す after it 4.5 px right of the first's spacing
    seen: bool,
    /// the wobble type of the glyphs (`Wobble`)
    wobble: u8,
    /// the fill colour index (`fill_colour`, 0 the slot's)
    fill: u8,
}

impl Style {
    /// The x scale jp_DD0D8 gives an ASCII run (tags 16 / 26 / 27): 0.725, 0.85 after Witch on the Holy
    /// Night's `@i`.
    fn ascii_scale(&self) -> f32 {
        if self.seen && crate::assets::witch() { 0.85 } else { 0.725 }
    }
}

/// Each character's pen step in message text: (its cell width, its glyph x scale, the spacing after it).
/// Japanese and Chinese ASCII runs follow `slot_cell`'s rule, which sub_BDC2C and the ruby pass (sub_C1080)
/// set up for the message writer too: a proportional run's glyph its own advance, a fixed one (tag 26, and
/// every run in Tsukihime, whose writer keeps +131 clear) half the fixed width (the glyph size), at the run's x
/// scale (`Style::ascii_scale`) and half the spacing (`slot_space`); other characters the fixed width (jp_C406C:
/// the writer's +56 on unless +101009, English, with +776, the glyph size; nn::font centres each glyph's
/// advance in it) and the full spacing; English ones their advance. Switch, the shared font's half-width 「
/// (advance 512 of 1024) in NZ6's `@z` text: centred in a 44.4 px cell, the next glyph 22 px further on. The
/// writer is not free (+289 clear: no kerning; Switch, 「dies. Te」 without the font's T-e
/// pair), and nn::font's TextWriter moves the cursor by the character space before each glyph but a line's
/// first, at the spacing the glyph prints with: the gap after a character is the next one's (`next` after
/// the last; Switch, 「“Vivo」's V 2 px nearer the quote, the ” after an ASCII run 2 px further). The ruby
/// writer has no fixed width (sub_DDA1C clears its +56), so each ruby glyph advances by its own width (every
/// non-ASCII ruby character in the scripts is a full em).
#[allow(clippy::too_many_arguments)]
fn steps(atlas: &FontAtlas, chars: &[char], next: Option<char>, pass: Pass, size: f32, sx: f32, spacing: f32, st: &Style) -> Vec<(f32, f32, f32)> {
    let prop = prop_runs(chars, pass);
    let gap = |c: char| if pass == Pass::Ruby && !ruby_runs_scaled() { spacing } else { slot_space(c, spacing) };
    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let after = gap(chars.get(i + 1).copied().or(next).unwrap_or(c));
            if english() {
                return (atlas.advance(c, size) * sx, sx, after);
            }
            if !(' '..='~').contains(&c) {
                return (size * sx, sx, after);
            }
            let k = match pass {
                Pass::Ruby if !ruby_runs_scaled() => sx,
                Pass::Ruby => sx * 0.725,
                _ => sx * st.ascii_scale(),
            };
            if prop[i] || pass == Pass::Ruby {
                (atlas.advance(c, size) * k, k, after)
            } else {
                (size * sx * 0.5, k, after)
            }
        })
        .collect()
}

/// A ruby base's width as sub_DDA1C measures it (the ruby centres on the pen after the base less half of it):
/// with a fresh tag processor (jp_DCFF8's fields: +131 set, +136 / +140 = 0.5), so an ASCII run opened by tag
/// 16 measures proportional at the run's x scale, with half the spacing, though Tsukihime draws it in fixed
/// half cells and Chinese at the full spacing (a tag 26 run of Witch on the Holy Night keeps its half cells);
/// an exfont run measures nothing (jp_DD0D8's tag 0xD). Switch, Witch on the Holy Night in Traditional
/// Chinese: 「儘管傾盡智慧──」 over a 53-character Latin base 50 px right of the drawn base's centre.
fn ruby_base_width(atlas: &FontAtlas, chars: &[char], lay: &Layout, st: &Style) -> f32 {
    if english() {
        let w: f32 = chars.iter().map(|&c| atlas.advance(c, lay.size) * lay.sx + lay.spacing).sum();
        return w - lay.spacing;
    }
    let prop = prop_runs(chars, Pass::Base);
    let ascii = |c: char| (' '..='~').contains(&c);
    let (mut w, mut first, mut i) = (0.0, true, 0);
    while i < chars.len() {
        let c = chars[i];
        let n = chars[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count();
        if run_kind(c).is_some_and(|g| atlas.has_ex(g) && makes_run(g, n, Pass::Base)) {
            i += n;
            continue;
        }
        let k = lay.sx * st.ascii_scale();
        let half = crate::assets::witch() && !prop[i];
        w += match ascii(c) {
            true if half => lay.size * lay.sx * 0.5,
            true => atlas.advance(c, lay.size) * k,
            false => lay.size * lay.sx,
        };
        if !std::mem::take(&mut first) {
            w += if ascii(c) { lay.spacing * 0.5 } else { lay.spacing };
        }
        i += 1;
    }
    w
}

/// Whether the ruby text's ASCII runs take the run's x scale and spacing (`ruby_runs_scaled`): Tsukihime's
/// sub_DD334 / sub_DD0D8 scale every run by 0.725 and the spacing by +128 (Switch: `<漢字|@_かんじ>` ruby
/// "@_" at 0.725); Witch on the Holy Night's and the overseas tag processors skip it while the ruby flag
/// sub_DDA1C sets is on (+126 / +130), so their runs keep the ruby writer's scale and spacing.
fn ruby_runs_scaled() -> bool {
    !crate::assets::witch() && !crate::assets::overseas()
}

/// The first character the runs after `runs[i]` print on the same line (`steps`'s `next`).
fn next_char(runs: &[Run], i: usize) -> Option<char> {
    for r in runs.get(i + 1..)? {
        match r {
            Run::Text(t) | Run::Bar { text: t, .. } | Run::Ruby { base: t, .. } => {
                if let Some(c) = t.chars().next() {
                    return Some(c);
                }
            }
            Run::Italic | Run::Wobble(_) | Run::Plain | Run::Fill(_) | Run::Edge(_) | Run::Indent => {}
            _ => return None,
        }
    }
    None
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
    // the writer's style from the markup (`Run::Italic` / `Run::Wobble` / `Run::Plain`)
    let mut style = Style::default();
    let start = out.len();
    // the slot's ruby mode (+100968, reset by jp_BDA9C): the ruby writer reads it at the draw, for every ruby
    let under = runs.iter().any(|r| matches!(r, Run::Ruby { under: true, .. }));
    // the line starts (sub_BF0F8, `plan_lines`), by the index of each `push`
    let plan = plan_lines(atlas, lay, runs, x);
    let mut flow = Flow::new(atlas, runs, x > lay.left);
    let unit = std::cell::Cell::new(0usize);
    // `n` > 0: a run of n source chars `c` drawn as one stretched exfont glyph (`ex`) over n cells. `cell`:
    // the glyph's cell width and x scale when not the layout's (an ASCII glyph, `ascii_cell`), the glyph
    // centred in it
    let push = |c: char, ex: Option<(char, u8)>, cell: Option<(f32, f32)>, after: f32, st: &Style, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| {
        let n = ex.map_or(1, |e| e.1) as f32;
        // a run crosses n full-width cells
        let adv = match cell {
            Some((w, _)) => w,
            None => (if ex.is_some() { lay.size } else { atlas.advance(c, lay.size) }) * lay.sx,
        };
        let width = n * (adv + lay.spacing) - lay.spacing;
        // the fixed-width writer's cell, and sub_DD334's pen moves around an exfont run (English: `english_text`)
        let fixed = lay.size * lay.sx;
        let shifts = ex.filter(|_| plan.is_some()).map(|e| run_shifts(e.1, lay, st));
        let i = unit.replace(unit.get() + 1);
        let wrap = match &plan {
            Some(p) => p.starts.get(i).copied().unwrap_or(false),
            // nn::font sizes a line in characters (sub_BD7EC): N = floor((W - g / 2) / (g + spacing)) + 1 for the
            // slot width W and the glyph width g, i.e. a character fits while its cell starts at most W - g / 2
            // from the left (28 full-width glyphs in the 464-unit message slot, 17 in the save note's 130
            // units); kinsoku chars never start a line, so they overhang the right edge
            None => *x + (n - 1.0) * (adv + lay.spacing) + lay.size * lay.sx / 2.0 > lay.right && !no_line_start(c),
        };
        // nn::font's own wrap (jp_145B78) at the writer's width limit (+60, set by sub_C5FB4): the slot width plus
        // (+100952 + 2) / 1920 of the screen (+100952 = 3, sub_BCDB0) plus allowance + 1 cells; a glyph or exfont
        // run whose right edge passes it starts a line unless it is the line's first. It shows when a break
        // was dropped (Switch, Witch on the Holy Night: 「…となに――――――らりるれろ」 breaks before り)
        // An exfont run is its glyph at the stretched x scale after tag 0xD has moved the pen on by `before`
        // (the writer checks the glyph's fixed cell there); a run wrapped at the limit loses that move, so its
        // glyph sits centred on the new line's first cell, reaching past the left edge (Switch: ten dashes
        // after 25 kana are drawn from x 58 on the next line, the kana after them at 547).
        let limit = lay.right - lay.left + 5.0 + (fixed + lay.spacing) * (kinsoku_rules().allow + 1) as f32;
        let limited = !wrap
            && plan.is_some()
            && match shifts {
                Some((before, _)) => *x + before + fixed - lay.left > limit,
                None => *x > lay.left && *x + width - lay.left > limit,
            };
        if wrap || limited {
            *x = lay.left;
            *baseline += lay.line_height;
        }
        // a run's glyph: centred on the cell at the pen (`run_quad` centres it over n cells from `gx`)
        let pen = match shifts {
            Some((before, _)) if !limited => *x + before,
            _ => *x,
        };
        let (gx, sx) = match cell {
            Some((w, k)) => (*x + (w - atlas.advance(c, lay.size) * k) / 2.0, k),
            None if shifts.is_some() => (pen + (fixed - n * (lay.size + lay.spacing)) / 2.0, lay.sx),
            None => (*x, lay.sx),
        };
        out.push(PlacedChar {
            c: ex.map_or(c, |e| e.0),
            x: gx,
            baseline: *baseline,
            size: lay.size,
            order: *order,
            run: ex.map(|e| (e.1, lay.spacing)),
            stretch: if ex.is_none() && c == '…' { (lay.size * lay.sx + lay.spacing) / (lay.size * lay.sx) } else { 1.0 },
            sx,
            bar: None,
            italic: st.italic,
            wobble: (st.wobble > 0).then(|| Wobble::new(st.wobble)),
            fill: st.fill,
            shear: 0.0,
        });
        // the run is one character to the reveal
        *order += 1;
        *x = match shifts {
            Some((_, back)) => pen + fixed + back + after,
            None => *x + width + after,
        };
    };
    let push_char = |c: char, st: &Style, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| {
        push(c, None, None, lay.spacing, st, x, baseline, order, out)
    };
    let text = |t: &str, pass: Pass, next: Option<char>, st: &mut Style, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>, flow: &mut Flow| {
        let chars = lone_dashes(t.chars().collect(), pass, |g| atlas.has_ex(g));
        if english() {
            let first = out.len();
            english_text(atlas, lay, &chars, pass, st, (x, baseline), order, out, flow);
            for pc in &mut out[first..] {
                pc.wobble = (st.wobble > 0).then(|| Wobble::new(st.wobble));
                pc.fill = st.fill;
            }
            return;
        }
        let step = steps(atlas, &chars, next, pass, lay.size, lay.sx, lay.spacing, st);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let n = chars[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
            match run_kind(c) {
                Some(g) if atlas.has_ex(g) && makes_run(g, n, pass) => {
                    push(c, Some((g, n as u8)), None, step[i + n - 1].2, st, x, baseline, order, out);
                    i += n;
                }
                _ => {
                    // an ASCII run of sub_BDC2C (tags 16 / 26 / 27), or a glyph in the fixed width
                    let (cell, k, after) = step[i];
                    push(c, None, Some((cell, k)), after, st, x, baseline, order, out);
                    i += 1;
                }
            }
        }
    };
    for (ri, run) in runs.iter().enumerate() {
        let next = next_char(runs, ri);
        match run {
            Run::Newline => {
                x = lay.left;
                baseline += lay.line_height;
                flow.newline();
            }
            Run::Text(t) => text(t, Pass::Text, next, &mut style, &mut x, &mut baseline, &mut order, out, &mut flow),
            Run::Italic => {
                style.italic = ITALIC_RATIO;
                style.seen = crate::assets::witch() || crate::assets::overseas();
            }
            Run::Wobble(k) => style.wobble = *k,
            Run::Plain => {
                style.italic = 0.0;
                style.wobble = 0;
                style.fill = 0;
            }
            Run::Fill(i) => style.fill = *i,
            Run::Edge(_) | Run::Indent => {}
            Run::Bar { row, text: t } => {
                let first = out.len();
                text(t, Pass::Text, next, &mut style, &mut x, &mut baseline, &mut order, out, &mut flow);
                if let Some(pc) = out.get_mut(first) {
                    let len = t.chars().count().min(usize::from(u8::MAX)) as u8;
                    pc.bar = Some(Bar { row: *row, len, top: pc.baseline - lay.ascent, cell: lay.size * lay.sx + lay.spacing });
                }
            }
            Run::Ruby { base, ruby, .. } => {
                let bchars: Vec<char> = base.chars().collect();
                let bstep = steps(atlas, &bchars, next, Pass::Base, lay.size, lay.sx, lay.spacing, &style);
                let bw: f32 = bstep.iter().map(|s| s.0 + s.2).sum();
                // the base's last character must fit (the line size in characters above)
                let last = bstep.last().map_or(0.0, |s| s.0 + s.2);
                if plan.is_none() && x + bw - last + lay.size * lay.sx / 2.0 > lay.right {
                    x = lay.left;
                    baseline += lay.line_height;
                }
                let first = order;
                // sub_C1080 lays the base out as the text around it (ASCII runs, dash runs)
                if english() {
                    for c in base.chars() {
                        let (at, y) = (out.len(), baseline);
                        push_char(c, &style, &mut x, &mut baseline, &mut order, out);
                        flow.placed(at, out.get(at).map_or(x, |pc| pc.x), baseline != y);
                    }
                } else {
                    text(base, Pass::Base, next, &mut style, &mut x, &mut baseline, &mut order, out, &mut flow);
                }
                // sub_DDA1C: the ruby writer at 0.52 of the text's scale and spacing, centred over the base (the
                // spread flag +101008 is 0 in sub_A7790's slot init); nn::font string widths have no trailing
                // spacing. Its cursor y is the base writer's less the scaled pair font's GetBaselinePos (vtable
                // +200: the message pair's, `sfnt::SetMetrics::baseline`) and +101748 (-2 px); both inks sit
                // `GLYPH_DROP` cache pixels below their cursors at their own scales. Switch: the ruby 45.37 px
                // above the base on the Japanese train page (まえぶ, B 41 -> 45.29), Traditional Chinese 41.15
                // (約定, the exfont's 37 -> 40.73).
                let table = if crate::assets::witch() { &RUBY_SCALE_WITCH } else { &RUBY_SCALE };
                let rk = table.get(language()).copied().unwrap_or(table[0]);
                let (rs, rsp) = (lay.size * rk, lay.spacing * rk);
                let rchars: Vec<char> = ruby.chars().collect();
                let rstep = steps(atlas, &rchars, None, Pass::Ruby, rs, lay.sx, rsp, &style);
                // an exfont run of the ruby text (sub_C1080's tag 13, as in the base): (its glyph, its length)
                let ex_run = |i: usize| {
                    let c = rchars[i];
                    let n = rchars[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
                    run_kind(c).filter(|&g| atlas.has_ex(g) && makes_run(g, n, Pass::Ruby)).map(|g| (g, n))
                };
                // the ruby's width: jp_DD0D8's tag 0xD skips an exfont run, so the string width leaves it out
                let mut kept = Vec::new();
                let mut i = 0;
                while i < rchars.len() {
                    match ex_run(i) {
                        Some((_, m)) => i += m,
                        None => {
                            kept.push(rchars[i]);
                            i += 1;
                        }
                    }
                }
                let mstep = steps(atlas, &kept, None, Pass::Ruby, rs, lay.sx, rsp, &style);
                let rw: f32 = mstep.iter().map(|s| s.0 + s.2).sum::<f32>() - mstep.last().map_or(0.0, |s| s.2);
                let n = ruby.chars().count().max(1) as f32;
                // mode 2 (`@_` anywhere in the slot's rubies): from the base's start plus half the writer's
                // font width (jp_13BCF0: scale.x x the pair font's width, `pair_width`), else centred; an italic
                // writer moves the ruby right by half its shear. The ruby writer's italic ratio is the text's
                // over the ruby scale (jp_DDA1C), so the top of each ruby glyph moves right by the base's full
                // shear (PrintGlyph's +48: ratio x scale.x x width) and leans over the message's shear height
                // (`italic_quad`; Switch, the chant's ruby: 6.3 px right of a glyph sheared at the text's
                // ratio, the same slope)
                let half = pair_width() * lay.size / BASE_SIZE * lay.sx * 0.5;
                // the ruby writer's ratio (+36): the overseas sub_DB650 sets it to 0.25 / n for a run's glyph
                // and to 0.25 after it once `@i` was seen, the ruby writer having no fixed width
                let mut rit = style.italic / rk;
                // the base's start: the pen after it less its width as sub_DDA1C measures it
                let bm = ruby_base_width(atlas, &bchars, lay, &style);
                let from = x - bstep.last().map_or(0.0, |s| s.2) - bm;
                let mut rx = if under { from + half } else { from + (bm - rw) / 2.0 } + style.italic * half;
                let b = MSG_BASELINE.load(Ordering::Relaxed) as f32 + (1.0 - rk) * GLYPH_DROP;
                let rb = baseline - b * lay.size / BASE_SIZE - lay.ruby_y;
                let span = (order - first).max(1);
                let mut k = 0;
                while k < rchars.len() {
                    let (c, (cell, gsx, _)) = (rchars[k], rstep[k]);
                    let order = first + (k * span / n as usize).min(span - 1);
                    let pc = PlacedChar {
                        c,
                        x: rx + (cell - atlas.advance(c, rs) * gsx) / 2.0,
                        baseline: rb,
                        size: rs,
                        order,
                        run: None,
                        stretch: 1.0,
                        sx: gsx,
                        bar: None,
                        italic: rit,
                        wobble: (style.wobble > 0).then(|| Wobble::new(style.wobble)),
                        fill: style.fill,
                        shear: 0.0,
                    };
                    match ex_run(k) {
                        Some((g, m)) => {
                            let (before, adv, back) = ruby_run_shifts(m as u8, rs * lay.sx / BASE_SIZE, rsp, &style, rit);
                            let pen = rx + before;
                            let run = (m as u8, rsp);
                            let free = crate::assets::overseas() && style.seen;
                            let italic = if free { ITALIC_RATIO / m as f32 } else { rit };
                            out.push(PlacedChar { c: g, x: pen - atlas.run_inset(g, run, rs), run: Some(run), sx: lay.sx, italic, ..pc });
                            if free {
                                rit = ITALIC_RATIO;
                            }
                            rx = pen + adv + back + rstep[k + m - 1].2;
                            k += m;
                        }
                        None => {
                            out.push(pc);
                            rx += cell + rstep[k].2;
                            k += 1;
                        }
                    }
                }
            }
        }
    }
    // a line start after the last character (the text's last line ending at its run's 0x11 tag)
    if plan.is_some_and(|p| p.trailing) {
        x = lay.left;
        baseline += lay.line_height;
    }
    // the draw buffer's shear height at the message's draw: its last glyph's (a ruby's after the base)
    if let Some(h) = out[start..].last().map(|pc| italic_height(pc.size)) {
        for pc in &mut out[start..] {
            pc.shear = h;
        }
    }
    (x, baseline, order)
}

/// Japanese and Chinese text's line starts for `layout`'s runs from pen x `x`: `starts[i]` when the `i`th glyph
/// or exfont run `layout` pushes starts a line, `trailing` when a line starts after the last. sub_BF0F8's plain
/// copy of the slot text (`kinsoku::breaks`): the base characters (ruby text left out) with sub_BDC2C's ASCII
/// run tags (`run_tag`) around each run of printable ASCII, a run closed at every markup code, ruby and bar; an
/// exfont run as its glyph repeated; a bar between its tags 0x16 (ber; zap 0x19) and 0x17, the QA bubble text
/// (style 0x40, Tsukihime) after tag 0x18; line feeds. The first line runs from the pen to the slot's right
/// edge. English text keeps nn::font's own breaking (`english_text`).
struct LinePlan {
    starts: Vec<bool>,
    trailing: bool,
}

/// The tag sub_BDC2C opens an ASCII run with at `chars[i]`: 0x10, and in Witch on the Holy Night (jp_BDC2C /
/// jp_C1080 for a ruby base) 0x1B for "!!!" (not in a base), 0x1A for two of "!?".
fn run_tag(chars: &[char], i: usize, pass: Pass) -> char {
    if !crate::assets::witch() {
        return '\u{10}';
    }
    let at = |j: usize| chars.get(i + j).copied();
    let mark = |j: usize| matches!(at(j), Some('!' | '?'));
    if pass == Pass::Text && at(0) == Some('!') && at(1) == Some('!') && at(2) == Some('!') {
        '\u{1b}'
    } else if mark(0) && mark(1) {
        '\u{1a}'
    } else {
        '\u{10}'
    }
}

/// sub_DD334 (Witch on the Holy Night's jp_DD334, the overseas sub_DB650) around an exfont run of `n` in a
/// fixed-width writer: tag 0xD stretches the x scale to k (W n + sp (n - 1)) / W (`run_quad`; W the pair font's
/// scaled width, sp the spacing) and moves the pen by `before` so that the glyph nn::font centres in its cell
/// covers the run; tag 6, after the glyph's cell, moves it by `after`. Italic (tag 0xE: the writer's +36 =
/// 0.25 and, in Witch on the Holy Night and the overseas build, +132 / +136) changes both.
fn run_shifts(n: u8, lay: &Layout, st: &Style) -> (f32, f32) {
    let n = f32::from(n);
    let s = lay.size * lay.sx / BASE_SIZE;
    let w = pair_width() * s;
    let sp = lay.spacing;
    // +68: the stretched glyph's width and its spacings past the cell's
    let wide = run_stretch() * (w * n + sp * (n - 1.0)) + sp * (n - 1.0) * s - w;
    let flag = st.seen;
    let before = ((2.0 * sp * s + wide) / 2.0 - sp * (n - 1.0) / 2.0 - 1.0) / (st.italic + 1.0) - if flag { 0.1 * wide } else { 0.0 };
    let back = before + before - before / (st.italic + 1.0) + if flag { 0.3 * wide } else { 0.0 };
    (before, back - sp * s)
}

/// sub_DD334's pen moves around an exfont run of `n` in the ruby text (the ruby writer at x scale `s`, spacing
/// `sp`): sub_DDA1C clears the writer's fixed width and sets the ruby flag (+118; Witch on the Holy Night's
/// +126, the overseas +130). Tag 0xD's move is a quarter of the fixed writer's over (italic + 1), less half the
/// run's extra width D (0.4 D in Witch on the Holy Night and the overseas build, and 0.1 D more after `@i`,
/// `Style::seen`); the stretched glyph advances by its own width; tag 6 moves the pen by a quarter of (that
/// move + D, 0.7 D more after `@i`) less the scaled spacing. `italic` is the ruby writer's ratio at the run.
/// Returns (tag 0xD's move, the glyph's advance, tag 6's move).
fn ruby_run_shifts(n: u8, s: f32, sp: f32, st: &Style, italic: f32) -> (f32, f32, f32) {
    let n = f32::from(n);
    let w = pair_width() * s;
    let adv = run_stretch() * (w * n + sp * (n - 1.0));
    let wide = adv + sp * (n - 1.0) * s - w;
    let later = crate::assets::overseas() || crate::assets::witch();
    let flag = st.seen;
    let fixed = ((2.0 * sp * s + wide) / 2.0 - sp * (n - 1.0) / 2.0 - 1.0) * 0.25 / (italic + 1.0);
    let before = fixed - if later { 0.4 } else { 0.5 } * wide - if flag { 0.1 * wide } else { 0.0 };
    let back = 0.25 * (before + wide + if flag { 0.7 * wide } else { 0.0 }) - sp * s;
    (before, adv, back)
}

/// The x scale sub_DD334's tag 0xD gives the writer for an exfont run of `n` cells at `size` (before the
/// layout's own x scale): the run's cells and spacings over the font width, at `run_stretch`.
fn run_scale((n, spacing): (u8, f32), size: f32) -> f32 {
    let n = f32::from(n);
    let w = pair_width() * size / BASE_SIZE;
    (w * n + spacing * (n - 1.0)) * size / BASE_SIZE / w * run_stretch()
}

/// tag 0xD's x scale factor: 0.965 in the Japanese build's sub_DD334, 0.98 in the others'
fn run_stretch() -> f32 {
    if crate::assets::overseas() || crate::assets::witch() { 0.98 } else { 0.965 }
}

/// The build's kinsoku lists and allowance.
fn kinsoku_rules() -> &'static kinsoku::Rules {
    if crate::assets::witch() { &kinsoku::WITCH } else { &kinsoku::TSUKIHIME }
}

fn plan_lines(atlas: &FontAtlas, lay: &Layout, runs: &[Run], x: f32) -> Option<LinePlan> {
    plan_lines_with(|c| atlas.has_ex(c), |c| atlas.advance(c, lay.size), atlas.bubble, lay, runs, x)
}

/// `plan_lines` over the atlas's exfont glyphs (`has_ex`) and advances at the layout size (`advance`).
fn plan_lines_with(has_ex: impl Fn(char) -> bool, advance: impl Fn(char) -> f32, bubble: bool, lay: &Layout, runs: &[Run], x: f32) -> Option<LinePlan> {
    if english() {
        return None;
    }
    let lb = LINE_BREAK.get()?;
    let witch = crate::assets::witch();
    let mut plain = String::new();
    if bubble && !witch {
        plain.push('\u{18}');
    }
    // each pushed unit's byte offsets in `plain`: its first and last copy (an exfont run's glyph repeated)
    let mut offs = Vec::new();
    let mut lasts = Vec::new();
    let mut open = false;
    let close = |plain: &mut String, open: &mut bool| {
        if std::mem::take(open) {
            plain.push('\u{11}');
        }
    };
    let chars_of = |plain: &mut String, open: &mut bool, offs: &mut Vec<usize>, lasts: &mut Vec<usize>, t: &str, pass: Pass| {
        let chars = lone_dashes(t.chars().collect(), pass, &has_ex);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let n = chars[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
            let (g, n) = match run_kind(c) {
                Some(g) if has_ex(g) && makes_run(g, n, pass) => (g, n),
                _ => (c, 1),
            };
            if (' '..='~').contains(&g) {
                if !*open {
                    plain.push(run_tag(&chars, i, pass));
                    *open = true;
                }
            } else if std::mem::take(open) {
                plain.push('\u{11}');
            }
            offs.push(plain.len());
            lasts.push(plain.len() + (n - 1) * g.len_utf8());
            for _ in 0..n {
                plain.push(g);
            }
            i += n;
        }
    };
    for run in runs {
        match run {
            Run::Text(t) => chars_of(&mut plain, &mut open, &mut offs, &mut lasts, t, Pass::Text),
            Run::Bar { text, .. } => {
                close(&mut plain, &mut open);
                plain.push('\u{16}');
                chars_of(&mut plain, &mut open, &mut offs, &mut lasts, text, Pass::Text);
                close(&mut plain, &mut open);
                plain.push('\u{17}');
            }
            Run::Ruby { base, .. } => {
                close(&mut plain, &mut open);
                chars_of(&mut plain, &mut open, &mut offs, &mut lasts, base, Pass::Base);
                close(&mut plain, &mut open);
            }
            Run::Newline => {
                close(&mut plain, &mut open);
                offs.push(plain.len());
                lasts.push(plain.len());
                plain.push('\n');
            }
            _ => close(&mut plain, &mut open),
        }
    }
    close(&mut plain, &mut open);
    // the line feed unit is no push of `layout`'s
    let newline: Vec<bool> = {
        let b = plain.as_bytes();
        offs.iter().map(|&o| b.get(o) == Some(&b'\n')).collect()
    };
    // Witch on the Holy Night's proportional runs at the run's x scale (the plain copy carries no wide tag)
    let prop = |c: char| advance(c) * lay.sx * 0.725;
    let m = kinsoku::Measure {
        cell: lay.size * lay.sx,
        spacing: lay.spacing,
        run_spacing: run_spacing(),
        prop: witch.then_some(&prop as &dyn Fn(char) -> f32),
    };
    let breaks = kinsoku::breaks(lb, plain.as_bytes(), lay.right - x, lay.right - lay.left, &m, kinsoku_rules());
    let mut starts = vec![false; offs.len()];
    let mut trailing = false;
    for b in breaks {
        // sub_BF0F8 keeps a break only where its plain copy has a table entry: an exfont run has one at its
        // first copy (the tag 0xD) and its last (the character), and sub_C5610 moves either before the 0xD, so
        // the whole run starts the line; a break between them is dropped (Switch, Witch on the Holy Night:
        // 「…思うけど―――」 keeps its run on the line, 「…てと――――――――。」 starts the next with it)
        if let Some(i) = offs.iter().zip(&lasts).position(|(&o, &l)| o <= b && b <= l) {
            if b == offs[i] || b == lasts[i] {
                starts[i] = true;
            }
            continue;
        }
        match offs.iter().position(|&o| o >= b) {
            Some(i) => starts[i] = true,
            None => trailing = true,
        }
    }
    let starts = starts.into_iter().zip(newline).filter(|&(_, nl)| !nl).map(|(s, _)| s).collect();
    Some(LinePlan { starts, trailing })
}

/// The characters of a message's English text as sub_BF0F8 hands them to the line breaker: the slot's whole
/// string, not each markup run (Switch, overseas: "…bien trop @ieffrontées@r." breaks before the italic word,
/// "…lit blanc @id'hôp@rital." would break before d'), with where each was placed so far.
struct Flow {
    /// the text runs', bars' and ruby bases' characters in order (as `layout` takes them), a line feed per `@n`
    chars: Vec<char>,
    /// per placed character: its index in `out` and the pen it was drawn from (none inside an exfont run)
    at: Vec<Option<(usize, f32)>>,
    /// the next character to place
    pos: usize,
    /// the current line's first character
    line: usize,
    /// the current line was started by the text before the message (a pen past the left edge)
    mid: bool,
}

impl Flow {
    fn new(atlas: &FontAtlas, runs: &[Run], mid: bool) -> Flow {
        let mut chars = Vec::new();
        if english() {
            for run in runs {
                match run {
                    Run::Text(t) | Run::Bar { text: t, .. } => chars.extend(lone_dashes(t.chars().collect(), Pass::Text, |g| atlas.has_ex(g))),
                    Run::Ruby { base, .. } => chars.extend(base.chars()),
                    Run::Newline => chars.push('\n'),
                    _ => {}
                }
            }
        }
        Flow { at: vec![None; chars.len()], chars, pos: 0, line: 0, mid }
    }

    fn newline(&mut self) {
        self.pos = (self.pos + 1).min(self.chars.len());
        self.line = self.pos;
        self.mid = false;
    }

    /// A ruby base character placed at `out[at]` from pen `x`, `wrapped` when it started a line.
    fn placed(&mut self, at: usize, x: f32, wrapped: bool) {
        if let Some(a) = self.at.get_mut(self.pos) {
            *a = Some((at, x));
        }
        if wrapped {
            self.line = self.pos;
            self.mid = false;
        }
        self.pos += 1;
    }

    /// Start a new line with character `from`, moving what was placed of it and the characters after it
    /// (`out` from the first of them on, their rubies with them) to the left edge of the next line.
    fn wrap(&mut self, from: usize, lay: &Layout, (x, baseline): (&mut f32, &mut f32), out: &mut [PlacedChar]) {
        if let Some((first, pen)) = self.at.get(from..).and_then(|a| a.iter().flatten().next().copied()) {
            let dx = lay.left - pen;
            for pc in &mut out[first..] {
                pc.x += dx;
                pc.baseline += lay.line_height;
                if let Some(bar) = &mut pc.bar {
                    bar.top += lay.line_height;
                }
            }
            for (_, p) in self.at[from..].iter_mut().flatten() {
                *p += dx;
            }
            *x += dx;
        } else {
            *x = lay.left;
        }
        *baseline += lay.line_height;
        self.line = from;
        self.mid = false;
    }
}

/// English message text (the overseas build's +101009 = English, sub_A7790): no per-line character count
/// (sub_BD7EC), but nn::font's line breaking. For each line sub_BF0F8 finds the first character that runs past
/// the slot width (sub_C7240 / sub_145334: pen + spacing + kerning + advance > width; a line's first character
/// always fits) and asks sub_148988 for the line's last character over the whole text (`Flow`); sub_C471C
/// breaks right after it, so a line keeps its trailing spaces. Glyphs advance by their own width plus the
/// kerning before them and the spacing.
#[allow(clippy::too_many_arguments)]
fn english_text(atlas: &FontAtlas, lay: &Layout, chars: &[char], pass: Pass, st: &mut Style, (x, baseline): (&mut f32, &mut f32), order: &mut usize, out: &mut Vec<PlacedChar>, flow: &mut Flow) {
    // the parsers' exfont runs (tag 13, `makes_run`) for English text too: units of (first char index, char
    // count, exfont glyph)
    let mut units: Vec<(usize, usize, Option<char>)> = Vec::new();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        let n = chars[k..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
        match run_kind(c) {
            Some(g) if atlas.has_ex(g) && makes_run(g, n, pass) => {
                units.push((k, n, Some(g)));
                k += n;
            }
            _ => {
                units.push((k, 1, None));
                k += 1;
            }
        }
    }
    // an exfont run of n at the italic ratio `italic`: the tag processor (sub_DB650; Witch on the Holy Night's
    // jp_DD334) of a writer without fixed width (+56 = 0) centres nothing (+92 clear for English, so D = 0) and
    // quarters both moves: tag 0xD moves the pen by v = (sp s - sp (n - 1) / 2 - 1) / 4 / (italic + 1), the
    // stretched glyph advances by 0.98 (W n + sp (n - 1)) (W = 41 s) plus the spacing, and tag 6 moves it by
    // (v + v - v / (italic' + 1)) / 4 - sp s. The overseas one, once `@i` was seen, draws the glyph at the
    // ratio 0.25 / n (italic') and is at 0.25 after it, `@r` or not. Switch, English: the text after a
    // two-dash or two-box run 0.5 px right of n (size + spacing) - spacing on. Returns (v, the glyph's
    // advance, the pen's move after it, italic', the ratio after the run).
    let run_moves = |n: usize, italic: f32| {
        let (n, sp, s) = (n as f32, lay.spacing, lay.size * lay.sx / BASE_SIZE);
        let v = (sp * s - sp * (n - 1.0) / 2.0 - 1.0) * 0.25 / (italic + 1.0);
        let free = st.seen && crate::assets::overseas();
        let it = if free { ITALIC_RATIO / n } else { italic };
        let back = (v + v - v / (it + 1.0)) * 0.25 - sp * s;
        (v, run_stretch() * (pair_width() * s * n + sp * (n - 1.0)), sp + back, it, if free { ITALIC_RATIO } else { italic })
    };
    // a unit's kerning (none next to a run), width and the spacing after it, at the italic ratio `italic`
    let width = |prev: Option<char>, u: (usize, usize, Option<char>), italic: f32| match u.2 {
        Some(_) => {
            let (v, adv, after, ..) = run_moves(u.1, italic);
            (0.0, v + adv, after)
        }
        None => {
            let c = chars[u.0];
            (atlas.kern(prev, c, lay.size) * lay.sx, atlas.advance(c, lay.size) * lay.sx, lay.spacing)
        }
    };
    let next_prev = |u: (usize, usize, Option<char>)| if u.2.is_some() { None } else { Some(chars[u.0]) };
    let italic_after = |u: (usize, usize, Option<char>), italic: f32| if u.2.is_some() { run_moves(u.1, italic).4 } else { italic };
    let lb = LINE_BREAK.get();
    // the characters' places in the flow
    let base = flow.pos;
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < units.len() {
        // the first unit past the width
        let mut px = *x;
        let mut kp = prev;
        let mut italic = st.italic;
        let mut overflow = None;
        for (j, &u) in units.iter().enumerate().skip(i) {
            let (kern, w, after) = width(kp, u, italic);
            if (j > i || *x > lay.left) && px + kern + w > lay.right {
                overflow = Some(j);
                break;
            }
            px += kern + w + after;
            kp = next_prev(u);
            italic = italic_after(u, italic);
        }
        let last = match overflow {
            None => units.len() - 1,
            Some(o) => {
                let (here, over) = (base + units[i].0, base + units[o].0);
                let c = match lb {
                    Some(lb) => lb.last_on_line(&flow.chars, flow.line, Some(over)),
                    None => over.saturating_sub(1).max(flow.line),
                };
                // text that continues a line whose first word no longer fits starts on the next one; a break
                // before this run's part of the line takes the characters after it there too
                let from = if flow.mid && c >= over { Some(flow.line) } else { (c < here).then_some(c + 1) };
                if let Some(from) = from {
                    flow.wrap(from, lay, (x, baseline), out);
                    prev = None;
                    continue;
                }
                // the unit holding that character
                units.iter().rposition(|u| base + u.0 <= c).unwrap_or(i).max(i)
            }
        };
        for &u in &units[i..=last] {
            let (kern, w, after) = width(prev, u, st.italic);
            *x += kern;
            flow.at[base + u.0] = Some((out.len(), *x));
            let (c, run, gx, italic) = match u.2 {
                Some(g) => {
                    let (v, _, _, it, _) = run_moves(u.1, st.italic);
                    (g, Some((u.1 as u8, lay.spacing)), *x + v, it)
                }
                None => (chars[u.0], None, *x, st.italic),
            };
            out.push(PlacedChar { c, x: gx, baseline: *baseline, size: lay.size, order: *order, run, stretch: 1.0, sx: lay.sx, bar: None, italic, wobble: None, fill: 0, shear: 0.0 });
            *order += 1;
            *x += w + after;
            prev = next_prev(u);
            st.italic = italic_after(u, st.italic);
        }
        i = last + 1;
        if i < units.len() {
            *x = lay.left;
            *baseline += lay.line_height;
            prev = None;
            flow.line = base + units[i].0;
            flow.mid = false;
        }
    }
    flow.pos = base + chars.len();
}

/// A text colour word (RGB565) as the exe's sub_1007C0 widens it: each channel's top bits repeated below.
pub fn rgb565(c: u16) -> [f32; 3] {
    let c = u32::from(c);
    let r = (c >> 8) & 0xF8 | c >> 13;
    let g = (c >> 3) & 0xFC | (c >> 9) & 3;
    let b = (c & 0x1F) << 3 | (c >> 2) & 7;
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

#[cfg(test)]
mod tests {
    /// Witch on the Holy Night's breaks around dash runs (Switch, Astris with these lines in script_text.mrg):
    /// a break on a run's first or last copy starts the line with the whole run, one between is dropped.
    #[test]
    fn witch_dash_run_breaks() {
        use super::*;
        crate::assets::TEST_WITCH.set(true);
        let nso = crate::assets::exe_tables(knife_core::exe::Build::Witch).unwrap();
        let _ = LINE_BREAK.set(Arc::new(LineBreak::load(&nso).unwrap()));
        let lay = Layout { line_height: 1.0, ..crate::game::WITCH_LAYOUT };
        for (text, want) in [
            ("あいうえおかきくけこさしすせそたちつてとなにぬ―――――。", vec!["あいうえおかきくけこさしすせそたちつてとなにぬ", "―――――。"]),
            ("あいうえおかきくけこさしすせそたちつてと――――――――。", vec!["あいうえおかきくけこさしすせそたちつてと", "――――――――。"]),
            ("あいうえおかきくけこさしすせそたちつてとなにぬねのは――らりるれろ", vec!["あいうえおかきくけこさしすせそたちつてとなにぬねのは", "――らりるれろ"]),
            ("あいうえおかきくけこさしすせそたちつてとなにぬねの―――らりるれろ", vec!["あいうえおかきくけこさしすせそたちつてとなにぬねの―――らりるれろ"]),
            ("つまり、なんていうか、口にするのもどうかと思うけど―――", vec!["つまり、なんていうか、口にするのもどうかと思うけど―――"]),
        ] {
            let runs = parse_markup(text);
            let plan = plan_lines_with(|_| true, |_| lay.size, false, &lay, &runs, lay.left).unwrap();
            // the pushed units: dash runs as one
            let mut units: Vec<String> = Vec::new();
            for c in text.chars() {
                match units.last_mut() {
                    Some(u) if c == '―' && u.ends_with('―') => u.push(c),
                    _ => units.push(c.to_string()),
                }
            }
            let mut lines = vec![String::new()];
            for (u, &start) in units.iter().zip(&plan.starts) {
                if start {
                    lines.push(String::new());
                }
                lines.last_mut().unwrap().push_str(u);
            }
            assert_eq!(lines, want, "{text}");
            assert!(!plan.trailing, "{text}");
        }
    }
}

