//! Flowchart chart layout, ported from cFlowchart (sub_FFFC and the helpers it calls).
//!
//! Nodes sit on a 96 x 56 px grid (column x 438 + 96c, row pitch 56); each phase is a section with a
//! flow_sam_bg band and phase titles at x 208. Connector pieces come from flow_line_merge: grey where
//! the edge into a node was ever taken, lit (cyan) where it was taken in the current playthrough.

use crate::flow_table::{LINES, ROWS, TITLES};

/// Node bit range of RDST/RDED/RTST (0..0x240).
pub const NODE_BITS: usize = 0x240;
/// Edge flags (system words 400+ / playthrough words 600+) are numbered from 564.
pub const EDGE_BASE: i32 = 564;
pub const EDGE_BITS: usize = 0x2C0;

/// Progress bits the chart is drawn from.
pub struct Flags {
    /// RDED: node finished (sub_922C0)
    pub completed: Vec<bool>,
    /// RDST: node started (sub_926B0)
    pub read: Vec<bool>,
    /// edge taken in any playthrough (sub_92300)
    pub edge: Vec<bool>,
    /// RTST in this playthrough (sub_92340)
    pub route: Vec<bool>,
    /// edge taken in this playthrough (sub_92380)
    pub route_edge: Vec<bool>,
}

impl Flags {
    fn bit(v: &[bool], i: i32) -> bool {
        usize::try_from(i).ok().and_then(|i| v.get(i)).copied().unwrap_or(false)
    }
    fn completed(&self, n: i32) -> bool {
        Self::bit(&self.completed, n)
    }
    fn read(&self, n: i32) -> bool {
        Self::bit(&self.read, n)
    }
    fn route(&self, n: i32) -> bool {
        Self::bit(&self.route, n)
    }
    fn edge(&self, f: i32) -> bool {
        f >= 1 && Self::bit(&self.edge, f - EDGE_BASE)
    }
    fn route_edge(&self, f: i32) -> bool {
        f >= 1 && Self::bit(&self.route_edge, f - EDGE_BASE)
    }
}

/// Edge flags set when the story moves from node `from` to node `to` (sub_954B0). Branch points
/// that are not RTST nodes themselves (ids 497+) are crossed as `from -> virtual -> to`.
/// Flowchart jump target of node `n`: its row index and scene id (record field 3, the id sub_178C8 passes on
/// to the scene load as F026).
pub fn node_entry(n: u16) -> Option<(usize, i32)> {
    crate::flow_table::ROWS
        .iter()
        .enumerate()
        .find_map(|(i, (_, recs))| recs.iter().find(|r| r[0] == i32::from(n) && r[3] >= 0).map(|r| (i, r[3])))
}

/// A textured piece of the chart in content coordinates (y not yet scrolled).
pub struct Sprite {
    pub tex: &'static str,
    pub dst: [f32; 4],
    pub src: [f32; 4],
    pub prio: i32,
}

/// A node the cursor can select.
pub struct Node {
    pub node: i32,
    pub pos: [f32; 2],
    pub row: usize,
    /// preview picture: allpaccg entry thumcg_top.mzp + 1 + n
    pub preview: i32,
    /// SYSMES title / description
    pub name: i32,
    pub desc: i32,
}

pub struct Chart {
    pub sprites: Vec<Sprite>,
    pub nodes: Vec<Node>,
    /// phase titles by phase number (cFlowchart + 104 + 24 phase): drawn pinned to the top of the view
    pub titles: Vec<Vec<Sprite>>,
    /// y of the upper divider of each section change, in order (every third entry of cFlowchart + 560); the
    /// titles of phase p are pushed up by entry p
    pub limits: Vec<f32>,
    /// node y of each shown row (row record + 24)
    pub row_y: Vec<f32>,
    /// scroll range (sub_FFFC end: sub_14B58 end + 56 + 4 - 1022, less 28 when that leaves at most 1022,
    /// less 56 when the last shown row is table row 276)
    pub scroll_max: f32,
}

pub const NODE_W: f32 = 104.0;
pub const NODE_H: f32 = 64.0;

const DRAWN: u32 = 1 << 31;
const LIT: u32 = 1 << 30;

struct Rec {
    f: [i32; 9],
    flags: u32,
}

struct Row {
    /// index into ROWS
    index: usize,
    phase: u8,
    recs: Vec<Rec>,
}

/// Rows worth showing (first loop of sub_FFFC): a row is kept when it holds a reached node, or a
/// placeholder whose column changes further down the same phase.
fn visible_rows(fl: &Flags) -> Vec<Row> {
    let mut kept = Vec::new();
    for (i, &(phase, recs)) in ROWS.iter().enumerate() {
        let (mut shown, mut wide) = (0, 0);
        for (j, r) in recs.iter().enumerate() {
            let n = r[0];
            if !(n >= 1 && fl.completed(n)) && !fl.edge(r[2]) {
                continue;
            }
            shown += 1;
            if i == 0 {
                wide += (n != 0) as i32;
                continue;
            }
            if n > 0 {
                wide += 1;
                continue;
            }
            if n == 0 {
                continue;
            }
            let prev = ROWS[i - 1].1;
            let above = prev.get(j).map_or(0, |p| p[0]);
            if !fl.completed(-n) {
                wide += (j > prev.len() || above + n != 0) as i32;
                continue;
            }
            if j > prev.len() || above.abs() != n.abs() {
                wide += 1;
                continue;
            }
            if i <= 0x113 {
                for &(ph, next) in &ROWS[i + 1..] {
                    if j > next.len() {
                        wide += 1;
                        break;
                    }
                    if ph > phase {
                        break;
                    }
                    match next.get(j) {
                        Some(x) if x[0] == n && x[2] == r[2] => {}
                        _ => {
                            wide += 1;
                            break;
                        }
                    }
                }
            }
        }
        if (wide > 0 && shown > 1) || (shown == 1 && (wide == 1 || recs.len() == 1)) {
            kept.push(Row { index: i, phase, recs: recs.iter().map(|&f| Rec { f, flags: 0 }).collect() });
        }
    }
    kept
}

struct Builder<'a> {
    fl: &'a Flags,
    rows: Vec<Row>,
    sprites: Vec<Sprite>,
}

impl Builder<'_> {
    fn push(&mut self, tex: &'static str, x: f32, y: f32, src: [f32; 4], prio: i32) {
        self.sprites.push(Sprite { tex, dst: [x, y, src[2], src[3]], src, prio });
    }

    fn cell(rc: [i16; 4]) -> [f32; 4] {
        rc.map(f32::from)
    }

    /// sub_13B54: section band from `top` to `bottom`; the next band starts 10 px lower.
    fn band(&mut self, top: f32, bottom: f32) -> f32 {
        let h = bottom - top + 3.0;
        self.sprites.push(Sprite { tex: "flow_sam_bg_middle_fullhd", dst: [0.0, top, 1440.0, h], src: [0.0, 0.0, 1440.0, 1.0], prio: 460 });
        bottom + 10.0
    }

    /// sub_13D04: section divider (lower edge of the band above, upper edge of the next).
    fn divider(&mut self, y: f32) {
        self.push("flow_sam_bg_under_fullhd", 0.0, y, [0.0, 0.0, 1440.0, 5.0], 450);
        self.push("flow_sam_bg_upper_fullhd", 0.0, y + 8.0, [0.0, 0.0, 1440.0, 5.0], 450);
    }

    fn rec(&self, k: usize, j: usize) -> Option<&Rec> {
        self.rows.get(k)?.recs.get(j)
    }

    fn rec_at(&self, k: usize, j: isize) -> Option<&Rec> {
        usize::try_from(j).ok().and_then(|j| self.rec(k, j))
    }

    /// Neighbour has a grey connector (or will: reached / its edge was taken).
    fn grey_on(&self, k: usize, j: isize) -> bool {
        self.rec_at(k, j)
            .is_some_and(|r| r.flags & DRAWN != 0 || (r.f[0] >= 1 && self.fl.completed(r.f[0])) || self.fl.edge(r.f[2]))
    }

    fn lit_on(&self, k: usize, j: isize) -> bool {
        self.rec_at(k, j)
            .is_some_and(|r| r.flags & LIT != 0 || (r.f[0] >= 1 && self.fl.route(r.f[0])) || self.fl.route_edge(r.f[2]))
    }

    fn up_with(&self, k: usize, j: usize, f: impl Fn(i32) -> bool) -> bool {
        k >= 1 && self.rec(k - 1, j).is_some_and(|p| p.f[0] != 0 && f(p.f[0].abs()))
    }

    /// sub_16078
    fn self_grey(&self, k: usize, j: usize) -> bool {
        let Some(r) = self.rec(k, j) else { return false };
        match r.f[0] {
            0 => false,
            n if n < 0 => self.fl.completed(-n) || k + 1 == self.rows.len(),
            n => self.fl.completed(n) || self.fl.edge(r.f[2]),
        }
    }

    fn self_lit(&self, k: usize, j: usize) -> bool {
        self.rec(k, j).is_some_and(|r| r.f[0] != 0 && self.fl.route(r.f[0].abs()))
    }

    /// Connector kind for the branch / merge kinds 5, 8, 10, 11 (sub_14D10 .. sub_15218 grey,
    /// sub_158B0 .. sub_15E80 lit), from which neighbours are connected.
    fn shape(&self, kind: i32, k: usize, j: usize, lit: bool) -> i32 {
        let fl = self.fl;
        let ji = j as isize;
        let on = |jj| if lit { self.lit_on(k, jj) } else { self.grey_on(k, jj) };
        let up = if lit { self.up_with(k, j, |n| fl.route(n)) } else { self.up_with(k, j, |n| fl.completed(n)) };
        let me = || if lit { self.self_lit(k, j) } else { self.self_grey(k, j) };
        let (left, right) = (on(ji - 1), on(ji + 1));
        match kind {
            5 => {
                if !up {
                    4
                } else if right {
                    if me() { 5 } else { 2 }
                } else if me() {
                    3
                } else {
                    5
                }
            }
            8 => match (up, left, right) {
                (false, ..) => 7,
                (_, true, r) => if r { 8 } else { 6 },
                (_, false, r) => if r { 2 } else { 8 },
            },
            10 => {
                if !up {
                    9
                } else if left {
                    if me() { 10 } else { 6 }
                } else if me() {
                    3
                } else {
                    10
                }
            }
            11 => match (me(), left, right) {
                (false, ..) => 7,
                (_, true, r) => if r { 11 } else { 9 },
                (_, false, r) => if r { 4 } else { 11 },
            },
            k => k,
        }
    }

    /// sub_153B8: draw connector `kind` above a node; `first`: first row of a section.
    fn line(&mut self, k: usize, j: usize, first: bool, lit: bool, kind: i32, x: f32, y: f32) {
        let base = if lit { 13 } else { 0 };
        let prio = if lit { 440 } else { 450 };
        let kind = if (0..=25).contains(&kind) { kind as usize } else { 12 };
        let cell = |i: usize| Self::cell(LINES.get(i).copied().unwrap_or([0, 0, 4, 4]));
        self.push("flow_line_merge", x, y, cell(kind + base), prio);
        let n = self.rec(k, j).map_or(0, |r| r.f[0]);
        if kind == 3 {
            if first {
                self.push("flow_line_merge", x, y - 13.0, cell(base), prio);
            }
        } else if kind <= 11 && (0xE30 >> kind) & 1 != 0 && n < 0 {
            let mut c = self.fl.read(-n);
            if lit && !c {
                c = self.fl.route(-n);
            }
            if c {
                self.push("flow_line_merge", x, y + 14.0, cell(base + 1), prio);
            }
        }
    }

    /// sub_14824: grey connector.
    fn grey(&mut self, k: usize, j: usize, first: bool, x: f32, y: f32) -> bool {
        let Some(r) = self.rec(k, j) else { return false };
        if r.flags & DRAWN == 0 && !self.fl.edge(r.f[2]) {
            return false;
        }
        let (n, kind) = (r.f[0], r.f[1]);
        let kind = match kind {
            3 if n < 0 => {
                let Some(p) = k.checked_sub(1).and_then(|k| self.rec(k, j)) else { return false };
                let ok = p.flags & DRAWN != 0
                    && match p.f[0] {
                        0 => false,
                        pn if pn < 0 => self.fl.read(-pn),
                        pn => self.fl.completed(pn),
                    };
                if !ok {
                    return false;
                }
                3
            }
            5 | 8 | 10 | 11 => self.shape(kind, k, j, false),
            kind => kind,
        };
        if kind < 0 {
            return false;
        }
        self.line(k, j, first, false, kind, x, y);
        true
    }

    /// sub_149B4: lit connector along the current playthrough.
    fn lit(&mut self, k: usize, j: usize, first: bool, x: f32, y: f32) -> bool {
        let Some(r) = self.rec(k, j) else { return false };
        if r.flags & LIT == 0 && !self.fl.route_edge(r.f[2]) {
            return false;
        }
        let (n, kind) = (r.f[0], r.f[1]);
        let kind = match kind {
            3 if n < 0 => {
                let ok = k >= 1
                    && self.rec(k - 1, j).is_some_and(|p| {
                        let a = p.f[0].abs();
                        p.flags & LIT != 0 && a >= 1 && self.fl.route(a)
                    });
                if !ok {
                    return true;
                }
                3
            }
            5 | 8 | 10 | 11 => self.shape(kind, k, j, true),
            kind => kind,
        };
        if kind >= 0 {
            self.line(k, j, first, true, kind, x, y);
        }
        true
    }

    /// sub_13FD4: the node picture. Returns whether the cursor may select it.
    fn node(&mut self, k: usize, j: usize, gallery: bool, x: f32, y: f32) -> bool {
        let fl = self.fl;
        let phase = self.rows[k].phase;
        let r = &self.rows[k].recs[j];
        let (n, edge, kind) = (r.f[0], r.f[2], r.f[1]);
        if n == 0 {
            return false;
        }
        if n < 0 {
            if fl.completed(-n) || !fl.edge(edge) || fl.read(-n) || (!gallery && fl.route(-n)) || kind == 3 {
                return false;
            }
        } else if !fl.completed(n) && !fl.edge(edge) {
            return false;
        }
        let (thumb, select) = if gallery {
            let c = n >= 1 && fl.completed(n);
            (c, c)
        } else if n < 1 {
            (false, false)
        } else {
            let rt = fl.route(n);
            if rt {
                self.rows[k].recs[j].flags |= LIT;
            }
            (fl.completed(n), phase > 0 && rt)
        };
        let t = self.rows[k].recs[j].f[5];
        if thumb && t >= 0 {
            let src = [(104 * (t % 20)) as f32 + 1.0, (64 * (t / 20)) as f32 + 1.0, 102.0, 62.0];
            self.sprites.push(Sprite { tex: "flow_thumb_all_nx", dst: [x + 1.0, y + 1.0, 102.0, 62.0], src, prio: 400 });
        } else {
            self.push("flow_sam_non", x, y, [0.0, 0.0, NODE_W, NODE_H], 400);
        }
        select
    }
}

/// Lay out the chart. `gallery`: opened from the title's Extra menu (no current playthrough).
pub fn build(fl: &Flags, gallery: bool) -> Chart {
    let mut b = Builder { fl, rows: visible_rows(fl), sprites: Vec::new() };
    let mut nodes = Vec::new();
    let mut titles: Vec<Vec<Sprite>> = Vec::new();
    let (mut limits, mut row_y) = (Vec::new(), Vec::new());
    let (mut y, mut title_y, mut band_top) = (0.0f32, 0.0f32, 0.0f32);
    let (mut phase, mut count, mut last_title) = (0u8, 0, -1);
    for k in 0..b.rows.len() {
        let first = b.rows[k].phase != phase;
        if first {
            band_top = b.band(band_top, y + 8.0);
            b.divider(y + 8.0);
            limits.push(y + 16.0);
            y += 13.0;
            phase = b.rows[k].phase;
            title_y = y;
            count = 0;
            last_title = -1;
        }
        let node_y = y + 4.0;
        row_y.push(node_y);
        let slot = usize::from(b.rows[k].phase);
        if titles.len() <= slot {
            titles.resize_with(slot + 1, Vec::new);
        }
        let title_at = |y: f32, cell: [f32; 4]| Sprite { tex: "flow_phasetitle", dst: [208.0, y, cell[2], cell[3]], src: cell, prio: 430 };
        for j in 0..b.rows[k].recs.len() {
            let f = b.rows[k].recs[j].f;
            let title = f[4];
            if title >= 0 && count <= 2 && title > last_title && fl.edge(f[2]) {
                let cell = Builder::cell(TITLES.get(title as usize).copied().unwrap_or([0, 0, 0, 0]));
                titles[slot].push(title_at(title_y + 8.0, cell));
                title_y += 32.0;
                last_title = title;
                if title != 0 {
                    count += 1;
                } else {
                    titles[slot].push(title_at(title_y + 8.0, Builder::cell(TITLES[TITLES.len() - 1])));
                    title_y += 32.0;
                    count += 2;
                }
            }
            let x = j as f32 * 96.0 + 438.0;
            if b.node(k, j, gallery, x, node_y) {
                nodes.push(Node { node: f[0], pos: [x, node_y], row: k, preview: f[6], name: f[7], desc: f[8] });
            }
            if b.grey(k, j, first, x, node_y - 5.0) {
                b.rows[k].recs[j].flags |= DRAWN;
            }
            if !gallery && b.lit(k, j, first, x, node_y - 5.0) {
                b.rows[k].recs[j].flags |= LIT;
            }
        }
        y += 56.0;
    }
    let end = y.max(title_y) + 8.0;
    b.band(band_top, end);
    b.push("flow_sam_bg_under_fullhd", 0.0, end, [0.0, 0.0, 1440.0, 5.0], 450);
    // farther pieces first
    b.sprites.sort_by_key(|s| std::cmp::Reverse(s.prio));
    let mut scroll_max = end + 56.0 + 4.0 - 1022.0;
    if scroll_max - 28.0 <= 1022.0 {
        scroll_max -= 28.0;
    }
    if b.rows.last().is_some_and(|r| r.index == 276) {
        scroll_max -= 56.0;
    }
    Chart { sprites: b.sprites, nodes, titles, limits, row_y, scroll_max: scroll_max.max(0.0) }
}
