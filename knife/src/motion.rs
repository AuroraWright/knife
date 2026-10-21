//! STMA keyframe motions baked into the executable, and the engine's easing curves.

use std::collections::HashMap;

use anyhow::{Context, Result};
use knife_core::exe::Nso;

use crate::motion_loops::MOTION_LOOPS;

const KEYS_VA: u64 = 0x65CE18;
const KEY_COUNT: usize = 0x118E1;
const ENDS_VA: u64 = 0x3B1940;
const LENS_VA: u64 = 0x399F40;
const MOTION_COUNT: usize = 0xBD * 128;
/// `qword_36AB38[id]` (sub_99960): the fields a motion drives, one or more bits per field (`gate`).
const MASKS_VA: u64 = 0x36AB38;
const MASK_COUNT: usize = 0x5E81;
/// sub_99950: this motion's clock is eased as a whole by its first key's ease, its segments run linear.
const EASED_WHOLE: i32 = 9999;

pub const NO_KEY: i32 = -32767;
pub const HOLD: i32 = -32766;
const NO_KEY_WIDE: i32 = -100001;
const HOLD_WIDE: i32 = -100000;

/// Field numbers (as used by the exe's accessor `sub_99FF0`).
pub mod field {
    pub const TIME: usize = 0;
    pub const EASE: usize = 1;
    pub const INTERP: usize = 2;
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const ALPHA: usize = 5;
    pub const PIVOT_X: usize = 6;
    pub const PIVOT_Y: usize = 7;
    pub const ROT: usize = 8;
    pub const ZOOM_X: usize = 9;
    pub const ZOOM_Y: usize = 10;
    pub const CLIP_MODE: usize = 15;
    pub const CLIP_INSET: usize = 16;
    pub const BLEND: usize = 17;
    pub const FILL: usize = 18;
    pub const SNX2: usize = 24;
    pub const SNX3: usize = 25;
    pub const GRAIN: usize = 26;
    pub const VARIANT_A: usize = 22;
    pub const VARIANT_B: usize = 23;
    pub const PRIO: usize = 27;
}

pub const FIELDS: usize = 28;

pub struct MotionTable {
    keys: Vec<[i32; FIELDS]>,
    ends: Vec<u32>,
    lens: Vec<u32>,
    masks: Vec<u64>,
    loops: HashMap<u16, u32>,
}

/// sub_97B80's gate bits per field (tested after `MotionTable::mask_at` clears the absent ones).
mod gate {
    pub const X: u64 = 0x40880;
    pub const Y: u64 = 0x5100000;
    pub const ALPHA: u64 = 0x4000;
    pub const PIVOT_X: u64 = 0x10002;
    pub const PIVOT_Y: u64 = 0x20004;
    pub const ROT: u64 = 0x88000;
    pub const ROT2: u64 = 0x400000000;
    pub const ZOOM_X: u64 = 0x80200000;
    pub const ZOOM_Y: u64 = 0x100400000;
    pub const CLIP_X: u64 = 0x10000080;
    pub const CLIP_Y: u64 = 0x4000400;
    pub const CLIP_MODE: u64 = 0x10;
    pub const CLIP_INSET: u64 = 0x20;
    pub const BLEND: u64 = 0x2000000;
    pub const FILL: u64 = 0x200;
    pub const COLOR: u64 = 0x200000000;
    pub const SNX2: u64 = 0x40;
    pub const SNX3: u64 = 0x100;
    pub const GRAIN: u64 = 0x1000;
    pub const PRIO: u64 = 0x1;
    pub const VARIANT_A: u64 = 0x20000000;
    pub const VARIANT_B: u64 = 0x40000000;
}

fn rd_u32(nso: &Nso, va: u64) -> Option<u32> {
    Some(u32::from_le_bytes(nso.bytes(va, 4)?.try_into().ok()?))
}

impl MotionTable {
    pub fn load(nso: &Nso) -> Result<Self> {
        let raw = nso.bytes(KEYS_VA, KEY_COUNT * 64).context("motion keys out of range")?;
        let keys = raw
            .as_chunks::<64>().0.iter()
            .map(|k| {
                let mut f = [NO_KEY; FIELDS];
                let i16_at = |o: usize| i16::from_le_bytes([k[o], k[o + 1]]) as i32;
                f[0] = u32::from_le_bytes(k[0..4].try_into().unwrap()) as i32;
                for (n, slot) in f.iter_mut().enumerate().take(8).skip(1) {
                    *slot = i16_at(4 + (n - 1) * 2);
                }
                f[8] = i32::from_le_bytes(k[20..24].try_into().unwrap());
                for (n, slot) in f.iter_mut().enumerate().skip(9) {
                    *slot = i16_at(24 + (n - 9) * 2);
                }
                f
            })
            .collect();
        let ends = (0..MOTION_COUNT)
            .map(|i| rd_u32(nso, ENDS_VA + i as u64 * 4).unwrap_or(0))
            .collect();
        let lens = (0..MOTION_COUNT)
            .map(|i| rd_u32(nso, LENS_VA + i as u64 * 4).unwrap_or(0))
            .collect();
        let masks = (0..MASK_COUNT)
            .map(|i| {
                nso.bytes(MASKS_VA + i as u64 * 8, 8)
                    .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
            })
            .collect();
        Ok(Self {
            keys,
            ends,
            lens,
            masks,
            loops: MOTION_LOOPS.iter().copied().collect(),
        })
    }

    /// Key index range of a motion id.
    fn range(&self, id: i32) -> Option<(usize, usize)> {
        if id < 1 || id as usize > self.ends.len() {
            return None;
        }
        let start = if id >= 2 { self.ends[id as usize - 2] } else { 0 } as usize;
        let end = self.ends[id as usize - 1] as usize;
        (end > start && end <= self.keys.len()).then_some((start, end))
    }

    pub fn length(&self, id: i32) -> u32 {
        if id < 1 {
            return 0;
        }
        self.lens.get(id as usize - 1).copied().unwrap_or(0)
    }

    /// SGMA: overwrite key `key` of motion `id` with a layer's current state so the
    /// following STMA starts where the layer is. Each value is (field, value).
    pub fn patch_key(&mut self, id: i32, key: usize, values: &[(usize, i32)]) {
        let Some((s, e)) = self.range(id) else { return };
        if s + key >= e {
            return;
        }
        for &(f, v) in values {
            self.keys[s + key][f] = v;
        }
    }

    pub fn exists(&self, id: i32) -> bool {
        self.range(id).is_some()
    }

    /// Wrap elapsed time according to the motion's loop setting; None when finished.
    fn local_time(&self, id: i32, t: f32) -> (f32, bool) {
        let len = self.length(id) as f32;
        if t <= len || len <= 0.0 {
            return (t.min(len.max(0.0)), t >= len);
        }
        let Some(&lp) = self.loops.get(&(id as u16)) else {
            return (len, true);
        };
        let lp = lp as f32;
        if lp <= 1.0 || lp >= len {
            (t % len, false)
        } else {
            ((t - lp) % (len - lp) + lp, false)
        }
    }

    /// sub_99950 / sub_961B0: motion 9999's clock is eased over its whole length by its first key's ease.
    fn eased_time(&self, id: i32, t: f32) -> f32 {
        let Some((s, e)) = self.range(id).filter(|_| id == EASED_WHOLE) else { return t };
        let total = self.keys[e - 1][field::TIME] as f32;
        let kind = self.keys[s][field::EASE];
        let kind = if kind == NO_KEY || kind == HOLD { 0 } else { kind };
        if total <= 0.0 { t } else { ease(kind, t / total) * total }
    }

    /// The motion's alpha (0..255) at time `t`, as `advance` would set it.
    pub fn alpha_at(&self, id: i32, t: f32, cur: f32) -> Option<f32> {
        let (t, _) = self.local_time(id, t);
        let t = self.eased_time(id, t);
        self.gated(id, t).then_some(())?;
        (self.mask_at(id, t) & gate::ALPHA != 0).then(|| self.sample(id, field::ALPHA, t, cur)).flatten()
    }

    fn gated(&self, id: i32, t: f32) -> bool {
        self.range(id).is_some_and(|(s, _)| t >= self.keys[s][field::TIME] as f32)
    }

    /// The motion's field mask at local time `t` (sub_97B80). A field whose only keys are at time 0 drives the
    /// layer only while the clock is on those keys (the first application, at STMA), so a later script change
    /// of x, y, alpha, zoom, blend or the image variant is not overwritten; the other fields keep their bits.
    fn mask_at(&self, id: i32, t: f32) -> u64 {
        let mut m = usize::try_from(id).ok().and_then(|i| self.masks.get(i)).copied().unwrap_or(0);
        let Some((s, e)) = self.range(id) else { return 0 };
        let keys = &self.keys[s..e];
        let time = |k: &[i32; FIELDS]| k[field::TIME];
        // the segment end key (first at or after t) is a time-0 key: every field still applies
        let at_start = keys.iter().find(|k| time(k) as f32 >= t).is_some_and(|k| time(k) == 0);
        let present = |f: usize| at_start || keys.iter().any(|k| time(k) != 0 && k[f] != NO_KEY && k[f] != NO_KEY_WIDE);
        if !present(field::X) {
            m &= !gate::X;
        }
        if !present(field::Y) {
            m &= !gate::Y;
        }
        if !present(field::ALPHA) {
            m &= !gate::ALPHA;
        }
        if !present(field::VARIANT_A) {
            m &= !gate::VARIANT_A;
        }
        if !present(field::VARIANT_B) {
            m &= !gate::VARIANT_B;
        }
        if !present(field::ZOOM_X) {
            m &= !gate::ZOOM_X;
        }
        if !present(field::ZOOM_Y) {
            m &= !gate::ZOOM_Y;
        }
        if !present(field::BLEND) {
            m &= !gate::BLEND;
        }
        m
    }

    /// Value of `f` at local time `t` (sub_97710); `cur` is the layer's current value in motion units, which
    /// the exe returns where the motion has nothing for the field and uses for undefined spline knots. None
    /// past the motion's last key. The segment ends at the first key at or after `t` (its value skipping HOLD
    /// keys forward) and starts at the key before it (skipping HOLD back, its value from an earlier defining
    /// key). Ease comes from the nearest earlier key that is not HOLD for `f` and defines one (0 for motion
    /// 9999); fields 15 and 17.. step, interp 2 steps, 1 is a spline (sub_972C0), else linear.
    pub fn sample(&self, id: i32, f: usize, t: f32, cur: f32) -> Option<f32> {
        let (a2, e) = self.range(id)?;
        let a3 = e - 1;
        let k = |i: usize, g: usize| self.keys[i][g];
        let time = |i: usize| k(i, field::TIME) as f32;
        let val = |i: usize| k(i, f) as f32;
        let (none, hold) = sentinels(f);
        let (none, hold) = (none as f32, hold as f32);
        let sentinel = |v: i32| v == NO_KEY || v == HOLD;
        if time(a2) > t {
            return Some(cur);
        }
        let a4 = (a2..e).find(|&i| time(i) >= t)?;
        // segment end: a4, skipping HOLD keys forward
        let mut v20 = a4;
        let (mut v7, mut v24);
        loop {
            let v21 = v20;
            v7 = val(v20);
            v24 = time(v20);
            v20 += 1;
            if !(v21 < a3 && v7 == hold) {
                break;
            }
        }
        let mut v8;
        let mut v9 = 0.0;
        if a4 == a2 || v24 <= t {
            loop {
                v20 -= 1;
                v8 = val(v20);
                if !((v8 == none || v8 == hold) && v20 > a2) {
                    break;
                }
            }
        } else {
            let mut v25 = a4;
            loop {
                v25 -= 1;
                v8 = val(v25);
                v9 = time(v25);
                if !(v25 > a2 && v8 == hold) {
                    break;
                }
            }
            if v7 != none && v8 == none && v25 > a2 {
                let mut v28 = v25 - 1;
                loop {
                    v8 = val(v28);
                    if (v8 != hold && v8 != none) || v28 <= a2 {
                        break;
                    }
                    v28 -= 1;
                }
            }
        }
        if v7 == none {
            // no key ahead sets f: the last value at or before a4
            let mut i = a4;
            loop {
                v7 = val(i);
                if (v7 != none && v7 != hold) || i <= a2 {
                    break;
                }
                i -= 1;
            }
            if v7 == none || v7 == hold {
                return Some(cur);
            }
            return Some(match f {
                field::X => v7 * 640.0 / 1280.0 * 1280.0 / 640.0,
                field::Y => v7 * 480.0 / 720.0 * 720.0 / 480.0,
                _ => v7,
            });
        }
        if v8 == none {
            return Some(cur);
        }
        if v7 == hold {
            // HOLD to the end: the last value that is not HOLD
            return Some((a2..a3).rev().map(val).find(|&v| v != hold).unwrap_or(cur));
        }
        if v8 == hold {
            return Some(cur);
        }
        let discrete = (17..28).contains(&f);
        let u = if v9 >= v24 {
            1.0
        } else {
            let kind = if id == EASED_WHOLE {
                0
            } else if a4 == a2 {
                k(a4, field::EASE)
            } else {
                (a2..a4)
                    .rev()
                    .filter(|&i| val(i) != hold)
                    .map(|i| k(i, field::EASE))
                    .find(|&v| !sentinel(v))
                    .unwrap_or(NO_KEY)
            };
            ease(if sentinel(kind) { 0 } else { kind }, (t - v9) / (v24 - v9))
        };
        if discrete || f == field::CLIP_MODE {
            return Some(v8);
        }
        let interp = if a4 == a2 {
            k(a4, field::INTERP)
        } else {
            (a2..a4).rev().map(|i| k(i, field::INTERP)).find(|&v| !sentinel(v)).unwrap_or(k(a2, field::INTERP))
        };
        if interp == 2 {
            return Some(v8);
        }
        if interp == 1
            && let Some((m0, m1)) = self.spline(f, a2, a3, a4, cur)
        {
            return Some(v8 + u * ((v7 - v8) - (m1 + (m0 + m0)) + u * (m0 * 3.0 + u * (m1 - m0))));
        }
        Some(v8 + (v7 - v8) * u)
    }

    /// sub_972C0: a natural cubic spline through the field's values over the whole motion, one knot per key
    /// that sets a value differing from a neighbour (HOLD and runs of equal values fold into the previous
    /// knot), knots evenly spaced. Returns the second-derivative terms (/ 6) at the ends of the knot segment
    /// holding key a4 - 1. The exe's forward sweep subtracts diag * (1 / diag) (about 1) where a Thomas
    /// solver would subtract the previous right-hand side over diag; that is kept.
    fn spline(&self, f: usize, a2: usize, a3: usize, a4: usize, a7: f32) -> Option<(f32, f32)> {
        if a2 == a4 || a3 - a2 < 2 {
            return None;
        }
        let (none, hold) = sentinels(f);
        let v = |i: usize| self.keys[i][f];
        let knot = |x: i32| if x == hold || x == none { a7 } else { x as f32 };
        let mut p = vec![knot(v(a2))];
        let mut map = vec![0usize; a3 - a2 + 1];
        for i in a2 + 1..a3 {
            let (vp, vi, vn) = (v(i - 1), v(i), v(i + 1));
            if vi != hold && (vp != vi || vi != vn) {
                let mut j = i;
                while (v(j) == hold || v(j) == none) && j > a2 {
                    j -= 1;
                }
                p.push(knot(v(j)));
            }
            map[i - a2] = p.len() - 1;
        }
        let (vl, vz) = (v(a3 - 1), v(a3));
        if vl != vz && vz != none {
            let mut j = a3;
            while (v(j) == hold || v(j) == none) && j > a2 {
                j -= 1;
            }
            p.push(knot(v(j)));
            map[a3 - a2] = p.len() - 1;
        }
        let n = p.len();
        if n < 3 {
            return None;
        }
        // d[j] = p[j] - p[j - 1]; the diagonal overwrites it as the sweep goes
        let mut d = vec![0.0f32; n + 1];
        let mut s = vec![0.0f32; n + 1];
        for j in 1..n {
            d[j] = p[j] - p[j - 1];
        }
        let mut prev = d[2];
        s[1] = d[2] - d[1];
        d[1] = 4.0;
        let mut diag = 4.0f32;
        for j in 2..n - 1 {
            let inv = 1.0 / diag;
            let next = d[j + 1];
            s[j] = (next - prev) - diag * inv;
            diag = 4.0 - inv;
            d[j] = diag;
            prev = next;
        }
        let m = n - 2;
        s[m] -= s[m + 1];
        s[m] = (s[m] - s[m + 1]) / d[m];
        for j in (2..=m).rev() {
            s[j - 1] = (s[j - 1] - s[j]) / d[j - 1];
        }
        let i = map[a4 - 1 - a2];
        Some((s[i], s[i + 1]))
    }
}

/// The (no key, HOLD) values of a field: fields 0 and 8 are 32-bit with their own pair.
fn sentinels(f: usize) -> (i32, i32) {
    if f == field::TIME || f == field::ROT { (NO_KEY_WIDE, HOLD_WIDE) } else { (NO_KEY, HOLD) }
}

/// Playback state of a motion on a layer.
#[derive(Clone, Debug)]
pub struct MotionState {
    pub id: i32,
    pub t: f32,
    pub done: bool,
    /// SPMA(1): clock frozen (new motions start unpaused)
    pub paused: bool,
}

impl MotionState {
    pub fn new(id: i32, start: f32) -> Self {
        Self {
            id,
            t: start,
            done: false,
            paused: false,
        }
    }

    pub fn finished(&self) -> bool {
        self.done
    }
}

/// Sampled motion values for one frame.
#[derive(Default, Debug)]
pub struct MotionFrame {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub alpha: Option<f32>,
    pub rot: Option<f32>,
    pub zoom_x: Option<f32>,
    pub zoom_y: Option<f32>,
    pub prio: Option<i32>,
    /// clip rect x, y, w, h (w / h rounded down to even)
    pub clip: [Option<f32>; 4],
    /// clip inset mode (STCH arg 17) and inset px (arg 16)
    pub clip_mode: Option<i32>,
    pub clip_inset: Option<i32>,
    /// pivot (sprite floats 25 / 26) in script units, before the image level scale
    pub pivot: [Option<f32>; 2],
    /// clip rotation (STRR, 1/100 degree)
    pub rot2: Option<i32>,
    pub blend: Option<i32>,
    /// field 18 and the colour of fields 19..21 (0xRRGGBB)
    pub fill: Option<i32>,
    pub fill_color: u32,
    /// colour set without field 18 (gate bit 33)
    pub color: Option<u32>,
    pub variant: [Option<i32>; 2],
    pub snx2: Option<f32>,
    pub snx3: Option<f32>,
    pub grain: Option<i32>,
}

impl MotionTable {
    /// One frame of sub_97B80. `cur` holds the layer's current value of each field in motion units (the
    /// fallback sub_97710 gets; rot2 falls back to the rotation like the exe, colours to 0).
    pub fn advance(&self, st: &mut MotionState, ms: f32, cur: &[f32; FIELDS]) -> MotionFrame {
        if !st.paused {
            st.t += ms;
        }
        let (t, done) = self.local_time(st.id, st.t);
        st.done = done;
        let t = self.eased_time(st.id, t);
        if !self.gated(st.id, t) {
            return MotionFrame::default();
        }
        let m = self.mask_at(st.id, t);
        let s = |g: u64, f: usize| if m & g != 0 { self.sample(st.id, f, t, cur[f]) } else { None };
        let even = |v: f32| ((v as i32) & !1) as f32;
        let rgb = || {
            let c = |f| self.sample(st.id, f, t, 0.0).map_or(0, |v| v as i32 as u32 & 0xff);
            c(19) << 16 | c(20) << 8 | c(21)
        };
        MotionFrame {
            x: s(gate::X, field::X),
            y: s(gate::Y, field::Y),
            alpha: s(gate::ALPHA, field::ALPHA).map(|a| a.clamp(0.0, 255.0) / 255.0),
            rot: s(gate::ROT, field::ROT).map(|r| r / 100.0),
            zoom_x: s(gate::ZOOM_X, field::ZOOM_X).map(|z| z / 1000.0),
            zoom_y: s(gate::ZOOM_Y, field::ZOOM_Y).map(|z| z / 1000.0),
            prio: s(gate::PRIO, field::PRIO).map(|p| p as i32),
            clip: [
                s(gate::CLIP_X, 11),
                s(gate::CLIP_Y, 12),
                s(gate::CLIP_X, 13).map(even),
                s(gate::CLIP_Y, 14).map(even),
            ],
            clip_mode: s(gate::CLIP_MODE, field::CLIP_MODE).map(|v| v.max(0.0) as i32),
            clip_inset: s(gate::CLIP_INSET, field::CLIP_INSET).map(|v| v.max(0.0) as i32),
            pivot: [s(gate::PIVOT_X, field::PIVOT_X), s(gate::PIVOT_Y, field::PIVOT_Y)],
            rot2: s(gate::ROT2, field::ROT).map(|r| r as i32),
            blend: s(gate::BLEND, field::BLEND).map(|v| v as i32),
            fill: s(gate::FILL, field::FILL).map(|v| v as i32),
            fill_color: rgb(),
            color: (m & gate::COLOR != 0).then(rgb),
            variant: [
                s(gate::VARIANT_A, field::VARIANT_A).map(|v| v as i32),
                s(gate::VARIANT_B, field::VARIANT_B).map(|v| v as i32),
            ],
            snx2: s(gate::SNX2, field::SNX2).map(|v| v / 255.0 + 1.0),
            snx3: s(gate::SNX3, field::SNX3).map(|v| v * 0.01 + 1.0),
            grain: s(gate::GRAIN, field::GRAIN).map(|v| v as i32),
        }
    }
}

/// sub_961B0's bounce-out curve on `x` (0..1).
fn bounce(x: f64) -> f64 {
    if x < 0.363636364 {
        x * x * 7.5625
    } else if x < 0.727272727 {
        let x = x - 0.545454545;
        x * 7.5625 * x + 0.75
    } else if x < 0.909090909 {
        let x = x - 0.818181818;
        x * 7.5625 * x + 0.9375
    } else {
        let x = x - 0.954545455;
        x * 7.5625 * x + 0.984375
    }
}

/// sub_96000: elastic out-in (kind 33).
#[allow(clippy::approx_constant)]
fn elastic_out_in(t: f32, b: f32, c: f32, d: f32) -> f32 {
    let tt = t + t;
    let h = c * 0.5;
    if d * 0.5 <= t {
        let b = h + b;
        let u = tt - d;
        if u == 0.0 {
            return b;
        }
        let p = u / d;
        if p == 1.0 {
            return h + b;
        }
        let s = d * 0.3;
        let q = p - 1.0;
        (b as f64 - (q as f64 * 10.0).exp2() * h as f64 * (((q * d + s * -0.25) as f64) * 6.28318531 / s as f64).sin())
            as f32
    } else {
        if tt == 0.0 {
            return b;
        }
        let p = tt / d;
        if p == 1.0 {
            return h + b;
        }
        let s = d * 0.3;
        ((p as f64 * -10.0).exp2() * h as f64 * (((p * d + s * -0.25) as f64) * 6.28318531 / s as f64).sin()
            + h as f64
            + b as f64) as f32
    }
}

/// The engine's easing (`sub_961B0`, copied as sub_30C80): Penner's (t, b, c, d) forms with the exe's own
/// constants. Kinds 2..=41 are quad, cubic, quart, quint, sine, circ, expo, elastic, back and bounce, each as
/// in / out / in-out / out-in; anything else is linear. Ported literally: the layer tweens call it with
/// d = -1, where the in-out halves swap.
#[allow(clippy::approx_constant, clippy::excessive_precision)]
pub fn penner(kind: i32, t: f32, b: f32, c: f32, d: f32) -> f32 {
    let (bd, cd) = (b as f64, c as f64);
    let r = t / d;
    let h = c * 0.5;
    match kind {
        2 => r * (r * c) + b,
        3 => b - r * c * (r - 2.0),
        4 => {
            let x = t / (d * 0.5);
            if x < 1.0 {
                (c as f64 * 0.5 * (x * x) as f64 + bd) as f32
            } else {
                let y = (x - 1.0) * ((x - 1.0) - 2.0);
                (cd * -0.5 * (y - 1.0) as f64 + bd) as f32
            }
        }
        5 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let u = (tt - d) / d;
                u * (u * h) + (h + b)
            } else {
                b + (-h * (tt / d)) * ((tt / d) - 2.0)
            }
        }
        6 => (r as f64 * r as f64 * (r * c) as f64 + bd) as f32,
        7 => (((r - 1.0) * (r - 1.0) * (r - 1.0) + 1.0) as f64 * cd + bd) as f32,
        8 => {
            let x = t / (d * 0.5);
            if x < 1.0 {
                (cd * 0.5 * (x as f64).powf(3.0) + bd) as f32
            } else {
                (cd * 0.5 * ((x - 2.0) * (x - 2.0) * (x - 2.0) + 2.0) as f64 + bd) as f32
            }
        }
        9 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let u = (tt - d) / d;
                (u as f64 * u as f64 * (u * h) as f64 + (h + b) as f64) as f32
            } else {
                let u = tt / d - 1.0;
                ((u * u * u + 1.0) as f64 * h as f64 + bd) as f32
            }
        }
        10 => ((r as f64).powf(3.0) * (r * c) as f64 + bd) as f32,
        11 => (bd - (((r - 1.0) as f64).powf(3.0) * (r - 1.0) as f64 - 1.0) * cd) as f32,
        12 => {
            let x = t / (d * 0.5);
            if x < 1.0 {
                (cd * 0.5 * (x as f64).powf(4.0) + bd) as f32
            } else {
                (cd * -0.5 * (((x - 2.0) as f64).powf(3.0) * (x - 2.0) as f64 - 2.0) + bd) as f32
            }
        }
        13 => {
            let tt = t + t;
            if d * 0.5 > t {
                let u = tt / d - 1.0;
                (((u as f64).powf(3.0) * u as f64 - 1.0) * (-h) as f64 + bd) as f32
            } else {
                let u = (tt - d) / d;
                ((u as f64).powf(3.0) * (u * h) as f64 + (h + b) as f64) as f32
            }
        }
        14 => ((r as f64).powf(4.0) * (r * c) as f64 + bd) as f32,
        15 => ((((r - 1.0) as f64).powf(4.0) * (r - 1.0) as f64 + 1.0) * cd + bd) as f32,
        16 => {
            let x = t / (d * 0.5);
            if x >= 1.0 {
                (cd * 0.5 * (((x - 2.0) as f64).powf(4.0) * (x - 2.0) as f64 + 2.0) + bd) as f32
            } else {
                (cd * 0.5 * (x as f64).powf(5.0) + bd) as f32
            }
        }
        17 => {
            let tt = t + t;
            if d * 0.5 > t {
                let u = tt / d - 1.0;
                (((u as f64).powf(4.0) * u as f64 + 1.0) * h as f64 + bd) as f32
            } else {
                let u = (tt - d) / d;
                ((u as f64).powf(4.0) * (u * h) as f64 + (h + b) as f64) as f32
            }
        }
        18 => (cd - (r as f64 * 1.57079633).cos() * cd + bd) as f32,
        19 => ((r as f64 * 1.57079633).sin() * cd + bd) as f32,
        20 => (bd + cd * -0.5 * ((t as f64 * 3.14159265 / d as f64).cos() - 1.0)) as f32,
        21 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let u = (tt - d) / d;
                (h as f64 - (u as f64 * 1.57079633).cos() * h as f64 + (h + b) as f64) as f32
            } else {
                ((tt / d) as f64 * 1.57079633).sin() as f32 * h + b
            }
        }
        22 => b - ((1.0 - r * r).sqrt() - 1.0) * c,
        23 => (1.0 - (r - 1.0) * (r - 1.0)).sqrt() * c + b,
        24 => {
            let x = t / (d * 0.5);
            if x >= 1.0 {
                (cd * 0.5 * ((1.0 - (x - 2.0) * (x - 2.0)).sqrt() + 1.0) as f64 + bd) as f32
            } else {
                (cd * -0.5 * ((1.0 - x as f64 * x as f64).sqrt() - 1.0) + bd) as f32
            }
        }
        25 => {
            let tt = t + t;
            if d * 0.5 > t {
                let u = tt / d - 1.0;
                h * (1.0 - u * u).sqrt() + b
            } else {
                let u = (tt - d) / d;
                (h + b) - ((1.0 - u * u).sqrt() - 1.0) * h
            }
        }
        26 => {
            if t == 0.0 {
                b
            } else {
                (((r - 1.0) * 10.0) as f64).exp2() as f32 * c + b + c * -0.001
            }
        }
        27 => {
            if t == d {
                b + c
            } else {
                (cd * 1.001 * (1.0 - (((t * -10.0) / d) as f64).exp2())) as f32 + b
            }
        }
        28 => {
            if t == 0.0 {
                return b;
            }
            if t == d {
                return b + c;
            }
            let x = t / (d * 0.5);
            if x < 1.0 {
                (cd * 0.5 * (((x - 1.0) * 10.0) as f64).exp2() + bd + cd * -0.0005) as f32
            } else {
                (cd * 0.5 * 1.0005 * (2.0 - (((x - 1.0) * -10.0) as f64).exp2()) + bd) as f32
            }
        }
        29 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let (u, b) = (tt - d, h + b);
                if u == 0.0 {
                    b
                } else {
                    ((((u / d) - 1.0) * 10.0) as f64).exp2() as f32 * h + b + h * -0.001
                }
            } else if tt == d {
                h + b
            } else {
                (h as f64 * 1.001 * (1.0 - (((tt * -10.0) / d) as f64).exp2())) as f32 + b
            }
        }
        30 => {
            if t == 0.0 {
                return b;
            }
            if r == 1.0 {
                return b + c;
            }
            let s = d * 0.3;
            let q = r - 1.0;
            (bd - ((q * 10.0) as f64).exp2() * cd * (((q * d + s * -0.25) as f64) * 6.28318531 / s as f64).sin())
                as f32
        }
        31 => {
            if t == 0.0 {
                return b;
            }
            if r == 1.0 {
                return b + c;
            }
            let s = d * 0.3;
            (((r * -10.0) as f64).exp2() * cd * (((r * d + s * -0.25) as f64) * 6.28318531 / s as f64).sin()
                + cd
                + bd) as f32
        }
        32 => {
            if t == 0.0 {
                return b;
            }
            let x = t / (d * 0.5);
            if x == 2.0 {
                return b + c;
            }
            let s = d * 0.45;
            let q = s * 0.25;
            let u = x - 1.0;
            let w = (((u * d - q) as f64) * 6.28318531 / s as f64).sin();
            if x >= 1.0 {
                (((u * -10.0) as f64).exp2() * cd * w * 0.5 + cd + bd) as f32
            } else {
                (bd + ((u * 10.0) as f64).exp2() * cd * w * -0.5) as f32
            }
        }
        33 => elastic_out_in(t, b, c, d),
        34 => r * (r * c) * (r * 2.7016 - 1.7016) + b,
        35 => ((r - 1.0) * (r - 1.0) * ((r - 1.0) * 2.7016 + 1.7016) + 1.0) * c + b,
        36 => {
            let x = t / (d * 0.5);
            if x < 1.0 {
                (cd * 0.5 * (x as f64 * x as f64) * (x * 3.5949 - 2.5949) as f64 + bd) as f32
            } else {
                let y = x - 2.0;
                (cd * 0.5 * (y * y * (y * 3.5949 + 2.5949) + 2.0) as f64 + bd) as f32
            }
        }
        37 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let u = (tt - d) / d;
                u * (u * h) * (u * 2.7016 - 1.7016) + (h + b)
            } else {
                let u = tt / d - 1.0;
                h * (u * u * (u * 2.7016 + 1.7016) + 1.0) + b
            }
        }
        38 => {
            let v = (bounce(((d - t) / d) as f64) * cd + 0.0) as f32;
            c - v + b
        }
        39 => (bounce(r as f64) * cd + bd) as f32,
        40 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let v = (bounce(((tt - d) / d) as f64) * cd + 0.0) as f32;
                (cd * 0.5 + v as f64 * 0.5 + bd) as f32
            } else {
                let v = (bounce(((d - tt) / d) as f64) * cd + 0.0) as f32;
                (((c - v) + 0.0) as f64 * 0.5 + bd) as f32
            }
        }
        41 => {
            let tt = t + t;
            if d * 0.5 <= t {
                let v = (bounce(((d - (tt - d)) / d) as f64) * h as f64 + 0.0) as f32;
                (h - v) + (h + b)
            } else {
                (bounce((tt / d) as f64) * h as f64 + bd) as f32
            }
        }
        _ => (t * c) / d + b,
    }
}

/// `penner` over 0..1 (the motion clocks).
pub fn ease(kind: i32, p: f32) -> f32 {
    penner(kind, p.clamp(0.0, 1.0), 0.0, 1.0, 1.0)
}
