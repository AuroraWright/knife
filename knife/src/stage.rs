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
    /// Pivot (render floats 25 / 26, 640x362 units; None = `Image::anchor`, the centre cut to whole units):
    /// STPV and motion fields 6 / 7. Zoom and rotation turn about it.
    pub pivot: Option<[f32; 2]>,
    /// Quad offset (render floats 18 / 19; None = minus `Image::anchor`). The image is drawn shifted by
    /// pivot + shift; motion fields 6 / 7 set shift = -pivot as well.
    pub shift: Option<[f32; 2]>,
    /// The stage anchor (floats 221 / 261; None = `Image::anchor`) and shift word (2923 / 2924, whole units;
    /// None = minus the anchor) that sub_5D3A0 makes the render pivot and shift at the next commit or load.
    /// STPV sets both (the word to (int)-pivot) but only the render pivot at once; SBAS children anchor to
    /// the stage anchor (sub_892B0).
    pub stage_pivot: Option<[f32; 2]>,
    pub stage_shift: Option<[f32; 2]>,
    /// STPV arg 3 sets only the stage rotation (dword 161), which sub_5D3A0 makes the render rotation target
    /// (sub_32BF0) at the next commit; STRT and motion rotation keys write both.
    pub stage_rot: Option<f32>,
    /// quake slots 0 (SQK2) and 1 (SHK2 / SSW2)
    pub quake: [Quake; 2],
    /// Witch on the Holy Night's one quake slot (STQK / STSW)
    pub wquake: WitchQuake,
    /// SNX2 brightness (1 + v / 255) and SNX3 contrast (1 + v / 100) as the stage holds them (floats
    /// 1301 / 1302 + 4 layer); like the STEC mode they reach the texture only when it is built (`bake`)
    pub snx: [f32; 2],
    /// Witch on the Holy Night's SNX4 values (stage floats 4844 + 16 layer + 8 / + 12, render +0x20C / +0x210):
    /// saved and copied by SCH2 with a source layer, reset by STCH / STCC; nothing draws with them
    pub snx4: [f32; 2],
    /// Witch on the Holy Night's STGS stage words (2244 / 2404 + 2 layer): the fade's start alpha (0..255) and
    /// frames, with its curve byte (564 + layer). STCH sets (alpha, 0) and curve 0, STGS (from, frames) (or
    /// (to, 0) while skipping); the word between them (2324) is the drawn alpha, which jp_3C970 writes every
    /// frame. While the frames word is set a commit rebuilds the fade from the three (sub_3E130 ->
    /// sub_13ED0); the sound resume (jp_5D7E0) moves the start to the alpha.
    pub wfade: [i16; 2],
    pub wfade_ease: u8,
    /// STRA haze state (render words +284..+288)
    pub haze: Haze,
    /// clip window (STCH args 12..16); children are placed relative to its top-left
    pub clip: Option<ClipRect>,
    /// SBAS transform base: position is in the base's local image space and
    /// inherits its zoom and rotation
    pub base: Option<usize>,
    /// SACL: placed in absolute coordinates and not clipped despite having a parent
    pub absolute: bool,
    /// STTR's rule number as the stage holds it (word 1142, what saves keep; cmd_STCH_impl resets it). The
    /// draw uses `Stage::rules`.
    pub rule: Option<i32>,
    /// STTR arg 2 (b, stage word bits 12..14): 1 mirrors the rule in x, 2 in y, 3 both
    pub rule_flip: u8,
    /// What STBR / STCP / STRR / SACL / SBAS / SOXY wrote to the stage block since the last commit
    pub staged: Staged,
    /// STNS: film-grain value; the layer draws the NOISE_PARTS texture instead of its image
    pub noise: Option<i32>,
}

/// Stage-block values STBR (byte 124), STCP (word 282), STRR (word 1522), SACL (byte 84), SBAS (word 1182) and
/// SOXY (words 1222 / 1262) wrote; those commands touch only the stage, so the drawn layer keeps its values
/// until sub_5D3A0 (`Stage::commit`) hands them over (sub_322E0 / sub_32450 / sub_322C0, render +276 / +284 /
/// +328 through sub_3C970). None = the stage holds what is drawn.
#[derive(Clone, Copy, Debug, Default)]
pub struct Staged {
    /// the STBR mode and what it renders as
    pub stbr: Option<(i32, Blend)>,
    pub prio: Option<i32>,
    pub rot2: Option<i16>,
    pub absolute: Option<bool>,
    pub base: Option<Option<usize>>,
    pub soxy: Option<(i16, i16)>,
}

/// A slot's render rule (render +280): the rule texture and its mirror mode.
#[derive(Clone)]
pub struct RenderRule {
    pub tex: TexRef,
    pub flip: u8,
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

    /// sub_32E30: with an inset, a clip is masked by a rounded rectangle (drawn into render target 1 from
    /// BORDER_PARTS) whose corners and edges fade out over the radius: (inset + odd) / 2 in 960x540 units
    /// (twice that for mode 7, with the sharper `_2` textures), at most half the clip's extent. Unrotated, a side
    /// reaching past the screen (or a clip spanning the whole width / height) gets no fade, and with a mode of
    /// 1..6 the scissor shrinks by the inset, which cuts the faded band away. Turned, the pieces are centred on
    /// the band (sub_B0710(0.5, 0.5), anchor 4) at r + 0.5 (r for modes 1..6), so the fade runs from 0.25 outside
    /// the clip to 0.25 past the inner rectangle.
    pub fn soft(&self) -> Option<SoftClip> {
        let band = (1..=6).contains(&self.mode);
        if self.inset == 0 || band && self.rot == 0.0 {
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
        // the turned pieces' overhang past each side of the band
        let over = if self.rot == 0.0 || band { 0.0 } else { 0.25 };
        let [mut rl, mut rr, mut rt, mut rb] = [r + over; 4];
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
            radius: (r + 2.0 * over) * s,
            row,
            texels: RADII[k],
            // a turned clip fills the inner rectangle with texture slot 343 (alphagradation512)'s centre texels
            // 250..260 (sub_B7010, a flat white at alpha 252) instead of sub_B1580's opaque rect
            fill: if self.rot == 0.0 { 1.0 } else { 252.0 / 255.0 },
        })
    }

    /// sub_34A70 (a2 = 2 into a lone clipped layer's target 2, -3 into the group pixel surface from sub_37EA0):
    /// with a mode of 1..6 a ridge of the mode's colour (1 black, 2 white, 3 red, 4..6 tints; alpha 1, draw
    /// mode 27 = blend state 0) along the clip's edges, r = inset / 2 in 960x540 units (at most half the clip's
    /// extent) to each side of its crest, from BORDER_PARTS (`border`: alphagradation020..512 then their
    /// `_inv`; texture and pieces qword_572990 by r as for the soft mask). Unrotated (sides zeroed as for the
    /// soft mask) the outer half is solid (sub_B1580) and lies in the strip the shrunk scissor cuts away, and
    /// the inner half fades out over the next r inside it: the gradation's centre-to-edge column along the
    /// sides, the `_inv` quadrants around the corners. Turned (no scissor) the pieces hang off points r / 2 inside
    /// the clip: rising corner quadrants outside, `_inv` ones inside, and along each side a rising strip 2r deep
    /// (piece 1 / 3, anchored at its far end) and a falling one r deep (piece 6 / 4) that sub_34A70 places the
    /// same way on opposite sides, so the bottom and right ridges rise inward. Quads in canvas pixels, `clip` the
    /// scissor.
    pub fn band(&self, border: &[TexRef], white: &TexRef) -> Vec<Quad> {
        let half = f32::from(self.inset) * 0.5;
        if half < 1.0 || !(1..=6).contains(&self.mode) || border.len() < 12 {
            return Vec::new();
        }
        let (l, t) = (self.x * 0.5, self.y * 0.5);
        let (rx, by) = ((self.x + self.w) * 0.5, (self.y + self.h) * 0.5);
        let (w, h) = (rx - l, by - t);
        let r = half.min(w * 0.5).min(h * 0.5);
        if r <= 0.0 {
            return Vec::new();
        }
        const RADII: [u32; 6] = [10, 32, 40, 64, 128, 256];
        let k = RADII.iter().position(|&n| r < n as f32).unwrap_or(5);
        let n = RADII[k] as f32;
        let rgb = match self.mode {
            2 => [1.0; 3],
            3 => [1.0, 0.0, 0.0],
            4 => [45.0 / 255.0, 19.0 / 255.0, 0.0],
            5 => [246.0 / 255.0, 244.0 / 255.0, 232.0 / 255.0],
            6 => [1.0, 232.0 / 255.0, 210.0 / 255.0],
            _ => [0.0; 3],
        };
        let s = CANVAS_W as f32 / 960.0;
        let (cx, cy) = ((l + rx) * 0.5 * s, (t + by) * 0.5 * s);
        let (sin, cos) = self.rot.to_radians().sin_cos();
        let clip = (self.rot == 0.0).then(|| self.corners());
        let mut out = Vec::new();
        // a rect of texture `tex` (texels x0, y0, x1, y1) or of the plain colour, top-left (x, y), size (w, h) in
        // 960x540 units, turned about the clip centre
        let mut quad = |tex: Option<usize>, x: f32, y: f32, w: f32, h: f32| {
            let p = |x: f32, y: f32| {
                let (dx, dy) = (x * s - cx, y * s - cy);
                [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos]
            };
            let (tex, uv) = match tex {
                Some(j) => {
                    // qword_572990: corners, then the 1-texel edge strips beside the centre
                    let m = n as u32;
                    let [x0, y0, x1, y1] = match j & 15 {
                        0 => [0, 0, m, m],
                        1 => [m - 1, 0, m, m],
                        2 => [m, 0, 2 * m, m],
                        3 => [0, m - 1, m, m],
                        4 => [m, m - 1, 2 * m, m],
                        5 => [0, m, m, 2 * m],
                        6 => [m - 1, m, m, 2 * m],
                        _ => [m, m, 2 * m, 2 * m],
                    };
                    (j >> 4, [x0, y0, x1, y1].map(|v| v as f32 / (2.0 * n)))
                }
                None => (usize::MAX, [0.0, 0.0, 1.0, 1.0]),
            };
            let tex = border.get(tex).unwrap_or(white).clone();
            let [u0, v0, u1, v1] = uv;
            out.push(Quad {
                pos: [p(x, y), p(x + w, y), p(x + w, y + h), p(x, y + h)],
                uv: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                clip,
                blend: Blend::Over,
                ..Quad::rect(tex, [0.0; 4], [rgb[0], rgb[1], rgb[2], 1.0])
            });
        };
        // texture (normal k, `_inv` k + 6) and piece, packed for `quad`
        let (gr, inv) = (k << 4, (k + 6) << 4);
        if self.rot == 0.0 {
            let rl = if l + r <= 0.0 || l == 0.0 && w == 960.0 { 0.0 } else { r };
            let rr = if rx - r >= 960.0 || l == 0.0 && w == 960.0 { 0.0 } else { r };
            let rt = if t + r <= 0.0 || t == 0.0 && h == 540.0 { 0.0 } else { r };
            let rb = if by - r >= 540.0 || t == 0.0 && h == 540.0 { 0.0 } else { r };
            for (on, x, y, piece, ix, iy) in [
                (rt > 0.0 && rl > 0.0, l, t, 0, l + r, t + r),
                (rb > 0.0 && rl > 0.0, l, by - r, 5, l + r, by - 2.0 * r),
                (rr > 0.0 && rt > 0.0, rx - r, t, 2, rx - 2.0 * r, t + r),
                (rb > 0.0 && rr > 0.0, rx - r, by - r, 7, rx - 2.0 * r, by - 2.0 * r),
            ] {
                if on {
                    quad(None, x, y, r, r);
                    quad(Some(inv | piece), ix, iy, r, r);
                }
            }
            let (along, down) = (w - 2.0 * rl - 2.0 * rr, h - 2.0 * rt - 2.0 * rb);
            if rt > 0.0 {
                quad(None, l + rl, t, w - rl - rr, rt);
                quad(Some(gr | 6), l + 2.0 * rl, t + rt, along, r);
            }
            if rb > 0.0 {
                quad(None, l + rl, by - rb, w - rl - rr, rb);
                quad(Some(gr | 1), l + 2.0 * rl, by - 2.0 * rb, along, r);
            }
            if rl > 0.0 {
                quad(None, l, t + rt, rl, h - rt - rb);
                quad(Some(gr | 4), l + rl, t + 2.0 * rt, r, down);
            }
            if rr > 0.0 {
                quad(None, rx - rr, t + rt, rr, h - rt - rb);
                quad(Some(gr | 3), rx - 2.0 * rr, t + 2.0 * rt, r, down);
            }
        } else {
            // sub_B2040 anchors (unk_3C9810 / 3C9840): the piece's fraction placed on the point
            let (x0, x1, y0, y1) = (l + r * 0.5, rx - r * 0.5, t + r * 0.5, by - r * 0.5);
            let mut at = |piece: usize, (px, py): (f32, f32), (ax, ay): (f32, f32), w: f32, h: f32| {
                quad(Some(piece), px - ax * w, py - ay * h, w, h);
            };
            for (p, g, ga, i, ia) in [
                ((x0, y0), 0, (1.0, 1.0), 0, (0.0, 0.0)),
                ((x0, y1), 5, (1.0, 0.0), 5, (0.0, 1.0)),
                ((x1, y0), 2, (0.0, 1.0), 2, (1.0, 0.0)),
                ((x1, y1), 7, (0.0, 0.0), 7, (1.0, 1.0)),
            ] {
                at(gr | g, p, ga, r, r);
                at(inv | i, p, ia, r, r);
            }
            let mid = ((l + rx) * 0.5, (t + by) * 0.5);
            for y in [y0, y1] {
                at(gr | 1, (mid.0, y), (0.5, 1.0), w - 2.0 * r, 2.0 * r);
                at(gr | 6, (mid.0, y), (0.5, 0.0), w - 4.0 * r, r);
            }
            for x in [x0, x1] {
                at(gr | 3, (x, mid.1), (1.0, 0.5), 2.0 * r, h - 2.0 * r);
                at(gr | 4, (x, mid.1), (0.0, 0.5), r, h - 4.0 * r);
            }
        }
        out
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

/// The angle (degrees) sub_36520 turns a clip by for ROT2 `rot2`: -32768 follows the layer's own rotation `rot`
/// (with its base's), normalised to +-180.
fn clip_spin(rot2: i16, rot: f32) -> f32 {
    match rot2 {
        0 => 0.0,
        i16::MIN => {
            let d = (rot * 100.0) as i32 % 36000;
            let d = if d > 18000 { d - 36000 } else if d <= -18001 { d + 36000 } else { d };
            d.min(35999) as f32 / 100.0
        }
        v => f32::from(v) / 100.0,
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

/// Witch on the Holy Night's quake: one slot per layer instead of Tsukihime's two (STQK sub_438A0, STSW
/// sub_43770, STQS sub_44320; stepped by jp_3C970, drawn by sub_16210). Render +340 / +342 amplitudes, +344
/// period P, +346 frames left D (-1 forever), +348 delay, +350 counter, +352 / +356 offset, +360 type; the stage
/// keeps ax, ay, P, D, the delay and the starting D (words 4084 / 4164 / 4244 / 4324 / 4404 / 4484) and the
/// type (byte 4564), which a load makes the render slot again (sub_3E130). Times are frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct WitchQuake {
    /// 0 / 4 / 5 random jolts every P frames (4 ends at a click wait once no voice plays, 5 does not fade),
    /// 2 sine (P = cycle frames + 100 cycles over the lifetime, fading with them), 3 a two-level square wave,
    /// 1 STSW's sway
    pub kind: i32,
    /// amplitudes in 1024x576 units; a negative ax follows layer 1's or layer 0's offset. The sway keeps its
    /// state in ax / ay (phase + 1000 amplitude) and P (the two periods)
    pub ax: i16,
    pub ay: i16,
    pub period: i16,
    pub left: i16,
    pub delay: i16,
    counter: i16,
    /// in 640x448 units
    pub offset: (f32, f32),
    /// the stage words: ax, ay, P, D, delay, starting D
    pub stage: [i16; 6],
    pub stage_kind: u8,
}

impl WitchQuake {
    /// STQK's slot (sub_438A0, one layer): `period_ms` (34 when absent), `dur_ms` (-1 forever), `delay` in
    /// frames; P = 3 period / 50 frames (at least 1 for the types 2..5), the types 2 / 3 add 100 x the cycles
    /// that fit the duration (the cycle then at most 99). Types other than 2..5 are 0.
    pub fn new(kind: i32, ax: i16, ay: i16, period_ms: i32, dur_ms: i32, delay: i16) -> Self {
        let f = 3 * period_ms / 50;
        let kind = if (2..=5).contains(&kind) { kind } else { 0 };
        let period = match kind {
            2 | 3 if period_ms <= dur_ms => f.clamp(1, 99) + 100 * dur_ms.checked_div(period_ms).unwrap_or(0),
            2..=5 => f.max(1),
            _ => f.max(0),
        } as i16;
        let left = if dur_ms < 0 { -1 } else { (3 * dur_ms / 50) as i16 };
        Self {
            kind,
            ax,
            ay,
            period,
            left,
            delay,
            stage: [ax, ay, period, left, delay, left],
            stage_kind: kind as u8,
            ..Self::default()
        }
    }

    /// STSW's sway (sub_43770(layer, 0, 0, 0, -1, 0, 1)): type 1, P 1, forever.
    pub fn sway() -> Self {
        Self { kind: 1, period: 1, left: -1, stage: [0, 0, 1, -1, 0, -1], stage_kind: 1, ..Self::default() }
    }

    /// The slot a load rebuilds from the stage words (jp_5BAF0 / sub_3E130); counter and offset start at 0.
    pub fn restore(kind: u8, stage: [i16; 6]) -> Self {
        let [ax, ay, period, left, delay, _] = stage;
        Self { kind: i32::from(kind), ax, ay, period, left, delay, stage, stage_kind: kind, ..Self::default() }
    }

    /// STQS: everything but the type (render +360, stage byte 4564) is zeroed.
    pub fn stop(&mut self) {
        *self = Self { kind: self.kind, stage_kind: self.stage_kind, ..Self::default() };
    }

    /// STCH without a source layer zeroes the stage words, D and the delay; the render amplitudes, P and the
    /// offset stay, so a quaking layer keeps its last offset.
    pub fn stch_clear(&mut self) {
        self.left = 0;
        self.delay = 0;
        self.stage = [0; 6];
        self.stage_kind = 0;
    }

    /// One frame of jp_3C970's quake. `lead`: layer 0's and layer 1's offsets where drawn, for a follower;
    /// `talk_done`: no voice plays and the message waits for a click (sub_9C400 == 0, AE04B8 0x400).
    #[allow(clippy::approx_constant)]
    fn step(&mut self, rng: &mut crate::fx::Rng, lead: [Option<(f32, f32)>; 2], talk_done: bool) {
        if self.delay >= 1 {
            self.delay = (self.delay - 1).max(0);
            self.stage[4] = self.delay;
            return;
        }
        if self.left == 0 {
            return;
        }
        if self.left >= 1 {
            self.left -= 1;
            if self.left <= 0 {
                *self = Self { stage: [0, 0, 0, 0, 0, self.stage[5]], ..Self::default() };
                return;
            }
        }
        self.stage[3] = self.left;
        self.counter -= 1;
        const X: f32 = 640.0 * 0.00097656;
        const Y: f32 = 448.0 / 576.0;
        match self.kind {
            2 | 3 => {
                let m = self.period % 100;
                let r = self.period / 100 * m;
                if self.counter <= 0 {
                    self.counter = m;
                }
                let k = if self.kind == 2 && r >= 1 { f32::from(self.left) / f32::from(r) } else { 1.0 };
                let mut phase = f32::from(m - self.counter) * 3.1416 / f32::from(m);
                if self.kind == 3 {
                    phase = if phase < 1.5708 { 0.0 } else { 0.7854 };
                }
                let s = (phase + phase).sin();
                let amp = |a: i16| f32::from(a.signum()) * k * f32::from(a.unsigned_abs());
                self.offset = (amp(self.ax) * s * X, amp(self.ay) * s * Y);
            }
            1 => self.step_sway(rng),
            4 if talk_done => *self = Self { stage: [0, 0, 0, 0, 0, self.stage[5]], ..Self::default() },
            _ => {
                if self.counter > 0 {
                    return;
                }
                self.counter = self.period;
                if self.ax < 0 {
                    let mut o = (f32::from(self.ax), f32::from(self.ay));
                    for l in lead.into_iter().flatten() {
                        o = l;
                    }
                    self.offset = o;
                    return;
                }
                let mut r = |a: i16| if a != 0 { (rng.f() * 2.0 - 1.0) * f32::from(a) } else { 0.0 };
                let (mut x, mut y) = (r(self.ax) * X, r(self.ay) * Y);
                let e = self.stage[5];
                if e >= 1 && self.kind != 5 {
                    let (e, d) = (f32::from(e), f32::from(self.left));
                    let k = if e > 120.0 { (d / (e * 0.5).max(e - 60.0)).min(1.0) } else { d / e };
                    x *= k;
                    y *= k;
                }
                self.offset = (x, y);
            }
        }
    }

    /// STSW's sway (the type 1 branch): per axis a phase counting down a period of 60..119 frames and an
    /// amplitude of 5..9 (1024x576 units; y at 0.3), rolled anew when the phase runs out, the sign kept; the
    /// state lives in ax / ay (phase + 1000 amplitude, signed) and P ((x period - 60) + 100 (y period - 60)).
    /// The offset follows sin(phase) only on some frames: every 4th phase step below |sin| 0.5, every 3rd up to
    /// 0.8, and above that every other one until 0.96.
    #[allow(clippy::approx_constant)]
    fn step_sway(&mut self, rng: &mut crate::fx::Rng) {
        // the counter is held at 10000, so it runs out only on the first frame
        let fresh = self.counter <= 0;
        self.counter = 10000;
        let mut u = || rng.rand() as f32 * 4.6566e-10;
        let (sx, sy, mut ax, mut ay, mut px, mut py) = if fresh {
            let sx = if u() < 0.5 { 1 } else { -1 };
            let sy = if u() < 0.5 { 1 } else { -1 };
            (sx, sy, 0, 0, 0, 0)
        } else {
            let (ax, ay) = (i32::from(self.ax) / 1000, i32::from(self.ay) / 1000);
            let phase = |a: i16| (i32::from(a.unsigned_abs()) % 1000 - 1).max(0);
            (ax.signum(), ay.signum(), ax, ay, phase(self.ax), phase(self.ay))
        };
        let mut per_y = i32::from(self.period) / 100 + 60;
        let per_x;
        if px != 0 {
            per_x = i32::from(self.period) % 100 + 60;
        } else {
            px = (u() * 60.0) as i32 + 60;
            ax = ((u() * 5.0) as i32 + 5) * sx;
            per_x = px;
        }
        if py == 0 {
            py = (u() * 60.0) as i32 + 60;
            ay = ((u() * 5.0) as i32 + 5) * sy;
            per_y = py;
        }
        let a = py as f32 * 6.2832 / per_y as f32;
        let s = a.sin().abs();
        let update = if s < 0.5 {
            py & 3 == 0
        } else if s >= 0.8 {
            py & 1 == 0 || s >= 0.96
        } else {
            py % 3 == 0
        };
        if update {
            self.offset = (
                (px as f32 * 6.2832 / per_x as f32).sin() * ax as f32 * 640.0 * 0.00097656,
                ay as f32 * 0.3 * a.sin() * 448.0 / 576.0,
            );
        }
        let pack = |p: i32, a: i32| (if a < 0 { -(p + 1000 * a.abs()) } else { p + 1000 * a.abs() }) as i16;
        self.ax = pack(px, ax);
        self.ay = pack(py, ay);
        self.period = (per_x + 100 * per_y - 6060) as i16;
    }
}

impl Layer {
    /// The drawn alpha as jp_3C970 keeps it in stage word 2324: (int)((target + offset) x 255).
    pub fn drawn_alpha(&self) -> i16 {
        (self.alpha.value() * 255.0 + 1.0e-3) as i32 as i16
    }

    /// sub_3E130 -> sub_13ED0 (Witch on the Holy Night): while the STGS frames word is set the alpha tween is
    /// rebuilt from the drawn alpha.
    pub fn restart_fade(&mut self) {
        let cur = self.drawn_alpha();
        self.rebuild_fade(cur);
    }

    /// sub_13ED0: amplitude start - to, progress |to - drawn| / |to - start| (4.12, saturating, NaN 0) and a
    /// step of -4096 / frames, so a linear fade goes on where it is.
    pub fn rebuild_fade(&mut self, cur: i16) {
        let [from, frames] = self.wfade;
        if frames < 1 {
            return;
        }
        let to = (self.alpha.to * 255.0).round() / 255.0;
        let (from, cur) = (f32::from(from) / 255.0, f32::from(cur) / 255.0);
        let ratio = (to - cur).abs() / (to - from).abs();
        self.alpha = Tween {
            to,
            diff: from - to,
            prog: (ratio * 4096.0) as i32 as i16,
            step: (-4096.0 / f32::from(frames)) as i32 as i16,
            ease: i32::from(self.wfade_ease),
        };
    }

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
            staged: Staged::default(),
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
            stage_pivot: None,
            stage_shift: None,
            stage_rot: None,
            quake: [Quake::default(); 2],
            wquake: WitchQuake::default(),
            snx: [1.0; 2],
            snx4: [0.0; 2],
            wfade: [255, 0],
            wfade_ease: 0,
            haze: Haze::default(),
            clip: None,
        }
    }

    /// The stage block's priority, STBR mode, ROT2, SACL flag, SBAS base and SOXY offset: what the next commit
    /// draws, what saves keep and what the commands that read the stage see.
    pub fn stage_prio(&self) -> i32 {
        self.staged.prio.unwrap_or(self.prio)
    }

    pub fn stage_blend(&self) -> (i32, Blend) {
        self.staged.stbr.unwrap_or((self.stbr, self.blend))
    }

    pub fn stage_rot2(&self) -> i16 {
        self.staged.rot2.unwrap_or(self.rot2)
    }

    pub fn stage_absolute(&self) -> bool {
        self.staged.absolute.unwrap_or(self.absolute)
    }

    pub fn stage_base(&self) -> Option<usize> {
        self.staged.base.unwrap_or(self.base)
    }

    pub fn stage_soxy(&self) -> (i16, i16) {
        self.staged.soxy.unwrap_or(self.soxy)
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
    /// waited on. A motion runs (sub_9598C) while it has no loop point (`looping`, sub_999A0), is short of its
    /// length and is not paused: a looping motion is never waited on.
    pub fn animating(&self, mask: i32, looping: &dyn Fn(i32) -> bool) -> bool {
        (mask & 0x20 != 0 && self.motion.as_ref().is_some_and(|m| !looping(m.id) && !m.finished() && !m.paused))
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

/// One step of the stage pass (`Stage::draws`). The layer group path draws some layers into the exe's
/// "temp buffer surface" pair instead of the stage: `Pixel` (pixel, off_AE0CC8) and `Mask` (alpha,
/// off_AE0CD0). Both persist across frames and are cleared only by a group's first draw.
pub enum Draw {
    Stage(Quad),
    Pixel { quad: Quad, clear: bool },
    Mask { quad: Quad, clear: bool },
    /// sub_37EA0: the pixel surface, its alpha multiplied by the mask's, drawn over the whole stage with the
    /// saved blend at the saved alpha
    Composite { blend: Blend, alpha: f32 },
}

/// The layer group state of the draw pass: mode / first / flag (off_AE0D18 / D20 / D28) and what the first
/// draw saved (blend off_AE0D30, alpha off_AE0D40). The saved pair is never reset.
#[derive(Default)]
struct Group {
    mode: u8,
    first: bool,
    blend: Blend,
    alpha: u8,
    /// the last frame's summary, so the debug log shows changes only
    log: String,
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
    /// Render +280 per slot, the rule alpha fades wipe through: only the STTR handler writes it, and only into
    /// a slot that shows an image (`shown`); STCC and a start or load through sub_3C550 reset it, while STCH,
    /// the commit, a rewind and the seek leave it
    pub rules: Vec<Option<RenderRule>>,
    /// Render +12 per slot: the slot held an image at the last commit (sub_3C970)
    pub shown: Vec<bool>,
    /// Stage word 0x183C, the backdrop colour index (Witch on the Holy Night's jp_48AC0 sets it each story
    /// frame: 0 black in `savedata::witch_dark_scene`, else 1 white; Tsukihime: always 0), and the colour the
    /// commit (sub_3E130 -> sub_13A60, off_6327E0) handed to the render, which sub_206C0 fills the screen with
    pub backdrop: u8,
    pub backdrop_drawn: u8,
    /// The rule wipes' edge width (`RuleMask::soft`): Tsukihime's fixed 128, or Witch on the Holy Night's
    /// dword_612C88 (64 at boot; FADS's fifth and STTR's fourth argument, sub_7EDA0 taking 64 for <= 0 or >= 255),
    /// which nothing saves or resets
    pub rule_soft: f32,
    /// the quake slots' random source and unspent time for their 60 Hz steps
    quake_rng: crate::fx::Rng,
    /// no voice plays and the message waits for a click: Witch on the Holy Night's type 4 quake ends
    pub talk_done: bool,
    frame_acc: f32,
    group: Group,
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
            rules: vec![None; LAYERS],
            shown: vec![false; LAYERS],
            backdrop: 0,
            backdrop_drawn: 0,
            rule_soft: if crate::assets::witch() { 64.0 } else { 128.0 },
            quake_rng: crate::fx::Rng::default(),
            talk_done: false,
            frame_acc: 0.0,
            group: Group::default(),
        }
    }

    /// STNS (sub_35FC0), once per frame: the quad that paints the noise into render target 40
    /// (canvas sized, never cleared). Noise texture 390 (noisehd1) when n%10==2, else 389 (noisehd0),
    /// scaled 0.6 in a 960x540 space (1.2x at canvas size) at a fresh random offset of -rand(0, 320) / 4 x
    /// -rand(0, 180) / 4 units, mirrored horizontally on alternate ticks (each frame when n>2, else every
    /// third). The strip the shifted texture misses keeps earlier frames' noise, as in the exe. Grain layers
    /// then show RT40 in place of their image. All grain layers share RT40 like the exe; the first one's
    /// parameters drive it.
    ///
    /// Witch on the Holy Night (jp_35FC0, render +364..+372 stepped by jp_3C970): scale 0.9375 (1.875x), a tick
    /// every 9 frames for n 11 / 12, 6 for 21 / 22 / 31 / 32, else 3, and the offset -rand(0, 896) / 4 x
    /// -rand(0, 504) / 4 drawn only at a tick (held between ticks).
    pub fn noise_pass(&self) -> Option<Quad> {
        let nt = self.noise.as_ref()?;
        let n = self.layers.iter().flatten().find_map(|l| l.noise.filter(|_| !l.pending))?;
        let t = &nt[usize::from(n % 10 == 2)];
        let witch = crate::assets::witch();
        let period = match (witch, n) {
            (true, 11 | 12) => 9,
            (true, 21 | 22 | 31 | 32) => 6,
            (true, _) => 3,
            (false, _) => if n > 2 { 1 } else { 3 },
        };
        let tick = self.frame / period;
        // a fresh offset each frame in Tsukihime, each tick in Witch on the Holy Night
        let seed = if witch { tick } else { self.frame };
        let rnd = |k: u32| {
            let mut v = seed.wrapping_mul(0x9E37_79B9) ^ k.wrapping_mul(0x85EB_CA6B);
            v ^= v >> 15;
            v = v.wrapping_mul(0x2C1B_3C6D);
            v ^= v >> 12;
            (v & 0xFFFF) as f32 / 65535.0
        };
        let k = CANVAS_W as f32 / 1920.0;
        // offsets in the 960x540 space (2 px each), the quad scaled there
        let (rx, ry, scale) = if witch { (896.0, 504.0, 0.9375) } else { (320.0, 180.0, 0.6) };
        let (x0, y0) = (-rnd(1) * rx / 4.0 * 2.0 * k, -rnd(2) * ry / 4.0 * 2.0 * k);
        let (tw, th) = (t.width as f32 * scale * 2.0 * k, t.height as f32 * scale * 2.0 * k);
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

    /// sub_5D3A0 (FADS, the end of the next-scene seek): the stage becomes what is drawn (sub_32300 / sub_32BB0:
    /// the render shift and pivot from the stage word and anchor), and sub_31F30(layer, target, target, 0
    /// frames) lands each layer's zoom tween on the stage's zoom target (kind 0).
    pub fn commit(&mut self) {
        self.backdrop_drawn = self.backdrop;
        for (shown, l) in self.shown.iter_mut().zip(&self.layers) {
            *shown = l.is_some();
        }
        for l in self.layers.iter_mut().flatten() {
            l.pending = false;
            let s = std::mem::take(&mut l.staged);
            if let Some((m, b)) = s.stbr {
                (l.stbr, l.blend) = (m, b);
            }
            l.prio = s.prio.unwrap_or(l.prio);
            l.rot2 = s.rot2.unwrap_or(l.rot2);
            l.absolute = s.absolute.unwrap_or(l.absolute);
            l.base = s.base.unwrap_or(l.base);
            l.soxy = s.soxy.unwrap_or(l.soxy);
            l.pivot = l.stage_pivot;
            l.shift = l.stage_shift;
            if let Some(r) = l.stage_rot.take() {
                l.rotation.to = r;
            }
            for t in [&mut l.zoom_x, &mut l.zoom_y] {
                *t = Tween::new(t.to, t.to, 0, 0);
            }
            if crate::assets::witch() {
                l.restart_fade();
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
        if crate::assets::witch() {
            // jp_3C970: a follower takes layer 0's offset, then layer 1's, where drawn (render +12 == 1 and the
            // frame's drawn flag)
            for li in 0..self.layers.len() {
                let lead = |k: usize| self.layers[k].as_ref().filter(|l| !l.pending).map(|l| l.wquake.offset);
                let lead = [lead(0), lead(1)];
                if let Some(l) = self.layers[li].as_mut() {
                    l.wquake.step(&mut self.quake_rng, lead, self.talk_done);
                }
            }
            return;
        }
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
        if crate::assets::witch() {
            return (0.0, 0.0);
        }
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

    /// Witch on the Holy Night's quake offset of a layer drawn at `pos` (sub_16210), in canvas pixels: only
    /// with P >= 1 and an image other than BLACK (id >= 1); once on an axis where the turned quad reaches within
    /// 30 units of the left (30.167 of the top) edge, twice otherwise.
    fn witch_quake_px(l: &Layer, pos: &[[f32; 2]; 4]) -> (f32, f32) {
        let q = &l.wquake;
        let black = matches!(l.source, Source::Color(c) if c == [0.0; 3]);
        if q.period < 1 || q.offset == (0.0, 0.0) || black {
            return (0.0, 0.0);
        }
        let (sx, sy) = (CANVAS_W as f32 / 640.0, CANVAS_H as f32 / 448.0);
        let min_x = pos.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        let min_y = pos.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let kx = if min_x - 30.0 * sx <= 0.0 { 1.0 } else { 2.0 };
        let ky = if min_y - 30.167 * sy <= 0.0 { 1.0 } else { 2.0 };
        (q.offset.0 * kx * sx, q.offset.1 * ky * sy)
    }

    pub fn animating(&self, i: i32, mask: i32, looping: &dyn Fn(i32) -> bool) -> bool {
        if i < 0 {
            self.layers.iter().flatten().any(|l| l.animating(mask, looping))
        } else {
            self.layers
                .get(i as usize)
                .and_then(|l| l.as_ref())
                .is_some_and(|l| l.animating(mask, looping))
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
        };
        if let Some(b) = l.base.filter(|&b| b != i && b < LAYERS)
            && depth < 8
            && let Some(bl) = self.layers[b].as_ref()
        {
            let br = self.resolved(b, depth + 1);
            // anchor: the base's stage anchor (floats 221 / 261, the image centre unless STPV / a motion moved it) in
            // its own unzoomed script space; children of a sized-down image (level 1..3) scale by its level
            // (sub_89190 / sub_892B0)
            let (ax, ay, k) = match &bl.source {
                Source::Image(img) => {
                    let [px, py] = bl.stage_pivot.unwrap_or(img.anchor());
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
        // sub_32E30 / sub_36520 turn the layer's clip mask by its ROT2 (word +316) about the clip centre
        if let Some(c) = &mut r.clip {
            c.rot = clip_spin(l.rot2, r.rot);
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
                    c.rot = clip_spin(rot2, r.rot);
                    c
                });
            }
        }
        r
    }

    /// sub_3BBA0 draws a layer (sub_37F50) only while its priority (render dword 5883 + layer) is >= 1, so
    /// STCP(l, 0) hides it; the rest go back to front by priority, ties by layer number
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.layers.len())
            .filter(|&i| self.layers[i].as_ref().is_some_and(|l| !l.pending && l.prio >= 1))
            .collect();
        order.sort_by_key(|&i| (self.layers[i].as_ref().unwrap().prio, i));
        order
    }

    /// Every shown layer drawn straight onto the stage (save previews, which never take the group path).
    pub fn quads(&self, white: &TexRef) -> Vec<Quad> {
        self.order().into_iter().filter_map(|i| self.layer_quad(i, white)).collect()
    }

    /// The stage pass with the group path (sub_3BBA0). Layers sharing a clip parent (STCH arg 11, render +272;
    /// a parent with children maps to itself; not the SBAS base, +276) that follow each other in draw order form a
    /// group: sub_A2BA0 gets mode 1 while the next drawn layer has the same parent, 2 for the last, 0 outside a
    /// group, and 0 -> 1 sets
    /// "first" (sub_9F020 resets mode 0 / first 1 every frame). sub_36520 raises the group flag (sub_A2BF0,
    /// mode >= 1 only) around the draws below; sub_37EA0 then composites at a group's last layer once
    /// something drew (first clear) and resets the mode.
    /// - Sprite draws (sub_9FE00: unclipped layers with the default shader) go onto the stage as usual and
    ///   add their alpha to the mask (blend state 0, alpha One / One; the group's first draw clears it).
    /// - Surface draws (sub_A4BC0: unclipped layers with any other shader, haze) go into the pixel surface
    ///   instead. The first is opaque (blend 0 / default shader, or noalpha with a clip inset), clears both
    ///   surfaces, saves its blend and alpha and puts its alpha into the mask; later ones keep their blend
    ///   and colour, reading the pixel surface as their backdrop.
    /// - A child of a shown parent (sub_36520 case 2, clipped to the parent's clip, sub_A6570) goes into the pixel
    ///   surface through the normal shader (blend state 7, shader 45: mix(backdrop premultiplied by its alpha, s, a), opaque) or
    ///   its own backdrop blend, in white at 255 alpha / the saved alpha (capped); the first instead draws
    ///   opaque white, clears both, saves and puts its alpha into the mask.
    /// - A clipped layer that is not a child (the group branch of sub_36520's clip path) is drawn through
    ///   noalpha (shader id 8) into the pixel surface likewise, then sub_A74B0 copies its rect (target 1,
    ///   also noalpha, inside the scissor) into the mask, clearing it, and saves its alpha.
    ///
    /// Only children and clipped layers end a group (LABEL_190), and a layer at alpha 0 (sub_37F50). A
    /// clipped layer whose clip lies off screen draws nothing and does not end one. sub_37EA0 first paints the
    /// parent's clip inset band (sub_34A70(layer, -3), clip modes 1..6) into the pixel surface; it lies outside
    /// the inset scissor the mask was drawn in, so the composite never shows it.
    pub fn draws(&mut self, white: &TexRef, border: &[TexRef]) -> Vec<Draw> {
        let order = self.order();
        // v69: each shown layer's parent; a parent with children maps to itself
        let mut key = [-1i32; LAYERS];
        for (i, l) in self.layers.iter().enumerate() {
            if let Some(l) = l
                && !l.pending
                && l.prio != 0
            {
                key[i] = l.parent.filter(|&b| b < LAYERS).map_or(-1, |b| b as i32);
            }
        }
        for i in 0..LAYERS {
            let b = key[i];
            if b >= 0 && key[b as usize] == -1 {
                key[b as usize] = b;
            }
        }
        self.group.mode = 0;
        self.group.first = true;
        let mut out = Vec::new();
        // debug log of the group draws (knife::group), printed when it changes
        let trace = tracing::enabled!(target: "knife::group", tracing::Level::DEBUG);
        let mut log = Vec::new();
        for (n, &i) in order.iter().enumerate() {
            let before = out.len();
            let g = key[i];
            let mode = match order.get(n + 1) {
                _ if g < 0 => 0,
                Some(&next) if key[next] == g => 1,
                _ => 2,
            };
            if mode == 1 && self.group.mode == 0 {
                self.group.first = true;
            }
            self.group.mode = mode;
            let Some(quad) = self.layer_quad(i, white) else {
                self.end_group(&mut out, i, white, border);
                if trace && mode != 0 {
                    log.push(format!("{i} (mode {mode}): alpha 0{}", if out.len() > before { ", composite" } else { "" }));
                }
                continue;
            };
            let l = self.layers[i].as_ref().unwrap();
            let alpha = self.resolved(i, 0).alpha.clamp(0.0, 1.0);
            let abyte = (alpha * 255.0) as u8;
            // sub_37F50 passes 2 (a child) when the parent is shown
            let parent = l.parent.filter(|&b| b != i && b < LAYERS && self.layers[b].as_ref().is_some_and(|b| !b.pending));
            // case 2 clips to the parent's clip, the default path to the layer's own
            let shake = self.clip_shake(l);
            let clip = match parent {
                Some(b) => self.resolved(b, 0).clip,
                None => self.resolved(i, 0).clip,
            }
            .filter(|c| c.w != 0.0 && c.h != 0.0)
            .map(|mut c| {
                (c.x, c.y) = (c.x + shake.0, c.y + shake.1);
                c
            });
            let on_screen = clip.is_none_or(|c| c.x < CANVAS_W as f32 && c.y < CANVAS_H as f32 && c.x + c.w > 0.0 && c.y + c.h > 0.0);
            if !on_screen {
                if trace && mode != 0 {
                    log.push(format!("{i} (mode {mode}): off screen"));
                }
                continue;
            }
            let flag = mode >= 1;
            let haze = quad.haze[0][3] > 0.5;
            // STNS grain (+420): sub_35FC0 draws it without the flag; a child's skips sub_37EA0 too
            if l.noise.is_some() && (parent.is_some() || clip.is_none()) {
                out.push(Draw::Stage(quad));
                continue;
            }
            match (parent.is_some(), clip.is_some()) {
                (_, true) if flag => {
                    let c = clip.unwrap_or_default();
                    self.clip_draw(&mut out, quad, l.blend, abyte, parent.is_none(), haze, c, white);
                }
                (true, false) | (false, false) if !flag => out.push(Draw::Stage(quad)),
                // the per-layer rule fade (sub_A6E90) bypasses the group
                (false, false) if self.rules[i].is_some() && alpha < 1.0 && quad.blend == Blend::Alpha => out.push(Draw::Stage(quad)),
                (_, false) if quad.blend != Blend::Alpha || haze => {
                    let inset = if haze { l.clip_inset } else { 0 };
                    self.surface_draw(&mut out, quad, l.blend, abyte, inset);
                }
                (_, false) => {
                    let first = std::mem::take(&mut self.group.first);
                    out.push(Draw::Mask { quad: footprint(&quad, quad.color[3]), clear: first });
                    out.push(Draw::Stage(quad));
                }
                (_, true) => {
                    let c = clip.unwrap_or_default();
                    match c.band(border, white) {
                        band if band.is_empty() => out.push(Draw::Stage(quad)),
                        band => band_draw(&mut out, quad, c, band, white),
                    }
                }
            }
            if parent.is_some() || clip.is_some() {
                self.end_group(&mut out, i, white, border);
            }
            for d in out[before..].iter().filter(|_| trace) {
                match d {
                    Draw::Pixel { clear, .. } => log.push(format!("{i} (mode {mode}): pixel{}", if *clear { " (clear)" } else { "" })),
                    Draw::Composite { blend, alpha } => log.push(format!("{i}: composite {blend:?} {alpha:.3}")),
                    _ => {}
                }
            }
        }
        if trace {
            let sum = log.join(", ");
            if sum != self.group.log {
                tracing::debug!(target: "knife::group", "{}", sum);
                self.group.log = sum;
            }
        }
        out
    }

    /// sub_A4BC0 under the group flag.
    fn surface_draw(&mut self, out: &mut Vec<Draw>, mut quad: Quad, blend: Blend, abyte: u8, inset: i16) {
        if std::mem::take(&mut self.group.first) {
            quad.color[3] = 1.0;
            (quad.blend, quad.shade) = if inset < 1 { (Blend::Over, Shade::Sprite) } else { (Blend::Opaque, Shade::NoAlpha) };
            self.group.blend = blend;
            self.group.alpha = abyte;
            out.push(Draw::Mask { quad: footprint(&quad, 1.0), clear: true });
            out.push(Draw::Pixel { quad, clear: true });
        } else {
            out.push(Draw::Pixel { quad, clear: false });
        }
    }

    /// sub_A6570 under the group flag (and sub_A74B0 after a clipped layer that is not a child).
    #[allow(clippy::too_many_arguments)]
    fn clip_draw(
        &mut self,
        out: &mut Vec<Draw>,
        mut quad: Quad,
        blend: Blend,
        abyte: u8,
        own: bool,
        haze: bool,
        clip: ClipRect,
        white: &TexRef,
    ) {
        let first = std::mem::take(&mut self.group.first);
        // sub_36520 LABEL_101: the target 1 redraw below runs at colour alpha v280 (the layer's alpha), or 1 when
        // sub_3C10 finds a render colour (+460: a multiply colour other than white or an add colour)
        let tinted = quad.shade != Shade::Fill && quad.color[..3] != [1.0; 3];
        let mask_alpha = if tinted { 1.0 } else { quad.color[3] };
        // that colour also reaches the pixel draw (a child's is its alpha times the parent's; sub_A74B0 saves the
        // layer's alpha afterwards)
        let drawn = if own && tinted { 255 } else { u32::from(abyte) };
        let a = if first {
            1.0
        } else if self.group.alpha == 0 {
            0.0
        } else {
            (255 * drawn / u32::from(self.group.alpha)).min(255) as f32 / 255.0
        };
        if quad.shade != Shade::Fill {
            quad.color = [1.0, 1.0, 1.0, a];
        } else {
            quad.color[3] = a;
        }
        if quad.blend == Blend::Alpha {
            quad.blend = Blend::Ps(knife_render::PsOp::Normal);
        }
        // a rule fade and the soft clip only shape target 1 (sub_A7190, sub_32E30), i.e. the mask; the pixels get
        // the scissor alone
        let rule = quad.rule.take();
        let soft = quad.soft.take();
        // with ROT2 (word +316) sub_36520 sets no scissor: the turned mask alone shapes the layer; a child's
        // target 1 is then texture slot 343 (alphagradation512)'s centre texels 250..260, a flat white at alpha
        // 252, stretched over the clip and turned by ROT2 (sub_B7010; sub_B0480 gets ROT2 in 1/65536 turns)
        let turned = (clip.rot != 0.0).then(|| quad.clip.take());
        let first_mask = match turned {
            Some(clip) if !own => {
                let mut f = footprint(&quad, 252.0 / 255.0);
                f.clip = clip;
                f
            }
            _ => footprint(&quad, 1.0),
        };
        // sub_36520 then redraws target 1 for sub_A74B0 to copy into the mask: with a clip inset sub_32E30's soft
        // inset rectangle (a clip of the whole screen at 0, 0 skips that and sub_A74B0), else the layer's image
        // through the default shader (id 40, blend state 4, so its alpha; 38 on the haze path) or the rule wipe
        let mask = match own {
            false => None,
            true if clip.inset != 0 => {
                let r = clip.inset_rect();
                let whole = r.x == 0.0 && r.y == 0.0 && r.w >= CANVAS_W as f32 && r.h >= CANVAS_H as f32;
                (!whole).then(|| {
                    let mut rect = Quad::rect(white.clone(), [0.0; 4], [1.0; 4]);
                    rect.pos = clip.corners();
                    rect.clip = quad.clip;
                    rect.soft = soft;
                    rect.blend = Blend::Opaque;
                    rect
                })
            }
            true => {
                // sub_A7190's rule wipe already takes the alpha as its colour (RuleMask)
                let mut rect = footprint(&quad, if rule.is_some() { 1.0 } else { mask_alpha });
                rect.haze = quad.haze;
                rect.blend = Blend::Opaque;
                rect.rule = rule;
                Some(rect)
            }
        };
        // the layer's own image goes into target 2 through noalpha (id 8; 38 on the haze path)
        if own && !haze {
            quad.shade = Shade::NoAlpha;
        }
        if first {
            self.group.blend = blend;
            self.group.alpha = abyte;
            out.push(Draw::Mask { quad: first_mask, clear: true });
        }
        if let Some(rect) = mask {
            out.push(Draw::Mask { quad: rect, clear: true });
            self.group.alpha = abyte;
        }
        out.push(Draw::Pixel { quad, clear: first });
    }

    /// sub_37EA0, at the group's last layer `i`: its clip band (ClipRect::band) into the pixel surface, then the
    /// composite
    fn end_group(&mut self, out: &mut Vec<Draw>, i: usize, white: &TexRef, border: &[TexRef]) {
        if self.group.mode == 2 && !self.group.first {
            let band = self.band_clip(i).map(|c| c.band(border, white)).unwrap_or_default();
            out.extend(band.into_iter().map(|quad| Draw::Pixel { quad, clear: false }));
            out.push(Draw::Composite { blend: self.group.blend, alpha: f32::from(self.group.alpha) / 255.0 });
            self.group.mode = 0;
        }
    }

    /// The screen shake sub_32050 adds to layer `l`'s clip (none for an exempt image or a colour fill), in canvas
    /// pixels: sub_34440's twice the 640x448 offset.
    fn clip_shake(&self, l: &Layer) -> (f32, f32) {
        match &l.source {
            Source::Image(img) if !img.no_shake => {
                (2.0 * self.shake.0 * CANVAS_W as f32 / 640.0, 2.0 * self.shake.1 * CANVAS_H as f32 / 448.0)
            }
            _ => (0.0, 0.0),
        }
    }

    /// sub_37EA0's clip for the band of a group's last layer `i`: its parent's clip (+276) and STRR (+316) copied
    /// over, with the inset and mode sub_37F50 left it (the parent's unless SACL-absolute), shaken by sub_32050.
    fn band_clip(&self, i: usize) -> Option<ClipRect> {
        let l = self.layers[i].as_ref()?;
        let parent = l.parent.filter(|&p| p != i && p < LAYERS && self.layers[p].is_some());
        let mut c = match parent {
            Some(p) if l.absolute => {
                let mut c = self.resolved(p, 0).clip?;
                (c.inset, c.mode) = (l.clip_inset, l.clip_mode);
                c
            }
            _ => self.resolved(i, 0).clip?,
        };
        let (sx, sy) = self.clip_shake(l);
        (c.x, c.y) = (c.x + sx, c.y + sy);
        (c.w != 0.0 && c.h != 0.0).then_some(c)
    }

    /// Layer `i` as drawn onto the stage, or None while its alpha is 0.
    fn layer_quad(&self, i: usize, white: &TexRef) -> Option<Quad> {
        {
            let l = self.layers[i].as_ref()?;
            let Resolved { x, y, alpha, clip, zoom: (zx, zy), rot } = self.resolved(i, 0);
            if alpha <= 0.0 {
                return None;
            }
            let (mut cx, mut cy) = script_to_px(x, y);
            // sub_34440 adds the screen shake twice (640x448 units) unless the image is exempt (+65 bit 1, the
            // colour fills) or the layer clips with a parent (+272) and is not absolute; sub_32050 shifts the
            // clip scissor the same way unless the image is exempt. Such a child is placed from its parent's
            // clip origin by sub_37F50, which is sub_32050 of the parent and so carries the parent's shake.
            let exempt = |l: &Layer| match &l.source {
                Source::Image(img) => img.no_shake,
                Source::Color(_) => true,
            };
            let full = (2.0 * self.shake.0 * CANVAS_W as f32 / 640.0, 2.0 * self.shake.1 * CANVAS_H as f32 / 448.0);
            let shake = self.clip_shake(l);
            let clip = clip.map(|mut c| {
                c.x += shake.0;
                c.y += shake.1;
                c
            });
            let parent = l.parent.filter(|&p| p != i && p < LAYERS).and_then(|p| self.layers[p].as_ref());
            match parent {
                Some(pl) if clip.is_some() && !l.absolute => {
                    if pl.clip.is_some() && !exempt(pl) {
                        cx += full.0;
                        cy += full.1;
                    }
                }
                _ => {
                    cx += shake.0;
                    cy += shake.1;
                }
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
                    // sub_34440: the quad turns about the pivot and is offset by pivot + shift. Witch on the Holy
                    // Night's (sub_16210) offsets it by pivot - the ImageInfo centre instead: the image's centre
                    // sits on the position whatever the shift words hold, and it turns about the pivot.
                    let centre = img.anchor();
                    let piv = l.pivot.unwrap_or(centre);
                    let sh = if crate::assets::witch() {
                        [-img.center_x, -img.center_y]
                    } else {
                        l.shift.unwrap_or([-centre[0], -centre[1]])
                    };
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
            // ROT2 turns only the clip mask (sub_32E30 / sub_36520), never the layer or its children
            let corner = |px: f32, py: f32| {
                let dx = (px - ox) * zx;
                let dy = (py - oy) * zy;
                [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos]
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
            let rule = self.rules[i].as_ref().map(|r| knife_render::RuleMask {
                // the shader compares c3[0x3c] with 1 / 2 / 3: 1 and 3 mirror x, 2 and 3 mirror y
                flip: if r.flip <= 3 { r.flip } else { 0 },
                soft: self.rule_soft,
                ..knife_render::RuleMask::new(r.tex.clone(), alpha.clamp(0.0, 1.0))
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
            let mut pos = [corner(0.0, 0.0), corner(w, 0.0), corner(w, h), corner(0.0, h)];
            if crate::assets::witch() {
                let q = Self::witch_quake_px(l, &pos);
                for p in &mut pos {
                    p[0] += q.0;
                    p[1] += q.1;
                }
            }
            Some(Quad {
                tex,
                pos,
                uv: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                color: [rgb[0], rgb[1], rgb[2], alpha.clamp(0.0, 1.0)],
                blend: l.blend,
                shade,
                // a 0-sized clip is no clip
                clip: clip.filter(|c| c.w != 0.0 && c.h != 0.0).map(|c| c.corners()),
                rule,
                haze,
                soft: clip.and_then(|c| c.soft()),
            })
        }
    }
}

/// sub_36520 for a clipped layer outside a group whose clip has a band: the layer into target 2 at alpha 1,
/// sub_34A70(layer, 2) over it, sub_32E30's mask into target 1 and target 2 composited through it at the layer's
/// alpha with its blend. A rule wipe or haze layer goes onto the stage with the band at its alpha over it.
fn band_draw(out: &mut Vec<Draw>, mut quad: Quad, clip: ClipRect, band: Vec<Quad>, white: &TexRef) {
    let alpha = quad.color[3];
    if quad.rule.is_some() || quad.haze[0][3] > 0.5 {
        out.push(Draw::Stage(quad));
        out.extend(band.into_iter().map(|mut q| {
            q.color[3] *= alpha;
            Draw::Stage(q)
        }));
        return;
    }
    let mut rect = Quad::rect(white.clone(), [0.0; 4], [1.0; 4]);
    rect.pos = clip.corners();
    rect.clip = quad.clip;
    rect.soft = quad.soft.take();
    rect.blend = Blend::Opaque;
    out.push(Draw::Mask { quad: rect, clear: true });
    // the default shader through blend state 4: a straight copy
    let blend = std::mem::replace(&mut quad.blend, Blend::Opaque);
    quad.color[3] = 1.0;
    out.push(Draw::Pixel { quad, clear: true });
    out.extend(band.into_iter().map(|quad| Draw::Pixel { quad, clear: false }));
    out.push(Draw::Composite { blend, alpha });
}

/// A quad's alpha as the group mask takes it: the default shader (texture alpha times `alpha`), no rule.
fn footprint(q: &Quad, alpha: f32) -> Quad {
    let mut f = q.clone();
    f.color = [1.0, 1.0, 1.0, alpha];
    f.blend = Blend::Footprint;
    if f.shade != Shade::NoAlpha {
        f.shade = Shade::Sprite;
    }
    f.rule = None;
    f.haze = Default::default();
    f
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
    fn commit_gives_the_render_layer_the_stage_anchor_shift_and_rotation() {
        // STPV leaves the render offset and rotation as they are; sub_5D3A0 then takes the shift word (int)-pivot
        // and the stage rotation, so the pivot lands on the layer's position
        let mut s = Stage::new();
        let mut l = Layer::new("WHITE".into(), Source::Color([1.0; 3]), 240.0, 136.0, 10);
        l.stage_pivot = Some([317.33, 179.67]);
        l.pivot = l.stage_pivot;
        l.stage_shift = Some([-317.0, -179.0]);
        l.stage_rot = Some(12.0);
        s.set(0, l);
        let l = s.get(0).unwrap();
        assert_eq!((l.shift, l.rotation.to), (None, 0.0));
        s.commit();
        let l = s.get(0).unwrap();
        assert_eq!((l.pivot, l.shift), (Some([317.33, 179.67]), Some([-317.0, -179.0])));
        assert_eq!((l.rotation.to, l.stage_rot), (12.0, None));
    }

    #[test]
    fn stage_only_commands_reach_the_drawn_layer_at_the_commit() {
        // STCP / STBR / STRR write only the stage block: the layer is drawn as before until sub_5D3A0
        let mut s = Stage::new();
        let mut l = Layer::new("WHITE".into(), Source::Color([1.0; 3]), 240.0, 136.0, 10);
        l.pending = false;
        s.set(3, l);
        let l = s.get(3).unwrap();
        l.staged.prio = Some(0);
        l.staged.stbr = Some((17, Blend::Add));
        l.staged.rot2 = Some(-500);
        assert_eq!((l.prio, l.stbr, l.rot2, l.stage_prio()), (10, -1, 0, 0));
        assert!(!s.shown[3]);
        s.commit();
        let l = s.get(3).unwrap();
        assert_eq!((l.prio, l.stbr, l.blend, l.rot2), (0, 17, Blend::Add, -500));
        assert!(l.staged.prio.is_none() && s.shown[3]);
    }

    #[test]
    fn every_ease_kind_spans_from_to() {
        for kind in 0..=42 {
            let a = crate::motion::ease(kind, 0.0);
            let b = crate::motion::ease(kind, 1.0);
            assert!(a.abs() < 2e-3 && (b - 1.0).abs() < 2e-3, "kind {kind}: {a} {b}");
            // the layer form (d = -1) starts at `from` for the in and out kinds; in-out and out-in halves swap
            if !(2..=41).contains(&kind) || (kind - 2) % 4 < 2 {
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
