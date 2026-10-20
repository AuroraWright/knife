//! Game controllers, turned into the same Switch buttons the keyboard drives (`ui::Pad`); all controllers act as
//! one. gilrs reads them (after shin-input's `RawInputAccumulator`), except on macOS: there, once another app
//! uses a PlayStation / Xbox pad through the GameController framework, raw HID clients such as gilrs stop
//! getting it until it reconnects, so knife polls the GameController framework itself (as SDL does). Directions repeat like the exe's pad layer
//! (sub_B37D0): the d-pad and both sticks fire when first held, again after 15 frames, then every 5 (the rates
//! sub_F268C stores at +1000 / +1004), one counter that runs while any direction is held. A stick direction is
//! held past 80 of ±128. The game reads the right stick with the d-pad, so it drives `Up`.. `Right`; the left
//! stick has its own directions and its summed tilt (`left`) scrolls and pans.

use std::collections::HashMap;


use crate::ui::Pad;

const PADS: [Pad; 19] = [
    Pad::Up,
    Pad::Down,
    Pad::Left,
    Pad::Right,
    Pad::A,
    Pad::B,
    Pad::X,
    Pad::Y,
    Pad::L,
    Pad::R,
    Pad::ZL,
    Pad::ZR,
    Pad::LUp,
    Pad::LDown,
    Pad::LLeft,
    Pad::LRight,
    Pad::Minus,
    Pad::Plus,
    Pad::R3,
];

/// d-pad and stick directions (game mask 0xFF00F0), the buttons that repeat
const DIRECTIONS: [Pad; 8] = [Pad::Up, Pad::Down, Pad::Left, Pad::Right, Pad::LUp, Pad::LDown, Pad::LLeft, Pad::LRight];

/// first repeat after this many frames, then one per `REPEAT` frames
const REPEAT_DELAY: f32 = 15.0;
const REPEAT: f32 = 5.0;
/// stick threshold: 80 of the exe's ±128 scale
const STICK: f32 = 80.0 / 128.0;

fn index(p: Pad) -> usize {
    PADS.iter().position(|&q| q == p).unwrap()
}

#[derive(Default)]
struct Controller {
    buttons: [bool; PADS.len()],
    left: [f32; 2],
    right: [f32; 2],
}

impl Controller {
    /// Buttons plus the stick directions.
    fn now_held(&self) -> [bool; PADS.len()] {
        let mut h = self.buttons;
        let [lx, ly] = self.left;
        h[index(Pad::LUp)] |= ly > STICK;
        h[index(Pad::LDown)] |= ly < -STICK;
        h[index(Pad::LLeft)] |= lx < -STICK;
        h[index(Pad::LRight)] |= lx > STICK;
        let [rx, ry] = self.right;
        h[index(Pad::Up)] |= ry > STICK;
        h[index(Pad::Down)] |= ry < -STICK;
        h[index(Pad::Left)] |= rx < -STICK;
        h[index(Pad::Right)] |= rx > STICK;
        h
    }
}

/// A button event for `Ui::pad`: (button, pressed, repeat).
pub type PadEvent = (Pad, bool, bool);

pub struct Gamepads {
    backend: backend::Backend,
    /// by backend id
    pads: HashMap<usize, Controller>,
    held: [bool; PADS.len()],
    /// frames to the next direction repeat while one is held
    repeat: Option<f32>,
}

impl Gamepads {
    pub fn new() -> Self {
        Self { backend: backend::Backend::new(), pads: HashMap::new(), held: [false; PADS.len()], repeat: None }
    }

    /// The left stick summed over the controllers, each axis in -128..128 with y up (sub_B37D0: x * 128
    /// truncated per controller, the sum clamped).
    pub fn left(&self) -> [i32; 2] {
        let mut v = [0i32; 2];
        for c in self.pads.values() {
            for (a, s) in v.iter_mut().zip(c.left) {
                *a = (*a + (s * 128.0) as i32).clamp(-128, 128);
            }
        }
        v
    }

    /// The right stick's y summed over the controllers like `left` (the CG viewer zoom).
    pub fn right_y(&self) -> i32 {
        self.pads.values().fold(0, |a, c| (a + (c.right[1] * 128.0) as i32).clamp(-128, 128))
    }

    /// Read the controllers after `frames` 60 Hz frames: presses, releases and direction repeats.
    pub fn poll(&mut self, frames: f32) -> Vec<PadEvent> {
        self.backend.read(&mut self.pads);
        self.step(frames)
    }

    /// Button changes and direction repeats since the last pass, `frames` 60 Hz frames ago.
    fn step(&mut self, frames: f32) -> Vec<PadEvent> {
        let mut now = [false; PADS.len()];
        for c in self.pads.values() {
            for (n, h) in now.iter_mut().zip(c.now_held()) {
                *n |= h;
            }
        }
        let mut out = Vec::new();
        for (i, &p) in PADS.iter().enumerate() {
            if now[i] != self.held[i] {
                out.push((p, now[i], false));
            }
        }
        let fresh: Vec<bool> = (0..PADS.len()).map(|i| now[i] && !self.held[i]).collect();
        self.held = now;
        let dirs: Vec<usize> = DIRECTIONS.iter().map(|&p| index(p)).filter(|&i| now[i]).collect();
        if dirs.is_empty() {
            self.repeat = None;
            return out;
        }
        let Some(t) = &mut self.repeat else {
            self.repeat = Some(REPEAT_DELAY);
            return out;
        };
        // the exe steps the counter by the frames since its last pass, at most 2
        *t -= frames.min(2.0);
        if *t <= 0.0 {
            *t = REPEAT;
            out.extend(dirs.into_iter().filter(|&i| !fresh[i]).map(|i| (PADS[i], true, true)));
        }
        out
    }
}

#[cfg(not(target_os = "macos"))]
mod backend {
    use std::collections::HashMap;

    use gilrs::{Axis, Button, EventType, Gilrs};

    use super::{Controller, Pad, index};

    /// The Switch button a controller button stands for (shin-input's mapping: gilrs already normalises the
    /// labelled face buttons to South / East / West / North, so they are not swapped again).
    fn button(b: Button) -> Option<Pad> {
        Some(match b {
            Button::South => Pad::A,
            Button::East => Pad::B,
            Button::West => Pad::X,
            Button::North => Pad::Y,
            Button::Start => Pad::Plus,
            Button::Select => Pad::Minus,
            Button::LeftTrigger => Pad::L,
            Button::RightTrigger => Pad::R,
            Button::LeftTrigger2 => Pad::ZL,
            Button::RightTrigger2 => Pad::ZR,
            Button::DPadUp => Pad::Up,
            Button::DPadDown => Pad::Down,
            Button::DPadLeft => Pad::Left,
            Button::DPadRight => Pad::Right,
            Button::RightThumb => Pad::R3,
            _ => return None,
        })
    }

    pub struct Backend(Option<Gilrs>);

    impl Backend {
        pub fn new() -> Self {
            Self(match Gilrs::new() {
                Ok(g) => Some(g),
                Err(gilrs::Error::NotImplemented(g)) => {
                    tracing::warn!("gamepad input is not implemented on this platform");
                    Some(g)
                }
                Err(e) => {
                    tracing::error!("gamepad input: {e}");
                    None
                }
            })
        }

        pub fn read(&mut self, pads: &mut HashMap<usize, Controller>) {
            let Some(gilrs) = &mut self.0 else { return };
            while let Some(ev) = gilrs.next_event() {
                let c = pads.entry(ev.id.into()).or_default();
                match ev.event {
                    EventType::ButtonPressed(b, _) | EventType::ButtonReleased(b, _) => {
                        if let Some(p) = button(b) {
                            c.buttons[index(p)] = matches!(ev.event, EventType::ButtonPressed(..));
                        }
                    }
                    EventType::AxisChanged(axis, v, _) => match axis {
                        Axis::LeftStickX => c.left[0] = v,
                        Axis::LeftStickY => c.left[1] = v,
                        Axis::RightStickX => c.right[0] = v,
                        Axis::RightStickY => c.right[1] = v,
                        _ => {}
                    },
                    EventType::Connected => {
                        tracing::info!("controller connected: {}", gilrs.gamepad(ev.id).name());
                        *c = Controller::default();
                    }
                    EventType::Disconnected => *c = Controller::default(),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use std::collections::HashMap;

    use objc2_game_controller::{GCController, GCControllerButtonInput, GCControllerDirectionPad, GCDevice};

    use super::{Controller, Pad, index};

    /// Polls `GCController.controllers` (kept up to date by the main run loop winit drives).
    pub struct Backend {
        /// controller count when last logged
        names: usize,
    }

    impl Backend {
        pub fn new() -> Self {
            Self { names: 0 }
        }

        pub fn read(&mut self, pads: &mut HashMap<usize, Controller>) {
            pads.clear();
            // SAFETY: plain property reads of the framework's controller objects on the main thread.
            unsafe {
                let list = GCController::controllers();
                if list.count() != self.names {
                    self.names = list.count();
                    for c in list.iter() {
                        let name = c.vendorName().map(|n| n.to_string()).unwrap_or_default();
                        tracing::info!("controller: {name}");
                    }
                }
                for (i, c) in list.iter().enumerate() {
                    let Some(g) = c.extendedGamepad() else { continue };
                    let mut s = Controller::default();
                    let mut set = |p: Pad, b: &GCControllerButtonInput| s.buttons[index(p)] = b.isPressed();
                    // the framework's names follow the Xbox layout: buttonA is the bottom face button
                    set(Pad::A, &g.buttonA());
                    set(Pad::B, &g.buttonB());
                    set(Pad::X, &g.buttonX());
                    set(Pad::Y, &g.buttonY());
                    set(Pad::Plus, &g.buttonMenu());
                    if let Some(b) = g.buttonOptions() {
                        set(Pad::Minus, &b);
                    }
                    set(Pad::L, &g.leftShoulder());
                    set(Pad::R, &g.rightShoulder());
                    set(Pad::ZL, &g.leftTrigger());
                    set(Pad::ZR, &g.rightTrigger());
                    if let Some(b) = g.rightThumbstickButton() {
                        set(Pad::R3, &b);
                    }
                    let d = g.dpad();
                    set(Pad::Up, &d.up());
                    set(Pad::Down, &d.down());
                    set(Pad::Left, &d.left());
                    set(Pad::Right, &d.right());
                    let stick = |p: &GCControllerDirectionPad| [p.xAxis().value(), p.yAxis().value()];
                    s.left = stick(&g.leftThumbstick());
                    s.right = stick(&g.rightThumbstick());
                    pads.insert(i, s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_repeat_like_the_exe() {
        let mut g = Gamepads { backend: backend::Backend::new(), pads: HashMap::new(), held: [false; PADS.len()], repeat: None };
        g.pads.insert(0, Controller { left: [0.0, -0.9], ..Default::default() });
        // pressed at once, repeats after 15 frames, then every 5
        let mut log = Vec::new();
        for f in 0..30 {
            for e in g.step(1.0) {
                log.push((f, e));
            }
        }
        let frames: Vec<i32> = log.iter().map(|(f, _)| *f).collect();
        assert_eq!(frames, [0, 15, 20, 25]);
        assert_eq!(log[0].1, (Pad::LDown, true, false));
        assert_eq!(log[1].1, (Pad::LDown, true, true));
        assert_eq!(g.left(), [0, -115]);
        // below the threshold the stick lets go
        g.pads.values_mut().next().unwrap().left = [0.0, -0.5];
        assert_eq!(g.step(1.0), [(Pad::LDown, false, false)]);
        assert!(g.repeat.is_none());
        // the right stick is the d-pad
        g.pads.values_mut().next().unwrap().right = [0.9, 0.0];
        assert_eq!(g.step(1.0), [(Pad::Right, true, false)]);
    }
}
