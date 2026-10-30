//! Font rasterisation into an R8 atlas, and NVL page layout.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont};
use knife_core::format::{bffnt, sfnt};
use knife_core::itype::glyph::Hinter;
use knife_core::itype::kern;
use knife_core::itype::raster::Graymap;
use knife_core::itype::tt;
use knife_core::kinsoku;
use knife_core::linebreak::LineBreak;
use knife_render::{Blend, Quad, Renderer, Shade, TexRef};

/// The glyph cache sheet's width at 1080p, its height twice that (nn::font's texture cache, 2048 x 4096).
const ATLAS: u32 = 2048;

#[derive(Clone, Copy)]
struct Glyph {
    /// sheet rect in pixels: the glyph's cache cell, bottom up (`FontAtlas::glyph`)
    rect: [u32; 4],
    /// offset of the cell's top-left from the pen position on the baseline
    off: [f32; 2],
    /// sheet pixels per base-size pixel (`FontAtlas::raster`)
    px: f32,
}

type GlyphKey = (char, u16, usize);

/// A registered glyph: whether a ruby's, the character, the faces' order (`GlyphCache::first_print`).
pub type Registration = (bool, char, FontOrder);

/// A cache line's height for a cell of `rows` (jp_1445D4 -> jp_144654): the power of two at or above it, 16 at
/// least; a cell over 1024 rows gets class 0, one row.
fn line_height(rows: u16) -> u32 {
    if rows > 1024 { 1 } else { u32::from(rows).next_power_of_two().max(16) }
}

/// One line of a cache sheet: its cells in x order, an evicted one kept (and in the way of a new run) until the
/// next completion takes it out.
struct Line {
    y: u32,
    height: u32,
    /// where the next cell goes
    x: u32,
    cells: Vec<Cell>,
}

struct Cell {
    x: u32,
    w: u32,
    key: GlyphKey,
    evicted: bool,
}

/// A font set's glyph cache sheet: sub_CA500's (or sub_CAA50's) four outline width groups of the set's fonts
/// share one texture cache. Switch, EG3 (the cache's entries read from guest memory): '?' and ' ' of each group's
/// first font in turn from x 0, then the page's glyphs of every group on the same line.
struct Sheet {
    tex: TexRef,
    lines: Vec<Line>,
}

impl Sheet {
    /// A place for a cell `w` wide in a line of `height` on a sheet `side` x 2 `side` (jp_143070): the first line
    /// of that height with room (its next x + w + 2 under the side), else a new line 2 rows over the last (256
    /// lines at most), else in place of a run of evictable glyphs (jp_143944, `evict`). (x, line y), y from the
    /// sheet's bottom.
    fn place(&mut self, key: GlyphKey, w: u32, height: u32, side: u32, evictable: impl Fn(&GlyphKey) -> bool, evicted: &mut Vec<GlyphKey>) -> Option<(u32, u32)> {
        let cell = |x| Cell { x, w, key, evicted: false };
        if let Some(l) = self.lines.iter_mut().find(|l| l.height == height && l.x + w + 2 < side) {
            let x = l.x;
            l.x += w + 2;
            l.cells.push(cell(x));
            return Some((x, l.y));
        }
        if self.lines.len() >= 256 {
            return None;
        }
        let y = self.lines.last().map_or(0, |l| l.y + l.height + 2);
        if height + y < 2 * side {
            self.lines.push(Line { y, height, x: w + 2, cells: vec![cell(0)] });
            return Some((0, y));
        }
        // the first run in a line of the height, from a glyph neither registered nor drawn in this completion
        // or the last, that the cell fits in: from the run's first glyph to its last one's end, with the free
        // space back to the cell before it (past evicted ones) less 2
        for l in self.lines.iter_mut().filter(|l| l.height == height) {
            let mut start = None;
            for j in 0..l.cells.len() {
                if l.cells[j].evicted || !evictable(&l.cells[j].key) {
                    start = None;
                    continue;
                }
                let s = *start.get_or_insert(j);
                let span = (l.cells[j].x + l.cells[j].w - l.cells[s].x) as i32;
                let free = if s == 0 {
                    0
                } else {
                    let p = (0..s).rev().find(|&i| !l.cells[i].evicted || i == 0).unwrap_or(0);
                    l.cells[s].x as i32 - (l.cells[p].x + l.cells[p].w) as i32 - 2
                };
                if span + free.max(0) >= w as i32 {
                    for c in &mut l.cells[s..=j] {
                        c.evicted = true;
                        evicted.push(c.key);
                    }
                    if free >= 1 {
                        l.cells[s].x -= free as u32;
                    }
                    let x = l.cells[s].x;
                    l.cells.insert(s, cell(x));
                    return Some((x, l.y));
                }
            }
        }
        None
    }
}

/// A face of the font list (TrueType or CFF) rasterized as nn::font's glyph caches do (`knife_core::itype`:
/// hinted, scan converted, outlined and trimmed bit for bit), with its hinting states at the atlas's raster size
/// and at the base size its advances are taken at (one state when the two agree, as at 1080p).
struct TtFace {
    data: Arc<Vec<u8>>,
    hinters: Mutex<Vec<(u16, Hinter)>>,
    /// a TrueType glyph's advance at the base size, whole pixels (None: CFF, or the glyph will not load)
    advances: Mutex<HashMap<char, Option<i16>>>,
    /// a character's glyph by the cmap the SDK reads (`tt::Font::glyph`)
    gids: Mutex<HashMap<char, u16>>,
}

impl TtFace {
    fn new(data: Arc<Vec<u8>>) -> Option<TtFace> {
        tt::Font::new(&data)?;
        Some(TtFace { data, hinters: Mutex::new(Vec::new()), advances: Mutex::new(HashMap::new()), gids: Mutex::new(HashMap::new()) })
    }

    /// `c`'s glyph from the Unicode cmap the SDK picks (`tt::Font::glyph`: 3,10, else 3,1), 0 when unmapped.
    /// ab_glyph takes the first of every Unicode subtable (platform 0 too) that maps it.
    fn gid(&self, c: char) -> u16 {
        let Ok(mut gids) = self.gids.lock() else { return 0 };
        *gids.entry(c).or_insert_with(|| tt::Font::new(&self.data).map_or(0, |f| f.glyph(c as u32)))
    }

    /// The hinting state at `ppem`, made when missing (only the base size's is kept beside it).
    fn hinter<'a>(hinters: &'a mut Vec<(u16, Hinter)>, font: &tt::Font, ppem: u16) -> &'a mut Hinter {
        let i = match hinters.iter().position(|&(p, _)| p == ppem) {
            Some(i) => i,
            None => {
                hinters.retain(|&(p, _)| p == BASE_PPEM);
                hinters.push((ppem, Hinter::new(font, ppem)));
                hinters.len() - 1
            }
        };
        &mut hinters[i].1
    }

    /// `c`'s cache map at `ppem` with outline width `ow` (`cache_cell`); None for a glyph that will not load.
    fn map(&mut self, c: char, ppem: u16, ow: u16) -> Option<Graymap> {
        let font = tt::Font::new(&self.data)?;
        let hinters = self.hinters.get_mut().ok()?;
        Self::hinter(hinters, &font, ppem).cache_map(&font, font.glyph(c as u32), ow)
    }

    /// A TrueType glyph's char width as FSS_get_advance gives it: the advance of its fitted outline at the base
    /// size (its program may move the advance point: Times New Roman's o is 19 px where its metrics give 20),
    /// to the nearest whole pixel. None for a CFF font or a glyph that will not load.
    fn advance(&self, c: char) -> Option<i16> {
        let mut advances = self.advances.lock().ok()?;
        *advances.entry(c).or_insert_with(|| {
            let font = tt::Font::new(&self.data).filter(|f| f.cff.is_none())?;
            let mut hinters = self.hinters.lock().ok()?;
            let e = Self::hinter(&mut hinters, &font, BASE_PPEM).glyph(&font, font.glyph(c as u32))?;
            let n = e.points();
            Some(((e.x[n + 1] - e.x[n]).wrapping_shl(10).wrapping_add(0x8000) as u32 >> 16) as i16)
        })
    }
}

/// exfont_nx01.bffnt, the bitmap font sub_CAE88 pairs with the message font (nn::font PairFont), and its sheet
/// as an upright R8 texture.
struct ExFont {
    font: bffnt::Font,
    tex: TexRef,
}

/// The glyph cache's font order (the order of every face's font list, sub_143D98), which the tag processor's
/// tags 20 / 21 / 24 set (sub_DD334 -> sub_CB74C / sub_CB688; the overseas sub_DB650 likewise) for the glyphs
/// drawn after them: `@g` / `@h` (sub_BDC2C writes tag 20 / 21, and 21 at the text's end while `@g` is open),
/// or the raw bytes 0x14 / 0x15 / 0x18 in a line. The measuring tag processor (sub_DD0D8) and the line breaker
/// pass over them, so they move no line break. Switch, Japanese: 「@g　―――その女と、…」 in TsukuGo,
/// 「かき@gあいう@hくけ」's あいう alone; 「か\x18鬲鬳\x15鬲き」's first 鬲 from FOT-Seurat, the second in TsukuMin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontOrder {
    /// the atlas's own (the message list in order, the bubble's FOT-Seurat first)
    #[default]
    Own,
    /// tag 20: `<folder>2` (the gothic font), the folder's font, `<folder>3`
    Gothic,
    /// tag 21: the language's list in order
    List,
    /// tag 24: `<folder>3`, the folder's font, `<folder>2` (the bubble variant's order)
    Bubble,
}

impl FontOrder {
    /// The tag that sets an order.
    fn of_tag(c: char) -> Option<FontOrder> {
        match c {
            '\u{14}' => Some(FontOrder::Gothic),
            '\u{15}' => Some(FontOrder::List),
            '\u{18}' => Some(FontOrder::Bubble),
            _ => None,
        }
    }

    /// The list's fonts first in this order (the rest after them in theirs); none for the atlas's own.
    fn first(self) -> Option<[u8; 3]> {
        match self {
            FontOrder::Own => None,
            FontOrder::Gothic => Some([1, 0, 2]),
            FontOrder::List => Some([0, 1, 2]),
            FontOrder::Bubble => Some([2, 0, 1]),
        }
    }
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

/// The alpha a text slot's glyphs draw at: the slot's print (sub_C5FB4, overseas sub_C4A50, Witch on the Holy
/// Night jp_9FA94) hands TextWriter its float alpha (+0x280, set by sub_AB710) as the byte (int)(alpha x 255),
/// truncated (fcvtzs); the slot's bars (sub_C6A58) keep the float. A read message's 1 - 155 / 255 = 0.39215684
/// draws at 99 / 255 (Switch, faded lines: 99.1..99.3 / 255 fitted through the frame's sRGB transfer).
pub fn slot_alpha(a: f32) -> f32 {
    (a * 255.0) as i32 as f32 / 255.0
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

/// The base size as a ppem.
const BASE_PPEM: u16 = 40;
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
    /// each face's place in the language's font list (the folder's font 0, `<folder>2` 1, `<folder>3` 2), which
    /// a `FontOrder` puts in its order
    langs: Vec<u8>,
    /// each face's OTF kerning table (`kern`), made on first use
    kerning: Vec<OnceLock<Option<kern::Kerning>>>,
    /// whether each face has one (`set_otf_kerning`)
    otf: Vec<bool>,
    /// each face's cmap cache size (`kern::cmap_cache_size`), and whether a character at its slot 0 has been
    /// looked up there, after which the face has U+0000 (`font_of`)
    cmap_slots: Vec<u32>,
    slot0: Vec<std::cell::Cell<bool>>,
    /// the font list's faces for iType (the font, then the fallbacks)
    tt: Vec<Option<TtFace>>,
    ex: Option<ExFont>,
    sheet: Option<Sheet>,
    /// by char, outline width and face
    glyphs: HashMap<GlyphKey, Option<Glyph>>,
    /// each cached glyph's last frame drawn (the cache's flags 1 / 4 / 0x10: `Sheet::place`), `u64::MAX` for one
    /// kept for good (flag 0x20)
    used: HashMap<GlyphKey, u64>,
    /// the frame the sheets' evicted cells were last taken out in
    frame: u64,
    /// the sheets' width
    side: u32,
    /// the glyphs' raster scale over the base size: the render scale, so a canvas drawn above or below 1080p keeps
    /// the glyphs as sharp as the Switch's
    raster: f32,
    /// Tsukihime's QA bubble font (`Game::msg_font`): its text carries style 0x40 (`plan_lines`)
    pub bubble: bool,
}

impl FontAtlas {
    pub fn new(data: Vec<u8>) -> anyhow::Result<Self> {
        let font = FontArc::try_from_vec(data.clone())?;
        let data = Arc::new(data);
        Ok(Self {
            font,
            fallback: Vec::new(),
            langs: vec![0],
            cmap_slots: vec![kern::cmap_cache_size(&data)],
            slot0: vec![Default::default()],
            tt: vec![TtFace::new(data)],
            kerning: vec![OnceLock::new()],
            otf: vec![true],
            ex: None,
            sheet: None,
            glyphs: HashMap::new(),
            used: HashMap::new(),
            frame: 0,
            side: ATLAS,
            raster: 1.0,
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
        self.ex_glyph(c).is_some()
    }

    /// The exfont glyph `c` draws with: its own, else, when no font of the list has `c` either, the exfont's
    /// alternate glyph (FINF +6, glyph 0: a box with a border at half coverage, drawn in the edge colour).
    /// Switch, Japanese with the French fan fonts: U+0E01 draws that box, 1 px of edge around it, 266..303 px
    /// down a line whose TT_TsukuMin-D ■ would span 264..301 with the glyph cache's 2 px rim; overseas
    /// Japanese: 杀 and 唧, which TT_TsukuMin-D lacks, 34 px wide at 45.6 px, in the text and in the ruby.
    fn ex_glyph(&self, c: char) -> Option<bffnt::Glyph> {
        let ex = self.ex.as_ref()?;
        ex.font.glyph(c).or_else(|| ex.font.alt.filter(|_| !c.is_control() && (0..=self.fallback.len()).all(|i| self.gid(i, c) == 0)))
    }

    /// Whether the placed char draws from the exfont sheet (a run, or an exfont glyph), not the glyph cache.
    pub fn is_ex(&self, pc: &PlacedChar) -> bool {
        pc.run.is_some() || self.has_ex(pc.c)
    }

    /// Add the next font of the language's font list: sub_CA500 gives each of the message glyph cache's
    /// faces the language's three font files as one nn::font font list, and the cache takes a glyph from the
    /// first font that has it (English: Helvetica Neue has no U+3000 or ■, which come from FOT-Seurat, en3).
    pub fn add_fallback(&mut self, data: Vec<u8>) -> anyhow::Result<()> {
        let lang = self.langs.len() as u8;
        self.add_font(data, lang)
    }

    /// Add font `lang` of the language's list (`langs`) as the next face.
    pub fn add_font(&mut self, data: Vec<u8>, lang: u8) -> anyhow::Result<()> {
        self.fallback.push(FontArc::try_from_vec(data.clone())?);
        self.cmap_slots.push(kern::cmap_cache_size(&data));
        self.slot0.push(Default::default());
        self.tt.push(TtFace::new(Arc::new(data)));
        self.kerning.push(OnceLock::new());
        self.otf.push(true);
        self.langs.push(lang);
        Ok(())
    }

    /// The first face's place in the language's font list (the bubble atlas's `<folder>3`, 2).
    pub fn set_first_lang(&mut self, lang: u8) {
        self.langs[0] = lang;
    }

    /// Which places of the font list have an OTF kerning table (`kern`): the glyph cache makes one for a font
    /// whose type (sub_AAF60, by file extension: ttf 0, otf 1, bfttf 2, bfotf 3) is 1 or 3 (jp_14193C); sub_CA500
    /// gives the list's first font its file's type and the other two `<folder>2`'s (+116 for both), and
    /// sub_CAA50 type 0 to every shared system font. Without one a font kerns by its `kern` table alone.
    /// Switch, Japanese: FOT-Seurat (`<folder>3`) takes TT_TsukuGo-D.ttf's type 0 and kerns by nothing.
    pub fn set_otf_kerning(&mut self, by_lang: [bool; 3]) {
        self.otf = self.langs.iter().map(|&l| by_lang[usize::from(l).min(2)]).collect();
    }

    /// The character drawn for `c` by an atlas without the exfont (the system fonts' UI text): ■ (U+25A0)
    /// when no font of the list has it; with the exfont such a character is its alternate glyph (`ex_glyph`).
    fn shown(&self, c: char) -> char {
        let missing = |c: char| (0..=self.fallback.len()).all(|i| self.gid(i, c) == 0);
        if !c.is_control() && !self.has_ex(c) && missing(c) && !missing('■') { '■' } else { c }
    }

    /// Face `i` of the list (0 the font, then the fallbacks).
    fn face(&self, i: usize) -> &FontArc {
        match i {
            0 => &self.font,
            i => &self.fallback[i - 1],
        }
    }

    /// The faces in the order the glyph cache asks them in `order` (`FontOrder`).
    fn faces(&self, order: FontOrder) -> Vec<usize> {
        let mut faces: Vec<usize> = (0..self.langs.len()).collect();
        if let Some(first) = order.first() {
            faces.sort_by_key(|&i| first.iter().position(|&l| l == self.langs[i]).unwrap_or(first.len() + usize::from(self.langs[i])));
        }
        faces
    }

    /// The face the glyph cache takes `c` from in `order`: the first that has it, else the first.
    fn face_index(&self, c: char, order: FontOrder) -> usize {
        let faces = self.faces(order);
        self.font_in(c, &faces).unwrap_or(faces[0])
    }

    /// The first of `faces` that has `c` (sub_142E58: CheckGlyphExist, FSS_map_char, in each in turn), noting
    /// the cmap cache's slot 0 in those it asks. U+0000 is there only in a face whose slot 0 a character has
    /// taken (`kern::cmap_cache_size`; its own lookup then keeps it), so on the Switch nn::font's kerning with a
    /// text's ends (`kern`, `kern_last`) does nothing in English, whose script has no such character (Switch:
    /// a QA bubble's line starting with FOT-Seurat's “ not moved by its `palt` placement).
    fn font_in(&self, c: char, faces: &[usize]) -> Option<usize> {
        if c == '\0' {
            return faces.iter().copied().find(|&i| self.slot0[i].get() && self.gid(i, c) != 0);
        }
        let code = c as u32;
        faces.iter().copied().find(|&i| {
            if code < 0x10000 && code.is_multiple_of(self.cmap_slots[i]) {
                self.slot0[i].set(true);
            }
            self.gid(i, c) != 0
        })
    }

    /// `c`'s glyph in face `i` of the list as the SDK maps it (`TtFace::gid`; ab_glyph's for a face iType
    /// cannot read).
    fn gid(&self, i: usize, c: char) -> u16 {
        match self.tt.get(i).and_then(Option::as_ref) {
            Some(f) => f.gid(c),
            None => if i == 0 { &self.font } else { &self.fallback[i - 1] }.glyph_id(c).0,
        }
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
        let g = self.ex_glyph(c)?;
        let sx = size / BASE_SIZE;
        let (tw, th) = (ex.font.sheet.width as f32, ex.font.sheet.height as f32);
        let (gw, ch) = (f32::from(g.width), ex.font.cell_h as f32);
        let q = Quad::rect(
            ex.tex.clone(),
            [x + (f32::from(g.left) - 0.5) * sx, baseline - (EX_TOP + 0.5) * sx, (gw + 1.0) * sx, (ch + 1.0) * sx],
            color,
        )
        .with_uv_rect((g.x as f32 - 0.5) / tw, (g.y as f32 - 0.5) / th, (g.x as f32 + gw + 0.5) / tw, (g.y as f32 + ch + 0.5) / th);
        let border = if bordered(c) || ex.font.glyph(c).is_none() { 1.0 } else { 0.0 };
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
            None if pc.blank => None,
            None => self.quad_stretched(r, pc.c, pc.size, (pc.x + dx, pc.baseline + dy), pc.stretch, color, ow, pc.font),
        }?;
        if pc.sx == 1.0 {
            return Some(q);
        }
        // the writer's x scale squeezes the glyph about the pen
        let x0 = pc.x + dx;
        Some(Quad { pos: q.pos.map(|[x, y]| [x0 + (x - x0) * pc.sx, y]), ..q })
    }

    /// The placed char filled `rgb` over its edge in `edge` (RGB bytes, the slot's edge colour), from the edged
    /// cache of font index `index` (`OUTLINE`), `alpha` fading the composite once. A slot of font index 1
    /// registers and draws its rubies in index 2's (sub_C5FB4: sub_CB478 / sub_CBF10 with index 2 around the ruby
    /// strings; Witch on the Holy Night's sub_9FA94 alike). Switch, Japanese first page: はだざむ's rim as
    /// index 2's outline width 3, a pixel thicker than the base text's.
    #[allow(clippy::too_many_arguments)]
    pub fn rimmed_quad_edge(&mut self, r: &Renderer, pc: &PlacedChar, index: usize, rgb: [f32; 3], edge: [u8; 3], alpha: f32) -> Option<Quad> {
        let index = if pc.ruby && index == 1 { 2 } else { index };
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
    /// 650 / 760 / 870 px, rounding at 40 px gives 2.5 / 5.7 / 6.3). A TrueType glyph's width is its hinted
    /// advance (`TtFace::advance`; Switch, Times New Roman as the message font: c e f k o v x y z sit in their
    /// cells by the advances their programs set).
    /// An exfont glyph advances by its CWDH advance at size / 40.
    pub fn advance(&self, c: char, size: f32) -> f32 {
        self.advance_in(c, size, FontOrder::Own)
    }

    /// `advance` from the face the glyph cache takes `c` from in `order`.
    pub fn advance_in(&self, c: char, size: f32, order: FontOrder) -> f32 {
        if let Some(g) = self.ex_glyph(c) {
            return f32::from(g.advance) * size / BASE_SIZE;
        }
        let c = self.shown(c);
        let i = self.face_index(c, order);
        if let Some(a) = self.tt.get(i).and_then(Option::as_ref).and_then(|f| f.advance(c)) {
            return f32::from(a) * size / BASE_SIZE;
        }
        let font = self.face(i);
        let base = font.as_scaled(Self::px(font, BASE_SIZE)).h_advance(GlyphId(self.gid(i, c)));
        base.round() * size / BASE_SIZE
    }

    /// The kerning nn::font's TextWriter adds before `b` after `a` (None: a line's first character, whose
    /// partner is U+0000), in px at `size`. The message font is a PairFont of the exfont and the glyph cache
    /// (sub_13D9A0 asks the font that has both characters, the exfont first; the exfont's KRNG pairs only
    /// characters it does not map, so it never kerns), and the cache (sub_140930) kerns two
    /// characters it takes from the same font of its order: by that font's OTF kerning table
    /// (`kern::Kerning`: whole pixels at the 40 px base size, scaled to the drawn size like the advances), and
    /// when that gives 0 by its `kern` table; a line's first character by the table's `palt` placement. Switch,
    /// English: "AVAWAY" 4 px wider than the GPOS kerning gives, as Helvetica Neue's A is no class of its pair
    /// adjustments (`kern::Kerning`); Demos Next's 7 px (V-A and W-A only).
    pub fn kern(&self, a: Option<char>, b: char, size: f32) -> f32 {
        self.kern_in(a, b, size, FontOrder::Own)
    }

    /// `kern` in the glyph cache's font order `order`.
    pub fn kern_in(&self, a: Option<char>, b: char, size: f32, order: FontOrder) -> f32 {
        if self.ex.as_ref().is_some_and(|e| a.is_some_and(|a| e.font.glyph(a).is_some()) && e.font.glyph(b).is_some()) {
            return 0.0;
        }
        let (Some(fa), Some(fb)) = (self.font_of(a.unwrap_or('\0'), order), self.font_of(b, order)) else { return 0.0 };
        let gb = self.gid(fb, b);
        let px = match a {
            None => self.kerning(fb).map_or(0, |k| k.px(k.raw_first(gb), BASE_SIZE as u32)),
            Some(_) if fa != fb => 0,
            Some(a) => {
                let ga = self.gid(fa, a);
                match self.kerning(fa).map_or(0, |k| k.px(k.raw(ga, gb), BASE_SIZE as u32)) {
                    0 => self.tt.get(fa).and_then(Option::as_ref).map_or(0, |t| {
                        let upem = tt::Font::new(&t.data).map_or(1000, |f| f.upem);
                        kern::kern_table_px(kern::kern_table(&t.data, ga, gb), upem, BASE_SIZE as u32)
                    }),
                    k => k,
                }
            }
        };
        px as f32 * size / BASE_SIZE
    }

    /// The kerning nn::font adds after a text's last character `c` (its partner U+0000): the OTF kerning table's
    /// `palt` advance less placement. TextWriter::Print (sub_145B78) moves the pen by it at the end of the text,
    /// and the line breaker's measure (sub_145408) counts it in a line's width (`english_text`'s fit test).
    pub fn kern_last(&self, c: char, size: f32, order: FontOrder) -> f32 {
        let (Some(_), Some(f)) = (self.font_of('\0', order), self.font_of(c, order)) else { return 0.0 };
        let g = self.gid(f, c);
        self.kerning(f).map_or(0, |k| k.px(k.raw_last(g), BASE_SIZE as u32)) as f32 * size / BASE_SIZE
    }

    /// The face the glyph cache takes `c` from in `order` (sub_142E58), none when no font has it.
    fn font_of(&self, c: char, order: FontOrder) -> Option<usize> {
        self.font_in(c, &self.faces(order))
    }

    /// Face `i`'s OTF kerning table, made on first use. It has the `palt` adjustments on the overseas build
    /// alone: its sub_CA500 passes the font manager's +136 (clear, read from the running game) as
    /// InitializeOtfKerningTable's `kern`-only flag, the Japanese build's and Witch on the Holy Night's 1.
    fn kerning(&self, i: usize) -> Option<&kern::Kerning> {
        let t = self.tt.get(i)?.as_ref().filter(|_| self.otf.get(i).copied().unwrap_or(false))?;
        self.kerning.get(i)?.get_or_init(|| kern::Kerning::new(&t.data, !crate::assets::overseas())).as_ref()
    }

    /// Start over on new sheets (quads already queued keep the old ones), sized for the raster scale.
    fn reset(&mut self) {
        self.side = (ATLAS * self.raster.ceil() as u32).min(8192);
        self.sheet = None;
        self.glyphs.clear();
        self.used.clear();
    }

    /// The glyph cache's baseline position at `ppem` (jp_14193C: each outline width's group of the set's fonts
    /// in list order (sub_CA500, sub_CAA50) takes the record of its tallest bbox; jp_143070 rounds its yMax / em
    /// at the size): a cell's top above the cache baseline. The list order is the faces' places, which a bubble
    /// atlas holds in another order.
    fn cell_baseline(&self, ppem: u16) -> i32 {
        let mut faces: Vec<_> = self.langs.iter().zip(&self.tt).collect();
        faces.sort_by_key(|&(&l, _)| l);
        let m: Vec<_> = faces.into_iter().filter_map(|(_, t)| sfnt::metrics(&t.as_ref()?.data)).collect();
        sfnt::set_metrics(&m, f32::from(ppem)).map_or(GLYPH_BASELINE.load(Ordering::Relaxed) as i32, |s| s.baseline)
    }

    /// The glyph as nn::font's glyph cache holds it (sub_143070): rasterized at the base size (font manager +124,
    /// 40 px) with its box on whole pixels and, for an edged cache, grown by the outline width `ow`
    /// (`cache_cell`: half coverage the edge, full the fill); the draw scales it to the slot size. Above or below
    /// 1080p at the render scale times that, to the nearest whole ppem, the outline width scaled alike.
    /// The faces rasterize as the Switch's iType does, TrueType and CFF, bit for bit at 1080p.
    /// A print's registrations (`GlyphCache::first_print`, `GlyphCache::registrations`) into the sheet in their
    /// order, the cells going where jp_143070 takes the pending entries in that order: a glyph's in the edged cache
    /// of font index 1 when `rim` (a ruby's in index 2's, `rimmed_quad_edge`), else of index 0.
    pub fn register(&mut self, r: &Renderer, regs: &[Registration], rim: bool) {
        for &(ruby, c, order) in regs {
            let ow = if rim { OUTLINE[if ruby { 2 } else { 1 }] } else { 0 };
            self.glyph(r, c, ow, order);
        }
    }

    fn glyph(&mut self, r: &Renderer, c: char, ow: u16, order: FontOrder) -> Option<Glyph> {
        if r.scale() != self.raster {
            self.raster = r.scale();
            self.reset();
        }
        // a completion takes the cells evicted since the last out of their lines
        if r.frame() != self.frame {
            self.frame = r.frame();
            for l in self.sheet.iter_mut().flat_map(|s| &mut s.lines) {
                l.cells.retain(|c| !c.evicted);
            }
        }
        // a new sheet starts with '?' and ' ' of each outline width group's first font, kept for good
        if self.sheet.is_none() {
            self.sheet = Some(Sheet {
                tex: r.create_writable(self.side, 2 * self.side, true, "glyph cache"),
                lines: Vec::new(),
            });
            for w in OUTLINE {
                for c in ['?', ' '] {
                    self.glyph(r, c, w, FontOrder::Own);
                    let key = (self.shown(c), w, self.face_index(c, FontOrder::Own));
                    self.used.insert(key, u64::MAX);
                }
            }
        }
        let c = self.shown(c);
        let i = self.face_index(c, order);
        let key = (c, ow, i);
        if let Some(g) = self.glyphs.get(&key) {
            let u = self.used.entry(key).or_default();
            if *u != u64::MAX {
                *u = self.frame;
            }
            return *g;
        }
        let ppem = (BASE_SIZE * self.raster).round().max(1.0) as u16;
        let scaled_ow = (f32::from(ow) * f32::from(ppem) / BASE_SIZE).round() as u16;
        let base = self.cell_baseline(ppem);
        let map = self.tt.get_mut(i).and_then(Option::as_mut).and_then(|face| face.map(c, ppem, scaled_ow));
        let blank = Glyph {
            rect: [0, 0, 0, 0],
            off: [0.0, 0.0],
            px: 1.0,
        };
        let Some(m) = map else {
            // a glyph the font lacks takes no cell (jp_143070's flag 0x40)
            self.glyphs.insert(key, Some(blank));
            return Some(blank);
        };
        // the cache cell (jp_143070, its quad jp_1406A8 -> PrintGlyph): from the set's baseline position above the
        // baseline (a taller glyph's top rows cut off) down to the glyph's foot, and the two rows under it the
        // quad's +1 height and half texel reach; an empty glyph (a space) takes its 2 columns too. Its line's
        // height is the power of two over its rows. The sheet is bottom up, the first lines at its foot: the
        // quads' v from 1 - (y - 0.5) / h, near 1 where the interpolated coordinate is half as fine as under 0.5
        // (Switch, EG3: a faded Y's border pixel at the 0.54 edge 13 levels darker from a cell in the sheet's
        // lower half, any x or line there); the cell's quad has its vertices (Switch, EG2 / EG3 glyph records:
        // border program pixels at the 0.54 edge turn with a 0.005 px move, from the corners of T, W and A).
        // an empty map's entry has its top at the baseline (Switch: every ' ' 40 rows at a baseline 40)
        let top = base - if m.width == 0 || m.height == 0 { 0 } else { i32::from(m.hi_y) };
        let rows = top + i32::from(m.height);
        let w = m.width as u32;
        let side = self.side;
        let (used, frame) = (&self.used, self.frame);
        let sheet = self.sheet.as_mut()?;
        let height = line_height(rows as u16);
        let mut evicted = Vec::new();
        let evictable = |k: &GlyphKey| used.get(k).is_none_or(|&f| f.saturating_add(1) < frame);
        let Some((x, y)) = sheet.place(key, w, height, side, evictable, &mut evicted) else {
            // no room: not drawn, and tried again at the next draw (jp_143070's errors 2 and 3)
            return Some(blank);
        };
        for k in evicted {
            self.glyphs.remove(&k);
            self.used.remove(&k);
        }
        self.used.insert(key, self.frame);
        // the rows written from the line's foot (jp_143070): the zero rows above the glyph, its rows (0, the
        // bitmap, 0 across w + 2 columns from x), then up to two zero rows while under the line's height
        let below = (height as i32 - i32::from(m.height)).clamp(0, 2);
        let written = (rows + below).min((2 * side - y) as i32);
        let g = if rows > 0 && w > 0 {
            let (h, rows) = (rows as u32 + 2, rows as u32);
            let ww = w + 2;
            let n = written.max(0) as u32;
            let mut cell = vec![0; (ww * n) as usize];
            for row in 0..rows.min(n) {
                let src = row as i32 - top;
                if src >= 0 {
                    let (s, d) = (src as usize * w as usize, (n - 1 - row) as usize * ww as usize + 1);
                    cell[d..d + w as usize].copy_from_slice(&m.bits[s..s + w as usize]);
                }
            }
            let sheet = self.sheet.as_ref()?;
            r.write_region(&sheet.tex, [x, 2 * side - y - n, ww, n], 1, &cell);
            Some(Glyph {
                rect: [x + 1, 2 * side - y - h, w, h],
                off: [f32::from(m.lo_x), -base as f32 - 1.0],
                px: f32::from(ppem) / BASE_SIZE,
            })
        } else {
            if written > 0 {
                let n = written as u32;
                r.write_region(&self.sheet.as_ref()?.tex, [x, 2 * side - y - n, w + 2, n], 1, &vec![0; ((w + 2) * n) as usize]);
            }
            Some(blank)
        };
        self.glyphs.insert(key, g);
        g
    }

    /// Quad for one glyph with its pen at (x, baseline) from the edged cache of font index `index` (`OUTLINE`):
    /// filled in the vertex colour over its edge in black (fs_text's bordered mode).
    #[allow(clippy::too_many_arguments)]
    pub fn edged_quad(&mut self, r: &Renderer, c: char, size: f32, x: f32, baseline: f32, color: [f32; 4], index: usize) -> Option<Quad> {
        self.quad_stretched(r, c, size, (x, baseline), 1.0, color, OUTLINE[index], FontOrder::Own).map(|q| edged_glyph(q, [0; 3]))
    }

    /// A glyph quad with its x scaled by `sx` about the middle of its em (Switch, train page ……: the stretched
    /// dots stay centred on the unstretched glyph).
    /// `ow`: the cache's outline width (`glyph`).
    #[allow(clippy::too_many_arguments)]
    fn quad_stretched(&mut self, r: &Renderer, c: char, size: f32, (x, baseline): (f32, f32), sx: f32, color: [f32; 4], ow: u16, order: FontOrder) -> Option<Quad> {
        let g = self.glyph(r, c, ow, order)?;
        if g.rect[2] == 0 {
            return None;
        }
        let [gx, gy, gw, gh] = g.rect;
        let (a, b) = (self.side as f32, 2.0 * self.side as f32);
        let k = size / BASE_SIZE / g.px;
        // padded by half a texel on every side as PrintGlyph pads every font's glyphs (the nn::font::Font
        // constructor, 0x13DD40, sets the +9 it reads): the atlas's empty gap blends in at the cell's edge. The
        // cell's last row is the quad's lower pad (`glyph`).
        let (x0, y0) = (x + (g.off[0] - 0.5) * k * sx - (sx - 1.0) * size / 2.0, baseline + (g.off[1] - 0.5) * k);
        // PrintGlyph's u (x - 0.5) / w, (x + w + 0.5) / w and v 1 - (y - 0.5) / h, 1 - (y + rows + 1.5) / h, y
        // the line's from the sheet's foot: exact in f32 either way
        let q = Quad::rect(self.sheet.as_ref()?.tex.clone(), [x0, y0, (gw as f32 + 1.0) * k * sx, gh as f32 * k], color)
            .with_uv_rect((gx as f32 - 0.5) / a, (gy + gh) as f32 / b + 0.5 / b, (gx + gw) as f32 / a + 0.5 / a, (gy as f32 + 0.5) / b);
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
        let color = [color[0], color[1], color[2], slot_alpha(color[3])];
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
                // TextWriter::Print kerns the run's glyph as a character
                if free {
                    pen += self.kern(prev.replace(g), g, h);
                }
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
            if let Some(q) = self.quad_stretched(r, c, h, (gx + (sx - 1.0) * h / 2.0, baseline), sx, color, ow, FontOrder::Own) {
                batches.push(false, if rim.is_some() { edged_glyph(q, [0; 3]) } else { q });
            }
            // nn::font's TextWriter spaces each glyph from the one before at its own spacing (`steps`)
            pen += cell + if free { spacing } else { slot_space(chars.get(i).copied().unwrap_or(c), spacing) };
        }
        batches.flush(out);
        // and after the text's last character (`kern_last`)
        if free && let Some(c) = prev {
            pen += self.kern_last(c, h, FontOrder::Own);
        }
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
            + prev.filter(|_| english()).map_or(0.0, |c| self.kern_last(c, h, FontOrder::Own))
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
        total - if chars.is_empty() { 0.0 } else { spacing } + prev.map_or(0.0, |c| self.kern_last(c, h, FontOrder::Own))
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
fn slot_chars(s: &str) -> impl Iterator<Item = char> + Clone + '_ {
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

/// The character space's factor inside an ASCII run, +100932 (sub_C36D8): 0.5 for Japanese, 1.0 for Chinese;
/// the overseas jp_A7790 leaves English at the initial 0.5 (Switch, English `ab\x10cd\x11ef`: c and d each 2 px
/// nearer the glyph before them at the spacing 4).
fn run_spacing() -> f32 {
    if language() == 0 || english() { 0.5 } else { 1.0 }
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
    /// the glyph cache's font order the glyph draws in (`FontOrder`)
    pub font: FontOrder,
    /// the glyph cache has the glyph's node dead: it draws as the space glyph (`GlyphCache::blank`)
    pub blank: bool,
    /// a ruby's glyph (`rimmed_quad_edge`)
    pub ruby: bool,
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
    /// a raw 0x03 ruby 0x04 not right after its base: the tag processor (case 3, sub_DDA1C) centres the ruby
    /// over the width of the last base a 0x01 measured (+16 / +24 / +32), ending at the cursor; the text before
    /// it draws as text (Switch: `あ\x01漢字\x02いう\x03かんじ\x04えお`'s かんじ over いう). None: no base before it
    /// in the text, or an 0x01 / 0x03 its 0x02 / 0x04 never closes: the processor reads the base pointer a
    /// slot never set (address 0) or an earlier text left, or scans on past the text, which stops the game
    /// (Switch: crashed or froze)
    RubyAt { base: Option<String>, ruby: String },
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
    /// `@x` (jp_BDC2C tag 12; `a4` flag 0x10, slot flag 0x40, drops it) or a 0x0C in the text: the writer's
    /// cursor x goes to the slot's base x (+101032, sub_DD0D8 / jp_DD334 case 0xC; `layout`'s `base`). Page
    /// text opens with it to go on after the message before (`Game::show_message`).
    Indent,
    /// Another control character in the text, which sub_BDC2C copies as it is (closing an ASCII run before it)
    /// and the writer takes as a tag, never a glyph (Switch: 0x02, 0x0B, 0x1A, 0x1F draw nothing and take no
    /// room): a tab moves the cursor to the next stop (nn::font sub_140CC8), 0x0E is `@i`'s tag 14 and 0x0B /
    /// 0x0F set the italic ratio back to 0 (sub_DD334), 0x14 / 0x15 / 0x18 set the font order
    /// (`FontOrder::of_tag`), 0x16 / 0x19 only the bar's position (drawn for a bar sub_BDC2C opened), and the
    /// ones that save and set the writer (0x06 / 0x07 / 0x08 / 0x0D / 0x10..0x13) act through `raw_glyphs`.
    Control(char),
    /// `@g` / `@h`: sub_BDC2C's tag 20 / 21 (no ASCII run closed before it), the glyph cache's font order
    /// (`FontOrder`) for what follows
    Font(FontOrder),
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

/// After a raw 0x01: the base up to its 0x02, the ruby between the 0x03 right after it and its 0x04, and the
/// characters that took (`parse_markup_edge`).
fn raw_ruby(mut it: impl Iterator<Item = char>) -> Option<(String, String, usize)> {
    let mut n = 0;
    let mut part = |end: char| -> Option<String> {
        let mut t = String::new();
        loop {
            let c = it.next()?;
            n += 1;
            match c {
                c if c == end => return Some(t),
                '\u{1}'..='\u{4}' => return None,
                c => t.push(c),
            }
        }
    };
    let base = part('\u{2}')?;
    if !part('\u{3}')?.is_empty() {
        return None;
    }
    let ruby = part('\u{4}')?;
    Some((base, ruby, n))
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
            Run::Newline | Run::Indent | Run::Control('\t') => return 0.0,
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
    // `@g` wrote tag 20 and no `@h` its 21 yet (sub_BDC2C's own flag, which the raw bytes leave alone)
    let mut gothic = false;
    // the last ruby base so far (the tag processor's case 1), for a raw 0x03 after it
    let mut last_base: Option<String> = None;
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
                    last_base = Some(b.to_string());
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
                    if matches!(n, 'n' | 'r' | 'i' | 'a' | 'c' | 'x' | 'g' | 'h') && !cur.is_empty() {
                        runs.push(Run::Text(std::mem::take(&mut cur)));
                    }
                    if n == 'g' && !gothic {
                        runs.push(Run::Font(FontOrder::Gothic));
                        gothic = true;
                    } else if n == 'h' && gothic {
                        runs.push(Run::Font(FontOrder::List));
                        gothic = false;
                    } else if n == 'n' {
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
            // sub_BDC2C copies a control character as it is (closing an ASCII run before it), and the tag
            // processor takes a 0x0C as `@x`'s tag 12 (Switch: a form feed in a line moves the cursor to the
            // slot's base x)
            '\u{c}' => {
                if !cur.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut cur)));
                }
                runs.push(Run::Indent);
            }
            // a raw ruby: sub_BDC2C copies 0x01 base 0x02 0x03 ruby 0x04 as they are, and the tag processor takes
            // them as the markup's own (cases 1 / 3; Switch: `あ\x01漢字\x02\x03かんじ\x04いう` as `あ<漢字|かんじ>いう`)
            '\u{1}' if raw_ruby(it.clone()).is_some() => {
                let (base, ruby, n) = raw_ruby(it.clone()).unwrap_or_default();
                for _ in 0..n {
                    it.next();
                }
                if !cur.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut cur)));
                }
                last_base = Some(base.clone());
                runs.push(Run::Ruby { base, ruby, under: false });
            }
            // a base alone draws as text; case 1 measures it up to its 0x02 for a later 0x03
            '\u{1}' => {
                if !cur.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut cur)));
                }
                let base = it.clone().any(|d| d == '\u{2}').then(|| it.clone().take_while(|&d| d != '\u{2}').collect::<String>());
                match base {
                    Some(b) => {
                        last_base = Some(b);
                        runs.push(Run::Control(c));
                    }
                    None => runs.push(Run::RubyAt { base: None, ruby: String::new() }),
                }
            }
            '\u{3}' => {
                if !cur.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut cur)));
                }
                let closed = it.clone().any(|d| d == '\u{4}');
                let ruby: String = it.by_ref().take_while(|&d| d != '\u{4}').collect();
                runs.push(Run::RubyAt { base: last_base.clone().filter(|_| closed), ruby });
            }
            c if c < ' ' && c != '\n' => {
                if !cur.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut cur)));
                }
                runs.push(Run::Control(c));
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
    if gothic {
        runs.push(Run::Font(FontOrder::List));
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
    /// the glyph cache's font order (`FontOrder`)
    font: FontOrder,
    /// the font order the line breaker measures the text in (`variant`)
    measure: FontOrder,
    /// English: the character the writer kerns the next one with (`FontAtlas::kern_in`), none at a line's start;
    /// nn::font's tags keep it
    prev: Option<char>,
    /// English: how far the writer's pen runs ahead of the line breaker's on the line after a raw 0x12
    /// (`raw_glyphs`), which the measure does not stretch by
    ahead: f32,
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
                return (atlas.advance_in(c, size, st.font) * sx, sx, after);
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
                (atlas.advance_in(c, size, st.font) * k, k, after)
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
        let w: f32 = chars.iter().map(|&c| atlas.advance_in(c, lay.size, st.font) * lay.sx + lay.spacing).sum();
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
            true => atlas.advance_in(c, lay.size, st.font) * k,
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

/// The room sub_BF0F8 gives the line after a tag 12 of a slot whose base is `off` from its x: the slot width
/// from the base (from the slot's x for a base past the slot), Tsukihime's 20 px more before a character of the
/// HANG list (the character after the tag in the slot's plain copy).
fn indent_room(lay: &Layout, off: f32, next: Option<char>) -> f32 {
    let width = lay.right - lay.left;
    let room = width - if off >= width { 0.0 } else { off.max(0.0) };
    let rules = kinsoku_rules();
    match next {
        Some(c) if !rules.witch && rules.hang.contains(c) => room + kinsoku::ROOM_MIN,
        _ => room,
    }
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
            Run::Italic | Run::Wobble(_) | Run::Plain | Run::Fill(_) | Run::Edge(_) | Run::Indent | Run::RubyAt { .. } => {}
            _ => return None,
        }
    }
    None
}

/// Lay out runs starting at pen (x, baseline), the slot's base x for tag 12 at `base` (+101032, sub_A8190;
/// 0 for none, which sub_C3710 makes the slot's x). Returns the writer's end pen (NaN after an English raw 0x12)
/// and the next order index.
#[allow(clippy::too_many_arguments)]
pub fn layout(atlas: &FontAtlas, lay: &Layout, runs: &[Run], x: f32, baseline: f32, base: f32, order: usize, out: &mut Vec<PlacedChar>) -> (f32, f32, usize) {
    layout_from(atlas, lay, runs, x, baseline, base, order, out, variant(runs))
}

/// `layout` with the writer starting in font order `start` (a message slot's: `GlyphCache::register`).
#[allow(clippy::too_many_arguments)]
pub fn layout_from(
    atlas: &FontAtlas,
    lay: &Layout,
    runs: &[Run],
    mut x: f32,
    mut baseline: f32,
    base: f32,
    mut order: usize,
    out: &mut Vec<PlacedChar>,
    start: FontOrder,
) -> (f32, f32, usize) {
    // the writer's style from the markup (`Run::Italic` / `Run::Wobble` / `Run::Plain`)
    let mut style = Style { font: start, measure: variant(runs), ..Style::default() };
    // where the text began, which tab stops count from
    let head = x;
    let start = out.len();
    // the slot's ruby mode (+100968, reset by jp_BDA9C): the ruby writer reads it at the draw, for every ruby
    let under = runs.iter().any(|r| matches!(r, Run::Ruby { under: true, .. }));
    // sub_BF0F8 takes a base at or past the slot's right edge back to the slot's x
    let base = if base == 0.0 || base - x >= lay.right - lay.left { x } else { base };
    // the line starts (sub_BF0F8, `plan_lines`), by the index of each `push`
    let plan = plan_lines(atlas, lay, runs, x, base, style.measure);
    // English text (N = -1): sub_BF0F8 breaks the line at a tag 12 when the room after the base is under
    // `ROOM_MIN_ENGLISH` (`indent_room`), as at every other tag 12 with less than `ROOM_MIN`
    let english_cuts: Vec<bool> = match &plan {
        None if english() => {
            let min = if crate::assets::witch() { kinsoku::ROOM_MIN_ENGLISH_WITCH } else { kinsoku::ROOM_MIN_ENGLISH };
            let rooms = runs.iter().enumerate().filter(|(_, r)| matches!(r, Run::Indent)).map(|(i, _)| indent_room(lay, base - x, next_char(runs, i)));
            rooms.map(|room| room < min || room < kinsoku::ROOM_MIN).collect()
        }
        _ => Vec::new(),
    };
    let cut = plan.as_ref().map_or(english_cuts.iter().filter(|&&c| c).count(), |p| p.cut);
    let mut indent = 0;
    let mut flow = Flow::new(atlas, runs, x > lay.left);
    let unit = std::cell::Cell::new(0usize);
    // `n` > 0: a run of n source chars `c` drawn as one stretched exfont glyph (`ex`) over n cells. `cell`:
    // the glyph's cell width and x scale when not the layout's (an ASCII glyph, `ascii_cell`), the glyph
    // centred in it
    let push = |c: char, ex: Option<(char, u8)>, cell: Option<(f32, f32)>, after: f32, raw: Option<&RawGlyph>, st: &Style, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| {
        let n = ex.map_or(1, |e| e.1) as f32;
        // a run crosses n full-width cells
        let adv = match cell {
            Some((w, _)) => w,
            None => (if ex.is_some() { lay.size } else { atlas.advance_in(c, lay.size, st.font) }) * lay.sx,
        };
        let width = n * (adv + lay.spacing) - lay.spacing;
        // the fixed-width writer's cell, and sub_DD334's pen moves around an exfont run (English: `english_text`)
        let fixed = lay.size * lay.sx;
        let shifts = ex.filter(|_| plan.is_some()).map(|e| run_shifts(f32::from(e.1), lay, st));
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
        // a run's glyph: centred on the cell at the pen (`run_quad` centres it over n cells from `gx`); a glyph
        // after a raw 0xD in its own cell after tag 0xD's move
        let moves = raw.and_then(|r| r.moves);
        let pen = match (shifts, moves) {
            (Some((before, _)), _) if !limited => *x + before,
            (None, Some((before, _))) => *x + before,
            _ => *x,
        };
        let (gx, sx) = match cell {
            Some((w, k)) => (pen + (w - atlas.advance_in(c, lay.size, st.font) * k) / 2.0, k),
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
            stretch: raw.map_or(if ex.is_none() && c == '…' { (lay.size * lay.sx + lay.spacing) / (lay.size * lay.sx) } else { 1.0 }, |r| r.stretch),
            sx,
            bar: None,
            italic: st.italic,
            wobble: (st.wobble > 0).then(|| Wobble::new(st.wobble)),
            fill: st.fill,
            shear: 0.0,
            font: st.font,
            blank: false,
            ruby: false,
        });
        // the run is one character to the reveal
        *order += 1;
        *x = match (shifts, moves) {
            (Some((_, back)), _) => pen + fixed + back + after,
            (None, Some((_, back))) => pen + width + back + after,
            _ => *x + width + after,
        };
    };
    let push_char = |c: char, st: &Style, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| {
        push(c, None, None, lay.spacing, None, st, x, baseline, order, out)
    };
    // the writer after raw control bytes, per text run's glyph
    let raw_plan = raw_glyphs(atlas, lay, runs, &style);
    let text = |t: &str, pass: Pass, next: Option<char>, raw: Option<&[RawGlyph]>, st: &mut Style, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>, flow: &mut Flow| {
        let chars = lone_dashes(t.chars().collect(), pass, |g| atlas.has_ex(g));
        if english() {
            let first = out.len();
            english_text(atlas, lay, &chars, pass, raw, st, (x, baseline), order, out, flow);
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
                    match raw.map(|r| &r[i]) {
                        Some(r) if r.plain => push(g, None, Some((r.cell, r.k)), r.after, None, st, x, baseline, order, out),
                        Some(r) if !r.skip => push(c, Some((g, n as u8)), None, r.after, None, st, x, baseline, order, out),
                        Some(_) => {}
                        None => push(c, Some((g, n as u8)), None, step[i + n - 1].2, None, st, x, baseline, order, out),
                    }
                    i += n;
                }
                _ => {
                    // an ASCII run of sub_BDC2C (tags 16 / 26 / 27), or a glyph in the fixed width
                    match raw.map(|r| &r[i]) {
                        Some(r) if !r.skip => push(c, None, Some((r.cell, r.k)), r.after, Some(r), st, x, baseline, order, out),
                        Some(_) => {}
                        None => {
                            let (cell, k, after) = step[i];
                            push(c, None, Some((cell, k)), after, None, st, x, baseline, order, out);
                        }
                    }
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
                style.prev = None;
                style.ahead = 0.0;
            }
            // markup and raw bytes a raw 0x07 / 0x08 took do nothing
            _ if raw_plan.as_ref().is_some_and(|p| p.taken[ri]) => {}
            Run::Text(t) => {
                let raw = raw_plan.as_ref().map(|p| p.glyphs[ri].as_slice());
                text(t, Pass::Text, next, raw, &mut style, &mut x, &mut baseline, &mut order, out, &mut flow)
            }
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
            Run::Edge(_) => {}
            Run::Control('\t') => {
                // nn::font's tab (sub_140CC8, the writer's tab width 4): the cursor, which carries no spacing
                // after its last glyph, to the next multiple of 4 glyph widths from where the text began, and no
                // character space before the next glyph (Switch: "abcd<tab>ef" puts e 4 x 45.6 px after a)
                let gap = match out[start..].last() {
                    Some(pc) if pc.baseline == baseline => if english() { if pc.run.is_some() { 0.0 } else { lay.spacing } } else { slot_space(pc.c, lay.spacing) },
                    _ => 0.0,
                };
                let stop = lay.size * lay.sx * 4.0;
                x = head + stop * (((x - gap - head) / stop).trunc() + 1.0);
            }
            Run::Control('\u{e}') => {
                style.italic = ITALIC_RATIO;
                style.seen = crate::assets::witch() || crate::assets::overseas();
            }
            Run::Control('\u{b}' | '\u{f}') => style.italic = 0.0,
            Run::Font(f) => style.font = *f,
            Run::Control(c) => {
                if let Some(f) = FontOrder::of_tag(*c) {
                    style.font = f;
                }
            }
            Run::Indent => {
                // sub_BF0F8 breaks the line at the tag (sub_C5610 puts a line feed before it), and each break
                // makes the text's first tag 12 left a 31
                let k = indent;
                indent += 1;
                let brk = match &plan {
                    Some(p) => p.indents.get(k).copied().unwrap_or(false),
                    None => english_cuts.get(k).copied().unwrap_or(false),
                };
                if brk {
                    x = lay.left;
                    baseline += lay.line_height;
                    flow.line = flow.pos;
                    flow.mid = false;
                    style.prev = None;
                    style.ahead = 0.0;
                }
                if k >= cut {
                    // the cursor to the base x; nn::font's writer puts the character space before every glyph but
                    // a line's first, so a glyph already on the line leaves it before the next one
                    let printed = out[start..].last().is_some_and(|pc| pc.baseline == baseline);
                    let gap = match next {
                        Some(c) if printed => if english() { lay.spacing } else { slot_space(c, lay.spacing) },
                        _ => 0.0,
                    };
                    x = base + gap;
                    // a NaN base (the cursor after a raw 0x12, `english_text`) is nonzero to sub_BF0F8, whose
                    // fmaxf takes its offset as 0: the line breaker gives the line the slot's whole width, while
                    // the writer draws from NaN up to the line's end
                    if x.is_nan() {
                        x = lay.left + gap;
                        style.ahead = f32::NAN;
                    }
                    flow.line = flow.pos;
                    flow.mid = x > lay.left;
                }
            }
            Run::Bar { row, text: t } => {
                let first = out.len();
                text(t, Pass::Text, next, None, &mut style, &mut x, &mut baseline, &mut order, out, &mut flow);
                if let Some(pc) = out.get_mut(first) {
                    let len = t.chars().count().min(usize::from(u8::MAX)) as u8;
                    pc.bar = Some(Bar { row: *row, len, top: pc.baseline - lay.ascent, cell: lay.size * lay.sx + lay.spacing });
                }
            }
            // a lone 0x03 stops the game (`Game::show_message`)
            Run::RubyAt { base: None, .. } => {}
            Run::Ruby { base, ruby, .. } | Run::RubyAt { base: Some(base), ruby } => {
                // a detached ruby (`Run::RubyAt`): over the earlier base's width up to the pen, which keeps the
                // spacing after the glyph before it; it shows with that glyph
                let at = matches!(run, Run::RubyAt { .. });
                // in a variant 1 text the ruby writer's switch to the ruby face and back (sub_DD334 case 3:
                // sub_CBDEC -> sub_CB478) puts every face in the gothic order again
                if style.measure == FontOrder::Gothic {
                    style.font = FontOrder::Gothic;
                }
                let bchars: Vec<char> = base.chars().collect();
                let bstep = steps(atlas, &bchars, next, Pass::Base, lay.size, lay.sx, lay.spacing, &style);
                let bw: f32 = bstep.iter().map(|s| s.0 + s.2).sum();
                // the base's last character must fit (the line size in characters above)
                let last = bstep.last().map_or(0.0, |s| s.0 + s.2);
                if !at && plan.is_none() && x + bw - last + lay.size * lay.sx / 2.0 > lay.right {
                    x = lay.left;
                    baseline += lay.line_height;
                }
                let first = if at { order.saturating_sub(1) } else { order };
                let gap = if at { trailing_gap(&runs[..ri], lay.spacing) } else { bstep.last().map_or(0.0, |s| s.2) };
                // sub_C1080 lays the base out as the text around it (ASCII runs, dash runs)
                if !at {
                    if english() {
                        for c in base.chars() {
                            let (at, y) = (out.len(), baseline);
                            push_char(c, &style, &mut x, &mut baseline, &mut order, out);
                            flow.placed(at, out.get(at).map_or(x, |pc| pc.x), baseline != y);
                        }
                    } else {
                        text(base, Pass::Base, next, None, &mut style, &mut x, &mut baseline, &mut order, out, &mut flow);
                    }
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
                let from = x - gap - bm;
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
                        x: rx + (cell - atlas.advance_in(c, rs, style.font) * gsx) / 2.0,
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
                        font: style.font,
                        blank: false,
                        ruby: true,
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
    // the cursor after the text (the click icon's, sub_C5FB4) has no `kern_last` of its last character, though
    // TextWriter::Print's end adds it to the character before it (Switch: the icon after a QA bubble line ending
    // in FOT-Seurat's ’, with U+0000 found, sits after the ’'s full advance)
    // a line start after the last character (the text's last line ending at its run's 0x11 tag)
    if plan.is_some_and(|p| p.trailing) {
        x = lay.left;
        baseline += lay.line_height;
        style.ahead = 0.0;
    }
    // the draw buffer's shear height at the message's draw: its last glyph's (a ruby's after the base)
    if let Some(h) = out[start..].last().map(|pc| italic_height(pc.size)) {
        for pc in &mut out[start..] {
            pc.shear = h;
        }
    }
    // the writer's own pen (English: ahead of the line breaker's after a raw 0x12)
    (x + style.ahead, baseline, order)
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
    /// per tag 12 (`Run::Indent`): a line starts at it
    indents: Vec<bool>,
    /// the tags 12 sub_BF0F8 turned into line breaks (`kinsoku::Breaks::cut`): the text's first this many draw
    /// nothing
    cut: usize,
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

/// A message glyph as the tag processor leaves the writer after raw control bytes (`raw_glyphs`): its cell
/// and x scale, the spacing before the next glyph, its stretch (`PlacedChar::stretch`).
#[derive(Clone, Copy, Debug)]
struct RawGlyph {
    /// a raw 0x07 / 0x08 took it from the text: not drawn
    skip: bool,
    cell: f32,
    k: f32,
    after: f32,
    stretch: f32,
    /// the stretch of tag 0xD alone, which the line breaker's measure keeps (sub_DD0D8 has no tag 0x12)
    mstretch: f32,
    /// a glyph between a raw 0x0D and its 0x06: tag 0xD's move before it (the first) and tag 6's after it
    moves: Option<(f32, f32)>,
    /// an exfont run whose tag 0xD a raw 0x07 / 0x08 took: its glyph once, in the writer's cell
    plain: bool,
}

/// The writer fields the tag processor saves and sets: the fixed width (+32), x scale (+12, `sx` with the
/// stretch of tags 0x12 and 0xD apart), character space (+64), as fractions of the layout's, and whether
/// the width is fixed (+56).
#[derive(Clone, Copy)]
struct Pen {
    cell: f32,
    sx: f32,
    stretch: f32,
    mstretch: f32,
    sp: f32,
    fixed: bool,
}

/// One unit of the text sub_BDC2C hands the writer.
enum Item {
    /// a tag it writes: an ASCII run's 0x10 / 0x1A / 0x1B and 0x11, an exfont run's 0xD and 6, a …'s 0x12 / 0x13
    Tag(char),
    /// the character after an exfont run's 0xD: its count
    Count(u32),
    /// a glyph: (run, character) of the text runs' characters (`lone_dashes`), `true` an exfont run's
    Glyph(usize, usize, bool),
    /// a raw control byte or a markup tag: (run, tag)
    Raw(usize, char),
}

/// What raw control bytes do to a message's writer (`raw_glyphs`): each text run's glyphs (by `lone_dashes`
/// character, an exfont run's at its first) and the runs a raw 0x07 / 0x08 took.
struct RawPlan {
    glyphs: Vec<Vec<RawGlyph>>,
    taken: Vec<bool>,
}

/// The per-glyph writer state for message text with raw control bytes that change it (0x06 / 0x07 / 0x08 / 0x0D
/// / 0x10..0x13, Witch on the Holy Night's 0x1A / 0x1B, the overseas 0x1A), else `None`. sub_BDC2C copies a raw
/// byte as it is between its own tags, and the tag processor (sub_DD334; Witch on the Holy Night's jp_DD334,
/// the overseas sub_DB650) takes them alike: 0x10 saves the writer's width, x scale and spacing (+72 / +52 /
/// +68) and halves the width at the run's x scale (`Style::ascii_scale`) and spacing (`run_spacing`), the
/// width proportional in Witch on the Holy Night but for 0x1A; 0x11 restores them; 0x12 saves them and widens
/// the x scale by (width + spacing) / width, 0x13 restores the x scale and spacing; 0xD reads the next
/// character as a count n, saves the x scale and spacing and stretches the x scale over n cells (`run_scale`)
/// with its pen moves (`run_shifts`), 6 restores the x scale; 0x07 / 0x08 read the next character, a tag of
/// sub_BDC2C's or a glyph, and drop it. One save serves them all, so a raw 0x10 before an ASCII run leaves the
/// text after the run at half width (Switch: `\x10ab\x11cdかき` draws かき at 0.725 in half cells), and a 0x08
/// before one drops its 0x10 (`e\x08fgh`: f g h in full cells).
fn raw_glyphs(atlas: &FontAtlas, lay: &Layout, runs: &[Run], start: &Style) -> Option<RawPlan> {
    let (witch, overseas, eng) = (crate::assets::witch(), crate::assets::overseas(), english());
    let stateful = |c: char| matches!(c, '\u{6}'..='\u{8}' | '\r' | '\u{10}'..='\u{13}') || matches!(c, '\u{1a}' | '\u{1b}') && (witch || overseas);
    if !runs.iter().any(|r| matches!(r, Run::Control(c) if stateful(*c))) {
        return None;
    }
    // sub_BDC2C's text: ASCII runs in their tags, closed before any other character or a run's end (English
    // text has neither ASCII runs nor … tags)
    let mut chars: Vec<Vec<char>> = Vec::new();
    let mut items = Vec::new();
    for (ri, run) in runs.iter().enumerate() {
        let cs = match run {
            Run::Text(t) => lone_dashes(t.chars().collect(), Pass::Text, |g| atlas.has_ex(g)),
            _ => Vec::new(),
        };
        match run {
            Run::Text(_) => {
                let mut open = false;
                let mut i = 0;
                while i < cs.len() {
                    let c = cs[i];
                    let ascii = !eng && (' '..='~').contains(&c);
                    if open && !ascii {
                        items.push(Item::Tag('\u{11}'));
                        open = false;
                    }
                    let n = cs[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
                    match run_kind(c) {
                        Some(g) if atlas.has_ex(g) && makes_run(g, n, Pass::Text) => {
                            items.extend([Item::Tag('\r'), Item::Count(n as u32), Item::Glyph(ri, i, true), Item::Tag('\u{6}')]);
                            i += n;
                            continue;
                        }
                        _ if ascii && !open => {
                            items.push(Item::Tag(run_tag(&cs, i, Pass::Text)));
                            open = true;
                            items.push(Item::Glyph(ri, i, false));
                        }
                        _ if c == '…' && !eng => items.extend([Item::Tag('\u{12}'), Item::Glyph(ri, i, false), Item::Tag('\u{13}')]),
                        _ => items.push(Item::Glyph(ri, i, false)),
                    }
                    i += 1;
                }
                if open {
                    items.push(Item::Tag('\u{11}'));
                }
            }
            Run::Control(c) => items.push(Item::Raw(ri, *c)),
            // the markup's own tags: `@i` 14, `@r` 11, `@g` / `@h` 20 / 21
            Run::Italic => items.push(Item::Raw(ri, '\u{e}')),
            Run::Plain => items.push(Item::Raw(ri, '\u{b}')),
            Run::Font(f) => items.push(Item::Raw(ri, if *f == FontOrder::Gothic { '\u{14}' } else { '\u{15}' })),
            _ => {}
        }
        chars.push(cs);
    }
    // the English writer has no fixed width (+56 clear, +32 = 0: a raw 0x12 widens the x scale without bound and
    // the text after it on the line draws nowhere; Switch, `mn\x12op\x13qr` draws "mn")
    let base = Pen { cell: 1.0, sx: 1.0, stretch: 1.0, mstretch: 1.0, sp: 1.0, fixed: !eng };
    // the processor's fields outlive a text; a message's runs leave the last save at the plain writer
    let (mut pen, mut save) = (base, base);
    let mut st = Style { font: start.font, ..Style::default() };
    let fixed = if eng { 0.0 } else { lay.size * lay.sx };
    // the overseas +138 / +137: a proportional run was opened, and 0x11 fixes the width again
    let mut latched = false;
    let skip = RawGlyph { skip: true, cell: 0.0, k: 0.0, after: 0.0, stretch: 1.0, mstretch: 1.0, moves: None, plain: false };
    let mut glyphs: Vec<Vec<RawGlyph>> = chars.iter().map(|cs| vec![skip; cs.len()]).collect();
    let mut taken = vec![false; runs.len()];
    // drawn glyphs in order with their spacing fraction, for the spacing before the next
    let mut order: Vec<((usize, usize), f32)> = Vec::new();
    // a raw 0xD's run: its moves and whether its first glyph is still to come; the last glyph drawn in it
    let mut stretched: Option<((f32, f32), bool)> = None;
    let mut last: Option<(usize, usize)> = None;
    // the character after a 0xD is its count (`Some(true)` a raw one's); an exfont run lost its 0xD
    let mut count: Option<bool> = None;
    let mut ex_plain = false;
    let mut eat = false;
    for item in &items {
        if let Some(raw) = count.take() {
            let n = match *item {
                Item::Tag(c) | Item::Raw(_, c) => c as u32,
                Item::Count(n) => n,
                Item::Glyph(ri, i, _) => chars[ri][i] as u32,
            };
            if let Item::Raw(ri, _) = *item {
                taken[ri] = true;
            }
            save = Pen { cell: save.cell, ..pen };
            if raw {
                let n = n as f32;
                let w = pair_width() * lay.size / BASE_SIZE;
                let ratio = (w * n + lay.spacing * (n - 1.0)) / w * run_stretch();
                pen.stretch *= ratio;
                pen.mstretch *= ratio;
                // the free writer's moves (`free_run_moves`), the fixed one's (`run_shifts`)
                let moves = if eng {
                    let (v, _, after, ..) = free_run_moves(n, lay, st.seen, st.italic);
                    (v, after - lay.spacing)
                } else {
                    run_shifts(n, lay, &st)
                };
                stretched = Some((moves, true));
            }
            continue;
        }
        if std::mem::take(&mut eat) {
            match *item {
                Item::Glyph(ri, i, _) => glyphs[ri][i] = skip,
                Item::Raw(ri, _) => taken[ri] = true,
                // an exfont run's 0xD taken: its count reads as a tag, and its glyph draws once
                Item::Tag('\r') => ex_plain = true,
                _ => {}
            }
            continue;
        }
        let tag = match *item {
            Item::Tag(c) | Item::Raw(_, c) => c,
            Item::Count(n) => match char::from_u32(n) {
                Some(c) if ex_plain && c < ' ' => c,
                _ => continue,
            },
            Item::Glyph(ri, i, ex) => {
                let c = chars[ri][i];
                let k = lay.sx * pen.sx;
                let cell = if pen.fixed { fixed * pen.cell } else { atlas.advance_in(c, lay.size, st.font) * k };
                let moves = stretched.as_mut().map(|(m, first)| (if std::mem::take(first) { m.0 } else { 0.0 }, 0.0));
                if moves.is_some() {
                    last = Some((ri, i));
                }
                let plain = ex && std::mem::take(&mut ex_plain);
                glyphs[ri][i] = RawGlyph { skip: false, cell, k, after: 0.0, stretch: pen.stretch, mstretch: pen.mstretch, moves, plain };
                order.push(((ri, i), pen.sp));
                continue;
            }
        };
        match tag {
            '\u{7}' | '\u{8}' => eat = true,
            '\r' => count = Some(matches!(item, Item::Raw(..))),
            '\u{6}' => {
                if let (Some(((_, back), _)), Some((ri, i))) = (stretched.take(), last.take()) {
                    let g = &mut glyphs[ri][i];
                    g.moves = g.moves.map(|(b, _)| (b, back));
                }
                pen.sx = save.sx;
                pen.stretch = save.stretch;
                pen.mstretch = save.mstretch;
            }
            '\u{10}' | '\u{1a}' | '\u{1b}' => {
                save = pen;
                pen.cell = save.cell * 0.5;
                pen.sx = save.sx * st.ascii_scale();
                pen.sp = save.sp * run_spacing();
                if witch && tag != '\u{1a}' || overseas && tag == '\u{1b}' {
                    pen.fixed = false;
                    latched = true;
                }
            }
            '\u{11}' => pen = Pen { fixed: if witch || latched { true } else { pen.fixed }, ..save },
            '\u{12}' => {
                save = pen;
                let w = fixed * pen.cell;
                pen.stretch *= (w + lay.spacing * pen.sp) / w;
            }
            '\u{13}' => pen = Pen { cell: pen.cell, fixed: pen.fixed, ..save },
            '\u{e}' => {
                st.italic = ITALIC_RATIO;
                st.seen = witch || crate::assets::overseas();
            }
            '\u{b}' | '\u{f}' => st.italic = 0.0,
            c => {
                if let Some(f) = FontOrder::of_tag(c) {
                    st.font = f;
                }
            }
        }
    }
    // the spacing before a glyph is the writer's as it prints it
    for w in 0..order.len() {
        let ((ri, i), sp) = order[w];
        glyphs[ri][i].after = lay.spacing * order.get(w + 1).map_or(sp, |o| o.1);
    }
    Some(RawPlan { glyphs, taken })
}

/// sub_DD334 (Witch on the Holy Night's jp_DD334, the overseas sub_DB650) around an exfont run of `n` in a
/// fixed-width writer: tag 0xD stretches the x scale to k (W n + sp (n - 1)) / W (`run_quad`; W the pair font's
/// scaled width, sp the spacing) and moves the pen by `before` so that the glyph nn::font centres in its cell
/// covers the run; tag 6, after the glyph's cell, moves it by `after`. Italic (tag 0xE: the writer's +36 =
/// 0.25 and, in Witch on the Holy Night and the overseas build, +132 / +136) changes both.
fn run_shifts(n: f32, lay: &Layout, st: &Style) -> (f32, f32) {
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

fn plan_lines(atlas: &FontAtlas, lay: &Layout, runs: &[Run], x: f32, base: f32, order: FontOrder) -> Option<LinePlan> {
    plan_lines_with(|c| atlas.has_ex(c), |c| atlas.advance_in(c, lay.size, order), atlas.bubble, lay, runs, x, base)
}

/// The font order a text starts in and is measured in: the overseas sub_BDC2C sets the slot's +0x440 to 1 when
/// `@g` writes its tag 20, and sub_BCE64 then lays the text out with the font objects of glyph cache variant 1,
/// whose selection (sub_CB478) puts every face in the gothic order; the measuring tag processor passes over
/// tags 20 / 21, and the writer draws from the text's start in it up to the first tag 21 (`@h`, or the one
/// after the text). Switch, English: "AVAWAY To Ty. @gAVAWAY…@h back again." in DemosNext up to `@h`, a
/// plain text after it in Helvetica Neue. The Japanese build has no variant 1 text: there, only what follows
/// `@g` (`FontOrder`).
fn variant(runs: &[Run]) -> FontOrder {
    match crate::assets::overseas() && runs.iter().any(|r| matches!(r, Run::Font(FontOrder::Gothic))) {
        true => FontOrder::Gothic,
        false => FontOrder::Own,
    }
}

/// nn::font's glyph cache as the page's texts fill it. Every font order call sets the order of all four faces of
/// the cache that variants 0..2 share (sub_143D98 from sub_CB478 / sub_CB74C / sub_CB688), and the order stays as
/// the last print left it. A slot's print (sub_C5FB4, vtable +16) registers its text (`Print`) and then looks its
/// glyphs up for the draw buffer (jp_1406A8 / sub_1406A8): a glyph draws when its node (size, font, code;
/// sub_144750) is registered or was drawn since the cache's last completion (flags 1 / 4, rolled into 0x10 by
/// sub_1450D0 at every completion, sub_CB95C after the print's registration), else as the space glyph at its own
/// advance. The page's slots print every frame: the earlier messages from the order the last one left, two
/// completions, then every message of the page in turn, which is what shows. A print can so draw what it or the
/// print before it registered or drew, nothing else. Switch, English: "A long line that will need breaking
/// @gsomewhere…" after "…@gAVAWAY To Ty. Gothic words@h back again." draws "breaking" in DemosNext with no b and
/// k (registered in Helvetica Neue only); Japanese, 「か\x14きく」 / 「あいう」 / 「\x15えおか」: 「　きく」 (か looked
/// up in TsukuMin, registered in TsukuGo), あいう in TsukuGo, nothing of the third (Astris, the cache's nodes
/// read from guest memory every frame). The backlog's entries print every frame in turn, with no earlier
/// prints: there the same page is 「かきく」 (the first from the list order the last left), あいう in TsukuGo,
/// nothing of the third.
#[derive(Clone)]
pub struct GlyphCache {
    /// the order the last print of a frame left
    order: FontOrder,
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self { order: FontOrder::List }
    }
}

/// A slot's print: the glyphs it registers (whether a ruby's, the font's place in the list, the character) and
/// the order its writer starts and ends in.
struct Print {
    regs: HashSet<(bool, u8, char)>,
    /// the glyphs in the order it registers them
    seq: Vec<Registration>,
    start: FontOrder,
    end: FontOrder,
}

impl GlyphCache {
    /// A new font list (the language's fonts loaded again) starts a new cache.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The print of `runs` with the faces in order `order` (sub_C5FB4): sub_C445C's six strings of the tagged text
    /// (by the last of tags 20 / 21 / 24 before each, a ruby's in its own three), each registered (sub_CBF10)
    /// under its order: the plain ones under the faces' order as it is, tag 20's gothic, tag 24's the bubble's,
    /// both then back to the list (the ruby face's after sub_CB478 for the text's variant). The QA bubble's text
    /// (style 0x40) is all tag 24's: sub_BDC2C puts tag 24 before it and tag 21 after it. The writer then starts
    /// in the variant's order (sub_CBDEC -> sub_CB478: variant 1 puts every face in the gothic order, the
    /// bubble's variant 2 in the bubble's, the bubble atlas's own; variant 0 leaves it) and follows the text's
    /// tags; in a variant 1 text the ruby writer's switch to the ruby face and back (sub_DD334 case 3:
    /// sub_CBDEC) puts the faces in the gothic order again.
    fn print(atlas: &FontAtlas, runs: &[Run], mut order: FontOrder) -> Print {
        let v1 = variant(runs) == FontOrder::Gothic;
        let mut text: [Vec<char>; 3] = Default::default();
        let mut ruby: [Vec<char>; 3] = Default::default();
        let mut state = if atlas.bubble { 2 } else { 0 };
        // tags 5 / 7 / 8 / 13 take the character after them
        let mut skip = false;
        let push = |to: &mut Vec<char>, t: &str, skip: &mut bool| {
            for c in t.chars() {
                if std::mem::take(skip) {
                    continue;
                }
                if c >= ' ' {
                    to.push(c);
                }
            }
        };
        for r in runs {
            match r {
                Run::Text(t) | Run::Bar { text: t, .. } => push(&mut text[state], t, &mut skip),
                Run::Ruby { base, ruby: rt, .. } => {
                    push(&mut text[state], base, &mut skip);
                    push(&mut ruby[state], rt, &mut skip);
                }
                Run::RubyAt { ruby: rt, .. } => push(&mut ruby[state], rt, &mut skip),
                Run::Font(f) => state = match f {
                    FontOrder::Gothic => 1,
                    FontOrder::Bubble => 2,
                    _ => 0,
                },
                Run::Control(c) => match c {
                    '\u{5}' | '\u{7}' | '\u{8}' | '\u{d}' => skip = true,
                    c => match FontOrder::of_tag(*c) {
                        Some(FontOrder::Gothic) => state = 1,
                        Some(FontOrder::Bubble) => state = 2,
                        Some(_) => state = 0,
                        None => {}
                    },
                },
                _ => {}
            }
        }
        let (mut regs, mut seq) = (HashSet::new(), Vec::new());
        let mut add = |is_ruby: bool, chars: &[char], order: FontOrder| {
            let faces = atlas.faces(order);
            for &c in chars {
                if let Some(i) = atlas.font_in(c, &faces) {
                    regs.insert((is_ruby, atlas.langs[i], c));
                    seq.push((is_ruby, c, order));
                }
            }
        };
        let ruby_order = |o: FontOrder| if v1 { FontOrder::Gothic } else { o };
        add(false, &text[0], order);
        for (k, o) in [(1, FontOrder::Gothic), (2, FontOrder::Bubble)] {
            if !text[k].is_empty() {
                add(false, &text[k], o);
                order = FontOrder::List;
            }
        }
        if !ruby[0].is_empty() {
            order = ruby_order(order);
            add(true, &ruby[0], order);
        }
        for (k, o) in [(1, FontOrder::Gothic), (2, FontOrder::Bubble)] {
            if !ruby[k].is_empty() {
                add(true, &ruby[k], ruby_order(o));
                order = FontOrder::List;
            }
        }
        let start = match variant(runs) {
            FontOrder::Gothic => FontOrder::Gothic,
            _ if atlas.bubble => FontOrder::Own,
            _ => order,
        };
        let mut end = start;
        for r in runs {
            match r {
                Run::Font(f) => end = *f,
                Run::Control(c) => end = FontOrder::of_tag(*c).unwrap_or(end),
                Run::Ruby { .. } | Run::RubyAt { base: Some(_), .. } if v1 => end = FontOrder::Gothic,
                _ => {}
            }
        }
        // the bubble text's closing tag 21
        if atlas.bubble {
            end = FontOrder::List;
        }
        Print { regs, seq, start, end }
    }

    /// The prints of a page's texts (`earlier`: a message page's, whose earlier messages print first) as the
    /// frames settle into them.
    fn frame(&mut self, atlas: &FontAtlas, texts: &[&[Run]], earlier: bool) -> Vec<Print> {
        let mut prints = Vec::new();
        for _ in 0..8 {
            // from the order the last print of the frame before left
            let mut order = self.order;
            for runs in texts.iter().take(if earlier { texts.len().saturating_sub(1) } else { 0 }) {
                order = Self::print(atlas, runs, order).end;
            }
            prints = texts
                .iter()
                .map(|runs| {
                    let p = Self::print(atlas, runs, order);
                    order = p.end;
                    p
                })
                .collect();
            if std::mem::replace(&mut self.order, order) == order {
                break;
            }
        }
        prints
    }

    /// The registrations of a message's first print (`FontAtlas::register`), which the frame after its layout
    /// makes: from the order the last print left, a variant 1 text's from the gothic order its layout's font
    /// objects (sub_BCE64 with variant 1, sub_CB478) put every face in. Switch, EG3 (the cache's entries read
    /// from guest memory): `@gAVAWAY To Ty.@h AVAWAY To Ty. VAVA` puts the gothic face's ' ' A V W Y T o y . in
    /// the sheet in that order, its plain string's, before the next message's A V W Y T in the list order.
    pub fn first_print(&self, atlas: &FontAtlas, runs: &[Run]) -> Vec<Registration> {
        let order = if variant(runs) == FontOrder::Gothic { FontOrder::Gothic } else { self.order };
        Self::print(atlas, runs, order).seq
    }

    /// The registrations of the frame's prints of a page's texts (`earlier` as for `frame`), in turn.
    pub fn registrations(&mut self, atlas: &FontAtlas, texts: &[&[Run]], earlier: bool) -> Vec<Registration> {
        self.frame(atlas, texts, earlier).into_iter().flat_map(|p| p.seq).collect()
    }

    /// The order each of a page's texts starts drawing in (`layout_from`; `earlier` as for `frame`).
    pub fn starts(&mut self, atlas: &FontAtlas, texts: &[&[Run]], earlier: bool) -> Vec<FontOrder> {
        self.frame(atlas, texts, earlier).iter().map(|p| p.start).collect()
    }

    /// Mark the page's glyphs (text `i`'s from `firsts[i]`) the frame's prints draw as the space glyph
    /// (`PlacedChar::blank`): a glyph of the atlas's fonts (not an exfont's) its print neither registered nor
    /// found drawn by the print before.
    pub fn blank(&mut self, atlas: &FontAtlas, texts: &[&[Run]], chars: &mut [PlacedChar], firsts: &[usize], size: f32, earlier: bool) {
        let prints = self.frame(atlas, texts, earlier);
        let mut drawn = HashSet::new();
        for (i, p) in prints.iter().enumerate() {
            let end = firsts.get(i + 1).copied().unwrap_or(chars.len()).min(chars.len());
            let live: HashSet<_> = p.regs.union(&drawn).copied().collect();
            drawn.clear();
            for pc in chars[firsts[i].min(end)..end].iter_mut() {
                pc.blank = false;
                if atlas.is_ex(pc) {
                    continue;
                }
                if let Some(f) = atlas.font_in(pc.c, &atlas.faces(pc.font)) {
                    let key = (pc.size < size, atlas.langs[f], pc.c);
                    if live.contains(&key) {
                        drawn.insert(key);
                    } else {
                        pc.blank = true;
                    }
                }
            }
        }
    }
}

/// `plan_lines` over the atlas's exfont glyphs (`has_ex`) and advances at the layout size (`advance`), for a
/// slot at `x` with its tag 12 base x at `base`.
#[allow(clippy::too_many_arguments)]
fn plan_lines_with(has_ex: impl Fn(char) -> bool, advance: impl Fn(char) -> f32, bubble: bool, lay: &Layout, runs: &[Run], x: f32, base: f32) -> Option<LinePlan> {
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
            Run::Indent => {
                close(&mut plain, &mut open);
                offs.push(plain.len());
                lasts.push(plain.len());
                plain.push('\u{c}');
            }
            Run::Control(c) => {
                close(&mut plain, &mut open);
                plain.push(*c);
            }
            Run::Font(f) => plain.push(if *f == FontOrder::Gothic { '\u{14}' } else { '\u{15}' }),
            _ => close(&mut plain, &mut open),
        }
    }
    close(&mut plain, &mut open);
    // the line feed and tag 12 units are no pushes of `layout`'s
    let (newline, indent): (Vec<bool>, Vec<bool>) = {
        let b = plain.as_bytes();
        offs.iter().map(|&o| (b.get(o) == Some(&b'\n'), b.get(o) == Some(&0x0C))).unzip()
    };
    // Witch on the Holy Night's proportional runs at the run's x scale (the plain copy carries no wide tag)
    let prop = |c: char| advance(c) * lay.sx * 0.725;
    let m = kinsoku::Measure {
        cell: lay.size * lay.sx,
        spacing: lay.spacing,
        run_spacing: run_spacing(),
        prop: witch.then_some(&prop as &dyn Fn(char) -> f32),
    };
    let breaks = kinsoku::breaks(lb, plain.as_bytes(), lay.right - x, lay.right - lay.left, base - x, &m, kinsoku_rules());
    let mut starts = vec![false; offs.len()];
    let mut trailing = false;
    for b in breaks.starts {
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
    let indents = starts.iter().zip(&indent).filter(|&(_, &i)| i).map(|(&s, _)| s).collect();
    let starts = starts.into_iter().zip(newline.iter().zip(&indent)).filter(|&(_, (&nl, &i))| !nl && !i).map(|(s, _)| s).collect();
    Some(LinePlan { starts, trailing, indents, cut: breaks.cut })
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
    fn wrap(&mut self, from: usize, lay: &Layout, kern: f32, (x, baseline): (&mut f32, &mut f32), out: &mut [PlacedChar]) {
        if let Some((first, pen)) = self.at.get(from..).and_then(|a| a.iter().flatten().next().copied()) {
            let dx = lay.left + kern - pen;
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

/// `english_text`'s exfont run of n at the italic ratio `italic` (`seen`: `Style::seen`): (tag 0xD's move,
/// the stretched glyph's advance, the pen's move after it, the glyph's italic ratio, the ratio after the run).
fn free_run_moves(n: f32, lay: &Layout, seen: bool, italic: f32) -> (f32, f32, f32, f32, f32) {
    let (sp, s) = (lay.spacing, lay.size * lay.sx / BASE_SIZE);
    let v = (sp * s - sp * (n - 1.0) / 2.0 - 1.0) * 0.25 / (italic + 1.0);
    let free = seen && crate::assets::overseas();
    let it = if free { ITALIC_RATIO / n } else { italic };
    let back = (v + v - v / (it + 1.0)) * 0.25 - sp * s;
    (v, run_stretch() * (pair_width() * s * n + sp * (n - 1.0)), sp + back, it, if free { ITALIC_RATIO } else { italic })
}

/// English message text (the overseas build's +101009 = English, sub_A7790): no per-line character count
/// (sub_BD7EC), but nn::font's line breaking. For each line sub_BF0F8 finds the first character that runs past
/// the slot width (sub_C7240 / sub_145334: pen + spacing + kerning + advance > width; a line's first character
/// always fits) and asks sub_148988 for the line's last character over the whole text (`Flow`); sub_C471C
/// breaks right after it, so a line keeps its trailing spaces. Glyphs advance by their own width plus the
/// kerning before them and the spacing.
#[allow(clippy::too_many_arguments)]
fn english_text(atlas: &FontAtlas, lay: &Layout, chars: &[char], pass: Pass, raw: Option<&[RawGlyph]>, st: &mut Style, (x, baseline): (&mut f32, &mut f32), order: &mut usize, out: &mut Vec<PlacedChar>, flow: &mut Flow) {
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
            // a raw 0x07 / 0x08 took the character (`raw_glyphs`)
            _ if raw.is_some_and(|r| r[k].skip) => k += 1,
            _ => {
                units.push((k, 1, None));
                k += 1;
            }
        }
    }
    // a character's writer after raw control bytes (`raw_glyphs`)
    let raw_at = |u: (usize, usize, Option<char>)| raw.map(|r| r[u.0]).filter(|_| u.2.is_none());
    // an exfont run of n at the italic ratio `italic`: the tag processor (sub_DB650; Witch on the Holy Night's
    // jp_DD334) of a writer without fixed width (+56 = 0) centres nothing (+92 clear for English, so D = 0) and
    // quarters both moves: tag 0xD moves the pen by v = (sp s - sp (n - 1) / 2 - 1) / 4 / (italic + 1), the
    // stretched glyph advances by 0.98 (W n + sp (n - 1)) (W = 41 s) plus the spacing, and tag 6 moves it by
    // (v + v - v / (italic' + 1)) / 4 - sp s. The overseas one, once `@i` was seen, draws the glyph at the
    // ratio 0.25 / n (italic') and is at 0.25 after it, `@r` or not. Switch, English: the text after a
    // two-dash or two-box run 0.5 px right of n (size + spacing) - spacing on. Returns (v, the glyph's
    // advance, the pen's move after it, italic', the ratio after the run).
    let run_moves = |n: usize, italic: f32| free_run_moves(n as f32, lay, st.seen, italic);
    let next_prev = |u: (usize, usize, Option<char>)| u.2.unwrap_or(chars[u.0]);
    // a unit's kerning, width, the spacing after it and the kerning after it at the end of a line (`kern_last`),
    // at the italic ratio `italic`, in font order `font` (the line breaker measures in the text's `variant`, the
    // writer draws in the current). A run kerns as its glyph, which TextWriter::Print reads as a character
    let width = |prev: Option<char>, u: (usize, usize, Option<char>), italic: f32, font: FontOrder| {
        let c = next_prev(u);
        let kern = atlas.kern_in(prev, c, lay.size, font) * lay.sx;
        let last = atlas.kern_last(c, lay.size, font) * lay.sx;
        match u.2 {
            Some(_) => {
                let (v, adv, after, ..) = run_moves(u.1, italic);
                (kern, v + adv, after, last)
            }
            // the line breaker's measuring tag processor has no tag 0x12 (sub_DD0D8): the x scale unstretched;
            // TextWriter::Print scales the kerning by it too (jp_145B78: +12 x GetKerning)
            None => match raw_at(u) {
                Some(r) => {
                    let m = r.k * r.mstretch / lay.sx;
                    (kern * m, atlas.advance_in(c, lay.size, font) * r.k * r.mstretch, r.after, last * m)
                }
                None => (kern, atlas.advance_in(c, lay.size, font) * lay.sx, lay.spacing, last),
            },
        }
    };
    let italic_after = |u: (usize, usize, Option<char>), italic: f32| if u.2.is_some() { run_moves(u.1, italic).4 } else { italic };
    let lb = LINE_BREAK.get();
    // the characters' places in the flow
    let base = flow.pos;
    let mut i = 0;
    while i < units.len() {
        // the first unit past the width
        let mut px = *x;
        let mut kp = st.prev;
        let mut italic = st.italic;
        let mut overflow = None;
        for (j, &u) in units.iter().enumerate().skip(i) {
            let (kern, w, after, last) = width(kp, u, italic, st.measure);
            if (j > i || *x > lay.left) && px + kern + w + last > lay.right {
                overflow = Some(j);
                break;
            }
            px += kern + w + after;
            kp = Some(next_prev(u));
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
                    // the line's new first character kerns by its `palt` placement alone
                    let first = flow.chars.get(from).map_or(0.0, |&c| atlas.kern_in(None, c, lay.size, st.font) * lay.sx);
                    flow.wrap(from, lay, first, (x, baseline), out);
                    st.prev = None;
                    st.ahead = 0.0;
                    continue;
                }
                // the unit holding that character
                units.iter().rposition(|u| base + u.0 <= c).unwrap_or(i).max(i)
            }
        };
        for &u in &units[i..=last] {
            let (kern, mut w, mut after, _) = width(st.prev, u, st.italic, st.font);
            *x += kern;
            flow.at[base + u.0] = Some((out.len(), *x));
            let (c, run, mut gx, italic) = match u.2 {
                Some(g) => {
                    let (v, _, _, it, _) = run_moves(u.1, st.italic);
                    (g, Some((u.1 as u8, lay.spacing)), *x + v, it)
                }
                None => (chars[u.0], None, *x, st.italic),
            };
            // after a raw 0x12 or 0xD the glyph kerns, draws and advances at the stretched x scale, a 0xD's moves
            // around it; the draw's pen runs ahead of the line breaker's by tag 0x12's stretch. A 0x12's is
            // infinite (+32 = 0), and IEEE arithmetic as the writer's: a kerning of 0 times it makes the pen NaN
            // for the rest of the line, as on the Switch (the click icon at the line's left, `Game` `as i32`)
            let mut sx = lay.sx;
            let r = raw_at(u);
            if let Some(r) = r {
                st.ahead += kern * (r.stretch / r.mstretch - 1.0);
            }
            gx += st.ahead;
            if let Some(r) = r {
                sx = r.k * r.stretch;
                st.ahead += w * (r.stretch / r.mstretch - 1.0);
                if let Some((before, back)) = r.moves {
                    gx += before;
                    w += before;
                    after += back;
                }
            }
            if gx.is_finite() && sx.is_finite() {
                out.push(PlacedChar { c, x: gx, baseline: *baseline, size: lay.size, order: *order, run, stretch: 1.0, sx, bar: None, italic, wobble: None, fill: 0, shear: 0.0, font: st.font, blank: false, ruby: false });
            }
            *order += 1;
            *x += w + after;
            st.prev = Some(next_prev(u));
            st.italic = italic_after(u, st.italic);
        }
        i = last + 1;
        if i < units.len() {
            *x = lay.left;
            *baseline += lay.line_height;
            st.prev = None;
            st.ahead = 0.0;
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
            let plan = plan_lines_with(|_| true, |_| lay.size, false, &lay, &runs, lay.left, lay.left).unwrap();
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

