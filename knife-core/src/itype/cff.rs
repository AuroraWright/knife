//! CFF glyph loading as iType does it for a glyph cache (load_cff, loadGlyph, the Type 2 charstring
//! interpreter sub_1744A0): the glyph's points in font units, each operand floored to a whole unit before it
//! moves the pen, every contour closed on its start and then reversed, with the eight phantom points of which
//! the first two are the origin and the advance. A charstring or subroutine ends at its endchar or return:
//! several romfs fonts carry bytes after one (Helvetica Neue en, Demos Next en2 / en3, FOT-Seurat ja3 / en3),
//! which the SDK never reads.

use super::fixed::{normalize, vardiv, varmul};
use super::tt::Element;

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

/// An offset of `size` bytes (INDEX offsets, FDSelect ranges).
fn off_at(d: &[u8], at: usize, size: usize) -> Option<usize> {
    d.get(at..at + size).map(|b| b.iter().fold(0, |v, &b| v << 8 | usize::from(b)))
}

/// An INDEX's entry count.
fn count(d: &[u8], at: usize) -> Option<usize> {
    u16_at(d, at).map(usize::from)
}

/// An INDEX's `i`th entry; None past its count.
fn entry(d: &[u8], at: usize, i: usize) -> Option<&[u8]> {
    let n = count(d, at)?;
    if i >= n {
        return None;
    }
    let size = usize::from(*d.get(at + 2)?);
    let a = off_at(d, at + 3 + i * size, size)?;
    let b = off_at(d, at + 3 + (i + 1) * size, size)?;
    let data = at + 2 + (n + 1) * size;
    d.get(data + a..data + b)
}

/// Where an INDEX ends.
fn skip(d: &[u8], at: usize) -> Option<usize> {
    let n = count(d, at)?;
    if n == 0 {
        return Some(at + 2);
    }
    let size = usize::from(*d.get(at + 2)?);
    Some(at + 2 + (n + 1) * size + off_at(d, at + 3 + n * size, size)?)
}

/// A DICT's operators with their operands, each tagged as iType's DICT reader (sub_177680) stores it: 1 an
/// integer, 2 16.16, 3 2.30 (a real under 2 in magnitude takes 2.30, one under 0x8000 16.16, a larger one is
/// an integer).
fn dict(d: &[u8], mut f: impl FnMut(u16, &[(i32, u8)])) {
    let mut args: Vec<(i32, u8)> = Vec::new();
    let mut p = 0;
    while let Some(&b) = d.get(p) {
        p += 1;
        match b {
            28 | 29 => {
                let n = if b == 28 { 2 } else { 4 };
                let Some(v) = d.get(p..p + n) else { return };
                p += n;
                let v = if b == 28 { i32::from(i16::from_be_bytes([v[0], v[1]])) } else { i32::from_be_bytes(v.try_into().unwrap()) };
                args.push((v, 1));
            }
            30 => {
                let mut s = String::new();
                'num: while let Some(&b) = d.get(p) {
                    p += 1;
                    for n in [b >> 4, b & 15] {
                        match n {
                            0..=9 => s.push(char::from(b'0' + n)),
                            10 => s.push('.'),
                            11 => s.push('e'),
                            12 => s.push_str("e-"),
                            14 => s.push('-'),
                            15 => break 'num,
                            _ => {}
                        }
                    }
                }
                let v: f64 = s.parse().unwrap_or(0.0);
                args.push(if v.abs() < 2.0 {
                    ((v * f64::from(1 << 30)).round() as i32, 3)
                } else if v.abs() < 32768.0 {
                    ((v * 65536.0).round() as i32, 2)
                } else {
                    (v as i32, 1)
                });
            }
            32..=246 => args.push((i32::from(b) - 139, 1)),
            247..=254 => {
                let Some(&b1) = d.get(p) else { return };
                p += 1;
                let v = (i32::from(b) - 247) % 4 * 256 + i32::from(b1) + 108;
                args.push((if b < 251 { v } else { -v }, 1));
            }
            255 => return,
            12 => {
                let Some(&b1) = d.get(p) else { return };
                p += 1;
                f(0x0C00 | u16::from(b1), &args);
                args.clear();
            }
            _ => {
                f(u16::from(b), &args);
                args.clear();
            }
        }
    }
}

/// An operand as an integer (int_arg).
fn int(a: (i32, u8)) -> i32 {
    match a.1 {
        2 => a.0 >> 16,
        3 => a.0 >> 30,
        _ => a.0,
    }
}

/// The DICT state a glyph is drawn with: the Private DICT's place, local subroutines and widths, and the font
/// matrix normalized to its first entry (2.30, that entry 1.0).
#[derive(Clone, Copy, Debug)]
struct Dicts {
    private: usize,
    private_len: usize,
    subrs: usize,
    default_width: i32,
    nominal_width: i32,
    matrix: [i32; 4],
}

impl Dicts {
    /// sub_1738F0: a Top or Font DICT over this state, the matrix renormalized after.
    fn top(&mut self, d: &[u8], cff: &mut Option<&mut Cff>) {
        let mut matrix = [self.matrix[0], self.matrix[1], self.matrix[2], self.matrix[3], 0, 0];
        dict(d, |op, a| {
            let arg = |i: usize| a.get(i).copied().unwrap_or((0, 1));
            match op {
                18 => (self.private_len, self.private) = (int(arg(0)).max(0) as usize, int(arg(1)).max(0) as usize),
                0x0C07 => {
                    for (i, m) in matrix.iter_mut().enumerate() {
                        let (v, t) = arg(i);
                        *m = match t {
                            1 => v << 30,
                            2 => v << 14,
                            _ => v,
                        };
                    }
                }
                _ => {}
            }
            if let Some(c) = cff.as_deref_mut() {
                match op {
                    15 => c.charset = int(arg(0)).max(0) as usize,
                    17 => c.char_strings = int(arg(0)).max(0) as usize,
                    0x0C1E => c.ros = int(arg(0)) != 0,
                    0x0C22 => c.cid_count = int(arg(0)),
                    0x0C24 => c.fd_array = int(arg(0)).max(0) as usize,
                    0x0C25 => c.fd_select = int(arg(0)).max(0) as usize,
                    _ => {}
                }
            }
        });
        let a = matrix[0];
        self.matrix = [1 << 30, vardiv(matrix[1], a, 30), vardiv(matrix[2], a, 30), vardiv(matrix[3], a, 30)];
    }

    /// sub_173E90: the Private DICT, its subroutines and widths reset first.
    fn private(&mut self, data: &[u8]) {
        (self.subrs, self.default_width, self.nominal_width) = (0, 0, 0);
        let Some(d) = data.get(self.private..self.private + self.private_len) else { return };
        let mut subrs = 0;
        dict(d, |op, a| {
            let v = a.first().map_or(0, |&a| int(a));
            match op {
                19 => subrs = v,
                20 => self.default_width = v,
                21 => self.nominal_width = v,
                _ => {}
            }
        });
        self.subrs = (self.private as i64 + i64::from(subrs)).max(0) as usize;
    }
}

/// A CFF table's glyph data (load_cff).
#[derive(Clone, Copy, Debug)]
pub struct Cff<'a> {
    data: &'a [u8],
    char_strings: usize,
    global_subrs: usize,
    charset: usize,
    /// a CID font (ROS with a registry): each glyph's Font DICT from FDSelect, for a glyph under CIDCount
    ros: bool,
    cid_count: i32,
    fd_array: usize,
    fd_select: usize,
    top: Dicts,
}

impl<'a> Cff<'a> {
    pub fn new(data: &'a [u8]) -> Option<Cff<'a>> {
        let names = usize::from(*data.get(2)?);
        let top_index = skip(data, names)?;
        let strings = skip(data, top_index)?;
        let global_subrs = skip(data, strings)?;
        let mut cff = Cff {
            data,
            char_strings: 0,
            global_subrs,
            charset: 0,
            ros: false,
            cid_count: 0,
            fd_array: 0,
            fd_select: 0,
            top: Dicts {
                private: 0,
                private_len: 0,
                subrs: 0,
                default_width: 0,
                nominal_width: 0,
                matrix: [1_073_742, 0, 0, 1_073_742],
            },
        };
        let mut top = cff.top;
        top.top(entry(data, top_index, 0)?, &mut Some(&mut cff));
        top.private(data);
        cff.top = top;
        Some(cff)
    }

    /// getFDindex: the Font DICT of a CID font's glyph from FDSelect (format 0 or 3), 0 at or past CIDCount.
    fn fd_index(&self, gid: u16) -> usize {
        let d = self.data;
        let at = self.fd_select;
        if i32::from(gid) >= self.cid_count {
            return 0;
        }
        match d.get(at) {
            Some(0) => d.get(at + 1 + usize::from(gid)).map_or(0, |&v| usize::from(v)),
            Some(3) => {
                let mut fd = 0;
                for r in 0..u16_at(d, at + 1).unwrap_or(0) as i16 {
                    let first = at + 3 + 3 * r as usize;
                    if u16_at(d, first).is_none_or(|f| gid < f) {
                        break;
                    }
                    fd = d.get(first + 2).map_or(0, |&v| usize::from(v));
                }
                fd
            }
            _ => 0,
        }
    }

    /// resetCIDdicts: a glyph's DICT state (the Top DICT's in a font without ROS); a Font DICT keeps what it
    /// does not set from the Top DICT.
    fn dicts(&self, gid: u16) -> Dicts {
        let mut s = self.top;
        if self.ros
            && let Some(d) = entry(self.data, self.fd_array, self.fd_index(gid))
        {
            s.top(d, &mut None);
            if s.private_len != 0 {
                s.private(self.data);
            }
        }
        s
    }

    /// The glyph's points in font units with its phantom points (loadGlyph); `scale` is the 16.16 pixels per
    /// font unit flex tests its depth with. None when the charstring fails.
    pub fn load(&self, gid: u16, scale: i32) -> Option<Element> {
        let mut e = Element::default();
        let mut run = Run {
            cff: self,
            dicts: self.dicts(gid),
            stack: [0; 48],
            argc: 0,
            saved: (0, 0),
            width: 0,
            has_width: false,
            stems: (0, 0),
            store: [0; 32],
            start: (0, 0),
            scale,
            e: &mut e,
            depth: 0,
        };
        match entry(self.data, self.char_strings, usize::from(gid)) {
            Some(cs) if !cs.is_empty() => run.run(cs)?,
            _ => {
                e.oox = vec![0; 8];
                e.ooy = vec![0; 8];
                e.on = vec![0; 8];
                e.f = vec![0; 8];
                return Some(e);
            }
        }
        run.close();
        let adv = if run.has_width { run.dicts.nominal_width + run.width } else { run.dicts.default_width };
        for (&sp, &ep) in e.sp.iter().zip(&e.ep) {
            let r = usize::from(sp)..usize::from(ep) + 1;
            e.oox[r.clone()].reverse();
            e.ooy[r.clone()].reverse();
            e.on[r].reverse();
        }
        e.oox.extend([0, adv, 0, 0, 0, 0, 0, 0]);
        e.ooy.extend([0; 8]);
        let n = e.oox.len();
        e.on.resize(n, 0);
        e.f = vec![0; n];
        Some(e)
    }

    /// The glyph of a seac component (StandardEncoding code to SID, then the charset): the code itself in a font
    /// with the identity charset, else the glyph whose SID it is (the SID's own index when that holds it), 0 if
    /// none.
    fn seac_glyph(&self, code: i32) -> u16 {
        let Some(sids) = self.charset_sids() else { return code as u16 };
        let sid = match code - 32 {
            c @ 0..=0xDF => u16::from(STANDARD_SIDS[c as usize]),
            _ => 0,
        };
        if sids.get(usize::from(sid)) == Some(&sid) {
            return sid;
        }
        sids.iter().position(|&s| s == sid).map_or(0, |g| g as u16)
    }

    /// load_cff's charset: each glyph's SID, None for the identity one (offset 0, or a range covering every
    /// glyph from SID 1); offsets 1 and 2 the predefined Expert and Expert Subset ranges. A format 0 charset is
    /// read as little-endian, as the SDK copies it.
    fn charset_sids(&self) -> Option<Vec<u16>> {
        let d = self.data;
        let n = count(d, self.char_strings)?;
        let mut sids = vec![0u16; n];
        let fill = |sids: &mut Vec<u16>, gid: &mut usize, first: u16, left: u16| {
            for sid in first..=first.saturating_add(left) {
                if *gid >= n {
                    break;
                }
                sids[*gid] = sid;
                *gid += 1;
            }
        };
        let mut gid = 1;
        match self.charset {
            0 => return None,
            1 | 2 => {
                let ranges = if self.charset == 1 { &EXPERT[..] } else { &EXPERT_SUBSET[..] };
                for &(a, b) in ranges {
                    fill(&mut sids, &mut gid, a, b - a);
                }
            }
            at => match d.get(at)? {
                0 => {
                    for (g, s) in sids.iter_mut().enumerate().skip(1) {
                        let b = d.get(at + 2 * g - 1..at + 2 * g + 1)?;
                        *s = u16::from_le_bytes([b[0], b[1]]);
                    }
                }
                &f if f == 1 || f == 2 => {
                    let mut p = at + 1;
                    while gid < n {
                        let first = u16_at(d, p)?;
                        let left = if f == 1 { u16::from(*d.get(p + 2)?) } else { u16_at(d, p + 2)? };
                        p += if f == 1 { 3 } else { 4 };
                        if usize::from(left) + 2 == n && first == 1 {
                            return None;
                        }
                        fill(&mut sids, &mut gid, first, left);
                    }
                }
                _ => {}
            },
        }
        Some(sids)
    }
}

/// The SDK's StandardEncoding (byte_A620EA): codes 32 to 255 to SIDs.
const STANDARD_SIDS: [u8; 224] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58,
    59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86,
    87, 88, 89, 90, 91, 92, 93, 94, 95, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 0, 111, 112, 113,
    114, 0, 115, 116, 117, 118, 119, 120, 121, 122, 0, 123, 0, 124, 125, 126, 127, 128, 129, 130, 131, 0, 132, 133,
    0, 134, 135, 136, 137, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 138, 0, 139, 0, 0, 0, 0, 140, 141, 142,
    143, 0, 0, 0, 0, 0, 144, 0, 0, 0, 145, 0, 0, 146, 147, 148, 149, 0, 0, 0, 0,
];

/// The predefined Expert charset's SID ranges (unk_A621CA).
const EXPERT: [(u16, u16); 17] = [
    (1, 1), (229, 238), (13, 15), (99, 99), (239, 248), (27, 28), (249, 266), (109, 110), (267, 318), (158, 158),
    (155, 155), (163, 163), (319, 326), (150, 150), (164, 164), (169, 169), (327, 378),
];

/// The predefined Expert Subset charset's SID ranges (unk_A62212).
const EXPERT_SUBSET: [(u16, u16); 23] = [
    (1, 1), (231, 232), (235, 238), (13, 15), (99, 99), (239, 248), (27, 28), (249, 251), (253, 266), (109, 110),
    (267, 270), (272, 272), (300, 302), (305, 305), (314, 315), (158, 158), (155, 155), (163, 163), (320, 326),
    (150, 150), (164, 164), (169, 169), (327, 346),
];

/// The subroutine bias for an INDEX of `n` entries.
fn bias(n: usize) -> i32 {
    if n < 1240 {
        107
    } else if n < 33900 {
        1131
    } else {
        32768
    }
}

/// One glyph's charstring interpreter state (sub_1744A0's a5): the 16.16 operand stack, the pen as the last
/// subroutine call or return left it, the width, the stem counts hintmask sizes its mask by and the transient
/// array; the points go to `e`.
struct Run<'c, 'a> {
    cff: &'c Cff<'a>,
    dicts: Dicts,
    stack: [i32; 48],
    argc: usize,
    saved: (i32, i32),
    width: i32,
    has_width: bool,
    stems: (i32, i32),
    store: [i32; 32],
    /// the open contour's first point
    start: (i32, i32),
    scale: i32,
    e: &'c mut Element,
    depth: u32,
}

impl Run<'_, '_> {
    /// Operand `i` in whole font units (floored).
    fn a(&self, i: usize) -> i32 {
        self.stack.get(i).map_or(0, |v| v >> 16)
    }

    /// The width from the first operand when the operator has one more than its own and none was seen yet;
    /// how many operands it took (0 or 1).
    fn take_width(&mut self, own: usize) -> usize {
        if self.argc == own + 1 && !self.has_width {
            (self.width, self.has_width) = (self.a(0), true);
            1
        } else {
            0
        }
    }

    /// A point through the font matrix.
    fn push(&mut self, x: i32, y: i32, on: bool) {
        let m = self.dicts.matrix;
        let px = varmul(x, m[0], 30).wrapping_add(varmul(y, m[2], 30));
        let py = varmul(x, m[1], 30).wrapping_add(varmul(y, m[3], 30));
        let e = &mut *self.e;
        if e.sp.is_empty() {
            e.sp.push(0);
            e.ep.push(0);
        }
        e.oox.push(px);
        e.ooy.push(py);
        e.on.push(u8::from(on));
        *e.ep.last_mut().unwrap() = (e.oox.len() - 1) as u16;
    }

    /// Close the open contour on its start point unless it ends there (sub_176F60, loadGlyph).
    fn close(&mut self) {
        let e = &mut *self.e;
        if let (Some(&x), Some(&y)) = (e.oox.last(), e.ooy.last())
            && !e.sp.is_empty()
            && (x, y) != self.start
        {
            e.oox.push(self.start.0);
            e.ooy.push(self.start.1);
            e.on.push(1);
            *e.ep.last_mut().unwrap() = (e.oox.len() - 1) as u16;
        }
    }

    /// sub_176F60
    fn move_to(&mut self, x: i32, y: i32) {
        self.close();
        let n = self.e.oox.len() as u16;
        self.e.sp.push(n);
        self.e.ep.push(n);
        self.push(x, y, true);
        self.start = (*self.e.oox.last().unwrap(), *self.e.ooy.last().unwrap());
    }

    /// sub_177290
    fn curve(&mut self, p: [i32; 6]) {
        self.push(p[0], p[1], false);
        self.push(p[2], p[3], false);
        self.push(p[4], p[5], true);
    }

    /// sub_1774D0: a flex drawn as a line to its end when its joint lies under `fd` / 100 pixels from the line
    /// its first control point's x and its end's y make with its start, else as its two curves.
    fn flex(&mut self, p: [(i32, i32); 7], fd: i32) {
        let s = self.scale;
        let (x0, y0) = (p[0].0.wrapping_mul(s), p[0].1.wrapping_mul(s));
        let n = normalize([p[6].1.wrapping_mul(s).wrapping_sub(y0), x0.wrapping_sub(p[1].0.wrapping_mul(s))]);
        let d = varmul(y0.wrapping_neg(), n[1], 16)
            .wrapping_add(varmul(x0.wrapping_neg(), n[0], 16))
            .wrapping_add(varmul(n[0], p[3].0.wrapping_mul(s), 16))
            .wrapping_add(varmul(n[1], p[3].1.wrapping_mul(s), 16));
        if d.wrapping_abs() < (fd << 16) / 100 {
            self.push(p[6].0, p[6].1, true);
        } else {
            self.curve([p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1]);
            self.curve([p[4].0, p[4].1, p[5].0, p[5].1, p[6].0, p[6].1]);
        }
    }

    /// A subroutine of the INDEX at `at` by its biased number on the stack.
    fn subr(&mut self, at: usize) -> Option<()> {
        self.argc = self.argc.checked_sub(1)?;
        let d = self.cff.data;
        let n = count(d, at)?;
        let i = bias(n).checked_add(self.a(self.argc))?;
        let cs = entry(d, at, usize::try_from(i).ok()?)?;
        self.run(cs)
    }

    /// endchar's seac: the base glyph, then the accent from (adx, ady) with no width or stems, the width kept.
    fn seac(&mut self) -> Option<()> {
        let w = usize::from(self.argc == 5);
        let width = if w == 1 { self.a(0) } else { 0 };
        let (adx, ady, base, accent) = (self.a(w), self.a(w + 1), self.a(w + 2), self.a(w + 3));
        self.argc = 0;
        let cff = self.cff;
        let glyph = |code: i32| {
            let gid = cff.seac_glyph(code);
            entry(cff.data, cff.char_strings, usize::from(gid)).filter(|cs| !cs.is_empty()).map(|cs| (gid, cs))
        };
        let done = (|| {
            let (gid, cs) = glyph(base)?;
            if cff.ros {
                self.dicts = cff.dicts(gid);
            }
            Some(self.run(cs))
        })();
        if let Some(r) = done {
            r?;
            let kept = (self.width, self.has_width);
            (self.width, self.has_width, self.saved, self.stems) = (0, false, (adx, ady), (0, 0));
            if let Some((gid, cs)) = glyph(accent) {
                if cff.ros {
                    self.dicts = cff.dicts(gid);
                }
                let r = self.run(cs);
                (self.width, self.has_width) = kept;
                r?;
            }
        }
        if w == 1 {
            (self.width, self.has_width) = (width, true);
        }
        Some(())
    }

    /// Interpret a charstring or subroutine to its end, `return` or `endchar`; None on an error (a subroutine
    /// out of range, a number cut short).
    fn run(&mut self, cs: &[u8]) -> Option<()> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let r = self.ops(cs);
        self.depth -= 1;
        r
    }

    fn ops(&mut self, cs: &[u8]) -> Option<()> {
        let (mut x, mut y) = self.saved;
        let mut p = 0;
        while let Some(&b) = cs.get(p) {
            p += 1;
            let v = match b {
                28 => {
                    let v = i16::from_be_bytes(cs.get(p..p + 2)?.try_into().ok()?);
                    p += 2;
                    Some(i32::from(v) << 16)
                }
                32..=246 => Some((i32::from(b) - 139) << 16),
                247..=254 => {
                    let w = (i32::from(b) - 247) % 4 * 256 + i32::from(*cs.get(p)?) + 108;
                    p += 1;
                    Some(if b < 251 { w << 16 } else { -w << 16 })
                }
                255 => {
                    let v = i32::from_be_bytes(cs.get(p..p + 4)?.try_into().ok()?);
                    p += 4;
                    Some(v)
                }
                _ => None,
            };
            if let Some(v) = v {
                *self.stack.get_mut(self.argc)? = v;
                self.argc += 1;
                continue;
            }
            let argc = self.argc;
            let st = self.stack;
            let a = |i: usize| st.get(i).map_or(0, |v| v >> 16);
            match b {
                // hstem, hstemhm, vstem, vstemhm
                1 | 18 | 3 | 23 => {
                    let w = if argc & 1 != 0 && !self.has_width {
                        (self.width, self.has_width) = (a(0), true);
                        1
                    } else {
                        0
                    };
                    let n = ((argc - w) / 2) as i32;
                    if b == 1 || b == 18 { self.stems.0 += n } else { self.stems.1 += n }
                }
                // vmoveto, rmoveto, hmoveto
                4 => {
                    let w = self.take_width(1);
                    y += self.a(w);
                    self.move_to(x, y);
                }
                21 => {
                    let w = self.take_width(2);
                    (x, y) = (x + self.a(w), y + self.a(w + 1));
                    self.move_to(x, y);
                }
                22 => {
                    let w = self.take_width(1);
                    x += self.a(w);
                    self.move_to(x, y);
                }
                // rlineto, hlineto, vlineto
                5 => {
                    for i in (0..argc).step_by(2) {
                        (x, y) = (x + a(i), y + a(i + 1));
                        self.push(x, y, true);
                    }
                }
                6 | 7 => {
                    for i in 0..argc {
                        if (i & 1 == 0) == (b == 6) { x += a(i) } else { y += a(i) }
                        self.push(x, y, true);
                    }
                }
                // rrcurveto
                8 => {
                    for i in (0..argc).step_by(6) {
                        let c = [x + a(i), y + a(i + 1)];
                        let d = [c[0] + a(i + 2), c[1] + a(i + 3)];
                        (x, y) = (d[0] + a(i + 4), d[1] + a(i + 5));
                        self.curve([c[0], c[1], d[0], d[1], x, y]);
                    }
                }
                // callsubr, callgsubr: the pen saved for the subroutine and taken back from it
                10 | 29 => {
                    self.saved = (x, y);
                    let at = if b == 10 { self.dicts.subrs } else { self.cff.global_subrs };
                    self.subr(at)?;
                    (x, y) = self.saved;
                    continue;
                }
                // return
                11 => {
                    self.saved = (x, y);
                    return Some(());
                }
                12 => {
                    let op = *cs.get(p)?;
                    p += 1;
                    if !self.escape(op, &mut x, &mut y) {
                        continue;
                    }
                }
                // endchar
                14 => {
                    if argc == 1 && !self.has_width {
                        (self.width, self.has_width) = (a(0), true);
                    } else if argc > 3 {
                        return self.seac();
                    }
                    self.saved = (x, y);
                    return Some(());
                }
                // hintmask, cntrmask: stems given before them are vertical
                19 | 20 => {
                    self.stems.1 += (argc >> 1) as i32;
                    p += ((self.stems.0 + self.stems.1 + 7) >> 3) as usize;
                }
                // rcurveline
                24 => {
                    let mut i = 0;
                    x += a(0);
                    while i + 2 < argc {
                        let c = [x, y + a(i + 1)];
                        let d = [c[0] + a(i + 2), c[1] + a(i + 3)];
                        let e = [d[0] + a(i + 4), d[1] + a(i + 5)];
                        self.curve([c[0], c[1], d[0], d[1], e[0], e[1]]);
                        (x, y) = (e[0] + a(i + 6), e[1]);
                        i += 6;
                    }
                    y += a(i + 1);
                    self.push(x, y, true);
                }
                // rlinecurve
                25 => {
                    let mut i = 0;
                    loop {
                        (x, y) = (x + a(i), y + a(i + 1));
                        if i + 6 >= argc {
                            break;
                        }
                        i += 2;
                        self.push(x, y, true);
                    }
                    let c = [x, y];
                    let d = [c[0] + a(i + 2), c[1] + a(i + 3)];
                    (x, y) = (d[0] + a(i + 4), d[1] + a(i + 5));
                    self.curve([c[0], c[1], d[0], d[1], x, y]);
                }
                // vvcurveto, hhcurveto
                26 | 27 => {
                    let mut i = argc & 1;
                    let mut first = if i == 1 { a(0) } else { 0 };
                    while i < argc {
                        let c = if b == 26 { [x + first, y + a(i)] } else { [x + a(i), y + first] };
                        let d = [c[0] + a(i + 1), c[1] + a(i + 2)];
                        (x, y) = if b == 26 { (d[0], d[1] + a(i + 3)) } else { (d[0] + a(i + 3), d[1]) };
                        self.curve([c[0], c[1], d[0], d[1], x, y]);
                        first = 0;
                        i += 4;
                    }
                }
                // vhcurveto, hvcurveto: alternating, the last curve's fifth operand the other way
                30 | 31 => {
                    let mut i = 0;
                    let mut vertical = b == 30;
                    while i < argc {
                        let last = argc - i == 5;
                        let c = if vertical { [x, y + a(i)] } else { [x + a(i), y] };
                        let d = [c[0] + a(i + 1), c[1] + a(i + 2)];
                        let f = if last { a(i + 4) } else { 0 };
                        (x, y) = if vertical { (d[0] + a(i + 3), d[1] + f) } else { (d[0] + f, d[1] + a(i + 3)) };
                        self.curve([c[0], c[1], d[0], d[1], x, y]);
                        vertical = !vertical;
                        i += if last { 5 } else { 4 };
                    }
                }
                // any other operator is passed over with its operands kept
                _ => continue,
            }
            self.argc = 0;
        }
        Some(())
    }

    /// An escaped operator; true when it clears the stack (the flexes).
    fn escape(&mut self, op: u8, x: &mut i32, y: &mut i32) -> bool {
        let n = self.argc;
        let st = self.stack;
        let top = |k: usize| n.checked_sub(k).and_then(|i| st.get(i).copied()).unwrap_or(0);
        let (t1, t2) = (top(1), top(2));
        let s = &mut self.stack;
        let put = |s: &mut [i32; 48], k: usize, v: i32| {
            if let Some(slot) = n.checked_sub(k).and_then(|i| s.get_mut(i)) {
                *slot = v;
            }
        };
        self.argc = match op {
            3 => {
                put(s, 2, i32::from(t2 != 0 && t1 != 0));
                n.saturating_sub(1)
            }
            4 => {
                put(s, 2, i32::from(t2 != 0 || t1 != 0));
                n.saturating_sub(1)
            }
            5 => {
                put(s, 1, i32::from(t1 == 0));
                n
            }
            9 => {
                put(s, 1, t1.wrapping_abs());
                n
            }
            10 | 11 | 12 | 15 | 24 => {
                put(s, 2, match op {
                    10 => t2.wrapping_add(t1),
                    11 => t2.wrapping_sub(t1),
                    12 => vardiv(t2, t1, 16),
                    15 => i32::from(t2 == t1),
                    _ => varmul(t2, t1, 16),
                });
                n.saturating_sub(1)
            }
            14 => {
                put(s, 1, t1.wrapping_neg());
                n
            }
            18 => n.saturating_sub(1),
            // put, get: 32 slots
            20 => {
                if (t1 as u32) >> 21 == 0 {
                    self.store[(t1 >> 16) as usize] = t2;
                }
                n.saturating_sub(2)
            }
            21 => {
                put(s, 1, if t1 >> 21 == 0 { self.store[(t1 >> 16) as usize] } else { 0 });
                n
            }
            // ifelse: the SDK's picks s2 when v1 <= v2
            22 => {
                put(s, 4, if t2 <= t1 { top(3) } else { top(4) });
                n.saturating_sub(3)
            }
            26 => {
                put(s, 1, (((t1.max(0) as u64) << 16) as f64).sqrt() as i32);
                n
            }
            27 => match s.get_mut(n) {
                Some(slot) => {
                    *slot = t1;
                    n + 1
                }
                None => n,
            },
            28 => {
                if n >= 2 {
                    s.swap(n - 1, n - 2);
                }
                n
            }
            // index
            29 => {
                let i = t1 >> 16;
                put(s, 1, if i < 0 { t2 } else { n.checked_sub(2 + i as usize).and_then(|k| st.get(k).copied()).unwrap_or(0) });
                n
            }
            // roll
            30 => {
                let (count, j) = (t2 >> 16, t1 >> 16);
                let n = n.saturating_sub(2);
                if count >= 1 && (count as usize) <= n {
                    s[n - count as usize..n].rotate_right(j.rem_euclid(count) as usize);
                }
                n
            }
            0x22..=0x25 => {
                let a = |i: usize| st.get(i).map_or(0, |v| v >> 16);
                let (x0, y0) = (*x, *y);
                let mut p = [(x0, y0); 7];
                let mut fd = 50;
                match op {
                    // hflex
                    0x22 => {
                        p[1] = (x0 + a(0), y0);
                        p[2] = (p[1].0 + a(1), y0 + a(2));
                        p[3] = (p[2].0 + a(3), p[2].1);
                        p[4] = (p[3].0 + a(4), p[2].1);
                        p[5] = (p[4].0 + a(5), y0);
                        p[6] = (p[5].0 + a(6), y0);
                    }
                    // flex
                    0x23 => {
                        for k in 1..7 {
                            p[k] = (p[k - 1].0 + a(2 * k - 2), p[k - 1].1 + a(2 * k - 1));
                        }
                        fd = a(12);
                    }
                    // hflex1
                    0x24 => {
                        p[1] = (x0 + a(0), y0 + a(1));
                        p[2] = (p[1].0 + a(2), p[1].1 + a(3));
                        p[3] = (p[2].0 + a(4), p[2].1);
                        p[4] = (p[3].0 + a(5), p[2].1);
                        p[5] = (p[4].0 + a(6), p[2].1 + a(7));
                        p[6] = (p[5].0 + a(8), y0);
                    }
                    // flex1: the last operand along the longer way, the other coordinate left as the fifth
                    // point's (the SDK's; the spec returns it to the start's)
                    _ => {
                        for k in 1..6 {
                            p[k] = (p[k - 1].0 + a(2 * k - 2), p[k - 1].1 + a(2 * k - 1));
                        }
                        let (dx, dy) = ((p[5].0 - x0).abs(), (p[5].1 - y0).abs());
                        p[6] = if dx > dy { (p[5].0 + a(10), p[5].1) } else { (p[5].0, p[5].1 + a(10)) };
                    }
                }
                (*x, *y) = p[6];
                self.argc = 0;
                self.flex(p, fd);
                return true;
            }
            _ => n,
        };
        false
    }
}

/// extract_outline's CFF path (fill_outline_cff): each contour of more than one point as a move to its first
/// point and then, along its points, a line to each on-curve point, a quadratic through one off-curve point and
/// a cubic through two (three in a row are passed over), 16.16, not closed. The first segment type carries 0x80;
/// the box is that of all the points, control points included. A glyph of fewer than three points has no outline.
pub fn extract(e: &Element) -> super::raster::Outline {
    let mut o = super::raster::Outline::default();
    if e.points() < 3 {
        return o;
    }
    let pt = |o: &mut super::raster::Outline, i: usize| {
        o.x.push(e.x.get(i).copied().unwrap_or(0) << 10);
        o.y.push(e.y.get(i).copied().unwrap_or(0) << 10);
    };
    let on = |i: usize| e.on.get(i).is_some_and(|&v| v != 0);
    for (&sp, &ep) in e.sp.iter().zip(&e.ep) {
        let (sp, ep) = (usize::from(sp), usize::from(ep));
        if sp == ep {
            continue;
        }
        o.types.push(0);
        pt(&mut o, sp);
        let mut i = sp;
        while i < ep {
            if on(i + 1) {
                o.types.push(1);
                pt(&mut o, i + 1);
                i += 1;
            } else if on(i + 2) {
                o.types.push(2);
                pt(&mut o, i + 1);
                pt(&mut o, i + 2);
                i += 2;
            } else {
                if on(i + 3) {
                    o.types.push(3);
                    for k in 1..4 {
                        pt(&mut o, i + k);
                    }
                }
                i += 3;
            }
        }
    }
    if let Some(t) = o.types.first_mut() {
        *t |= 0x80;
    }
    if !o.x.is_empty() {
        let (xs, ys) = (o.x.iter(), o.y.iter());
        o.bbox = [*xs.clone().min().unwrap(), *xs.max().unwrap(), *ys.clone().min().unwrap(), *ys.max().unwrap()];
    }
    o
}

#[cfg(test)]
mod tests {
    use super::super::glyph::Hinter;
    use super::super::tt::Font;

    /// The glyphs whose charstring or subroutine has bytes after its endchar / return draw (needs switch_romfs
    /// and switch_romfs_en): the bubble text "Sometimes, ..." of the overseas bad ends, the English message
    /// font's ö, ’, ”, — and the rest.
    #[test]
    fn bytes_after_endchar() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for font in [
            "switch_romfs/ja3/FOT-SeuratPro-M.otf",
            "switch_romfs_en/en/HelveticaNeueLTGEO-55Roman.otf",
            "switch_romfs_en/en2/DemosNextPro-Regular.otf",
            "switch_romfs_en/en3/FOT-SeuratPro-M.otf",
        ] {
            let Ok(data) = std::fs::read(root.join(font)) else { continue };
            let f = Font::new(&data).unwrap();
            let mut h = Hinter::new(&f, 40);
            for c in "Sometimes,it'snourheöà’”—".chars() {
                let gid = f.glyph(c as u32);
                if gid != 0 {
                    assert!(h.cache_map(&f, gid, 0).is_some_and(|m| m.width > 0), "{font}: {c}");
                }
            }
        }
    }
}
