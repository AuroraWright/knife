//! Script-visible layers ("stage channels") and their animations.

use std::sync::Arc;

use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Shade, TexRef};

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

#[derive(Clone, Copy, Debug, Default)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub dur: f32,
    pub t: f32,
    pub ease: i32,
}

impl Tween {
    pub fn new(from: f32, to: f32, ms: f32, ease: i32) -> Self {
        Self {
            from,
            to,
            dur: ms.max(0.0),
            t: 0.0,
            ease,
        }
    }
    pub fn fixed(v: f32) -> Self {
        Self::new(v, v, 0.0, 0)
    }
    pub fn value(&self) -> f32 {
        if self.dur <= 0.0 || self.t >= self.dur {
            return self.to;
        }
        let p = self.t / self.dur;
        let p = crate::motion::ease(self.ease, p);
        self.from + (self.to - self.from) * p
    }
    pub fn done(&self) -> bool {
        self.dur <= 0.0 || self.t >= self.dur
    }
    pub fn advance(&mut self, ms: f32) {
        self.t = (self.t + ms).min(self.dur.max(0.0));
    }
    /// Retarget from the current value.
    pub fn retarget(&mut self, to: f32, ms: f32, ease: i32) {
        *self = Self::new(self.value(), to, ms, ease);
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
    pub shade: Shade,
    /// STEC colour fill (silhouette), rgb
    pub fill: Option<[f32; 3]>,
    pub motion: Option<MotionState>,
    /// Extra offset (script units) from motions / shakes.
    pub offset: (f32, f32),
    pub motion_alpha: f32,
    pub motion_zoom: (f32, f32),
    pub motion_rot: f32,
    pub quake: Option<Quake>,
    /// SSW2 smooth random sway
    pub sway: Option<Sway>,
    /// SNX2 brightness multiplier
    pub bright: f32,
    /// clip window (STCH args 12..16); children are placed relative to its top-left
    pub clip: Option<ClipRect>,
    /// SBAS transform base: position is in the base's local image space and
    /// inherits its zoom and rotation
    pub base: Option<usize>,
    /// SACL: placed in absolute coordinates and not clipped despite having a parent
    pub absolute: bool,
    /// STTR: alpha fades of this layer wipe through a rule image (number, texture)
    pub rule: Option<(i32, TexRef)>,
    /// STNS: film-grain value; the layer draws the NOISE_PARTS texture instead of its image
    pub noise: Option<i32>,
}

#[derive(Default)]
struct Resolved {
    x: f32,
    y: f32,
    alpha: f32,
    clip: Option<ClipRect>,
    zoom: (f32, f32),
    rot: f32,
}

/// Rotated clip rectangle in 1920x1080 canvas pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClipRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// degrees
    pub rot: f32,
}

impl ClipRect {
    pub fn corners(&self) -> [[f32; 2]; 4] {
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        let (s, c) = self.rot.to_radians().sin_cos();
        let p = |dx: f32, dy: f32| [cx + dx * c - dy * s, cy + dx * s + dy * c];
        let (hw, hh) = (self.w / 2.0, self.h / 2.0);
        [p(-hw, -hh), p(hw, -hh), p(hw, hh), p(-hw, hh)]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Quake {
    pub ax: f32,
    pub ay: f32,
    pub period: f32,
    /// remaining ms (negative = forever)
    pub left: f32,
    pub t: f32,
}

/// SSW2: per axis `sign * amp * strength * sin(2pi t / period)`, with amp and period re-rolled
/// within their ranges every cycle (sub_3C970, quake slot mode 3).
#[derive(Clone, Copy, Debug)]
pub struct Sway {
    pub amp: (f32, f32),
    pub period: (f32, f32),
    pub strength: f32,
    /// remaining ms (negative = forever)
    pub left: f32,
    pub seed: u32,
    axes: [SwayAxis; 2],
    pub offset: (f32, f32),
}

#[derive(Clone, Copy, Debug, Default)]
struct SwayAxis {
    t: f32,
    amp: f32,
    period: f32,
    sign: f32,
}

impl Sway {
    pub fn new(amp: (f32, f32), period: (f32, f32), strength: f32, dur: f32, seed: u32) -> Self {
        Self {
            amp,
            period,
            strength,
            left: dur,
            seed,
            axes: [SwayAxis::default(); 2],
            offset: (0.0, 0.0),
        }
    }

    fn rand(&mut self) -> f32 {
        // xorshift32
        let mut x = self.seed.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    fn advance(&mut self, ms: f32) -> bool {
        if self.left >= 0.0 {
            self.left -= ms;
            if self.left <= 0.0 {
                return false;
            }
        }
        let mut out = [0.0; 2];
        for i in 0..2 {
            let mut a = self.axes[i];
            a.t += ms;
            if a.period <= 0.0 || a.t > a.period {
                a.t = 0.0;
                a.amp = self.amp.0 + self.rand() * (self.amp.1 - self.amp.0);
                a.period = (self.period.0 + self.rand() * (self.period.1 - self.period.0)).max(16.0);
                if a.sign == 0.0 {
                    a.sign = if self.rand() < 0.5 { 1.0 } else { -1.0 };
                }
            }
            out[i] = a.sign * a.amp * self.strength * (a.t * std::f32::consts::TAU / a.period).sin();
            self.axes[i] = a;
        }
        self.offset = (out[0], out[1]);
        true
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
            pending: true,
            blend: Blend::Alpha,
            shade: Shade::Sprite,
            fill: None,
            motion: None,
            offset: (0.0, 0.0),
            motion_alpha: 1.0,
            motion_zoom: (1.0, 1.0),
            motion_rot: 0.0,
            quake: None,
            sway: None,
            bright: 1.0,
            clip: None,
        }
    }

    pub fn advance(&mut self, ms: f32) {
        if let Some(sw) = &mut self.sway
            && !sw.advance(ms)
        {
            self.sway = None;
        }
        if let Some(q) = &mut self.quake {
            q.t += ms;
            if q.left >= 0.0 {
                q.left -= ms;
            }
            if q.left == 0.0 || (q.left < 0.0 && q.left > -1.0e9 && q.left + ms >= 0.0) {
                self.quake = None;
                self.offset = (0.0, 0.0);
            } else {
                // alternate direction every period with a pseudo-random magnitude
                let step = (q.t / q.period.max(16.0)) as u32;
                let h = step.wrapping_mul(2654435761) >> 16;
                let rx = ((h & 0xFF) as f32 / 255.0) * 0.5 + 0.5;
                let ry = (((h >> 8) & 0xFF) as f32 / 255.0) * 0.5 + 0.5;
                let sign = if step % 2 == 0 { 1.0 } else { -1.0 };
                self.offset = (q.ax * rx * sign, q.ay * ry * -sign);
            }
        }
        for t in [
            &mut self.x,
            &mut self.y,
            &mut self.zoom_x,
            &mut self.zoom_y,
            &mut self.alpha,
            &mut self.rotation,
        ] {
            t.advance(ms);
        }
    }

    pub fn animating(&self) -> bool {
        ![self.x, self.y, self.zoom_x, self.zoom_y, self.alpha, self.rotation]
            .iter()
            .all(|t| t.done())
            || self.motion.as_ref().is_some_and(|m| !m.finished() && !m.paused)
            || self.quake.as_ref().is_some_and(|q| q.left > 0.0)
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
            t.t = t.dur;
        }
    }
}

#[derive(Default)]
pub struct Stage {
    pub layers: Vec<Option<Layer>>,
    /// Whole-screen shake offset in script units.
    pub shake: (f32, f32),
    /// frames drawn, drives the STNS grain jitter
    pub frame: u32,
    /// STNS grain textures (noise1, noisehd0)
    pub noise: Option<[TexRef; 2]>,
    /// The exe's render target 40 that STNS paints into and never clears (owned by Game)
    pub noise_rt: Option<TexRef>,
}

impl Stage {
    pub fn new() -> Self {
        Self {
            layers: vec![None; LAYERS],
            shake: (0.0, 0.0),
            frame: 0,
            noise: None,
            noise_rt: None,
        }
    }

    /// STNS (sub_35FC0), once per frame: the quad that paints the noise into render target 40
    /// (canvas sized, never cleared). Noise texture 390 (noisehd0) when n%10==2, else 389 (noise1),
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

    pub fn commit(&mut self) {
        for l in self.layers.iter_mut().flatten() {
            l.pending = false;
        }
    }

    pub fn advance(&mut self, ms: f32) {
        self.frame = self.frame.wrapping_add(1);
        for l in self.layers.iter_mut().flatten() {
            l.advance(ms);
        }
    }

    pub fn animating(&self, i: i32) -> bool {
        if i < 0 {
            self.layers.iter().flatten().any(|l| l.animating())
        } else {
            self.layers
                .get(i as usize)
                .and_then(|l| l.as_ref())
                .is_some_and(|l| l.animating())
        }
    }

    /// Resolved (x, y, alpha, clip) of a layer including its parents, in script units.
    fn resolved(&self, i: usize, depth: u32) -> Resolved {
        let Some(l) = self.layers[i].as_ref() else {
            return Resolved::default();
        };
        let mut r = Resolved {
            x: l.x.value() + l.offset.0 + l.sway.map_or(0.0, |s| s.offset.0),
            y: l.y.value() + l.offset.1 + l.sway.map_or(0.0, |s| s.offset.1),
            alpha: l.alpha.value() * l.motion_alpha,
            clip: l.clip,
            zoom: (l.zoom_x.value() * l.motion_zoom.0, l.zoom_y.value() * l.motion_zoom.1),
            rot: l.rotation.value() + l.motion_rot,
        };
        if let Some(b) = l.base.filter(|&b| b != i && b < LAYERS)
            && depth < 8
            && let Some(bl) = self.layers[b].as_ref()
        {
            let br = self.resolved(b, depth + 1);
            // anchor: the base image's top-left in its own unzoomed script space
            let (ax, ay) = match &bl.source {
                Source::Image(img) => (
                    img.center_x * SCRIPT_W / UNIT_W,
                    img.center_y * SCRIPT_H / UNIT_H,
                ),
                Source::Color(_) => (SCRIPT_W / 2.0, SCRIPT_H / 2.0),
            };
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
            if r.clip.is_none() && !l.absolute {
                r.clip = pr.clip;
            }
        }
        r
    }

    pub fn quads(&self, white: &TexRef) -> Vec<Quad> {
        let mut order: Vec<usize> = (0..self.layers.len())
            .filter(|&i| self.layers[i].as_ref().is_some_and(|l| !l.pending))
            .collect();
        order.sort_by_key(|&i| (self.layers[i].as_ref().unwrap().prio, i));
        let mut out = Vec::new();
        for i in order {
            let l = self.layers[i].as_ref().unwrap();
            let Resolved { x, y, alpha, clip, zoom: (zx, zy), rot } = self.resolved(i, 0);
            if alpha <= 0.0 {
                continue;
            }
            let (x, y) = (x + self.shake.0, y + self.shake.1);
            let (cx, cy) = script_to_px(x, y);
            let rot = rot.to_radians();
            let sx = CANVAS_W as f32 / UNIT_W;
            let sy = CANVAS_H as f32 / UNIT_H;
            let (mut tex, w, h, ox, oy, uvm) = match &l.source {
                Source::Image(img) => (
                    img.tex.clone(),
                    img.width * sx,
                    img.height * sy,
                    img.center_x * sx,
                    img.center_y * sy,
                    img.tex.uv_max(),
                ),
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
            let corner = |px: f32, py: f32| {
                let dx = (px - ox) * zx;
                let dy = (py - oy) * zy;
                [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos]
            };
            let rgb = match (&l.source, l.fill) {
                (_, Some(f)) => f,
                (Source::Color(c), None) => *c,
                _ => [l.bright; 3],
            };
            let shade = match (&l.source, l.fill) {
                (Source::Color(_), _) => Shade::Sprite,
                (_, Some(_)) => Shade::Fill,
                _ => l.shade,
            };
            // with a rule, the layer's own opacity drives the wipe instead of blending
            let (alpha, rule) = match &l.rule {
                Some((_, t)) => {
                    let own = (l.alpha.value() * l.motion_alpha).clamp(0.0, 1.0);
                    let rest = if own > 0.0 { alpha / own } else { 0.0 };
                    let mask = knife_render::RuleMask { tex: t.clone(), progress: own, vague: 0.25 };
                    (rest, Some(mask))
                }
                None => (alpha, None),
            };
            let [u0, v0, u1, v1] = uv;
            out.push(Quad {
                tex,
                pos: [corner(0.0, 0.0), corner(w, 0.0), corner(w, h), corner(0.0, h)],
                uv: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                color: [rgb[0], rgb[1], rgb[2], alpha.clamp(0.0, 1.0)],
                blend: l.blend,
                shade,
                clip: clip.map(|c| c.corners()),
                rule,
            });
        }
        out
    }
}
