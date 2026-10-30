//! BFFNT bitmap fonts (nn::font, version 4). The game has one, `exfont_nx01.bffnt`: 20 glyphs the text
//! engine's tag pass puts in place of runs of dashes and boxes (sub_C1080) and a few Latin-1 symbols.

use anyhow::{Context, Result, bail};

use super::bntx;

/// One glyph's cell in the sheet and its CWDH widths (pixels at the font's native size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// the cell's top-left corner in the sheet flipped upright (see `Font::sheet`)
    pub x: u32,
    pub y: u32,
    /// CWDH: left bearing, ink width and advance
    pub left: i8,
    pub width: u8,
    pub advance: u8,
}

#[derive(Debug, Clone)]
pub struct Font {
    /// FINF: line height, ascent, the default advance and the line feed
    pub height: u8,
    pub ascent: u8,
    pub width: u8,
    pub line_feed: u16,
    /// TGLP: cell size and the baseline from the cell top
    pub cell_w: u8,
    pub cell_h: u8,
    pub baseline: u16,
    /// the glyph sheet (one BNTX texture, single channel coverage), stored bottom-up: decoded, its rows run
    /// from the bottom, so cell (0, 0) is in the bottom-left corner and the glyphs are upside down
    pub sheet: bntx::Texture,
    /// FINF's alternate glyph (+6), which nn::font draws for a character no font of the pair has
    pub alt: Option<Glyph>,
    /// (code point, glyph)
    pub glyphs: Vec<(u32, Glyph)>,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(d.get(o..o + 2).context("bffnt truncated")?.try_into()?))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("bffnt truncated")?.try_into()?))
}

impl Font {
    pub fn glyph(&self, c: char) -> Option<Glyph> {
        self.glyphs.iter().find(|g| g.0 == c as u32).map(|g| g.1)
    }

    pub fn parse(d: &[u8]) -> Result<Self> {
        if d.get(..4) != Some(b"FFNT") || d.get(4..6) != Some(&[0xff, 0xfe]) {
            bail!("not a little-endian BFFNT");
        }
        let header = u16_at(d, 6)? as usize;
        if d.get(header..header + 4) != Some(b"FINF") {
            bail!("BFFNT: no FINF");
        }
        let f = header + 8;
        let (height, width, ascent) = (d[f + 1], d[f + 2], d[f + 3]);
        let line_feed = u16_at(d, f + 4)?;
        // section offsets point past each section's 8-byte header
        let tglp = u32_at(d, f + 12)? as usize;
        let mut cwdh = u32_at(d, f + 16)? as usize;
        let mut cmap = u32_at(d, f + 20)? as usize;

        let (cell_w, cell_h, sheets) = (d[tglp], d[tglp + 1], d[tglp + 2]);
        let baseline = u16_at(d, tglp + 8)?;
        let cols = u16_at(d, tglp + 12)? as u32;
        let sheet_at = u32_at(d, tglp + 20)? as usize;
        if sheets != 1 {
            bail!("BFFNT: {sheets} sheets");
        }
        let sheet = bntx::parse(&d[sheet_at..])?.into_iter().next().context("BFFNT: empty sheet")?;

        // CWDH blocks: first index, last index, next block, then (left, width, advance) per glyph
        let mut widths = Vec::new();
        while cwdh != 0 {
            let (first, last) = (u16_at(d, cwdh)? as usize, u16_at(d, cwdh + 2)? as usize);
            widths.resize(widths.len().max(last + 1), (0i8, 0u8, 0u8));
            for (k, w) in widths[first..=last].iter_mut().enumerate() {
                let o = cwdh + 8 + 3 * k;
                *w = (d[o] as i8, d[o + 1], d[o + 2]);
            }
            cwdh = u32_at(d, cwdh + 4)? as usize;
        }

        let at = |index: usize| {
            let (col, row) = (index as u32 % cols, index as u32 / cols);
            let (left, width, advance) = widths.get(index).copied().unwrap_or((0, cell_w, cell_w));
            Glyph { x: col * (cell_w as u32 + 1) + 1, y: row * (cell_h as u32 + 1) + 1, left, width, advance }
        };
        let alt = Some(u16_at(d, f + 6)? as usize).filter(|&i| i != 0xffff).map(at);
        let mut glyphs = Vec::new();
        let mut put = |code: u32, index: usize| {
            if index != 0xffff {
                glyphs.push((code, at(index)));
            }
        };
        // CMAP blocks: first code, last code, method (0 direct, 1 table, 2 scan), next block
        while cmap != 0 {
            let (first, last) = (u32_at(d, cmap)?, u32_at(d, cmap + 4)?);
            let method = u16_at(d, cmap + 8)?;
            let p = cmap + 16;
            match method {
                0 => {
                    let base = u16_at(d, p)? as usize;
                    for c in first..=last {
                        put(c, base + (c - first) as usize);
                    }
                }
                1 => {
                    for c in first..=last {
                        put(c, u16_at(d, p + 2 * (c - first) as usize)? as usize);
                    }
                }
                _ => {
                    for i in 0..u32_at(d, p)? as usize {
                        put(u32_at(d, p + 4 + 8 * i)?, u16_at(d, p + 8 + 8 * i)? as usize);
                    }
                }
            }
            cmap = u32_at(d, cmap + 12)? as usize;
        }
        Ok(Self { height, ascent, width, line_feed, cell_w, cell_h, baseline, sheet, alt, glyphs })
    }
}
