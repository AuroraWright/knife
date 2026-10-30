//! The TrueType instruction interpreter as the SDK runs it: fnt_Execute with its instruction table (interpFunc,
//! 0xC26810), the font program once per size (fs_NewTransformation), the control value program whenever a glyph
//! of another size or font was the last one fitted (fsg_GridFit, fsg_RunPreProgram) and a glyph's own program
//! (do_hints). The SDK checks nothing; where it would read or write out of bounds, jump outside a program or loop
//! without end, the program stops here as on one of its own errors.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::fixed::{frac_sqrt, muldiv, vardiv, varmul};
use super::tt::{self, Element, Scale};

/// A program's error (fnt_Execute's longjmp: 416 for ENDF outside FDEF or an instruction no IDEF defines).
#[derive(Debug)]
pub struct Error;

type R<T = ()> = Result<T, Error>;

/// Instructions one program may run before it is taken to loop for ever.
const BUDGET: u32 = 10_000_000;

/// The deepest CALL / LOOPCALL / IDEF nesting.
const DEPTH: u32 = 128;

/// The most stack entries (the SDK's stack runs on into the twilight zone's arrays past maxStackElements).
const STACK: usize = 0x10000;

/// fnt_ParameterBlock: the graphics state each glyph program starts from (GS+60, fsg_SetDefaults) and the one
/// a program changes (GS+112).
#[derive(Clone, Copy, Debug)]
struct Params {
    /// control value cut-in
    wtci: i32,
    /// single width cut-in
    swci: i32,
    /// the single width, scaled
    scaled_sw: i32,
    /// SCANCTRL in the low half, SCANTYPE in the high
    scan_control: i32,
    instruct_control: i32,
    min_dist: i32,
    /// 0 SROUND, 1 S45ROUND, 2 RTG, 3 RTHG, 4 RTDG, 5 ROFF, 6 RDTG, 7 RUTG
    round_state: i32,
    /// SROUND's period as a mask
    period_mask: i32,
    /// S45ROUND's period, 2.30
    period45: i32,
    phase: i16,
    threshold: i16,
    delta_base: u16,
    delta_shift: u16,
    sw: i16,
    auto_flip: bool,
}

const DEFAULTS: Params = Params {
    wtci: 68,
    swci: 0,
    scaled_sw: 0,
    scan_control: 0,
    instruct_control: 0,
    min_dist: 64,
    round_state: 2,
    period_mask: 0,
    period45: 0,
    phase: 0,
    threshold: 0,
    delta_base: 9,
    delta_shift: 3,
    sw: 0,
    auto_flip: true,
};

/// The graphics state the font program runs with: fs_NewTransformation fills only the defaults.
const ZERO: Params = Params {
    wtci: 0,
    swci: 0,
    scaled_sw: 0,
    scan_control: 0,
    instruct_control: 0,
    min_dist: 0,
    round_state: 0,
    period_mask: 0,
    period45: 0,
    phase: 0,
    threshold: 0,
    delta_base: 0,
    delta_shift: 0,
    sw: 0,
    auto_flip: false,
};

/// A FDEF or IDEF: its program and where its body lies (the SDK keeps an offset into the program it was defined
/// in), the instruction an IDEF defines.
#[derive(Clone, Default)]
struct Def {
    code: Option<Arc<[u8]>>,
    start: usize,
    len: usize,
    opcode: u8,
}

/// The key last fitted with (fsg_GridFit compares the SDK's per-context key pointer).
static LAST: AtomicU64 = AtomicU64::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);

/// One font's interpreter state at one size (the SDK's key): the font and control value programs, storage,
/// the scaled control values, the function and instruction definitions and the twilight zone, which carry over
/// from one glyph to the next.
pub struct Interp {
    id: u64,
    fpgm: Arc<[u8]>,
    prep: Arc<[u8]>,
    cvt_fu: Vec<i16>,
    cvt: Vec<i32>,
    storage: Vec<i32>,
    funcs: Vec<Def>,
    idefs: Vec<Def>,
    max_idefs: usize,
    twilight: Element,
    defaults: Params,
    cur: Params,
    scale: Scale,
    ppem: i32,
    /// fsg_RunPreProgram ran once (GS+288, never cleared): INSTCTRL works, the single width is left as SSW scaled
    /// it, and the ppem and point size are set (the font program sees 0)
    prepped: bool,
    /// the font program failed: the size cannot be used (fs_NewTransformation's error)
    broken: bool,
}

/// What a glyph is fitted with besides the interpreter: the autohinter, which an AA[] with selector 11 runs on
/// the glyph's points.
pub type Autohint<'a> = &'a mut dyn FnMut(&mut Element);

impl Interp {
    /// A font's interpreter at `ppem` whole pixels per em with its scale: runs the font program.
    pub fn new(data: &[u8], scale: Scale, ppem: u16) -> Interp {
        let table = |tag: &[u8; 4]| -> &[u8] {
            let n = data.get(4..6).map_or(0, |b| usize::from(u16::from_be_bytes([b[0], b[1]])));
            for i in 0..n {
                let r = 12 + 16 * i;
                let Some(rec) = data.get(r..r + 16) else { break };
                if &rec[..4] == tag {
                    let off = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize;
                    let len = u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize;
                    return data.get(off..off.saturating_add(len)).unwrap_or(&[]);
                }
            }
            &[]
        };
        let maxp = table(b"maxp");
        let m = |at: usize| maxp.get(at..at + 2).map_or(0, |b| usize::from(u16::from_be_bytes([b[0], b[1]])));
        let twilight_points = m(16);
        let mut twilight = Element {
            x: vec![0; twilight_points],
            y: vec![0; twilight_points],
            ox: vec![0; twilight_points],
            oy: vec![0; twilight_points],
            oox: vec![0; twilight_points],
            ooy: vec![0; twilight_points],
            on: vec![0; twilight_points],
            f: vec![0; twilight_points],
            sp: vec![0],
            ep: vec![(twilight_points as u16).wrapping_sub(1)],
        };
        if twilight_points == 0 {
            twilight.sp.clear();
            twilight.ep.clear();
        }
        let cvt_fu = table(b"cvt ").as_chunks::<2>().0.iter().map(|&b| i16::from_be_bytes(b)).collect::<Vec<_>>();
        let mut it = Interp {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            fpgm: Arc::from(table(b"fpgm")),
            prep: Arc::from(table(b"prep")),
            cvt: vec![0; cvt_fu.len()],
            cvt_fu,
            storage: vec![0; m(18).max(4)],
            funcs: vec![Def::default(); m(20)],
            idefs: Vec::new(),
            max_idefs: m(22),
            twilight,
            defaults: DEFAULTS,
            cur: ZERO,
            scale,
            ppem: i32::from(ppem),
            prepped: false,
            broken: false,
        };
        // fsg_RunFontProgram, then scale_font_ttf's first fsg_RunPreProgram: a failure in either makes the
        // size unusable
        if !it.fpgm.is_empty() {
            let code = it.fpgm.clone();
            let mut none = Element::default();
            it.broken = it.run(&code, 1, &mut none, None).is_err();
        }
        if !it.broken {
            it.broken = it.run_prep().is_err();
            if !it.broken {
                LAST.store(it.id, Ordering::Relaxed);
            }
        }
        it
    }

    /// fsg_GridFit's start: the control value program runs unless this size was the last one fitted (any font);
    /// then whether glyphs are fitted at all (INSTCTRL's first bit clear) or None when the size cannot be used
    /// or the control value program failed.
    pub fn begin(&mut self) -> Option<bool> {
        if self.broken {
            return None;
        }
        if LAST.swap(self.id, Ordering::Relaxed) != self.id {
            self.run_prep().ok()?;
        }
        Some(self.cur.instruct_control & 1 == 0)
    }

    /// fsg_RunPreProgram: the defaults, the control values scaled afresh, the twilight zone's points cleared, then
    /// the program; its graphics state becomes the glyphs' unless it set INSTCTRL's second bit.
    fn run_prep(&mut self) -> R {
        self.prepped = true;
        self.defaults = DEFAULTS;
        self.cur = DEFAULTS;
        self.cvt = self.cvt_fu.iter().map(|&v| self.scale.apply(i32::from(v))).collect();
        let t = &mut self.twilight;
        for v in [&mut t.x, &mut t.y, &mut t.ox, &mut t.oy] {
            v.fill(0);
        }
        let code = self.prep.clone();
        let mut none = Element::default();
        let r = self.run(&code, 0, &mut none, None);
        if self.cur.instruct_control & 2 == 0 {
            self.defaults = self.cur;
        }
        r
    }

    /// do_hints: the graphics state from the defaults, then the glyph's program (none when `code` is empty) over its
    /// points, the twilight zone kept from glyph to glyph.
    pub fn reset(&mut self) {
        self.cur = self.defaults;
    }

    /// Run a glyph's program over `e` (do_hints after `reset`).
    pub fn glyph(&mut self, e: &mut Element, code: &[u8], autohint: Option<Autohint>) -> R {
        if code.is_empty() {
            return Ok(());
        }
        let code: Arc<[u8]> = Arc::from(code);
        self.run(&code, 0, e, autohint)
    }

    /// fnt_Execute over a whole program (`pgm` the SDK's program index: 1 the font program, else 0).
    fn run(&mut self, code: &Arc<[u8]>, pgm: u8, glyph: &mut Element, autohint: Option<Autohint>) -> R {
        // fnt_Execute does nothing below 2 ppem but in the font program
        if pgm != 1 && self.ppem < 2 {
            return Ok(());
        }
        let zones = [std::mem::take(&mut self.twilight), std::mem::take(glyph)];
        let mut m = Machine {
            it: self,
            zones,
            zp: [1; 3],
            proj: [0x4000, 0],
            free: [0x4000, 0],
            dual: [0x4000, 0],
            pf: 0x4000,
            mover: Mover::X,
            project: Proj::X,
            old_project: Proj::X,
            rp: [0; 3],
            loop_: 0,
            op: 0,
            stack: Vec::new(),
            code: code.clone(),
            ip: 0,
            budget: BUDGET,
            depth: 0,
            autohint,
        };
        let r = m.exec(0, code.len());
        let [t, g] = m.zones;
        self.twilight = t;
        *glyph = g;
        r
    }
}

/// MovePoint (sub_183900 / sub_184050 / sub_184090).
#[derive(Clone, Copy, PartialEq)]
enum Mover {
    X,
    Y,
    Free,
}

/// Project / OldProject (sub_183930 / sub_184080 / sub_184280 / sub_1842E0).
#[derive(Clone, Copy, PartialEq)]
enum Proj {
    X,
    Y,
    Vec,
    Dual,
}

/// fnt_LocalGraphicStateType: one program's run.
struct Machine<'a, 'b> {
    it: &'a mut Interp,
    zones: [Element; 2],
    zp: [usize; 3],
    proj: [i16; 2],
    free: [i16; 2],
    dual: [i16; 2],
    /// the projection vector against the freedom vector, 2.14
    pf: i16,
    mover: Mover,
    project: Proj,
    old_project: Proj,
    rp: [i32; 3],
    /// SLOOP's count less one
    loop_: i32,
    op: u8,
    stack: Vec<i32>,
    code: Arc<[u8]>,
    ip: usize,
    budget: u32,
    depth: u32,
    autohint: Option<Autohint<'b>>,
}

/// A stack value as a point, contour, zone, storage or table index.
fn idx(v: i32) -> R<usize> {
    usize::try_from(v).map_err(|_| Error)
}

/// The projection against the freedom vector, an almost perpendicular pair taken as parallel (SPVTL and the like).
fn dot(p: [i16; 2], f: [i16; 2]) -> i16 {
    let a = (i32::from(f[1]) * i32::from(p[1]) + 0x2000) as u32 >> 14;
    let b = (i32::from(f[0]) * i32::from(p[0]) + 0x2000) as u32 >> 14;
    let v = a.wrapping_add(b);
    clamp_pf(v as u16 as i16)
}

/// A projection-freedom product within ±1023 of zero taken as ±1.0.
fn clamp_pf(v: i16) -> i16 {
    if (v as u16).wrapping_add(1023) < 0x7FF {
        if v < 0 { -0x4000 } else { 0x4000 }
    } else {
        v
    }
}

/// A vector made a 2.14 unit vector (sub_1841A0); the zero vector gives the x axis.
fn unit(a: i32, b: i32) -> [i16; 2] {
    let m = a.wrapping_abs().max(b.wrapping_abs());
    let sh = (m.leading_zeros() as i32 - 2) as u32 & 31;
    let (a, b) = (a.wrapping_shl(sh), b.wrapping_shl(sh));
    let len = frac_sqrt(varmul(b, b, 30).wrapping_add(varmul(a, a, 30)));
    if len == 0 {
        return [0x4000, 0];
    }
    let half = |v: i32| ((v as u32).wrapping_add(0x8000) >> 16) as u16 as i16;
    [half(vardiv(a, len, 30)), half(vardiv(b, len, 30))]
}

/// The scale along the projection vector against the control values' (sub_183F50; both axes 1.0 here).
fn stretch(p: [i16; 2]) -> i32 {
    if p[1] == 0 || p[0] == 0 {
        return 0x10000;
    }
    let sq = |v: i16| {
        let v = i32::from(v);
        varmul(((4 * v).wrapping_mul(v).wrapping_add(0x8000) >> 16).wrapping_mul(4), 0x10000, 16)
    };
    let s = sq(p[1]).wrapping_add(sq(p[0]));
    if s < 0x10000 { (frac_sqrt(s << 14) + 0x2000) >> 14 } else { 0x10000 }
}

/// A distance's magnitude with the compensation, plus `add`, masked, then `or`ed, its sign put back.
fn grid(v: i32, c: i32, add: i32, mask: i32, or: i32) -> i32 {
    if v < 0 {
        (c.wrapping_sub(v).wrapping_add(add) & mask | or).wrapping_neg()
    } else {
        c.wrapping_add(v).wrapping_add(add) & mask | or
    }
}

/// Round a distance as the round state says (RoundFuncCall), with the engine compensation `c`.
fn round(st: &Params, v: i32, c: i32) -> i32 {
    let keep = |r: i32, z: i32| if (r ^ v) >= 0 || v == 0 { r } else { z };
    match st.round_state {
        0 => {
            let (ph, th) = (i32::from(st.phase), i32::from(st.threshold));
            let r = if v < 0 {
                ((st.period_mask & c.wrapping_sub(v).wrapping_add(th).wrapping_sub(ph)).wrapping_add(ph)).wrapping_neg()
            } else {
                (st.period_mask & c.wrapping_add(v).wrapping_add(th).wrapping_sub(ph)).wrapping_add(ph)
            };
            if v == 0 {
                return r;
            }
            keep(r, if v <= 0 { -ph } else { ph })
        }
        1 => {
            let (ph, th) = (i32::from(st.phase), i32::from(st.threshold));
            let r = if v < 0 {
                let q = vardiv(c.wrapping_sub(v).wrapping_add(th).wrapping_sub(ph), st.period45, 30);
                varmul(q & !63, st.period45, 30).wrapping_add(ph).wrapping_neg()
            } else {
                let q = vardiv(c.wrapping_add(v).wrapping_add(th).wrapping_sub(ph), st.period45, 30);
                varmul(q & !63, st.period45, 30).wrapping_add(ph)
            };
            if v == 0 {
                return r;
            }
            keep(r, if v <= 0 { -ph } else { ph })
        }
        2 => keep(grid(v, c, 32, !63, 0), 0),
        3 => keep(grid(v, c, 0, !63, 32), if v > 0 { 32 } else { -32 }),
        4 => keep(grid(v, c, 16, !31, 0), 0),
        5 => keep(v.wrapping_add(if v >= 0 { c } else { c.wrapping_neg() }), 0),
        6 => keep(grid(v, c, 0, !63, 0), 0),
        7 => keep(grid(v, c, 63, !63, 0), 0),
        _ => 0,
    }
}

impl Machine<'_, '_> {
    fn pop(&mut self) -> R<i32> {
        self.stack.pop().ok_or(Error)
    }

    fn push(&mut self, v: i32) -> R {
        if self.stack.len() >= STACK {
            return Err(Error);
        }
        self.stack.push(v);
        Ok(())
    }

    fn top(&mut self) -> R<&mut i32> {
        self.stack.last_mut().ok_or(Error)
    }

    /// Bytes of the running program.
    fn byte(&self, at: usize) -> R<u8> {
        self.code.get(at).copied().ok_or(Error)
    }

    fn zone(&self, z: usize) -> &Element {
        &self.zones[z]
    }

    /// A point's coordinates: current (0), original scaled (1) or font units (2).
    fn at(&self, z: usize, p: i32, kind: u8) -> R<(i32, i32)> {
        let e = &self.zones[z];
        let p = idx(p)?;
        let (x, y) = match kind {
            0 => (&e.x, &e.y),
            1 => (&e.ox, &e.oy),
            _ => (&e.oox, &e.ooy),
        };
        Ok((*x.get(p).ok_or(Error)?, *y.get(p).ok_or(Error)?))
    }

    /// A point number off the stack, checked against zone `z`.
    fn pop_point(&mut self, z: usize) -> R<usize> {
        let p = self.pop()?;
        self.check(z, p)
    }

    fn check(&self, z: usize, p: i32) -> R<usize> {
        let p = idx(p)?;
        if p < self.zones[z].x.len() { Ok(p) } else { Err(Error) }
    }

    fn proj_with(&self, which: Proj, dx: i32, dy: i32) -> i32 {
        match which {
            Proj::X => dx,
            Proj::Y => dy,
            Proj::Vec => varmul(dx, i32::from(self.proj[0]), 14).wrapping_add(varmul(dy, i32::from(self.proj[1]), 14)),
            Proj::Dual => varmul(dx, i32::from(self.dual[0]), 14).wrapping_add(varmul(dy, i32::from(self.dual[1]), 14)),
        }
    }

    fn project(&self, dx: i32, dy: i32) -> i32 {
        self.proj_with(self.project, dx, dy)
    }

    fn old_project(&self, dx: i32, dy: i32) -> i32 {
        self.proj_with(self.old_project, dx, dy)
    }

    /// MovePoint: a point moved `d` along the freedom vector, touched in the axes it moved in.
    fn move_point(&mut self, z: usize, p: i32, d: i32) -> R {
        let p = self.check(z, p)?;
        let (fx, fy, pf) = (i32::from(self.free[0]), i32::from(self.free[1]), i32::from(self.pf));
        let e = &mut self.zones[z];
        match self.mover {
            Mover::X => {
                e.x[p] = e.x[p].wrapping_add(d);
                e.f[p] |= 1;
            }
            Mover::Y => {
                e.y[p] = e.y[p].wrapping_add(d);
                e.f[p] |= 2;
            }
            Mover::Free => {
                let by = |f: i32| if pf == 0x4000 { varmul(d, f, 14) } else { muldiv(d, f, pf) };
                if fx != 0 {
                    e.x[p] = e.x[p].wrapping_add(by(fx));
                    e.f[p] |= 1;
                }
                if fy != 0 {
                    e.y[p] = e.y[p].wrapping_add(by(fy));
                    e.f[p] |= 2;
                }
            }
        }
        Ok(())
    }

    /// Shift a point by a displacement in both axes the freedom vector moves in.
    fn shift(&mut self, z: usize, p: usize, (dx, dy): (i32, i32)) {
        let (fx, fy) = (self.free[0] != 0, self.free[1] != 0);
        let e = &mut self.zones[z];
        if fx {
            e.x[p] = e.x[p].wrapping_add(dx);
            e.f[p] |= 1;
        }
        if fy {
            e.y[p] = e.y[p].wrapping_add(dy);
            e.f[p] |= 2;
        }
    }

    /// GetCVTEntry (sub_183960): a control value stretched along the projection vector.
    fn cvt(&self, n: i32) -> R<i32> {
        let v = *self.it.cvt.get(idx(n)?).ok_or(Error)?;
        Ok(varmul(v, stretch(self.proj), 16))
    }

    /// GetSingleWidth (sub_1839B0).
    fn single_width(&self) -> i32 {
        varmul(self.it.cur.scaled_sw, stretch(self.proj), 16)
    }

    /// The ppem along the projection vector (MPPEM, the DELTAs).
    fn ppem(&self) -> i32 {
        let ppem = if self.it.prepped { self.it.ppem } else { 0 };
        varmul(ppem, stretch(self.proj), 16)
    }

    /// The displacement of the reference point SHP / SHC / SHZ shift by (sub_184340): rp2 in zp1, or rp1 in zp0
    /// for the odd opcodes, from its original place along the freedom vector; its zone and number.
    fn ref_shift(&self) -> R<((i32, i32), usize, i32)> {
        let (z, p) = if self.op & 1 == 0 { (self.zp[1], self.rp[2]) } else { (self.zp[0], self.rp[1]) };
        let (x, y) = self.at(z, p, 0)?;
        let (ox, oy) = self.at(z, p, 1)?;
        let d = self.project(x.wrapping_sub(ox), y.wrapping_sub(oy));
        let (fx, fy, pf) = (i32::from(self.free[0]), i32::from(self.free[1]), i32::from(self.pf));
        let by = |f: i32| if pf == 0x4000 { varmul(d, f, 14) } else { muldiv(d, f, pf) };
        let dx = if fx != 0 { by(fx) } else { 0 };
        let dy = if fy != 0 { by(fy) } else { 0 };
        Ok(((dx, dy), z, p))
    }

    /// Run the running program from `from` until the instruction pointer reaches `to` (fnt_Execute's loop).
    fn exec(&mut self, from: usize, to: usize) -> R {
        self.ip = from;
        while self.ip < to {
            self.budget = self.budget.checked_sub(1).ok_or(Error)?;
            self.op = self.byte(self.ip)?;
            self.ip += 1;
            self.step()?;
        }
        Ok(())
    }

    /// Run a definition's body (CALL, LOOPCALL, an IDEF'd instruction), the caller's program and place kept.
    fn call(&mut self, d: &Def) -> R {
        let Some(code) = d.code.clone() else { return Ok(()) };
        self.depth += 1;
        if self.depth > DEPTH {
            return Err(Error);
        }
        let (saved, ip) = (std::mem::replace(&mut self.code, code), self.ip);
        let r = self.exec(d.start, d.start + d.len);
        self.code = saved;
        self.ip = ip;
        self.depth -= 1;
        r
    }

    /// Skip a definition's body to just past its ENDF (FDEF, IDEF); its length, ENDF excluded.
    fn skip_def(&mut self) -> R<usize> {
        let start = self.ip;
        loop {
            let op = self.byte(self.ip)?;
            self.ip += 1;
            match op {
                0x2D => return Ok(self.ip - start - 1),
                0x40 => self.ip += 1 + usize::from(self.byte(self.ip)?),
                0x41 => self.ip += 1 + 2 * usize::from(self.byte(self.ip)?),
                0xB0..=0xB7 => self.ip += usize::from(op - 0xAF),
                0xB8..=0xBF => self.ip += 2 * usize::from(op - 0xB7),
                _ => {}
            }
        }
    }

    /// Skip past the pushed bytes of an instruction being skipped over.
    fn skip_push(&mut self, op: u8) -> R {
        match op {
            0x40 => self.ip += 1 + usize::from(self.byte(self.ip)?),
            0x41 => self.ip += 1 + 2 * usize::from(self.byte(self.ip)?),
            0xB0..=0xB7 => self.ip += usize::from(op - 0xAF),
            0xB8..=0xBF => self.ip += 2 * usize::from(op - 0xB7),
            _ => {}
        }
        Ok(())
    }

    /// A relative jump from the jump instruction.
    fn jump(&mut self, by: i32) -> R {
        let to = (self.ip as i64 - 1) + i64::from(by);
        self.ip = usize::try_from(to).map_err(|_| Error)?;
        Ok(())
    }

    /// SPVTL / SFVTL / SDPVTL's vector: point p1 (zp1) from point p2 (zp2), rotated a quarter turn for the odd
    /// opcodes, with the stack's two points.
    fn line(&mut self) -> R<(i32, i32, i32, i32)> {
        let p2 = self.pop()?;
        let p1 = self.pop()?;
        let (x1, y1) = self.at(self.zp[1], p1, 0)?;
        let (x2, y2) = self.at(self.zp[2], p2, 0)?;
        Ok((x1.wrapping_sub(x2), y1.wrapping_sub(y2), p1, p2))
    }

    fn rotate(&self, v: [i16; 2]) -> [i16; 2] {
        if self.op & 1 != 0 { [(v[1] as i32).wrapping_neg() as i16, v[0]] } else { v }
    }

    fn step(&mut self) -> R {
        let op = self.op;
        match op {
            // SVTCA
            0x00 | 0x01 => {
                let (axis, m, p) = if op == 0 { ([0, 0x4000], Mover::Y, Proj::Y) } else { ([0x4000, 0], Mover::X, Proj::X) };
                self.proj = axis;
                self.free = axis;
                self.mover = m;
                self.project = p;
                self.old_project = p;
                self.pf = 0x4000;
            }
            // SPVTCA
            0x02 | 0x03 => {
                let (axis, p, f) = if op & 1 != 0 { ([0x4000, 0], Proj::X, self.free[0]) } else { ([0, 0x4000], Proj::Y, self.free[1]) };
                self.proj = axis;
                self.mover = Mover::Free;
                self.project = p;
                self.old_project = p;
                self.pf = clamp_pf(f);
            }
            // SFVTCA
            0x04 | 0x05 => {
                let (axis, p) = if op & 1 != 0 { ([0x4000, 0], self.proj[0]) } else { ([0, 0x4000], self.proj[1]) };
                self.free = axis;
                self.pf = clamp_pf(p);
                self.mover = Mover::Free;
            }
            // SPVTL
            0x06 | 0x07 => {
                let (dx, dy, _, _) = self.line()?;
                self.proj = self.rotate(unit(dx, dy));
                self.pf = dot(self.proj, self.free);
                self.old_project = Proj::Vec;
                self.mover = Mover::Free;
                self.project = Proj::Vec;
            }
            // SFVTL
            0x08 | 0x09 => {
                let (dx, dy, _, _) = self.line()?;
                self.free = self.rotate(unit(dx, dy));
                self.pf = dot(self.proj, self.free);
                self.mover = Mover::Free;
            }
            // SPVFS
            0x0A => {
                let y = self.pop()? as i16;
                let x = self.pop()? as i16;
                self.proj = [x, y];
                self.pf = dot(self.proj, self.free);
                self.old_project = Proj::Vec;
                self.mover = Mover::Free;
                self.project = Proj::Vec;
            }
            // SFVFS
            0x0B => {
                let y = self.pop()? as i16;
                let x = self.pop()? as i16;
                self.free = [x, y];
                self.pf = dot(self.proj, self.free);
                self.mover = Mover::Free;
            }
            // GPV, GFV
            0x0C | 0x0D => {
                let v = if op == 0x0C { self.proj } else { self.free };
                self.push(i32::from(v[0]))?;
                self.push(i32::from(v[1]))?;
            }
            // SFVTPV
            0x0E => {
                self.free = self.proj;
                self.pf = 0x4000;
                self.mover = Mover::Free;
            }
            0x0F => self.isect()?,
            // SRP0-2
            0x10..=0x12 => {
                let v = self.pop()?;
                self.rp[usize::from(op - 0x10)] = v;
            }
            // SZP0-2, SZPS
            0x13..=0x16 => {
                let z = idx(self.pop()?)?;
                if z > 1 {
                    return Err(Error);
                }
                match op {
                    0x13 => self.zp[0] = z,
                    0x14 => self.zp[1] = z,
                    0x15 => self.zp[2] = z,
                    _ => self.zp = [z; 3],
                }
            }
            // SLOOP
            0x17 => self.loop_ = self.pop()?.wrapping_sub(1),
            // RTG, RTHG, RTDG, ROFF, RUTG, RDTG
            0x18 => self.it.cur.round_state = 2,
            0x19 => self.it.cur.round_state = 3,
            0x3D => self.it.cur.round_state = 4,
            0x7A => self.it.cur.round_state = 5,
            0x7C => self.it.cur.round_state = 7,
            0x7D => self.it.cur.round_state = 6,
            // SMD
            0x1A => self.it.cur.min_dist = self.pop()?,
            // ELSE
            0x1B => {
                let mut level = 1;
                while level != 0 {
                    let o = self.byte(self.ip)?;
                    self.ip += 1;
                    match o {
                        0x58 => level += 1,
                        0x59 => level -= 1,
                        _ => self.skip_push(o)?,
                    }
                }
            }
            // JMPR
            0x1C => {
                let by = self.pop()?;
                self.jump(by)?;
            }
            // SCVTCI, SSWCI
            0x1D => self.it.cur.wtci = self.pop()?,
            0x1E => self.it.cur.swci = self.pop()?,
            // SSW
            0x1F => {
                let v = self.pop()? as i16;
                self.it.cur.sw = v;
                self.it.cur.scaled_sw = self.it.scale.apply(i32::from(v));
            }
            // DUP
            0x20 => {
                let v = *self.top()?;
                self.push(v)?;
            }
            // POP
            0x21 => {
                self.pop()?;
            }
            // CLEAR
            0x22 => self.stack.clear(),
            // SWAP
            0x23 => {
                let n = self.stack.len();
                if n < 2 {
                    return Err(Error);
                }
                self.stack.swap(n - 1, n - 2);
            }
            // DEPTH
            0x24 => {
                let n = self.stack.len() as i32;
                self.push(n)?;
            }
            // CINDEX
            0x25 => {
                let k = *self.top()?;
                let n = self.stack.len();
                let at = (n as i64 - 1 - i64::from(k)).try_into().map_err(|_| Error)?;
                let v = *self.stack.get::<usize>(at).ok_or(Error)?;
                *self.top()? = v;
            }
            // MINDEX
            0x26 => {
                let k = self.pop()?;
                let n = self.stack.len();
                let at: usize = (n as i64 - i64::from(k)).try_into().map_err(|_| Error)?;
                if k != 0 {
                    if at >= n {
                        return Err(Error);
                    }
                    let v = self.stack.remove(at);
                    self.stack.push(v);
                } else {
                    // k = 0 leaves itself on the stack
                    self.push(0)?;
                }
            }
            // ALIGNPTS: the two points' distance projected from zp0 / zp1, the points moved crosswise (the SDK's own)
            0x27 => {
                let p2 = self.pop()?;
                let p1 = self.pop()?;
                let (ax, ay) = self.at(self.zp[0], p2, 0)?;
                let (bx, by) = self.at(self.zp[1], p1, 0)?;
                let d = self.project(ax.wrapping_sub(bx), ay.wrapping_sub(by));
                let half = d >> 1;
                self.move_point(self.zp[0], p1, half)?;
                self.move_point(self.zp[1], p2, half.wrapping_sub(d))?;
            }
            // the glyph's scaled advance (an SDK instruction)
            0x28 => {
                let e = &self.zones[1];
                let last = usize::from(*e.ep.last().ok_or(Error)?);
                let (a, b) = (*e.ox.get(last + 1).ok_or(Error)?, *e.ox.get(last + 2).ok_or(Error)?);
                self.push(b.wrapping_sub(a))?;
            }
            // UTP
            0x29 => {
                let z = self.zp[0];
                let p = self.pop_point(z)?;
                let (fx, fy) = (self.free[0] != 0, self.free[1] != 0);
                let f = &mut self.zones[z].f[p];
                if fx {
                    *f &= !1;
                }
                if fy {
                    *f &= !2;
                }
            }
            // LOOPCALL
            0x2A => {
                let n = idx(self.pop()?)?;
                let count = self.pop()?;
                let d = self.it.funcs.get(n).cloned().ok_or(Error)?;
                if count >= 1 && d.len != 0 {
                    for _ in 0..count {
                        self.call(&d)?;
                    }
                }
            }
            // CALL
            0x2B => {
                let n = idx(self.pop()?)?;
                let d = self.it.funcs.get(n).cloned().ok_or(Error)?;
                self.call(&d)?;
            }
            // FDEF
            0x2C => {
                let n = idx(self.pop()?)?;
                if n >= self.it.funcs.len() {
                    return Err(Error);
                }
                let start = self.ip;
                let len = usize::from(self.skip_def()? as u16);
                self.it.funcs[n] = Def { code: Some(self.code.clone()), start, len, opcode: 0 };
            }
            // ENDF outside a definition, and an undefined instruction
            0x2D | 0x7B => return Err(Error),
            // MDAP
            0x2E | 0x2F => {
                let z = self.zp[0];
                let p = self.pop()?;
                self.rp[0] = p;
                self.rp[1] = p;
                let d = if op & 1 != 0 {
                    let (x, y) = self.at(z, p, 0)?;
                    let v = self.project(x, y);
                    round(&self.it.cur, v, 0).wrapping_sub(v)
                } else {
                    0
                };
                self.move_point(z, p, d)?;
            }
            // IUP
            0x30 | 0x31 => {
                let z = self.zp[2];
                tt::iup(&mut self.zones[z], op & 1 != 0);
            }
            // SHP
            0x32 | 0x33 => {
                let (d, _, _) = self.ref_shift()?;
                let z = self.zp[2];
                for _ in 0..self.loop_.max(-1) + 1 {
                    let p = self.pop_point(z)?;
                    self.shift(z, p, d);
                }
                self.loop_ = 0;
            }
            // SHC
            0x34 | 0x35 => {
                let (d, rz, rp) = self.ref_shift()?;
                let z = self.zp[2];
                let c = idx(self.pop()?)?;
                let e = self.zone(z);
                let (s, t) = (i32::from(*e.sp.get(c).ok_or(Error)?), i32::from(*e.ep.get(c).ok_or(Error)?));
                let same = rz == z;
                for p in s..=t {
                    if same && p == rp {
                        continue;
                    }
                    let p = self.check(z, p)?;
                    self.shift(z, p, d);
                }
            }
            // SHZ
            0x36 | 0x37 => {
                let (d, rz, rp) = self.ref_shift()?;
                let z = idx(self.pop()?)?;
                if z > 1 {
                    return Err(Error);
                }
                let e = self.zone(z);
                let (Some(&first), Some(&last)) = (e.sp.first(), e.ep.last()) else { return Err(Error) };
                let (first, last) = (i32::from(first), i32::from(last));
                let skip = if rz == z { rp } else { -1 };
                // the SDK's two runs, before the reference point and after it
                let mut p = first;
                for _ in 0..(skip - first).max(0) {
                    let q = self.check(z, p)?;
                    self.shift(z, q, d);
                    p += 1;
                }
                let rest = if skip == -1 { last - first } else { last - skip - 1 };
                if skip != -1 {
                    p += 1;
                }
                for _ in 0..(rest + 1).max(0) {
                    let q = self.check(z, p)?;
                    self.shift(z, q, d);
                    p += 1;
                }
            }
            0x38 => self.shpix()?,
            0x39 => self.ip_()?,
            // MSIRP
            0x3A | 0x3B => {
                let d = self.pop()?;
                let p = self.pop()?;
                let (z0, z1) = (self.zp[0], self.zp[1]);
                let r0 = self.rp[0];
                if z1 == 0 {
                    let (ox, oy) = self.at(z0, r0, 1)?;
                    let q = self.check(z1, p)?;
                    let e = &mut self.zones[z1];
                    e.ox[q] = varmul(d, i32::from(self.proj[0]), 14).wrapping_add(ox);
                    e.oy[q] = varmul(d, i32::from(self.proj[1]), 14).wrapping_add(oy);
                    e.x[q] = e.ox[q];
                    e.y[q] = e.oy[q];
                }
                let (x, y) = self.at(z1, p, 0)?;
                let (rx, ry) = self.at(z0, r0, 0)?;
                let cur = self.project(x.wrapping_sub(rx), y.wrapping_sub(ry));
                self.move_point(z1, p, d.wrapping_sub(cur))?;
                self.rp[1] = r0;
                self.rp[2] = p;
                if op & 1 != 0 {
                    self.rp[0] = p;
                }
            }
            // ALIGNRP
            0x3C => {
                let (z0, z1) = (self.zp[0], self.zp[1]);
                if self.loop_ >= 0 {
                    let (rx, ry) = self.at(z0, self.rp[0], 0)?;
                    for _ in 0..=self.loop_ {
                        let p = self.pop()?;
                        let (x, y) = self.at(z1, p, 0)?;
                        let d = self.project(x.wrapping_sub(rx), y.wrapping_sub(ry));
                        self.move_point(z1, p, d.wrapping_neg())?;
                    }
                }
                self.loop_ = 0;
            }
            // MIAP
            0x3E | 0x3F => {
                let n = self.pop()?;
                let mut v = self.cvt(n)?;
                let p = self.pop()?;
                let z = self.zp[0];
                self.rp[0] = p;
                self.rp[1] = p;
                if z == 0 {
                    let q = self.check(z, p)?;
                    let (px, py) = (i32::from(self.proj[0]), i32::from(self.proj[1]));
                    let e = &mut self.zones[z];
                    e.ox[q] = varmul(v, px, 14);
                    e.x[q] = e.ox[q];
                    e.oy[q] = varmul(v, py, 14);
                    e.y[q] = e.oy[q];
                }
                let (x, y) = self.at(z, p, 0)?;
                let d = self.project(x, y);
                if op & 1 != 0 {
                    let off = v.wrapping_sub(d).wrapping_abs();
                    let base = if off <= self.it.cur.wtci { v } else { d };
                    v = round(&self.it.cur, base, 0);
                }
                self.move_point(z, p, v.wrapping_sub(d))?;
            }
            // NPUSHB, NPUSHW
            0x40 => {
                let n = usize::from(self.byte(self.ip)?);
                for i in 0..n {
                    let b = self.byte(self.ip + 1 + i)?;
                    self.push(i32::from(b))?;
                }
                self.ip += 1 + n;
            }
            0x41 => {
                let n = usize::from(self.byte(self.ip)?);
                for i in 0..n {
                    let w = i16::from_be_bytes([self.byte(self.ip + 1 + 2 * i)?, self.byte(self.ip + 2 + 2 * i)?]);
                    self.push(i32::from(w))?;
                }
                self.ip += 1 + 2 * n;
            }
            // WS, RS
            0x42 => {
                let v = self.pop()?;
                let n = idx(self.pop()?)?;
                *self.it.storage.get_mut(n).ok_or(Error)? = v;
            }
            0x43 => {
                let n = idx(*self.top()?)?;
                let v = *self.it.storage.get(n).ok_or(Error)?;
                *self.top()? = v;
            }
            // WCVTP: a value stored so that it reads back stretched as given
            0x44 => {
                let v = self.pop()?;
                let n = self.pop()?;
                let i = idx(n)?;
                *self.it.cvt.get_mut(i).ok_or(Error)? = v;
                if v != 0 {
                    let r = self.cvt(n)?;
                    if r != 0 && r != v {
                        self.it.cvt[i] = varmul(v, vardiv(v, r, 16), 16);
                    }
                }
            }
            // RCVT
            0x45 => {
                let n = self.pop()?;
                let v = self.cvt(n)?;
                self.push(v)?;
            }
            // GC
            0x46 | 0x47 => {
                let p = self.pop()?;
                let z = self.zp[2];
                let v = if op & 1 != 0 {
                    let (x, y) = self.at(z, p, 1)?;
                    self.old_project(x, y)
                } else {
                    let (x, y) = self.at(z, p, 0)?;
                    self.project(x, y)
                };
                self.push(v)?;
            }
            // SCFS
            0x48 => {
                let v = self.pop()?;
                let p = self.pop()?;
                let z = self.zp[2];
                let (x, y) = self.at(z, p, 0)?;
                let d = self.project(x, y);
                self.move_point(z, p, v.wrapping_sub(d))?;
                if z == 0 {
                    let q = self.check(z, p)?;
                    let e = &mut self.zones[z];
                    e.ox[q] = e.x[q];
                    e.oy[q] = e.y[q];
                }
            }
            // MD: 0x49 the current outline, 0x4A the original in font units, scaled
            0x49 | 0x4A => {
                let p2 = self.pop()?;
                let p1 = self.pop()?;
                let (z0, z1) = (self.zp[0], self.zp[1]);
                let v = if op & 1 != 0 {
                    let (ax, ay) = self.at(z0, p1, 0)?;
                    let (bx, by) = self.at(z1, p2, 0)?;
                    self.project(ax.wrapping_sub(bx), ay.wrapping_sub(by))
                } else {
                    let (ax, ay) = self.at(z0, p1, 2)?;
                    let (bx, by) = self.at(z1, p2, 2)?;
                    self.it.scale.apply(self.old_project(ax.wrapping_sub(bx), ay.wrapping_sub(by)))
                };
                self.push(v)?;
            }
            // MPPEM, MPS (the point size the SDK scales at, 1)
            0x4B => {
                let v = self.ppem() as u16;
                self.push(i32::from(v))?;
            }
            0x4C => self.push(i32::from(self.it.prepped))?,
            // FLIPON, FLIPOFF
            0x4D => self.it.cur.auto_flip = true,
            0x4E => self.it.cur.auto_flip = false,
            // DEBUG
            0x4F => {
                self.pop()?;
            }
            // LT .. NEQ, AND, OR, ADD, SUB, DIV, MUL, MAX, MIN
            0x50..=0x55 | 0x5A | 0x5B | 0x60..=0x63 | 0x8B | 0x8C => {
                let b = self.pop()?;
                let a = self.pop()?;
                let v = match op {
                    0x50 => i32::from(a < b),
                    0x51 => i32::from(a <= b),
                    0x52 => i32::from(a > b),
                    0x53 => i32::from(a >= b),
                    0x54 => i32::from(a == b),
                    0x55 => i32::from(a != b),
                    0x5A => i32::from(a != 0 && b != 0),
                    0x5B => i32::from(a | b != 0),
                    0x60 => a.wrapping_add(b),
                    0x61 => a.wrapping_sub(b),
                    0x62 => vardiv(a, b, 6),
                    0x63 => varmul(a, b, 6),
                    0x8B => a.max(b),
                    _ => a.min(b),
                };
                self.push(v)?;
            }
            // ODD, EVEN (on the whole-pixel grid), NOT, ABS, NEG, FLOOR, CEILING
            0x56 | 0x57 | 0x5C | 0x64..=0x67 => {
                let v = *self.top()?;
                *self.top()? = match op {
                    0x56 | 0x57 => {
                        let r = if v < 0 { -((32 - v) & !63) } else { (v + 32) & !63 };
                        let mut px = if (r ^ v) >= 0 || v == 0 { (r >> 6) as u8 } else { 0 };
                        if op == 0x56 {
                            px = px.wrapping_add(1);
                        }
                        i32::from(px & 1 == 0)
                    }
                    0x5C => i32::from(v == 0),
                    0x64 => v.wrapping_abs(),
                    0x65 => v.wrapping_neg(),
                    0x66 => v & !63,
                    _ => v.wrapping_add(63) & !63,
                };
            }
            // IF
            0x58 => {
                if self.pop()? == 0 {
                    let mut level = 1;
                    loop {
                        let o = self.byte(self.ip)?;
                        self.ip += 1;
                        match o {
                            0x1B if level == 1 => break,
                            0x1B => {}
                            0x58 => level += 1,
                            0x59 => {
                                level -= 1;
                                if level == 0 {
                                    break;
                                }
                            }
                            _ => self.skip_push(o)?,
                        }
                    }
                }
            }
            // EIF
            0x59 => {}
            // DELTAP1-3, DELTAC1-3
            0x5D => self.delta(0, false)?,
            0x71 => self.delta(16, false)?,
            0x72 => self.delta(32, false)?,
            0x73 => self.delta(0, true)?,
            0x74 => self.delta(16, true)?,
            0x75 => self.delta(32, true)?,
            // SDB, SDS
            0x5E => self.it.cur.delta_base = self.pop()? as u16,
            0x5F => self.it.cur.delta_shift = self.pop()? as u16,
            // ROUND, NROUND with the engine compensation, which is 0
            0x68..=0x6B => {
                let v = *self.top()?;
                *self.top()? = round(&self.it.cur, v, 0);
            }
            0x6C..=0x6F => {}
            // WCVTF
            0x70 => {
                let v = self.pop()?;
                let n = idx(self.pop()?)?;
                *self.it.cvt.get_mut(n).ok_or(Error)? = self.it.scale.apply(v);
            }
            // SROUND, S45ROUND
            0x76 | 0x77 => {
                let n = self.pop()?;
                self.super_round(n, op == 0x77);
            }
            // JROT, JROF
            0x78 | 0x79 => {
                let c = self.pop()?;
                let by = self.pop()?;
                if (c != 0) == (op == 0x78) {
                    self.jump(by)?;
                }
            }
            // SANGW
            0x7E => {
                self.pop()?;
            }
            0x7F => self.aa()?,
            // FLIPPT
            0x80 => {
                let z = self.zp[0];
                for _ in 0..self.loop_.max(-1) + 1 {
                    let p = self.pop_point(z)?;
                    self.zones[z].on[p] ^= 1;
                }
                self.loop_ = 0;
            }
            // FLIPRGON, FLIPRGOFF
            0x81 | 0x82 => {
                let hi = self.pop()?;
                let lo = self.pop()?;
                let z = self.zp[0];
                for p in lo..=hi {
                    let p = self.check(z, p)?;
                    if op == 0x81 {
                        self.zones[z].on[p] |= 1;
                    } else {
                        self.zones[z].on[p] &= !1;
                    }
                }
            }
            // SCANCTRL, SCANTYPE (dropout control: a graymap has none)
            0x85 => {
                let v = self.pop()?;
                self.it.cur.scan_control = v | (self.it.cur.scan_control as u32 >> 16 << 16) as i32;
            }
            0x8D => {
                let v = self.pop()?;
                if (0..=2).contains(&v) {
                    self.it.cur.scan_control = (self.it.cur.scan_control & 0xFFFF) | v << 16;
                }
            }
            // SDPVTL
            0x86 | 0x87 => {
                let (dx, dy, p1, p2) = self.line()?;
                self.proj = unit(dx, dy);
                let (ox1, oy1) = self.at(self.zp[1], p1, 1)?;
                let (ox2, oy2) = self.at(self.zp[2], p2, 1)?;
                self.dual = unit(ox1.wrapping_sub(ox2), oy1.wrapping_sub(oy2));
                self.proj = self.rotate(self.proj);
                self.dual = self.rotate(self.dual);
                self.mover = Mover::Free;
                self.project = Proj::Vec;
                self.pf = dot(self.proj, self.free);
                self.old_project = Proj::Dual;
            }
            // GETINFO: version 1, not rotated or stretched
            0x88 => {
                let s = *self.top()?;
                *self.top()? = s & 1;
            }
            // IDEF
            0x89 => {
                let opcode = self.pop()? as u8;
                let start = self.ip;
                let len = usize::from(self.skip_def()? as u16);
                let d = Def { code: Some(self.code.clone()), start, len, opcode };
                let full = self.it.idefs.len() >= self.it.max_idefs;
                match self.it.idefs.iter_mut().find(|d| d.opcode == opcode) {
                    Some(old) => *old = d,
                    None if !full => self.it.idefs.push(d),
                    None => return Err(Error),
                }
            }
            // ROLL
            0x8A => {
                let n = self.stack.len();
                if n < 3 {
                    return Err(Error);
                }
                let a = self.stack[n - 3];
                self.stack[n - 3] = self.stack[n - 2];
                self.stack[n - 2] = self.stack[n - 1];
                self.stack[n - 1] = a;
            }
            // INSTCTRL, once the control value program has run
            0x8E => {
                let sel = self.pop()?;
                let v = self.pop()?;
                if self.it.prepped {
                    let c = &mut self.it.cur.instruct_control;
                    match sel {
                        2 => *c = (*c & !2) | if v & 2 != 0 { 2 } else { 0 },
                        1 => *c = (*c & !1) | (v & 1),
                        _ => {}
                    }
                }
            }
            // SDK instructions that only take their arguments
            0xA2..=0xA4 => {
                let _ = self.pop()?;
                let n = idx(self.pop()?)?;
                let keep = self.stack.len().checked_sub(n).ok_or(Error)?;
                self.stack.truncate(keep);
            }
            0xA5 | 0xA6 => {
                for _ in 0..3 {
                    self.pop()?;
                }
            }
            // SDK deltas: whole contours, runs of points along the projection, points along it
            0xA7..=0xA9 => self.delta_contours(u16::from(op - 0xA7) * 16)?,
            0xAA..=0xAC => self.delta_run(u16::from(op - 0xAA) * 16)?,
            0xAD..=0xAF => self.delta_level(u16::from(op - 0xAD) * 16)?,
            // PUSHB, PUSHW
            0xB0..=0xB7 => {
                let n = usize::from(op - 0xAF);
                for i in 0..n {
                    let b = self.byte(self.ip + i)?;
                    self.push(i32::from(b))?;
                }
                self.ip += n;
            }
            0xB8..=0xBF => {
                let n = usize::from(op - 0xB7);
                for i in 0..n {
                    let w = i16::from_be_bytes([self.byte(self.ip + 2 * i)?, self.byte(self.ip + 2 * i + 1)?]);
                    self.push(i32::from(w))?;
                }
                self.ip += 2 * n;
            }
            0xC0..=0xDF => self.mdrp()?,
            0xE0..=0xFF => self.mirp()?,
            // anything else: an IDEF, else an error
            _ => {
                let d = self.it.idefs.iter().find(|d| d.opcode == op).cloned().ok_or(Error)?;
                self.call(&d)?;
            }
        }
        Ok(())
    }

    /// ISECT: the point in zp2 put where line a0-a1 (zp1) crosses line b0-b1 (zp0), touched in both axes.
    fn isect(&mut self) -> R {
        let b1 = self.pop()?;
        let b0 = self.pop()?;
        let a1 = self.pop()?;
        let a0 = self.pop()?;
        let p = self.pop()?;
        let (bx0, by0) = self.at(self.zp[0], b0, 0)?;
        let (bx1, by1) = self.at(self.zp[0], b1, 0)?;
        let (ax0, ay0) = self.at(self.zp[1], a0, 0)?;
        let (ax1, ay1) = self.at(self.zp[1], a1, 0)?;
        let z = self.zp[2];
        let q = self.check(z, p)?;
        self.zones[z].f[q] |= 3;
        let (dbx, dby) = (bx1.wrapping_sub(bx0), by1.wrapping_sub(by0));
        let (dax, day) = (ax1.wrapping_sub(ax0), ay1.wrapping_sub(ay0));
        let mid = |e: &mut Element| {
            e.x[q] = (bx0.wrapping_add(dbx >> 1).wrapping_add(ax0).wrapping_add(dax >> 1)) >> 1;
            e.y[q] = (by0.wrapping_add(dby >> 1).wrapping_add(ay0).wrapping_add(day >> 1)) >> 1;
        };
        // (n, d): the second line's parameter of the crossing as n / d
        let (n, d) = if dby != 0 {
            if dbx != 0 {
                if dbx.wrapping_abs() <= dby.wrapping_abs() {
                    let k = vardiv(dbx, dby, 30);
                    let n = bx0.wrapping_sub(ax0).wrapping_add(varmul(ay0.wrapping_sub(by0), k, 30));
                    let r = varmul(day, k, 30);
                    (n, dax.wrapping_sub(r))
                } else {
                    let k = vardiv(dby, dbx, 30);
                    let n = ay0.wrapping_sub(by0).wrapping_sub(varmul(ax0.wrapping_sub(bx0), k, 30));
                    let r = varmul(dax, k, 30);
                    (n, r.wrapping_sub(day))
                }
            } else {
                if day == 0 {
                    self.zones[z].x[q] = bx0;
                    self.zones[z].y[q] = ay0;
                    return Ok(());
                }
                (ax0.wrapping_sub(bx0), dax.wrapping_neg())
            }
        } else {
            if dax == 0 {
                self.zones[z].x[q] = ax0;
                self.zones[z].y[q] = by0;
                return Ok(());
            }
            (ay0.wrapping_sub(by0), day.wrapping_neg())
        };
        // parallel lines: halfway between their midpoints
        let parallel = match (dby != 0, dbx != 0) {
            (true, true) => d == 0,
            (true, false) => dax == 0,
            (false, _) => day == 0,
        };
        let e = &mut self.zones[z];
        if parallel {
            mid(e);
            return Ok(());
        }
        let (x, y) = if n.wrapping_abs() >= d.wrapping_abs() {
            if n != 0 {
                let k = vardiv(d, n, 30);
                (vardiv(dax, k, 30).wrapping_add(ax0), ay0.wrapping_add(vardiv(day, k, 30)))
            } else {
                (ax0, ay0)
            }
        } else {
            let k = vardiv(n, d, 30);
            (varmul(dax, k, 30).wrapping_add(ax0), ay0.wrapping_add(varmul(day, k, 30)))
        };
        e.x[q] = x;
        e.y[q] = y;
        Ok(())
    }

    /// IP: points in zp2 kept where they lay between rp1 (zp0) and rp2 (zp1), originals in font units unless
    /// the twilight zone is one of the three.
    fn ip_(&mut self) -> R {
        let (z0, z1, z2) = (self.zp[0], self.zp[1], self.zp[2]);
        let twilight = z0 == 0 || z1 == 0 || z2 == 0;
        let (r1, r2) = (self.rp[1], self.rp[2]);
        let (ax, ay) = self.at(z1, r2, 0)?;
        let (bx, by) = self.at(z0, r1, 0)?;
        let cur = self.project(ax.wrapping_sub(bx), ay.wrapping_sub(by));
        let kind = if twilight { 1 } else { 2 };
        let (oax, oay) = self.at(z1, r2, kind)?;
        let (obx, oby) = self.at(z0, r1, kind)?;
        let orig = self.old_project(oax.wrapping_sub(obx), oay.wrapping_sub(oby));
        if self.loop_ >= 0 {
            while let Some(p) = self.stack.pop() {
                let (ox, oy) = self.at(z2, p, kind)?;
                let mut d = self.old_project(ox.wrapping_sub(obx), oy.wrapping_sub(oby));
                if orig != 0 {
                    d = muldiv(cur, d, orig);
                }
                let (x, y) = self.at(z2, p, 0)?;
                let now = self.project(x.wrapping_sub(bx), y.wrapping_sub(by));
                self.move_point(z2, p, d.wrapping_sub(now))?;
                let left = self.loop_;
                self.loop_ -= 1;
                if left <= 0 {
                    break;
                }
            }
        }
        self.loop_ = 0;
        Ok(())
    }

    /// The single width cut-in: a distance within it of the single width becomes the single width.
    fn cut_in_sw(&self, d: i32) -> i32 {
        let sw = self.single_width();
        let ci = self.it.cur.swci;
        if d < 0 {
            let mut off = d.wrapping_neg().wrapping_sub(sw);
            if off < 0 {
                off = sw.wrapping_add(d);
            }
            (if off >= ci { d.wrapping_neg() } else { sw }).wrapping_neg()
        } else {
            let off = d.wrapping_sub(sw).wrapping_abs();
            if off < ci { sw } else { d }
        }
    }

    /// The engine compensation added away from zero, a sign change giving 0 (`z` the value deciding that).
    fn compensate(d: i32, c: i32, z: i32) -> i32 {
        let r = d.wrapping_add(if d < 0 { c.wrapping_neg() } else { c });
        if (r ^ d) >= 0 || z == 0 { r } else { 0 }
    }

    /// The minimum distance kept on the original distance's side.
    fn min_dist(&self, v: i32, orig: i32) -> i32 {
        let m = self.it.cur.min_dist;
        if orig < 0 {
            if v > m.wrapping_neg() { m.wrapping_neg() } else { v }
        } else if v < m {
            m
        } else {
            v
        }
    }

    /// MDRP: a point in zp1 put at its original distance from rp0 (zp0), the distance from font units (scaled
    /// along each axis) unless a twilight zone is involved.
    fn mdrp(&mut self) -> R {
        let op = self.op;
        let p = self.pop()?;
        let (z0, z1) = (self.zp[0], self.zp[1]);
        let r0 = self.rp[0];
        let mut d = if z0 == 0 || z1 == 0 {
            let (ax, ay) = self.at(z1, p, 1)?;
            let (bx, by) = self.at(z0, r0, 1)?;
            self.old_project(ax.wrapping_sub(bx), ay.wrapping_sub(by))
        } else {
            let (ax, ay) = self.at(z1, p, 2)?;
            let (bx, by) = self.at(z0, r0, 2)?;
            let (dx, dy) = (ax.wrapping_sub(bx), ay.wrapping_sub(by));
            let s = self.it.scale;
            if self.proj[0] == 0 || self.proj[1] == 0 {
                s.apply(self.old_project(dx, dy))
            } else {
                let (vx, vy) = if self.old_project == self.project { (self.proj[0], self.proj[1]) } else { (self.dual[0], self.dual[1]) };
                let a = varmul(dx, i32::from(vx), 14);
                let b = varmul(dy, i32::from(vy), 14);
                s.apply(a).wrapping_add(s.apply(b))
            }
        };
        if self.it.cur.swci != 0 {
            d = self.cut_in_sw(d);
        }
        let mut v = if op & 4 != 0 { round(&self.it.cur, d, 0) } else { Self::compensate(d, 0, d) };
        if op & 8 != 0 {
            v = self.min_dist(v, d);
        }
        let (x, y) = self.at(z1, p, 0)?;
        let (rx, ry) = self.at(z0, r0, 0)?;
        let cur = self.project(x.wrapping_sub(rx), y.wrapping_sub(ry));
        self.move_point(z1, p, v.wrapping_sub(cur))?;
        self.rp[1] = r0;
        self.rp[2] = p;
        if op & 0x10 != 0 {
            self.rp[0] = p;
        }
        Ok(())
    }

    /// MIRP: a point in zp1 put at a control value's distance from rp0 (zp0), flipped to the original side
    /// with FLIPON, the original distance kept when the control value is beyond the cut-in.
    fn mirp(&mut self) -> R {
        let op = self.op;
        let n = self.pop()?;
        let cv = self.cvt(n)?;
        let p = self.pop()?;
        let mut v = cv;
        if self.it.cur.swci != 0 {
            v = self.cut_in_sw(v);
        }
        let (z0, z1) = (self.zp[0], self.zp[1]);
        let r0 = self.rp[0];
        if z1 == 0 {
            let (ox, oy) = self.at(z0, r0, 1)?;
            let q = self.check(z1, p)?;
            let (px, py) = (i32::from(self.proj[0]), i32::from(self.proj[1]));
            let e = &mut self.zones[z1];
            e.ox[q] = ox.wrapping_add(varmul(v, px, 14));
            e.oy[q] = oy.wrapping_add(varmul(v, py, 14));
            e.x[q] = e.ox[q];
            e.y[q] = e.oy[q];
        }
        let (ax, ay) = self.at(z1, p, 1)?;
        let (bx, by) = self.at(z0, r0, 1)?;
        let orig = self.old_project(ax.wrapping_sub(bx), ay.wrapping_sub(by));
        let flip = ((orig ^ v) & i32::MIN) != 0 && self.it.cur.auto_flip;
        let d = if flip { v.wrapping_neg() } else { v };
        let mut r = if op & 4 != 0 {
            let off = d.wrapping_sub(orig).wrapping_abs();
            let base = if off <= self.it.cur.wtci { d } else { orig };
            round(&self.it.cur, base, 0)
        } else {
            Self::compensate(d, 0, v)
        };
        if op & 8 != 0 {
            r = self.min_dist(r, orig);
        }
        let (x, y) = self.at(z1, p, 0)?;
        let (rx, ry) = self.at(z0, r0, 0)?;
        let cur = self.project(x.wrapping_sub(rx), y.wrapping_sub(ry));
        self.move_point(z1, p, r.wrapping_sub(cur))?;
        self.rp[1] = r0;
        self.rp[2] = p;
        if op & 0x10 != 0 {
            self.rp[0] = p;
        }
        Ok(())
    }

    /// SROUND / S45ROUND's period, phase and threshold.
    fn super_round(&mut self, n: i32, diag: bool) {
        let c = &mut self.it.cur;
        let period: i32 = if diag {
            let (p, f) = match n & 0xC0 {
                0 => (23, 379_625_062),
                0x40 => (45, 759_250_125),
                0x80 => (91, 1_518_500_250),
                _ => (0, 999),
            };
            c.period45 = f;
            p
        } else {
            let p = match n & 0xC0 {
                0 => 32,
                0x40 => 64,
                0x80 => 128,
                _ => 999,
            };
            c.period_mask = (!((p - 1) as u16)) as i32 | !0xFFFF;
            p
        };
        c.phase = match (n >> 4) & 3 {
            0 => 0,
            1 => ((period + 2) as u32 >> 2) as i16,
            2 => ((period + 1) as u32 >> 1) as i16,
            _ => ((3 * period + 2) as u32 >> 2) as i16,
        };
        let t = n & 0xF;
        c.threshold = if t == 0 { (period - 1) as i16 } else { (((t - 4) * period + 4) as u32 >> 3) as i16 };
        c.round_state = i32::from(diag);
    }

    /// The ppem a DELTA's arguments count from, and whether the ppem is within their 16 sizes; the shift.
    fn delta_ppem(&self, add: u16) -> (i32, u32) {
        let base = i32::from(self.it.cur.delta_base.wrapping_add(add) as i16);
        (self.ppem().wrapping_sub(base).wrapping_mul(16), u32::from(self.it.cur.delta_shift) & 31)
    }

    /// A DELTA argument's step, in 26.6 after the shift.
    fn delta_step(arg: i32, shift: u32) -> i32 {
        let s = (arg & 0xF) as u32;
        (((s as i32) - if s > 7 { 7 } else { 8 }) << 6) >> shift
    }

    /// DELTAP / DELTAC (sub_1886A0): the pairs in order of their ppem, found by halving, then each at this ppem
    /// moving its point (zp0) or control value.
    fn delta(&mut self, add: u16, cvt: bool) -> R {
        let n = self.pop()?;
        let pairs = 2i64 * i64::from(n);
        let len = self.stack.len() as i64;
        if pairs < 0 || pairs > len {
            return Err(Error);
        }
        let base = (len - pairs) as usize;
        let args: Vec<i32> = self.stack.split_off(base);
        let (ppem, shift) = self.delta_ppem(add);
        if !((ppem / 16) as u32 <= 0xF && ppem >= 0) {
            return Ok(());
        }
        let count = args.len() as i32;
        let mut half = count >> 1;
        let mut step = half & !1;
        let mut at = 0i32;
        while step > 2 {
            let probe = (step + at) as usize;
            if (args.get(probe).copied().ok_or(Error)? & !0xF) < ppem {
                at += step;
            }
            half >>= 1;
            step = half & !1;
        }
        let mut i = at;
        while i < count {
            let a = args[i as usize];
            if a & !0xF == ppem {
                let target = *args.get(i as usize + 1).ok_or(Error)?;
                let d = Self::delta_step(a, shift);
                if cvt {
                    let k = idx(target)?;
                    let by = vardiv(d, stretch(self.proj), 16);
                    let c = self.it.cvt.get_mut(k).ok_or(Error)?;
                    *c = c.wrapping_add(by);
                } else {
                    self.move_point(self.zp[0], target, d)?;
                }
            } else if (a & !0xF) > ppem {
                break;
            }
            i += 2;
        }
        Ok(())
    }

    /// The contour of zp0 holding point `p`: its first and last point.
    fn contour_of(&self, z: usize, p: i32) -> R<(i32, i32)> {
        let e = self.zone(z);
        let mut c = 0;
        loop {
            let end = i32::from(*e.ep.get(c).ok_or(Error)?);
            if p <= end {
                let start = if c == 0 { 0 } else { i32::from(e.ep[c - 1]) + 1 };
                return Ok((start, end));
            }
            c += 1;
        }
    }

    /// The SDK's contour delta (sub_188880): at this ppem, whole contours (of the listed points) moved.
    fn delta_contours(&mut self, add: u16) -> R {
        let k = self.pop()?;
        let pts_at = (self.stack.len() as i64).checked_sub(i64::from(k)).filter(|v| *v >= 1).ok_or(Error)? as usize;
        let points: Vec<i32> = self.stack.split_off(pts_at);
        let n = self.pop()?;
        let at = (self.stack.len() as i64).checked_sub(i64::from(n)).filter(|v| *v >= 0).ok_or(Error)? as usize;
        let args: Vec<i32> = self.stack.split_off(at);
        let (ppem, shift) = self.delta_ppem(add);
        let z = self.zp[0];
        for &a in &args {
            if a & !0xF == ppem {
                if k >= 1 {
                    let d = Self::delta_step(a, shift);
                    for &p in points.iter().rev() {
                        let (s, t) = self.contour_of(z, i32::from(p as i16))?;
                        let (fx, fy) = (i32::from(self.free[0]), i32::from(self.free[1]));
                        if fx != 0 {
                            let by = varmul(d, fx, 14);
                            for q in s..=t {
                                let q = self.check(z, q)?;
                                self.zones[z].x[q] = self.zones[z].x[q].wrapping_add(by);
                                self.zones[z].f[q] |= 1;
                            }
                        }
                        if fy != 0 {
                            let by = varmul(d, fy, 14);
                            for q in s..=t {
                                let q = self.check(z, q)?;
                                self.zones[z].y[q] = self.zones[z].y[q].wrapping_add(by);
                                self.zones[z].f[q] |= 2;
                            }
                        }
                    }
                }
                return Ok(());
            }
            if (a & !0xF) > ppem {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The point's arguments for the SDK's run and level deltas: the point (zp0) and its delta arguments.
    fn delta_point_args(&mut self) -> R<(i32, Vec<i32>)> {
        let p = self.pop()? as i16;
        let n = self.pop()?;
        let at = (self.stack.len() as i64).checked_sub(i64::from(n)).filter(|v| *v >= 0).ok_or(Error)? as usize;
        Ok((i32::from(p), self.stack.split_off(at)))
    }

    fn on_curve(&self, z: usize, p: i32) -> R<bool> {
        Ok(self.zone(z).on.get(idx(p)?).ok_or(Error)? != &0)
    }

    fn orig_proj(&self, z: usize, p: i32) -> R<i32> {
        let (x, y) = self.at(z, p, 1)?;
        Ok(self.project(x, y))
    }

    /// The SDK's run delta (sub_188AB0): an on-curve point and the on-curve points next to it in its contour at
    /// the same place along the projection vector, moved at this ppem.
    fn delta_run(&mut self, add: u16) -> R {
        let (p, args) = self.delta_point_args()?;
        let z = self.zp[0];
        if !self.on_curve(z, p)? {
            return Ok(());
        }
        let (ppem, shift) = self.delta_ppem(add);
        for &a in &args {
            if a & !0xF == ppem {
                let (start, end) = self.contour_of(z, p)?;
                let level = self.orig_proj(z, p)?;
                let mut hi = p;
                if end > p {
                    let mut q = p + 1;
                    loop {
                        if !self.on_curve(z, q)? || self.orig_proj(z, q)? != level {
                            hi = q - 1;
                            break;
                        }
                        hi += 1;
                        if q == end {
                            break;
                        }
                        q += 1;
                    }
                }
                let mut lo = p;
                if p > start {
                    let mut q = p - 1;
                    loop {
                        if !self.on_curve(z, q)? || self.orig_proj(z, q)? != level {
                            lo = q + 1;
                            break;
                        }
                        lo -= 1;
                        if q <= start {
                            break;
                        }
                        q -= 1;
                    }
                }
                let d = Self::delta_step(a, shift);
                let (fx, fy) = (i32::from(self.free[0]), i32::from(self.free[1]));
                if fx != 0 {
                    let by = varmul(d, fx, 14);
                    for q in lo..=hi {
                        let q = self.check(z, q)?;
                        self.zones[z].x[q] = self.zones[z].x[q].wrapping_add(by);
                        self.zones[z].f[q] |= 1;
                    }
                }
                if fy != 0 {
                    let by = varmul(d, fy, 14);
                    for q in lo..=hi {
                        let q = self.check(z, q)?;
                        self.zones[z].y[q] = self.zones[z].y[q].wrapping_add(by);
                        self.zones[z].f[q] |= 2;
                    }
                }
                return Ok(());
            }
            if (a & !0xF) > ppem {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The SDK's level delta (sub_188DA0): every on-curve point of the zone at an on-curve point's place along the
    /// projection vector moved at this ppem.
    fn delta_level(&mut self, add: u16) -> R {
        let (p, args) = self.delta_point_args()?;
        let z = self.zp[0];
        if !self.on_curve(z, p)? {
            return Ok(());
        }
        let (ppem, shift) = self.delta_ppem(add);
        for &a in &args {
            if a & !0xF == ppem {
                let d = Self::delta_step(a, shift);
                let dx = varmul(d, i32::from(self.free[0]), 14);
                let dy = varmul(d, i32::from(self.free[1]), 14);
                let level = self.orig_proj(z, p)?;
                let last = i32::from(*self.zone(z).ep.last().ok_or(Error)? as i16);
                for q in (0..=last).rev() {
                    if self.on_curve(z, q)? && self.orig_proj(z, q)? == level {
                        let q = self.check(z, q)?;
                        let e = &mut self.zones[z];
                        if self.free[0] != 0 {
                            e.x[q] = e.x[q].wrapping_add(dx);
                            e.f[q] |= 1;
                        }
                        if self.free[1] != 0 {
                            e.y[q] = e.y[q].wrapping_add(dy);
                            e.f[q] |= 2;
                        }
                    }
                }
                return Ok(());
            }
            if (a & !0xF) > ppem {
                return Ok(());
            }
        }
        Ok(())
    }

    /// SHPIX (sub_1807C0): the amount popped, then SLOOP's count of zp2's points, each moved by it along the
    /// freedom vector.
    fn shpix(&mut self) -> R {
        let amount = self.pop()?;
        let dx = if self.free[0] != 0 { varmul(amount, i32::from(self.free[0]), 14) } else { 0 };
        let dy = if self.free[1] != 0 { varmul(amount, i32::from(self.free[1]), 14) } else { 0 };
        let z = self.zp[2];
        for _ in 0..self.loop_.max(-1) + 1 {
            let p = self.pop_point(z)?;
            self.shift(z, p, (dx, dy));
        }
        self.loop_ = 0;
        Ok(())
    }

    /// AA[] (sub_181BA0), the SDK's own instruction: selector 11 autohints the glyph; with fsFlags bit 1 clear (a
    /// glyph cache's 0x21404000) 252 and 253 run the SDK's compressed delta hints, 5 to 9, 250 and 251 are errors
    /// and the rest do nothing.
    fn aa(&mut self) -> R {
        match self.pop()? {
            11 => {
                if let Some(f) = self.autohint.as_mut() {
                    f(&mut self.zones[1]);
                }
                Ok(())
            }
            252 => self.deltas_252(),
            253 => self.deltas_253(),
            5..=9 | 250 | 251 => Err(Error),
            _ => Ok(()),
        }
    }

    /// The next `n` bits of a compressed hint stream: the stack's values from the top, the low 16 bits of each,
    /// most significant first, a value popped once read through (a read past its end takes the next one's first
    /// bits). `pos` is the bit the top value is read from.
    fn bits(&mut self, pos: &mut u32, n: u32) -> R<i32> {
        let len = self.stack.len();
        let top = *self.stack.last().ok_or(Error)? as u32 & 0xFFFF;
        let end = *pos + n;
        let word = if end > 16 { top << 16 | *self.stack.get(len.wrapping_sub(2)).ok_or(Error)? as u32 & 0xFFFF } else { top };
        let v = (word >> ((16 - end) & 15)) & ((1 << n) - 1);
        if end >= 16 {
            self.stack.pop();
        }
        *pos = end % 16;
        Ok(v as i32)
    }

    /// The points shifted together by `amount` (the stack's top values, SLOOP's count less one in `loop_`), as SHPIX.
    fn shift_by(&mut self, points: impl IntoIterator<Item = i32>, loop_: i32, amount: i32) -> R {
        for p in points {
            self.push(p)?;
        }
        self.push(amount)?;
        self.loop_ = loop_;
        self.shpix()
    }

    /// AA[] 253 (sub_181BA0): delta hints packed in the stack under the count of values to skip and the bit to
    /// start from with the size they start at (when that is no larger than this one), the first size and bit,
    /// the amount step and the amount. Per size, from the first: runs of up to eight points or single points by
    /// their distance from the last, each shifted by the amount, points skipped by 16 or 8, a repeat count for
    /// the next skip, size or amount step; the stream ends past this size, the stack emptied.
    fn deltas_253(&mut self) -> R {
        let amount0 = self.pop()?;
        let step = self.pop()?;
        let first = self.pop()?;
        let resume = self.pop()?;
        let ppem = i32::from(self.ppem() as u16);
        let (mut size, mut pos) = (first & 0xFF, 0);
        let (skip, bit) = (resume & 0x3FF, ((first >> 9) & 0xF) as u32);
        if skip + bit as i32 != 0 && i32::from(resume as u16 >> 10) <= ppem {
            let n = self.stack.len().checked_sub(skip as usize).ok_or(Error)?;
            self.stack.truncate(n);
            (size, pos) = (i32::from(resume as u16 >> 10), bit);
        }
        let (mut amount, mut point, mut rep) = (amount0, 0i32, 1i32);
        loop {
            match self.bits(&mut pos, 2)? {
                2 => {
                    let n = self.bits(&mut pos, 3)? + 1;
                    let at = self.bits(&mut pos, 3)?;
                    if size == ppem {
                        let p = point.wrapping_add(at);
                        point = p.wrapping_add(if n >= 8 { n } else { n + 1 });
                        self.shift_by((0..n).map(|i| p.wrapping_add(i)), n - 1, amount)?;
                    }
                }
                1 => {
                    let at = match self.bits(&mut pos, 2)? {
                        2 => 0,
                        1 => 3 - self.bits(&mut pos, 1)?,
                        0 => 5 - self.bits(&mut pos, 1)?,
                        _ if self.bits(&mut pos, 1)? == 1 => 1,
                        _ => 7 - self.bits(&mut pos, 1)?,
                    };
                    if size == ppem {
                        let p = point.wrapping_add(at);
                        point = p.wrapping_add(2);
                        let l = self.loop_;
                        self.shift_by([p], l, amount)?;
                    }
                }
                3 => {
                    if self.bits(&mut pos, 1)? == 1 {
                        for _ in 0..rep {
                            amount = if amount.wrapping_add(step) != 0 { amount.wrapping_add(step) } else { step };
                        }
                    } else {
                        amount = amount0;
                        size = size.wrapping_add(rep);
                        if size > ppem {
                            self.stack.clear();
                            return Ok(());
                        }
                    }
                    (point, rep) = (0, 1);
                }
                _ => {
                    if self.bits(&mut pos, 1)? == 0 {
                        (point, rep) = (point.wrapping_add(16 * rep), 1);
                    } else if self.bits(&mut pos, 1)? == 1 {
                        (point, rep) = (point.wrapping_add(8 * rep), 1);
                    } else if self.bits(&mut pos, 1)? == 0 {
                        self.stack.clear();
                        return Ok(());
                    } else {
                        rep = match self.bits(&mut pos, 2)? {
                            0 => 5,
                            1 if self.bits(&mut pos, 1)? == 1 => 6,
                            1 => 10 - self.bits(&mut pos, 1)?,
                            2 if self.bits(&mut pos, 1)? == 1 => 11,
                            2 => 8 - self.bits(&mut pos, 1)?,
                            _ => 4,
                        };
                    }
                }
            }
        }
    }

    /// AA[] 252 (sub_184450): delta hints packed in the stack under their amount (its low byte signed by bit 15,
    /// or the value below when that byte is 0 and the bit set, times 2 to the power of bits 8 to 11 less 10 above
    /// 9, else plus 6). The stream's codes (`DELTA_CODES`) walk sizes from 5 and points or contours of the glyph
    /// from the first, shifting them at this size or a range of sizes from the current one; an amount step goes
    /// back to size 5 and the first point.
    fn deltas_252(&mut self) -> R {
        let w = self.pop()?;
        let e = (w >> 8) & 0xF;
        let shift = (e + if e <= 9 { 6 } else { -10 }) as u32;
        let mut v = if w & 0x8000 != 0 { -(w & 0xFF) } else { w & 0xFF };
        if w & 0x80FF == 0x8000 {
            v = self.pop()?;
        }
        let unit = 1i32 << shift;
        let mut amount = v.wrapping_shl(shift);
        let ppem = i32::from(self.ppem() as u16);
        let (mut size, mut p, mut pos) = (5i32, 0i32, 0u32);
        loop {
            let (mut len, mut bits) = (0, 0u16);
            let code = loop {
                bits = bits << 1 | self.bits(&mut pos, 1)? as u16;
                len += 1;
                if let Some(&(_, _, c)) = DELTA_CODES.iter().find(|&&(n, b, _)| n == len && b == bits) {
                    break c;
                }
            };
            let sizes = |range: u8| (ppem.wrapping_sub(size) as u32) < u32::from(range);
            let g = &self.zones[1];
            let last = || g.ep.last().map(|&e| i32::from(e)).ok_or(Error);
            match code {
                Delta::Same(range) => {
                    if sizes(range) {
                        let orig = if self.proj[0] != 0 { &g.ox } else { &g.oy };
                        let at = *orig.get(idx(p)?).ok_or(Error)?;
                        let same: Vec<i32> = (0..=last()?).filter(|&i| g.on[i as usize] != 0 && orig[i as usize] == at).collect();
                        let n = same.len() as i32;
                        self.shift_by(same, n - 1, amount)?;
                    }
                    p = p.wrapping_add(1);
                }
                Delta::Contours(count, range) => {
                    if sizes(range) {
                        let from = i32::from(*g.sp.get(idx(p)?).ok_or(Error)?);
                        let to = i32::from(*g.ep.get(idx(p.wrapping_add(i32::from(count) - 1))?).ok_or(Error)?);
                        self.shift_by(from..=to, to - from, amount)?;
                    }
                }
                Delta::Point(range) => {
                    if sizes(range) {
                        let l = self.loop_;
                        self.shift_by([p], l, amount)?;
                    }
                    p = p.wrapping_add(1);
                }
                Delta::Run(count) => {
                    if sizes(1) {
                        let count = i32::from(count);
                        self.shift_by((0..count).map(|i| p.wrapping_add(i)), count - 1, amount)?;
                    }
                    p = p.wrapping_add(1);
                }
                Delta::All(range) => {
                    if sizes(range) {
                        let last = last()?;
                        self.shift_by((0..=last).rev(), last, amount)?;
                    }
                }
                Delta::Amount(steps) => {
                    let next = (i32::from(steps) << shift).wrapping_add(amount);
                    amount = next.wrapping_add(if next > -1 && amount < 0 { unit } else { 0 });
                    (size, p) = (5, 0);
                }
                Delta::Size(n) => (size, p) = (size.wrapping_add(i32::from(n)), 0),
                Delta::Skip(n) => p = p.wrapping_add(i32::from(n)),
                Delta::End => {
                    if pos != 0 {
                        self.stack.pop();
                    }
                    return Ok(());
                }
            }
        }
    }
}

/// What a code of AA[] 252's stream does. A range is the count of sizes from the current one it applies at.
#[derive(Clone, Copy)]
enum Delta {
    /// the glyph's on-curve points sharing the current point's original coordinate along the projection axis
    /// (x unless the projection vector is vertical), over a range; on to the next point
    Same(u8),
    /// a count of contours from the current point's number taken as a contour's, over a range
    Contours(u8, u8),
    /// the current point over a range (SLOOP's count kept); on to the next point
    Point(u8),
    /// a count of points from the current one; on to the next point
    Run(u8),
    /// every point of the glyph, the last first, over a range
    All(u8),
    /// the amount on by a count of steps (skipping 0), back to size 5 and the first point
    Amount(u16),
    /// sizes on, back to the first point
    Size(u16),
    /// points on
    Skip(u16),
    End,
}

/// AA[] 252's codes (sub_184450's decision tree): length, bits, meaning; a complete prefix code.
const DELTA_CODES: [(u8, u16, Delta); 191] = [
    (3, 0b000, Delta::Same(1)),
    (4, 0b1000, Delta::Size(1)),
    (5, 0b00101, Delta::Contours(1, 1)),
    (5, 0b01001, Delta::Point(1)),
    (5, 0b01011, Delta::Run(2)),
    (5, 0b01100, Delta::Amount(1)),
    (5, 0b10011, Delta::Size(7)),
    (5, 0b10100, Delta::Size(2)),
    (5, 0b11010, Delta::Skip(16)),
    (5, 0b11011, Delta::Skip(1)),
    (6, 0b001001, Delta::Contours(1, 2)),
    (6, 0b001100, Delta::Contours(4, 1)),
    (6, 0b001110, Delta::Contours(3, 1)),
    (6, 0b001111, Delta::Contours(2, 1)),
    (6, 0b010001, Delta::Point(2)),
    (6, 0b011101, Delta::End),
    (6, 0b011111, Delta::All(1)),
    (6, 0b100100, Delta::Size(9)),
    (6, 0b101010, Delta::Size(4)),
    (6, 0b101100, Delta::Size(16)),
    (6, 0b101101, Delta::Size(8)),
    (6, 0b101110, Delta::Size(3)),
    (6, 0b110000, Delta::Skip(6)),
    (6, 0b110010, Delta::Skip(5)),
    (6, 0b111000, Delta::Skip(48)),
    (6, 0b111010, Delta::Skip(4)),
    (6, 0b111101, Delta::Skip(3)),
    (6, 0b111110, Delta::Skip(32)),
    (6, 0b111111, Delta::Skip(2)),
    (7, 0b0010001, Delta::Contours(1, 3)),
    (7, 0b0011010, Delta::Contours(5, 1)),
    (7, 0b0100001, Delta::Point(3)),
    (7, 0b0101010, Delta::Run(3)),
    (7, 0b0101011, Delta::Run(4)),
    (7, 0b0110100, Delta::Amount(3)),
    (7, 0b0110110, Delta::Amount(2)),
    (7, 0b0111001, Delta::Same(2)),
    (7, 0b0111101, Delta::All(2)),
    (7, 0b1001010, Delta::Size(11)),
    (7, 0b1001011, Delta::Size(6)),
    (7, 0b1010111, Delta::Size(10)),
    (7, 0b1011111, Delta::Size(5)),
    (7, 0b1100011, Delta::Skip(12)),
    (7, 0b1100110, Delta::Skip(14)),
    (7, 0b1100111, Delta::Skip(10)),
    (7, 0b1110010, Delta::Skip(9)),
    (7, 0b1110011, Delta::Skip(64)),
    (7, 0b1110110, Delta::Skip(11)),
    (7, 0b1110111, Delta::Skip(7)),
    (7, 0b1111000, Delta::Skip(8)),
    (8, 0b00100000, Delta::Contours(1, 4)),
    (8, 0b00110111, Delta::Contours(6, 1)),
    (8, 0b01010000, Delta::Run(7)),
    (8, 0b01010010, Delta::Run(5)),
    (8, 0b01010011, Delta::Run(6)),
    (8, 0b01101011, Delta::Amount(5)),
    (8, 0b01101111, Delta::Amount(4)),
    (8, 0b01110001, Delta::Same(3)),
    (8, 0b01111001, Delta::All(3)),
    (8, 0b10101100, Delta::Size(14)),
    (8, 0b10101101, Delta::Size(13)),
    (8, 0b10111100, Delta::Size(12)),
    (8, 0b11000101, Delta::Skip(80)),
    (8, 0b11110010, Delta::Skip(13)),
    (8, 0b11110011, Delta::Skip(15)),
    (9, 0b001101101, Delta::Contours(7, 1)),
    (9, 0b010000011, Delta::Point(4)),
    (9, 0b011011100, Delta::Amount(6)),
    (9, 0b011011101, Delta::Amount(8)),
    (9, 0b011100001, Delta::Same(4)),
    (9, 0b011110000, Delta::All(4)),
    (9, 0b101111010, Delta::Size(15)),
    (9, 0b110001001, Delta::Skip(96)),
    (10, 0b0010000110, Delta::Contours(1, 6)),
    (10, 0b0010000111, Delta::Contours(1, 5)),
    (10, 0b0011011000, Delta::Contours(9, 1)),
    (10, 0b0100000100, Delta::Point(5)),
    (10, 0b0101000100, Delta::Run(9)),
    (10, 0b0101000111, Delta::Run(8)),
    (10, 0b0110101000, Delta::Amount(10)),
    (10, 0b0110101010, Delta::Amount(7)),
    (10, 0b0110101011, Delta::Amount(9)),
    (10, 0b1011110111, Delta::Size(32)),
    (10, 0b1100010001, Delta::Skip(112)),
    (11, 0b00100001000, Delta::Contours(1, 7)),
    (11, 0b00110110011, Delta::Contours(8, 1)),
    (11, 0b01000000000, Delta::Point(8)),
    (11, 0b01000000001, Delta::Point(9)),
    (11, 0b01000000010, Delta::Point(10)),
    (11, 0b01000000011, Delta::Point(11)),
    (11, 0b01000000100, Delta::Point(12)),
    (11, 0b01000000101, Delta::Point(13)),
    (11, 0b01000000110, Delta::Point(14)),
    (11, 0b01000000111, Delta::Point(15)),
    (11, 0b01000001010, Delta::Point(16)),
    (11, 0b01010001010, Delta::Run(16)),
    (11, 0b01010001011, Delta::Run(11)),
    (11, 0b01010001100, Delta::Run(10)),
    (11, 0b01110000001, Delta::Same(6)),
    (11, 0b01110000011, Delta::Same(5)),
    (11, 0b01111000101, Delta::All(7)),
    (11, 0b01111000110, Delta::All(6)),
    (12, 0b001000010010, Delta::Contours(1, 9)),
    (12, 0b001000010110, Delta::Contours(1, 8)),
    (12, 0b001000010111, Delta::Contours(1, 10)),
    (12, 0b001101100101, Delta::Contours(10, 1)),
    (12, 0b010000010110, Delta::Point(6)),
    (12, 0b010000010111, Delta::Point(7)),
    (12, 0b010100011010, Delta::Run(12)),
    (12, 0b011010100110, Delta::Amount(12)),
    (12, 0b011100000000, Delta::Same(8)),
    (12, 0b011110001111, Delta::All(5)),
    (13, 0b0010000100110, Delta::Contours(1, 11)),
    (13, 0b0010000100111, Delta::Contours(1, 12)),
    (13, 0b0010000101000, Delta::Contours(1, 13)),
    (13, 0b0010000101001, Delta::Contours(1, 14)),
    (13, 0b0010000101010, Delta::Contours(1, 15)),
    (13, 0b0010000101011, Delta::Contours(1, 16)),
    (13, 0b0101000110110, Delta::Run(14)),
    (13, 0b0110101001110, Delta::Amount(11)),
    (13, 0b0110101001111, Delta::Amount(14)),
    (13, 0b0111000000011, Delta::Same(7)),
    (13, 0b0111000001001, Delta::Same(11)),
    (13, 0b0111100010000, Delta::All(16)),
    (13, 0b0111100010010, Delta::All(9)),
    (13, 0b0111100011101, Delta::All(10)),
    (13, 0b1011110110000, Delta::Size(240)),
    (13, 0b1011110110001, Delta::Size(256)),
    (13, 0b1100010000000, Delta::Skip(160)),
    (13, 0b1100010000001, Delta::Skip(176)),
    (13, 0b1100010000010, Delta::Skip(192)),
    (13, 0b1100010000011, Delta::Skip(208)),
    (13, 0b1100010000100, Delta::Skip(224)),
    (13, 0b1100010000101, Delta::Skip(240)),
    (13, 0b1100010000110, Delta::Skip(256)),
    (14, 0b00110110010001, Delta::Contours(11, 1)),
    (14, 0b01010001101110, Delta::Run(15)),
    (14, 0b01110000000100, Delta::Same(1)),
    (14, 0b01110000000101, Delta::Same(9)),
    (14, 0b01110000010000, Delta::Same(10)),
    (14, 0b01110000010001, Delta::Same(12)),
    (14, 0b01110000010100, Delta::Same(13)),
    (14, 0b01110000010101, Delta::Same(14)),
    (14, 0b01110000010110, Delta::Same(15)),
    (14, 0b01110000010111, Delta::Same(16)),
    (14, 0b01111000100010, Delta::All(8)),
    (14, 0b01111000100011, Delta::All(11)),
    (14, 0b01111000100110, Delta::All(12)),
    (14, 0b01111000100111, Delta::All(13)),
    (14, 0b01111000111000, Delta::All(14)),
    (14, 0b01111000111001, Delta::All(15)),
    (14, 0b10111101100100, Delta::Size(48)),
    (14, 0b10111101100101, Delta::Size(64)),
    (14, 0b10111101100110, Delta::Size(80)),
    (14, 0b10111101100111, Delta::Size(96)),
    (14, 0b10111101101000, Delta::Size(112)),
    (14, 0b10111101101001, Delta::Size(128)),
    (14, 0b10111101101010, Delta::Size(144)),
    (14, 0b10111101101011, Delta::Size(160)),
    (14, 0b10111101101100, Delta::Size(176)),
    (14, 0b10111101101101, Delta::Size(192)),
    (14, 0b10111101101110, Delta::Size(208)),
    (14, 0b10111101101111, Delta::Size(224)),
    (14, 0b11000100001110, Delta::Skip(128)),
    (14, 0b11000100001111, Delta::Skip(144)),
    (15, 0b001101100100000, Delta::Contours(1, 1)),
    (15, 0b001101100100001, Delta::Contours(12, 1)),
    (15, 0b001101100100100, Delta::Contours(13, 1)),
    (15, 0b001101100100101, Delta::Contours(14, 1)),
    (15, 0b001101100100110, Delta::Contours(15, 1)),
    (15, 0b001101100100111, Delta::Contours(16, 1)),
    (15, 0b010100011011110, Delta::Run(1)),
    (15, 0b010100011011111, Delta::Run(13)),
    (15, 0b011010100100000, Delta::Amount(48)),
    (15, 0b011010100100001, Delta::Amount(64)),
    (15, 0b011010100100010, Delta::Amount(80)),
    (15, 0b011010100100011, Delta::Amount(96)),
    (15, 0b011010100100100, Delta::Amount(112)),
    (15, 0b011010100100101, Delta::Amount(128)),
    (15, 0b011010100100110, Delta::Amount(144)),
    (15, 0b011010100100111, Delta::Amount(160)),
    (15, 0b011010100101000, Delta::Amount(176)),
    (15, 0b011010100101001, Delta::Amount(192)),
    (15, 0b011010100101010, Delta::Amount(208)),
    (15, 0b011010100101011, Delta::Amount(224)),
    (15, 0b011010100101100, Delta::Amount(240)),
    (15, 0b011010100101101, Delta::Amount(256)),
    (16, 0b0110101001011100, Delta::Amount(13)),
    (16, 0b0110101001011101, Delta::Amount(15)),
    (16, 0b0110101001011110, Delta::Amount(16)),
    (16, 0b0110101001011111, Delta::Amount(32)),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A glyph of points with their original positions (the instructed ones start there), on-curve flags and
    /// contour ends.
    fn glyph(x: Vec<i32>, y: Vec<i32>, ox: Vec<i32>, oy: Vec<i32>, on: Vec<u8>, ep: Vec<u16>) -> Element {
        let sp = std::iter::once(0).chain(ep.iter().take(ep.len().saturating_sub(1)).map(|&e| e.wrapping_add(1))).collect();
        let f = vec![0; x.len()];
        Element { x, y, oox: ox.clone(), ooy: oy.clone(), ox, oy, on, f, sp, ep }
    }

    /// AA[] (its selector on top of `stack`) over glyph `g` alone, every zone pointer on it: the result, the
    /// glyph, the stack and the loop count after.
    fn aa(stack: &[i32], ppem: i32, pv: [i16; 2], fv: [i16; 2], loop_: i32, g: Element) -> (R, Element, Vec<i32>, i32) {
        let mut it = Interp {
            id: 0,
            fpgm: Arc::from(&[][..]),
            prep: Arc::from(&[][..]),
            cvt_fu: Vec::new(),
            cvt: Vec::new(),
            storage: Vec::new(),
            funcs: Vec::new(),
            idefs: Vec::new(),
            max_idefs: 0,
            twilight: Element::default(),
            defaults: DEFAULTS,
            cur: DEFAULTS,
            scale: Scale::new(ppem << 16, 2048),
            ppem,
            prepped: true,
            broken: false,
        };
        let mut m = Machine {
            it: &mut it,
            zones: [Element::default(), g],
            zp: [1; 3],
            proj: pv,
            free: fv,
            dual: pv,
            pf: 0x4000,
            mover: Mover::X,
            project: Proj::X,
            old_project: Proj::X,
            rp: [0; 3],
            loop_,
            op: 0x7F,
            stack: stack.to_vec(),
            code: Arc::from(&[][..]),
            ip: 0,
            budget: BUDGET,
            depth: 0,
            autohint: None,
        };
        let r = m.aa();
        let [_, g] = std::mem::take(&mut m.zones);
        (r, g, m.stack, m.loop_)
    }

    /// The compressed delta hints as the SDK's sub_181BA0 moves the points (the executable run under an
    /// emulator on the same state).
    #[test]
    fn compressed_deltas() {
        let ox = vec![0, 64, 0, 0, 64, 64, -128, -128, 64, 0, 128, 64];
        let oy = vec![-128, -192, 0, -64, -128, -192, 64, 128, 128, -192, 64, 0];
        let on = vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1];
        let g = glyph(ox.clone(), oy.clone(), ox, oy, on, vec![3, 7, 11]);
        let x = [0x4000, 0];
        let (r, e, stack, _) = aa(&[42762, 18898, 48153, 0, 7, 64, 64, 253], 7, x, x, 0, g.clone());
        assert_eq!((r.is_ok(), e.x, stack), (true, vec![0, 64, 0, 0, 128, 128, -64, -64, 128, 64, 192, 128], vec![]));
        let (r, e, stack, _) = aa(&[30267, 31511, 37631, 7770, 30851, 257, 252], 12, x, x, 0, g);
        assert_eq!((r.is_ok(), e.x, stack), (true, vec![128, 192, 128, 128, 192, 192, 0, 0, 192, 128, 256, 192], vec![]));
    }

    /// tools/itype/aa_fuzz.py's cases (KNIFE_AA_CASES, one per line: `ppem pv.x pv.y fv.x fv.y loop|stack|x|y|
    /// ox|oy|on|ep`), each one's result written to KNIFE_AA_OUT: `ok|stack|loop|x|y|flags` or `error`.
    #[test]
    fn exe_cases() {
        let Ok(path) = std::env::var("KNIFE_AA_CASES") else { return };
        let ints = |p: &str| p.split_whitespace().map(|v| v.parse::<i32>().unwrap()).collect::<Vec<_>>();
        let join = |v: &mut dyn Iterator<Item = String>| v.collect::<Vec<_>>().join(" ");
        let mut out = String::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let p: Vec<Vec<i32>> = line.split('|').map(ints).collect();
            let h = &p[0];
            let on = p[6].iter().map(|&v| v as u8).collect();
            let ep = p[7].iter().map(|&v| v as u16).collect();
            let g = glyph(p[2].clone(), p[3].clone(), p[4].clone(), p[5].clone(), on, ep);
            let (pv, fv) = ([h[1] as i16, h[2] as i16], [h[3] as i16, h[4] as i16]);
            let (r, g, stack, loop_) = aa(&p[1], h[0], pv, fv, h[5], g);
            out += &match r {
                Ok(()) => format!(
                    "ok|{}|{loop_}|{}|{}|{}\n",
                    join(&mut stack.iter().map(i32::to_string)),
                    join(&mut g.x.iter().map(i32::to_string)),
                    join(&mut g.y.iter().map(i32::to_string)),
                    join(&mut g.f.iter().map(u8::to_string))
                ),
                Err(_) => "error\n".to_string(),
            };
        }
        std::fs::write(std::env::var("KNIFE_AA_OUT").unwrap(), out).unwrap();
    }
}
