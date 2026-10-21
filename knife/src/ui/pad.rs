//! The Switch controller, driven by the keyboard (see README.md for the key map).
//!
//! Game button bits (sub_F3EC4 remaps nn::hid through the table at 0x3C9C3C): A 0x2000, B 0x4000, X 0x1000,
//! Y 0x8000, L 0x400, R 0x800, ZL 0x100, ZR 0x200, Plus 0x8, Minus 0x1, D-pad 0x10 / 0x20 / 0x40 / 0x80
//! (up, right, down, left), left stick 0x10000.. 0x80000, right stick 0x100000.. 0x800000 (sub_B37D0). Menus and
//! the story take the right stick with the d-pad (masks such as 0x100010), so a controller's right stick drives
//! `Up` / `Down` / `Left` / `Right`; the left stick is its own `LUp`.. `LRight` plus the analog `Ui::stick` that
//! scrolls lists (`stick_frame`). The action masks (off_AE02C0, defaults at
//! 0xAC0AC8) used in the story (sub_40ED0): [0] A advance, [1] B cancel / stop skip, [10] Y system menu,
//! [12] R skip on / off, [13] L auto on / off, [17] X backlog, [19] ZR fast forward, [20] ZL rewind,
//! [21] up / R-stick up mode panel, [22] down advance / close the panel, [25] B window hide toggle.

use knife_render::Renderer;

use super::{Screen, Ui, touch::Panel};
use crate::game::Game;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pad {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    X,
    Y,
    L,
    R,
    ZL,
    ZR,
    /// right stick press (R3)
    R3,
    /// left stick directions (past 80 of ±128)
    LUp,
    LDown,
    LLeft,
    LRight,
    Minus,
    Plus,
}

impl Ui {
    /// A controller button went down (`pressed`) or up. Key repeat only moves cursors.
    pub fn pad(&mut self, game: &mut Game, r: &Renderer, b: Pad, pressed: bool, repeat: bool) {
        // held buttons
        match b {
            Pad::A => self.enter_held = pressed,
            Pad::ZL => self.touch.zl = pressed.then_some(self.touch.zl.unwrap_or(0.0)),
            Pad::ZR => self.touch.zr = pressed.then_some(self.touch.zr.unwrap_or(0.0)),
            _ => {}
        }
        if !pressed || (repeat && !matches!(b, Pad::Up | Pad::Down | Pad::Left | Pad::Right | Pad::LUp | Pad::LDown | Pad::LLeft | Pad::LRight)) {
            return;
        }
        if self.screen == Screen::Game && self.popup.is_none() {
            self.pad_story(game, r, b, repeat);
            return;
        }
        self.nav_repeat = repeat;
        match b {
            Pad::Up => self.key_nav(game, r, -1, 0, false),
            Pad::Down => self.key_nav(game, r, 1, 0, false),
            Pad::Left => self.key_nav(game, r, 0, -1, false),
            Pad::Right => self.key_nav(game, r, 0, 1, false),
            Pad::A => self.key_nav(game, r, 0, 0, true),
            Pad::B => self.cancel(game, r),
            Pad::L => self.shoulder(game, r, -1),
            Pad::R => self.shoulder(game, r, 1),
            Pad::LUp | Pad::LDown | Pad::LLeft | Pad::LRight => self.stick_pad(game, r, b),
            // sub_4CEA0 mask [11] (Y | B | ZL) closes the system menu; sub_28A90 mask [18] (B | X | ZL) the backlog
            Pad::Y | Pad::ZL if self.screen == Screen::Menu => self.cancel(game, r),
            Pad::X | Pad::ZL if matches!(self.screen, Screen::Backlog { .. }) => self.cancel(game, r),
            Pad::ZR if matches!(self.screen, Screen::Backlog { .. }) => self.shoulder(game, r, -1),
            // CG viewer (sub_C56C): Minus zooms to fill the screen, R3 back to the fit scale, centred; sound 0
            // the gallery movie player takes B / Plus (mask 0x4008) only
            Pad::Plus if self.screen == Screen::MoviePlay => self.gal_movie_skip(game),
            Pad::Minus if matches!(self.screen, Screen::CgView { .. }) && !self.busy() => {
                game.play_sys_sound(0);
                self.cg_fill();
            }
            Pad::R3 if matches!(self.screen, Screen::CgView { .. }) && !self.busy() => {
                game.play_sys_sound(0);
                self.cg_zoom = 0.0;
                self.cg_pan = [0.0; 2];
            }
            _ => {}
        }
    }

    /// sub_40ED0 / sub_4E170 with the story on screen.
    fn pad_story(&mut self, game: &mut Game, r: &Renderer, b: Pad, repeat: bool) {
        // a scene countdown: sub_40ED0 returns before reading the buttons
        if self.touch.scene_cd.is_some() {
            return;
        }
        let panel = self.touch.panel;
        // a story movie (sub_40480) takes only B / Plus, which skip it when it may be skipped
        if game.in_movie() {
            match b {
                Pad::B | Pad::Plus if !repeat => game.skip_movie(),
                Pad::A if !repeat => game.advance(),
                _ => {}
            }
            return;
        }
        // the staff roll task (off_AE0650 = 1): the system menu, backlog, panel, auto, skip and rewind wait; A
        // advances (ZR fast-forwards, see touch_update)
        if game.mode_task() && !game.in_choice() {
            if b == Pad::A && !repeat {
                if self.touch.hidden {
                    self.show_window();
                } else {
                    game.advance();
                }
            }
            return;
        }
        if panel != Panel::Closed {
            // state 3 (however the panel opened, state 1 sets the button request): left / right move along
            // the buttons (wrapping only on a fresh press), A presses one (1 and 3 act while held, see
            // touch_update); any of down, R-stick down, B or Y closes it (masks [22] | [10], less left / right).
            // The story keys and a choice wait until it has closed (sub_83E50 returns while sub_4E170 reports
            // the panel busy, AE06C0).
            if panel == Panel::Open {
                let k = self.touch.panel_key.unwrap_or(self.touch.panel_last);
                let key = match b {
                    Pad::Right if k < 7 => Some(k + 1),
                    Pad::Right if !repeat => Some(0),
                    Pad::Left if k > 0 => Some(k - 1),
                    Pad::Left if !repeat => Some(7),
                    Pad::A if k != 1 && k != 3 => {
                        self.panel_press(game, r, k);
                        None
                    }
                    Pad::Down | Pad::B | Pad::Y => {
                        self.touch.panel = Panel::Closing;
                        None
                    }
                    _ => None,
                };
                if let Some(k) = key {
                    // sub_4E170: sound 0 as the selection moves; the pad takes the focus from the pointer
                    game.play_sys_sound(0);
                    self.touch.panel_hover = None;
                    self.touch.panel_key = Some(k);
                    self.touch.panel_last = k;
                }
            }
            return;
        }
        if game.in_choice() {
            match b {
                Pad::Up => game.choice_move(-1),
                Pad::Down => game.choice_move(1),
                Pad::Left => game.choice_side(false),
                Pad::Right => game.choice_side(true),
                Pad::A => game.advance(),
                Pad::Y => self.cancel(game, r),
                _ => {}
            }
            return;
        }
        // the story's buttons act on the press (the trigger set), not on key repeat
        if repeat {
            return;
        }
        match b {
            // A shows a hidden window (and does not advance)
            Pad::A | Pad::Down if self.touch.hidden => self.show_window(),
            Pad::A | Pad::Down => game.advance(),
            // B: stops skip mode first, else hides / shows the window
            Pad::B if game.skip_mode => game.skip_mode = false,
            Pad::B if self.touch.hidden => self.show_window(),
            Pad::B => self.touch.hidden = true,
            // Y opens the system menu, ending skip mode and showing the window
            Pad::Y if !game.menu_lock => {
                game.skip_mode = false;
                self.show_window();
                self.cancel(game, r);
            }
            Pad::X if game.backlog_len() > 0 => {
                // sub_28950: the backlog opens with sound 3
                self.snd(game, 3);
                self.push(Screen::Backlog { scroll: 0 });
            }
            Pad::L => self.toggle_auto(game),
            // R toggles skip mode (off_AE0660 bit 1) unless the window is hidden
            Pad::R if !self.touch.hidden => game.skip_mode = !game.skip_mode,
            // mask [21] opens the mode panel on its last button
            Pad::Up if !self.touch.hidden => {
                self.touch.panel = Panel::Opening;
                self.touch.panel_key = Some(self.touch.panel_last);
            }
            _ => {}
        }
    }

    /// Left stick directions: the CG grid moves its cursor left / right with them (sub_9CBC masks 0x880080 /
    /// 0x220020), the flowchart scrolls by the stick's tilt per press and repeat (sub_11418, bits 0x10000 /
    /// 0x40000): (|v| - 40) * 0.022727 * 504 from where it is over 1/6 s, then the cursor follows (sub_12D1C).
    /// Every other screen reads the stick only as a scroll value (`stick_frame`).
    fn stick_pad(&mut self, game: &mut Game, r: &Renderer, b: Pad) {
        if self.busy() || self.popup.is_some() {
            return;
        }
        match (self.screen, b) {
            (Screen::Gallery, Pad::LLeft | Pad::LRight) if !self.gal_side => {
                self.key_nav(game, r, 0, if b == Pad::LLeft { -1 } else { 1 }, false);
            }
            (Screen::Flowchart { .. }, Pad::LUp | Pad::LDown) => {
                let v = self.stick[1];
                let d = if b == Pad::LUp { -(v - 40) } else { -40 - v } as f32 * 0.022727 * 504.0;
                self.flow_scroll_by_hand(self.flow_ref + d);
            }
            _ => {}
        }
    }

    /// One 60 Hz frame of the left stick's analog scroll (`stick`, ±128, y up):
    /// - CG grid (sub_9CBC) / music list (sub_1A7FC), past ±40: the list heads for (|v| - 40) * 0.011364 * 486.5
    ///   (music: 0.0087413 * 767) beyond where it is over 1/6 s, and the cursor row is pulled into view;
    /// - config (sub_2B930): the tab's scroll target moves by trunc(-v / 3) and tweens there (sub_9AE58);
    /// - save list (sub_50FC0): past ±12, v² adds up (dword_F1F478, cleared at |v| <= 11) and every 14401 steps
    ///   the cursor a save up / down like the d-pad without wrapping.
    pub(super) fn stick_frame(&mut self, game: &mut Game, r: &Renderer) {
        if self.busy() || self.popup.is_some() {
            self.wheel = 0.0;
            self.wheel_press = 0.0;
            self.wheel_steps = 0.0;
            return;
        }
        // the mouse wheel / trackpad holds the stick tilted (`Ui::wheel`) while a controller or WASD does not
        let mut v = self.stick[1];
        if self.wheel != 0.0 {
            let s = self.wheel.signum();
            if v == 0 {
                v = super::WHEEL_TILT * s as i32;
            }
            self.wheel -= s * self.wheel.abs().min(1.0);
            // the flowchart reads the stick per press, repeated every 5 frames: one press per notch's frames
            if matches!(self.screen, Screen::Backlog { .. }) {
                self.wheel_press += s;
            }
            if matches!(self.screen, Screen::Flowchart { .. }) {
                self.wheel_press += s;
                if self.wheel_press.abs() >= super::WHEEL_FRAMES {
                    self.wheel_press = 0.0;
                    let d = -((v.abs() - 40) * v.signum()) as f32 * 0.022727 * 504.0;
                    self.flow_scroll_by_hand(self.flow_ref + d);
                }
            }
        } else {
            self.wheel_press = 0.0;
        }
        match self.screen {
            // CG viewer (sub_C56C): the left stick moves the picture by (-x, y) / 8 per frame (sub_D270); the
            // right stick past ±40 zooms by y / 4000 per frame about the picture centre
            Screen::CgView { .. } => {
                let [x, y] = [self.stick[0], v];
                self.cg_pan[0] -= x as f32 * 0.125;
                self.cg_pan[1] += y as f32 * 0.125;
                let ry = self.rstick;
                if ry.abs() > 40 && self.cg_size[0] > 0.0 {
                    let [w, h] = self.cg_size;
                    let fit = (super::W / w).min(super::H / h);
                    let z = if self.cg_zoom <= 0.0 { fit } else { self.cg_zoom };
                    self.cg_zoom = (z + ry as f32 * 0.03125 * 0.008).clamp(fit, fit.max(1.0));
                }
            }
            Screen::Gallery | Screen::Music if !self.gal_side && v.abs() > 40 => {
                // the list scrolls whenever it has the focus (sub_9CBC / sub_1A7FC run for the list, not for the
                // music panel); the mouse leaving every row does not take the focus away
                let cur = self.hover.and_then(|h| self.items(game).get(h).map(|(_, a)| *a));
                let k = match (self.screen, cur) {
                    (_, Some(super::Action::MusicCtl(_) | super::Action::MusicVol)) => return,
                    (Screen::Gallery, Some(super::Action::Cg(_)) | None) => 0.011364 * 486.5,
                    (Screen::Music, Some(super::Action::Track(_)) | None) => 0.0087413 * 767.0,
                    _ => return,
                };
                let max = self.gal_scroll_max();
                let d = -((v.abs() - 40) * v.signum()) as f32 * k;
                self.gal_scroll.to((self.gal_scroll.pos + d).trunc(), max, true);
                self.gal_track = true;
            }
            // the backlog ignores the left stick; the wheel steps pages as the touch scroll does (sub_28A90:
            // touch + 60 >= 1 older, < 0 newer, without wrapping), one page per notch
            Screen::Backlog { scroll } if self.wheel_press.abs() >= super::WHEEL_FRAMES => {
                let up = self.wheel_press > 0.0;
                self.wheel_press = 0.0;
                let max = game.backlog_len().saturating_sub(1);
                let next = if up { (scroll + 1).min(max) } else { scroll.saturating_sub(1) };
                self.backlog_to(game, next);
            }
            Screen::Config if !self.side && v != 0 => {
                self.conf_scroll_to(self.conf_target[self.conf_tab] - (v / 3) as f32);
            }
            Screen::SaveLoad { .. } if !self.side => {
                if v.abs() <= 11 {
                    self.stick_acc = 0;
                }
                let dy = if v >= 13 {
                    self.stick_acc -= v * v;
                    (self.stick_acc <= -14401).then_some(-1)
                } else if v <= -13 {
                    self.stick_acc += v * v;
                    (self.stick_acc >= 14401).then_some(1)
                } else {
                    None
                };
                if let Some(dy) = dy {
                    self.stick_acc = 0;
                    self.nav_repeat = true;
                    self.key_nav(game, r, dy, 0, false);
                }
                // the wheel / trackpad scrolls the four frames a row per notch (frame height of travel); the
                // cursor stays on its save unless that leaves the view
                while self.wheel_steps.abs() >= 1.0 {
                    let s = self.wheel_steps.signum();
                    self.wheel_steps -= s;
                    self.save_scroll_by(game, -s as i32);
                }
            }
            _ => {}
        }
    }

    /// CG viewer Minus (sub_C56C): the scale that fills the screen along the picture's shorter side (the
    /// other side when that equals the fit scale), at most the maximum; the pan is kept and clamped.
    fn cg_fill(&mut self) {
        let [w, h] = self.cg_size;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (sw, sh) = (super::W / w, super::H / h);
        let fit = sw.min(sh);
        let mut fill = if w >= h { sh } else { sw };
        if (fit - fill).abs() < 1.1920929e-7 {
            fill = if w >= h { sw } else { sh };
        }
        self.cg_zoom = fill.min(fit.max(1.0));
    }
}
