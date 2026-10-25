//! The exe's 2D draw state and object system as Witch on the Holy Night's screens use them.
//!
//! - `Pen`: the sprite state the draw calls read: the coordinate space (jp_B0650), scale (jp_B0460), colour
//!   (jp_B03E0) and the anchor in the low nibble of a sprite's flags (unk_286C50 / unk_286C80: 0..8 = left /
//!   centre / right x top / middle / bottom). jp_B0920 / jp_B0D90 place a sprite at `kx * x` with its size
//!   `texels * kx * sx` (k = screen / space), scaled only with flag 0x10.
//! - `WObjs`: the 374 objects at jd_18C0C08 (136 bytes each): jp_ADC50 installs a table, jp_AE000 / jp_AE020
//!   place and scale them, jp_AE840 hit-tests the pointer and steps their frame states, jp_AEEC0 draws a group.

use knife_render::{Blend, Quad, TexRef};

use super::super::{H, W};

/// A frame of an object state (32 bytes): texture id, sprite flags (anchor nibble, 0x10 scale), blend,
/// offset from the object, source rect (x0, y0, x1, y1) and ARGB colour (0xFFFFFFFF untinted).
#[derive(Clone, Copy, Debug)]
pub struct WFrame {
    pub tex: i32,
    pub flags: u32,
    /// jp_B6DE0's mode (jd_3C9748 / jd_3C9698 as STBR): the tables use 0 and 27, both plain alpha, so the
    /// draw does not need it
    #[allow(dead_code)]
    pub blend: i32,
    pub d: [f32; 2],
    pub src: [i16; 4],
    pub color: u32,
}

/// The frames of the five states: 0 normal, 1 focused, 2 on (flag 2), 3 on and focused, 4 pressed.
pub type WStates = [&'static [WFrame]; 5];

/// An object table entry (48 bytes, jp_ADCA0).
#[derive(Clone, Copy, Debug)]
pub struct WObjDef {
    pub id: u16,
    pub group: u8,
    /// 1 takes input (jp_ADEB0 toggles it), 2 on, 0x20 crossfade between states, 0x40 rect y x 448 / 480,
    /// 0x100 help balloon, 0x400 clipped
    pub flags: u32,
    /// touch rect from the position, inclusive, placed by the first frame's anchor
    pub rect: [i16; 4],
    pub pos: [f32; 2],
    pub alpha: f32,
    /// field [32]: the help balloon's SYSMES id
    pub help: i32,
    pub states: WStates,
}

/// jp_B0920's anchor origin per nibble (4 is qword_613018, 0.5 / 0.5 by default).
const ANCHOR_X: [f32; 9] = [0.0, 0.5, 1.0, 0.0, 0.5, 1.0, 0.0, 0.5, 1.0];
const ANCHOR_Y: [f32; 9] = [0.0, 0.0, 0.0, 0.5, 0.5, 0.5, 1.0, 1.0, 1.0];

/// The draw state.
#[derive(Clone, Copy, Debug)]
pub struct Pen {
    /// jp_B0650: width, height and the pixel offset of the space
    pub space: [f32; 4],
    pub scale: [f32; 2],
    /// jp_B03E0 (alpha first in the exe): r, g, b, a
    pub color: [f32; 4],
}

impl Default for Pen {
    fn default() -> Self {
        Self { space: [640.0, 448.0, 0.0, 0.0], scale: [1.0, 1.0], color: [1.0; 4] }
    }
}

impl Pen {
    pub fn scale(&mut self, sx: f32, sy: f32) {
        self.scale = [sx, sy];
    }

    /// jp_B03E0(a, r, g, b)
    pub fn color(&mut self, a: f32, r: f32, g: f32, b: f32) {
        self.color = [r, g, b, a];
    }

    pub fn alpha(&mut self, a: f32) {
        self.color = [1.0, 1.0, 1.0, a];
    }

    /// Space units to canvas pixels.
    pub fn k(&self) -> [f32; 2] {
        [W / self.space[0], H / self.space[1]]
    }

    /// jp_B0D90 / jp_B0920: `src` (x0, y0, x1, y1 texels; the whole texture when None) at (x, y).
    pub fn spr(&self, out: &mut Vec<Quad>, tex: &TexRef, flags: u32, src: Option<[f32; 4]>, x: f32, y: f32) {
        if self.color[3] <= 0.0 {
            return;
        }
        let [kx, ky] = self.k();
        let [sx, sy] = if flags & 0x10 != 0 { self.scale } else { [1.0, 1.0] };
        let [x0, y0, x1, y1] = src.unwrap_or([0.0, 0.0, tex.width as f32, tex.height as f32]);
        let (w, h) = ((x1 - x0) * kx * sx, (y1 - y0) * ky * sy);
        let a = (flags & 0xF) as usize;
        let (ox, oy) = (ANCHOR_X.get(a).copied().unwrap_or(0.0), ANCHOR_Y.get(a).copied().unwrap_or(0.0));
        let px = self.space[2] + kx * x - ox * w;
        let py = self.space[3] + ky * y - oy * h;
        let (tw, th) = (tex.texture.size().width as f32, tex.texture.size().height as f32);
        let mut q = Quad::rect(tex.clone(), [px, py, w, h], self.color).with_uv_rect(x0 / tw, y0 / th, x1 / tw, y1 / th);
        q.blend = Blend::Alpha;
        out.push(q);
    }

    /// `spr` turned by `angle` radians about its anchor point (flag 0x20: jp_B0920 reads jp_B0480's rotation,
    /// a 16-bit angle x pi / 32768).
    #[allow(clippy::too_many_arguments)]
    pub fn spr_rot(&self, out: &mut Vec<Quad>, tex: &TexRef, flags: u32, src: Option<[f32; 4]>, x: f32, y: f32, angle: f32) {
        let n = out.len();
        self.spr(out, tex, flags, src, x, y);
        if flags & 0x20 == 0 || angle == 0.0 {
            return;
        }
        let [kx, ky] = self.k();
        let (cx, cy) = (self.space[2] + kx * x, self.space[3] + ky * y);
        let (sin, cos) = angle.sin_cos();
        for q in &mut out[n..] {
            for p in &mut q.pos {
                let (dx, dy) = (p[0] - cx, p[1] - cy);
                *p = [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos];
            }
        }
    }

    /// jp_B1260: a filled rect (x, y, w, h in the space) in the pen colour.
    pub fn fill(&self, out: &mut Vec<Quad>, white: &TexRef, x: f32, y: f32, w: f32, h: f32) {
        if self.color[3] <= 0.0 {
            return;
        }
        let [kx, ky] = self.k();
        out.push(Quad::rect(white.clone(), [self.space[2] + kx * x, self.space[3] + ky * y, kx * w, ky * h], self.color));
    }
}

/// An installed object.
#[derive(Clone, Copy)]
pub struct WObj {
    pub group: u8,
    pub flags: u32,
    /// +4: the state drawn, +76 the state before, +80 the one fading out over it
    pub state: u8,
    last: u8,
    prev: u8,
    /// +16: frames left pressed (jp_AE040)
    pub press: i32,
    pub rect: [i16; 4],
    pub pos: [f32; 2],
    pub scale: [f32; 2],
    pub alpha: f32,
    /// +64 / +68: the fade-out weight of `prev` and its step
    w: f32,
    dw: f32,
    pub help: i32,
    pub states: &'static WStates,
}

pub struct WObjs {
    pub o: Vec<Option<WObj>>,
    /// LODWORD(jd_19150EC): the object the pad focuses
    pub focus: i32,
    /// HIDWORD(jd_19150EC): the object under the pointer (-1 none while the pointer is active, -2 when it went
    /// elsewhere; it stays on the focus object after the release, so a tap reads it on the release frame)
    pub hit: i32,
    /// jd_AC0C08 / dword_612EAC: the objects the pointer may hit (jp_ADBE0)
    pub range: (i32, i32),
    /// jd_19150E8: the pointer is held to the focus object
    pub lock: i32,
    /// jd_19150F4: help balloons on (jp_ADBF0 sets it)
    pub help_on: bool,
    /// HIDWORD / LODWORD(jd_19150F8): the balloon's object and SYSMES id, jd_1915100 its fade (+0.05 a frame)
    pub help_obj: i32,
    pub help_id: i32,
    pub help_a: f32,
    /// jd_AC0C10: the last object the balloon was asked for
    help_last: i32,
}

impl Default for WObjs {
    fn default() -> Self {
        Self {
            o: vec![None; 374],
            focus: -1,
            hit: -1,
            range: (-1, 373),
            lock: 0,
            help_on: true,
            help_obj: -1,
            help_id: 0,
            help_a: 0.0,
            help_last: -1,
        }
    }
}

impl WObjs {
    /// jp_ADC50
    pub fn install(&mut self, table: &'static [WObjDef]) {
        for d in table {
            self.o[usize::from(d.id)] = Some(WObj {
                group: d.group,
                flags: d.flags,
                state: 0,
                last: 0,
                prev: 0,
                press: 0,
                rect: d.rect,
                pos: d.pos,
                scale: [1.0, 1.0],
                alpha: d.alpha,
                w: 0.0,
                dw: 0.2,
                help: d.help,
                states: &d.states,
            });
        }
    }

    /// jp_ADCA0(id, group, flags, rect) without frames: a touch area the screen draws itself.
    pub fn create(&mut self, id: i32, group: u8, flags: u32, rect: [i16; 4]) {
        const NONE: WStates = [&[], &[], &[], &[], &[]];
        if let Some(o) = usize::try_from(id).ok().and_then(|i| self.o.get_mut(i)) {
            *o = Some(WObj {
                group,
                flags,
                state: 0,
                last: 0,
                prev: 0,
                press: 0,
                rect,
                pos: [0.0, 0.0],
                scale: [1.0, 1.0],
                alpha: 1.0,
                w: 0.0,
                dw: 0.2,
                help: 0,
                states: &NONE,
            });
        }
    }

    /// Drop a group's objects (the screens' teardown, jp_ADD70).
    pub fn remove_group(&mut self, group: u8) {
        for o in &mut self.o {
            if o.is_some_and(|o| o.group == group) {
                *o = None;
            }
        }
    }

    pub fn get(&mut self, id: i32) -> Option<&mut WObj> {
        usize::try_from(id).ok().and_then(|i| self.o.get_mut(i)).and_then(Option::as_mut)
    }

    /// jp_AE000
    pub fn place(&mut self, id: i32, x: f32, y: f32, a: f32) {
        if let Some(o) = self.get(id) {
            o.pos = [x, y];
            o.alpha = a;
        }
    }

    /// jp_AE020
    pub fn scale(&mut self, id: i32, sx: f32, sy: f32) {
        if let Some(o) = self.get(id) {
            o.scale = [sx, sy];
        }
    }

    /// jp_AE040: pressed for `frames` frames, only while it takes input and is not on.
    pub fn press(&mut self, id: i32, frames: i32) -> bool {
        match self.get(id) {
            Some(o) if o.flags & 3 == 1 => {
                o.press = frames;
                true
            }
            _ => false,
        }
    }

    /// jp_ADEB0
    pub fn group_flags(&mut self, group: u8, set: u32, clear: u32) {
        for o in self.o.iter_mut().flatten().filter(|o| o.group == group) {
            o.flags = (o.flags | set) & !clear;
        }
    }

    /// jp_ADE80
    pub fn flags(&mut self, id: i32, set: u32, clear: u32) {
        if let Some(o) = self.get(id) {
            o.flags = (o.flags | set) & !clear;
        }
    }

    /// jp_ADE00
    pub fn set_states(&mut self, id: i32, states: &'static WStates) {
        if let Some(o) = self.get(id) {
            o.states = states;
        }
    }

    /// jp_AE840's hit test: the first input-taking object in range whose rect (placed by its state's first
    /// frame anchor, clipped to the 640x448 space) holds the pointer.
    fn hit_test(&self, p: [f32; 2]) -> i32 {
        let (start, end) = (self.range.0.max(0) as usize, if self.range.0 >= 0 { (self.range.1 + 1).max(0) as usize } else { 374 });
        let (tx, ty) = (p[0] as i32, p[1] as i32);
        for i in start..end.min(374) {
            let Some(o) = &self.o[i] else { continue };
            if o.flags & 1 == 0 {
                continue;
            }
            let anchor = o.states[usize::from(o.state)].first().map_or(0, |f| f.flags & 0xF);
            let [rx0, ry0, rx1, ry1] = o.rect.map(i32::from);
            let (w, h) = (rx1 - rx0, ry1 - ry0);
            let half = |v: i32| (if v >= 0 { v } else { v + 1 }) >> 1;
            let x0 = match anchor {
                0 | 3 | 6 => rx0,
                1 | 4 | 7 => rx0 - half(w),
                _ => rx0 - w,
            } + o.pos[0] as i32;
            let mut y0 = match anchor {
                0..=2 => ry0,
                3..=5 => ry0 - half(h),
                _ => ry0 - h,
            } + o.pos[1] as i32;
            let mut y1 = y0 + h;
            if o.flags & 0x40 != 0 {
                y0 = 448 * y0 / 480;
                y1 = 448 * y1 / 480;
            }
            let (x0, x1, y0, y1) = (x0.max(0), (x0 + w).min(640), y0.max(0), y1.min(448));
            if (y0..=y1).contains(&ty) && (x0..=x1).contains(&tx) {
                return i as i32;
            }
        }
        -1
    }

    /// jp_AE110(obj, force): the balloon for `obj` (its help id), restarted when the object changes.
    pub fn help(&mut self, obj: i32, force: bool) {
        let id = usize::try_from(obj).ok().and_then(|i| self.o[i].as_ref()).map_or(0, |o| o.help);
        if obj < 0 || !self.help_on || id <= 0 {
            self.help_obj = -1;
            self.help_id = 0;
            self.help_a = 0.0;
            return;
        }
        if self.help_obj != obj || force {
            self.help_id = id;
            self.help_obj = obj;
            self.help_a = 0.0;
        }
    }

    /// jp_AE820
    pub fn reset_hit(&mut self) {
        self.help_last = -1;
        self.help_obj = -1;
    }

    /// jp_AE840: the pointer's object (while the pointer is active) becomes the focus; leaving the focus
    /// object takes the touch's 0x1000 / 0x4000 / 0x8000 bits away; every object's state follows its press
    /// counter, the focus and flag 2, and a change starts the crossfade (the old state on top, -0.2 a frame).
    pub fn update(&mut self, inp: &mut super::WIn) {
        if self.lock != 0 {
            self.hit = self.focus;
        }
        // a mouse hovering off every object counts as no pointer: the focus and its balloon stay (a held finger
        // off them restarts the balloon each frame, which a moving mouse would make flicker)
        let active = inp.active && !(inp.hovering() && self.lock == 0 && self.range.1 >= 0 && self.hit_test(inp.pos) < 0);
        let v7 = if self.lock != 0 || !active || self.range.1 < 0 {
            self.focus
        } else {
            self.hit = self.hit_test(inp.pos);
            if self.hit >= 0 { self.hit } else { self.focus }
        };
        if let Some(o) = usize::try_from(v7).ok().and_then(|i| self.o[i].as_ref()) {
            let (px, py) = (o.pos[0] as i32, o.pos[1] as i32);
            let [rx0, ry0, rx1, ry1] = o.rect.map(i32::from);
            let (x0, x1, y0, y1) = ((rx0 + px).max(0), (rx1 + px).min(640), (ry0 + py).max(0), (ry1 + py).min(448));
            let (tx, ty) = (inp.pos[0] as i32, inp.pos[1] as i32);
            if ty > y1 || y0 > ty || x0 > tx || tx > x1 {
                inp.touch &= 0xFFFF_2FFF;
            }
        }
        if self.hit == v7 {
            if self.lock <= 1 {
                self.help(v7, false);
            }
            self.help_last = v7;
        } else {
            if !active || self.hit != -1 {
                self.hit = -2;
            } else {
                self.help_obj = -1;
                self.help_id = 0;
                self.help_a = 0.0;
            }
            if v7 < 0 {
                self.help_obj = -1;
                self.help_id = 0;
                self.help_a = 0.0;
            } else if self.help_last == v7 {
                if self.help_obj != v7 {
                    self.help(v7, false);
                }
            } else {
                self.help(v7, false);
                self.help_last = v7;
            }
        }
        if self.help_id >= 1 && self.help_a < 1.0 {
            self.help_a = (self.help_a + 0.05).min(1.0);
        }
        for (i, slot) in self.o.iter_mut().enumerate() {
            let Some(o) = slot else { continue };
            let st = if o.press > 0 {
                o.press -= 1;
                4
            } else if v7 == i as i32 {
                self.focus = v7;
                if o.flags & 2 != 0 { 3 } else { 1 }
            } else if o.flags & 2 != 0 {
                2
            } else {
                0
            };
            o.state = st;
            let last = o.last;
            if last != st {
                o.w = 1.0;
                o.last = st;
                o.prev = last;
            } else if o.prev == st {
                continue;
            }
            o.w -= o.dw;
            if o.w <= 0.0 {
                o.w = 0.0;
                o.prev = st;
            }
        }
    }

    /// jp_AEEC0(group): the objects from the last down, each state's frames at the object's position plus
    /// the frame offset, tinted by the frame colour times the object alpha; with flag 0x20 the state before
    /// on top at the crossfade weight. `tex` resolves the frames' texture ids.
    pub fn draw(&self, pen: &Pen, group: u8, out: &mut Vec<Quad>, tex: &dyn Fn(i32) -> Option<TexRef>) {
        for o in self.o.iter().rev().flatten().filter(|o| o.group == group) {
            let mut p = *pen;
            p.scale = [o.scale[0], o.scale[1]];
            if o.alpha > 0.0 {
                Self::frames(&p, o, o.states[usize::from(o.state)], o.alpha, out, tex);
            }
            if o.w > 0.0 && o.flags & 0x20 != 0 && o.prev != o.state {
                Self::frames(&p, o, o.states[usize::from(o.prev)], o.w * o.alpha, out, tex);
            }
        }
    }

    fn frames(pen: &Pen, o: &WObj, frames: &[WFrame], alpha: f32, out: &mut Vec<Quad>, tex: &dyn Fn(i32) -> Option<TexRef>) {
        let mut p = *pen;
        for f in frames {
            let Some(t) = tex(f.tex) else { continue };
            let c = f.color;
            let a = alpha * (c >> 24) as f32 / 255.0;
            if c & 0xFF_FFFF == 0xFF_FFFF {
                p.color(a, 1.0, 1.0, 1.0);
            } else {
                p.color(a, ((c >> 16) & 0xFF) as f32 / 255.0, ((c >> 8) & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0);
            }
            let src = f.src.map(f32::from);
            p.spr(out, &t, f.flags, Some(src), o.pos[0] + f.d[0], o.pos[1] + f.d[1]);
        }
    }
}
