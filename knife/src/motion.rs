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

pub const NO_KEY: i32 = -32767;
pub const HOLD: i32 = -32766;
const NO_KEY_WIDE: i32 = -100000;

/// Field numbers (as used by the exe's accessor `sub_99FF0`).
pub mod field {
    pub const TIME: usize = 0;
    pub const EASE: usize = 1;
    pub const INTERP: usize = 2;
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const ALPHA: usize = 5;
    pub const ROT: usize = 8;
    pub const ZOOM_X: usize = 9;
    pub const ZOOM_Y: usize = 10;
    pub const BLEND: usize = 17;
    pub const VARIANT_A: usize = 22;
    pub const VARIANT_B: usize = 23;
    pub const PRIO: usize = 27;
}

pub const FIELDS: usize = 28;

pub struct MotionTable {
    keys: Vec<[i32; FIELDS]>,
    ends: Vec<u32>,
    lens: Vec<u32>,
    loops: HashMap<u16, u32>,
}

fn rd_u32(nso: &Nso, va: u64) -> Option<u32> {
    Some(u32::from_le_bytes(nso.bytes(va, 4)?.try_into().ok()?))
}

impl MotionTable {
    pub fn load(nso: &Nso) -> Result<Self> {
        let raw = nso.bytes(KEYS_VA, KEY_COUNT * 64).context("motion keys out of range")?;
        let keys = raw
            .chunks_exact(64)
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
        Ok(Self {
            keys,
            ends,
            lens,
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

    /// The motion's alpha (0..255) at time `t`, as `advance` would set it.
    pub fn alpha_at(&self, id: i32, t: f32) -> Option<f32> {
        let (t, _) = self.local_time(id, t);
        self.sample(id, field::ALPHA, t)
    }

    pub fn loops(&self, id: i32) -> bool {
        self.loops.contains_key(&(id as u16))
    }

    /// Value of `f` at local time `t`, or None when the motion leaves it alone (sub_97710). The segment is
    /// bounded by keys of any field: it ends at the first key at or after `t` (skipping HOLD values of `f`)
    /// and starts at the time of the key before that, taking the value of `f` from there or, when that key
    /// does not set `f`, from the last earlier key that does. Ease comes from the nearest earlier key that is
    /// not HOLD for `f` and defines one; interpolation (2 = step, 1 = spline, else linear) from the nearest
    /// earlier key that defines it. A segment ending in a key without `f` holds the last value.
    /// Splines (72 keys in the table, sub_972C0) are drawn linear here.
    pub fn sample(&self, id: i32, f: usize, t: f32) -> Option<f32> {
        let (s, e) = self.range(id)?;
        let keys = &self.keys[s..e];
        let time = |i: usize| keys[i][field::TIME] as f32;
        let no = |v: i32| v == NO_KEY || (f == field::ROT && v <= NO_KEY_WIDE);
        let hold = |v: i32| v == HOLD;
        let defined = |v: i32| !no(v) && !hold(v);
        let last = keys.len() - 1;
        if t < time(0) {
            return None;
        }
        // first key at or after t; past the end the exe samples nothing
        let a4 = (0..keys.len()).find(|&i| time(i) >= t)?;
        // segment end value, skipping HOLD keys forward
        let mut j = a4;
        while j < last && hold(keys[j][f]) {
            j += 1;
        }
        let target = keys[j][f];
        if !defined(target) {
            // no further key sets f: keep the last value set at or before a4
            return (0..=a4).rev().map(|i| keys[i][f]).find(|&v| defined(v)).map(|v| v as f32);
        }
        if a4 == 0 || time(j) <= t {
            return Some(target as f32);
        }
        // segment start: the key before a4 (skipping HOLD); its time, and its value or an earlier one
        let mut k = a4 - 1;
        while k > 0 && hold(keys[k][f]) {
            k -= 1;
        }
        let from_t = time(k);
        let from = (0..=k).rev().map(|i| keys[i][f]).find(|&v| !no(v))?;
        if hold(from) {
            return None;
        }
        // discrete fields step
        if f >= 17 {
            return Some(from as f32);
        }
        let (va, vb) = (from as f32, target as f32);
        let tb = time(j);
        let p = if from_t >= tb {
            1.0
        } else {
            let ease_kind = (0..a4)
                .rev()
                .filter(|&i| !hold(keys[i][f]))
                .map(|i| keys[i][field::EASE])
                .find(|&v| v != NO_KEY && v != HOLD)
                .unwrap_or(0);
            ease(ease_kind, (t - from_t) / (tb - from_t))
        };
        let interp = (0..a4)
            .rev()
            .map(|i| keys[i][field::INTERP])
            .find(|&v| v != NO_KEY && v != HOLD)
            .unwrap_or(0);
        if interp == 2 && f != 15 {
            return Some(va);
        }
        Some(va + (vb - va) * p)
    }
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
    /// clip rect x, y, w, h, rotation (0.1 deg)
    pub clip: [Option<f32>; 5],
}

impl MotionTable {
    pub fn advance(&self, st: &mut MotionState, ms: f32) -> MotionFrame {
        if !st.paused {
            st.t += ms;
        }
        let (t, done) = self.local_time(st.id, st.t);
        st.done = done;
        let s = |f| self.sample(st.id, f, t);
        MotionFrame {
            x: s(field::X),
            y: s(field::Y),
            alpha: s(field::ALPHA).map(|a| a / 255.0),
            rot: s(field::ROT).map(|r| r / 100.0),
            zoom_x: s(field::ZOOM_X).map(|z| z / 1000.0),
            zoom_y: s(field::ZOOM_Y).map(|z| z / 1000.0),
            prio: s(field::PRIO).map(|p| p as i32),
            clip: [s(11), s(12), s(13), s(14), s(16)],
        }
    }
}

fn bounce_out(p: f32) -> f32 {
    if p < 1.0 / 2.75 {
        7.5625 * p * p
    } else if p < 2.0 / 2.75 {
        let p = p - 1.5 / 2.75;
        7.5625 * p * p + 0.75
    } else if p < 2.5 / 2.75 {
        let p = p - 2.25 / 2.75;
        7.5625 * p * p + 0.9375
    } else {
        let p = p - 2.625 / 2.75;
        7.5625 * p * p + 0.984375
    }
}

/// The engine's easing curves (`sub_961B0`): kinds 2..=41 are Penner families
/// (quad, cubic, quart, quint, sine, circ, expo, elastic, back, bounce) each as
/// in / out / in-out / out-in. Anything else is linear.
pub fn ease(kind: i32, p: f32) -> f32 {
    use std::f32::consts::PI;
    let p = p.clamp(0.0, 1.0);
    if !(2..=41).contains(&kind) {
        return p;
    }
    let family = (kind - 2) / 4;
    let variant = (kind - 2) % 4;
    let ein = |x: f32| -> f32 {
        match family {
            0 => x * x,
            1 => x * x * x,
            2 => x.powi(4),
            3 => x.powi(5),
            4 => 1.0 - (x * PI / 2.0).cos(),
            5 => 1.0 - (1.0 - x * x).max(0.0).sqrt(),
            6 => {
                if x <= 0.0 {
                    0.0
                } else {
                    2f32.powf(10.0 * (x - 1.0))
                }
            }
            7 => {
                if x <= 0.0 || x >= 1.0 {
                    x
                } else {
                    let per = 0.3;
                    -(2f32.powf(10.0 * (x - 1.0))) * (((x - 1.0) - per / 4.0) * 2.0 * PI / per).sin()
                }
            }
            8 => x * x * (2.7016 * x - 1.7016),
            _ => 1.0 - bounce_out(1.0 - x),
        }
    };
    let eout = |x: f32| 1.0 - ein(1.0 - x);
    match variant {
        0 => ein(p),
        1 => eout(p),
        2 => {
            if p < 0.5 {
                ein(p * 2.0) / 2.0
            } else {
                0.5 + eout(p * 2.0 - 1.0) / 2.0
            }
        }
        _ => {
            if p < 0.5 {
                eout(p * 2.0) / 2.0
            } else {
                0.5 + ein(p * 2.0 - 1.0) / 2.0
            }
        }
    }
}
