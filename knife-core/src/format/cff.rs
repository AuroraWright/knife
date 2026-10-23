//! CFF charstring clean-up for the OpenType fonts the game ships. nn::fontll ends a glyph at its `endchar`
//! (directly or inside a subroutine) and ignores what follows, and several of the romfs fonts carry bytes
//! there: the overseas Helvetica (en: ö, à, ’, ”, — ...), DemosNext (en2: A-E) and both FOT-Seurat builds
//! (ja3 / en3: o, e, t, s, ...). ttf-parser rejects such a glyph whole (`DataAfterEndChar`), so `normalize`
//! moves the ending operator with its operands to the end of its charstring or subroutine and fills the
//! freed bytes with bare `hstem` operators, which with an empty stack add no stems and draw nothing. The
//! outline is then the one the Switch draws.

use std::collections::HashMap;
use std::ops::Range;

const HSTEM: u8 = 1;
const CALLSUBR: u8 = 10;
const RETURN: u8 = 11;
const ESCAPE: u8 = 12;
const ENDCHAR: u8 = 14;
const HINTMASK: u8 = 19;
const CNTRMASK: u8 = 20;
const CALLGSUBR: u8 = 29;

/// Rewrite the `CFF ` table of an sfnt in place (no-op for other fonts or on malformed data). Returns how
/// many charstrings and subroutines changed.
pub fn normalize(data: &mut [u8]) -> usize {
    let Some(cff) = cff_range(data) else { return 0 };
    let Some(font) = Cff::parse(&data[cff.clone()]) else { return 0 };
    let base = cff.start;
    let mut fixes: HashMap<usize, Fix> = HashMap::new();
    for (gid, cs) in font.char_strings.iter().enumerate() {
        let local = font.local_subrs(gid);
        let mut w = Walker { cff: &data[cff.clone()], font: &font, local, stems: 0, fixes: &mut fixes, budget: 10_000 };
        w.walk(cs.clone(), 0, &mut Vec::new());
    }
    let mut n = 0;
    for (_, f) in fixes {
        let Some(f) = f else { continue };
        let buf = &mut data[base + f.buf.start..base + f.buf.end];
        let (head, tail) = (f.keep - f.buf.start, f.end - f.buf.start);
        // [head, tail) is the ending operator with its operands; everything after it is dead
        let moved = buf[head..tail].to_vec();
        let pad = buf.len() - tail;
        buf[head..head + pad].fill(HSTEM);
        buf[head + pad..].copy_from_slice(&moved);
        n += 1;
    }
    n
}

/// The ending operator of a charstring or subroutine: `keep` is where its operands start (the stack is
/// empty there), `end` just past the operator; `None` once two callers disagree.
#[derive(Clone)]
struct FixAt {
    buf: Range<usize>,
    keep: usize,
    end: usize,
}

type Fix = Option<FixAt>;

impl FixAt {
    fn same(&self, o: &FixAt) -> bool {
        self.buf == o.buf && self.keep == o.keep && self.end == o.end
    }
}

struct Walker<'a> {
    cff: &'a [u8],
    font: &'a Cff,
    local: Option<&'a [Range<usize>]>,
    stems: u32,
    fixes: &'a mut HashMap<usize, Fix>,
    budget: u32,
}

enum Exit {
    Return,
    End,
    Error,
}

impl Walker<'_> {
    /// Run one charstring or subroutine like ttf-parser does (operand counts for the stems and hint masks,
    /// subroutine calls). `stack` holds the pending operands (value, start offset or `usize::MAX` when they
    /// were pushed by a caller).
    fn walk(&mut self, buf: Range<usize>, depth: u32, stack: &mut Vec<(f64, usize)>) -> Exit {
        if depth > 10 || self.budget == 0 {
            return Exit::Error;
        }
        self.budget -= 1;
        let d = &self.cff[buf.clone()];
        // operands pushed by a caller are below anything pushed here
        let caller = stack.len();
        let mut keep = buf.start;
        let mut i = 0;
        let ended = |w: &mut Self, keep: usize, end: usize| {
            if end < buf.end {
                let at = FixAt { buf: buf.clone(), keep, end };
                // operands from the caller are on the stack at the buffer start: no room for the filler
                let ok = keep > buf.start || caller == 0;
                let e = w.fixes.entry(buf.start).or_insert(if ok { Some(at.clone()) } else { None });
                if !e.as_ref().is_some_and(|f| f.same(&at)) {
                    *e = None;
                }
            }
            Exit::End
        };
        while i < d.len() {
            let b = d[i];
            let start = buf.start + i;
            match b {
                28 => {
                    let Some(v) = d.get(i + 1..i + 3) else { return Exit::Error };
                    stack.push((i16::from_be_bytes([v[0], v[1]]) as f64, start));
                    i += 3;
                }
                32..=246 => {
                    stack.push((b as f64 - 139.0, start));
                    i += 1;
                }
                247..=254 => {
                    let Some(&b1) = d.get(i + 1) else { return Exit::Error };
                    let v = if b <= 250 { (b as f64 - 247.0) * 256.0 + b1 as f64 + 108.0 } else { -(b as f64 - 251.0) * 256.0 - b1 as f64 - 108.0 };
                    stack.push((v, start));
                    i += 2;
                }
                255 => {
                    let Some(v) = d.get(i + 1..i + 5) else { return Exit::Error };
                    stack.push((i32::from_be_bytes([v[0], v[1], v[2], v[3]]) as f64 / 65536.0, start));
                    i += 5;
                }
                CALLSUBR | CALLGSUBR => {
                    i += 1;
                    let Some((v, _)) = stack.pop() else { return Exit::Error };
                    let subrs: &[Range<usize>] = if b == CALLSUBR { self.local.unwrap_or(&[]) } else { &self.font.global_subrs };
                    let bias = match subrs.len() {
                        n if n < 1240 => 107,
                        n if n < 33900 => 1131,
                        _ => 32768,
                    };
                    let Some(sub) = usize::try_from(v as i64 + bias).ok().and_then(|k| subrs.get(k)) else { return Exit::Error };
                    match self.walk(sub.clone(), depth + 1, stack) {
                        Exit::Return => {}
                        Exit::End => return ended(self, keep, buf.start + i),
                        Exit::Error => return Exit::Error,
                    }
                    // the stack keeps whatever the subroutine left; operands start over only after an operator
                    if stack.is_empty() {
                        keep = buf.start + i;
                    }
                }
                RETURN => return Exit::Return,
                ENDCHAR => return ended(self, keep, start + 1),
                HINTMASK | CNTRMASK => {
                    self.stems += stack.len() as u32 >> 1;
                    stack.clear();
                    i += 1 + (self.stems as usize).div_ceil(8);
                    keep = buf.start + i.min(d.len());
                }
                ESCAPE => {
                    stack.clear();
                    i += 2;
                    keep = buf.start + i.min(d.len());
                }
                1 | 3 | 18 | 23 => {
                    self.stems += stack.len() as u32 >> 1;
                    stack.clear();
                    i += 1;
                    keep = buf.start + i;
                }
                _ => {
                    stack.clear();
                    i += 1;
                    keep = buf.start + i;
                }
            }
        }
        // ran off the end (ttf-parser stops the same way)
        Exit::Return
    }
}

/// The `CFF ` table of an sfnt (the first font of a collection).
fn cff_range(data: &[u8]) -> Option<Range<usize>> {
    let u16_at = |o: usize| Some(u16::from_be_bytes(data.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_be_bytes(data.get(o..o + 4)?.try_into().ok()?));
    let base = if data.get(..4)? == b"ttcf" { u32_at(12)? as usize } else { 0 };
    let n = u16_at(base + 4)? as usize;
    let rec = (0..n).map(|i| base + 12 + 16 * i).find(|&r| data.get(r..r + 4) == Some(b"CFF "))?;
    let (off, len) = (u32_at(rec + 8)? as usize, u32_at(rec + 12)? as usize);
    (off + len <= data.len()).then_some(off..off + len)
}

/// The parts of a CFF (version 1) font the charstrings need, as ranges into the table.
struct Cff {
    char_strings: Vec<Range<usize>>,
    global_subrs: Vec<Range<usize>>,
    /// the font's local subroutines (one entry) or each font dict's (CID-keyed)
    local: Vec<Vec<Range<usize>>>,
    /// CID-keyed: the font dict of each glyph
    fd_select: Option<Vec<u8>>,
}

impl Cff {
    fn parse(d: &[u8]) -> Option<Self> {
        let mut at = *d.get(2)? as usize;
        let (_, next) = index(d, at)?;
        at = next;
        let (top, next) = index(d, at)?;
        at = next;
        let (_, next) = index(d, at)?;
        at = next;
        let (global_subrs, _) = index(d, at)?;
        let top = dict(d.get(top.first()?.clone())?)?;
        let char_strings = index(d, *top.get(&17)?.first()? as usize)?.0;
        let local_of = |dict: &HashMap<u16, Vec<f64>>| -> Option<Vec<Range<usize>>> {
            let p = dict.get(&18)?;
            let (size, off) = (*p.first()? as usize, *p.get(1)? as usize);
            let private = self::dict(d.get(off..off + size)?)?;
            Some(index(d, off + *private.get(&19)?.first()? as usize)?.0)
        };
        let (local, fd_select) = match top.get(&(0x0c00 | 36)) {
            // CID-keyed: FDArray + FDSelect
            Some(fda) => {
                let (fds, _) = index(d, *fda.first()? as usize)?;
                let local = fds.iter().map(|r| d.get(r.clone()).and_then(dict).and_then(|f| local_of(&f)).unwrap_or_default()).collect();
                let sel = *top.get(&(0x0c00 | 37))?.first()? as usize;
                (local, Some(fd_select(d, sel, char_strings.len())?))
            }
            None => (vec![local_of(&top).unwrap_or_default()], None),
        };
        Some(Self { char_strings, global_subrs, local, fd_select })
    }

    fn local_subrs(&self, gid: usize) -> Option<&[Range<usize>]> {
        let fd = self.fd_select.as_ref().map_or(Some(0), |s| s.get(gid).map(|&f| f as usize))?;
        self.local.get(fd).map(Vec::as_slice)
    }
}

/// An INDEX at `at`: its objects and the offset just past it.
fn index(d: &[u8], at: usize) -> Option<(Vec<Range<usize>>, usize)> {
    let count = u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?) as usize;
    if count == 0 {
        return Some((Vec::new(), at + 2));
    }
    let size = *d.get(at + 2)? as usize;
    if !(1..=4).contains(&size) {
        return None;
    }
    let off = |k: usize| -> Option<usize> {
        let b = d.get(at + 3 + k * size..at + 3 + (k + 1) * size)?;
        Some(b.iter().fold(0usize, |a, &x| a << 8 | x as usize))
    };
    let data = at + 3 + (count + 1) * size - 1;
    let mut out = Vec::with_capacity(count);
    for k in 0..count {
        let (s, e) = (data + off(k)?, data + off(k + 1)?);
        if s > e || e > d.len() {
            return None;
        }
        out.push(s..e);
    }
    let end = data + off(count)?;
    Some((out, end))
}

/// A DICT: operator (escaped ones as 0x0c00 | op) -> operands.
fn dict(d: &[u8]) -> Option<HashMap<u16, Vec<f64>>> {
    let mut out = HashMap::new();
    let mut ops = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        match b {
            0..=21 => {
                let op = if b == ESCAPE {
                    i += 1;
                    0x0c00 | *d.get(i)? as u16
                } else {
                    b as u16
                };
                out.insert(op, std::mem::take(&mut ops));
                i += 1;
            }
            28 => {
                ops.push(i16::from_be_bytes(d.get(i + 1..i + 3)?.try_into().ok()?) as f64);
                i += 3;
            }
            29 => {
                ops.push(i32::from_be_bytes(d.get(i + 1..i + 5)?.try_into().ok()?) as f64);
                i += 5;
            }
            30 => {
                // real: nibbles up to 0xf (the value is not needed for any operator read here)
                i += 1;
                while i < d.len() && d[i] & 0x0f != 0x0f && d[i] >> 4 != 0x0f {
                    i += 1;
                }
                i += 1;
                ops.push(0.0);
            }
            32..=246 => {
                ops.push(b as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                ops.push((b as f64 - 247.0) * 256.0 + *d.get(i + 1)? as f64 + 108.0);
                i += 2;
            }
            251..=254 => {
                ops.push(-(b as f64 - 251.0) * 256.0 - *d.get(i + 1)? as f64 - 108.0);
                i += 2;
            }
            _ => return None,
        }
    }
    Some(out)
}

/// FDSelect formats 0 and 3: the font dict of each of `n` glyphs.
fn fd_select(d: &[u8], at: usize, n: usize) -> Option<Vec<u8>> {
    match *d.get(at)? {
        0 => Some(d.get(at + 1..at + 1 + n)?.to_vec()),
        3 => {
            let u16_at = |o: usize| Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?) as usize);
            let ranges = u16_at(at + 1)?;
            let mut out = vec![0u8; n];
            for r in 0..ranges {
                let o = at + 3 + r * 3;
                let (first, fd, next) = (u16_at(o)?, *d.get(o + 2)?, u16_at(o + 3)?);
                out.get_mut(first..next.min(n))?.fill(fd);
            }
            Some(out)
        }
        _ => None,
    }
}
