//! Font rasterisation into an R8 atlas, and NVL page layout.

use std::collections::HashMap;

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use knife_core::format::bffnt;
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

/// The fonts' base size (the font manager's +124, from sub_AB0F0's init struct): glyph scale = size / 40.
pub const BASE_SIZE: f32 = 40.0;
/// The pair font's (exfont + the system shared font at the base size) width and line feed, in base px: the
/// larger font's, the shared font's 50 (the exfont has 41 / 47). Switch: line pitch 50 * 1.14 + 24 = 81 px,
/// and the 3- and 6-dash runs 140.0 / 285.9 px wide (the 0xD stretch gives 141.6 / 286.4 with 50).
pub const PAIR_METRIC: f32 = 50.0;
/// An exfont cell's top above the line's baseline, in base-size px. Switch (Astris): the dash ink centre
/// (cell row 23) 18.35 px above the baseline at 45.6 px, so the cell's glyph baseline (TGLP 37) sits 0.8
/// base px above the line's.
const EX_TOP: f32 = 37.8;

pub struct FontAtlas {
    font: FontArc,
    ex: Option<ExFont>,
    pub tex: TexRef,
    glyphs: HashMap<(char, u32), Option<Glyph>>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

impl FontAtlas {
    pub fn new(r: &Renderer, data: Vec<u8>) -> anyhow::Result<Self> {
        let font = FontArc::try_from_vec(data)?;
        let tex = r.create_writable(ATLAS, ATLAS, true, "font atlas");
        Ok(Self {
            font,
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

    /// Quad for a placed char (a font glyph, or an exfont glyph stretched over a run) offset by (dx, dy).
    pub fn placed_quad(&mut self, r: &Renderer, pc: &PlacedChar, dx: f32, dy: f32, color: [f32; 4]) -> Option<Quad> {
        match pc.run {
            Some((n, spacing)) => self.run_quad(pc.c, (n, spacing), pc.size, (pc.x + dx, pc.baseline + dy), color),
            None => self.quad_stretched(r, pc.c, pc.size, (pc.x + dx, pc.baseline + dy), pc.stretch, color),
        }
    }

    /// The placed char as one white glyph over its black rim (`Shade::RimText`): the quad grows by the
    /// `rim` px the taps reach, each tap at `rim_alpha`, and `alpha` fades the composite once.
    pub fn rimmed_quad(&mut self, r: &Renderer, pc: &PlacedChar, rim: f32, rim_alpha: f32, alpha: f32) -> Option<Quad> {
        let q = self.placed_quad(r, pc, 0.0, 0.0, [1.0, 1.0, 1.0, alpha])?;
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
    /// ((W n + sp (n - 1)) sx / W) * 0.965 with W the pair font's scaled width (sub_13BCF0) and sp the
    /// character spacing,
    /// centred over the n cells the pen crosses (Switch: run centres at pen + n * 49.6 / 2).
    fn run_quad(&self, c: char, (n, spacing): (u8, f32), size: f32, (x, baseline): (f32, f32), color: [f32; 4]) -> Option<Quad> {
        let ex = self.ex.as_ref()?;
        let g = ex.font.glyph(c)?;
        let n = n as f32;
        let sx = size / BASE_SIZE;
        let w = PAIR_METRIC * sx;
        let stretch = (w * n + spacing * (n - 1.0)) * sx / w * 0.965;
        let qw = g.width as f32 * stretch;
        let cells = n * (size + spacing);
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

    pub fn advance(&self, c: char, size: f32) -> f32 {
        self.font.as_scaled(PxScale::from(size)).h_advance(self.font.glyph_id(c))
    }

    fn reset(&mut self) {
        self.glyphs.clear();
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_h = 0;
    }

    fn glyph(&mut self, r: &Renderer, c: char, size: f32) -> Option<Glyph> {
        let key = (c, (size * 4.0) as u32);
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let scale = PxScale::from(size);
        let id = self.font.glyph_id(c);
        let outlined = self.font.outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0)));
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
        let g = self.glyph(r, c, size)?;
        if g.rect[2] == 0 {
            return None;
        }
        let [gx, gy, gw, gh] = g.rect;
        let a = ATLAS as f32;
        let q = Quad::rect(
            self.tex.clone(),
            [x + g.off[0] * sx - (sx - 1.0) * size / 2.0, baseline + g.off[1], gw as f32 * sx, gh as f32],
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
    pub fn draw_str_slot(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: [f32; 2], spacing: f32, pos: [f32; 2], color: [f32; 4]) -> f32 {
        self.slot_line(r, out, s, size, spacing, pos, color, None)
    }

    /// `draw_str_slot` with the text engine's dark rim (`rimmed_quad`; the slots share sub_A7790 with the
    /// messages), its width scaled from the messages' 2 px at 45.6 px glyphs to `h`.
    pub fn draw_str_slot_rimmed(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: [f32; 2], spacing: f32, pos: [f32; 2], color: [f32; 4]) -> f32 {
        self.slot_line(r, out, s, size, spacing, pos, color, Some(2.0 * size[1] / (60.0 * 0.76)))
    }

    #[allow(clippy::too_many_arguments)]
    fn slot_line(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, [w, h]: [f32; 2], spacing: f32, [x, baseline]: [f32; 2], color: [f32; 4], rim: Option<f32>) -> f32 {
        let k = w / h;
        let mut pen = x;
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
                        Some(rim) => Self::rim(q, rim, 0.55),
                        None => q,
                    });
                }
                pen += n as f32 * (w + spacing);
                continue;
            }
            let (cell, sx) = slot_cell(c, w, k);
            let gx = pen + (cell - self.advance(c, h) * sx) / 2.0;
            // quad_stretched centres its stretch on the em; shift it back so the glyph starts at gx
            if let Some(q) = self.quad_stretched(r, c, h, (gx + (sx - 1.0) * h / 2.0, baseline), sx, color) {
                out.push(match rim {
                    Some(rim) => Self::rim(q, rim, 0.55),
                    None => q,
                });
            }
            pen += cell + slot_space(c, spacing);
        }
        pen - x
    }

    /// Width of a text slot line: the advances plus the spacing between glyphs.
    pub fn measure_slot(&self, s: &str, [w, h]: [f32; 2], spacing: f32) -> f32 {
        let chars: Vec<char> = slot_chars(s).collect();
        let last = chars.last().map_or(0.0, |&c| slot_space(c, spacing));
        chars.iter().map(|&c| slot_cell(c, w, w / h).0 + slot_space(c, spacing)).sum::<f32>() - last
    }
}

/// A text slot string as sub_AB4D0 hands it on: each double space becomes U+3000.
fn slot_chars(s: &str) -> impl Iterator<Item = char> + '_ {
    let mut it = s.chars().peekable();
    std::iter::from_fn(move || {
        let c = it.next()?;
        if c == ' ' && it.peek() == Some(&' ') {
            it.next();
            return Some('\u{3000}');
        }
        Some(c)
    })
}

/// The character space after `c`: halved inside sub_BDC2C's ASCII runs (tag 0x10, +100932 = 0.5).
fn slot_space(c: char, spacing: f32) -> f32 {
    if (' '..='~').contains(&c) { spacing * 0.5 } else { spacing }
}

/// A text slot glyph's fixed cell width and x scale: ASCII runs (tag 0x10) half the cell at 0.725 of the x scale.
fn slot_cell(c: char, w: f32, k: f32) -> (f32, f32) {
    if (' '..='~').contains(&c) { (w * 0.5, k * 0.725) } else { (w, k) }
}

/// One laid-out character of a page.
#[derive(Clone, Debug)]
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
}

/// Parse the message markup used in script_text: `<base|ruby>`, `@g` (style marker),
/// `^` (newline), `[ber00]`-style inline glyphs and `@k`/`@e`/`@n` control codes.
pub fn parse_markup(s: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
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
                for d in it.by_ref() {
                    if d == ']' {
                        break;
                    }
                    tag.push(d);
                }
                cur.push_str(match tag.as_str() {
                    t if t.starts_with("ber") => "♥",
                    t if t.starts_with("zap") => "―",
                    _ => "",
                });
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

/// Ruby scale (dword_3C9430[language], 0.52 for Japanese) of the text's glyph scale and spacing, and the
/// ruby cursor's extra y (slot +101748 = -2 * H / 1080 from the slot init's +100924).
const RUBY_SCALE: f32 = 0.52;
const RUBY_Y: f32 = -2.0;
/// The pair font's baseline position (vtable +192: the taller font's, the system shared font's ScalableFont
/// +36 = round(metric * 40), not in the romfs) at the base size. Switch (Astris, train page まえぶ): the ruby
/// ink centroid 45.37 px above the base's baseline offset, so 1.14 * B - 2 = 45.37 and B = 41.6, a whole 42.
const PAIR_BASELINE: f32 = 42.0;

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
        // nn::font wraps when the glyph would end past the slot width (464 units): 28 full-width glyphs a line;
        // kinsoku chars never start a line, so they overhang the right edge
        if *x + width > lay.right && !no_line_start(c) {
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
        });
        // the run is one character to the reveal
        *order += 1;
        *x += width + lay.spacing;
    };
    let push_char = |c: char, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>| push(c, None, x, baseline, order, out);
    for run in runs {
        match run {
            Run::Newline => {
                x = lay.left;
                baseline += lay.line_height;
            }
            Run::Text(t) => {
                let chars: Vec<char> = t.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    let c = chars[i];
                    let n = chars[i..].iter().take_while(|&&d| run_kind(d) == run_kind(c)).count().min(255);
                    match run_kind(c) {
                        // runs of dashes (any length) and of two or more boxes (sub_C1080)
                        Some(g) if atlas.has_ex(g) && (g == EX_DASH || n >= 2) => {
                            push(c, Some((g, n as u8)), &mut x, &mut baseline, &mut order, out);
                            i += n;
                        }
                        _ => {
                            push_char(c, &mut x, &mut baseline, &mut order, out);
                            i += 1;
                        }
                    }
                }
            }
            Run::Ruby { base, ruby } => {
                let bw: f32 = base.chars().map(|c| atlas.advance(c, lay.size) + lay.spacing).sum();
                if x + bw - lay.spacing > lay.right {
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
                // spacing. Its cursor y is the base writer's (on the baseline) less the scaled pair-font baseline
                // position and +101748 (-2 px).
                let (rs, rsp) = (lay.size * RUBY_SCALE, lay.spacing * RUBY_SCALE);
                let bw = bw - lay.spacing;
                let rw: f32 = ruby.chars().map(|c| atlas.advance(c, rs) + rsp).sum::<f32>() - rsp;
                let n = ruby.chars().count().max(1) as f32;
                let mut rx = start_x + (bw - rw) / 2.0;
                let rb = baseline - PAIR_BASELINE * lay.size / BASE_SIZE - RUBY_Y;
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
                    });
                    rx += atlas.advance(c, rs) + rsp;
                }
            }
        }
    }
    (x, baseline, order)
}
