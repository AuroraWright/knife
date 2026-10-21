//! Touch screen effects and story gestures, driven by the mouse (left button = finger).
//!
//! - Ripples (sub_AD4E0 -> sub_1034, updated / drawn by sub_E00): every touch-down once the game task runs
//!   (sub_AFA40 draws them), i.e. from the title or the fresh-profile prologue on; the launch task
//!   (sub_B5650: TM_CI logo, caution2 notice) draws none.
//! - Story drag arrow (sub_40ED0, drawn by sub_3C510): a horizontal drag fast-forwards or rewinds.
//! - Message window hiding at the bottom edge (sub_40ED0).
//! - Mode panel at the top edge (sub_4E170 state machine, drawn by sub_4DE10).
//!
//! Positions in the exe's input state are 640x448 units (touch x / 2, y * 448 / 720).

use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Renderer, TexRef, Upload};

use super::{Confirm, Screen, Ui};
use crate::{game::Game, save};

const W: f32 = CANVAS_W as f32;
const H: f32 = CANVAS_H as f32;
const UX: f32 = W / 640.0;
const UY: f32 = H / 448.0;

/// sub_1034: 16 slots, a new touch takes the first free one (none when all are busy)
const RIPPLES: usize = 16;
/// radius (render pixels) and duration (seconds) passed by sub_AD4E0
const RIPPLE_RADIUS: f32 = 48.0;
const RIPPLE_SECS: f32 = 0.5;
/// a touch that moved this far (|dx| + |dy|, units) is a drag, not a tap (sub_AD4E0)
const DRAG_MIN: f32 = 7.0;
/// mode panel button objects 207..214: x of each (units), y below the panel top
const PANEL_X0: f32 = 25.333;
const PANEL_DX: f32 = 73.667;
const PANEL_BTN_Y: f32 = 6.637;
/// mode_panel_btn_ja cells (8 columns; rows: disabled, normal, hover, pressed): the sprite defs at 0x591cf8 take
/// 224x64 from (8 + 232 column, 8 + 72 row), drawn from the object's position
const BTN_W: f32 = 224.0;
const BTN_H: f32 = 64.0;

/// The exe's drag tracker (the object at 0xAE2EE8, stepped once a frame by sub_40C) for a list's vertical drag:
/// a touch that starts inside the screen's rect is followed in the 1280x720 touch space. Each frame the speed
/// decays by 0.9 (0 below 0.5), then takes the frame's movement (previous y - y, so up is positive) when that
/// is larger the same way; after release it keeps decaying, so a flick coasts. sub_6C4 reads the speed.
#[derive(Default)]
pub(super) struct TouchDrag {
    /// [previous, current] touch y while the touch lasts
    y: Option<[i32; 2]>,
    pub vel: f32,
}

impl TouchDrag {
    /// A touch began inside the rect, at canvas y `y`.
    pub fn press(&mut self, y: f32) {
        let y = Self::raw(y);
        self.y = Some([y, y]);
        self.vel = 0.0;
    }

    pub fn moved(&mut self, y: f32) {
        if let Some(p) = &mut self.y {
            p[1] = Self::raw(y);
        }
    }

    pub fn release(&mut self) {
        self.y = None;
    }

    /// One 60 Hz frame of sub_40C.
    pub fn frame(&mut self) {
        if self.vel.abs() > f32::EPSILON {
            self.vel *= 0.9;
            if self.vel.abs() < 0.5 {
                self.vel = 0.0;
            }
        }
        if let Some([prev, cur]) = &mut self.y {
            let d = *cur - *prev;
            let v = -d as f32;
            if (d >= 1 && self.vel > v) || (d < 0 && self.vel < v) {
                self.vel = v;
            }
            *prev = *cur;
        }
    }

    /// Canvas y in touch pixels (720 high).
    fn raw(y: f32) -> i32 {
        (y * 720.0 / H) as i32
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Panel {
    #[default]
    Closed,
    Opening,
    Open,
    Closing,
}

pub struct Touch {
    disc: Option<TexRef>,
    /// ripple slots: centre (canvas px) and age (s)
    ripples: [Option<([f32; 2], f32)>; RIPPLES],
    /// left button: where it went down (units), whether it became a drag, whether its release is swallowed
    press: Option<[f32; 2]>,
    dragged: bool,
    swallow: bool,
    /// frames left of touch visibility (off_AE02C8 + 0xC: 256 while a finger is down, then counting down),
    /// the button / finger is down (+8), and where it last was (units, +0x2C / +0x30: it stays there after
    /// the release). Only a held button moves the touch: a mouse that merely hovers is no finger on the
    /// screen.
    visible: f32,
    pub(super) down: bool,
    at: [f32; 2],
    /// drag arrow: alpha (off_AE0600), effect fade (dword_AFECA4), start (units) and stretch
    arrow_a: f32,
    arrow_fx: f32,
    /// sub_3C510's mode icon (dword_AFEC9C): mode_disp cell (0 none, 1 previous scene, 2 rewind, 3 fast
    /// forward, 4 next scene, 5 skip mode)
    mode_icon: usize,
    /// sub_40ED0's scene countdowns: next scene (off_AE0680 3 / off_AE06B0 3, 20 frames; 4 for an RDAS hit
    /// without the confirmation) or previous scene (off_AE0680 6, 20 frames), frames left and the 60 Hz
    /// frames owed; the script and the story input wait meanwhile
    pub(super) scene_cd: Option<(bool, i32)>,
    cd_tick: f32,
    /// sub_3F110 / sub_3EF20: black over the story at min(v, 1), v falling 0.04 per frame once set
    cd_black: f32,
    arrow_at: [f32; 2],
    arrow_dx: f32,
    /// the gesture currently drives game.skip / a rewind at this speed
    drag_skip: bool,
    rewind: u32,
    /// 60 Hz frames towards the next rewind step
    rewind_t: f32,
    /// message window hidden at the bottom edge (off_AE0690)
    pub hidden: bool,
    /// mode panel (dword_B35A10), its slide (dword_B35A28), its hovered button, the button held
    /// and how long (dword_AFFCCC), and the per-button press flash (sub_AE040(.., 10))
    pub panel: Panel,
    panel_t: f32,
    pub panel_hover: Option<usize>,
    /// the pointer moved since the last frame, pressed or not (the panel's buttons follow a hovering mouse
    /// as the other menus' items do)
    hovered: bool,
    held_frames: f32,
    flash: [f32; 8],
    /// The panel's selected button (HIDWORD(qword_B35A1C)), and the last one picked by either pointer or pad,
    /// which the next opening starts on (LODWORD)
    pub panel_key: Option<usize>,
    pub panel_last: usize,
    /// pointer two frames back (units): the release flick compares against it (sub_AD4E0 slot fields 7 / 9)
    trail: [[f32; 2]; 2],
    trail_t: f32,
    /// ZL / ZR held (frames)
    pub zl: Option<f32>,
    pub zr: Option<f32>,
}

impl Touch {
    pub fn new() -> Self {
        Self {
            disc: None,
            ripples: [None; RIPPLES],
            press: None,
            dragged: false,
            swallow: false,
            visible: 0.0,
            down: false,
            // sub_AD1F0: the screen centre
            at: [320.0, 224.0],
            arrow_a: 0.0,
            arrow_fx: 0.0,
            arrow_at: [0.0; 2],
            mode_icon: 0,
            scene_cd: None,
            cd_tick: 0.0,
            cd_black: 0.0,
            rewind_t: 0.0,
            arrow_dx: 0.0,
            drag_skip: false,
            rewind: 0,
            hidden: false,
            panel: Panel::Closed,
            panel_t: 0.0,
            panel_hover: None,
            hovered: false,
            held_frames: 0.0,
            flash: [0.0; 8],
            panel_key: None,
            // sub_4DCC0: the panel's cursor starts on fast forward
            panel_last: 3,
            trail: [[0.0; 2]; 2],
            trail_t: 0.0,
            zl: None,
            zr: None,
        }
    }

    /// The ripple mesh (sub_B8648: 32-segment fan, centre colour transparent, rim colour (1, 1, 1, 0.5))
    /// as a texture: alpha rises linearly from the centre to the rim.
    fn disc(&mut self, r: &Renderer) -> TexRef {
        self.disc
            .get_or_insert_with(|| {
                const N: usize = 128;
                let mut px = vec![255u8; N * N * 4];
                let c = N as f32 / 2.0;
                for y in 0..N {
                    for x in 0..N {
                        let d = ((x as f32 + 0.5 - c).hypot(y as f32 + 0.5 - c)) / c;
                        // 1 px antialiased rim
                        let edge = ((1.0 - d) * c).clamp(0.0, 1.0);
                        px[(y * N + x) * 4 + 3] = (d.min(1.0) * edge * 255.0).round() as u8;
                    }
                }
                r.upload(N as u32, N as u32, Upload::Rgba8(&px), "ripple")
            })
            .clone()
    }

    fn spawn(&mut self, x: f32, y: f32) {
        if let Some(s) = self.ripples.iter_mut().find(|s| s.is_none()) {
            *s = Some(([x, y], 0.0));
        }
    }

    fn draw_ripples(&mut self, r: &Renderer, out: &mut Vec<Quad>) {
        if self.ripples.iter().all(Option::is_none) {
            return;
        }
        let tex = self.disc(r);
        for &([x, y], t) in self.ripples.iter().flatten() {
            // sub_E00: alpha quad-in 1 -> 0, radius cubic-out 0 -> 48 over the duration (sub_E1BFC types 2 / 3)
            let u = (t / RIPPLE_SECS).min(1.0);
            let a = 1.0 - u * u;
            let rad = RIPPLE_RADIUS * ((u - 1.0).powi(3) + 1.0);
            if rad <= 0.0 || a <= 0.0 {
                continue;
            }
            out.push(Quad::rect(tex.clone(), [x - rad, y - rad, rad * 2.0, rad * 2.0], [1.0, 1.0, 1.0, 0.5 * a]));
        }
    }
}

impl Ui {
    fn units(&self) -> [f32; 2] {
        [self.mouse.0 / UX, self.mouse.1 / UY]
    }

    /// Mouse moved (canvas px): a drag moves the touch, a hover does not.
    /// sub_4E170 case 6, after the quick load (sub_527A0(139)): the mode panel slides shut from where it is
    /// (state 4) with no button focused (sub_AE0E0(-1)), and sub_AD440(320, 224, 256) puts the last touch back at
    /// the screen centre, so the tap on Q.LOAD in the top 25 units does not open the panel again.
    pub(super) fn close_panel_after_quick_load(&mut self) {
        self.touch.panel = Panel::Closing;
        self.touch.panel_hover = None;
        self.touch.panel_key = None;
        self.touch.at = [320.0, 224.0];
    }

    pub(super) fn touch_move(&mut self) {
        self.touch.hovered = true;
        if !self.touch.down {
            return;
        }
        self.touch.visible = 256.0;
        // CG viewer (sub_C56C): a held touch moves the picture twice as far as the finger
        if matches!(self.screen, Screen::CgView { .. }) && !self.busy() {
            let [x, y] = self.units();
            let [ax, ay] = self.touch.at;
            self.cg_pan[0] += 2.0 * (x - ax) * UX;
            self.cg_pan[1] += 2.0 * (y - ay) * UY;
        }
        self.touch.at = self.units();
        self.drag.moved(self.mouse.1);
        if let Some(p) = self.touch.press {
            let [x, y] = self.units();
            if (x - p[0]).abs() + (y - p[1]).abs() >= DRAG_MIN {
                self.touch.dragged = true;
            }
        }
    }

    /// Left button down: a ripple everywhere past the launch logos. In the story the tap acts on release, as
    /// on the touch screen, so a drag can become a gesture instead; elsewhere it clicks at once.
    pub fn mouse_down(&mut self, game: &mut Game, r: &Renderer) {
        let (x, y) = self.mouse;
        self.touch.trail = [self.units(); 2];
        if !matches!(self.intro, Some(super::Intro::Logo { .. } | super::Intro::Notice { .. })) {
            self.touch.spawn(x, y);
        }
        self.touch.hovered = true;
        self.touch.visible = 256.0;
        self.touch.down = true;
        self.touch.at = self.units();
        // a touch inside the screen's drag rect (sub_6A0 before sub_40C) is tracked; the config only takes
        // one that is on none of its objects (sub_40C a2 = sub_AE0B0 < 0)
        let rect = match self.screen {
            Screen::Flowchart { .. } => Some([0.0, 0.0, 1360.0, 1022.0]),
            Screen::Gallery => Some([392.0, 109.0, 1440.0, 905.0]),
            Screen::Music => Some([408.0, 109.0, 926.0, 905.0]),
            Screen::Config if !self.items(game).iter().any(|(rc, _)| super::rect_hit(*rc, (x, y))) => Some([407.0, 165.0, 1402.0, 858.0]),
            _ => None,
        };
        if let Some([rx, ry, rw, rh]) = rect
            && (rx..=rx + rw).contains(&x)
            && (ry..=ry + rh).contains(&y)
        {
            self.drag.press(y);
        }
        if !self.busy() && self.popup.is_none() && self.bar_grab(y) {
            return;
        }
        if matches!(self.screen, Screen::Game | Screen::CgView { .. }) {
            self.touch.press = Some(self.units());
            self.touch.dragged = false;
            return;
        }
        self.click(game, r);
    }

    pub fn mouse_up(&mut self, game: &mut Game, r: &Renderer) {
        self.touch.down = false;
        self.drag.release();
        self.bar = None;
        let Some(start) = self.touch.press.take() else { return };
        let swallow = std::mem::take(&mut self.touch.swallow);
        if matches!(self.screen, Screen::CgView { .. }) {
            self.cg_flick(game);
            return;
        }
        if self.screen != Screen::Game {
            return;
        }
        let [x, y] = self.units();
        if self.touch.dragged {
            self.flick(game, r, start, [x, y]);
            return;
        }
        if swallow {
            return;
        }
        match self.touch.panel {
            // a tap in the bottom 40 units only hides the window (sub_40ED0); a hidden window takes no tap
            Panel::Closed if self.touch.hidden || y > 408.0 => {}
            // sub_4E170 runs before the story and the choice (sub_83E50) read the tap: one in the top 25 units
            // opens the panel (state 1) and goes no further
            Panel::Closed if y < 25.0 && self.panel_allowed(game) => {
                self.touch.panel = Panel::Opening;
                self.touch.panel_key = Some(self.touch.panel_last);
            }
            Panel::Closed => self.click(game, r),
            Panel::Open => {
                if y <= 43.0 {
                    if let Some(b) = self.panel_button() {
                        self.panel_press(game, r, b);
                    }
                } else if y > 120.0 {
                    // sub_4E170 state 3: a tap below the panel closes it (the text does not advance)
                    self.touch.panel = Panel::Closing;
                }
            }
            _ => {}
        }
    }

    /// Release of a drag (sub_AD4E0 swipe bits, from the move over the last two frames: >= 7 units, the
    /// stronger axis wins) acted on in the story (sub_40ED0 / sub_4E170).
    fn flick(&mut self, game: &mut Game, r: &Renderer, start: [f32; 2], end: [f32; 2]) {
        let [px, py] = self.touch.trail[1];
        let (dx, dy) = (end[0] - px, end[1] - py);
        let horiz = dx.abs() > dy.abs();
        let (left, right) = (horiz && dx <= -7.0, horiz && dx >= 7.0);
        let (up, down) = (!horiz && dy <= -7.0, !horiz && dy >= 7.0);
        if game.in_choice() {
            return;
        }
        match self.touch.panel {
            // state 3: an upward flick closes the panel
            Panel::Open if up => self.touch.panel = Panel::Closing,
            Panel::Closed if !self.touch.hidden => {
                if down && start[1] <= 99.0 {
                    // a downward flick from the top opens the mode panel (sub_4E090(1))
                    self.touch.panel = Panel::Opening;
                } else if right && start[0] <= 99.0 {
                    // from the left edge: the system menu (off_AE0678)
                    self.cancel(game, r);
                } else if left && start[0] > 540.0 && game.backlog_len() > 0 {
                    // from the right edge: the backlog (sub_27EB0)
                    self.push(Screen::Backlog { scroll: 0 });
                }
            }
            _ => {}
        }
    }

    /// CG viewer release (sub_C56C, swipe bits of sub_AD4E0 at 13+ units): right (0x20) the previous
    /// picture, left (0x10) the next, up or down (0xC0) closes.
    fn cg_flick(&mut self, game: &mut Game) {
        let [x, y] = self.units();
        let [px, py] = self.touch.trail[1];
        let (dx, dy) = (x - px, y - py);
        if dx.abs() > dy.abs() {
            if dx >= 13.0 {
                self.cg_step(game, -1);
            } else if dx <= -13.0 {
                self.cg_step(game, 1);
            }
        } else if dy.abs() >= 13.0 {
            self.cg_close(game);
        }
    }

    /// Show the hidden message window by other means than a touch: like the exe (0x41E1C, 0x42158, 0x421C4,
    /// 0x4223C) the remembered touch moves out of the bottom 40 units (y 409+ -> 398) so it does not hide
    /// the window again.
    pub(super) fn show_window(&mut self) {
        self.touch.hidden = false;
        if self.touch.at[1] >= 409.0 {
            self.touch.at[1] = 398.0;
        }
    }

    /// Help balloon of the focused panel button once the panel is open (state 3 focuses object 207 + button,
    /// sub_AE0E0): object table unk_5918C8, SYSMES 169 171 173 172 170 174 175 176, rect (4, -10) 70x36, flags 0x320.
    pub(super) fn panel_help(&self) -> Option<(usize, [f32; 2], [f32; 4], u32)> {
        if self.touch.panel != Panel::Open || matches!(self.screen, Screen::Confirm(_)) || self.dlg_holding() {
            return None;
        }
        let i = self.touch.panel_hover.or(self.touch.panel_key)?;
        let id = [169, 171, 173, 172, 170, 174, 175, 176][i];
        Some((id, [PANEL_X0 + PANEL_DX * i as f32, self.panel_y() + PANEL_BTN_Y], [4.0, -10.0, 70.0, 36.0], 0x320))
    }

    /// sub_40ED0 enables the mode panel (sub_4E080(1)) in the story, also over a choice list; only the SELS / SELA
    /// question dialogs (byte_14715B0 / byte_14715B1), a scene countdown and the engine tasks turn it off.
    fn panel_allowed(&self, game: &Game) -> bool {
        self.screen == Screen::Game && !game.choice_question() && self.touch.scene_cd.is_none() && !game.mode_task()
    }

        fn panel_y(&self) -> f32 {
        // sub_4DE10: quad-out from -43.14 to 0 units over the slide
        let t = self.touch.panel_t;
        -43.14 - 43.14 * t * (t - 2.0)
    }

    fn panel_button(&self) -> Option<usize> {
        let [x, y] = self.units();
        let top = self.panel_y() + PANEL_BTN_Y;
        (0..8).find(|&i| {
            let x0 = PANEL_X0 + PANEL_DX * i as f32;
            x >= x0 && x < x0 + BTN_W / UX && y >= top && y < top + BTN_H / UY
        })
    }

    /// Panel buttons 4..6 that sub_4E0C0 leaves enabled: next scene except in a gallery flowchart replay
    /// (sub_50F0), Q.SAVE / Q.LOAD unless saving is off (F025, sub_47B10).
    pub(super) fn panel_ok(&self, game: &Game, b: usize) -> bool {
        match b {
            4 => self.gal_ret[0] != 2,
            5 | 6 => game.var("F025") == 0,
            _ => true,
        }
    }

    /// A tapped mode panel button (sub_4E170 state 3 cases 0..7; 1 and 3 act while held, see `touch_update`).
    pub(super) fn panel_press(&mut self, game: &mut Game, r: &Renderer, b: usize) {
        self.touch.flash[b] = 10.0;
        // sub_4E170: a button that acts plays system sound 1 (before its dialog), an unavailable one sound 2
        match b {
            // previous / next scene and quick load ask first (dialogs 11, 10, 9)
            0 if game.can_prev_scene() => {
                self.snd(game, 1);
                self.confirm(game, Confirm::PrevScene);
            }
            2 => {
                self.snd(game, 1);
                self.toggle_auto(game);
            }
            4 if self.panel_ok(game, 4) => {
                self.snd(game, 1);
                self.confirm(game, Confirm::NextScene);
            }
            5 if game.can_save() && self.panel_ok(game, 5) => {
                self.snd(game, 1);
                self.quick_save(game, r);
            }
            // case 6: sound 2 without a quick save (sub_52BF0(139) < 1)
            6 if self.panel_ok(game, 6) && self.slots.get(save::QUICK_SLOT).is_some_and(Option::is_some) => {
                self.snd(game, 1);
                self.confirm(game, Confirm::QuickLoad);
            }
            0 | 4 | 5 | 6 => self.snd(game, 2),
            7 => game.replay_voice(),
            _ => {}
        }
    }

    /// Per frame (60 Hz frames `f`), after the game update.
    pub(super) fn touch_update(&mut self, game: &mut Game, r: &Renderer, secs: f32, f: f32) {
        for s in &mut self.touch.ripples {
            if let Some((_, t)) = s {
                *t += secs;
                if *t >= RIPPLE_SECS {
                    *s = None;
                }
            }
        }
        // no story touches during a scene countdown (sub_40ED0 returns first)
        // nor during the staff roll task (off_AE0650 = 1; only taps and ZR act)
        let in_game = self.screen == Screen::Game && !game.in_choice() && self.touch.scene_cd.is_none() && !game.mode_task();
        let panel_game = self.panel_allowed(game);
        let held = self.touch.press.is_some();
        let [x, y] = self.touch.at;
        let hovered = std::mem::take(&mut self.touch.hovered);
        // the pointer is sampled once per 60 Hz frame, like the touch panel
        self.touch.trail_t += f;
        if self.touch.trail_t >= 1.0 {
            self.touch.trail_t = (self.touch.trail_t - 1.0).min(1.0);
            self.touch.trail = [[x, y], self.touch.trail[0]];
        }
        for h in [&mut self.touch.zl, &mut self.touch.zr].into_iter().flatten() {
            *h += f;
        }
        self.touch.visible = if self.touch.down { 256.0 } else { (self.touch.visible - f).max(0.0) };
        for v in &mut self.touch.flash {
            *v = (*v - f).max(0.0);
        }

        // drag arrow (sub_40ED0): a touch that started in the middle of the screen; the stretch is
        // clamped to 260 units, and past 45 it sets the speed (dx / 5 - 8, or dx / 5 + 8 to the left)
        let mut speed = 0i32;
        let arrow_on = in_game
            && self.touch.panel == Panel::Closed
            && self.touch.press.is_some_and(|p| (121.0..=519.0).contains(&p[0]) && (125.0..=383.0).contains(&p[1]));
        if arrow_on {
            let p = self.touch.press.unwrap();
            self.touch.arrow_at = p;
            self.touch.arrow_dx = (x - p[0]).clamp(-260.0, 260.0);
            self.touch.arrow_a = (self.touch.arrow_a + 0.03 * f).min(1.0);
            let dx = self.touch.arrow_dx as i32;
            speed = if dx >= 45 {
                dx / 5 - 8
            } else if dx <= -45 {
                dx / 5 + 8
            } else {
                0
            };
        } else {
            self.touch.arrow_a = (self.touch.arrow_a - 0.03 * f).max(0.0);
        }

        // mode panel buttons held: rewind (case 1, speed ramping over 8..107 frames up to 64) and
        // skip (case 3, speed 64)
        let mut panel_rewind = None;
        let mut panel_skip = false;
        let held_button = if held && y <= 43.0 {
            self.panel_button()
        } else if self.enter_held {
            self.touch.panel_key
        } else {
            None
        };
        if in_game && self.touch.panel == Panel::Open && held_button.is_some() {
            match held_button {
                Some(1) => {
                    let t = self.touch.held_frames;
                    panel_rewind = Some(if t > 107.0 { 64 } else if t >= 8.0 { ((t as i32) * 64 - 512) / 100 } else { 0 });
                    self.touch.held_frames += f;
                }
                Some(3) => panel_skip = true,
                _ => self.touch.held_frames = 0.0,
            }
        } else {
            self.touch.held_frames = 0.0;
        }

        // ZR / ZL held (sub_40ED0, masks [19] / [20] = 0x200 / 0x100): speed 1 for the first 4 frames held,
        // then frames / 15 up to 64
        let ramp = |t: f32| -> u32 { if t < 4.0 { 1 } else { (((t as u64 * 0x22_2222_2240) >> 32) >> 7).min(64) as u32 } };
        // sub_40ED0: while a story movie or the staff roll runs (off_AE0650) skip mode is cleared every frame;
        // the staff roll takes ZR (its pressure past 71 of 255: any press of a digital ZR) at 32
        // (the inits set the drag arrow's alpha to 0 with sub_3B5C0, and nothing raises it while it is set)
        if game.mode_task() {
            game.skip_mode = false;
            self.touch.arrow_a = 0.0;
        }
        let staff = self.screen == Screen::Game && game.mode_task() && !game.in_choice();
        let zr = if staff {
            self.touch.zr.map(|_| 32)
        } else if in_game && self.touch.panel == Panel::Closed {
            self.touch.zr.map(ramp)
        } else {
            None
        };
        let zl = if in_game && self.touch.panel == Panel::Closed { self.touch.zl.map(ramp) } else { None };
        // forward: skip at the speed (unread text still stops it unless the config allows it);
        // backward: rewind, one ring entry a frame without running the script
        let fwd = if panel_skip { 64 } else { zr.unwrap_or(speed.max(0) as u32) };
        if game.skip_mode && self.screen == Screen::Game {
            // R skip mode: on until B, R again, or unread text stops the skip
            game.set_skip(true);
            if !game.skip {
                game.skip_mode = false;
                game.set_skip(false);
            }
            self.touch.drag_skip = true;
            // skip mode runs at word_34E054[off_AE0660 & 3] = 64 (ZR / the gesture when held)
            game.speed = if fwd > 0 { fwd } else { 64 };
        } else if fwd > 0 {
            game.set_skip(true);
            game.speed = fwd;
            self.touch.drag_skip = true;
        } else if self.touch.drag_skip {
            self.touch.drag_skip = false;
            game.set_skip(false);
            game.speed = 1;
        }
        // (the held panel button rewinds from its first frame, at speed 0 for the first 8: sub_814C0 ignores
        // the speed)
        let back = if let Some(v) = zl {
            Some(v)
        } else if let Some(v) = panel_rewind {
            Some(v as u32)
        } else if speed < 0 && !self.touch.hidden {
            Some((-speed) as u32)
        } else {
            None
        };
        // (off_AE05B8 stays set while held, also once the ring has run out)
        if let Some(back) = back.filter(|_| game.can_rewind() || game.rewinding) {
            self.touch.rewind = back.max(1);
            game.speed = back.max(1);
            // sub_40ED0 runs sub_814C0 instead of the script: no message is shown while rewinding, and each
            // 60 Hz frame restores the stage of the previous ring entry (whatever the display rate)
            game.rewind_frame();
            game.rewinding = true;
            self.touch.rewind_t = (self.touch.rewind_t + f).min(1.0);
            if self.touch.rewind_t >= 1.0 && game.settled() {
                self.touch.rewind_t -= 1.0;
                game.rewind_step(r);
            }
        } else if self.touch.rewind > 0 {
            // released: the script runs again from the restored message, which shows and types out
            self.touch.rewind = 0;
            game.end_rewind();
            game.speed = 1;
        }
        // sub_3C510: the effect fade (dword_AFECA4) rises 0.05 per frame (x5 while rewinding) while rewinding,
        // at speed 2 or more (a skip; ZR from its 30th frame), in auto mode (dword_AFEC98), or while the drag
        // gesture runs (off_AE0610), and falls otherwise
        let rate = 0.05 * f * if self.touch.rewind > 0 { 5.0 } else { 1.0 };
        let story = self.screen == Screen::Game && !game.in_choice();
        let fx_on = self.touch.rewind > 0 || (story && (game.speed >= 2 || game.auto)) || arrow_on;
        // the mode icon (sub_3B5B0, set each frame by sub_40ED0): 5 in skip mode, then 2 while rewinding or 3
        // while skipping fast (off_AE0670)
        self.touch.mode_icon = if self.touch.rewind > 0 {
            2
        } else if self.touch.drag_skip && in_game {
            3
        } else if game.skip_mode {
            5
        } else {
            0
        };
        self.scene_countdown(game, r, f);
        self.touch.arrow_fx = if fx_on {
            (self.touch.arrow_fx + rate).min(1.0)
        } else {
            (self.touch.arrow_fx - rate).max(0.0)
        };

        // message window (sub_40ED0): hidden while the last touch is in the bottom 40 units (y > 408 of 448),
        // back with a touch above them (that touch does not also tap)
        if in_game && self.touch.panel == Panel::Closed {
            if y > 408.0 {
                self.touch.hidden = true;
            } else if y < 408.0 && self.touch.hidden && self.touch.down {
                self.touch.hidden = false;
                self.touch.swallow |= held;
            }
        }
        if self.screen != Screen::Game && self.touch.hidden {
            self.show_window();
        }

        // mode panel (sub_4E170 state 1): opens when the last touch, still visible, is in the top 25 units; slides
        // 0.05 per frame, stays until a tap below it or an upward swipe
        match self.touch.panel {
            Panel::Closed => {
                // state 1 sets the button request (HIDWORD(qword_B35A14) = 1) however it opens, so a panel
                // opened by touch also starts on its last button and takes the pad
                if panel_game && !self.touch.hidden && self.touch.visible > 0.0 && y < 25.0 {
                    self.touch.panel = Panel::Opening;
                    self.touch.panel_key = Some(self.touch.panel_last);
                }
            }
            Panel::Opening => {
                self.touch.panel_t += 0.05 * f;
                if self.touch.panel_t >= 1.0 {
                    self.touch.panel_t = 1.0;
                    self.touch.panel = Panel::Open;
                    self.show_window();
                    // state 3 reads the pointed object every frame: a pointer resting on a button takes it
                    if let Some(h) = self.panel_button() {
                        self.touch.panel_hover = Some(h);
                        self.touch.panel_last = h;
                    }
                }
            }
            Panel::Open => {
                // state 3, as the other menus' cursors: the pointed button takes the focus (sound 0 when it
                // changes) and is the one the pad goes on from (qword_B35A1C); a pointer moved off the buttons
                // leaves none lit, the pad's focus (HIDWORD(qword_B35A14)) only stays until the pointer moves
                if hovered {
                    let before = self.touch.panel_hover.or(self.touch.panel_key);
                    self.touch.panel_hover = self.panel_button();
                    self.touch.panel_key = None;
                    if let Some(h) = self.touch.panel_hover {
                        if before != Some(h) {
                            game.play_sys_sound(0);
                        }
                        self.touch.panel_last = h;
                    }
                }
                if self.screen == Screen::Game && game.choice_question() {
                    self.touch.panel = Panel::Closing;
                }
            }
            Panel::Closing => {
                self.touch.panel_t -= 0.05 * f;
                if self.touch.panel_t <= 0.0 {
                    self.touch.panel_t = 0.0;
                    self.touch.panel = Panel::Closed;
                    self.touch.panel_hover = None;
                    self.touch.panel_key = None;
                }
            }
        }
    }

    /// Story overlays: drag arrow and mode panel.
    /// Start a next (dialog 10, an RDAS hit) / previous (dialog 11) scene countdown of `frames`; the mode panel
    /// closes (sub_4E9A0) and the window comes back (off_AE0690 = 0).
    pub(super) fn start_scene_countdown(&mut self, next: bool, frames: i32) {
        self.touch.scene_cd = Some((next, frames));
        self.touch.cd_tick = 0.0;
        if matches!(self.touch.panel, Panel::Open | Panel::Opening) {
            self.touch.panel = Panel::Closing;
        }
        self.touch.hidden = false;
    }

    /// sub_40ED0 states 3 / 6: per frame the count goes down, the next-scene one sets sub_3F110(1.5 - n / 30) and
    /// mode icon 4, the previous-scene one icon 1; at 0 the seek starts (sub_53950(1) / sub_85510 + (-1)).
    fn scene_countdown(&mut self, game: &mut Game, r: &Renderer, f: f32) {
        self.touch.cd_tick += f;
        while self.touch.cd_tick >= 1.0 {
            self.touch.cd_tick -= 1.0;
            // sub_3EF20 after drawing
            if self.touch.cd_black > 0.0 {
                self.touch.cd_black = (self.touch.cd_black - 0.04).max(0.0);
            }
            let Some((next, n)) = &mut self.touch.scene_cd else { continue };
            *n -= 1;
            if *next {
                self.touch.cd_black = 1.5 - *n as f32 / 30.0;
            }
            if *n <= 0 {
                let next = *next;
                self.touch.scene_cd = None;
                if next {
                    game.next_scene();
                } else {
                    game.prev_scene(r);
                }
            }
        }
        if self.touch.scene_cd.is_none() && self.touch.cd_tick > 1.0 {
            self.touch.cd_tick = 0.0;
        }
        if let Some((next, _)) = self.touch.scene_cd {
            self.touch.mode_icon = if next { 4 } else { 1 };
        }
        game.hold = self.touch.scene_cd.is_some();
    }

    pub(super) fn draw_touch_game(&self, game: &Game, r: &Renderer, out: &mut Vec<Quad>) {
        // sub_3EF20, first in sub_3C510: the countdown's black over the screen
        if self.touch.cd_black > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, self.touch.cd_black.min(1.0)]));
        }
        // sub_3C510: mode_skip_0 anchored at its left middle on the touch start, x scale
        // stretch / 88 * 0.5 (negative points left) and y scale 0.5 in the 640x448 space
        let a = self.touch.arrow_a * self.touch.arrow_fx * 0.6;
        if a > 0.0
            && let Some(t) = self.tex.get("mode_skip_0")
        {
            let [sx, sy] = self.touch.arrow_at;
            let len = t.width as f32 * self.touch.arrow_dx * UX / 176.0;
            let h = t.height as f32 * 0.5 * UY;
            let (x0, x1) = (sx * UX, sx * UX + len);
            let mut q = Quad::rect(t.clone(), [x0.min(x1), sy * UY - h / 2.0, len.abs(), h], [1.0, 1.0, 1.0, a]);
            let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
            let (u1, v1) = (t.width as f32 / tw, t.height as f32 / th);
            q = if len < 0.0 { q.with_uv_rect(u1, 0.0, 0.0, v1) } else { q.with_uv_rect(0.0, 0.0, u1, v1) };
            out.push(q);
        }
        // sub_3C510: the mode icon, mode_disp cell 88x88 centred (anchor 20) at (dword_34DBE4[icon], 38.16)
        // units, alpha dword_AFECA4 (x 0.6 in skip mode)
        let icon = self.touch.mode_icon;
        if (1..=5).contains(&icon) && self.touch.arrow_fx > 0.0 {
            const X: [f32; 6] = [589.333, 546.667, 546.667, 626.667, 626.667, 626.667];
            const CELL: [f32; 6] = [176.0, 0.0, 88.0, 264.0, 352.0, 264.0];
            let a = self.touch.arrow_fx * if icon == 5 { 0.6 } else { 1.0 };
            let (x, y) = (X[icon] * UX, 38.163 * UY);
            self.sprite_c(out, "mode_disp", [x - 44.0, y - 44.0, 88.0, 88.0], Some([CELL[icon], 0.0, 88.0, 88.0]), a, Blend::Alpha);
        }
        if self.touch.panel_t > 0.0 {
            let y = self.panel_y();
            self.spr(out, "modepanel_window", 0.0, y * UY, None);
            let top = (y + PANEL_BTN_Y) * UY;
            // a dialog over the panel (quick load, previous / next scene) takes the object focus (sub_AE0E0), so
            // no panel button shows as pointed until sub_4E170 is back in state 3
            let dialog = self.under || self.dlg_holding();
            let hover = self.touch.panel_hover.or(self.touch.panel_key).filter(|_| !dialog);
            let pressing = (self.touch.press.is_some() && self.touch.panel_hover.is_some()) || (self.enter_held && self.touch.panel_key.is_some());
            for i in 0..8 {
                // states (sub_ADE80 flag 2): previous scene unavailable -> dark; auto on -> lit
                let row = if (i == 0 && !game.can_prev_scene()) || !self.panel_ok(game, i) {
                    0.0
                } else if self.touch.flash[i] > 0.0 || (i == 2 && game.auto) || (pressing && hover == Some(i)) {
                    3.0
                } else if hover == Some(i) {
                    2.0
                } else {
                    1.0
                };
                let x = (PANEL_X0 + PANEL_DX * i as f32) * UX;
                self.sprite(out, "mode_panel_btn_ja", [x, top, BTN_W, BTN_H], Some([8.0 + i as f32 * 232.0, 8.0 + row * 72.0, BTN_W, BTN_H]));
            }
        }
    }

    pub(super) fn draw_ripples(&mut self, r: &Renderer, out: &mut Vec<Quad>) {
        self.touch.draw_ripples(r, out);
    }
}
