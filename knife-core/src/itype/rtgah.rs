//! iType's real-time grid-aligned autohinter (rtgah), which the SDK runs on every TrueType glyph without
//! instructions of its own: it finds the straight segments of the outline along each axis, pairs them into
//! stems, fits the stems and the script's reference lines to the pixel grid and interpolates the other points
//! (IUP). Ported from the JP sdk (rtgah 0x195760 and its helpers), keeping their order of work: the segment
//! point lists live in a bump arena as the SDK's do, and the record arrays have the SDK's limits.
// the loops index as the SDK's do, often several arrays by one index
#![allow(clippy::needless_range_loop)]

use super::fixed::{muldiv, normalize, vardiv, varmul};
use super::tt::{iup, Element};

/// No point / no list.
const NIL: u32 = u32::MAX;
/// A stem merged away or dropped (its width set to this).
const GONE: i32 = -0x7FFF_FFFF;

/// The scratch arena the point lists come from (globalGS p352 .. p360, 16 bytes a node).
pub struct Arena {
    pub nodes: Vec<Node>,
    pub cap: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub pt: u16,
    pub next: u32,
}

impl Arena {
    pub fn new(cap: usize) -> Arena {
        Arena { nodes: Vec::with_capacity(cap), cap }
    }

    pub fn alloc(&mut self, pt: u16, next: u32) -> u32 {
        self.nodes.push(Node { pt, next });
        (self.nodes.len() - 1) as u32
    }

    fn full(&self) -> bool {
        self.nodes.len() >= self.cap
    }

    /// Copy a list node by node, each new node pointing back at the one before (`tail` the first's), so the
    /// copy comes out reversed; returns its head (`tail` for an empty list).
    fn copy_onto(&mut self, mut src: u32, tail: u32) -> u32 {
        let mut prev = tail;
        while src != NIL {
            let n = self.nodes[src as usize];
            prev = self.alloc(n.pt, prev);
            src = n.next;
        }
        prev
    }

    pub fn iter(&self, mut at: u32) -> impl Iterator<Item = u16> + '_ {
        std::iter::from_fn(move || {
            (at != NIL).then(|| {
                let n = self.nodes[at as usize];
                at = n.next;
                n.pt
            })
        })
    }
}

/// A straight run of a contour's points along one axis (16-byte records, at most 127 kept).
#[derive(Clone, Copy, Debug, Default)]
pub struct Seg {
    pub start: u16,
    pub end: u16,
    pub contour: u16,
    /// +1 when the run goes up the other axis, else -1
    pub dir: i16,
    /// the points between, last first
    pub list: u32,
}

/// A stem: two segments of opposite direction facing each other (48-byte records, at most 64).
#[derive(Clone, Copy, Debug, Default)]
pub struct Stem {
    pub a: [u16; 2],
    pub b: [u16; 2],
    pub list_a: u32,
    pub list_b: u32,
    /// the middle between the segments, and where it is fitted to
    pub pos: i32,
    pub fit: i32,
    pub width: i32,
    /// the extent along the other axis
    pub lo: i32,
    pub hi: i32,
    pub flag: i32,
}

/// The coordinate across an axis's segments and the one along them: x and y for 'x', y and x for 'y'.
fn axes(e: &Element, x_axis: bool) -> (&Vec<i32>, &Vec<i32>) {
    if x_axis { (&e.x, &e.y) } else { (&e.y, &e.x) }
}

fn axes_mut(e: &mut Element, x_axis: bool) -> (&mut Vec<i32>, &mut Vec<i32>) {
    if x_axis { (&mut e.x, &mut e.y) } else { (&mut e.y, &mut e.x) }
}

/// Sort records by the across coordinate of their first point, an insertion sort keeping equal ones in order.
fn sort_by<T: Copy>(v: &mut [T], key: impl Fn(&T) -> i32) {
    for i in 1..v.len() {
        let r = v[i];
        let k = key(&r);
        let mut j = i;
        while j > 0 && key(&v[j - 1]) > k {
            v[j] = v[j - 1];
            j -= 1;
        }
        v[j] = r;
    }
}

/// The straight segments for a CJK glyph (sub_198590): runs of a contour's points within 3 units of the first
/// across the axis, kept when 22 times that spread is less than the run's length; a contour whose ends line up
/// starts its first run at the end point.
pub fn cjk_segments(x_axis: bool, e: &Element, arena: &mut Arena) -> Vec<Seg> {
    let (c, a) = axes(e, x_axis);
    let at = |v: &Vec<i32>, p: u16| v[usize::from(p)];
    let mut out: Vec<Seg> = Vec::new();
    'contours: for k in 0..e.sp.len() {
        let (sp, ep) = (e.sp[k], e.ep[k]);
        if sp >= ep {
            continue;
        }
        let mut start = if (at(c, sp) - at(c, ep)).abs() >= 4 { sp } else { ep };
        let mut i = sp;
        loop {
            while i != ep && (at(c, start) - at(c, i + 1)).abs() <= 3 {
                i += 1;
            }
            if start != i && (at(c, i) - at(c, start)).abs().wrapping_mul(22) < (at(a, i) - at(a, start)).abs() {
                let mut s = Seg { start, end: i, contour: k as u16, dir: 0, list: NIL };
                let mut p = if ep == start { sp } else { start + 1 };
                let mut prev = NIL;
                while p != i {
                    prev = arena.alloc(p, prev);
                    s.list = prev;
                    if arena.full() {
                        break;
                    }
                    p = if ep == p { sp } else { p + 1 };
                }
                s.dir = if at(a, i) > at(a, start) { 1 } else { -1 };
                out.push(s);
                if out.len() == 128 {
                    break 'contours;
                }
            }
            i += 1;
            start = i;
            if ep <= i {
                break;
            }
        }
    }
    if out.len() == 128 {
        out.pop();
    }
    sort_by(&mut out, |s| c[usize::from(s.start)]);
    out
}

/// The straight segments for other scripts (sub_199530): every run of a contour's points within 3 units of the
/// first across the axis, its points moved to the middle of its ends; once sorted, the segments within 8 units
/// past an earlier one are moved onto it.
pub fn latin_segments(x_axis: bool, e: &mut Element, arena: &mut Arena) -> Vec<Seg> {
    let (sps, eps) = (e.sp.clone(), e.ep.clone());
    let (c, a) = axes_mut(e, x_axis);
    let mut out: Vec<Seg> = Vec::new();
    'contours: for k in 0..sps.len() {
        let (sp, ep) = (sps[k], eps[k]);
        if sp >= ep {
            continue;
        }
        let mut start = if (c[usize::from(ep)] - c[usize::from(sp)]).abs() >= 4 { sp } else { ep };
        let mut i = sp;
        loop {
            while i != ep && (c[usize::from(i) + 1] - c[usize::from(start)]).abs() <= 3 {
                i += 1;
            }
            if start != i {
                let sum = c[usize::from(i)] + c[usize::from(start)];
                let mid = (if sum >= 0 { sum } else { sum + 1 }) >> 1;
                let mut s = Seg { start, end: i, contour: k as u16, dir: 0, list: NIL };
                c[usize::from(start)] = mid;
                c[usize::from(i)] = mid;
                let mut p = if ep == start { sp } else { start + 1 };
                while p != i {
                    s.list = arena.alloc(p, s.list);
                    c[usize::from(p)] = mid;
                    if arena.full() {
                        break;
                    }
                    p = if ep == p { sp } else { p + 1 };
                }
                s.dir = if a[usize::from(i)] > a[usize::from(start)] { 1 } else { -1 };
                out.push(s);
                if out.len() == 128 {
                    break 'contours;
                }
            }
            i += 1;
            start = i;
            if ep <= i {
                break;
            }
        }
    }
    if out.len() == 128 {
        out.pop();
    }
    sort_by(&mut out, |s| c[usize::from(s.start)]);
    for i in 0..out.len() {
        let v = c[usize::from(out[i].start)];
        for s in &out[i + 1..] {
            if c[usize::from(s.start)] - v > 8 {
                break;
            }
            c[usize::from(s.end)] = v;
            c[usize::from(s.start)] = v;
            for p in arena.iter(s.list) {
                c[usize::from(p)] = v;
            }
        }
    }
    out
}

/// Whether a contour turns clockwise at its top (sub_19BAD0, -1 or 0): the topmost point (leftmost of equal
/// ones) and the cross product of its neighbours, or when that is zero the difference of the unit vectors' x.
pub fn orientation(k: usize, e: &Element) -> i32 {
    let (x, y) = (&e.x, &e.y);
    let (sp, ep) = (usize::from(e.sp[k]), usize::from(e.ep[k]));
    let mut best = sp;
    for j in sp + 1..=ep {
        if y[j] > y[best] || (y[j] == y[best] && x[j] < x[best]) {
            best = j;
        }
    }
    let mut prev = if best == sp { ep } else { best - 1 };
    if x[prev] == x[best] && y[prev] == y[best] {
        prev = if prev == sp { ep } else { prev - 1 };
    }
    let next = if best == ep { sp } else { best + 1 };
    let c1 = varmul(x[best] - x[prev], y[next] - y[prev], 6);
    let c2 = varmul(y[best] - y[prev], x[next] - x[prev], 6);
    let mut d = c1.wrapping_sub(c2);
    if c1 == c2 {
        let u = normalize([(x[prev] - x[best]) << 10, (y[prev] - y[best]) << 10]);
        let v = normalize([(x[next] - x[best]) << 10, (y[next] - y[best]) << 10]);
        d = u[0].wrapping_sub(v[0]);
    }
    d >> 31
}

/// The overlap along the axis of a stem's two segments.
fn overlap(a: &[i32], s: &Stem) -> i32 {
    let at = |p: u16| a[usize::from(p)];
    let hi = at(s.a[0]).max(at(s.a[1])).min(at(s.b[0]).max(at(s.b[1])));
    let lo = at(s.a[0]).min(at(s.a[1])).max(at(s.b[0]).min(at(s.b[1])));
    hi - lo
}

/// Four points in order along the axis, an insertion sort keeping equal ones in order.
fn sort4(a: &[i32], mut w: [u16; 4]) -> [u16; 4] {
    sort_by(&mut w, |&p| a[usize::from(p)]);
    w
}

/// Pair a CJK glyph's segments into stems (sub_198870): a segment running the way of the outer contour (found
/// at the leftmost point) with each following one of the other direction within `thr` across that overlaps it
/// along; then of two stems sharing a segment the one whose own segments overlap less goes, and stems at the
/// same place with the same width are merged. None at all when the arena runs out.
pub fn cjk_stems(x_axis: bool, segs: &[Seg], e: &Element, thr: i32, arena: &mut Arena) -> Vec<Stem> {
    let (c, a) = axes(e, x_axis);
    let at = |v: &[i32], p: u16| v[usize::from(p)];
    let sign = if x_axis { 1 } else { -1 };
    let mut left = 0;
    let mut min = e.x[0];
    for k in 0..e.sp.len() {
        for p in usize::from(e.sp[k])..=usize::from(e.ep[k]) {
            if e.x[p] < min {
                left = k;
                min = e.x[p];
            }
        }
    }
    let dir = if orientation(left, e) != 0 { sign } else { -sign };
    let n = segs.len();
    let mut stems: Vec<Stem> = Vec::new();
    for i in 0..n {
        let s = segs[i];
        if s.dir as i32 == dir && i + 1 < n && at(c, segs[i + 1].start) - at(c, s.start) <= thr {
            let (lo, hi) = (at(a, s.start).min(at(a, s.end)), at(a, s.start).max(at(a, s.end)));
            let mut j = i + 1;
            loop {
                let t = segs[j];
                if t.dir as i32 != dir {
                    let (tlo, thi) = (at(a, t.start).min(at(a, t.end)), at(a, t.start).max(at(a, t.end)));
                    if tlo <= hi && thi >= lo {
                        let list_a = arena.copy_onto(s.list, NIL);
                        if arena.full() {
                            return Vec::new();
                        }
                        let list_b = arena.copy_onto(t.list, NIL);
                        if arena.full() {
                            return Vec::new();
                        }
                        let pos = (at(c, t.start) + at(c, s.start)) / 2;
                        stems.push(Stem {
                            a: [s.start, s.end],
                            b: [t.start, t.end],
                            list_a,
                            list_b,
                            pos,
                            fit: pos,
                            width: at(c, t.start) - at(c, s.start),
                            lo: lo.min(tlo),
                            hi: hi.max(thi),
                            flag: 0,
                        });
                    }
                }
                j += 1;
                if stems.len() > 63 || j >= n || at(c, segs[j].start) - at(c, s.start) > thr {
                    break;
                }
            }
        }
        if stems.len() > 63 {
            break;
        }
    }
    let n = stems.len();
    if n == 0 {
        return Vec::new();
    }
    sort_by(&mut stems, |s| s.pos);
    for i in 0..n {
        if stems[i].width == GONE {
            continue;
        }
        for j in i + 1..n {
            if stems[j].width == GONE || stems[i].width == stems[j].width {
                continue;
            }
            let (si, sj) = (stems[i], stems[j]);
            if !(si.a == sj.a || si.b == sj.b) {
                continue;
            }
            if overlap(a, &si) < overlap(a, &sj) {
                stems[i].width = GONE;
            } else {
                stems[j].width = GONE;
            }
        }
    }
    for i in 0..n {
        if stems[i].width == GONE {
            continue;
        }
        let mut j = i + 1;
        while j < n && stems[j].pos <= stems[i].pos {
            let (si, sj) = (stems[i], stems[j]);
            if sj.width != GONE && si.width == sj.width {
                let s = sort4(a, [si.a[0], si.a[1], sj.a[0], sj.a[1]]);
                let mut head = NIL;
                if s[1] != s[0] && s[1] != s[3] {
                    head = arena.alloc(s[1], NIL);
                }
                if arena.full() {
                    return Vec::new();
                }
                if s[2] != s[0] && s[2] != s[3] {
                    head = arena.alloc(s[2], head);
                }
                if arena.full() {
                    return Vec::new();
                }
                head = arena.copy_onto(si.list_a, head);
                if arena.full() {
                    return Vec::new();
                }
                head = arena.copy_onto(sj.list_a, head);
                if arena.full() {
                    return Vec::new();
                }
                let t = sort4(a, [si.b[0], si.b[1], sj.b[0], sj.b[1]]);
                let mut head_b = NIL;
                if t[1] != t[0] && t[1] != t[3] {
                    head_b = arena.alloc(t[1], NIL);
                }
                if arena.full() {
                    return Vec::new();
                }
                if t[2] != t[0] && t[2] != t[3] {
                    head_b = arena.alloc(t[2], head_b);
                }
                if arena.full() {
                    return Vec::new();
                }
                head_b = arena.copy_onto(si.list_b, head_b);
                if arena.full() {
                    return Vec::new();
                }
                head_b = arena.copy_onto(sj.list_b, head_b);
                if arena.full() {
                    return Vec::new();
                }
                let m = &mut stems[i];
                m.a = [s[0], s[3]];
                m.b = [t[0], t[3]];
                m.list_a = head;
                m.list_b = head_b;
                m.fit = m.pos;
                m.lo = at(a, t[0]).min(at(a, s[0]));
                m.hi = at(a, t[3]).max(at(a, s[3]));
                m.flag = 0;
                stems[j].width = GONE;
            }
            j += 1;
        }
    }
    stems.retain(|s| s.width != GONE);
    stems
}

/// The lowest and highest of a segment's two points along the axis (ties: the first lowest, the second highest).
fn ends(a: &[i32], p: u16, q: u16) -> (u16, u16) {
    if a[usize::from(p)] <= a[usize::from(q)] { (p, q) } else { (q, p) }
}

/// Merge stem `j`'s segment into stem `i`'s (sub_199890): the lowest and highest of their four points become the
/// ends, the other two join `i`'s list (in front of `j`'s list, which is linked on behind it).
fn merge_side(a: &[i32], p: [u16; 2], q: [u16; 2], list_p: u32, list_q: u32, arena: &mut Arena) -> Option<([u16; 2], u32)> {
    let at = |p: u16| a[usize::from(p)];
    let (p_lo, p_hi) = ends(a, p[0], p[1]);
    let (q_lo, q_hi) = ends(a, q[0], q[1]);
    let (lo, mid_lo) = if at(p_lo) <= at(q_lo) { (p_lo, q_lo) } else { (q_lo, p_lo) };
    let (mid_hi, hi) = if at(p_hi) <= at(q_hi) { (p_hi, q_hi) } else { (q_hi, p_hi) };
    let (inner_lo, inner_hi) = if at(mid_hi) <= at(mid_lo) { (mid_hi, mid_lo) } else { (mid_lo, mid_hi) };
    let mut head = list_p;
    if head != NIL {
        let mut t = head;
        while arena.nodes[t as usize].next != NIL {
            t = arena.nodes[t as usize].next;
        }
        arena.nodes[t as usize].next = list_q;
    } else {
        head = list_q;
    }
    if inner_lo != lo && inner_lo != hi {
        head = arena.alloc(inner_lo, head);
    }
    if arena.full() {
        return None;
    }
    if inner_hi != lo && inner_hi != hi {
        head = arena.alloc(inner_hi, head);
    }
    if arena.full() {
        return None;
    }
    Some(([lo, hi], head))
}

/// Pair the segments of a glyph in another script into stems (sub_199890): as for CJK, but the segments must
/// also overlap along by an eighth of their distance across; stems at the same place with the same width are
/// merged first, then of two that share an edge (the same place across and overlapping points) the one with the
/// shorter segments goes.
pub fn latin_stems(x_axis: bool, segs: &[Seg], e: &Element, thr: i32, arena: &mut Arena) -> Vec<Stem> {
    let (c, a) = axes(e, x_axis);
    let at = |v: &[i32], p: u16| v[usize::from(p)];
    let sign = if x_axis { 1 } else { -1 };
    let mut left = 0;
    let mut min = e.x[0];
    for k in 0..e.sp.len() {
        if e.sp[k] < e.ep[k] {
            for p in usize::from(e.sp[k])..=usize::from(e.ep[k]) {
                if e.x[p] < min {
                    min = e.x[p];
                    left = k;
                }
            }
        }
    }
    let dir = if orientation(left, e) != 0 { sign } else { -sign };
    let n = segs.len();
    let mut stems: Vec<Stem> = Vec::new();
    for i in 0..n {
        let s = segs[i];
        if s.dir as i32 == dir && i + 1 < n {
            let (lo, hi) = (at(a, s.start).min(at(a, s.end)), at(a, s.start).max(at(a, s.end)));
            let mut j = i + 1;
            let mut dist = at(c, segs[j].start) - at(c, s.start);
            while dist <= thr {
                let t = segs[j];
                if t.dir as i32 != dir {
                    let (tlo, thi) = (at(a, t.start).min(at(a, t.end)), at(a, t.start).max(at(a, t.end)));
                    if tlo <= hi && thi >= lo && hi.min(thi) - lo.max(tlo) >= dist >> 3 {
                        let list_a = arena.copy_onto(s.list, NIL);
                        if arena.full() {
                            return Vec::new();
                        }
                        let list_b = arena.copy_onto(t.list, NIL);
                        if arena.full() {
                            return Vec::new();
                        }
                        let pos = (at(c, t.start) + at(c, s.start)) / 2;
                        stems.push(Stem {
                            a: [s.start, s.end],
                            b: [t.start, t.end],
                            list_a,
                            list_b,
                            pos,
                            fit: pos,
                            width: at(c, t.start) - at(c, s.start),
                            lo: lo.min(tlo),
                            hi: hi.max(thi),
                            flag: 0,
                        });
                    }
                }
                j += 1;
                if stems.len() > 63 || j >= n {
                    break;
                }
                dist = at(c, segs[j].start) - at(c, s.start);
            }
        }
        if stems.len() > 63 {
            break;
        }
    }
    let n = stems.len();
    if n == 0 {
        return stems;
    }
    sort_by(&mut stems, |s| s.pos);
    for i in 0..n {
        let mut j = i + 1;
        while j < n && stems[j].pos <= stems[i].pos {
            if stems[i].width == stems[j].width {
                let (si, sj) = (stems[i], stems[j]);
                let Some((ea, la)) = merge_side(a, si.a, sj.a, si.list_a, sj.list_a, arena) else { return Vec::new() };
                stems[j].list_a = NIL;
                let Some((eb, lb)) = merge_side(a, si.b, sj.b, si.list_b, sj.list_b, arena) else { return Vec::new() };
                stems[j].list_b = NIL;
                let m = &mut stems[i];
                m.a = ea;
                m.b = eb;
                m.list_a = la;
                m.list_b = lb;
                m.fit = m.pos;
                m.lo = at(a, eb[0]).min(at(a, ea[0]));
                m.hi = at(a, eb[1]).max(at(a, ea[1]));
                m.flag = 0;
                stems[j].width = GONE;
            }
            j += 1;
        }
    }
    let len = |s: &Stem| {
        let (a0, a1, b0, b1) = (at(a, s.a[0]), at(a, s.a[1]), at(a, s.b[0]), at(a, s.b[1]));
        (a0.max(a1) - a0.min(a1) + b0.max(b1) - b0.min(b1)).unsigned_abs() >> 1
    };
    for i in 0..n {
        if stems[i].width == GONE {
            continue;
        }
        for j in i + 1..n {
            let (si, sj) = (stems[i], stems[j]);
            if sj.width == GONE || si.width == sj.width {
                continue;
            }
            let (p, q) = if at(c, si.a[0]) == at(c, sj.a[0]) {
                (si.a, sj.a)
            } else if at(c, si.a[0]) == at(c, sj.b[0]) {
                (si.a, sj.b)
            } else if at(c, si.b[0]) == at(c, sj.a[0]) {
                (si.b, sj.a)
            } else if at(c, si.b[0]) == at(c, sj.b[0]) {
                (si.b, sj.b)
            } else {
                continue;
            };
            if p[0].max(p[1]) >= q[0].min(q[1]) && q[0].max(q[1]) >= p[0].min(p[1]) {
                if len(&si) >= len(&sj) {
                    stems[j].width = GONE;
                } else {
                    stems[i].width = GONE;
                }
            }
        }
    }
    stems.retain(|s| s.width != GONE);
    stems
}

/// 26.6 rounding to the nearest pixel, halves up (wrapping like the exe's: a CJK font without 林 or the two
/// complex characters has i32::MAX lines, nintendo_udsg-r_ext_zh-cn among the shared fonts).
fn round(v: i32) -> i32 {
    v.wrapping_add(32) & !63
}

/// The move putting whichever edge of a stem centred at `c` with half width `h` is nearer the grid onto it.
fn nearer(c: i32, h: i32) -> i32 {
    let lo = round(c - h) - (c - h);
    let hi = round(c + h) - (c + h);
    if lo.abs() < hi.abs() { lo } else { hi }
}

/// Whether two stems are too close for the grid: the left one's right edge `r` and the right one's left edge
/// `l` overlap, or share a pixel row, or leave a gap that would not show.
fn collide(r: i32, l: i32) -> bool {
    if l < r {
        return true;
    }
    let gap = l - r;
    let (fr, fl, a) = (r & !63, l & !63, r - (r & !63));
    if a == 0 {
        return gap <= if l == fl { 63 } else { 47 };
    }
    let b = l - fl;
    if b == 0 || fr == fl {
        return gap <= 47;
    }
    !(fl != fr + 64 || (a < 25 && b > 39) || a < 17 || b > 47)
}

/// Whether two stems (ordered by their fitted places) are too close.
fn too_close(st: &[Stem], p: usize, q: usize) -> bool {
    let (l, r) = if st[p].fit <= st[q].fit { (p, q) } else { (q, p) };
    collide(st[l].fit + st[l].width / 2, st[r].fit - st[r].width / 2)
}

/// Whether stems `k` and `m` overlap along enough to matter: by half the narrower width, along or across.
fn overlaps(k: &Stem, m: &Stem) -> bool {
    if m.hi < k.lo || k.hi < m.lo {
        return false;
    }
    let along = (m.hi - k.lo).min(k.hi - m.lo);
    let (hk, hm) = (k.width / 2, m.width / 2);
    let across = ((k.fit - hk) - hm - m.fit).min(-(hk + k.fit) + (m.fit - hm));
    let w = k.width.min(m.width) / 2;
    along >= w || across >= w
}

/// The stems that move with stem `r`: those sharing its first edge or its second, or within 15 units of it.
fn movers(st: &[Stem], r: usize) -> Vec<usize> {
    let r = st[r];
    (0..st.len())
        .filter(|&k| {
            let s = st[k];
            r.a[0] == s.a[0] || r.b[0] == s.a[0] || r.b[0] == s.b[0] || r.a[0] == s.b[0] || (r.pos - s.pos).abs() <= 15
        })
        .collect()
}

/// The other stems that stem `k`, newly placed, would now be too close to.
fn crowded(st: &[Stem], k: usize) -> Vec<bool> {
    (0..st.len()).map(|m| m != k && too_close(st, k, m) && overlaps(&st[k], &st[m])).collect()
}

/// Move the stems going with stem `j` up off stem `i` (sub_19BCA0): each to where its nearer edge reaches the
/// next pixel, a pixel up from where it was when that leaves it on the grid or still too close to `i`, then
/// the stems above it that it now crowds, in turn. A stem may not go 97 units from its place or past the last
/// stem; if any cannot, every stem goes back and the move fails.
fn move_up(i: usize, j: usize, st: &mut [Stem]) -> bool {
    let n = st.len();
    if n < 1 {
        return true;
    }
    let outer: Vec<i32> = st.iter().map(|s| s.fit).collect();
    let cands = movers(st, j);
    if cands.is_empty() {
        return true;
    }
    let mut ok = true;
    for &k in &cands {
        let local: Vec<i32> = st.iter().map(|s| s.fit).collect();
        if st[k].pos == st[n - 1].pos {
            ok = false;
            break;
        }
        let w = st[k].width;
        let h = (if w + 1 >= 0 { w + 1 } else { w + 2 }) >> 1;
        let fit = st[k].fit;
        let edge = if ((h + fit) & 63) >= 3 { h + fit } else { fit - h };
        let d = ((edge + 63) & !63) - edge;
        st[k].fit = d + fit;
        if d == 0 || (k != i && too_close(st, i, k)) {
            st[k].fit = local[k] + 64;
        }
        if (st[k].fit - st[k].pos).abs() >= 97 || st[k].fit > st[n - 1].fit {
            st[k].fit = local[k];
            ok = false;
            break;
        }
        let flags = crowded(st, k);
        if !flags.contains(&true) || k + 1 >= n {
            continue;
        }
        let mut all = true;
        for m in k + 1..n {
            if flags[m] {
                all &= move_up(k, m, st);
            }
            if !all {
                break;
            }
        }
        if !all {
            for m in k..n {
                st[m].fit = local[m];
            }
            ok = false;
            break;
        }
    }
    if !ok {
        for (s, f) in st.iter_mut().zip(outer) {
            s.fit = f;
        }
    }
    ok
}

/// The same downwards (sub_19C390): the stems going with stem `i` move off stem `j` to where their nearer
/// edge (the right one unless it is within 3 of a pixel) falls back to a pixel, and on down to the stems below;
/// none may go below the first stem's place.
fn move_down(i: usize, j: usize, st: &mut [Stem]) -> bool {
    let n = st.len();
    if n < 1 {
        return true;
    }
    let outer: Vec<i32> = st.iter().map(|s| s.fit).collect();
    let cands = movers(st, i);
    if cands.is_empty() {
        return true;
    }
    let mut ok = true;
    for &k in &cands {
        let local: Vec<i32> = st.iter().map(|s| s.fit).collect();
        if st[k].fit == st[0].fit {
            ok = false;
            break;
        }
        let w = st[k].width;
        let h = (if w + 1 >= 0 { w + 1 } else { w + 2 }) >> 1;
        let fit = st[k].fit;
        let right = (h + fit) & 63;
        let d = if right >= 3 { right } else { (fit - h) & 63 };
        st[k].fit = fit - d;
        if d == 0 || (k != j && too_close(st, k, j)) {
            st[k].fit = local[k] - 64;
        }
        if (st[k].fit - st[k].pos).abs() >= 97 || st[k].fit < st[0].pos {
            st[k].fit = local[k];
            ok = false;
            break;
        }
        let flags = crowded(st, k);
        if !flags.contains(&true) || k == 0 {
            continue;
        }
        let mut all = true;
        for m in (0..k).rev() {
            if flags[m] {
                all &= move_down(m, k, st);
            }
            if !all {
                break;
            }
        }
        if !all {
            for (s, &f) in st.iter_mut().zip(&local) {
                s.fit = f;
            }
            ok = false;
            break;
        }
    }
    if !ok {
        for (s, f) in st.iter_mut().zip(outer) {
            s.fit = f;
        }
    }
    ok
}

/// Fit a CJK glyph's stems to the grid (sub_19A350): each on the pixel edge nearer to one of its edges; then
/// each pair overlapping along and within two pixels that the grid would run together is moved apart, up or
/// down with the stems going with them, or failing that both are narrowed to 7/8 when wider than 54 units and
/// kept so if that parts them.
pub fn cjk_fit(st: &mut [Stem]) {
    let n = st.len();
    for s in st.iter_mut() {
        s.fit = s.pos + nearer(s.pos, s.width >> 1);
    }
    for i in 0..n {
        for j in 0..n {
            if st[j].fit < st[i].fit {
                continue;
            }
            let (si, sj) = (st[i], st[j]);
            if si.hi < sj.lo || sj.hi < si.lo {
                continue;
            }
            if (sj.fit - si.fit) - sj.width / 2 - si.width / 2 > 128 {
                continue;
            }
            if i == j || !overlaps(&si, &sj) {
                continue;
            }
            if !collide(si.fit + si.width / 2, sj.fit - sj.width / 2) {
                continue;
            }
            if move_up(i, j, st) || move_down(i, j, st) {
                continue;
            }
            let (wi, fi, fj, wj) = (st[i].width, st[i].fit, st[j].fit, st[j].width);
            if wi <= 54 && wj < 55 {
                continue;
            }
            if wi >= 55 {
                let v = 7 * wi + 4;
                st[i].width = v >> 3;
                st[i].fit = fi + nearer(fi, v >> 4);
            }
            if wj > 54 {
                let v = 7 * wj + 4;
                st[j].width = v >> 3;
                st[j].fit = fj + nearer(fj, v >> 4);
            }
            if too_close(st, i, j) {
                st[i].fit = fi;
                st[j].fit = fj;
                st[i].width = wi;
                st[j].width = wj;
            }
        }
    }
}


/// The cost of a reference whose extent [lo, hi] along misses `v`: the distance from it.
fn miss(lo: i32, hi: i32, v: i32) -> i32 {
    if lo <= v {
        if hi < v { v - hi } else { 0 }
    } else {
        lo - v
    }
}

/// A stem edge to interpolate the other points between: its point, original place across and the stem's
/// extent along (16-byte records in p360).
#[derive(Clone, Copy)]
struct Ref {
    pt: u16,
    at: i32,
    lo: i32,
    hi: i32,
}

/// For a CJK glyph, interpolate point `p` between the nearest stem below it and the first above that both
/// reach it along, if any (sub_19A7F0); `cur` and `orig` across the axis, `along` the original other way.
fn between(p: usize, st: &[Stem], cur: &mut [i32], orig: &[i32], along: &[i32], f: &mut [u8], flag: u8) {
    let (v, w) = (orig[p], along[p]);
    for m in (0..st.len()).rev() {
        let sm = st[m];
        if v < orig[usize::from(sm.a[0])] || w < sm.lo || w > sm.hi {
            continue;
        }
        for sq in &st[m + 1..] {
            let up = usize::from(sq.b[0]);
            if v > orig[up] || w < sq.lo || w > sq.hi {
                continue;
            }
            let lo = usize::from(sm.a[0]);
            let mut c = cur[lo];
            if orig[up] != orig[lo] && cur[up] != c && v != orig[lo] {
                c += muldiv(cur[up] - c, v - orig[lo], orig[up] - orig[lo]);
            }
            cur[p] = c;
            f[p] |= flag;
            return;
        }
    }
}

/// Put the fitted stems into the outline along one axis (sub_19A7F0): a stem still where it was goes on the
/// grid; the points of each stem none of whose ends are placed yet go to its edges; the glyph's lowest and
/// highest points across join the stems, as one-unit stems over its whole extent. Then for CJK each contour's
/// first lowest and highest points still free are interpolated between a stem below and one above that reach
/// them (and a CFF glyph's contours are closed up along y); for other scripts every free local extreme of a
/// contour goes between the stem edges nearest it below and above, distance along outside a stem's extent
/// counting as well.
pub fn apply_stems(x_axis: bool, script: i32, st: &mut Vec<Stem>, e: &mut Element, arena: &Arena, gs418: i16) {
    let flag = if x_axis { 1u8 } else { 2 };
    for s in st.iter_mut() {
        if s.fit == s.pos {
            s.fit += nearer(s.fit, s.width >> 1);
        }
    }
    let Element { x, y, ox, oy, f, sp, ep, .. } = e;
    let (cur, orig, along) = if x_axis { (x, &*ox, &*oy) } else { (y, &*oy, &*ox) };
    for s in st.iter() {
        let ends = [s.a[0], s.a[1], s.b[0], s.b[1]];
        if ends.iter().any(|&p| f[usize::from(p)] & flag != 0) {
            continue;
        }
        let lo = s.fit - s.width / 2;
        for p in s.a.into_iter().chain(arena.iter(s.list_a)) {
            cur[usize::from(p)] = lo;
            f[usize::from(p)] |= flag;
        }
        let hi = s.fit + s.width / 2;
        for p in s.b.into_iter().chain(arena.iter(s.list_b)) {
            cur[usize::from(p)] = hi;
            f[usize::from(p)] |= flag;
        }
    }
    let last = ep.last().map_or(0, |&p| usize::from(p));
    let (mut lo, mut hi, mut lo_at, mut hi_at) = (orig[0], orig[0], 0, 0);
    let (mut lo_w, mut hi_w) = (along[0], along[0]);
    for p in 1..=last {
        let w = along[p];
        if w < lo_w {
            lo_w = w;
        } else {
            hi_w = hi_w.max(w);
        }
        let c = orig[p];
        let placed = f[p] & flag != 0;
        if c == lo && placed {
            lo_at = p;
        } else if c == hi && placed {
            hi_at = p;
        } else if c < lo {
            (lo, lo_at) = (c, p);
        } else if c > hi {
            (hi, hi_at) = (c, p);
        }
    }
    for (at, pos) in [(lo_at, lo), (hi_at, hi)] {
        let p = at as u16;
        st.push(Stem {
            a: [p; 2],
            b: [p; 2],
            list_a: NIL,
            list_b: NIL,
            pos,
            fit: cur[at],
            width: 1,
            lo: lo_w,
            hi: hi_w,
            flag: 0,
        });
    }
    sort_by(st, |s| s.pos);
    if script == 7 {
        for (&s, &t) in sp.iter().zip(ep.iter()) {
            if s == t {
                continue;
            }
            let (mut min, mut max) = (usize::from(s), usize::from(s));
            for p in usize::from(s)..=usize::from(t) {
                if orig[p] < orig[min] {
                    min = p;
                } else if orig[p] > orig[max] {
                    max = p;
                }
            }
            for p in [max, min] {
                if f[p] & flag == 0 {
                    between(p, st, cur, orig, along, f, flag);
                }
            }
        }
        if gs418 == 7 && !x_axis {
            for (&s, &t) in sp.iter().zip(ep.iter()) {
                let (s, t) = (usize::from(s), usize::from(t));
                if t == 0 {
                    continue;
                }
                if cur[t] == cur[t - 1] {
                    cur[s] = cur[t];
                } else {
                    cur[t] = cur[s];
                }
            }
        }
        return;
    }
    let mut refs = Vec::with_capacity(2 * st.len());
    for s in st.iter() {
        refs.push(Ref { pt: s.a[0], at: orig[usize::from(s.a[0])], lo: s.lo, hi: s.hi });
        if s.a[0] != s.b[0] {
            refs.push(Ref { pt: s.b[0], at: orig[usize::from(s.b[0])], lo: s.lo, hi: s.hi });
        }
    }
    sort_by(&mut refs, |r| r.at);
    let count = refs.len();
    // the last pair of points interpolated between and their ratio
    let (mut pair, mut ratio) = ((u16::MAX, u16::MAX), 0);
    for (&s, &t) in sp.iter().zip(ep.iter()) {
        if s >= t {
            continue;
        }
        let (s, t) = (usize::from(s), usize::from(t));
        let mut prev = t;
        for p in s..=t {
            let before = prev;
            prev = p;
            if f[p] & flag != 0 {
                continue;
            }
            let next = if p == t { s } else { p + 1 };
            let (a, c, b) = (orig[before], orig[p], orig[next]);
            if !((a <= c && b < c) || (a >= c && b > c)) {
                continue;
            }
            let w = along[p];
            let mut k = if refs[count / 2].at >= c { 0 } else { count / 2 };
            while k + 1 < count && refs[k].at < c {
                k += 1;
            }
            let (mut best, mut upper) = (i32::MAX, usize::MAX);
            let mut d = refs[k].at - c;
            let mut j = k;
            if d != i32::MAX {
                loop {
                    d += miss(refs[j].lo, refs[j].hi, w);
                    if d < best {
                        (best, upper) = (d, j);
                    }
                    j += 1;
                    if j >= count {
                        break;
                    }
                    d = refs[j].at - c;
                    if d >= best {
                        break;
                    }
                }
            }
            while k > 0 && refs[k].at > c {
                k -= 1;
            }
            let (mut best, mut lower) = (i32::MAX, usize::MAX);
            let mut d = c - refs[k].at;
            let mut j = k as isize;
            if d != i32::MAX {
                loop {
                    d += miss(refs[j as usize].lo, refs[j as usize].hi, w);
                    if d < best {
                        (best, lower) = (d, j as usize);
                    }
                    j -= 1;
                    if j < 0 {
                        break;
                    }
                    d = c - refs[j as usize].at;
                    if d >= best {
                        break;
                    }
                }
            }
            let (Some(up), Some(down)) = (refs.get(upper), refs.get(lower)) else {
                continue;
            };
            let (u, l) = (usize::from(up.pt), usize::from(down.pt));
            let mut v = cur[l];
            if c != orig[l] && cur[u] != v && orig[u] != orig[l] {
                if pair != (up.pt, down.pt) {
                    ratio = vardiv(cur[u] - v, orig[u] - orig[l], 16);
                    pair = (up.pt, down.pt);
                }
                v += varmul(c - orig[l], ratio, 16);
            }
            cur[p] = v;
            f[p] |= flag;
        }
    }
}

/// Whether point `p` is a contour's local top along y (1) or bottom (-1), by its original place, passing over
/// neighbours level with it; 0 otherwise or on a flat contour (sub_19CA90).
fn extreme(p: usize, e: &Element) -> i32 {
    let oy = &e.oy;
    let Some(c) = (0..e.sp.len()).find(|&c| usize::from(e.sp[c]) <= p && p <= usize::from(e.ep[c])) else {
        return 0;
    };
    let (s, t) = (usize::from(e.sp[c]), usize::from(e.ep[c]));
    if s == t || (s + 1..=t).all(|k| oy[k] == oy[s]) {
        return 0;
    }
    let v = oy[p];
    let mut q = if p == s { t } else { p - 1 };
    while oy[q] == v {
        q = if q == s { t } else { q - 1 };
    }
    let mut r = if p == t { s } else { p + 1 };
    while oy[r] == v {
        r = if r == t { s } else { r + 1 };
    }
    let (i, j) = (oy[q], oy[r]);
    if i > v && j > v { -1 } else { i32::from(i < v && j < v) }
}

/// Fit a CJK (or Hangul) glyph between the script's reference lines along y (sub_19B570): `r` the top and
/// its overshoot, the bottom and its overshoot. The glyph's top goes to the top line when within half a pixel
/// of it, or to the top stem's edge when that is long and near; likewise the bottom; the outline is scaled
/// between, and stems resting on the overshoot line follow it.
fn cjk_lines(r: [i32; 4], st: &mut [Stem], e: &mut Element, ppem64: i32) {
    let Some(&last) = e.ep.last() else {
        return;
    };
    let last = usize::from(last);
    let at_least = |v: i32| if ppem64 > 896 && v < 1 { 64 } else { v };
    let bottom_h = at_least(round(r[2] - r[3]));
    let top_h = at_least(round(r[0] - r[1]));
    if last == 0 {
        return;
    }
    let y = &e.y;
    let (mut lo, mut hi) = (y[0], y[0]);
    for &v in &y[1..=last] {
        hi = hi.max(v);
        lo = lo.min(v);
    }
    if hi == lo {
        return;
    }
    let base = round(r[3]);
    let top = (r[0].wrapping_sub(r[3]).wrapping_add(38) & !63).wrapping_add(base);
    let over = top.wrapping_sub(top_h);
    let n = st.len();
    let x = &e.x;
    let length = |s: &Stem| {
        let (a0, a1, b0, b1) = (x[usize::from(s.a[0])], x[usize::from(s.a[1])], x[usize::from(s.b[0])], x[usize::from(s.b[1])]);
        ((a0.max(a1) - a0.min(a1) + b0.max(b1) - b0.min(b1)).unsigned_abs() >> 1) as i32
    };
    let long = ppem64 / 6;
    let to_hi;
    let mut first_long = false;
    if n == 0 {
        to_hi = if dist(hi, r[0]) >= 32 { hi } else { top };
    } else {
        first_long = length(&st[0]) > long;
        let edge = st[n - 1].pos + st[n - 1].width / 2;
        if long < length(&st[n - 1]) && (edge - hi).abs() <= 47 {
            if dist(edge, r[1]) >= 32 {
                to_hi = hi;
            } else {
                to_hi = over;
                hi = edge;
            }
        } else {
            to_hi = if dist(hi, r[0]) >= 32 { hi } else { top };
        }
    }
    let mut to_lo = None;
    if first_long {
        let edge = st[0].pos - st[0].width / 2;
        if (edge - lo).abs() <= 15 {
            if dist(edge, r[2]) >= 32 {
                to_lo = Some(lo);
            } else {
                to_lo = Some(bottom_h.wrapping_add(base));
                lo = edge;
            }
        }
    }
    let to_lo = to_lo.unwrap_or(if dist(lo, r[3]) >= 32 { lo } else { base });
    if hi != to_hi || lo != to_lo {
        let ratio = vardiv(to_hi - to_lo, hi - lo, 16);
        for p in 0..=last {
            e.y[p] = varmul(ratio, e.oy[p] - lo, 16) + to_lo;
        }
        for s in st.iter_mut() {
            let v = (e.y[usize::from(s.b[0])] + e.y[usize::from(s.a[0])]) / 2;
            s.pos = v;
            s.fit = v;
        }
    }
    if (to_lo == base || to_hi == top) && n >= 1 {
        for s in st.iter_mut() {
            let h = s.width / 2;
            if dist(s.pos - h, r[1]) > 31 {
                break;
            }
            s.fit = h.wrapping_add(over);
        }
        for s in st.iter_mut().rev() {
            let h = s.width / 2;
            if dist(h + s.pos, r[1]) > 31 {
                break;
            }
            s.fit = over.wrapping_sub(h);
        }
    }
    e.oy[..=last].copy_from_slice(&e.y[..=last]);
}

/// |a - b| wrapping like the exe's (`round`).
fn dist(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b).wrapping_abs()
}

/// Put the stems whose top edge lies on one of a script's two top lines, or whose bottom edge lies on one of
/// its two bottom lines, onto them (sub_19B9E0): `r` the upper and main top lines, the main and lower bottom.
fn lines4(r: [i32; 4], st: &mut [Stem]) {
    let (top, base) = (round(r[1]), round(r[2]));
    let upper = round(r[0] - r[1]) + top;
    let lower = r[2] - round(r[2] - r[3]);
    for s in st.iter_mut() {
        let h = s.width / 2;
        let up = s.pos + h;
        if (r[1] - up).abs() < 32 {
            s.fit = top - h;
        } else if (r[0] - up).abs() <= 31 {
            s.fit = upper - h;
        }
        let down = s.pos - h;
        if (r[2] - down).abs() <= 31 {
            s.fit = h + base;
        } else if (r[3] - down).abs() <= 31 {
            s.fit = lower + h;
        }
    }
}

/// Nodes in the point list arena (globalGS p352 .. p360).
pub const ARENA: usize = 2928;

/// What rtgah keeps in the global graphics state from glyph to glyph: the reference lines get_ref_lines measures
/// (refs[13..], refs[19..20] set by the first glyph hinted along x), and what it reports back for them.
#[derive(Clone, Debug)]
pub struct Globals {
    pub ppem: u16,
    pub fs_flags: u32,
    pub font_flags: u32,
    pub refs: [i32; 72],
    /// globalGS+418: 7 closes the contours up along y
    pub g418: i16,
    /// globalGS+408 / +412: the fitted top of the highest stem along y and the bottom of the lowest
    pub top: i32,
    pub bottom: i32,
    /// globalGS+416, cleared when the first glyph is too complex to measure
    pub g416: u16,
}

/// Bring the extreme points of each contour's curves level (the start of rtgah): an off-curve extreme, or one
/// between off-curve points, has a neighbour put level with it when that one is 22 times as far along as across.
fn level_extremes(e: &mut Element) {
    for c in 0..e.sp.len() {
        let (s, t) = (usize::from(e.sp[c]), usize::from(e.ep[c]));
        if s == t {
            continue;
        }
        let (mut xmax, mut xmin, mut ymax, mut ymin) = (s, s, s, s);
        for p in s + 1..=t {
            if e.x[p] > e.x[xmax] {
                xmax = p;
            } else if e.x[p] < e.x[xmin] {
                xmin = p;
            }
            if e.y[p] > e.y[ymax] {
                ymax = p;
            } else if e.y[p] < e.y[ymin] {
                ymin = p;
            }
        }
        for (p, x_axis) in [(xmax, true), (xmin, true), (ymax, false), (ymin, false)] {
            let prev = if p == s { t } else { p - 1 };
            let next = if p == t { s } else { p + 1 };
            if e.on[p] != 0 && (e.on[prev] != 0 || e.on[next] != 0) {
                continue;
            }
            let Element { x, y, ox, oy, .. } = e;
            let (a, b, o) = if x_axis { (x, &*y, ox) } else { (y, &*x, oy) };
            if a[p] == a[prev] || a[p] == a[next] {
                continue;
            }
            for q in [prev, next] {
                if 22 * (a[p] - a[q]).abs() < (b[p] - b[q]).abs() {
                    a[q] = a[p];
                    o[q] = a[p];
                }
            }
        }
    }
}

/// For a small Latin, Greek or Cyrillic size, scale the glyph and its reference lines so the x-height is a
/// whole pixel more (by refs[16] + 8 over refs[16]), keeping the lines at least two pixels apart.
fn grow(e: &mut Element, refs: &mut [i32; 72]) {
    let last = usize::from(e.ep[e.ep.len() - 1]);
    let r = vardiv(refs[16] + 8, refs[16], 16);
    for i in 13..19 {
        if refs[i] >= 1 {
            refs[i] = varmul(r, refs[i], 16);
        }
    }
    if ((refs[15] + 32) ^ (refs[16] + 32)) as u32 > 63 {
        refs[15] = refs[16];
    }
    if refs[13] - refs[15] <= 127 {
        refs[13] = refs[15] + 128;
    }
    for p in 0..=last {
        let v = varmul(r, e.ox[p], 16);
        e.ox[p] = v;
        e.x[p] = v;
        if e.oy[p] >= 1 {
            let v = varmul(r, e.oy[p], 16);
            e.oy[p] = v;
            e.y[p] = v;
        }
    }
    let q = last + 2;
    e.ox[q] = varmul(r, e.ox[q], 16);
    e.x[q] = round(e.ox[q]);
}

/// Fit a Latin glyph's sides along x (rtgah, before the x stems go in): a single stem with little outline beside
/// it moves so its side bearing is the font's measured one (refs[19] on the left, refs[20] on the right); with
/// several stems, the outer stem edges go to those places and the outline between them is scaled to fit.
fn latin_sides(e: &mut Element, st: &mut [Stem], left_ref: i32, right_ref: i32) {
    let np = e.points();
    let adv = e.x[np + 1];
    let (mut hi, mut lo) = (e.x[0], e.x[0]);
    for &v in &e.x[1..np] {
        hi = hi.max(v);
        lo = lo.min(v);
    }
    let (first, last) = (st[0], st[st.len() - 1]);
    let h0 = first.width / 2;
    let right = last.pos + last.width / 2;
    let left = first.pos - h0;
    let gap = adv - right;
    if (first.pos - last.pos).abs() <= 31 {
        let first_right = h0 + first.pos;
        let (out_l, out_r) = (left - lo, hi - first_right);
        let mut d = 0;
        if out_l <= out_r && out_l <= 63 {
            let v = left_ref - left;
            d = if v.abs() >= 65 { 0 } else { v };
        }
        if out_r <= out_l && out_r <= 63 && (right_ref - gap).abs() <= 64 {
            d = adv - right_ref - first_right;
        }
        if d != 0 {
            for p in 0..np {
                e.x[p] += d;
                e.ox[p] = e.x[p];
            }
            for s in st.iter_mut() {
                s.pos += d;
                s.fit = s.pos;
            }
        }
        return;
    }
    let to_l = if left - lo <= 63 && (left_ref - left).abs() < 73 { left_ref } else { left };
    let to_r = if hi - right <= 63 && (right_ref - gap).abs() < 73 { adv - right_ref } else { right };
    if to_l == left && to_r == right {
        return;
    }
    let ratio = if right == left { 0x40_0000 } else { vardiv(to_r - to_l, right - left, 16) };
    for p in 0..np {
        let v = e.x[p];
        e.x[p] = if v < left {
            v + (to_l - left)
        } else if v <= right {
            varmul(ratio, v - left, 16) + to_l
        } else {
            v + (to_r - right)
        };
        e.ox[p] = e.x[p];
    }
    for s in st.iter_mut() {
        let v = (e.x[usize::from(s.b[0])] + e.x[usize::from(s.a[0])]) / 2;
        s.pos = v;
        s.fit = v;
    }
}

/// An i or j: the stem nearest across its tallest one (within a quarter pixel) is fitted, and the tallest one
/// (the dot) goes to the same place.
fn dot_on_stem(st: &mut [Stem]) {
    let n = st.len();
    let mut top = 0;
    for k in 1..n {
        if st[top].hi < st[k].hi {
            top = k;
        }
    }
    let c = st[top].pos;
    let (mut best, mut pick) = (300_000, None);
    for k in (0..n).filter(|&k| k != top) {
        let d = (c - st[k].pos).abs();
        if d < best && d < 16 {
            best = d;
            pick = Some(k);
        }
    }
    if let Some(k) = pick {
        let fit = st[k].pos + nearer(st[k].pos, st[k].width >> 1);
        st[top].fit = fit;
        st[k].fit = fit;
    }
}

/// A one-unit-wide stem at point `pt`'s original height along y with every other point at that height in its
/// list (newest first, stopping when the arena fills).
fn level_stem(pt: usize, last: usize, lo: i32, hi: i32, ys: &[i32], arena: &mut Arena) -> Stem {
    let v = ys[pt];
    let mut list = NIL;
    for q in (0..=last).filter(|&q| q != pt) {
        if ys[q] == v {
            list = arena.alloc(q as u16, list);
            if arena.full() {
                break;
            }
        }
    }
    let p = pt as u16;
    Stem { a: [p; 2], b: [p; 2], list_a: list, list_b: NIL, pos: v, fit: v, width: 0, lo, hi, flag: 0 }
}

/// The Latin, Greek and Cyrillic y-axis fitting (rtgah, scripts 1 to 3): the glyph's top and bottom, and the
/// points that peak at the cap height or dip at the baseline, become stems if no stem has them; stems on a
/// reference line are fitted to it; then contours floating wholly above the tallest one (dots and accents) are
/// placed at least a pixel above its fitted top and scaled to whole pixels, the outline's originals taking the
/// result.
fn latin_y(e: &mut Element, st: &mut Vec<Stem>, refs: &[i32; 72], arena: &mut Arena) {
    let (r13, r14, r15, r16, r17, r18) = (refs[13], refs[14], refs[15], refs[16], refs[17], refs[18]);
    let t13 = round(r13);
    let narrow = r14 - r13 < 44 || r17 - r18 < 44;
    let t15 = if narrow { round(r16) } else { round(r15) };
    let t14 = if narrow { t13 } else { round(r14) };
    let t16 = round(r16);
    let last = usize::from(e.ep[e.ep.len() - 1]);
    let (ox, oy) = (&e.ox, &e.oy);
    let (mut xlo, mut xhi) = (ox[0], ox[0]);
    let (mut top, mut bottom) = (0, 0);
    let (mut cap, mut base) = (None, None);
    for p in 1..=last {
        if ox[p] < xlo {
            xlo = ox[p];
        } else {
            xhi = xhi.max(ox[p]);
        }
        if oy[p] > oy[top] {
            top = p;
        }
        if oy[p] < oy[bottom] {
            bottom = p;
        }
        if (oy[p] - r15).abs() <= 3 && extreme(p, e) == 1 {
            cap = Some(p);
        } else if (oy[p] - r17).abs() <= 3 && extreme(p, e) == -1 {
            base = Some(p);
        }
    }
    let has = |st: &[Stem], v: i32| {
        st.iter().any(|s| {
            let h = s.width / 2;
            s.pos - h - 1 <= v && v <= s.pos + h + 1
        })
    };
    for p in [Some(top), Some(bottom), cap] {
        if let Some(p) = p
            && !has(st, oy[p])
        {
            st.push(level_stem(p, last, xlo, xhi, oy, arena));
        }
    }
    if let Some(p) = base
        && oy[p] != oy[bottom]
    {
        st.push(level_stem(p, last, xlo, xhi, oy, arena));
    }
    for s in st.iter_mut() {
        let h = s.width / 2;
        let up = s.pos + h;
        let down = s.pos - h;
        s.fit = if (up - r13).abs() <= 31 {
            t13 - h
        } else if (up - r14).abs() <= 31 {
            t14 - h
        } else if (up - r15).abs() <= 47 {
            t15 - h
        } else if (up - r16).abs() <= 47 {
            t16 - h
        } else if (down - r17).abs() <= 47 {
            h + round(r17)
        } else if (down - r16).abs() <= 47 {
            h + t16
        } else {
            continue;
        };
    }
    // each contour's extreme points by the current outline, the first 32 contours
    let nc = e.sp.len().min(32);
    let (x, y) = (&e.x, &e.y);
    let (mut xmin, mut ymin, mut xmax, mut ymax) = (vec![0; nc], vec![0; nc], vec![0; nc], vec![0; nc]);
    for k in 0..nc {
        let s = usize::from(e.sp[k]);
        (xmin[k], ymin[k], xmax[k], ymax[k]) = (s, s, s, s);
        for m in s + 1..=usize::from(e.ep[k]) {
            if y[m] > y[ymax[k]] {
                ymax[k] = m;
            } else if y[m] < y[ymin[k]] {
                ymin[k] = m;
            }
            if x[m] > x[xmax[k]] {
                xmax[k] = m;
            } else if x[m] < x[xmin[k]] {
                xmin[k] = m;
            }
        }
    }
    let mut leftmost = 0;
    for k in 1..nc {
        if x[xmin[k]] < x[xmin[leftmost]] {
            leftmost = k;
        }
    }
    let turn = orientation(leftmost, e);
    let mut keep: Vec<bool> = (0..nc).map(|k| orientation(k, e) == turn).collect();
    let (mut tall, mut height) = (0, 0);
    for k in 0..nc {
        let h = y[ymax[k]] - y[ymin[k]];
        if h > height {
            (tall, height) = (k, h);
        }
    }
    let top_pt = ymax[tall];
    let top = y[top_pt];
    if top < r15 - 32 || nc == 0 || y[ymin[tall]] > r17 + 32 {
        return;
    }
    for k in 0..nc {
        if e.sp[k] == e.ep[k] {
            keep[k] = false;
        }
    }
    let inside = |m: usize, k: usize| {
        y[ymin[m]] > y[ymin[k]] && y[ymax[m]] < y[ymax[k]] && x[xmin[m]] > x[xmin[k]] && x[xmax[m]] < x[xmax[k]]
    };
    for k in 0..nc {
        if keep[k] && (0..nc).any(|m| m != k && inside(m, k)) {
            keep[k] = false;
        }
    }
    let mut any = false;
    for k in 0..nc {
        if y[ymin[k]] < top {
            keep[k] = false;
        } else {
            any |= keep[k];
        }
    }
    if !any {
        return;
    }
    let fitted = match st.iter().position(|s| (s.pos + s.width / 2 - top).abs() <= 15) {
        Some(k) => st[k].fit + st[k].width / 2,
        None => {
            let lastc = usize::from(e.ep[nc - 1]);
            let mut s = level_stem(top_pt, lastc, x[xmin[tall]], x[xmax[tall]], y, arena);
            s.fit = round(top);
            st.push(s);
            round(top)
        }
    };
    let (mut lo, mut hi) = (i32::MAX, -i32::MAX);
    for k in (0..nc).filter(|&k| keep[k]) {
        lo = lo.min(y[ymin[k]]);
        hi = hi.max(y[ymax[k]]);
    }
    let gap = match round(lo - top) {
        0 => 64,
        g => g,
    };
    let span = hi - lo;
    if span == 0 {
        return;
    }
    let to = gap + fitted;
    let ratio = (if (span - 65) as u32 >= 64 { span << 16 } else { 0x80_0000 }) / span;
    let Element { x, y, oy, f, sp, ep, .. } = e;
    for k in (0..nc).filter(|&k| keep[k]) {
        for p in usize::from(sp[k])..=usize::from(ep[k]) {
            y[p] = to + varmul(ratio, oy[p] - lo, 16);
            f[p] |= 2;
        }
    }
    if nc >= 3 {
        for k in (0..nc).filter(|&k| keep[k]) {
            let (a, b) = (xmax[k], xmin[k]);
            if (f[a] | f[b]) & 1 == 0 && x[a] - x[b] <= 127 {
                let c = (x[a] + x[b]) / 2;
                let d = ((c & !63) | 32) - c;
                for p in usize::from(sp[k])..=usize::from(ep[k]) {
                    x[p] += d;
                }
            }
        }
    }
    for s in st.iter_mut() {
        if s.pos > top {
            let (a, b) = (y[usize::from(s.a[0])], y[usize::from(s.b[0])]);
            s.pos = (a + b) / 2;
            s.fit = s.pos;
            s.width = (b - a).abs();
        }
    }
    let n = usize::from(4u16.wrapping_mul(ep[nc - 1]).wrapping_add(4) & 0xFFFC) / 4;
    e.ox[..n].copy_from_slice(&e.x[..n]);
    e.oy[..n].copy_from_slice(&e.y[..n]);
}

/// The reference lines a script's stems along y are fitted to, by rtgah's switch on the script.
fn script_lines(script: i32, st: &mut [Stem], e: &mut Element, refs: &[i32; 72], ppem64: i32) {
    let snap_top = |st: &mut [Stem], r: i32| {
        for s in st.iter_mut() {
            let h = s.width / 2;
            if (h - r + s.pos).abs() <= 63 {
                s.fit = round(r) - h;
            }
        }
    };
    match script {
        5 => {
            let r = refs[64];
            for s in st.iter_mut() {
                let h = s.width / 2;
                if (s.pos - h - r).abs() <= 31 {
                    s.fit = h + round(r);
                }
            }
        }
        6 => cjk_lines([refs[27], refs[25], refs[26], refs[28]], st, e, ppem64),
        7 => cjk_lines([refs[23], refs[21], refs[22], refs[24]], st, e, ppem64),
        8 => snap_top(st, refs[29]),
        9 => snap_top(st, refs[31]),
        10 => snap_top(st, refs[33]),
        11 => {
            let (r0, r1) = (refs[35], refs[36]);
            for s in st.iter_mut() {
                let h = s.width / 2;
                if (h - r0 + s.pos).abs() <= 31 {
                    s.fit = round(r0) - h;
                }
                if (s.pos - h - r1).abs() <= 31 {
                    s.fit = h + round(r1);
                }
            }
        }
        13 => lines4([refs[37], refs[38], refs[40], refs[39]], st),
        14 => lines4([refs[41], refs[42], refs[44], refs[43]], st),
        15 => lines4([refs[45], refs[46], refs[48], refs[47]], st),
        16 | 17 => lines4([refs[49], refs[50], refs[52], refs[51]], st),
        18 => lines4([refs[57], refs[57], refs[59], refs[58]], st),
        19 => lines4([refs[60], refs[61], refs[63], refs[62]], st),
        _ => {}
    }
}

/// The stems along one axis: CJK segments and stems for script 7, Latin ones otherwise.
fn stems(x_axis: bool, script: i32, e: &mut Element, thr: i32, arena: &mut Arena) -> Vec<Stem> {
    if script == 7 {
        let segs = cjk_segments(x_axis, e, arena);
        cjk_stems(x_axis, &segs, e, thr, arena)
    } else {
        let segs = latin_segments(x_axis, e, arena);
        latin_stems(x_axis, &segs, e, thr, arena)
    }
}

/// Close each contour up along y: its last point level with the one before takes the first along, else the
/// last goes to the first.
fn close_contours(e: &mut Element) {
    for (&s, &t) in e.sp.iter().zip(e.ep.iter()) {
        let (s, t) = (usize::from(s), usize::from(t));
        if t == 0 {
            continue;
        }
        if e.y[t] == e.y[t - 1] {
            e.y[s] = e.y[t];
        } else {
            e.y[t] = e.y[s];
        }
    }
}

/// Autohint a scaled glyph (rtgah 0x195760): `script` from which_script, `ch` its character.
pub fn rtgah(e: &mut Element, script: i32, ch: i32, g: &mut Globals) {
    if e.sp.is_empty() || g.fs_flags & 0x40_0000 == 0 || g.font_flags & 2 != 0 {
        return;
    }
    let saved: [i32; 6] = g.refs[13..19].try_into().unwrap();
    let latin = (1..=3).contains(&script) && g.refs[13] != 0;
    if latin && g.ppem <= 18 {
        grow(e, &mut g.refs);
    }
    let cjk = script | 1 == 7;
    level_extremes(e);
    let np = e.points();
    e.x[..np].copy_from_slice(&e.ox[..np]);
    e.y[..np].copy_from_slice(&e.oy[..np]);
    let thr = ((if cjk { 640 } else { 1408 } * u32::from(g.ppem)) | 0x30) / 100;
    let thr = thr as i32;
    if g.font_flags & 0x200 == 0 || g.refs[20] == 0 {
        let mut arena = Arena::new(ARENA);
        let mut st = stems(true, script, e, thr, &mut arena);
        let n = st.len();
        if g.refs[20] == 0 {
            let mut k = 0;
            for i in 1..n {
                if st[i].width < st[k].width {
                    k = i;
                }
            }
            let w = if n == 0 { 0 } else { st[k].width };
            let v = e.x[np + 1] - w / 2 * 2 - e.x[np] + 32;
            let whole = v & !63;
            let left = (whole / 2) & !63;
            g.refs[19] = left;
            g.refs[20] = (whole - left).max(64);
            if n > 2 && np >= 51 {
                g.g416 = 0;
                g.refs[13] = saved[0];
                g.refs[14] = (g.refs[14] & !0xFFFF) | (saved[1] & 0xFFFF);
                return;
            }
        }
        if g.fs_flags & 0x200_0000 == 0 {
            if latin {
                if n != 0 && g.refs[20] + g.refs[19] <= 128 {
                    latin_sides(e, &mut st, g.refs[19], g.refs[20]);
                }
                if (ch == 105 || ch == 106) && n >= 1 {
                    dot_on_stem(&mut st);
                }
            }
            if cjk {
                cjk_fit(&mut st);
            }
            apply_stems(true, script, &mut st, e, &arena, g.g418);
            if script != 7 {
                iup(e, true);
            }
        }
    }
    let mut arena = Arena::new(ARENA);
    let mut st = stems(false, script, e, thr, &mut arena);
    if let (Some(&s0), Some(&sl)) = (st.first(), st.last()) {
        g.bottom = s0.pos - s0.width / 2 + nearer(s0.pos, s0.width >> 1);
        g.top = sl.pos + sl.width / 2 + nearer(sl.pos, sl.width >> 1);
    }
    match script {
        1..=3 => latin_y(e, &mut st, &g.refs, &mut arena),
        _ => script_lines(script, &mut st, e, &g.refs, i32::from(g.ppem) << 6),
    }
    if cjk {
        cjk_fit(&mut st);
    }
    apply_stems(false, script, &mut st, e, &arena, g.g418);
    if script != 7 {
        iup(e, false);
        if g.g418 == 7 {
            close_contours(e);
        }
    }
    if script | 1 == 5 {
        right_to_left(e);
    }
    g.refs[13..19].copy_from_slice(&saved);
}

/// A small free contour (a dot) in a Hebrew or Arabic glyph: its contour, extreme points, original centre and
/// fitted centre (28-byte records).
#[derive(Clone, Copy)]
struct Dot {
    contour: usize,
    xmin: usize,
    xmax: usize,
    ymin: usize,
    ymax: usize,
    c: [i32; 2],
    to: [i32; 2],
}

/// Move the points of contour `k` by `d` along one axis.
fn shift_contour(e: &mut Element, k: usize, x_axis: bool, d: i32) {
    let a = if x_axis { &mut e.x } else { &mut e.y };
    for p in usize::from(e.sp[k])..=usize::from(e.ep[k]) {
        a[p] += d;
    }
}

/// The dots of a Hebrew or Arabic glyph (rtgah, scripts 4 and 5): one to three contours under three pixels that
/// no stem placed are centred on pixels, aligned with each other when within half a pixel, kept a pixel apart
/// where they were apart, and three in a triangle keep the odd one centred between the others.
fn right_to_left(e: &mut Element) {
    let mut dots: Vec<Dot> = Vec::new();
    for c in 0..e.sp.len() {
        let (s, t) = (usize::from(e.sp[c]), usize::from(e.ep[c]));
        if s == t {
            continue;
        }
        let (ox, oy) = (&e.ox, &e.oy);
        let (mut xmin, mut xmax, mut ymin, mut ymax) = (s, s, s, s);
        let mut placed = 0;
        for p in s + 1..=t {
            if ox[p] > ox[xmax] {
                xmax = p;
            }
            placed |= e.f[p];
            if ox[p] < ox[xmin] {
                xmin = p;
            }
            if oy[p] > oy[ymax] {
                ymax = p;
            }
            if oy[p] < oy[ymin] {
                ymin = p;
            }
        }
        if placed != 0 || ox[xmax] - ox[xmin] > 191 || oy[ymax] - oy[ymin] > 191 {
            continue;
        }
        let c2 = [(ox[xmin] + ox[xmax]) / 2, (oy[ymin] + oy[ymax]) / 2];
        dots.push(Dot { contour: c, xmin, xmax, ymin, ymax, c: c2, to: c2 });
        if dots.len() == 4 {
            return;
        }
    }
    let n = dots.len();
    if !(1..=3).contains(&n) {
        return;
    }
    for i in 0..n {
        let to = dots[i].to;
        for d in &mut dots[i + 1..] {
            for a in 0..2 {
                if (to[a] - d.to[a]).abs() <= 31 {
                    d.to[a] = to[a];
                }
            }
        }
    }
    for d in &mut dots {
        d.to = d.to.map(|v| (v & !63) | 32);
    }
    for d in &dots {
        for p in usize::from(e.sp[d.contour])..=usize::from(e.ep[d.contour]) {
            e.x[p] = d.to[0] - d.c[0] + e.ox[p];
            e.y[p] = d.to[1] - d.c[1] + e.oy[p];
            e.f[p] |= 3;
        }
    }
    if n < 2 {
        return;
    }
    for x_axis in [true, false] {
        let hi = |d: &Dot| if x_axis { d.xmax } else { d.ymax };
        let lo = |d: &Dot| if x_axis { d.xmin } else { d.ymin };
        let a = if x_axis { &e.x } else { &e.y };
        sort_by(&mut dots, |d| a[hi(d)]);
        for i in 0..n - 1 {
            for j in i + 1..n {
                let (p, q) = (hi(&dots[i]), lo(&dots[j]));
                let (a, o) = if x_axis { (&e.x, &e.ox) } else { (&e.y, &e.oy) };
                if o[p] >= o[q] {
                    continue;
                }
                let (r, l) = (a[p], a[q]);
                let (fr, fl, up) = (r & !63, l & !63, (l + 63) & !63);
                let apart = if r >= l {
                    true
                } else if fr == fl {
                    r - fr > 15 || up - l >= 16
                } else {
                    fr + 64 == fl && (r - fr > 31 || up - l >= 32)
                };
                if apart {
                    shift_contour(e, dots[j].contour, x_axis, 64);
                    dots[j].to[usize::from(!x_axis)] += 64;
                }
            }
        }
    }
    if n != 3 {
        return;
    }
    let (c, to): (Vec<i32>, Vec<i32>) = dots.iter().map(|d| (d.c[0], d.to[0])).unzip();
    let mut d = 0;
    let mut which = 0;
    if (c[0] - (c[2] + c[1]) / 2).abs() <= 31 {
        let m = (to[2] + to[1]) / 2;
        d = if to[0] - m > 32 { -64 } else if m - to[0] > 32 { 64 } else { 0 };
    }
    if (c[1] - (c[2] + c[0]) / 2).abs() < 32 {
        let m = (to[2] + to[0]) / 2;
        if to[1] - m > 32 {
            (d, which) = (-64, 1);
        } else if m - to[1] > 32 {
            (d, which) = (64, 1);
        }
    }
    if (c[2] - (c[1] + c[0]) / 2).abs() < 32 {
        let m = (to[1] + to[0]) / 2;
        if to[2] - m > 32 {
            (d, which) = (-64, 2);
        } else if m - to[2] > 32 {
            (d, which) = (64, 2);
        }
    }
    if d != 0 {
        shift_contour(e, dots[which].contour, true, d);
    }
}
