//! Touch screen effects and story gestures, driven by the mouse (left button = finger).
//!
//! - Ripples (sub_AD4E0 -> sub_1034, updated / drawn by sub_E00): every touch-down once the game task runs
//!   (sub_AFA40 draws them), i.e. from the title or the fresh-profile prologue on; the launch task
//!   (sub_B5650: TM_CI logo, caution2 notice) draws none.
//! - Story drag arrow (sub_40ED0, drawn by sub_3C510): a horizontal drag fast-forwards or rewinds (Tsukihime
//!   only: Witch on the Holy Night's jp_40ED0 dropped it, and its sub_1D670 draws a spinner instead).
//! - Message window hiding at the bottom edge (sub_40ED0).
//! - Mode panel at the top edge (sub_4E170 state machine, drawn by sub_4DE10).
//!
//! Positions in the exe's input state are 640x448 units (touch x / 2, y * 448 / 720).

use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Renderer, TexRef, Upload};

use super::{Confirm, Obj, Screen, Ui, add_copy};
use crate::{game::Game, save};

const W: f32 = CANVAS_W as f32;
const H: f32 = CANVAS_H as f32;
const UX: f32 = W / 640.0;
const UY: f32 = H / 448.0;
/// Float error allowed in a count of 60 Hz frames (a 60 Hz display's frame is 0.99999994 of one)
const FRAME_EPS: f32 = 1e-4;

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

/// knife: the pointer once a 60 Hz frame since the press, newest first, for the release's flick. The touch panel
/// judges a flick by the move over its last two frames, which a mouse or trackpad drag rarely makes: the finger
/// slows down before it lifts. As shin does for its desktop gestures (`desktop_gesture_vector`), a drag that
/// went 64 canvas pixels from its start flicks by that whole move, and a shorter one by its speed over the last
/// 80 ms (5 frames), scaled to the two frames the exe compares.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Trail {
    pts: [[f32; 2]; 6],
    n: usize,
}

impl Trail {
    pub(crate) fn reset(&mut self, p: [f32; 2]) {
        self.pts = [p; 6];
        self.n = 0;
    }

    pub(crate) fn push(&mut self, p: [f32; 2]) {
        self.pts.copy_within(0..5, 1);
        self.pts[0] = p;
        self.n = (self.n + 1).min(5);
    }

    /// The release's move in 640x448 units, as the exe's two-frame flick test reads it.
    pub(crate) fn flick(&self, start: [f32; 2], end: [f32; 2]) -> [f32; 2] {
        let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
        if (dx * CANVAS_W as f32 / 640.0).hypot(dy * CANVAS_H as f32 / 448.0) >= 64.0 {
            return [dx, dy];
        }
        // at least the exe's two frames (the press point fills the older ones), so a quick tap is not scaled up
        let k = self.n.max(2);
        let o = self.pts[k];
        let f = 2.0 / k as f32;
        [(end[0] - o[0]) * f, (end[1] - o[1]) * f]
    }
}

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
    /// frames the button has been down (sub_AD4E0: release frame - qword_17FC720)
    down_t: f32,
    swallow: bool,
    /// frames left of touch visibility (off_AE02C8 + 0xC: 256 while a finger is down, then counting down),
    /// the button / finger is down (+8), and where it last was (units, +0x2C / +0x30: it stays there after
    /// the release). Only a held button moves the touch: a mouse that merely hovers is no finger on the
    /// screen.
    visible: f32,
    pub(super) down: bool,
    pub(super) at: [f32; 2],
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
    /// the last rewind step went back (AE05F8, jp_814C0's sub_3B410)
    rewound: bool,
    /// Witch on the Holy Night: 60 Hz frames left of the normal frame after a rewind step (off_AE05B8 = 2)
    rewind_rest: f32,
    /// what holds the rewind: ZL (0), the mode panel's button (1) or the drag (2)
    rewind_by: u8,
    /// Witch on the Holy Night's mode display (sub_1D670)
    wspin: super::witch::WSpin,
    /// message window hidden at the bottom edge (off_AE0690)
    pub hidden: bool,
    /// mode panel (dword_B35A10), its slide (dword_B35A28), its hovered button, the button held
    /// and how long (dword_AFFCCC), and the per-button press flash (sub_AE040(.., 10))
    pub panel: Panel,
    pub(super) panel_t: f32,
    pub panel_hover: Option<usize>,
    /// the pointer moved since the last frame, pressed or not (the panel's buttons follow a hovering mouse
    /// as the other menus' items do)
    hovered: bool,
    held_frames: f32,
    pub(super) flash: [f32; 8],
    /// The buttons' object flag 2 (objects 207..214, sub_ADE80): previous scene / next scene / Q.SAVE /
    /// Q.LOAD unavailable, auto lit. The objects start without it (unk_5918C8: flags 0x320).
    pub(super) pflags: [bool; 8],
    /// Witch on the Holy Night: the buttons' object states (jp_AE840's crossfade)
    wbtn: [super::witch::WBtn; 8],
    /// The panel's selected button (HIDWORD(qword_B35A1C)), and the last one picked by either pointer or pad,
    /// which the next opening starts on (LODWORD)
    pub panel_key: Option<usize>,
    pub panel_last: usize,
    /// the pointer each frame since the press (units): the release flick reads it (sub_AD4E0 slot fields 7 / 9,
    /// knife's desktop policy in `Trail`)
    trail: Trail,
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
            down_t: 0.0,
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
            rewound: false,
            rewind_rest: 0.0,
            rewind_by: 0,
            wspin: Default::default(),
            hidden: false,
            panel: Panel::Closed,
            panel_t: 0.0,
            panel_hover: None,
            hovered: false,
            held_frames: 0.0,
            flash: [0.0; 8],
            pflags: [false; 8],
            wbtn: Default::default(),
            panel_key: None,
            // sub_4DCC0: the panel's cursor starts on fast forward
            panel_last: 3,
            trail: Trail::default(),
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
    /// sub_4E170 state 3 closed by a button (0x4e460): the last touch's y (+0x30, and the pointer's +0x18) goes to
    /// 120 so a touch that opened the panel from the top 25 units does not open it again in state 1, and the
    /// press is used up for the frame (off_AE0490 = 1).
    /// Witch on the Holy Night (jp_4E170) puts it at 224 and sets off_632388, so a ZL that closed the panel
    /// does not rewind until it is let go.
    pub(super) fn close_panel_by_button(&mut self) {
        self.touch.panel = Panel::Closing;
        if crate::assets::witch() {
            self.touch.at[1] = 224.0;
            self.w.zl_block = true;
        } else {
            self.touch.at[1] = 120.0;
        }
    }

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
        self.touch.trail.reset(self.units());
        if !matches!(self.intro, Some(super::Intro::Logo { .. } | super::Intro::Notice { .. })) {
            self.touch.spawn(x, y);
        }
        if self.screen == Screen::Witch {
            self.touch.down = true;
            self.touch.at = self.units();
            self.witch_mouse_down();
            return;
        }
        if self.screen == Screen::Title && crate::assets::witch() {
            self.witch_mouse_down();
            // the load screen (state 25) and the popups take the touch through the input word only
            if self.wt.st == 25 || self.w.popup.busy != 0 {
                self.touch.down = true;
                return;
            }
        }
        self.touch.hovered = true;
        self.touch.visible = 256.0;
        self.touch.down = true;
        self.touch.down_t = 0.0;
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
        if self.popup.is_some() {
            // sub_49750 takes no touch-down: only the release closes the popup
            self.touch.press = Some(self.units());
            self.touch.dragged = false;
            return;
        }
        if !self.busy() && self.bar_grab(y) {
            return;
        }
        if matches!(self.screen, Screen::Game | Screen::CgView { .. }) {
            self.touch.press = Some(self.units());
            self.touch.dragged = false;
            return;
        }
        // the system menu acts on the touch at once but also reads its release swipe (sub_4CEA0)
        if self.screen == Screen::Menu {
            self.touch.press = Some(self.units());
            self.touch.dragged = false;
        }
        self.click(game, r);
    }

    pub fn mouse_up(&mut self, game: &mut Game, r: &Renderer) {
        self.touch.down = false;
        if self.screen == Screen::Witch {
            self.witch_mouse_up();
            return;
        }
        if self.screen == Screen::Title && crate::assets::witch() {
            self.witch_mouse_up();
        }
        self.drag.release();
        self.bar = None;
        let Some(start) = self.touch.press.take() else { return };
        let swallow = std::mem::take(&mut self.touch.swallow);
        if self.popup.is_some() {
            // sub_49750 closes on +0x34 bit 0x2000: a release after 7..199 frames held without a drag
            // (sub_AD4E0); a quicker tap leaves it up
            if !self.touch.dragged && (7.0..200.0).contains(&self.touch.down_t) {
                self.popup_input();
            }
            return;
        }
        if matches!(self.screen, Screen::CgView { .. }) {
            self.cg_flick(game, start);
            return;
        }
        // sub_4CEA0: a left or up swipe (+0x54 bits 1 / 4) closes the system menu like B
        if self.screen == Screen::Menu && self.touch.dragged && !self.busy() {
            let [dx, dy] = self.touch.trail.flick(start, self.units());
            let horiz = dx.abs() > dy.abs();
            if (horiz && dx <= -7.0) || (!horiz && dy <= -7.0) {
                self.cancel(game, r);
            }
            return;
        }
        if self.screen != Screen::Game {
            return;
        }
        let [x, y] = self.units();
        // the TeaTime menu: a tap released on its focused button presses it, or sends the Saved banner away
        if game.teatime_active() {
            if !self.touch.dragged && !swallow {
                game.teatime_click();
            }
            return;
        }
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
    /// stronger axis wins; `Trail::flick`) acted on in the story (sub_40ED0 / sub_4E170).
    fn flick(&mut self, game: &mut Game, r: &Renderer, start: [f32; 2], end: [f32; 2]) {
        let [dx, dy] = self.touch.trail.flick(start, end);
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
                } else if crate::assets::witch() {
                    // Witch on the Holy Night (jp_40ED0) the other way round: a right flick from x < 100 the
                    // backlog, a left flick from x >= 541 the system menu
                    if right && start[0] < 100.0 && self.wl_ok(game) {
                        self.wl_open(game, r);
                    } else if left && start[0] >= 541.0 {
                        self.cancel(game, r);
                    }
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
    fn cg_flick(&mut self, game: &mut Game, start: [f32; 2]) {
        let [dx, dy] = self.touch.trail.flick(start, self.units());
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

    /// TeaTime state 10 (ZL): the menu returns 2 and sub_40ED0 starts the rewind
    /// (off_AE05B8 = 1) with a step that frame (sub_814C0); it goes on while ZL or the drag holds.
    pub(super) fn teatime_rewind(&mut self, game: &mut Game, r: &Renderer) {
        if !game.teatime_rewind() {
            return;
        }
        self.touch.rewind = 1;
        self.touch.rewind_t = 0.0;
        self.touch.rewind_rest = 0.0;
        game.speed = 1;
        game.start_rewind(false);
        game.rewinding = true;
        if game.settled() {
            game.rewind_frame();
            self.rewind_step(game, r);
        }
    }

    /// Witch on the Holy Night, before the frame's script: the frame after a step (off_AE05B8 = 2) is an ordinary
    /// one while the rewind is still held (jp_40ED0 decides it first). Held ZL is no input to jp_802C0, so a
    /// restored message keeps its click wait at head 0 (nothing shows); after a step that restored nothing (the
    /// oldest entry already current) the wait is gone and the script goes on into the next message, which shows
    /// the restored one whole above it and pushes a ring entry for the next step to take back.
    pub fn rewind_before_script(&mut self, game: &mut Game) {
        game.rest_hold = false;
        if self.touch.rewind == 0 {
            return;
        }
        let held = match self.touch.rewind_by {
            0 => self.touch.zl.is_some(),
            1 => self.touch.press.is_some() || self.enter_held,
            _ => self.touch.press.is_some(),
        };
        if !held {
            return;
        }
        if self.touch.rewind_rest >= 1.0 - FRAME_EPS {
            game.end_rewind();
        } else {
            // (after the frame between two steps: its script ran once, the next step comes in the touch handling)
            game.rest_hold = !game.rewinding;
        }
    }

    /// One rewind step (sub_814C0). Witch on the Holy Night's jp_40ED0 then sets off_AE05B8 to 2 unless a choice
    /// was up (AE04B8 & 0x10 as the frame began): the next frame clears it and runs like any other, so a held
    /// rewind alternates its steps with frames that show the restored page and run the script (once a step has
    /// nothing to restore and cleared the click wait, the script goes on into the next message, which the next
    /// step takes back: the Switch blinks the loaded page while ZL holds right after a load).
    fn rewind_step(&mut self, game: &mut Game, r: &Renderer) {
        let choice = game.in_choice();
        self.touch.rewound = game.rewind_step(r);
        // (1.0.3's sub_41760 and the overseas build also write 2 after a step, but only test it against 0, and
        // the next held frame sets it back to 1 and steps again)
        if crate::assets::witch() && !choice {
            self.touch.rewind_rest = 1.0;
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
        let pos = [PANEL_X0 + PANEL_DX * i as f32, self.panel_y() + PANEL_BTN_Y];
        // Witch on the Holy Night: table 0x3cdfd8, SYSMES 145..152, rect (5, -4)-(70, 35)
        if crate::assets::witch() {
            return Some((super::witch::PANEL_HELP[i], pos, [5.0, -4.0, 70.0, 35.0], 0x320));
        }
        let id = [169, 171, 173, 172, 170, 174, 175, 176][i];
        Some((id, pos, [4.0, -10.0, 70.0, 36.0], 0x320))
    }

    /// sub_40ED0 enables the mode panel (sub_4E080(1)) in the story, also over a choice list; only the SELS / SELA
    /// question dialogs (byte_14715B0 / byte_14715B1), a scene countdown and the engine tasks turn it off.
    /// Witch on the Holy Night's state 1 also waits out a choice (jp_83B00: AE04B8 0x10 or a question) and a
    /// TeaTime scene (sub_3A820, which also covers the story movie's F036).
    pub(super) fn panel_allowed(&self, game: &Game) -> bool {
        self.screen == Screen::Game
            && !game.choice_question()
            && self.touch.scene_cd.is_none()
            && !game.mode_task()
            && !(crate::assets::witch() && (game.in_choice() || game.teatime_scene()))
    }

        fn panel_y(&self) -> f32 {
        // sub_4DE10: quad-out from -43.14 to 0 units over the slide
        let t = self.touch.panel_t;
        -43.14 - 43.14 * t * (t - 2.0)
    }

    /// Witch on the Holy Night hit-tests the objects' rect (5, -4)-(70, 35), inclusive (jp_AE840).
    fn panel_button(&self) -> Option<usize> {
        let [x, y] = self.units();
        let top = self.panel_y() + PANEL_BTN_Y;
        (0..8).find(|&i| {
            let x0 = PANEL_X0 + PANEL_DX * i as f32;
            if crate::assets::witch() {
                let (x0, y0) = ((x0 as i32 + 5).max(0) as f32, (top as i32 - 4).max(0) as f32);
                return (x0..=x0 + 65.0).contains(&x.trunc()) && (y0..=top.trunc() + 35.0).contains(&y.trunc());
            }
            x >= x0 && x < x0 + BTN_W / UX && y >= top && y < top + BTN_H / UY
        })
    }

    /// Witch on the Holy Night: button `i`'s object state (jp_AE840): 4 pressed, 1 / 3 focused (3 with flag 2),
    /// 2 flag 2 (unavailable; auto lit), else 0. Only the open panel's buttons take the focus, and none while a
    /// dialog has it.
    fn wpanel_state(&self, i: usize) -> u8 {
        let dialog = self.screen == Screen::Witch || self.w.dlg.busy();
        let focus = self.touch.panel_hover.or(self.touch.panel_key).filter(|_| !dialog && self.touch.panel == Panel::Open);
        let on = self.touch.pflags[i];
        if self.touch.flash[i] > 0.0 {
            4
        } else if focus == Some(i) {
            if on { 3 } else { 1 }
        } else if on {
            2
        } else {
            0
        }
    }

    /// Panel buttons 4..6 that sub_4E0C0 leaves enabled: next scene except in a gallery flowchart replay
    /// (sub_50F0), Q.SAVE / Q.LOAD unless saving is off (F025, sub_47B10).
    pub(super) fn panel_ok(&self, game: &Game, b: usize) -> bool {
        match b {
            // (Witch on the Holy Night has no gallery replay: nothing sets object 184's flag)
            4 => crate::assets::witch() || self.gal_ret[0] != 2,
            5 | 6 => game.var("F025") == 0,
            _ => true,
        }
    }

    /// sub_4E0C0: objects 211..213 take flag 2 for the unavailable next scene / Q.SAVE / Q.LOAD. It runs when
    /// the panel has slid open (sub_4E170 state 2), as the story starts (sub_48AC0 case 2) and, in the
    /// overseas build, as the panel opens (state 1).
    fn panel_flags_save(&mut self, game: &Game) {
        for b in 4..=6 {
            self.touch.pflags[b] = !self.panel_ok(game, b);
        }
    }

    /// sub_4E170 state 3, every frame but the one that closes it (the overseas build's sub_4F100, also called
    /// as the panel opens): object 209 lit in auto mode, 207 unavailable without a previous scene.
    fn panel_flags_scene(&mut self, game: &Game) {
        self.touch.pflags[2] = game.auto;
        self.touch.pflags[0] = !game.can_prev_scene();
    }

    /// A tapped mode panel button (sub_4E170 state 3 cases 0..7; 1 and 3 act while held, see `touch_update`).
    pub(super) fn panel_press(&mut self, game: &mut Game, r: &Renderer, b: usize) {
        if crate::assets::witch() {
            self.witch_panel_press(game, r, b);
            return;
        }
        // sub_AE040(.., 10) only on the cases that act (LABEL_84), and not while the object has flag 2 (auto
        // being turned off)
        let flagged = self.touch.pflags[b];
        // sub_4E170: a button that acts plays system sound 1 (before its dialog), an unavailable one sound 2
        let acted = match b {
            // previous / next scene and quick load ask first (dialogs 11, 10, 9)
            // cases 0 / 4 test the objects' flag 2 (sub_ADE60(207 / 211, 2)), 5 / 6 sub_47B10 itself
            0 if !self.touch.pflags[0] => {
                self.snd(game, 1);
                game.drop_capture();
                self.confirm(game, Confirm::PrevScene);
                true
            }
            2 => {
                self.snd(game, 1);
                self.toggle_auto(game);
                true
            }
            4 if !self.touch.pflags[4] => {
                self.snd(game, 1);
                game.drop_capture();
                self.confirm(game, Confirm::NextScene);
                true
            }
            // case 5 also wants dword_AFFCC4 run out (sub_40E90)
            5 if game.can_save() && self.panel_ok(game, 5) && self.save_cool == 0 => {
                self.snd(game, 1);
                self.quick_save(game, r);
                true
            }
            // case 6: sound 2 without a quick save (sub_52BF0(139) < 1)
            6 if self.panel_ok(game, 6) && self.slots.get(save::quick_slot()).is_some_and(Option::is_some) => {
                self.snd(game, 1);
                self.confirm(game, Confirm::QuickLoad);
                true
            }
            0 | 4 | 5 | 6 => {
                self.snd(game, 2);
                false
            }
            7 => {
                game.replay_voice();
                true
            }
            _ => false,
        };
        if acted && !flagged {
            self.touch.flash[b] = 10.0;
        }
    }

    /// Witch on the Holy Night's jp_4E170 state 3 cases 0..7: a button that acts plays sound 0 and is pressed
    /// for 10 frames (jp_AE040, not while its flag 2 is on); previous / next scene want config 0x32 and sound 6
    /// while unavailable, then ask when sub_53980 allows the jump (jp_40ED0 states 4 / 1: dialogs 11 / 10);
    /// Q.SAVE wants saving on (F025), the cooldown out (jp_40E90), a resumable point and no quick record at
    /// this point already (sub_359F0), else sound 1; Q.LOAD wants saving on and a quick record (sub_35B30(120)),
    /// else sound 1.
    fn witch_panel_press(&mut self, game: &mut Game, r: &Renderer, b: usize) {
        let jumps = game.settings.scene_jump;
        let acted = match b {
            0 | 4 if !jumps => false,
            0 | 4 if self.touch.pflags[b] => {
                self.snd(game, 6);
                false
            }
            0 | 4 => {
                self.snd(game, 0);
                let next = b == 4;
                if if next { game.next_scene_allowed() } else { game.prev_scene_allowed() } {
                    game.drop_capture();
                    self.wd_ask(game, r, if next { Confirm::NextScene } else { Confirm::PrevScene });
                }
                true
            }
            2 => {
                self.snd(game, 0);
                self.toggle_auto(game);
                true
            }
            5 => {
                let same = game
                    .save_data()
                    .zip(self.slots.get(save::quick_slot()).and_then(Option::as_ref))
                    .is_some_and(|(a, b)| a.scene == b.scene && a.pc == b.pc);
                if game.var("F025") == 0 && self.save_cool == 0 && game.can_save() && !same {
                    self.snd(game, 0);
                    self.show_banner(1);
                    self.write_quiet(game, r, save::quick_slot());
                    self.save_cool = 180;
                    game.play_sys_sound(13);
                    true
                } else {
                    self.snd(game, 1);
                    false
                }
            }
            6 => {
                if game.var("F025") == 0 && self.slots.get(save::quick_slot()).is_some_and(Option::is_some) {
                    self.snd(game, 0);
                    self.wd_ask(game, r, Confirm::QuickLoad);
                    true
                } else {
                    self.snd(game, 1);
                    false
                }
            }
            7 => {
                game.replay_voice();
                true
            }
            _ => false,
        };
        if acted && !self.touch.pflags[b] {
            self.touch.flash[b] = 10.0;
        }
    }

    /// Per frame (60 Hz frames `f`), after the game update.
    pub(super) fn touch_update(&mut self, game: &mut Game, r: &Renderer, secs: f32, f: f32) {
        // sub_48AC0 case 2 runs sub_4E0C0 as the story starts: outside the story the flags follow it
        if self.screen != Screen::Game && !self.stack.contains(&Screen::Game) {
            self.panel_flags_save(game);
        }
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
        let in_game = self.screen == Screen::Game
            && !game.in_choice()
            && !game.teatime_active()
            && self.touch.scene_cd.is_none()
            && !game.mode_task();
        let panel_game = self.panel_allowed(game);
        let held = self.touch.press.is_some();
        let [x, y] = self.touch.at;
        let hovered = std::mem::take(&mut self.touch.hovered);
        // the pointer is sampled once per 60 Hz frame, like the touch panel
        self.touch.trail_t += f;
        if self.touch.trail_t >= 1.0 {
            self.touch.trail_t = (self.touch.trail_t - 1.0).min(1.0);
            self.touch.trail.push([x, y]);
        }
        for h in [&mut self.touch.zl, &mut self.touch.zr].into_iter().flatten() {
            *h += f;
        }
        self.touch.visible = if self.touch.down { 256.0 } else { (self.touch.visible - f).max(0.0) };
        if self.touch.down {
            self.touch.down_t += f;
        }
        for v in &mut self.touch.flash {
            *v = (*v - f).max(0.0);
        }
        if crate::assets::witch() {
            self.wmp_load(game, r);
            for i in 0..8 {
                let st = self.wpanel_state(i);
                self.touch.wbtn[i].step(st, f);
            }
        }

        // drag arrow (sub_40ED0): a touch that started in the middle of the screen; the stretch is
        // clamped to 260 units, and past 45 it sets the speed (dx / 5 - 8, or dx / 5 + 8 to the left).
        // Witch on the Holy Night's jp_40ED0 has no such gesture: the stretch its TeaTime menu, choices and
        // mode display read (dword_64987C) and the arrow's alpha (AE0600, only zeroed by jp_3B5C0) stay 0
        let mut speed = 0i32;
        let arrow_on = in_game
            && !crate::assets::witch()
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
            // cases 1 / 3 press their object every held frame (sub_AE040(.., 10); WoH jp_AE040)
            if let Some(b @ (1 | 3)) = held_button {
                self.touch.flash[b] = 10.0;
            }
        } else {
            self.touch.held_frames = 0.0;
        }

        // ZR / ZL held (sub_40ED0, masks [19] / [20] = 0x200 / 0x100): the speed comes from the trigger's
        // pressure (qword_1935130, sub_B37D0: the pad's analog value x 255), 1 below 4, else p / 3.75 up to 64.
        // The Switch's triggers are digital (255: speed 64 from the first frame, measured on the Switch)
        let ramp = |p: u64| -> u32 { if p < 4 { 1 } else { ((p * 0x22_2222_2240) >> 39).min(64) as u32 } };
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
            // (Witch on the Holy Night's jp_40ED0: held ZR, key 19, starts the fast skip, AE0670, at 64)
            self.touch.zr.map(|_| if crate::assets::witch() { 64 } else { ramp(255) })
        } else {
            None
        };
        // (Witch on the Holy Night: not while off_632388 holds it after a screen closed with ZL)
        if self.touch.zl.is_none() {
            self.w.zl_block = false;
        }
        let zl = if in_game && self.touch.panel == Panel::Closed && !self.w.zl_block { self.touch.zl.map(|_| ramp(255)) } else { None };
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
            self.touch.rewind_by = 0;
            Some(v)
        } else if let Some(v) = panel_rewind {
            self.touch.rewind_by = 1;
            Some(v as u32)
        } else if speed < 0 && !self.touch.hidden {
            self.touch.rewind_by = 2;
            Some((-speed) as u32)
        } else {
            None
        };
        // (sub_40ED0 starts it whatever the ring holds, and off_AE05B8 stays set while held: sub_814C0 just finds
        // nothing to go back to)
        game.rewind_held = crate::assets::witch() && back.is_some();
        if let Some(back) = back {
            self.touch.rewind = back.max(1);
            game.speed = back.max(1);
            if self.touch.rewind_rest > FRAME_EPS {
                // Witch on the Holy Night: the frame after a step (off_AE05B8 = 2) is an ordinary one at the same
                // speed (jp_40ED0 leaves v108 as it was), started before the script (`rewind_before_script`); the
                // next starts the rewind again (off_AE05B8 = 1). The panel button's ramp (dword_649890) starts
                // over in it, so that rewind stays at speed 0.
                self.touch.rewind_rest -= f;
                self.touch.rewind_t = 0.0;
                self.touch.held_frames = 0.0;
                game.end_rewind();
            } else {
                // sub_40ED0 runs sub_814C0 instead of the script: no message is shown while rewinding, and each
                // 60 Hz frame restores the stage of the previous ring entry (whatever the display rate)
                if !game.rewinding {
                    game.start_rewind(zl.is_some() || panel_rewind.is_some());
                }
                game.rewinding = true;
                self.touch.rewind_t = (self.touch.rewind_t + f).min(1.0);
                // (sub_814C0 runs, clearing the click wait first, once per 60 Hz frame without a transition, v60)
                if self.touch.rewind_t >= 1.0 - FRAME_EPS && game.settled() {
                    self.touch.rewind_t = (self.touch.rewind_t - 1.0).max(0.0);
                    game.rewind_frame();
                    self.rewind_step(game, r);
                }
            }
        } else if self.touch.rewind > 0 {
            // released: the script runs again from the restored message, which shows and types out
            self.touch.rewind = 0;
            self.touch.rewind_rest = 0.0;
            game.end_rewind();
            game.speed = 1;
        }
        // off_AE05B8 set this frame (not on Witch on the Holy Night's frames between steps)
        let rewinding = self.touch.rewind > 0 && game.rewinding;
        // sub_3C510: the effect fade (dword_AFECA4) rises 0.05 per frame (x5 while rewinding) while rewinding,
        // at speed 2 or more (a skip; ZR held), in auto mode (dword_AFEC98), or while the drag
        // gesture runs (off_AE0610), and falls otherwise
        let rate = 0.05 * f * if rewinding { 5.0 } else { 1.0 };
        let story = self.screen == Screen::Game && !game.in_choice();
        let fx_on = rewinding || (story && (game.speed >= 2 || game.auto)) || arrow_on;
        // the mode icon (sub_3B5B0, set each frame by sub_40ED0): 5 in skip mode, then 2 while rewinding or 3
        // while skipping fast (off_AE0670)
        self.touch.mode_icon = if rewinding {
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
        if crate::assets::witch() {
            if self.screen == Screen::Witch && self.w.scr == super::witch::WScr::Backlog {
                self.touch.wspin.hide();
            } else {
                let off = game.in_choice() || game.mode_task();
                self.touch.wspin.step(f, game.speed as f32, game.auto, rewinding, self.touch.rewound, off);
            }
        }

        // message window (sub_40ED0): hidden while the last touch is in the bottom 40 units (y > 408 of 448) and
        // the window is up (sub_84040; else it shows and the touch moves to y 398), back with a touch above them
        // (that touch does not also tap)
        if in_game && self.touch.panel == Panel::Closed {
            if y > 408.0 {
                if game.window_hideable() {
                    self.touch.hidden = true;
                } else {
                    self.show_window();
                }
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
                if self.touch.panel_t == 0.0 {
                    // sub_4EC10 state 1 (overseas sub_4E170; not 1.0.2): sub_4EAD0 and sub_4EB80 before the slide
                    // (Witch on the Holy Night: objects 185 / 186 and sub_2E8C0)
                    self.panel_flags_save(game);
                    self.panel_flags_scene(game);
                }
                self.touch.panel_t += 0.05 * f;
                if self.touch.panel_t >= 1.0 {
                    self.touch.panel_t = 1.0;
                    self.touch.panel = Panel::Open;
                    self.panel_flags_save(game);
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
                // (Witch on the Holy Night: sound 2; a story dialog over the panel takes the pointer, and a popup
                // takes the buttons, as from the pad)
                if hovered && self.screen != Screen::Witch && self.popup.is_none() {
                    let before = self.touch.panel_hover.or(self.touch.panel_key);
                    self.touch.panel_hover = self.panel_button();
                    self.touch.panel_key = None;
                    if let Some(h) = self.touch.panel_hover {
                        if before != Some(h) {
                            game.play_sys_sound(if crate::assets::witch() { 2 } else { 0 });
                        }
                        self.touch.panel_last = h;
                    }
                }
                if self.screen == Screen::Game && game.choice_question() {
                    self.touch.panel = Panel::Closing;
                } else {
                    self.panel_flags_scene(game);
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

    /// Story overlays: the countdown's black, the drag arrow and mode icon (WoH: the mode display), the mode
    /// panel.
    pub(super) fn draw_touch_game(&self, r: &Renderer, out: &mut Vec<Quad>) {
        // sub_3EF20, first in sub_3C510: the countdown's black over the screen
        if self.touch.cd_black > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, self.touch.cd_black.min(1.0)]));
        }
        if crate::assets::witch() {
            // Witch on the Holy Night's sub_1E770: sub_1D670's spinner and icons (no drag arrow)
            self.wmode_draw(out, &self.touch.wspin, self.touch.mode_icon);
        } else {
            self.draw_arrow_icon(out);
        }
        if self.touch.panel_t > 0.0 && crate::assets::witch() {
            self.wmp_draw(out, self.panel_y(), &self.touch.wbtn);
        } else if self.touch.panel_t > 0.0 {
            let y = self.panel_y();
            self.spr(out, "modepanel_window", 0.0, y * UY, None);
            let top = (y + PANEL_BTN_Y) * UY;
            // a dialog over the panel (quick load, previous / next scene) takes the object focus (sub_AE0E0), so
            // no panel button shows as pointed until sub_4E170 is back in state 3
            let dialog = self.under || self.dlg_holding();
            let hover = self.touch.panel_hover.or(self.touch.panel_key).filter(|_| !dialog);
            for i in 0..8 {
                // object states (unk_5918C8; sub_ADE80 flag 2: an unavailable button, auto on): 4 pressed, 1 / 3
                // focused (3 with flag 2), 2 flag 2, else 0. Flag 2 shows the disabled row, auto's the pressed
                // one; focused with it, the disabled row with the hover row over it at alpha 0x80 (auto: hover)
                let on = self.touch.pflags[i];
                let rows: &[(f32, f32)] = if self.touch.flash[i] > 0.0 {
                    &[(3.0, 1.0)]
                } else if hover == Some(i) {
                    if on && i != 2 { &[(0.0, 1.0), (2.0, 128.0 / 255.0)] } else { &[(2.0, 1.0)] }
                } else if on {
                    if i == 2 { &[(3.0, 1.0)] } else { &[(0.0, 1.0)] }
                } else {
                    &[(1.0, 1.0)]
                };
                let x = (PANEL_X0 + PANEL_DX * i as f32) * UX;
                let n = out.len();
                for &(row, a) in rows {
                    let src = [8.0 + i as f32 * 232.0, 8.0 + row * 72.0, BTN_W, BTN_H];
                    self.sprite_c(out, "mode_panel_btn_ja", [x, top, BTN_W, BTN_H], Some(src), a, Blend::Alpha);
                }
                // flag 0x20 (0x320): the focus flash over it
                add_copy(out, n..out.len(), self.obj_flash(Obj::Panel(i)));
            }
        }
    }

    /// Tsukihime's sub_3C510 overlays: the drag arrow and the mode icon.
    fn draw_arrow_icon(&self, out: &mut Vec<Quad>) {
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
    }

    pub(super) fn draw_ripples(&mut self, r: &Renderer, out: &mut Vec<Quad>) {
        self.touch.draw_ripples(r, out);
    }
}

#[cfg(test)]
mod tests {
    use super::Trail;

    /// A drag from `start` sampled once a frame at `pts`, released at the last one.
    fn release(start: [f32; 2], pts: &[[f32; 2]]) -> [f32; 2] {
        let mut t = Trail::default();
        t.reset(start);
        for &p in pts {
            t.push(p);
        }
        t.flick(start, *pts.last().unwrap_or(&start))
    }

    #[test]
    fn slow_long_drag_flicks_by_its_displacement() {
        // 100 units left at 3 units a frame, then held still before the release (a trackpad's lift)
        let mut pts: Vec<[f32; 2]> = (1..=34).map(|i| [600.0 - 3.0 * i as f32, 200.0]).collect();
        pts.extend([[498.0, 200.0]; 10]);
        assert_eq!(release([600.0, 200.0], &pts), [-102.0, 0.0]);
    }

    #[test]
    fn short_drags_keep_the_two_frame_speed() {
        // a tap, and a slow 15-unit drag that stops: no flick
        assert_eq!(release([300.0, 200.0], &[]), [0.0, 0.0]);
        let pts: Vec<[f32; 2]> = (1..=5).map(|i| [300.0 + 3.0 * i as f32, 200.0]).chain([[315.0, 200.0]; 6]).collect();
        assert_eq!(release([300.0, 200.0], &pts), [0.0, 0.0]);
        // a quick short flick: 6 units a frame over the window
        let pts: Vec<[f32; 2]> = (1..=3).map(|i| [300.0 - 6.0 * i as f32, 200.0]).collect();
        assert_eq!(release([300.0, 200.0], &pts), [-12.0, 0.0]);
    }
}
