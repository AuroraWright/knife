//! A TrueType font's glyphs as the SDK hands them to the scan converter at the glyph cache's size: loaded,
//! scaled and, when the font may be autohinted, put through rtgah with the reference lines get_ref_lines
//! measured from the font's own letters the first time a glyph was asked for.

use super::cff;
use super::effect;
use super::raster::{self, Graymap, Outline};
use super::rtgah::{self, Globals};
use super::fixed::{vardiv, varmul};
use super::fnt::Interp;
use super::tt::{self, Composite, Element, Font, Scale};

/// fsFlags while the SDK fills a glyph cache: autohinting on and forced over any glyph program.
const FS_FLAGS: u32 = 0x2140_4000;

/// The program do_hints runs for an autohinted glyph: `PUSHB[0] 11 AA[]`, the SDK's instruction that autohints.
const AUTOHINT: [u8; 3] = [0xB0, 11, 0x7F];

/// A script's reference characters: a stem letter, flat capitals, round capital, x-height letters, round small letter.
type RefChars = (u32, &'static [u32], u32, &'static [u32], u32);

/// One font's autohinting state at one size (the font itself is passed to each call).
pub struct Hinter {
    pub scale: Scale,
    pub g: Globals,
    /// refs+48: whether glyphs are autohinted at all (get_ref_lines clears it for a font it cannot measure)
    pub autohint: bool,
    /// fsFlags 0x1000000, set only while get_ref_lines measures: autohinting even a glyph with its own program
    forced: bool,
    ranges: [u32; 3],
    /// a TrueType font's instruction interpreter at this size (fs_NewTransformation ran its font program)
    interp: Option<Interp>,
}

impl Hinter {
    /// `ppem` whole pixels per em; runs get_ref_lines.
    pub fn new(font: &Font, ppem: u16) -> Hinter {
        let scale = Scale::new(i32::from(ppem) << 16, font.upem);
        // the font type the SDK's loader tags the font with (sub_1715F0's +65), which rtgah's y pass reads: 7 for
        // an OpenType CFF font, whose contours it then closes up
        let g418 = if font.cff.is_some() { 7 } else { 0 };
        let g = Globals { ppem, fs_flags: FS_FLAGS, font_flags: 0, refs: [0; 72], g418, top: 0, bottom: 0, g416: 1 };
        let ranges = font.ranges();
        let interp = font.cff.is_none().then(|| Interp::new(font.data, scale, ppem));
        let mut h = Hinter { scale, g, autohint: true, forced: true, ranges, interp };
        h.ref_lines(font);
        h.forced = false;
        h
    }

    /// A glyph's points ready for the scan converter (26.6, y up), hinted when the font allows; None for a
    /// glyph that will not load or whose program fails. A TrueType glyph is fitted whether or not the font may
    /// be autohinted (its program runs instead).
    pub fn glyph(&mut self, font: &Font, gid: u16) -> Option<Element> {
        self.outline(font, gid, font.cff.is_none() || self.autohint)
    }

    /// A glyph as nn::font's glyph cache holds it (`cache_cell` of its raster at the hinter's size); None as
    /// for `glyph`.
    pub fn cache_map(&mut self, font: &Font, gid: u16, ow: u16) -> Option<Graymap> {
        let e = self.glyph(font, gid)?;
        let ppem = self.g.ppem;
        let o = if font.cff.is_some() { cff::extract(&e) } else { extract(&e) };
        Some(cache_cell(&raster::graymap(&o, ppem)?, ow))
    }

    /// fsg_GridFit and get_comp: a TrueType glyph is fitted (`hint`: fsFlags bit 2 clear) unless the control
    /// value program, run first whenever another size or font was fitted last, set INSTCTRL's first bit; it is
    /// autohinted when the font allows it and the glyph has no program of its own (any glyph while
    /// get_ref_lines forces it), else its program runs (fsFlags at a glyph cache fill: 0x400000).
    /// A CFF glyph (cff_GridFit) has no program and only its origin is put on the grid before autohinting (the
    /// advance it may round after is not read here).
    fn outline(&mut self, font: &Font, gid: u16, hint: bool) -> Option<Element> {
        let (mut e, code, hint) = match &font.cff {
            Some(c) => (c.load(gid, self.scale.ppem / i32::from(font.upem))?, &[][..], hint),
            None => {
                let hint = self.interp.as_mut()?.begin()? && hint;
                match font.load(gid) {
                    Some((e, code)) => (e, code, hint),
                    None => return self.composite(font, gid, hint),
                }
            }
        };
        tt::scale(&mut e, &self.scale);
        let n = e.points();
        if font.cff.is_some() {
            e.x[n] = (e.x[n] + 32) & !63;
            if hint && !e.sp.is_empty() && self.autohint {
                let ch = font.inverse(gid);
                let script = tt::which_script(ch, self.ranges);
                self.g.g416 = 1;
                rtgah::rtgah(&mut e, script, ch as i32, &mut self.g);
            }
        } else {
            tt::round_phantoms(&mut e, &self.scale, font.upem, hint);
            let code = if !hint || e.sp.is_empty() {
                &[][..]
            } else if self.autohint && (self.forced || code.is_empty()) {
                &AUTOHINT[..]
            } else {
                code
            };
            self.hints(font, gid, &mut e, code)?;
        }
        Some(e)
    }

    /// do_hints: the graphics state from the defaults, then the glyph's program over its points (none when
    /// `code` is empty), AA[] autohinting it with rtgah; None when the program fails.
    fn hints(&mut self, font: &Font, gid: u16, e: &mut Element, code: &[u8]) -> Option<()> {
        let Hinter { interp, g, ranges, autohint, .. } = self;
        let it = interp.as_mut()?;
        it.reset();
        if code.is_empty() {
            return Some(());
        }
        let ch = font.inverse(gid);
        let script = tt::which_script(ch, *ranges);
        let mut aa = |e: &mut Element| {
            g.g416 = u16::from(*autohint);
            rtgah::rtgah(e, script, ch as i32, g);
        };
        it.glyph(e, code, Some(&mut aa)).ok()
    }

    /// get_comp for a composite glyph: its advance points from its own box and metrics, put on the grid as a
    /// simple glyph's (comp_phantom_points), then its components one after another (`assemble`); the whole
    /// is autohinted when the font allows it, unless the glyph has instructions (they run instead) or one of its
    /// components ran a program of its own (do_hints marks the font, +109 bit 0); while get_ref_lines forces
    /// autohinting it always is.
    fn composite(&mut self, font: &Font, gid: u16, hint: bool) -> Option<Element> {
        let c = font.composite(gid)?;
        let mut p = Element::default();
        font.push_phantoms(&mut p, gid, c.x_min, c.y_max);
        tt::scale(&mut p, &self.scale);
        tt::round_phantoms(&mut p, &self.scale, font.upem, hint);
        let mut st = Assembly { ran: false, transform: false, metrics: [0, 1, 2, 3].map(|i| (p.x[i], p.y[i])), simple: 0 };
        let mut e = self.assemble(font, &c, IDENTITY, hint, &mut st, p)?;
        e.f.fill(0);
        let code = if !hint || e.sp.is_empty() {
            &[][..]
        } else if self.autohint && (self.forced || (!c.instructions && !st.ran)) {
            &AUTOHINT[..]
        } else if c.instructions {
            c.code
        } else {
            &[][..]
        };
        if !code.is_empty() {
            to_font_units(&mut e, font.upem, self.scale.ppem);
        }
        self.hints(font, gid, &mut e, code)?;
        Some(e)
    }

    /// A composite glyph's components (get_comp's loop) under the matrix `m`: each loaded, scaled and on the grid
    /// as a glyph of its own, its program run (none while get_ref_lines forces autohinting), from the first
    /// scaled component on transformed by its matrix (apply_transformation), then moved by its offset
    /// (apply_offsets) and appended, its contours numbered on. The advance points are the last component's with
    /// the first four the glyph's (`Assembly::metrics`); `none` stands in when there are no components.
    fn assemble(&mut self, font: &Font, c: &Composite, m: [i32; 4], hint: bool, st: &mut Assembly, none: Element) -> Option<Element> {
        let mut out = Element::default();
        let mut last = none;
        for comp in &c.components {
            // compose_trans: a scaled component's matrix under the composite's, and all after it transformed
            let cm = match comp.scale {
                Some(s) => {
                    st.transform = true;
                    compose(s, m)
                }
                None => m,
            };
            let (mut e, code, simple) = match font.composite(comp.gid) {
                Some(inner) => {
                    let e = self.assemble(font, &inner, cm, hint, st, last.clone())?;
                    (e, inner.instructions.then_some(inner.code), false)
                }
                None => {
                    let (mut e, code) = font.load(comp.gid)?;
                    tt::scale(&mut e, &self.scale);
                    tt::round_phantoms(&mut e, &self.scale, font.upem, hint);
                    let run = hint && !code.is_empty() && !(self.forced && self.autohint);
                    self.hints(font, comp.gid, &mut e, if run { code } else { &[] })?;
                    st.ran |= run;
                    if st.transform {
                        transform(&mut e, cm);
                    }
                    (e, None, true)
                }
            };
            let n = e.points();
            // apply_offsets: x / y offsets scaled as the points (by the composite's matrix when it only scales),
            // on the grid with ROUND_XY_TO_GRID; else the composite's point arg 1 matched with the
            // component's point arg 2 (after the first simple component)
            let (dx, dy) = if comp.flags & 2 != 0 {
                let (mut dx, mut dy) = (self.scale.apply(comp.args.0), self.scale.apply(comp.args.1));
                if m[1] == 0 && m[2] == 0 {
                    (dx, dy) = (varmul(dx, m[0], 16), varmul(dy, m[3], 16));
                }
                if comp.flags & 4 != 0 {
                    (dx, dy) = ((dx + 32) & !63, (dy + 32) & !63);
                }
                (dx, dy)
            } else if st.simple != 0 {
                let at = |v: &[i32], i: i32| usize::try_from(i).ok().and_then(|i| v.get(i).copied()).unwrap_or(0);
                let (a, b) = comp.args;
                (at(&out.x, a) - at(&e.x[..n], b), at(&out.y, a) - at(&e.y[..n], b))
            } else {
                (0, 0)
            };
            e.x.iter_mut().for_each(|v| *v += dx);
            e.y.iter_mut().for_each(|v| *v += dy);
            e.ox.clone_from(&e.x);
            e.oy.clone_from(&e.y);
            // a nested composite's own instructions, after its offset
            if let Some(code) = code {
                let run = hint && !code.is_empty();
                if run {
                    to_font_units(&mut e, font.upem, self.scale.ppem);
                }
                self.hints(font, comp.gid, &mut e, if run { code } else { &[] })?;
                st.ran |= run;
            }
            if simple {
                st.simple += 1;
            }
            if comp.flags & 0x200 != 0 {
                st.metrics = [0, 1, 2, 3].map(|i| (e.x[n + i], e.y[n + i]));
            }
            let base = out.x.len() as u16;
            for (to, from) in [(&mut out.x, &e.x), (&mut out.y, &e.y), (&mut out.ox, &e.ox), (&mut out.oy, &e.oy), (&mut out.oox, &e.oox), (&mut out.ooy, &e.ooy)] {
                to.extend_from_slice(&from[..n]);
            }
            out.on.extend_from_slice(&e.on[..n]);
            out.f.extend_from_slice(&e.f[..n]);
            out.sp.extend(e.sp.iter().map(|&v| v + base));
            out.ep.extend(e.ep.iter().map(|&v| v + base));
            last = e;
        }
        let n = last.points();
        for (to, from) in [(&mut out.x, &last.x), (&mut out.y, &last.y), (&mut out.ox, &last.ox), (&mut out.oy, &last.oy), (&mut out.oox, &last.oox), (&mut out.ooy, &last.ooy)] {
            to.extend_from_slice(&from[n..]);
        }
        out.on.extend_from_slice(&last.on[n..]);
        out.f.extend_from_slice(&last.f[n..]);
        let n = out.points();
        for (i, &(x, y)) in st.metrics.iter().enumerate() {
            (out.x[n + i], out.y[n + i]) = (x, y);
        }
        out.ox.clone_from(&out.x);
        out.oy.clone_from(&out.y);
        Some(out)
    }

    /// A reference letter's lowest and highest points (26.6) as get_ref_lines reads them from the outline it
    /// makes (find_or_make_outline): hinted or not, None when the font lacks the letter. An outline of under
    /// three points has an empty box.
    fn measure(&mut self, font: &Font, ch: u32, hint: bool) -> Option<(i32, i32)> {
        let gid = font.glyph(ch);
        if gid == 0 {
            return None;
        }
        let e = self.outline(font, gid, hint)?;
        let n = e.points();
        if n < 3 {
            return Some((0, 0));
        }
        let ys = &e.y[..n];
        Some((*ys.iter().min()?, *ys.iter().max()?))
    }

    /// Hint a script's first reference letter (along x it sets refs[19..20] when nothing has); false when that
    /// glyph proved too complex to measure, which stops the measuring and the autohinting.
    fn first(&mut self, font: &Font, ch: u32) -> bool {
        self.measure(font, ch, true).is_none() || self.g.g416 != 0
    }

    /// get_ref_lines (0x1681c0): the reference lines of each script the font's OS/2 ranges name, from the
    /// bounds of its letters: for Latin the cap height and baseline (the lowest top and highest bottom of
    /// B E H T Z), O's overshoots, the x-height (the lowest top of w x y z) and o's top; Greek and Cyrillic
    /// alike from their letters; digits when there are no capitals; for CJK the hinted tops and bottoms of
    /// two complex characters and 林's bounds; Hangul's from two syllables.
    fn ref_lines(&mut self, font: &Font) {
        let r = &mut self.g.refs;
        if r[65] != 0 {
            return;
        }
        r[13..69].fill(0);
        r[65] = 1;
        if font.panose_family().is_none_or(|p| (3..=5).contains(&p)) {
            self.autohint = false;
            return;
        }
        if font.subtable(3, 1).is_none() && font.subtable(3, 10).is_none() {
            return;
        }
        let [r1, r2, _r3] = self.ranges;
        let r1 = if r1 == 0 { 1 } else { r1 };
        // capitals, round capital, x-height letters and round small letter of the font's first alphabet
        let latin: Option<RefChars> = if r1 & 1 != 0 {
            Some((108, &[66, 69, 72, 84, 90], 79, &[119, 120, 121, 122], 111))
        } else if r1 & 0x200 != 0 {
            Some((1048, &[1045], 1054, &[1083], 1086))
        } else if r1 & 0x80 != 0 {
            Some((921, &[917], 927, &[960], 959))
        } else {
            None
        };
        if let Some((stem, caps, round, small, round_small)) = latin {
            if !self.first(font, stem) {
                self.autohint = false;
                return;
            }
            let (mut top, mut base) = (i32::MAX, -i32::MAX);
            for &c in caps {
                if let Some((lo, hi)) = self.measure(font, c, false) {
                    top = top.min(hi);
                    base = base.max(lo);
                }
            }
            if top != i32::MAX {
                self.g.refs[13] = top;
            }
            if base != -i32::MAX {
                self.g.refs[17] = base;
            }
            if let Some((lo, hi)) = self.measure(font, round, false) {
                self.g.refs[14] = hi;
                self.g.refs[18] = lo;
            }
            let mut x = i32::MAX;
            for &c in small {
                if let Some((_, hi)) = self.measure(font, c, false) {
                    x = x.min(hi);
                }
            }
            if x != i32::MAX {
                self.g.refs[15] = x;
            }
            if let Some((_, hi)) = self.measure(font, round_small, false) {
                self.g.refs[16] = hi;
            }
        }
        let r = &mut self.g.refs;
        if r[20] == 0 {
            r[20] = 64;
        }
        if !(r[14] >= r[13] && r[13] >= r[16] && r[16] >= r[15] && r[15] >= r[17] && r[17] >= r[18]) {
            self.autohint = false;
            return;
        }
        if r[13] | r[14] == 0 {
            if let Some((lo, _)) = self.measure(font, 49, false) {
                self.g.refs[17] = lo;
            }
            if let Some((_, hi)) = self.measure(font, 55, false) {
                self.g.refs[13] = hi;
            }
            if let Some((lo, hi)) = self.measure(font, 48, false) {
                self.g.refs[14] = hi;
                self.g.refs[18] = lo;
            }
            let r = &mut self.g.refs;
            (r[15], r[16]) = (r[13], r[14]);
        }
        if r2 & 0x800_0000 != 0 {
            if !self.first(font, 20008) {
                self.autohint = false;
                return;
            }
            let (mut n, mut top, mut bottom) = (0, 0, 0);
            for c in [38728, 38662] {
                if self.measure(font, c, true).is_some() {
                    n += 1;
                    top += self.g.top;
                    bottom += self.g.bottom;
                }
            }
            let (lo, hi) = self.measure(font, 26519, true).unwrap_or((i32::MAX, i32::MAX));
            let r = &mut self.g.refs;
            (r[23], r[24]) = (hi, lo);
            (r[21], r[22]) = if n != 0 { (top / n, bottom / n) } else { (i32::MAX, i32::MAX) };
        }
        if r2 & 0x100_0000 != 0 {
            if let Some((lo, hi)) = self.measure(font, 54512, false) {
                (self.g.refs[25], self.g.refs[26]) = (hi, lo);
            }
            if let Some((lo, hi)) = self.measure(font, 44032, false) {
                (self.g.refs[27], self.g.refs[28]) = (hi, lo);
            }
        }
        self.other_scripts(font);
    }
}

impl Hinter {
    /// Store a letter's top and / or bottom in refs when the font has it; whether it did.
    fn put(&mut self, font: &Font, ch: u32, top: Option<usize>, bottom: Option<usize>) -> Option<(i32, i32)> {
        let m = self.measure(font, ch, false);
        if let Some((lo, hi)) = m {
            if let Some(i) = top {
                self.g.refs[i] = hi;
            }
            if let Some(i) = bottom {
                self.g.refs[i] = lo;
            }
        }
        m
    }

    /// The rest of get_ref_lines: each Indic script, Thai, Khmer and Arabic the OS/2 ranges name, from its
    /// letters' tops and bottoms (Kannada has none; Thai's lines are measured though rtgah fits Thai to
    /// Sinhala's).
    fn other_scripts(&mut self, font: &Font) {
        let [r1, r2, r3] = self.ranges;
        let r1 = if r1 == 0 { 1 } else { r1 };
        if r1 & 0x8000 != 0 {
            self.put(font, 0x93E, Some(29), Some(30));
        }
        if r1 & 0x10000 != 0 {
            self.put(font, 0x995, Some(31), Some(32));
        }
        if r1 & 0x20000 != 0 {
            self.put(font, 0xA38, Some(33), Some(34));
        }
        if r1 & 0x40000 != 0 {
            self.put(font, 0xA9F, Some(60), Some(62));
            let m = self.measure(font, 0xABE, false);
            let r = &mut self.g.refs;
            if let Some((_, hi)) = m {
                r[63] = 0;
                r[61] = hi;
            }
            if r[61] > r[60] {
                r.swap(60, 61);
            }
        }
        if r1 & 0x10_0000 != 0 {
            self.put(font, 0xBAA, Some(42), Some(44));
            self.put(font, 0xBB1, Some(41), None);
            self.put(font, 0xB95, None, Some(43));
        }
        if r1 & 0x20_0000 != 0 {
            self.put(font, 0xC05, Some(35), Some(36));
        }
        if r1 & 0x80_0000 != 0 {
            self.put(font, 0xD20, Some(37), Some(39));
            self.put(font, 0xD1A, Some(38), Some(40));
        }
        if r1 & 0x8_0000 != 0 {
            self.put(font, 0xB20, Some(45), Some(47));
            if let Some((lo, hi)) = self.measure(font, 0xB3E, false) {
                let r = &mut self.g.refs;
                r[48] = 0;
                r[46] = if lo != 0 { r[45] } else { hi };
            }
        }
        if r3 & 0x200 != 0 {
            let m = self.measure(font, 0xDA7, false);
            let r = &mut self.g.refs;
            (r[49], r[50], r[52]) = (0, 0, 0);
            if let Some((lo, _)) = m {
                r[51] = lo;
            }
        }
        if r1 & 0x100_0000 != 0 {
            self.put(font, 0xE01, Some(53), None);
            self.put(font, 0xE40, Some(54), None);
            self.put(font, 0xE07, None, Some(55));
            self.put(font, 0xE1A, None, Some(56));
        }
        if r3 & 0x10000 != 0 {
            self.put(font, 0x1783, None, Some(59));
            self.put(font, 0x1791, Some(57), Some(58));
        }
        if r1 & 0x2000 != 0 || r2 & 0x8000_0000 != 0 || r3 & 8 != 0 {
            self.put(font, 0x62A, None, Some(64));
        }
    }
}

/// The unit matrix (16.16).
const IDENTITY: [i32; 4] = [0x10000, 0, 0, 0x10000];

/// Where a composite glyph's assembly stands (get_comp's state in the key): whether a component ran a program
/// of its own (do_hints, +109 bit 0), whether a scaled component was met (+528 cleared, so every component from
/// it on is transformed), the advance points the glyph takes (the glyph's own, or a USE_MY_METRICS
/// component's) and the simple components placed so far (+518).
struct Assembly {
    ran: bool,
    transform: bool,
    metrics: [(i32, i32); 4],
    simple: usize,
}

/// compose_trans: a component's matrix `c` under its composite's `p` (16.16 xx, xy, yx, yy).
fn compose(c: [i32; 4], p: [i32; 4]) -> [i32; 4] {
    let mul = |a: i32, b: i32| varmul(a, b, 16);
    [
        mul(p[0], c[0]) + mul(p[1], c[2]),
        mul(p[0], c[1]) + mul(p[1], c[3]),
        mul(p[2], c[0]) + mul(p[3], c[2]),
        mul(p[2], c[1]) + mul(p[3], c[3]),
    ]
}

/// apply_transformation at the glyph cache's unit transform (+532 / +536 = 1.0): every point by the matrix,
/// the originals following; an advance the matrix turned round swaps the first two advance points.
fn transform(e: &mut Element, m: [i32; 4]) {
    for (x, y) in e.x.iter_mut().zip(e.y.iter_mut()) {
        (*x, *y) = (varmul(*x, m[0], 16) + varmul(*y, m[2], 16), varmul(*x, m[1], 16) + varmul(*y, m[3], 16));
    }
    e.ox.clone_from(&e.x);
    e.oy.clone_from(&e.y);
    let n = e.points();
    if (e.x[n + 1] ^ e.oox[n + 1]) < 0 {
        e.x.swap(n, n + 1);
        e.y.swap(n, n + 1);
    }
}

/// do_hints for a composite glyph: its points and its first advance point back in font units for its hinting
/// (the components' offsets included), x upem / ppem.
fn to_font_units(e: &mut Element, upem: u16, ppem: i32) {
    let k = vardiv(i32::from(upem) << 10, ppem, 16);
    for i in 0..=e.points() {
        e.oox[i] = varmul(e.x[i], k, 16);
        e.ooy[i] = varmul(e.y[i], k, 16);
    }
}

/// A rasterized glyph as the glyph cache stores it (sub_143070, from the maps AcquireGlyphmap trims): plain, or
/// with an outline width the glyph grown by `ow` px less half of the outline alone (outline flags 2 and 1), so
/// full coverage is the fill and half the edge. The cell takes the grown map's width and the taller map's
/// height, each row of the outline read at its own map's stride (the two are the same size in practice).
pub fn cache_cell(raw: &Graymap, ow: u16) -> Graymap {
    if ow == 0 {
        return effect::trim(raw.clone());
    }
    let filled = effect::trim(effect::outline(raw, ow, 0x10000, false));
    let ring = effect::trim(effect::outline(raw, ow, 0x10000, true));
    let (fw, fh) = (filled.width as usize, filled.height as usize);
    let (rw, rh) = (ring.width as usize, ring.height as usize);
    let h = fh.max(rh);
    let mut bits = vec![0u8; fw * h];
    for r in 0..h {
        for c in 0..fw {
            let f = if r < fh { filled.bits[r * fw + c] } else { 0 };
            let o = if r < rh { ring.bits.get(r * rw + c).map_or(0, |v| v >> 1) } else { 0 };
            bits[r * fw + c] = f.wrapping_sub(o);
        }
    }
    Graymap { height: h as i16, bits, ..filled }
}

/// extract_outline's TrueType path (0x193180 with fill_outline, 0x193eb0): the glyph's contours as moves, lines and
/// quadratic curves, 16.16. A contour of one point is left out; one starting off the curve starts on its second
/// point, or halfway to it (rounded down to a whole 26.6 unit) when that is off the curve too, and every contour
/// closes on its start. The first segment type carries 0x80; the box is that of all the points, control points
/// included. A glyph of fewer than three points has no outline.
pub fn extract(e: &Element) -> Outline {
    let mut o = Outline::default();
    if e.points() < 3 {
        return o;
    }
    let p = |i: usize| (e.x[i], e.y[i]);
    let on = |i: usize| e.on[i] & 1 != 0;
    let full = |(x, y): (i32, i32)| (x << 10, y << 10);
    let mid = |(ax, ay): (i32, i32), (bx, by): (i32, i32)| (ax.wrapping_add(bx) << 9, ay.wrapping_add(by) << 9);
    let seg = |o: &mut Outline, t: u8, pts: &[(i32, i32)]| {
        o.types.push(t);
        for &(x, y) in pts {
            o.x.push(x);
            o.y.push(y);
        }
    };
    for (&sp, &ep) in e.sp.iter().zip(&e.ep) {
        let (sp, ep) = (usize::from(sp), usize::from(ep));
        if sp == ep {
            continue;
        }
        let (start, mut prev, mut prev_on, from) = if on(sp) {
            (full(p(sp)), p(sp), true, sp + 1)
        } else if on(sp + 1) {
            (full(p(sp + 1)), p(sp + 1), true, sp + 2)
        } else {
            let (x, y) = mid(p(sp), p(sp + 1));
            ((x & !0x3FF, y & !0x3FF), p(sp + 1), false, sp + 2)
        };
        seg(&mut o, 0, &[start]);
        for i in from..=ep {
            let (cur, cur_on) = (p(i), on(i));
            if !prev_on {
                seg(&mut o, 2, &[full(prev), if cur_on { full(cur) } else { mid(prev, cur) }]);
            } else if cur_on {
                seg(&mut o, 1, &[full(cur)]);
            }
            (prev, prev_on) = (cur, cur_on);
        }
        if on(sp) {
            if prev_on {
                seg(&mut o, 1, &[start]);
            } else {
                seg(&mut o, 2, &[full(prev), start]);
            }
        } else {
            if !prev_on {
                seg(&mut o, 2, &[full(prev), mid(prev, p(sp))]);
            }
            seg(&mut o, 2, &[full(p(sp)), start]);
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
