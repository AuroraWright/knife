//! Font rasterisation into an R8 atlas, and NVL page layout.

use std::collections::HashMap;

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use knife_render::{Blend, Quad, Renderer, Shade, TexRef};

const ATLAS: u32 = 2048;
const PAD: u32 = 2;

#[derive(Clone, Copy)]
struct Glyph {
    /// atlas rect in pixels
    rect: [u32; 4],
    /// offset of the bitmap's top-left from the pen position on the baseline
    off: [f32; 2],
    advance: f32,
}

pub struct FontAtlas {
    font: FontArc,
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
            tex,
            glyphs: HashMap::new(),
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
        })
    }

    pub fn advance(&self, c: char, size: f32) -> f32 {
        self.font.as_scaled(PxScale::from(size)).h_advance(self.font.glyph_id(c))
    }

    pub fn ascent(&self, size: f32) -> f32 {
        self.font.as_scaled(PxScale::from(size)).ascent()
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
        let sf = self.font.as_scaled(scale);
        let id = self.font.glyph_id(c);
        let advance = sf.h_advance(id);
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
            r.write_region(&self.tex, x, y, w, h, 1, &buf);
            Glyph {
                rect: [x, y, w, h],
                off: [b.min.x, b.min.y],
                advance,
            }
        });
        let g = g.or(Some(Glyph {
            rect: [0, 0, 0, 0],
            off: [0.0, 0.0],
            advance,
        }));
        self.glyphs.insert(key, g);
        g
    }

    /// Quad for one glyph with its pen at (x, baseline).
    pub fn quad(&mut self, r: &Renderer, c: char, size: f32, x: f32, baseline: f32, color: [f32; 4]) -> Option<Quad> {
        let g = self.glyph(r, c, size)?;
        if g.rect[2] == 0 {
            return None;
        }
        let [gx, gy, gw, gh] = g.rect;
        let a = ATLAS as f32;
        let q = Quad::rect(
            self.tex.clone(),
            [x + g.off[0], baseline + g.off[1], gw as f32, gh as f32],
            color,
        )
        .with_uv_rect(gx as f32 / a, gy as f32 / a, (gx + gw) as f32 / a, (gy + gh) as f32 / a);
        Some(Quad {
            blend: Blend::Alpha,
            shade: Shade::Text,
            ..q
        })
    }

    /// Draw a string left-aligned; returns the advance width.
    pub fn draw_str(&mut self, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: f32, x: f32, baseline: f32, color: [f32; 4]) -> f32 {
        let mut pen = x;
        for c in s.chars() {
            if let Some(q) = self.quad(r, c, size, pen, baseline, color) {
                out.push(q);
            }
            pen += self.advance(c, size);
        }
        pen - x
    }

    pub fn measure(&self, s: &str, size: f32) -> f32 {
        s.chars().map(|c| self.advance(c, size)).sum()
    }
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

/// Characters that must not start a line (kinsoku).
fn no_line_start(c: char) -> bool {
    "、。，．・？！゛゜ヽヾゝゞ々ー）］｝」』!),.:;?]}…―ぁぃぅぇぉっゃゅょァィゥェォッャュョ〟’”".contains(c)
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub left: f32,
    pub right: f32,
    pub size: f32,
    pub line_height: f32,
    pub ruby_size: f32,
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
    let push_char = |c: char, x: &mut f32, baseline: &mut f32, order: &mut usize, out: &mut Vec<PlacedChar>, next: Option<char>| {
        let adv = atlas.advance(c, lay.size);
        // sub_391A0 breaks the line once the pen has reached the width (pen - left >= 464 units), so the last
        // glyph may overhang the right edge; kinsoku chars never start a line
        if *x >= lay.right && !no_line_start(c) {
            *x = lay.left;
            *baseline += lay.line_height;
        }
        let _ = next;
        out.push(PlacedChar {
            c,
            x: *x,
            baseline: *baseline,
            size: lay.size,
            order: *order,
        });
        *order += 1;
        *x += adv;
    };
    for run in runs {
        match run {
            Run::Newline => {
                x = lay.left;
                baseline += lay.line_height;
            }
            Run::Text(t) => {
                let chars: Vec<char> = t.chars().collect();
                for (i, &c) in chars.iter().enumerate() {
                    push_char(c, &mut x, &mut baseline, &mut order, out, chars.get(i + 1).copied());
                }
            }
            Run::Ruby { base, ruby } => {
                let bw: f32 = base.chars().map(|c| atlas.advance(c, lay.size)).sum();
                if x + bw - atlas.advance(base.chars().last().unwrap_or(' '), lay.size) >= lay.right {
                    x = lay.left;
                    baseline += lay.line_height;
                }
                let start_x = x;
                let first = order;
                for c in base.chars() {
                    push_char(c, &mut x, &mut baseline, &mut order, out, None);
                }
                let rw: f32 = ruby.chars().map(|c| atlas.advance(c, lay.ruby_size)).sum();
                let n = ruby.chars().count().max(1) as f32;
                // spread ruby over the base when shorter, centre otherwise
                let (mut rx, gap) = if rw < bw {
                    let gap = (bw - rw) / n;
                    (start_x + gap / 2.0, gap)
                } else {
                    (start_x + (bw - rw) / 2.0, 0.0)
                };
                let rb = baseline - lay.size * 0.95;
                let span = (order - first).max(1);
                for (k, c) in ruby.chars().enumerate() {
                    out.push(PlacedChar {
                        c,
                        x: rx,
                        baseline: rb,
                        size: lay.ruby_size,
                        order: first + (k * span / n as usize).min(span - 1),
                    });
                    rx += atlas.advance(c, lay.ruby_size) + gap;
                }
            }
        }
    }
    (x, baseline, order)
}
