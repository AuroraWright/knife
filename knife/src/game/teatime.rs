//! Witch on the Holy Night's TeaTime menu (TTCT, sub_73EB0): after a TeaTime episode four buttons fade in on
//! the right (sub_74010 / sub_745E0 / sub_74190, drawn by sub_74B40 after the choices): archive, save, title,
//! next episode (objects 357..360, group 40, teatime_parts). The script waits until the menu returns (sub_40ED0
//! runs sub_745E0 instead of the script while byte_F4763C is set): next episode goes on with the script, title
//! and archive end the story, ZL (or a drag to the left) rewinds. The exe steps every timer per 60 Hz frame.

use knife_render::{Quad, Renderer, TexRef};

use super::{Game, UX, UY, Wait};

/// Object positions (sub_74010 / sub_745E0: jp_AE000(357 + k, 509.33, y)), 640x448 units, top-left, scale
/// 1/3 x 0.41481 (jp_AE020): native pixels.
const BTN_X: f32 = 509.33;
const BTN_Y: [f32; 4] = [218.1926, 264.65186, 311.1111, 361.7185];
/// teatime_parts image per button in table unk_612938 (byte_F4763C 1, the `_b` set); unk_612A28 (2) takes
/// the next image (`_w`)
const BTN_IMG: [usize; 4] = [6, 0, 2, 4];
/// Frame rows (x 16..424, 88 high) per object state (sub_88DD0): 0 plain, 1 focused, 2 / 3 disabled, 4 pressed
/// (the ten frames after jp_AE040(id, 10))
const ROW_Y: [f32; 5] = [240.0, 128.0, 352.0, 352.0, 16.0];
/// "A new book was added to the archive" (teatime_parts 8 = image 4193)
const POPUP_IMG: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TeaReq {
    /// state 3: the save screen opens (sub_31A70); state 4 waits for it (sub_324D0 == 3)
    Save,
    /// title, not in a replay: sub_1D4A0(1) (the Saved banner), sub_35790(1) (the resume record and system
    /// data, sub_2F560 / jp_B6520), dword_AFFCC4 = 180
    TitleSave,
    /// title in a replay (F025, sub_47B10): the system data write (sub_87BA0 / sub_87C10)
    Store,
}

/// One button object (jd_18C0C08 + 136 id): its state, the state it crossfades from (flag 0x20: the old
/// frame on top at the fade alpha, -0.2 a frame) and the press countdown.
#[derive(Clone, Copy, Default)]
struct Btn {
    alpha: f32,
    look: usize,
    prev: usize,
    fade: f32,
    flash: f32,
}

pub struct TeaTime {
    /// dword_F47640
    state: u8,
    /// dword_F47648: the fade in (1/60 a frame) and out (1/30)
    t: f32,
    /// dword_612930 (starts on save)
    cursor: usize,
    /// byte_F4763C == 2: the `_w` images
    white: bool,
    /// HIBYTE(word_F47638) / dword_F47644: the archive popup's frame count
    popup: Option<f32>,
    /// LOBYTE(word_F47638): set up (sub_74010)
    shown: bool,
    /// object group 40 takes input (jp_ADEB0(40, 1, 0))
    input: bool,
    btn: [Btn; 4],
    parts: Option<Vec<TexRef>>,
    req: Option<TeaReq>,
}

impl Game {
    pub fn teatime_active(&self) -> bool {
        self.teatime.is_some()
    }

    /// sub_3A800: a TeaTime scene (TEATIME1..13, scenes 138..150): no quick save / load, no mode panel.
    pub fn teatime_scene(&self) -> bool {
        crate::assets::witch() && (138..=150).contains(&self.scene_index)
    }

    /// TTCT(n) (sub_73EB0): the next-scene seek ends with its sound, fast forward and auto / skip mode end,
    /// G009 is cleared by the menu's first frame; n in 2, 4, 5, 10, 11, 12 takes the `_w` buttons, n in 1, 8, 9
    /// shows the archive popup the first time (G026 bit n). The script waits for the menu.
    pub(super) fn cmd_ttct(&mut self, n: Option<i32>) {
        if self.seek_node {
            self.end_seek(true);
        }
        self.teatime_replay = false;
        self.skip = false;
        self.skip_mode = false;
        self.auto = false;
        let mut popup = None;
        let mut white = false;
        if let Some(n) = n {
            white = (0..=12).contains(&n) && (1 << n) & 0x1C34 != 0;
            if (0..=9).contains(&n) && (1 << n) & 0x302 != 0 {
                let seen = self.var("G026");
                if seen & 1 << n == 0 {
                    popup = Some(0.0);
                    self.set_var("G026", seen | 1 << n);
                }
            }
        }
        self.teatime = Some(TeaTime {
            state: 0,
            t: 0.0,
            cursor: 1,
            white,
            popup,
            shown: false,
            input: false,
            btn: [Btn::default(); 4],
            parts: None,
            req: None,
        });
        self.wait = Wait::TeaTime;
    }

    /// sub_745E0 states 0..13 and the object update (sub_88DD0) for one frame.
    pub(super) fn update_teatime(&mut self, dt: f32) {
        let f = dt / (1000.0 / 60.0);
        let banner = self.banner_up;
        let Some(tt) = &mut self.teatime else { return };
        let mut end = None;
        match tt.state {
            0 | 1 => {
                if tt.state == 0 {
                    // sub_74010; G009 = 0
                    tt.state = 1;
                    tt.shown = true;
                    tt.cursor = 1;
                    self.vars.remove("G009");
                }
                tt.t += f / 60.0;
                if tt.t >= 1.0 {
                    tt.t = 1.0;
                    tt.state = 2;
                    tt.input = true;
                }
                for (k, b) in tt.btn.iter_mut().enumerate() {
                    b.alpha = ((tt.t - 0.2 * k as f32).max(0.0) * 2.5).min(1.0);
                }
            }
            5 => {
                tt.t = 1.0;
                tt.state = 2;
                tt.input = true;
            }
            6 => {
                tt.t -= f / 30.0;
                if tt.t <= 0.0 {
                    tt.t = 0.0;
                    tt.state = 11;
                }
                for b in &mut tt.btn {
                    b.alpha = tt.t;
                }
            }
            // sub_3B5A0: the Saved banner has gone
            13 if !banner => tt.state = 9,
            9 => end = Some(1),
            10 => end = Some(2),
            11 => end = Some(3),
            _ => {}
        }
        // sub_88DD0: pressed for the countdown, else focused or plain; a change crossfades from the old look
        let focus = tt.cursor;
        for (k, b) in tt.btn.iter_mut().enumerate() {
            let look = if b.flash > 0.0 {
                b.flash -= f;
                4
            } else if k == focus {
                1
            } else {
                0
            };
            if look != b.look {
                b.prev = b.look;
                b.look = look;
                b.fade = 1.0;
            }
            if b.prev != b.look {
                b.fade -= 0.2 * f;
                if b.fade <= 0.0 {
                    b.fade = 0.0;
                    b.prev = b.look;
                }
            }
        }
        if let Some(p) = &mut tt.popup
            && tt.shown
            && tt.t > 0.0
        {
            *p += f;
            if *p > 251.0 {
                tt.popup = None;
            }
        }
        if let Some(code) = end {
            self.end_teatime(code);
        }
    }

    /// States 9 / 10 / 11: the objects and teatime_parts go. 1 (title, archive): sub_5F200 drops the text
    /// slots and the message and sub_40ED0 returns 1, the story ends. 2: the rewind starts (off_AE05B8,
    /// knife: the UI's `teatime_rewind`). 3: the script goes on.
    fn end_teatime(&mut self, code: u8) {
        self.teatime = None;
        match code {
            1 => self.ended = true,
            _ => self.wait = Wait::None,
        }
    }

    /// The UI's request: the save screen (state 3), or a write for the title button.
    pub fn take_teatime_req(&mut self) -> Option<TeaReq> {
        let tt = self.teatime.as_mut()?;
        let req = tt.req?;
        if req == TeaReq::Save {
            if tt.state != 3 {
                return None;
            }
            tt.state = 4;
            tt.input = false;
        } else {
            tt.req = None;
        }
        Some(req)
    }

    /// State 4: the save screen closed (sub_324D0 == 3, jp_B8070(22, 256, 0)) -> state 5.
    pub fn teatime_save_closed(&mut self) {
        if let Some(tt) = &mut self.teatime
            && tt.state == 4
        {
            tt.req = None;
            tt.state = 5;
        }
    }

    /// sub_74190, mask moves: up (D-pad / stick up, ZR) and down (D-pad / stick down, R) step, wrapping on a
    /// fresh press only; left (L) goes to the top, right to the bottom. Sound 2.
    pub fn teatime_move(&mut self, dir: i32, repeat: bool) {
        let Some(tt) = &mut self.teatime else { return };
        if !tt.input || tt.state != 2 {
            return;
        }
        let c = tt.cursor;
        let next = match dir {
            -1 if c >= 1 => c - 1,
            -1 if !repeat => 3,
            1 if c <= 2 => c + 1,
            1 if !repeat => 0,
            -2 if c >= 1 => 0,
            2 if c <= 2 => 3,
            _ => return,
        };
        tt.cursor = next;
        self.play_sys_sound(2);
    }

    /// sub_74190: the pointer over a button moves the cursor there (sound 2).
    pub(super) fn teatime_hover_at(&mut self, x: f32, y: f32) {
        let Some(k) = Self::teatime_button_at(x, y) else { return };
        let Some(tt) = &mut self.teatime else { return };
        if !tt.input || tt.state != 2 || tt.cursor == k {
            return;
        }
        tt.cursor = k;
        self.play_sys_sound(2);
    }

    /// Object rects (0, 0, 136, 36) at the buttons' places (jp_AE0B0), in canvas pixels.
    fn teatime_button_at(x: f32, y: f32) -> Option<usize> {
        let (ux, uy) = (x / UX, y / UY);
        (0..4).find(|&k| (BTN_X..=BTN_X + 136.0).contains(&ux) && (BTN_Y[k]..=BTN_Y[k] + 36.0).contains(&uy))
    }

    /// A tap released on the focused button (touch flag 2 and jp_AE0B0 == the cursor's object).
    pub fn teatime_click(&mut self) {
        if self.teatime.as_ref().is_some_and(|t| t.state == 13) {
            self.banner_away = true;
            return;
        }
        let (x, y) = self.mouse;
        self.teatime_hover_at(x, y);
        if Self::teatime_button_at(x, y).is_some_and(|k| self.teatime.as_ref().is_some_and(|t| t.cursor == k)) {
            self.teatime_press();
        }
    }

    /// A pressed (mask [0]) or a tap on the cursor's button: the object shows its pressed look for ten
    /// frames (jp_AE040), then by button. While the Saved banner holds, a press sends it away (state 13,
    /// jp_3B480).
    pub fn teatime_press(&mut self) {
        let replay = self.var("F025") != 0;
        let Some(tt) = &mut self.teatime else { return };
        if tt.state == 13 {
            self.banner_away = true;
            return;
        }
        if !tt.input || tt.state != 2 {
            return;
        }
        let k = tt.cursor;
        let mut sounds: [Option<usize>; 2] = [None, None];
        match k {
            // archive: G009 = the TeaTime (scenes 138..150 -> 1..13), then the story ends
            0 => {
                sounds[0] = Some(0);
                let n = self.scene_index;
                let v = if (138..=150).contains(&n) { n as i32 - 137 } else { 0 };
                self.vars.insert("G009".into(), v);
                tt.state = 9;
            }
            // save: sound 1 while saving is off, else the save screen (states 3 / 4 / 5)
            1 if replay => sounds[0] = Some(1),
            1 => {
                sounds[0] = Some(0);
                tt.state = 3;
                tt.req = Some(TeaReq::Save);
            }
            2 if replay => {
                sounds[0] = Some(0);
                tt.req = Some(TeaReq::Store);
                tt.state = 9;
            }
            2 => {
                sounds = [Some(0), Some(13)];
                tt.req = Some(TeaReq::TitleSave);
                tt.state = 13;
                // sub_1D4A0(1): the banner is up from this frame
                self.banner_up = true;
            }
            _ => {
                sounds[0] = Some(0);
                tt.state = 6;
            }
        }
        // LABEL_64: every press, jp_AE040(357 + cursor, 10)
        tt.btn[k].flash = 10.0;
        for s in sounds.into_iter().flatten() {
            self.play_sys_sound(s);
        }
    }

    /// ZL or a drag 45 units to the left while the menu takes input, with something to go back to
    /// (AE0470 | AE0460): state 10, the menu returns 2 and the rewind starts.
    pub fn teatime_rewind(&mut self) -> bool {
        let ok = self.can_rewind();
        let Some(tt) = &mut self.teatime else { return false };
        if !tt.input || tt.state != 2 || !ok {
            return false;
        }
        self.end_teatime(2);
        true
    }

    /// sub_74B40 (after the choices): the buttons at their alpha (the old look on top while it fades), and
    /// the archive popup at (400, 448 -> 401.54 over 36 frames), holding to frame 215, fading by 251.
    pub(super) fn teatime_quads(&mut self, r: &Renderer, out: &mut Vec<Quad>) {
        let Some(tt) = &self.teatime else { return };
        if !tt.shown || tt.t <= 0.0 {
            return;
        }
        if tt.parts.is_none() {
            let parts = match self.assets.ui_textures(r, &self.assets.ui_atlas("TEATIME_PARTS")) {
                Ok(t) => t.into_iter().map(|(_, t)| t).collect(),
                Err(e) => {
                    tracing::warn!("TEATIME_PARTS: {e:#}");
                    Vec::new()
                }
            };
            if let Some(tt) = &mut self.teatime {
                tt.parts = Some(parts);
            }
        }
        let Some(tt) = &self.teatime else { return };
        let parts = tt.parts.as_deref().unwrap_or_default();
        let frame = |t: &TexRef, x: f32, y: f32, src: [f32; 4], a: f32| {
            let [u, v] = t.uv_max();
            let (tw, th) = (t.width as f32, t.height as f32);
            let [x0, y0, x1, y1] = src;
            Quad::rect(t.clone(), [x * UX, y * UY, x1 - x0, y1 - y0], [1.0, 1.0, 1.0, a])
                .with_uv_rect(x0 / tw * u, y0 / th * v, x1 / tw * u, y1 / th * v)
        };
        for (k, b) in tt.btn.iter().enumerate() {
            let Some(t) = parts.get(BTN_IMG[k] + usize::from(tt.white)) else { continue };
            let row = |look: usize| [16.0, ROW_Y[look], 424.0, ROW_Y[look] + 88.0];
            if b.alpha > 0.0 {
                out.push(frame(t, BTN_X, BTN_Y[k], row(b.look), b.alpha));
            }
            if b.fade > 0.0 && b.prev != b.look {
                out.push(frame(t, BTN_X, BTN_Y[k], row(b.prev), b.fade * b.alpha));
            }
        }
        if let (Some(p), Some(t)) = (tt.popup, parts.get(POPUP_IMG))
            && p >= 1.0
        {
            let (y, a) = if p <= 35.0 {
                (448.0 - p * 46.459 / 36.0, tt.t)
            } else if p <= 215.0 {
                (401.54074, tt.t)
            } else {
                (401.54074, ((p - 36.0 - 180.0) / -36.0 + 1.0) * tt.t)
            };
            let (w, h) = (t.width as f32, t.height as f32);
            out.push(frame(t, 400.0, y, [0.0, 0.0, w, h], a));
        }
    }
}
