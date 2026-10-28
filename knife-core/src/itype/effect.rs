//! iType's graymap effects as the glyph caches use them: the outline effect (outline_graymap: the glyph grown by
//! the outline width, optionally less the glyph itself, scaled by the opacity) and the trim of blank rows and
//! columns every graymap gets last (trim_convert).

use super::raster::Graymap;

/// Grow the glyph by `ow` pixels into a map padded by ow + 1: each pass sets a pixel that has any coverage to full
/// and an empty one to the most of its neighbours, the 3x3 square and the plus shape in turn, ending on the plus.
/// `hollow` (the outline-only flags) then takes the glyph back out, and an `opacity` below 1.0 (16.16) scales it.
pub fn outline(map: &Graymap, ow: u16, opacity: i32, hollow: bool) -> Graymap {
    if map.width == 0 || map.height == 0 {
        return map.clone();
    }
    let pad = usize::from(ow) + 1;
    let (w, h) = (map.width as usize, map.height as usize);
    let (ww, hh) = (w + 2 * pad, h + 2 * pad);
    let mut buf = vec![0u8; ww * hh];
    for r in 0..h {
        buf[(r + pad) * ww + pad..][..w].copy_from_slice(&map.bits[r * w..][..w]);
    }
    for pass in (1..=ow).rev() {
        let src = buf.clone();
        for r in 1..hh - 1 {
            for c in 1..ww - 1 {
                let i = r * ww + c;
                buf[i] = if src[i] != 0 {
                    0xFF
                } else {
                    let plus = src[i - 1].max(src[i + 1]).max(src[i - ww]).max(src[i + ww]);
                    if pass & 1 != 0 {
                        plus
                    } else {
                        plus.max(src[i - ww - 1]).max(src[i - ww + 1]).max(src[i + ww - 1]).max(src[i + ww + 1])
                    }
                };
            }
        }
    }
    if hollow {
        for r in 0..h {
            for c in 0..w {
                let i = (r + pad) * ww + pad + c;
                buf[i] = buf[i].wrapping_sub(map.bits[r * w + c]);
            }
        }
    }
    if opacity < 0x10000 {
        for v in &mut buf {
            *v = ((u32::from(*v) * opacity as u32) >> 16) as u8;
        }
    }
    Graymap {
        lo_x: map.lo_x - pad as i16,
        hi_y: map.hi_y + pad as i16,
        width: ww as i16,
        height: hh as i16,
        bits: buf,
    }
}

/// Drop the rows above and below and the columns either side that have no coverage; a blank map keeps none, its
/// origin moved past them all.
pub fn trim(map: Graymap) -> Graymap {
    let (w, h) = (map.width as usize, map.height as usize);
    let row = |r: usize| map.bits[r * w..][..w].iter().any(|&v| v != 0);
    let top = (0..h).find(|&r| row(r)).unwrap_or(h);
    let bottom = (top..h).rev().find(|&r| row(r)).map_or(top, |r| r + 1);
    let col = |c: usize| (top..bottom).any(|r| map.bits[r * w + c] != 0);
    let left = (0..w).find(|&c| col(c)).unwrap_or(w);
    let right = (0..w).rev().find(|&c| col(c)).map_or(0, |c| c + 1);
    let width = right.saturating_sub(left);
    if top == 0 && width == w && bottom == h {
        return map;
    }
    let mut bits = Vec::with_capacity(width * (bottom - top));
    for r in top..bottom {
        bits.extend_from_slice(&map.bits[r * w + left..][..width]);
    }
    Graymap {
        lo_x: map.lo_x + left as i16,
        hi_y: map.hi_y - top as i16,
        width: width as i16,
        height: (bottom - top) as i16,
        bits,
    }
}
