//! nn::fontll's OTF kerning table (ScalableFontEngine::InitializeOtfKerningTable, sdk 0x16781c), which nn::font's
//! glyph cache kerns a scalable font's glyph pairs with: PickKerningFromGposTable (sdk 0x166860) flattens the
//! GPOS `kern` feature's pair adjustments and, unless the cache turns them off, the `palt` feature's single
//! adjustments into one array of glyph pairs, sorts it by (first, second) with libc++'s std::sort, and the
//! Acquire*OtfKerning calls bisect it.
//!
//! What it reads differs from a shaper's: the feature comes from the first `latn` / `kana` / `DFLT` script's
//! default language system (else its first one); a lookup must be of type 2 (`palt`: 1), so an extension
//! lookup adds nothing; a class pair subtable never reads its coverage and gives pairs only to the glyphs its
//! class definitions list, so a covered glyph left to class 0 (Helvetica Neue's A) kerns with nothing; a
//! pair listed by two subtables keeps both entries, the bisection finding one of them.

fn u16_at(d: &[u8], at: usize) -> u16 {
    d.get(at..at + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A table entry (OtfKerningTable::KerningPair): a glyph pair's x advance and x placement adjustments in font
/// units; `second` 0xFFFF holds a `palt` adjustment of `first` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub first: u16,
    pub second: u16,
    pub advance: i16,
    pub placement: i16,
}

/// A font's kerning table and its scale (1 / units per em).
pub struct Kerning {
    pub pairs: Vec<Pair>,
    scale: f32,
}

/// The second glyph of a `palt` entry.
const SINGLE: u16 = 0xFFFF;

impl Kerning {
    /// The table of a font file (sfnt), with the `palt` adjustments unless `kern_only`; none without a GPOS
    /// table (FSS_get_table fails, and the cache asks the font's `kern` table alone).
    pub fn new(data: &[u8], kern_only: bool) -> Option<Kerning> {
        let tables = usize::from(u16_at(data, 4));
        let find = |tag: &[u8; 4]| (0..tables).map(|i| 12 + 16 * i).find(|&r| data.get(r..r + 4) == Some(tag)).map(|r| u32_at(data, r + 8) as usize);
        let gpos = data.get(find(b"GPOS")?..)?;
        let upem = u16_at(data, find(b"head")? + 18);
        let mut pairs = Vec::new();
        let (scripts, features, lookups) = (u16_at(gpos, 4), u16_at(gpos, 6), u16_at(gpos, 8));
        if let Some(kern) = feature(gpos, scripts, features, b"kern") {
            kern_values(&mut pairs, gpos, usize::from(features) + usize::from(kern), usize::from(lookups));
        }
        if !kern_only && let Some(palt) = feature(gpos, scripts, features, b"palt") {
            palt_values(&mut pairs, gpos, usize::from(features) + usize::from(palt), usize::from(lookups));
        }
        sort(&mut pairs);
        Some(Kerning { pairs, scale: 1.0 / f32::from(upem) })
    }

    /// The entry of (first, second) as the SDK's bisection finds it: halving [lo, hi] by C's division and
    /// stopping at the first probe that matches.
    fn find(&self, first: u16, second: u16) -> Option<Pair> {
        let (mut lo, mut hi) = (0i32, self.pairs.len() as i32 - 1);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let p = self.pairs[mid as usize];
            if (p.first, p.second) > (first, second) {
                hi = mid - 1;
            } else if (p.first, p.second) < (first, second) {
                lo = mid + 1;
            } else {
                return Some(p);
            }
        }
        None
    }

    /// AcquireRawOtfKerning (sdk 0x16799c): the pair's x advance plus the `palt` advance less placement of the
    /// first glyph and the `palt` placement of the second, in font units.
    pub fn raw(&self, a: u16, b: u16) -> i32 {
        let first = self.find(a, SINGLE).map_or(0, |p| i32::from(p.advance) - i32::from(p.placement));
        let second = self.find(b, SINGLE).map_or(0, |p| i32::from(p.placement));
        let pair = self.find(a, b).map_or(0, |p| i32::from(p.advance));
        i32::from((pair + first + second) as i16)
    }

    /// AcquireRawOtfKerningFirst (sdk 0x167c6c): a line's first glyph's `palt` placement.
    pub fn raw_first(&self, g: u16) -> i32 {
        self.find(g, SINGLE).map_or(0, |p| i32::from(p.placement))
    }

    /// AcquireRawOtfKerningLast (sdk 0x167e08): a line's last glyph's `palt` advance less placement.
    pub fn raw_last(&self, g: u16) -> i32 {
        self.find(g, SINGLE).map_or(0, |p| i32::from((i32::from(p.advance) - i32::from(p.placement)) as i16))
    }

    /// Font units to whole pixels at `size` px as AcquireOtfKerning rounds them: floor(units / em * size + 0.5)
    /// in single precision.
    pub fn px(&self, units: i32, size: u32) -> i32 {
        (self.scale * units as f32 * size as f32 + 0.5).floor() as i32
    }
}

/// The number of slots of the font's cmap cache (load_fnt, sdk 0x16ca90): the power of two nearest a tenth of its
/// glyph count, at least 256 (a tie takes the larger). map_char (sdk 0x177ee0) keeps the last BMP character it
/// mapped at slot (16807 c) mod that number, in a zeroed table, so U+0000 finds its empty slot's glyph 0 and
/// counts as missing until a character at slot 0 (a multiple of the number) has been mapped.
pub fn cmap_cache_size(data: &[u8]) -> u32 {
    let tables = usize::from(u16_at(data, 4));
    let glyphs = (0..tables).map(|i| 12 + 16 * i).find(|&r| data.get(r..r + 4) == Some(b"maxp")).map_or(0, |r| u16_at(data, u32_at(data, r + 8) as usize + 4));
    let want = u32::from(glyphs / 10).max(256);
    let (mut below, mut above) = (1u32, 1u32);
    while want > above {
        below = above;
        above *= 2;
    }
    if want == above || want - below >= above - want { above } else { below }
}

/// The font's `kern` table value for a glyph pair in font units, as FSS_get_kerning (sdk 0x191370) finds it:
/// load_kerning takes the first subtable of version 0 and reads it as format 0 pairs, bisected.
pub fn kern_table(data: &[u8], a: u16, b: u16) -> i16 {
    let tables = usize::from(u16_at(data, 4));
    let Some(kern) = (0..tables).map(|i| 12 + 16 * i).find(|&r| data.get(r..r + 4) == Some(b"kern")).map(|r| u32_at(data, r + 8) as usize) else {
        return 0;
    };
    let mut sub = kern + 4;
    let mut found = None;
    for _ in 0..u16_at(data, kern + 2) {
        if u16_at(data, sub) == 0 {
            found = Some(sub + 6);
            break;
        }
        sub += usize::from(u16_at(data, sub + 2));
    }
    let Some(t) = found else { return 0 };
    let (mut lo, mut hi) = (0i32, i32::from(u16_at(data, t)) - 1);
    while lo <= hi {
        let mid = ((lo + hi) as u16 >> 1) as i32;
        let e = t + 8 + 6 * mid as usize;
        match (u16_at(data, e), u16_at(data, e + 2)).cmp(&(a, b)) {
            std::cmp::Ordering::Greater => hi = mid - 1,
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Equal => return u16_at(data, e + 4) as i16,
        }
    }
    0
}

/// A `kern` table value in whole pixels at `size` px as the glyph cache takes it: FSS_get_kerning scales it to
/// 16.16 pixels by muldiv_64 (rounding half away from zero) and the cache drops the fraction (an arithmetic
/// shift, so toward minus infinity).
pub fn kern_table_px(units: i16, upem: u16, size: u32) -> i32 {
    let (a, b, c) = (i64::from(size) << 16, i64::from(units), i64::from(upem));
    if b == 0 || c == 0 {
        return 0;
    }
    let q = ((c >> 1) + a * b.abs()) / c;
    let v = q.min(0x7FFF_FFFF) as i32;
    (if b < 0 { -v } else { v }) as f32 as i32 >> 16
}

/// PickFeatureTableOffset (sdk 0x164fd0): the offset in the feature list of the feature `tag` of the first
/// script tagged `latn`, `kana` or `DFLT`, from its default language system, else its first one.
fn feature(gpos: &[u8], scripts: u16, features: u16, tag: &[u8; 4]) -> Option<u16> {
    let list = usize::from(scripts);
    let script = (0..usize::from(u16_at(gpos, list))).map(|i| list + 2 + 6 * i).find_map(|r| {
        if !matches!(gpos.get(r..r + 4), Some(b"latn" | b"kana" | b"DFLT")) {
            return None;
        }
        let script = list + usize::from(u16_at(gpos, r + 4));
        match u16_at(gpos, script) {
            0 if u16_at(gpos, script + 2) == 0 => None,
            0 => Some((script, u16_at(gpos, script + 8))),
            sys => Some((script, sys)),
        }
    });
    let (script, lang) = script?;
    let lang = script + usize::from(lang);
    let flist = usize::from(features);
    (0..usize::from(u16_at(gpos, lang + 4)))
        .map(|i| flist + 2 + 6 * usize::from(u16_at(gpos, lang + 6 + 2 * i)))
        .find(|&r| gpos.get(r..r + 4) == Some(tag))
        .map(|r| u16_at(gpos, r + 4))
}

/// The subtables of the feature at `feature`'s lookups of type `kind` (with any subtables), in order.
fn subtables(gpos: &[u8], feature: usize, lookups: usize, kind: u16) -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..usize::from(u16_at(gpos, feature + 2)) {
        let lookup = lookups + usize::from(u16_at(gpos, lookups + 2 + 2 * usize::from(u16_at(gpos, feature + 4 + 2 * i))));
        if u16_at(gpos, lookup) != kind || u16_at(gpos, lookup + 4) == 0 {
            continue;
        }
        out.extend((0..usize::from(u16_at(gpos, lookup + 4))).map(|j| lookup + usize::from(u16_at(gpos, lookup + 6 + 2 * j))));
    }
    out
}

/// A value record of `format` at `at`: (x placement, x advance, its size in bytes).
fn value(gpos: &[u8], at: usize, format: u16) -> (i16, i16, usize) {
    let field = |bit: u16| (0..bit.trailing_zeros()).filter(|&b| format & (1 << b) != 0).count() * 2;
    let place = if format & 1 != 0 { u16_at(gpos, at) as i16 } else { 0 };
    let advance = if format & 4 != 0 { u16_at(gpos, at + field(4)) as i16 } else { 0 };
    (place, advance, format.count_ones() as usize * 2)
}

/// The glyph at `index` of the coverage table at `cov` as the SDK walks it, 0 past its end.
fn covered(gpos: &[u8], cov: usize, index: usize) -> u16 {
    let count = usize::from(u16_at(gpos, cov + 2));
    if u16_at(gpos, cov) == 1 {
        return if index < count { u16_at(gpos, cov + 4 + 2 * index) } else { 0 };
    }
    let mut left = index;
    for r in 0..count {
        let (start, end) = (u16_at(gpos, cov + 4 + 6 * r), u16_at(gpos, cov + 6 + 6 * r));
        if start > end {
            continue;
        }
        let n = usize::from(end - start) + 1;
        if left < n {
            return start + left as u16;
        }
        left -= n;
    }
    0
}

/// The glyphs a class definition table lists in class `class`, in its order (class 0 only as listed).
fn class_glyphs(gpos: &[u8], def: usize, class: u16) -> Vec<u16> {
    if u16_at(gpos, def) == 1 {
        let start = u16_at(gpos, def + 2);
        return (0..u16_at(gpos, def + 4)).filter(|&i| u16_at(gpos, def + 6 + 2 * usize::from(i)) == class).map(|i| start.wrapping_add(i)).collect();
    }
    let mut out = Vec::new();
    for r in 0..usize::from(u16_at(gpos, def + 2)) {
        let (start, end, c) = (u16_at(gpos, def + 4 + 6 * r), u16_at(gpos, def + 6 + 6 * r), u16_at(gpos, def + 8 + 6 * r));
        if c == class && start <= end {
            out.extend(start..=end);
        }
    }
    out
}

/// PickKernXValues (sdk 0x165ed0): the pair adjustment subtables' entries with an x advance or placement.
fn kern_values(out: &mut Vec<Pair>, gpos: &[u8], feature: usize, lookups: usize) {
    for st in subtables(gpos, feature, lookups, 2) {
        let (f1, f2) = (u16_at(gpos, st + 4), u16_at(gpos, st + 6));
        match u16_at(gpos, st) {
            1 => {
                let cov = st + usize::from(u16_at(gpos, st + 2));
                for i in 0..usize::from(u16_at(gpos, st + 8)) {
                    let first = covered(gpos, cov, i);
                    let set = st + usize::from(u16_at(gpos, st + 10 + 2 * i));
                    let mut at = set + 2;
                    for _ in 0..u16_at(gpos, set) {
                        let (place, advance, size) = value(gpos, at + 2, f1);
                        if advance != 0 || place != 0 {
                            out.push(Pair { first, second: u16_at(gpos, at), advance, placement: place });
                        }
                        at += 2 + size + value(gpos, at + 2 + size, f2).2;
                    }
                }
            }
            2 => {
                let (def1, def2) = (st + usize::from(u16_at(gpos, st + 8)), st + usize::from(u16_at(gpos, st + 10)));
                let (n1, n2) = (u16_at(gpos, st + 12), u16_at(gpos, st + 14));
                let mut at = st + 16;
                for c1 in 0..n1 {
                    for c2 in 0..n2 {
                        let (place, advance, size) = value(gpos, at, f1);
                        at += size + value(gpos, at + size, f2).2;
                        if advance == 0 && place == 0 {
                            continue;
                        }
                        let seconds = class_glyphs(gpos, def2, c2);
                        for first in class_glyphs(gpos, def1, c1) {
                            out.extend(seconds.iter().map(|&second| Pair { first, second, advance, placement: place }));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// PickPaltXValues (sdk 0x1666b0): the single adjustment subtables' entries, for as many glyphs as the coverage
/// table's count field says (its range count for a range coverage).
fn palt_values(out: &mut Vec<Pair>, gpos: &[u8], feature: usize, lookups: usize) {
    for st in subtables(gpos, feature, lookups, 1) {
        let cov = st + usize::from(u16_at(gpos, st + 2));
        let format = u16_at(gpos, st + 4);
        let count = usize::from(u16_at(gpos, cov + 2));
        match u16_at(gpos, st) {
            1 => {
                let (place, advance, _) = value(gpos, st + 6, format);
                if advance != 0 || place != 0 {
                    out.extend((0..count).map(|i| Pair { first: covered(gpos, cov, i), second: SINGLE, advance, placement: place }));
                }
            }
            2 => {
                let mut at = st + 8;
                for i in 0..count {
                    let (place, advance, size) = value(gpos, at, format);
                    at += size;
                    if advance != 0 || place != 0 {
                        out.push(Pair { first: covered(gpos, cov, i), second: SINGLE, advance, placement: place });
                    }
                }
            }
            _ => {}
        }
    }
}

/// CompareKerningPair (sdk 0x166820).
fn less(a: &Pair, b: &Pair) -> bool {
    (a.first, a.second) < (b.first, b.second)
}

/// libc++'s std::__sort as the SDK has it (sdk 0x1669a0): insertion sort up to 30 elements, else a median of
/// three (five from 1000 elements) and a partition that leaves equal keys where it finds them, so a pair two
/// subtables list ends up in the same order as on the console.
fn sort(v: &mut [Pair]) {
    let (mut first, mut last) = (0usize, v.len());
    loop {
        let len = last - first;
        match len {
            0 | 1 => return,
            2 => {
                if less(&v[last - 1], &v[first]) {
                    v.swap(first, last - 1);
                }
                return;
            }
            3 => {
                sort3(v, first, first + 1, last - 1);
                return;
            }
            4 => {
                sort4(v, first, first + 1, first + 2, last - 1);
                return;
            }
            5 => {
                sort5(v, first, first + 1, first + 2, first + 3, last - 1);
                return;
            }
            _ => {}
        }
        if len <= 30 {
            insertion_sort_3(v, first, last);
            return;
        }
        let mut m = first + len / 2;
        let lm1 = last - 1;
        let mut swaps = if len >= 1000 {
            let delta = len / 2 / 2;
            sort5(v, first, first + delta, m, m + delta, lm1)
        } else {
            sort3(v, first, m, lm1)
        };
        let mut i = first;
        let mut j = lm1;
        let mut restart = false;
        if !less(&v[i], &v[m]) {
            loop {
                j -= 1;
                if i == j {
                    i += 1;
                    j = last - 1;
                    if !less(&v[first], &v[j]) {
                        loop {
                            if i == j {
                                return;
                            }
                            if less(&v[first], &v[i]) {
                                v.swap(i, j);
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                    }
                    if i == j {
                        return;
                    }
                    loop {
                        while !less(&v[first], &v[i]) {
                            i += 1;
                        }
                        j -= 1;
                        while less(&v[first], &v[j]) {
                            j -= 1;
                        }
                        if i >= j {
                            break;
                        }
                        v.swap(i, j);
                        i += 1;
                    }
                    first = i;
                    restart = true;
                    break;
                }
                if less(&v[j], &v[m]) {
                    v.swap(i, j);
                    swaps += 1;
                    break;
                }
            }
        }
        if restart {
            continue;
        }
        i += 1;
        if i < j {
            loop {
                while less(&v[i], &v[m]) {
                    i += 1;
                }
                j -= 1;
                while !less(&v[j], &v[m]) {
                    j -= 1;
                }
                if i > j {
                    break;
                }
                v.swap(i, j);
                swaps += 1;
                if m == i {
                    m = j;
                }
                i += 1;
            }
        }
        if i != m && less(&v[m], &v[i]) {
            v.swap(i, m);
            swaps += 1;
        }
        if swaps == 0 {
            let head = insertion_sort_incomplete(v, first, i);
            if insertion_sort_incomplete(v, i + 1, last) {
                if head {
                    return;
                }
                last = i;
                continue;
            } else if head {
                first = i + 1;
                continue;
            }
        }
        if i - first < last - i {
            sort(&mut v[first..i]);
            first = i + 1;
        } else {
            sort(&mut v[i + 1..last]);
            last = i;
        }
    }
}

fn sort3(v: &mut [Pair], x: usize, y: usize, z: usize) -> u32 {
    if !less(&v[y], &v[x]) {
        if !less(&v[z], &v[y]) {
            return 0;
        }
        v.swap(y, z);
        if less(&v[y], &v[x]) {
            v.swap(x, y);
            return 2;
        }
        return 1;
    }
    if less(&v[z], &v[y]) {
        v.swap(x, z);
        return 1;
    }
    v.swap(x, y);
    if less(&v[z], &v[y]) {
        v.swap(y, z);
        return 2;
    }
    1
}

fn sort4(v: &mut [Pair], x1: usize, x2: usize, x3: usize, x4: usize) -> u32 {
    let mut r = sort3(v, x1, x2, x3);
    if less(&v[x4], &v[x3]) {
        v.swap(x3, x4);
        r += 1;
        if less(&v[x3], &v[x2]) {
            v.swap(x2, x3);
            r += 1;
            if less(&v[x2], &v[x1]) {
                v.swap(x1, x2);
                r += 1;
            }
        }
    }
    r
}

fn sort5(v: &mut [Pair], x1: usize, x2: usize, x3: usize, x4: usize, x5: usize) -> u32 {
    let mut r = sort4(v, x1, x2, x3, x4);
    if less(&v[x5], &v[x4]) {
        v.swap(x4, x5);
        r += 1;
        if less(&v[x4], &v[x3]) {
            v.swap(x3, x4);
            r += 1;
            if less(&v[x3], &v[x2]) {
                v.swap(x2, x3);
                r += 1;
                if less(&v[x2], &v[x1]) {
                    v.swap(x1, x2);
                    r += 1;
                }
            }
        }
    }
    r
}

/// Insert v[i] into the sorted v[first..i] (the hole moving down from `i`).
fn insert(v: &mut [Pair], first: usize, i: usize) {
    let t = v[i];
    let mut j = i;
    while j > first && less(&t, &v[j - 1]) {
        v[j] = v[j - 1];
        j -= 1;
    }
    v[j] = t;
}

fn insertion_sort_3(v: &mut [Pair], first: usize, last: usize) {
    sort3(v, first, first + 1, first + 2);
    for i in first + 3..last {
        if less(&v[i], &v[i - 1]) {
            insert(v, first, i);
        }
    }
}

/// Whether the range is sorted after at most eight insertions.
fn insertion_sort_incomplete(v: &mut [Pair], first: usize, last: usize) -> bool {
    match last - first {
        0 | 1 => return true,
        2 => {
            if less(&v[last - 1], &v[first]) {
                v.swap(first, last - 1);
            }
            return true;
        }
        3 => {
            sort3(v, first, first + 1, last - 1);
            return true;
        }
        4 => {
            sort4(v, first, first + 1, first + 2, last - 1);
            return true;
        }
        5 => {
            sort5(v, first, first + 1, first + 2, first + 3, last - 1);
            return true;
        }
        _ => {}
    }
    sort3(v, first, first + 1, first + 2);
    let mut count = 0;
    for i in first + 3..last {
        if less(&v[i], &v[i - 1]) {
            insert(v, first, i);
            count += 1;
            if count == 8 {
                return i + 1 == last;
            }
        }
    }
    true
}
