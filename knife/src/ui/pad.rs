//! The Switch controller, driven by the keyboard (see README.md for the key map).
//!
//! Game button bits (sub_F3EC4 remaps nn::hid through the table at 0x3C9C3C): A 0x2000, B 0x4000, X 0x1000,
//! Y 0x8000, L 0x400, R 0x800, ZL 0x100, ZR 0x200, Plus 0x8, Minus 0x1, D-pad 0x10 / 0x20 / 0x40 / 0x80
//! (up, right, down, left), left stick << 12, right stick << 16. The action masks (off_AE02C0, defaults at
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
    /// right stick up / down (also the mouse wheel)
    RUp,
    RDown,
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
        if !pressed || (repeat && !matches!(b, Pad::Up | Pad::Down | Pad::Left | Pad::Right | Pad::RUp | Pad::RDown)) {
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
            Pad::RUp => self.wheel(game, true),
            Pad::RDown => self.wheel(game, false),
            // sub_4CEA0 mask [11] (Y | B | ZL) closes the system menu; sub_28A90 mask [18] (B | X | ZL) the backlog
            Pad::Y | Pad::ZL if self.screen == Screen::Menu => self.cancel(game, r),
            Pad::X | Pad::ZL if matches!(self.screen, Screen::Backlog { .. }) => self.cancel(game, r),
            // CG viewer (sub_C56C): Minus switches between the fit and the fill zoom
            Pad::Minus if matches!(self.screen, Screen::CgView { .. }) => self.cg_fill(),
            _ => {}
        }
    }

    /// sub_40ED0 / sub_4E170 with the story on screen.
    fn pad_story(&mut self, game: &mut Game, r: &Renderer, b: Pad, repeat: bool) {
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
        if game.in_choice() {
            match b {
                Pad::Up | Pad::RUp => game.choice_move(-1),
                Pad::Down | Pad::RDown => game.choice_move(1),
                Pad::Left => game.choice_side(false),
                Pad::Right => game.choice_side(true),
                Pad::A => game.advance(),
                Pad::Y => self.cancel(game, r),
                _ => {}
            }
            return;
        }
        if panel != Panel::Closed {
            // state 3 of a panel opened with a button: left / right move along the buttons (wrapping only on
            // a fresh press), A presses one (1 and 3 act while held, see touch_update); any of down, R-stick
            // down, B or Y closes it (masks [22] | [10], less left / right). A panel opened by touch ignores
            // the pad, and the story keys wait until it has closed.
            if panel == Panel::Open && self.touch.panel_pad {
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
                    Pad::Down | Pad::RDown | Pad::B | Pad::Y => {
                        self.touch.panel = Panel::Closing;
                        None
                    }
                    _ => None,
                };
                if let Some(k) = key {
                    // sub_4E170: sound 0 as the selection moves
                    game.play_sys_sound(0);
                    self.touch.panel_key = Some(k);
                    self.touch.panel_last = k;
                }
            }
            return;
        }
        // the story's buttons act on the press (the trigger set), not on key repeat
        if repeat {
            return;
        }
        match b {
            // A shows a hidden window (and does not advance)
            Pad::A | Pad::Down | Pad::RDown if self.touch.hidden => self.show_window(),
            Pad::A | Pad::Down | Pad::RDown => game.advance(),
            // B: stops skip mode first, else hides / shows the window
            Pad::B if self.touch.skip_mode => self.touch.skip_mode = false,
            Pad::B if self.touch.hidden => self.show_window(),
            Pad::B => self.touch.hidden = true,
            // Y opens the system menu, ending skip mode and showing the window
            Pad::Y => {
                self.touch.skip_mode = false;
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
            Pad::R if !self.touch.hidden => self.touch.skip_mode = !self.touch.skip_mode,
            // mask [21] opens the mode panel on its last button
            Pad::Up | Pad::RUp if !self.touch.hidden => {
                self.touch.panel = Panel::Opening;
                self.touch.panel_pad = true;
                self.touch.panel_key = Some(self.touch.panel_last);
            }
            _ => {}
        }
    }

    /// CG viewer Minus (sub_C56C): the scale that fills the screen, or back to the fit scale from there.
    fn cg_fill(&mut self) {
        let [w, h] = self.cg_size;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let fit = (super::W / w).min(super::H / h);
        let fill = (super::W / w).max(super::H / h).min(fit.max(1.0));
        self.cg_zoom = if (self.cg_zoom - fill).abs() < 1e-4 { 0.0 } else { fill };
        self.cg_pan = [0.0; 2];
    }
}
