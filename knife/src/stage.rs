//! Script-visible layers ("stage channels") and their animations.

use std::sync::Arc;

use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Shade, SoftClip, TexRef};

use crate::{assets::Image, motion::MotionState};

pub const LAYERS: usize = 40;

/// Script coordinate space (the screen is 480x272 with (240,136) at its centre).
pub const SCRIPT_W: f32 = 480.0;
pub const SCRIPT_H: f32 = 272.0;
/// Image metric space (640x362 units for a full screen).
pub const UNIT_W: f32 = 640.0;
pub const UNIT_H: f32 = 362.0;

pub fn script_to_px(x: f32, y: f32) -> (f32, f32) {
    (x / SCRIPT_W * CANVAS_W as f32, y / SCRIPT_H * CANVAS_H as f32)
}

/// A layer tween as the exe keeps it (sub_31F30 zoom, sub_32630 position, sub_326E0 rotation, sub_32820
/// alpha): the target, `from - to`, a progress word starting at 4096 and stepped once per 60 Hz frame by
/// int(-4096 / frames) (sub_3C970; it ends at or below 0), and the ease kind. The value is
/// `to + diff * (1 - sub_30C80(kind, prog / 4096 - 1, 0, 1, -1))`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tween {
    pub to: f32,
    pub diff: f32,
    pub prog: i16,
    pub step: i16,
    pub ease: i32,
}

impl Tween {
    /// A tween over `frames` 60 Hz frames (60 ms / 1000 in the commands); under 1 frame jumps to `to`.
    pub fn new(from: f32, to: f32, frames: i32, ease: i32) -> Self {
        let (prog, step) = if frames >= 1 { (4096, (-4096.0 / frames as f32) as i32 as i16) } else { (0, 0) };
        Self {
            to,
            diff: if frames != 0 { from - to } else { 0.0 },
            prog,
            step,
            ease,
        }
    }
    pub fn fixed(v: f32) -> Self {
        Self::new(v, v, 0, 0)
    }
    /// prog / 4096 (the exe multiplies by 0.00024414)
    fn frac(&self) -> f32 {
        self.prog as f32 * 0.00024414
    }
    /// 1 - ease, 0 once finished
    fn left(&self) -> f32 {
        let f = self.frac();
        if f > 0.0 { 1.0 - crate::motion::penner(self.ease, f - 1.0, 0.0, 1.0, -1.0) } else { 0.0 }
    }
    pub fn value(&self) -> f32 {
        self.to + self.diff * self.left()
    }
    /// sub_3BBA0's x: kind 16 uses the raw progress instead of the curve (y keeps the curve).
    pub fn value_x(&self) -> f32 {
        let k = if self.ease == 16 { self.frac() } else { self.left() };
        self.to + self.diff * k
    }
    /// The rotation (degrees): sub_3BBA0 truncates `diff * (1 - ease)` to whole 1/100 degrees.
    pub fn value_rot(&self) -> f32 {
        self.to + ((self.diff * 100.0).round() as i16 as f32 * self.left()) as i32 as i16 as f32 / 100.0
    }
    pub fn done(&self) -> bool {
        self.prog <= 0
    }
    /// One frame of sub_3C970.
    pub fn step(&mut self) {
        if self.prog >= 1 {
            let n = (self.step as f32 + self.prog as f32) as i32;
            if self.step < 0 && n >= 1 {
                self.prog = n as i16;
            } else {
                (self.prog, self.step) = (0, 0);
            }
        }
    }
    pub fn finish(&mut self) {
        (self.prog, self.step) = (0, 0);
    }
}

#[derive(Clone)]
pub enum Source {
    Image(Arc<Image>),
    Color([f32; 3]),
}

#[derive(Clone)]
pub struct Layer {
    pub name: String,
    pub source: Source,
    pub x: Tween,
    pub y: Tween,
    pub zoom_x: Tween,
    pub zoom_y: Tween,
    pub alpha: Tween,
    pub rotation: Tween,
    pub prio: i32,
    pub parent: Option<usize>,
    /// Created since the last FADS; not drawn until committed.
    pub pending: bool,
    pub blend: Blend,
    /// STBR mode as given (-1 none); `blend` is what it renders as
    pub stbr: i32,
    pub shade: Shade,
    /// STEC colour fill (silhouette), rgb
    pub fill: Option<[f32; 3]>,
    /// STEC colour grade and SNX2 / SNX3 tone the image was built with (`grade::Bake`)
    pub bake: crate::grade::Bake,
    /// STEC mode and colour as the stage holds them (word 2562 + layer, colour at 5850). `fill` and `bake`
    /// are what the texture was built with: the exe rebuilds it only when the committed image changes
    /// (sub_3C970 -> sub_3ECC0), so an STEC on a shown layer only takes effect at its next load.
    pub stec_mode: i32,
    pub stec_color: [f32; 3],
    /// Replaces a shown layer with the same image: its texture is kept as built
    pub keeps_texture: bool,
    pub motion: Option<MotionState>,
    pub motion_alpha: f32,
    pub motion_zoom: (f32, f32),
    /// STRR / STCH arg 19 / motion field 8 (stage word 1522, render word 164), 1/100 degree wrapped to
    /// -18000..18000; -32768 (STRR -99999) follows the layer's rotation. A layer's children inherit it and
    /// are drawn through an intermediate that sub_36520 composites rotated by it about the clip centre.
    pub rot2: i16,
    /// STCH args 16 / 17 (render +296 / +298, motion fields 16 / 15): with a mode of 1..6 the clip scissor is
    /// inset by this many 1920x1080 pixels on each side whose extent is under the screen size
    pub clip_inset: i16,
    pub clip_mode: i8,
    /// SOXY args 3 / 4 (stage words 1222 / 1262 -> render +284 / +286): sub_37F50 adds (a * 0.33333, b *
    /// 0.41481) to the drawn position in the 640x448 draw space, i.e. (a, b) 1920x1080 pixels
    pub soxy: (i16, i16),
    /// Pivot (render floats 25 / 26, 640x362 units; None = the image centre): STPV and motion fields 6 / 7.
    /// Zoom and rotation turn about it, and SBAS children anchor to it.
    pub pivot: Option<[f32; 2]>,
    /// Quad offset (render floats 18 / 19; None = minus the image centre). The image is drawn shifted by
    /// pivot + shift: STPV moves only the pivot, motion fields 6 / 7 set shift = -pivot as well.
    pub shift: Option<[f32; 2]>,
    /// quake slots 0 (SQK2) and 1 (SHK2 / SSW2)
    pub quake: [Quake; 2],
    /// SNX2 brightness (1 + v / 255) and SNX3 contrast (1 + v / 100) as the stage holds them (floats
    /// 1301 / 1302 + 4 layer); like the STEC mode they reach the texture only when it is built (`bake`)
    pub snx: [f32; 2],
    /// STRA haze state (render words +284..+288)
    pub haze: Haze,
    /// clip window (STCH args 12..16); children are placed relative to its top-left
    pub clip: Option<ClipRect>,
    /// SBAS transform base: position is in the base's local image space and
    /// inherits its zoom and rotation
    pub base: Option<usize>,
    /// SACL: placed in absolute coordinates and not clipped despite having a parent
    pub absolute: bool,
    /// STTR: alpha fades of this layer wipe through a rule image (number, texture)
    pub rule: Option<(i32, TexRef)>,
    /// STTR arg 2 (b, stage word bits 12..14): 1 mirrors the rule in x, 2 in y, 3 both
    pub rule_flip: u8,
    /// STNS: film-grain value; the layer draws the NOISE_PARTS texture instead of its image
    pub noise: Option<i32>,
}

/// STRA haze state of a layer (render words +284..+288, sub_645F0), stepped per frame by sub_3C970
/// and drawn through nexl_nxshader_haze with the uniforms of sub_362A0.
#[derive(Clone, Copy, Debug, Default)]
pub struct Haze {
    /// table id (sub_91520); 0 = off
    pub id: i32,
    /// frames since the effect started, wrapping at 31416
    pub phase: i32,
    /// fade-in length in frames; cleared once `phase` reaches it
    pub fade_in: i32,
    /// fade-out length and frames left
    pub fade_total: i32,
    pub fade_left: i32,
    /// table entry of `id` (sub_91520)
    pub entry: Option<[f32; 10]>,
    /// sub-frame remainder of the elapsed time
    pub acc: f32,
}

impl Haze {
    /// STRA(layer, id, ms) on one layer; `entry` is the table entry of `id`.
    pub fn set(&mut self, id: i32, ms: i32, entry: Option<[f32; 10]>) {
        let frames = 60 * ms / 1000;
        if ms == 0 {
            self.id = id;
            self.entry = entry;
            self.phase = 0;
            if id != 0 {
                self.fade_in = 0;
            } else {
                self.fade_total = 0;
                self.fade_left = 0;
            }
        } else if id != 0 {
            self.fade_in = frames;
        } else {
            self.fade_total = frames;
            self.fade_left = frames;
        }
    }

    fn advance(&mut self, ms: f32) {
        self.acc += ms * 0.06;
        let d = self.acc.floor();
        self.acc -= d;
        let d = d as i32;
        if d == 0 {
            return;
        }
        if self.id <= 0 {
            self.phase = 0;
            return;
        }
        self.phase = (self.phase + d) % 31416;
        if self.fade_in >= 1 && self.phase >= self.fade_in {
            self.fade_in = 0;
        }
        if self.fade_total >= 1 {
            self.fade_left -= d;
            if self.fade_left <= 0 {
                self.id = 0;
                self.entry = None;
                self.fade_total = 0;
                self.fade_left = 0;
            }
        }
    }

    /// Shader parameters (sub_362A0) for a `w`x`h` texel texture whose logical image spans
    /// `uvm` of the texture coordinates; `None` when the id has no table entry.
    pub fn params(&self, w: f32, h: f32, uvm: [f32; 2]) -> Option<[[f32; 4]; 2]> {
        let e = self.entry.as_ref()?;
        let t = self.phase as f32 / 60.0;
        let amp = if self.fade_in != 0 {
            let v = (1000 * self.phase / 60).min(e[9] as i32);
            let a = v as f32 * 1000.0 / e[9];
            // float -> int conversion saturates and turns NaN into 0, like the arm64 fcvtzs
            a as i32
        } else if self.fade_total >= 1 {
            (self.fade_left as f32 * 1000.0 / self.fade_total as f32) as i32
        } else {
            1000
        };
        let v26 = CANVAS_W as f32 * 0.00097656 * 0.001 * amp as f32;
        let ax = v26 * e[6] * e[2];
        let ay = v26 * e[6] * e[5];
        let kx = (e[0] + e[7] * 0.1) * (h / CANVAS_H as f32) * 0.5;
        let ky = (e[3] + e[7] * 0.1) * (w / CANVAS_W as f32) * 0.5;
        let px = (e[8] * 0.33333 * (t + e[1])).fract();
        let py = (e[8] * 0.33333 * (t + e[4])).fract();
        // the exe's uv spans the logical image; here it spans `uvm` of a padded texture. hx.w = 1 marks the
        // haze path (sub_362A0 returned 1: shader 38 into the intermediate, see `sample_img` in shader.wgsl)
        Some([
            [if w > 0.0 { ax / w * uvm[0] } else { 0.0 }, kx / uvm[1], px, 1.0],
            [if h > 0.0 { ay / h * uvm[1] } else { 0.0 }, ky / uvm[1], py, 0.0],
        ])
    }
}

#[derive(Default)]
struct Resolved {
    x: f32,
    y: f32,
    alpha: f32,
    clip: Option<ClipRect>,
    zoom: (f32, f32),
    rot: f32,
    /// rotation of an inherited clip (the parent's STRR) the layer is composited with, degrees
    spin: f32,
}

/// Rotated clip rectangle in 1920x1080 canvas pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClipRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// degrees (an inherited STRR rotation)
    pub rot: f32,
    /// scissor inset (px) and its mode (applied for 1..6, unrotated clips only)
    pub inset: i16,
    pub mode: i8,
}

impl ClipRect {
    /// sub_36520: an unrotated scissor with a mode of 1..6 shrinks by the inset on each side whose extent is
    /// under the screen size.
    fn inset_rect(&self) -> Self {
        let mut c = *self;
        if c.rot == 0.0 && (1..=6).contains(&c.mode) {
            let d = f32::from(c.inset);
            if c.w < CANVAS_W as f32 {
                c.x += d;
                c.w -= 2.0 * d;
            }
            if c.h < CANVAS_H as f32 {
                c.y += d;
                c.h -= 2.0 * d;
            }
        }
        c
    }

    /// sub_32E30: with an inset, a clip of mode 0 or 7.. is masked by a rounded rectangle (drawn into render
    /// target 1 from BORDER_PARTS) whose corners and edges fade out over the radius: (inset + odd) / 2 in
    /// 960x540 units (twice that for mode 7, with the sharper `_2` textures), at most half the clip's extent.
    /// Unrotated, a side reaching past the screen (or a clip spanning the whole width / height) gets no fade.
    /// Modes 1..6 shrink the scissor by the inset instead, which cuts the faded band away.
    pub fn soft(&self) -> Option<SoftClip> {
        if self.inset == 0 || (1..=6).contains(&self.mode) {
            return None;
        }
        let half = f32::from((self.inset & 1) + self.inset) * 0.5;
        let r0 = if self.mode == 7 { half * 2.0 } else { half };
        let (l, t) = (self.x * 0.5, self.y * 0.5);
        let (rx, by) = ((self.x + self.w) * 0.5, (self.y + self.h) * 0.5);
        let r = r0.min((rx - l) * 0.5).min((by - t) * 0.5);
        if r <= 0.0 {
            return None;
        }
        let [mut rl, mut rr, mut rt, mut rb] = [r; 4];
        if self.rot == 0.0 {
            rl = if l + r <= 0.0 { 0.0 } else { r };
            rr = if rx - r >= 960.0 { 0.0 } else { r };
            rt = if t + r <= 0.0 { 0.0 } else { r };
            rb = if by - r >= 540.0 { 0.0 } else { r };
            if l == 0.0 && self.w * 0.5 == 960.0 {
                (rl, rr) = (0.0, 0.0);
            }
            if t == 0.0 && self.h * 0.5 == 540.0 {
                (rt, rb) = (0.0, 0.0);
            }
        }
        // texture by radius: alphagradation020 / 064 / 080 / 128 / 256 / 512 (dword_34D81C: their radii)
        const RADII: [u32; 6] = [10, 32, 40, 64, 128, 256];
        let k = RADII.iter().position(|&n| r < n as f32).unwrap_or(5);
        let row = if self.mode == 7 { k + 12 } else { k } as u32;
        let s = CANVAS_W as f32 / 960.0;
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        Some(SoftClip {
            inner: [(l + rl) * s - cx, (t + rt) * s - cy, (rx - rr) * s - cx, (by - rb) * s - cy],
            pivot: [cx, cy],
            rot: self.rot.to_radians(),
            radius: r * s,
            row,
            texels: RADII[k],
        })
    }

    pub fn corners(&self) -> [[f32; 2]; 4] {
        let c = self.inset_rect();
        let (cx, cy) = (c.x + c.w / 2.0, c.y + c.h / 2.0);
        let (s, cs) = self.rot.to_radians().sin_cos();
        let p = |dx: f32, dy: f32| [cx + dx * cs - dy * s, cy + dx * s + dy * cs];
        let (hw, hh) = (c.w / 2.0, c.h / 2.0);
        [p(-hw, -hh), p(hw, -hh), p(hw, hh), p(-hw, hh)]
    }
}

/// sub_3C970's 2 pi
#[allow(clippy::approx_constant)]
const EXE_TAU: f32 = 6.2832;

/// One of a layer's two quake slots (sub_63520; stage bytes +3604, delay +3684, remaining +3844, parameters
/// +4004; runtime sprite +344..). Slot 0 takes SQK2 (mode 1), slot 1 SHK2 (mode 2) or SSW2 (mode 3). sub_3C970
/// steps it once per 60 Hz frame; times are frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct Quake {
    pub mode: u8,
    /// frames before it starts, counted down (the stage keeps the count)
    pub delay: i16,
    /// lifetime in frames, -1 forever
    pub remaining: i16,
    /// mode 1: amp x, amp y, frames per offset - 1, ramp-in, fade-out, follow layers 0 / 1;
    /// mode 2: amp x, amp y, period, fade over the lifetime, ramp-in, 1-in-5 divisor;
    /// mode 3: amp min, amp max, period min, period max, strength
    pub params: [i16; 6],
    pub elapsed: i16,
    pub counter: i16,
    /// mode 3 per axis: amplitude, period and sign rolled each cycle, and the y counter
    amp: [i16; 2],
    period: [i16; 2],
    sign: [i16; 2],
    counter_y: i16,
    /// the offset sub_34440 adds as a third of a 640x448 unit (sprite +388 / +396)
    pub offset: (f32, f32),
}

impl Quake {
    pub fn new(mode: u8, params: [i16; 6], dur_ms: i32, delay_ms: i32) -> Self {
        Self {
            mode,
            delay: (3 * delay_ms / 50) as i16,
            remaining: if dur_ms < 0 { -1 } else { (3 * dur_ms / 50) as i16 },
            params,
            ..Self::default()
        }
    }

    /// The saved slot (sub_5D3A0 restores mode, delay, remaining and parameters; the counters start at 0).
    pub fn restore(mode: u8, delay: i16, remaining: i16, params: [i16; 6]) -> Self {
        Self { mode, delay, remaining, params, ..Self::default() }
    }

    pub fn active(&self) -> bool {
        self.mode != 0
    }

    /// One frame of sub_3C970. `lead`: for layers 2+ the offset of this slot on layer 1 if it is drawn, else
    /// layer 0's, else zero (a mode 1 quake with the follow flag copies it).
    fn step(&mut self, rng: &mut crate::fx::Rng, lead: Option<(f32, f32)>) {
        if self.delay >= 1 {
            self.delay = (self.delay - 1).max(0);
            return;
        }
        if self.remaining == 0 {
            return;
        }
        self.elapsed = (self.elapsed + 1).min(30000);
        if self.remaining >= 1 && self.remaining < self.elapsed {
            *self = Self::default();
            return;
        }
        let p = self.params.map(f32::from);
        let e = f32::from(self.elapsed);
        match self.mode {
            1 => {
                self.counter += 1;
                if self.params[2] >= self.counter {
                    return;
                }
                self.counter = 0;
                if let Some(o) = lead.filter(|_| self.params[5] != 0) {
                    self.offset = o;
                    return;
                }
                let mut k = 1.0;
                if e >= p[3] {
                    if self.remaining >= 1 {
                        let t = f32::from(self.remaining) - p[4];
                        if t < e {
                            k = 1.0 - (e - t) / p[4];
                        }
                    }
                } else {
                    k = e / p[3];
                }
                let mut r = || rng.f() * 2.0 - 1.0;
                self.offset = (p[0] * k * r(), p[1] * k * r());
            }
            2 => {
                let period = if self.params[2] == 0 { 60 } else { self.params[2] };
                let ap = period.unsigned_abs() as i16;
                let mut c = self.counter + 1;
                while c >= ap {
                    c -= ap;
                }
                self.counter = c;
                let mut s = (f32::from(c) * EXE_TAU / f32::from(period)).sin();
                if self.params[4] >= 1 && self.elapsed < self.params[4] {
                    s *= e / p[4];
                }
                if self.params[3] != 0 && self.remaining >= 1 {
                    let rem = f32::from(self.remaining);
                    s *= (rem - e.min(rem)) / rem;
                }
                if self.params[5] > 1 {
                    s = if rng.rand() % 5 == 0 { s / p[5] } else { 0.0 };
                }
                self.offset = (s * p[0], s * p[1]);
            }
            3 => {
                let mut axis = |c: &mut i16, amp: &mut i16, period: &mut i16, sign: &mut i16| {
                    *c += 1;
                    if *period < *c {
                        *c = 0;
                        let u = rng.rand() as f32 * 4.6566e-10;
                        *amp = (u * (p[1] - p[0]) + p[0]) as i16;
                        let u = rng.rand() as f32 * 4.6566e-10;
                        *period = (u * (p[3] - p[2]) + p[2]) as i16;
                        if *sign == 0 {
                            *sign = if (rng.rand() as f32 * 4.6566e-10) < 0.5 { 1 } else { -1 };
                        }
                    }
                    let v = f32::from(*sign) * f32::from(*amp) * p[4] * (f32::from(*c) * EXE_TAU / f32::from(*period)).sin();
                    if v.is_nan() { 0.0 } else { v }
                };
                let x = axis(&mut self.counter, &mut self.amp[0], &mut self.period[0], &mut self.sign[0]);
                let y = axis(&mut self.counter_y, &mut self.amp[1], &mut self.period[1], &mut self.sign[1]);
                self.offset = (x, y);
            }
            _ => self.offset = (0.0, 0.0),
        }
    }
}

impl Layer {
    pub fn new(name: String, source: Source, x: f32, y: f32, prio: i32) -> Self {
        Self {
            name,
            source,
            x: Tween::fixed(x),
            y: Tween::fixed(y),
            zoom_x: Tween::fixed(1.0),
            zoom_y: Tween::fixed(1.0),
            alpha: Tween::fixed(1.0),
            rotation: Tween::fixed(0.0),
            prio,
            parent: None,
            base: None,
            absolute: false,
            noise: None,
            rule: None,
            rule_flip: 0,
            pending: true,
            blend: Blend::Alpha,
            stbr: -1,
            shade: Shade::Sprite,
            fill: None,
            bake: crate::grade::Bake::NONE,
            stec_mode: 0,
            stec_color: [1.0; 3],
            keeps_texture: false,
            motion: None,
            motion_alpha: 1.0,
            motion_zoom: (1.0, 1.0),
            rot2: 0,
            clip_inset: 0,
            clip_mode: 0,
            soxy: (0, 0),
            pivot: None,
            shift: None,
            quake: [Quake::default(); 2],
            snx: [1.0; 2],
            haze: Haze::default(),
            clip: None,
        }
    }

    pub fn advance(&mut self, ms: f32) {
        self.haze.advance(ms);
    }

    /// One 60 Hz frame of the tweens (sub_3C970).
    pub fn step_tweens(&mut self) {
        for t in [
            &mut self.x,
            &mut self.y,
            &mut self.zoom_x,
            &mut self.zoom_y,
            &mut self.alpha,
            &mut self.rotation,
        ] {
            t.step();
        }
    }

    /// sub_329F0: a layer is busy while one of the `mask` parts runs: 1 position, 4 rotation, 8 alpha,
    /// 0x20 the motion (bit 2 is a tween no command sets, 0x10 the unused STRC clip tween); zoom is never
    /// waited on.
    pub fn animating(&self, mask: i32) -> bool {
        (mask & 0x20 != 0 && self.motion.as_ref().is_some_and(|m| !m.finished() && !m.paused))
            || (mask & 1 != 0 && !(self.x.done() && self.y.done()))
            || (mask & 4 != 0 && !self.rotation.done())
            || (mask & 8 != 0 && !self.alpha.done())
    }

    pub fn finish(&mut self) {
        for t in [
            &mut self.x,
            &mut self.y,
            &mut self.zoom_x,
            &mut self.zoom_y,
            &mut self.alpha,
            &mut self.rotation,
        ] {
            t.finish();
        }
    }
}

#[derive(Default)]
pub struct Stage {
    pub layers: Vec<Option<Layer>>,
    /// The screen shake offset (off_AE0580 / off_AE0588) in 640x448 draw units.
    pub shake: (f32, f32),
    /// frames drawn, drives the STNS grain jitter
    pub frame: u32,
    /// STNS grain textures (noisehd0, noisehd1)
    pub noise: Option<[TexRef; 2]>,
    /// The exe's render target 40 that STNS paints into and never clears (owned by Game)
    pub noise_rt: Option<TexRef>,
    /// What STCC left in the stage block of each layer it cleared (only saves show it)
    pub kept: Vec<Option<crate::save::Kept>>,
    /// the quake slots' random source and unspent time for their 60 Hz steps
    quake_rng: crate::fx::Rng,
    frame_acc: f32,
}

impl Stage {
    pub fn new() -> Self {
        Self {
            layers: vec![None; LAYERS],
            shake: (0.0, 0.0),
            frame: 0,
            noise: None,
            noise_rt: None,
            kept: vec![None; LAYERS],
            quake_rng: crate::fx::Rng::default(),
            frame_acc: 0.0,
        }
    }

    /// STNS (sub_35FC0), once per frame: the quad that paints the noise into render target 40
    /// (canvas sized, never cleared). Noise texture 390 (noisehd1) when n%10==2, else 389 (noisehd0),
    /// scaled 0.6 in a 960x540 space (1.2x at canvas size) at a fresh random offset of 0..-100 x 0..-45
    /// units, mirrored horizontally on alternate ticks (each frame when n>2, else every third). The
    /// strip the shifted texture misses keeps earlier frames' noise, as in the exe. Grain layers then
    /// show RT40 in place of their image. All grain layers share RT40 like the exe; the first one's
    /// parameters drive it.
    pub fn noise_pass(&self) -> Option<Quad> {
        let nt = self.noise.as_ref()?;
        let n = self.layers.iter().flatten().find_map(|l| l.noise.filter(|_| !l.pending))?;
        let t = &nt[usize::from(n % 10 == 2)];
        let tick = self.frame / if n > 2 { 1 } else { 3 };
        let rnd = |k: u32| {
            let mut v = self.frame.wrapping_mul(0x9E37_79B9) ^ k.wrapping_mul(0x85EB_CA6B);
            v ^= v >> 15;
            v = v.wrapping_mul(0x2C1B_3C6D);
            v ^= v >> 12;
            (v & 0xFFFF) as f32 / 65535.0
        };
        let k = CANVAS_W as f32 / 1920.0;
        let (x0, y0) = (-rnd(1) * 200.0 * k, -rnd(2) * 90.0 * k);
        let (tw, th) = (t.width as f32 * 1.2 * k, t.height as f32 * 1.2 * k);
        let um = t.uv_max();
        let (u0, u1) = if tick % 2 == 1 { (um[0], 0.0) } else { (0.0, um[0]) };
        let mut q = Quad::rect(t.clone(), [x0, y0, tw, th], [1.0; 4]);
        q.uv = [[u0, 0.0], [u1, 0.0], [u1, um[1]], [u0, um[1]]];
        Some(q)
    }

    pub fn get(&mut self, i: i32) -> Option<&mut Layer> {
        self.layers.get_mut(usize::try_from(i).ok()?)?.as_mut()
    }

    pub fn set(&mut self, i: i32, layer: Layer) {
        if let Some(slot) = usize::try_from(i).ok().and_then(|i| self.layers.get_mut(i)) {
            *slot = Some(layer);
        }
    }

    pub fn clear(&mut self, i: i32) {
        if i < 0 {
            self.layers.iter_mut().for_each(|l| *l = None);
        } else if let Some(slot) = self.layers.get_mut(i as usize) {
            *slot = None;
        }
    }

    /// sub_5D3A0 (FADS, the end of the next-scene seek): the stage becomes what is drawn, and sub_31F30(layer,
    /// target, target, 0 frames) lands each layer's zoom tween on the stage's zoom target (kind 0).
    pub fn commit(&mut self) {
        for l in self.layers.iter_mut().flatten() {
            l.pending = false;
            for t in [&mut l.zoom_x, &mut l.zoom_y] {
                *t = Tween::new(t.to, t.to, 0, 0);
            }
        }
    }

    pub fn advance(&mut self, ms: f32) {
        self.frame = self.frame.wrapping_add(1);
        for l in self.layers.iter_mut().flatten() {
            l.advance(ms);
        }
        self.frame_acc += ms;
        while self.frame_acc >= 1000.0 / 60.0 {
            self.frame_acc -= 1000.0 / 60.0;
            for l in self.layers.iter_mut().flatten() {
                l.step_tweens();
            }
            self.step_quakes();
        }
    }

    /// The quake part of sub_3C970, layers in order so that followers see layers 0 / 1 already stepped.
    fn step_quakes(&mut self) {
        for li in 0..self.layers.len() {
            for s in 0..2 {
                let lead = |k: usize| self.layers[k].as_ref().filter(|l| !l.pending).map(|l| l.quake[s].offset);
                let follow = (li >= 2).then(|| lead(1).or_else(|| lead(0)).unwrap_or((0.0, 0.0)));
                if let Some(l) = self.layers[li].as_mut()
                    && l.quake[s].active()
                {
                    l.quake[s].step(&mut self.quake_rng, follow);
                }
            }
        }
    }

    /// sub_34440: the quake offsets of a layer in canvas pixels (a third of a unit of the 640x448 draw space,
    /// sub_B5650's viewport). A
    /// mode 1 slot takes the offset of its topmost quaking parent; without a parent, a quaking base gives its
    /// offset, or none when the base clips.
    fn quake_px(&self, i: usize) -> (f32, f32) {
        let Some(l) = self.layers[i].as_ref() else { return (0.0, 0.0) };
        let get = |k: usize| self.layers.get(k).and_then(|l| l.as_ref());
        let (mut x, mut y) = (0.0, 0.0);
        for s in 0..2 {
            let q = &l.quake[s];
            if !q.active() {
                continue;
            }
            let mut off = q.offset;
            if q.mode == 1 {
                if let Some(p) = l.parent {
                    let mut cur = Some(p);
                    let mut depth = 0;
                    while let Some(k) = cur
                        && let Some(pl) = get(k)
                        && depth < LAYERS
                    {
                        if pl.quake[s].active() {
                            off = pl.quake[s].offset;
                        }
                        cur = pl.parent;
                        depth += 1;
                    }
                } else if let Some(bl) = l.base.and_then(get)
                    && bl.quake[s].active()
                {
                    off = if bl.clip.is_some() { (0.0, 0.0) } else { bl.quake[s].offset };
                }
            }
            x += off.0;
            y += off.1;
        }
        (x / 3.0 * CANVAS_W as f32 / 640.0, y / 3.0 * CANVAS_H as f32 / 448.0)
    }

    pub fn animating(&self, i: i32, mask: i32) -> bool {
        if i < 0 {
            self.layers.iter().flatten().any(|l| l.animating(mask))
        } else {
            self.layers
                .get(i as usize)
                .and_then(|l| l.as_ref())
                .is_some_and(|l| l.animating(mask))
        }
    }

    /// Resolved (x, y, alpha, clip) of a layer including its parents, in script units.
    fn resolved(&self, i: usize, depth: u32) -> Resolved {
        let Some(l) = self.layers[i].as_ref() else {
            return Resolved::default();
        };
        let mut r = Resolved {
            x: l.x.value_x(),
            y: l.y.value(),
            alpha: l.alpha.value() * l.motion_alpha,
            // the layer's own clip carries its own inset and mode (sprite +296 / +298)
            clip: l.clip.map(|mut c| {
                c.inset = l.clip_inset;
                c.mode = l.clip_mode;
                c
            }),
            zoom: (l.zoom_x.value() * l.motion_zoom.0, l.zoom_y.value() * l.motion_zoom.1),
            rot: l.rotation.value_rot(),
            spin: 0.0,
        };
        if let Some(b) = l.base.filter(|&b| b != i && b < LAYERS)
            && depth < 8
            && let Some(bl) = self.layers[b].as_ref()
        {
            let br = self.resolved(b, depth + 1);
            // anchor: the base's pivot (its stage anchor, the image centre unless STPV / a motion moved it) in
            // its own unzoomed script space; children of a sized-down image (level 1..3) scale by its level
            // (sub_89190 / sub_892B0)
            let (ax, ay, k) = match &bl.source {
                Source::Image(img) => {
                    let [px, py] = bl.pivot.unwrap_or([img.center_x, img.center_y]);
                    (px * SCRIPT_W / UNIT_W, py * SCRIPT_H / UNIT_H, img.level)
                }
                Source::Color(_) => (SCRIPT_W / 2.0, SCRIPT_H / 2.0, 1.0),
            };
            r.x *= k;
            r.y *= k;
            r.zoom = (r.zoom.0 * k, r.zoom.1 * k);
            // rotate in canvas pixels so the result matches how the base is drawn
            let kx = CANVAS_W as f32 / SCRIPT_W;
            let ky = CANVAS_H as f32 / SCRIPT_H;
            let dx = (r.x - ax) * br.zoom.0 * kx;
            let dy = (r.y - ay) * br.zoom.1 * ky;
            let (sin, cos) = br.rot.to_radians().sin_cos();
            r.x = br.x + (dx * cos - dy * sin) / kx;
            r.y = br.y + (dx * sin + dy * cos) / ky;
            r.zoom = (r.zoom.0 * br.zoom.0, r.zoom.1 * br.zoom.1);
            r.rot += br.rot;
            r.clip = r.clip.or(br.clip);
        }
        if let Some(p) = l.parent.filter(|&p| p != i && p < LAYERS)
            && depth < 8
            && let Some(pl) = self.layers[p].as_ref()
        {
            let pr = self.resolved(p, depth + 1);
            match pl.clip {
                _ if l.absolute => {}
                Some(c) => {
                    // children of a clip window are placed relative to its top-left corner
                    r.x += c.x * SCRIPT_W / CANVAS_W as f32;
                    r.y += c.y * SCRIPT_H / CANVAS_H as f32;
                }
                None => {
                    r.x += pr.x - SCRIPT_W / 2.0;
                    r.y += pr.y - SCRIPT_H / 2.0;
                }
            }
            r.alpha *= pr.alpha;
            if !l.absolute {
                // sub_37F50 copies the parent's clip, inset, mode and STRR rotation over the child's own (a
                // motion keying the child's clip to 0 x 0 changes nothing)
                r.clip = pr.clip.map(|mut c| {
                    let rot2 = pl.rot2;
                    c.inset = pl.clip_inset;
                    c.mode = pl.clip_mode;
                    c.rot = match rot2 {
                        0 => 0.0,
                        i16::MIN => {
                            // follows the child's own rotation (with its base's), normalised to +-180
                            let d = (r.rot * 100.0) as i32 % 36000;
                            let d = if d > 18000 { d - 36000 } else if d <= -18001 { d + 36000 } else { d };
                            d.min(35999) as f32 / 100.0
                        }
                        v => f32::from(v) / 100.0,
                    };
                    r.spin = c.rot;
                    c
                });
            }
        }
        r
    }

    pub fn quads(&self, white: &TexRef) -> Vec<Quad> {
        // sub_3BBA0 draws a layer (sub_37F50) only while its priority (render dword 5883 + layer) is >= 1, so
        // STCP(l, 0) hides it; the rest go back to front by priority, ties by layer number
        let mut order: Vec<usize> = (0..self.layers.len())
            .filter(|&i| self.layers[i].as_ref().is_some_and(|l| !l.pending && l.prio >= 1))
            .collect();
        order.sort_by_key(|&i| (self.layers[i].as_ref().unwrap().prio, i));
        let mut out = Vec::new();
        for i in order {
            let l = self.layers[i].as_ref().unwrap();
            let Resolved { x, y, alpha, clip, zoom: (zx, zy), rot, spin } = self.resolved(i, 0);
            if alpha <= 0.0 {
                continue;
            }
            let (mut cx, mut cy) = script_to_px(x, y);
            // sub_34440 adds the screen shake twice (640x448 units) unless the image is exempt (+65 bit 1, the
            // colour fills) or the layer clips with a base and is not absolute; sub_32050 shifts the clip
            // scissor the same way unless the image is exempt
            let exempt = match &l.source {
                Source::Image(img) => img.no_shake,
                Source::Color(_) => true,
            };
            let shake = if exempt { (0.0, 0.0) } else { (2.0 * self.shake.0 * CANVAS_W as f32 / 640.0, 2.0 * self.shake.1 * CANVAS_H as f32 / 448.0) };
            let clip = clip.map(|mut c| {
                c.x += shake.0;
                c.y += shake.1;
                c
            });
            if !(clip.is_some() && l.base.is_some() && !l.absolute) {
                cx += shake.0;
                cy += shake.1;
            }
            // sub_37F50: SOXY in 1920x1080 pixels, scaled into the 640x448 draw space
            cx += f32::from(l.soxy.0) * 0.33333 * CANVAS_W as f32 / 640.0;
            cy += f32::from(l.soxy.1) * 0.41481 * CANVAS_H as f32 / 448.0;
            let (qx, qy) = self.quake_px(i);
            cx += qx;
            cy += qy;
            let rot = rot.to_radians();
            let sx = CANVAS_W as f32 / UNIT_W;
            let sy = CANVAS_H as f32 / UNIT_H;
            let (mut tex, w, h, ox, oy, uvm) = match &l.source {
                Source::Image(img) => {
                    // sub_34440: the quad turns about the pivot and is offset by pivot + shift
                    let centre = [img.center_x, img.center_y];
                    let piv = l.pivot.unwrap_or(centre);
                    let sh = l.shift.unwrap_or([-centre[0], -centre[1]]);
                    cx += (piv[0] + sh[0]) * CANVAS_W as f32 / UNIT_W;
                    cy += (piv[1] + sh[1]) * 1.25 * CANVAS_H as f32 / 448.0;
                    (img.tex.clone(), img.width * sx, img.height * sy, piv[0] * sx, piv[1] * sy, img.tex.uv_max())
                }
                Source::Color(_) => (
                    white.clone(),
                    CANVAS_W as f32 * 1.2,
                    CANVAS_H as f32 * 1.2,
                    CANVAS_W as f32 * 0.6,
                    CANVAS_H as f32 * 0.6,
                    [1.0, 1.0],
                ),
            };
            // STNS grain layers show render target 40 (see noise_pass) instead of their image
            let mut uv = [0.0, 0.0, uvm[0], uvm[1]];
            if let (Some(_), Some(rt)) = (l.noise, &self.noise_rt) {
                tex = rt.clone();
                uv = [0.0, 0.0, 1.0, 1.0];
            }
            let (sin, cos) = rot.sin_cos();
            // a child of a rotated clip is drawn into the clip rect and composited turned about its centre
            let (ssin, scos) = spin.to_radians().sin_cos();
            let (scx, scy) = clip.map_or((0.0, 0.0), |c| (c.x + c.w / 2.0, c.y + c.h / 2.0));
            let corner = |px: f32, py: f32| {
                let dx = (px - ox) * zx;
                let dy = (py - oy) * zy;
                let (qx, qy) = (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);
                if spin == 0.0 {
                    return [qx, qy];
                }
                let (rx, ry) = (qx - scx, qy - scy);
                [scx + rx * scos - ry * ssin, scy + rx * ssin + ry * scos]
            };
            // sub_37F50 -> sub_32C70: a layer that is not a colour fill is multiplied by its render colour
            // (+460: white unless a motion's colour keys set it)
            let rgb = match (&l.source, l.fill) {
                (_, Some(f)) => f,
                (Source::Color(c), None) if l.stec_mode != 1 => {
                    [c[0] * l.stec_color[0], c[1] * l.stec_color[1], c[2] * l.stec_color[2]]
                }
                (Source::Color(c), None) => *c,
                _ if l.stec_mode != 1 => l.stec_color,
                _ => [1.0; 3],
            };
            let shade = match (&l.source, l.fill) {
                (Source::Color(_), _) => Shade::Sprite,
                (_, Some(_)) => Shade::Fill,
                _ => l.shade,
            };
            // with a rule, sub_36520 draws the layer at colour 1 into target 2 and that through the rule with
            // sub_A7190: the draw alpha is both the wipe progress and the colour, so it also fades
            let rule = l.rule.as_ref().map(|(_, t)| knife_render::RuleMask {
                // the shader compares c3[0x3c] with 1 / 2 / 3: 1 and 3 mirror x, 2 and 3 mirror y
                flip: if l.rule_flip <= 3 { l.rule_flip } else { 0 },
                ..knife_render::RuleMask::new(t.clone(), alpha.clamp(0.0, 1.0))
            });
            // STRA: the layer's texture (slot 393 + layer) is drawn through the haze shader
            let haze = match &l.source {
                Source::Image(img) if l.noise.is_none() => {
                    l.haze.params(img.tex.width as f32, img.tex.height as f32, uvm)
                }
                _ => None,
            }
            .unwrap_or_default();
            let [u0, v0, u1, v1] = uv;
            out.push(Quad {
                tex,
                pos: [corner(0.0, 0.0), corner(w, 0.0), corner(w, h), corner(0.0, h)],
                uv: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                color: [rgb[0], rgb[1], rgb[2], alpha.clamp(0.0, 1.0)],
                blend: l.blend,
                shade,
                // a 0-sized clip is no clip
                clip: clip.filter(|c| c.w != 0.0 && c.h != 0.0).map(|c| c.corners()),
                rule,
                haze,
                soft: clip.and_then(|c| c.soft()),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_tween_runs_from_4096_down() {
        // STGS(0,,255,800,0) from 0: 48 frames, step int(-4096 / 48) = -85, so 49 steps reach 0
        let mut t = Tween::new(0.0, 1.0, 48, 0);
        assert_eq!((t.prog, t.step), (4096, -85));
        // 4096 * 0.00024414 is just under 1
        assert!(t.value() < 1e-5);
        t.step();
        assert!((t.value() - 85.0 * 0.00024414).abs() < 1e-5);
        for _ in 0..47 {
            t.step();
        }
        assert!(!t.done());
        t.step();
        assert!(t.done() && t.value() == 1.0);
    }

    #[test]
    fn every_ease_kind_spans_from_to() {
        for kind in 0..=42 {
            let a = crate::motion::ease(kind, 0.0);
            let b = crate::motion::ease(kind, 1.0);
            assert!(a.abs() < 2e-3 && (b - 1.0).abs() < 2e-3, "kind {kind}: {a} {b}");
            // the layer form (d = -1) starts at `from` for the in and out kinds; in-out and out-in halves swap
            if kind < 2 || kind > 41 || (kind - 2) % 4 < 2 {
                let t = Tween::new(3.0, 1.0, 10, kind);
                assert!((t.value() - 3.0).abs() < 1e-2, "kind {kind}: {}", t.value());
            }
        }
    }

    #[test]
    fn sqk2_quake_steps_every_third_frame_and_fades_out() {
        // SQK2(-1, 30, 45, 0, 300): frames 2, 18 frames long, fading over all 18
        let mut q = Quake::new(1, [30, 45, 2, 0, 18, 0], 300, 0);
        assert_eq!(q.remaining, 18);
        let mut rng = crate::fx::Rng::default();
        let mut seen = Vec::new();
        for _ in 0..19 {
            q.step(&mut rng, None);
            seen.push(q.offset);
        }
        // a new offset on frames 3, 6, ... within +-ax * k, k = 1 - e / 18
        assert_eq!(seen[0], (0.0, 0.0));
        assert_eq!(seen[1], (0.0, 0.0));
        assert_ne!(seen[2], (0.0, 0.0));
        assert_eq!(seen[3], seen[2]);
        assert!(seen[2].0.abs() <= 30.0 * (1.0 - 3.0 / 18.0) + 1e-4);
        // frame 19 is past the lifetime: the slot clears
        assert!(!q.active());
        assert_eq!(q.offset, (0.0, 0.0));
    }
}
