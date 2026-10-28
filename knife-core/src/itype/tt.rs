//! TrueType glyph loading and scaling as iType does it for a glyph cache (fsg_GridFit / get_comp): the glyph's
//! points in font units plus the eight phantom points, scaled to 26.6 pixels by the font's exact ratio
//! (fs_NewTransformation, sub_189FD0), then the phantom points put on the grid (sub_18B560) before hinting.

use super::fixed::muldiv;

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn i16_at(d: &[u8], at: usize) -> Option<i16> {
    u16_at(d, at).map(|v| v as i16)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The tables a TrueType outline needs, or an OpenType font's CFF outlines.
pub struct Font<'a> {
    pub data: &'a [u8],
    pub upem: u16,
    pub num_glyphs: u16,
    long_loca: bool,
    loca: usize,
    glyf: usize,
    /// the `CFF ` table's glyphs, for a font without glyf
    pub cff: Option<super::cff::Cff<'a>>,
    hmtx: usize,
    num_hmetrics: u16,
    /// vmtx and its metric count, else the advance height is the ascender less the descender
    vmtx: Option<(usize, u16)>,
    ascent: i16,
    descent: i16,
    pub cmap: Option<usize>,
}

/// One glyph's points (font units in `oox` / `ooy`, 26.6 pixels in the rest), the last eight the phantom points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub x: Vec<i32>,
    pub y: Vec<i32>,
    pub ox: Vec<i32>,
    pub oy: Vec<i32>,
    pub oox: Vec<i32>,
    pub ooy: Vec<i32>,
    pub on: Vec<u8>,
    pub f: Vec<u8>,
    pub sp: Vec<u16>,
    pub ep: Vec<u16>,
}

impl Element {
    /// The glyph's own points, without the phantom ones.
    pub fn points(&self) -> usize {
        self.x.len() - 8
    }
}

/// A scale from font units to 26.6 pixels (fs_NewTransformation): the ppem over the units per em, both 16.16
/// and with their common trailing zero bits dropped; a ratio of 0x8000 or more multiplies by its 16.16 quotient,
/// a power-of-two divisor shifts, any other divides.
#[derive(Clone, Copy, Debug)]
pub struct Scale {
    pub ppem: i32,
    pub mul: i32,
    pub div: i32,
    fixed: i32,
    shift: u32,
    mode: u8,
}

impl Scale {
    pub fn new(ppem: i32, upem: u16) -> Scale {
        let den = i32::from(upem) << 16;
        let both = den | ppem;
        let tz = if both == 0 { 31 } else { both.trailing_zeros() };
        let (mut mul, mut div) = (ppem >> tz, den >> tz);
        if mul >= 0x200_0000 {
            div >>= 6;
        } else {
            mul <<= 6;
        }
        let mut s = Scale { ppem, mul, div, fixed: 0, shift: 0, mode: 1 };
        if mul >= 0x8000 {
            s.fixed = super::fixed::vardiv(mul, div, 16);
            s.mode = 2;
        } else if div.count_ones() == 1 {
            s.shift = div.trailing_zeros();
            s.mode = 0;
        }
        s
    }

    /// Font units to 26.6, rounded half away from zero (half up for a shift).
    pub fn apply(&self, v: i32) -> i32 {
        let half = self.div >> 1;
        match self.mode {
            0 => (half + v.wrapping_mul(self.mul)) >> self.shift,
            1 if v >= 0 => (v * self.mul + half) / self.div,
            1 => -((half - v * self.mul) / self.div),
            _ => super::fixed::varmul(self.fixed, v, 16),
        }
    }
}

impl<'a> Font<'a> {
    pub fn new(data: &'a [u8]) -> Option<Font<'a>> {
        let tables = u16_at(data, 4)?;
        let find = |tag: &[u8; 4]| -> Option<usize> {
            (0..usize::from(tables))
                .map(|i| 12 + 16 * i)
                .find(|&r| data.get(r..r + 4) == Some(tag))
                .and_then(|r| u32_at(data, r + 8))
                .map(|o| o as usize)
        };
        let head = find(b"head")?;
        let hhea = find(b"hhea")?;
        let maxp = find(b"maxp")?;
        let vmtx = match (find(b"vhea"), find(b"vmtx")) {
            (Some(vhea), Some(vmtx)) => Some((vmtx, u16_at(data, vhea + 34)?)),
            _ => None,
        };
        let os2 = find(b"OS/2");
        let (ascent, descent) = match os2 {
            Some(o) => (i16_at(data, o + 68)?, i16_at(data, o + 70)?),
            None => (i16_at(data, hhea + 4)?, i16_at(data, hhea + 6)?),
        };
        let (loca, glyf, cff) = match (find(b"loca"), find(b"glyf")) {
            (Some(loca), Some(glyf)) => (loca, glyf, None),
            _ => (0, 0, Some(super::cff::Cff::new(data.get(find(b"CFF ")?..)?)?)),
        };
        Some(Font {
            data,
            upem: u16_at(data, head + 18)?,
            num_glyphs: u16_at(data, maxp + 4)?,
            long_loca: i16_at(data, head + 50)? != 0,
            loca,
            glyf,
            cff,
            hmtx: find(b"hmtx")?,
            num_hmetrics: u16_at(data, hhea + 34)?,
            vmtx,
            ascent,
            descent,
            cmap: find(b"cmap"),
        })
    }

    /// The offset of the cmap subtable for a platform and encoding.
    pub fn subtable(&self, platform: u16, encoding: u16) -> Option<usize> {
        let (d, c) = (self.data, self.cmap?);
        (0..usize::from(u16_at(d, c + 2)?))
            .map(|i| c + 4 + 8 * i)
            .find(|&r| u16_at(d, r) == Some(platform) && u16_at(d, r + 2) == Some(encoding))
            .and_then(|r| u32_at(d, r + 4))
            .map(|o| c + o as usize)
    }

    /// The Unicode cmap the SDK picks (FSS_set_cmap 3,1 else 3,10).
    fn unicode(&self) -> Option<usize> {
        self.subtable(3, 1).or_else(|| self.subtable(3, 10))
    }

    /// A character's glyph (0 when unmapped), from a format 4 or 12 Unicode cmap.
    pub fn glyph(&self, ch: u32) -> u16 {
        self.unicode().and_then(|t| self.lookup(t, ch)).unwrap_or(0)
    }

    fn lookup(&self, t: usize, ch: u32) -> Option<u16> {
        let d = self.data;
        match u16_at(d, t)? {
            4 => {
                let ch = u16::try_from(ch).ok()?;
                let n = usize::from(u16_at(d, t + 6)? / 2);
                let (ends, starts) = (t + 14, t + 16 + 2 * n);
                let (deltas, ranges) = (starts + 2 * n, starts + 4 * n);
                let i = (0..n).find(|&i| u16_at(d, ends + 2 * i).is_some_and(|e| e >= ch))?;
                let start = u16_at(d, starts + 2 * i)?;
                if ch < start {
                    return None;
                }
                let delta = u16_at(d, deltas + 2 * i)?;
                let range = u16_at(d, ranges + 2 * i)?;
                if range == 0 {
                    return Some(ch.wrapping_add(delta));
                }
                let g = u16_at(d, ranges + 2 * i + usize::from(range) + 2 * usize::from(ch - start))?;
                Some(if g == 0 { 0 } else { g.wrapping_add(delta) })
            }
            12 => {
                let n = u32_at(d, t + 12)? as usize;
                (0..n).map(|i| t + 16 + 12 * i).find_map(|g| {
                    let (a, b, first) = (u32_at(d, g)?, u32_at(d, g + 4)?, u32_at(d, g + 8)?);
                    (a <= ch && ch <= b).then(|| (first + ch - a) as u16)
                })
            }
            _ => None,
        }
    }

    /// The character a glyph is taken for (inverse_map_char): in a format 4 map the first segment, in code
    /// order, whose delta range holds the glyph (any segment with a delta) or whose glyph array lists it raw
    /// (any without); in a format 12 map the first group from where the SDK's search lands (it compares the
    /// groups' last characters with the glyph) whose glyphs hold it. 0xFFFF when none, and for glyph 0 (the SDK
    /// takes only glyphs past the font's first).
    pub fn inverse(&self, gid: u16) -> u32 {
        let found = self.unicode().and_then(|t| {
            if gid == 0 {
                return None;
            }
            let d = self.data;
            match u16_at(d, t)? {
                4 => {
                    let n = usize::from(u16_at(d, t + 6)? / 2);
                    let (ends, starts) = (t + 14, t + 16 + 2 * n);
                    let (deltas, ranges) = (starts + 2 * n, starts + 4 * n);
                    for i in 0..n {
                        let (start, end) = (u16_at(d, starts + 2 * i)?, u16_at(d, ends + 2 * i)?);
                        let delta = u16_at(d, deltas + 2 * i)?;
                        if delta != 0 {
                            if gid >= start.wrapping_add(delta) && gid <= end.wrapping_add(delta) {
                                return Some(u32::from(gid.wrapping_sub(delta)));
                            }
                        } else if start <= end {
                            let at = ranges + 2 * i + usize::from(u16_at(d, ranges + 2 * i)? & !1);
                            for (k, code) in (start..=end).enumerate() {
                                if u16_at(d, at + 2 * k)? == gid {
                                    return Some(u32::from(code));
                                }
                            }
                        }
                    }
                    None
                }
                12 => {
                    let n = u32_at(d, t + 12)? as usize;
                    let g = u32::from(gid);
                    let last = |k: usize| u32_at(d, t + 20 + 12 * k);
                    let top = if n >= 2 { 1usize << n.ilog2() } else { 0 };
                    let probe = top >> usize::from(n == top);
                    let mut k = if last(probe)? >= g { 0 } else { n - 1 - probe };
                    let mut step = probe >> 1;
                    if step.is_power_of_two() && (2..=0x4000).contains(&step) {
                        while step >= 2 {
                            if last(k + step - 1)? < g {
                                k += step;
                            }
                            step >>= 1;
                        }
                    }
                    (k..n).map(|i| t + 16 + 12 * i).find_map(|r| {
                        let (a, b, first) = (u32_at(d, r)?, u32_at(d, r + 4)?, u32_at(d, r + 8)?);
                        (first <= g && first.wrapping_sub(a).wrapping_add(b) >= g).then(|| a + g - first)
                    })
                }
                _ => None,
            }
        });
        match found {
            Some(c) if c != 0xFFFF => c,
            _ => 0xFFFF,
        }
    }

    fn os2(&self) -> Option<usize> {
        let d = self.data;
        (0..usize::from(u16_at(d, 4)?))
            .map(|i| 12 + 16 * i)
            .find(|&r| d.get(r..r + 4) == Some(b"OS/2"))
            .and_then(|r| u32_at(d, r + 8))
            .map(|o| o as usize)
    }

    /// The OS/2 Unicode range bits (ulUnicodeRange1..3), all zero without OS/2.
    pub fn ranges(&self) -> [u32; 3] {
        self.os2().map_or([0; 3], |o| [42, 46, 50].map(|at| u32_at(self.data, o + at).unwrap_or(0)))
    }

    /// The PANOSE family kind (OS/2 panose[0]); None without OS/2.
    pub fn panose_family(&self) -> Option<u8> {
        self.data.get(self.os2()? + 32).copied()
    }

    /// The glyph's bytes in glyf (empty for a glyph without an outline).
    pub fn glyph_data(&self, gid: u16) -> &'a [u8] {
        if self.cff.is_some() {
            return &[];
        }
        let gid = if gid >= self.num_glyphs { 0 } else { usize::from(gid) };
        let (a, b) = if self.long_loca {
            (u32_at(self.data, self.loca + 4 * gid), u32_at(self.data, self.loca + 4 * gid + 4))
        } else {
            (
                u16_at(self.data, self.loca + 2 * gid).map(|v| u32::from(v) * 2),
                u16_at(self.data, self.loca + 2 * gid + 2).map(|v| u32::from(v) * 2),
            )
        };
        match (a, b) {
            (Some(a), Some(b)) if b > a => {
                self.data.get(self.glyf + a as usize..self.glyf + b as usize).unwrap_or(&[])
            }
            _ => &[],
        }
    }

    /// Left side bearing, advance width, top side bearing and advance height (get_glyph_metrics).
    pub fn metrics(&self, gid: u16) -> (i16, u16, i16, u16) {
        let d = self.data;
        let n = self.num_hmetrics;
        let (lsb, adv) = if gid < n {
            let at = self.hmtx + 4 * usize::from(gid);
            (i16_at(d, at + 2), u16_at(d, at))
        } else {
            let last = self.hmtx + 4 * usize::from(n) - 4;
            (i16_at(d, last + 4 + 2 * usize::from(gid - n)), u16_at(d, last))
        };
        let (tsb, adv_h) = match self.vmtx {
            Some((vmtx, n)) if gid < n => {
                let at = vmtx + 4 * usize::from(gid);
                (i16_at(d, at + 2), u16_at(d, at))
            }
            Some((vmtx, n)) => {
                let last = vmtx + 4 * usize::from(n) - 4;
                (i16_at(d, last + 4 + 2 * usize::from(gid - n)), u16_at(d, last))
            }
            None => (Some(0), Some((self.ascent - self.descent) as u16)),
        };
        (lsb.unwrap_or(0), adv.unwrap_or(0), tsb.unwrap_or(0), adv_h.unwrap_or(0))
    }

    /// A simple glyph's points in font units with its phantom points (sfnt_UnfoldCurve, get_comp), and its
    /// instructions; None for a composite glyph.
    pub fn load(&self, gid: u16) -> Option<(Element, &'a [u8])> {
        let g = self.glyph_data(gid);
        let mut e = Element::default();
        let mut code: &[u8] = &[];
        let (mut x_min, mut y_max) = (0, 0);
        if g.len() >= 10 {
            let nc = i16_at(g, 0)?;
            if nc < 0 {
                return None;
            }
            (x_min, y_max) = (i32::from(i16_at(g, 2)?), i32::from(i16_at(g, 8)?));
            let mut p = 10;
            for c in 0..nc as usize {
                e.sp.push(if c == 0 { 0 } else { e.ep[c - 1] + 1 });
                e.ep.push(u16_at(g, p)?);
                p += 2;
            }
            if nc > 0 {
                let n = usize::from(e.ep[e.ep.len() - 1]) + 1;
                let len = usize::from(u16_at(g, p)?);
                code = g.get(p + 2..p + 2 + len)?;
                p += 2 + len;
                let mut flags = Vec::with_capacity(n);
                while flags.len() < n {
                    let f = *g.get(p)?;
                    p += 1;
                    flags.push(f);
                    if f & 8 != 0 {
                        let r = *g.get(p)?;
                        p += 1;
                        flags.extend(std::iter::repeat_n(f, usize::from(r)));
                    }
                }
                flags.truncate(n);
                for (out, short, same) in [(&mut e.oox, 2, 0x10), (&mut e.ooy, 4, 0x20)] {
                    let mut v = 0i32;
                    for &f in &flags {
                        if f & short != 0 {
                            let d = i32::from(*g.get(p)?);
                            p += 1;
                            v += if f & same != 0 { d } else { -d };
                        } else if f & same == 0 {
                            v += i32::from(i16_at(g, p)?);
                            p += 2;
                        }
                        out.push(v);
                    }
                }
                e.on = flags.iter().map(|f| f & 1).collect();
            }
        }
        self.push_phantoms(&mut e, gid, x_min, y_max);
        Some((e, code))
    }

    /// The eight phantom points (font units) after a glyph's own, from its metrics and the left and top of its
    /// box (get_comp; comp_phantom_points for a composite glyph).
    pub fn push_phantoms(&self, e: &mut Element, gid: u16, x_min: i32, y_max: i32) {
        let (lsb, adv, _tsb, adv_h) = self.metrics(gid);
        let left = x_min - i32::from(lsb);
        let right = left + i32::from(adv);
        let mid = (left + right) / 2;
        let h = i32::from(adv_h);
        e.oox.extend([left, right, mid, mid, left, x_min, mid, mid]);
        e.ooy.extend([0, 0, h, 2 * h, 0, 0, h, y_max]);
        let n = e.oox.len();
        e.on.resize(n, 0);
        e.f = vec![0; n];
    }

    /// A composite glyph (number of contours -1, get_comp): the left and top of its box, its component records
    /// (read_comp_parms_ttf) and the instructions after them; None for any other glyph.
    pub fn composite(&self, gid: u16) -> Option<Composite<'a>> {
        let g = self.glyph_data(gid);
        if g.len() < 10 || i16_at(g, 0)? != -1 {
            return None;
        }
        let (x_min, y_max) = (i32::from(i16_at(g, 2)?), i32::from(i16_at(g, 8)?));
        let mut p = 10;
        let mut components = Vec::new();
        let mut instructions = false;
        loop {
            let (flags, gid) = (u16_at(g, p)?, u16_at(g, p + 2)?);
            // words with ARG_1_AND_2_ARE_WORDS, else bytes, either read signed
            let args = if flags & 1 != 0 {
                p += 8;
                (i32::from(i16_at(g, p - 4)?), i32::from(i16_at(g, p - 2)?))
            } else {
                p += 6;
                (i32::from(*g.get(p - 2)? as i8), i32::from(*g.get(p - 1)? as i8))
            };
            // 2.14 to 16.16
            let f2 = |at: usize| i16_at(g, at).map(|v| 4 * i32::from(v));
            let scale = if flags & 8 != 0 {
                p += 2;
                let s = f2(p - 2)?;
                Some([s, 0, 0, s])
            } else if flags & 0x40 != 0 {
                p += 4;
                Some([f2(p - 4)?, 0, 0, f2(p - 2)?])
            } else if flags & 0x80 != 0 {
                p += 8;
                Some([f2(p - 8)?, f2(p - 6)?, f2(p - 4)?, f2(p - 2)?])
            } else {
                None
            };
            instructions |= flags & 0x100 != 0;
            components.push(Component { flags, gid, args, scale });
            if flags & 0x20 == 0 {
                break;
            }
        }
        let code = match instructions {
            true => {
                let len = usize::from(u16_at(g, p)?);
                g.get(p + 2..).map_or(&[][..], |c| &c[..len.min(c.len())])
            }
            false => &[],
        };
        Some(Composite { x_min, y_max, components, instructions, code })
    }
}

/// A composite glyph's component (read_comp_parms_ttf).
pub struct Component {
    pub flags: u16,
    pub gid: u16,
    /// offsets in font units (ARGS_ARE_XY_VALUES) or point numbers
    pub args: (i32, i32),
    /// the scale as a 16.16 matrix (xx, xy, yx, yy), when the record has one
    pub scale: Option<[i32; 4]>,
}

/// A composite glyph's records.
pub struct Composite<'a> {
    pub x_min: i32,
    pub y_max: i32,
    pub components: Vec<Component>,
    /// WE_HAVE_INSTRUCTIONS on any record
    pub instructions: bool,
    pub code: &'a [u8],
}

/// Scale the font-unit points to 26.6 (sub_189FD0), the scaled ones also the originals.
pub fn scale(e: &mut Element, s: &Scale) {
    e.x = e.oox.iter().map(|&v| s.apply(v)).collect();
    e.y = e.ooy.iter().map(|&v| s.apply(v)).collect();
    e.ox = e.x.clone();
    e.oy = e.y.clone();
}

/// Put the phantom points on the grid (sub_18B560 for a simple glyph): the vertical origin, the left side
/// bearing point with the whole glyph moved by its rounding, and the advances, rounded too when hinting.
pub fn round_phantoms(e: &mut Element, s: &Scale, upem: u16, hinting: bool) {
    let n = e.points();
    let round = |v: i32| (v + 32) & !63;
    e.x[n + 2] = round(e.x[n + 2]);
    e.x[n + 3] = e.x[n + 2];
    e.y[n + 2] = round(e.y[n + 2]);
    let mut adv = (muldiv(s.ppem, e.ooy[n + 3] - e.ooy[n + 2], i32::from(upem)) + 512) >> 10;
    if hinting {
        adv = round(adv);
    }
    e.y[n + 3] = adv + e.y[n + 2];
    let left = round(e.x[n]);
    let d = left - e.x[n];
    e.x[n] = left;
    if d != 0 {
        for v in &mut e.x[..n] {
            *v += d;
        }
    }
    let mut adv = (muldiv(s.ppem, e.oox[n + 1] - e.oox[n], i32::from(upem)) + 512) >> 10;
    if hinting {
        adv = round(adv);
    }
    e.x[n + 1] = adv + left;
}

/// Interpolate the untouched points of each contour along one axis (fnt_IUP): every run of them between two
/// touched points keeps its place relative to theirs, interpolated by the font units between them when its
/// original place lies between theirs, else shifted as the nearer one was; a contour with one touched point,
/// or two at the same place, shifts with it.
pub fn iup(e: &mut Element, x_axis: bool) {
    let flag = if x_axis { 1 } else { 2 };
    let Element { x, y, ox, oy, oox, ooy, f, sp, ep, .. } = e;
    let (cur, o, oo) = if x_axis { (x, &*ox, &*oox) } else { (y, &*oy, &*ooy) };
    for (&s, &t) in sp.iter().zip(ep.iter()) {
        let (s, t) = (usize::from(s), usize::from(t));
        let Some(first) = (s..=t).find(|&p| f[p] & flag != 0) else {
            continue;
        };
        let wrap = |p: usize| if p == t { s } else { p + 1 };
        let mut at = first;
        loop {
            let start = wrap(at);
            let mut next = start;
            while f[next] & flag == 0 {
                next = wrap(next);
            }
            let (prev, end) = (at, next);
            // the touched point lower in font units, and the higher
            let (lo, hi) = if oo[prev] >= oo[end] { (end, prev) } else { (prev, end) };
            let range = oo[hi] - oo[lo];
            let (base, span) = (cur[lo], cur[hi].wrapping_sub(cur[lo]));
            let (o_lo, o_hi) = (o[lo], o[hi]);
            let (shift_lo, shift_hi) = (cur[lo] - o_lo, cur[hi] - o_hi);
            let mut p = start;
            if range == 0 {
                while p != end {
                    cur[p] += shift_lo;
                    p = wrap(p);
                }
            } else {
                let wide = span >= 0x8000 || range >= 0x8000;
                let mut ratio = None;
                while p != end {
                    let v = o[p];
                    cur[p] = if v <= o_lo {
                        v + shift_lo
                    } else if v >= o_hi {
                        v + shift_hi
                    } else if wide {
                        let r = *ratio.get_or_insert_with(|| super::fixed::vardiv(span, range, 16));
                        super::fixed::varmul(oo[p] - oo[lo], r, 16) + base
                    } else {
                        ((range >> 1).wrapping_add((oo[p] - oo[lo]).wrapping_mul(span))) / range + base
                    };
                    p = wrap(p);
                }
            }
            at = end;
            if at == first {
                break;
            }
        }
    }
}

/// The script rtgah hints a character as (which_script): 1 Latin, 2 Greek, 3 Cyrillic, 4 Hebrew, 5 Arabic,
/// 6 Hangul, 7 CJK ideographs, 8 to 17 the Indic scripts and Thai, 18 Khmer, 0 anything else (kana, CJK
/// punctuation, full-width forms). A private-use or unknown character takes the one script the font's OS/2
/// ranges name, if just one.
pub fn which_script(c: u32, ranges: [u32; 3]) -> i32 {
    let [r1, r2, r3] = ranges;
    if c == 0xFFFF || (c.wrapping_sub(0xE000) >> 8) <= 0x18 {
        let rtl = r2 & 0x8000_0000 | r1 & 0x2000 | r3 & 8;
        let mut s = if rtl != 0 { 5 } else { ((r1 & 0x800) >> 9) as i32 };
        if r2 & 0x110_0000 != 0 {
            s = 6;
        }
        let bits = [0x800, 0x8000, 0x10000, 0x20000, 0x40000, 0x80000, 0x10_0000, 0x20_0000, 0x40_0000, 0x80_0000, 0x100_0000];
        let mut n = bits.iter().filter(|&&b| r1 & b != 0).count() as i32 + i32::from(r2 & 0x110_0000 != 0);
        if r2 & 0x28C1_0000 != 0 {
            s = 7;
            n += 1;
        }
        n += i32::from(r3 & 0x200 != 0) + i32::from(r3 & 0x10000 != 0) + i32::from(rtl != 0);
        for (b, v) in [(0x8000, 8), (0x10000, 9), (0x20000, 10), (0x40000, 19), (0x80000, 15), (0x10_0000, 14), (0x20_0000, 11), (0x40_0000, 12), (0x80_0000, 13), (0x100_0000, 17)] {
            if r1 & b != 0 {
                s = v;
            }
        }
        if r3 & 0x200 != 0 {
            s = 16;
        }
        if r3 & 0x10000 != 0 {
            s = 18;
        }
        return if n == 1 { s } else { 0 };
    }
    let page = c & !0xFF;
    let half = c & !0x7F;
    let latin = c.wrapping_sub(0xFB00) < 7
        || c.wrapping_sub(0xA720) < 0xE0
        || c & !31 == 0x2C60
        || page == 0x1E00
        || half == 0x1D00
        || c.wrapping_sub(0x250) < 0x60
        || c.wrapping_sub(0x180) < 0xD0
        || half == 0x100
        || c.wrapping_sub(0x20) < 0x60
        || half == 0x80;
    if latin {
        return 1;
    }
    if half == 0x2C80 || c.wrapping_sub(0x370) < 0x90 || page == 0x1F00 {
        return 2;
    }
    if c.wrapping_sub(0xA640) < 0x60 || c & !31 == 0x2DE0 || page == 0x400 || c.wrapping_sub(0x500) < 0x30 {
        return 3;
    }
    if c.wrapping_sub(0x590) < 0x70 || c.wrapping_sub(0xFB1D) < 0x33 {
        return 4;
    }
    if c.wrapping_sub(0xFE70) < 0x90 || c.wrapping_sub(0xFB50) < 0x2B0 || page == 0x600 || c.wrapping_sub(0x750) < 0x30 {
        return 5;
    }
    if c.wrapping_sub(0xAC00) >> 4 < 0x2BB {
        return 6;
    }
    if page == 0xF900
        || c.wrapping_sub(0x4E00) >> 9 < 0x29
        || c.wrapping_sub(0x3400) >> 6 < 0x67
        || c.wrapping_sub(0x2E80) < 0x156
        || c.wrapping_sub(0x31C0) < 0x30
    {
        return 7;
    }
    if half.wrapping_sub(0x900) < 0x580 {
        return [8, 9, 10, 19, 15, 14, 11, 12, 13, 16, 17][(half.wrapping_sub(0x900) >> 7) as usize];
    }
    if half == 0x1780 { 18 } else { 0 }
}
