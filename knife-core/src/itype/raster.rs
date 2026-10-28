//! iType's anti-aliased scan converter (the SDK's make_graymap and its line-to-cell walker sub_18F260): the
//! outline's curves are flattened into lines, each line leaves a cell per pixel it crosses (the coverage left of
//! its midpoint in the pixel and its height there, 16.16), and a row's pixels are the running sum of the cells'
//! heights plus the pixel's own coverage, |sum| >> 8. The rows are done in bands of up to 512 cells: a band that
//! overflows makes the whole glyph start over with twice the bands, then with coarser curve flattening.

use super::fixed::{varmul, vardiv};

/// An outline as iType's rasterizer takes it, 16.16 pixels with y up.
#[derive(Clone, Debug, Default)]
pub struct Outline {
    /// 0 / 128 move to, 1 line to, 2 quadratic, 3 cubic (one, two or three points each)
    pub types: Vec<u8>,
    pub x: Vec<i32>,
    pub y: Vec<i32>,
    /// x min, x max, y min, y max
    pub bbox: [i32; 4],
}

/// An 8-bit graymap: `hi_y` is the top row's pixel y (up), `lo_x` the left column's x.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graymap {
    pub lo_x: i16,
    pub hi_y: i16,
    pub width: i16,
    pub height: i16,
    pub bits: Vec<u8>,
}

impl Graymap {
    pub fn at(&self, x: i32, y: i32) -> u8 {
        if (0..i32::from(self.width)).contains(&x) && (0..i32::from(self.height)).contains(&y) {
            self.bits[(y * i32::from(self.width) + x) as usize]
        } else {
            0
        }
    }
}

const MAX_CELLS: usize = 512;
const NONE: u16 = u16::MAX;

#[derive(Clone, Copy)]
struct Cell {
    x: i16,
    cover: i32,
    area: i32,
    next: u16,
}

/// The cells of one band: a list per row below the band's top.
struct Cells {
    cells: Vec<Cell>,
    rows: [u16; 256],
    bands: i32,
    y0: i32,
    y1: i32,
    top: i32,
    dir: i32,
}

struct Overflow;

/// The coverage left of a line piece's midpoint in its pixel, for a piece `d` high whose midpoint is `fx`
/// into the pixel.
fn partial(fx: u32, d: i32) -> i32 {
    let cov = 0x10000 - fx;
    if d >= 0x10000 {
        cov as i32
    } else if fx == 0 {
        d
    } else {
        (cov.wrapping_mul(d as u32).wrapping_add(0x8000) >> 16) as i32
    }
}

impl Cells {
    fn new() -> Self {
        Cells { cells: Vec::with_capacity(MAX_CELLS), rows: [NONE; 256], bands: 1, y0: 0, y1: 0, top: 0, dir: 1 }
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.rows = [NONE; 256];
    }

    /// The cell of a line piece from (xa, ya) up to (xb, yb), within one pixel.
    fn piece(&mut self, xa: i32, ya: i32, xb: i32, yb: i32) -> Result<(), Overflow> {
        if self.cells.len() >= MAX_CELLS {
            return Err(Overflow);
        }
        let r = self.top.wrapping_sub(ya.wrapping_add(yb) >> 17) as u32;
        if r <= 0xFF {
            let sx = xa.wrapping_add(xb);
            let d = yb.wrapping_sub(ya);
            let cover = partial(((sx >> 1) & 0xFFFF) as u32, d);
            let (cover, area) = if self.dir > 0 { (cover, d) } else { (cover.wrapping_neg(), d.wrapping_neg()) };
            self.cells.push(Cell { x: (sx >> 17) as i16, cover, area, next: self.rows[r as usize] });
            self.rows[r as usize] = (self.cells.len() - 1) as u16;
        }
        Ok(())
    }

    /// Clip a line's y range to the band (only when there are bands), moving its x along `inv` (dx / dy).
    fn clip(&self, (xlo, ylo, xhi, yhi): &mut (i32, i32, i32, i32), inv: i32) {
        if self.bands >= 2 {
            if *ylo < self.y0 {
                *xlo = *xhi - varmul(*yhi - self.y0, inv, 16);
                *ylo = self.y0;
            }
            if *yhi > self.y1 {
                *xhi = varmul(self.y1 - *ylo, inv, 16) + *xlo;
                *yhi = self.y1;
            }
        }
    }

    /// The cells of the line (x0, y0) - (x1, y1) (sub_18F260).
    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) -> Result<(), Overflow> {
        if y0 == y1 {
            return Ok(());
        }
        let (mut l, dir) = if y0 <= y1 { ((x0, y0, x1, y1), 1) } else { ((x1, y1, x0, y0), -1) };
        self.dir = dir;
        let (xlo, ylo, xhi, yhi) = l;
        let dx = xhi - xlo;
        let dy = yhi - ylo;
        let rows = ((yhi - 1) as u32 ^ ylo as u32) >> 16 != 0;
        let (clo, chi) = if xhi == xlo {
            (xlo as u32 & 0xFFFF_0000, xlo as u32 & 0xFFFF_0000)
        } else if dx < 1 {
            ((xlo - 1) as u32 & 0xFFFF_0000, xhi as u32 & 0xFFFF_0000)
        } else {
            (xlo as u32 & 0xFFFF_0000, (xhi - 1) as u32 & 0xFFFF_0000)
        };
        if !rows && clo == chi {
            return self.piece(xlo, ylo, xhi, yhi);
        }
        if xhi == xlo {
            // vertical: a piece per row
            if self.bands >= 2 {
                l.1 = l.1.max(self.y0);
                l.3 = l.3.min(self.y1);
            }
            let (x, ylo, _, yhi) = l;
            let mut y = ((ylo + 0x10000) as u32 & 0xFFFF_0000) as i32;
            y = y.min(yhi);
            self.piece(x, ylo, x, y)?;
            while i64::from(y) + 0x10000 < i64::from(yhi) {
                self.piece(x, y, x, y + 0x10000)?;
                y += 0x10000;
            }
            if yhi > y {
                self.piece(x, y, x, yhi)?;
            }
            return Ok(());
        }
        if clo == chi {
            // one column: a piece per row, x stepping by dx / dy
            let inv = vardiv(dx, dy, 16);
            self.clip(&mut l, inv);
            let (xlo, ylo, xhi, yhi) = l;
            let ny = ((ylo + 0x10000) as u32 & 0xFFFF_0000) as i32;
            let (mut x, mut y) = if ny <= yhi { (varmul(ny - ylo, inv, 16) + xlo, ny) } else { (xhi, yhi) };
            self.piece(xlo, ylo, x, y)?;
            while i64::from(y) + 0x10000 < i64::from(yhi) {
                self.piece(x, y, x + inv, y + 0x10000)?;
                x += inv;
                y += 0x10000;
            }
            if yhi > y {
                self.piece(x, y, xhi, yhi)?;
            }
            return Ok(());
        }
        let slope = vardiv(dy, dx, 16);
        if !rows {
            // one row: a piece per column, y stepping by dy / dx
            if dx < 1 {
                let mut x = ((xlo - 1) as u32 & 0xFFFF_0000) as i32;
                let mut y = varmul(x - xlo, slope, 16) + ylo;
                self.piece(xlo, ylo, x, y)?;
                while i64::from(x) - 0x10000 > i64::from(xhi) {
                    self.piece(x, y, x - 0x10000, y - slope)?;
                    x -= 0x10000;
                    y -= slope;
                }
                if x > xhi {
                    self.piece(x, y, xhi, yhi)?;
                }
            } else {
                let mut x = ((xlo + 0x10000) as u32 & 0xFFFF_0000) as i32;
                let mut y = varmul(x - xlo, slope, 16) + ylo;
                self.piece(xlo, ylo, x, y)?;
                while i64::from(x) + 0x10000 < i64::from(xhi) {
                    self.piece(x, y, x + 0x10000, y + slope)?;
                    x += 0x10000;
                    y += slope;
                }
                if x < xhi {
                    self.piece(x, y, xhi, yhi)?;
                }
            }
            return Ok(());
        }
        // rows and columns: step to whichever grid line comes first
        if slope == 0 {
            return Ok(());
        }
        let step = if (slope.wrapping_add(0x4000_0000) as u32) > 0x8000_0000 { slope >> 1 } else { slope };
        let inv = vardiv(dx, dy, 16);
        self.clip(&mut l, inv);
        let (mut xlo, mut ylo, xhi, yhi) = l;
        let mut ny = ((ylo + 0x10000) as u32 & 0xFFFF_0000) as i32;
        let mut x_ny = varmul(ny - ylo, inv, 16) + xlo;
        let (mut nx, nx_step, y_step) = if dx <= 0 {
            (((xlo - 1) as u32 & 0xFFFF_0000) as i32, -0x10000, -step)
        } else {
            (((xlo + 0x10000) as u32 & 0xFFFF_0000) as i32, 0x10000, step)
        };
        let mut y_nx = varmul(nx - xlo, step, 16) + ylo;
        if ny <= yhi || y_nx <= yhi {
            let mut col = ny >= y_nx;
            loop {
                if col {
                    self.piece(xlo, ylo, nx, y_nx)?;
                    xlo = nx;
                    nx += nx_step;
                    ylo = y_nx;
                    y_nx += y_step;
                } else {
                    self.piece(xlo, ylo, x_ny, ny)?;
                    ylo = ny;
                    ny += 0x10000;
                    xlo = x_ny;
                    x_ny += inv;
                }
                if ny > yhi {
                    if y_nx > yhi {
                        break;
                    }
                    col = true;
                } else {
                    col = ny >= y_nx;
                }
            }
        }
        if yhi == ylo && xlo == xhi {
            return Ok(());
        }
        self.piece(xlo, ylo, xhi, yhi)
    }
}

/// The flattening tolerance at a size (16.16 px): finer below 16 ppem, coarser above 64.
fn tolerance(ppem: u16) -> i32 {
    match ppem {
        0..=15 => 2048,
        16..=64 => i32::from(ppem) << 7,
        _ => 0x2000,
    }
}

/// Rasterize an outline at `ppem` (make_graymap without its effects and trimming).
pub fn graymap(o: &Outline, ppem: u16) -> Option<Graymap> {
    let [x_min, x_max, y_min, y_max] = o.bbox;
    let lo_x = x_min >> 16;
    let hi_x = (x_max + 0xFFFF) >> 16;
    let top = ((y_max + 0xFFFF) as u32 >> 16) as u16 as i16;
    let lo_y = y_min >> 16;
    let width = (hi_x - lo_x) as i16;
    let height = (i32::from(top) - lo_y) as i16;
    let mut map = Graymap {
        lo_x: lo_x as i16,
        hi_y: top.wrapping_sub(1),
        width,
        height,
        bits: vec![0; (i32::from(width) * i32::from(height)).max(0) as usize],
    };
    if o.types.is_empty() {
        return Some(map);
    }
    // the outline's length sets the first band count
    let mut length = 0i32;
    let (mut px, mut py) = (0, 0);
    let mut p = 0;
    for &t in &o.types {
        let n = match t {
            0 | 128 => {
                (px, py) = (o.x[p], o.y[p]);
                p += 1;
                continue;
            }
            1..=3 => usize::from(t),
            _ => continue,
        };
        for _ in 0..n {
            length = length.wrapping_add((o.x[p] - px).abs()).wrapping_add((o.y[p] - py).abs());
            (px, py) = (o.x[p], o.y[p]);
            p += 1;
        }
    }
    let mut c = Cells::new();
    c.bands = (length >> 16) / 512 + 1;
    let mut tol = tolerance(ppem);
    let mut retried = false;
    let width = i32::from(map.width);
    let mut cover = vec![0i32; width.max(0) as usize];
    let mut area = vec![0i32; width.max(0) as usize];
    let mut overflowed = false;
    'restart: loop {
        if std::mem::take(&mut overflowed) {
            if !retried || c.bands <= i32::from(map.height) {
                c.bands *= 2;
                retried = true;
            } else {
                if tol > 0x8000 {
                    return None;
                }
                tol += 0x2000;
            }
        }
        for band in 0..c.bands {
            c.clear();
            let bh = (((y_max - y_min) / c.bands + 0x8000) as u32 & 0xFFFF_0000) as i32;
            c.y0 = (y_min as u32 & 0xFFFF_0000) as i32 + bh * band;
            c.y1 = if band == c.bands - 1 { ((y_max + 0xFFFF) as u32 & 0xFFFF_0000) as i32 } else { c.y0 + bh };
            c.top = c.y1 >> 16;
            if flatten(o, &mut c, tol).is_err() {
                overflowed = true;
                continue 'restart;
            }
            let rows = c.top - (c.y0 >> 16) + 1;
            if rows <= 1 || rows > 0xFF {
                continue;
            }
            for r in 1..rows {
                cover.fill(0);
                area.fill(0);
                let mut i = c.rows[r as usize];
                while i != NONE {
                    let cell = c.cells[usize::from(i)];
                    let col = (i32::from(cell.x) - i32::from(map.lo_x)).max(0);
                    if col < width {
                        cover[col as usize] = cover[col as usize].wrapping_add(cell.cover);
                        area[col as usize] = area[col as usize].wrapping_add(cell.area);
                    }
                    i = cell.next;
                }
                let row = i32::from(map.hi_y) - c.top + r;
                let mut sum = 0i32;
                for x in 0..width as usize {
                    let v = sum.wrapping_add(cover[x]);
                    if v != 0 {
                        map.bits[(row * width) as usize + x] = (v.unsigned_abs() >> 8).min(0xFF) as u8;
                    }
                    sum = sum.wrapping_add(area[x]);
                }
            }
        }
        return Some(map);
    }
}

/// Feed the band's part of the outline to the cells: lines as they are, curves split in halves until flat
/// within `tol` (or the split stack is full), skipping parts entirely above or below the band.
fn flatten(o: &Outline, c: &mut Cells, tol: i32) -> Result<(), Overflow> {
    let (mut cx, mut cy) = (0, 0);
    let mut p = 0;
    let mut stack: Vec<i32> = Vec::with_capacity(64);
    let outside = |c: &Cells, ys: &[i32]| {
        let lo = *ys.iter().min().unwrap();
        let hi = *ys.iter().max().unwrap();
        lo > c.y1 || hi < c.y0
    };
    for &t in &o.types {
        match t {
            0 | 128 => {
                (cx, cy) = (o.x[p], o.y[p]);
                p += 1;
            }
            1 => {
                let (x, y) = (o.x[p], o.y[p]);
                p += 1;
                if cy.min(y) <= c.y1 && cy.max(y) >= c.y0 {
                    c.line(cx, cy, x, y)?;
                }
                (cx, cy) = (x, y);
            }
            2 => {
                stack.clear();
                stack.extend([cx, cy, o.x[p], o.y[p], o.x[p + 1], o.y[p + 1]]);
                (cx, cy) = (o.x[p + 1], o.y[p + 1]);
                p += 2;
                while !stack.is_empty() {
                    let b = stack.len() - 6;
                    let [x0, y0, x1, y1, x2, y2] = stack[b..b + 6] else { unreachable!() };
                    if outside(c, &[y0, y1, y2]) {
                        stack.truncate(b);
                        continue;
                    }
                    let (ax, bx) = ((x0 + x1) >> 1, (x1 + x2) >> 1);
                    let (ay, by) = ((y0 + y1) >> 1, (y1 + y2) >> 1);
                    let (mx, my) = ((ax + bx) >> 1, (ay + by) >> 1);
                    let (ex, ey) = ((x1 - mx).abs(), (y1 - my).abs());
                    let dist = ex.max(ey) + ((3 * ex.min(ey)) as u32 >> 3) as i32;
                    if b <= 47 && dist >= tol {
                        stack[b..b + 4].copy_from_slice(&[mx, my, bx, by]);
                        stack.extend([x0, y0, ax, ay, mx, my]);
                    } else {
                        stack.truncate(b);
                        c.line(x0, y0, x2, y2)?;
                    }
                }
            }
            3 => {
                stack.clear();
                stack.extend([cx, cy, o.x[p], o.y[p], o.x[p + 1], o.y[p + 1], o.x[p + 2], o.y[p + 2]]);
                (cx, cy) = (o.x[p + 2], o.y[p + 2]);
                p += 3;
                while !stack.is_empty() {
                    let b = stack.len() - 8;
                    let [x0, y0, x1, y1, x2, y2, x3, y3] = stack[b..b + 8] else { unreachable!() };
                    if outside(c, &[y0, y1, y2, y3]) {
                        stack.truncate(b);
                        continue;
                    }
                    let (ax, bx, cx2) = ((x0 + x1) >> 1, (x1 + x2) >> 1, (x2 + x3) >> 1);
                    let (ay, by, cy2) = ((y0 + y1) >> 1, (y1 + y2) >> 1, (y2 + y3) >> 1);
                    let (abx, aby) = ((ax + bx) >> 1, (ay + by) >> 1);
                    let (bcx, bcy) = ((bx + cx2) >> 1, (by + cy2) >> 1);
                    let dist = super::fixed::norm(x1 - abx, y1 - aby).max(super::fixed::norm(x2 - bcx, y2 - bcy));
                    if b > 47 || dist < tol {
                        stack.truncate(b);
                        c.line(x0, y0, x3, y3)?;
                    } else {
                        let (mx, my) = ((abx + bcx) >> 1, (aby + bcy) >> 1);
                        stack[b..b + 6].copy_from_slice(&[mx, my, bcx, bcy, cx2, cy2]);
                        stack.extend([x0, y0, ax, ay, abx, aby, mx, my]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
