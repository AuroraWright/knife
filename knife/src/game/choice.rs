//! Choices (SELR options, then SELD / SELS / SELA). SELD lists the options as numbered lines under the page
//! text (sub_83E50 update, sub_84E50 draw); SELS and SELA ask a yes / no question in the dialog window
//! (sub_848B0 / sub_84A90 update, sub_85690 / sub_85C20 input, sub_85900 / sub_85E40 draw, drawn by the UI).
//! The exe steps every timer per 60 Hz frame.

use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Renderer, TexRef};

use super::{ASCENT, Game, LAYOUT, TOP_BASELINE, UX, UY, Wait};
use crate::text::{self, PlacedChar};

#[derive(Clone, Debug)]
pub struct ChoiceOpt {
    pub value: i32,
    pub text: String,
    /// SELR's third argument 1 (off_AE09E0): the cursor skips the option and it cannot be picked
    pub disabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChoiceKind {
    /// SELD: numbered lines under the page
    Page,
    /// SELS: the dead-end hint corner question (SYSMES 1445 + F027); stores the button index (0 No, 1 Yes)
    Hint,
    /// SELA: "save your progress?" (SYSMES 216); Yes opens the save screen, nothing is stored
    Save,
}

pub struct Choice {
    pub(super) var: String,
    pub(super) kind: ChoiceKind,
    pub(super) opts: Vec<ChoiceOpt>,
    /// focused option (AE0A48) and the one focused before (AE0A50)
    pub(super) hover: Option<usize>,
    prev: Option<usize>,
    /// focus fade 0..1 (AE0A68, +0.1 per frame, reset on a focus change)
    hover_t: f32,
    /// sparkle pulse between 1/3 and 1 (AE0A58, step AE0A60)
    pulse: f32,
    pulse_v: f32,
    /// sub_83E50 state: 1 fade in, 2 text reveal, 3 input, 4 picked; SELA adds 5 fade before the save
    /// screen, 6 open it, 7 wait for it to close
    state: u8,
    /// AE0A28
    alpha: f32,
    /// pick flash (AE0A98, 1 -> 0 at 0.02 per frame)
    flash: f32,
    /// SELD layout: the options' glyphs (reveal order from 0 per option), revealed characters
    /// (AE0A88, 768 = all) and the first option's baseline
    placed: Vec<Vec<PlacedChar>>,
    reveal: Vec<f32>,
    top: f32,
    /// the blurred option texts (texture 5) need drawing
    glow_dirty: bool,
}

/// Pulse step per frame (0x3C3A1D9C).
const PULSE_STEP: f32 = 0.011_359_6;

impl Choice {
    pub(super) fn new(var: String, kind: ChoiceKind, opts: Vec<ChoiceOpt>) -> Self {
        let n = opts.len();
        Self {
            var,
            kind,
            opts,
            hover: None,
            prev: None,
            hover_t: 0.0,
            pulse: 1.0,
            pulse_v: -PULSE_STEP,
            state: 1,
            alpha: 0.0,
            flash: 0.0,
            placed: Vec::new(),
            reveal: vec![0.0; n],
            top: TOP_BASELINE,
            glow_dirty: true,
        }
    }

    fn set_hover(&mut self, h: Option<usize>) {
        if h != self.hover {
            self.prev = self.hover;
            self.hover = h;
            self.hover_t = 0.0;
            self.pulse = 1.0;
            self.pulse_v = -PULSE_STEP;
        }
    }

    fn all_revealed(&self) -> bool {
        self.placed.iter().zip(&self.reveal).all(|(p, &r)| r >= p.len() as f32)
    }
}

impl Game {
    /// A SYSMES line (the table is read once).
    fn sysmes_line(&mut self, i: usize) -> String {
        let assets = &self.assets;
        self.sysmes.get_or_insert_with(|| assets.sysmes()).get(i).cloned().unwrap_or_default()
    }

    /// SELR: one option. The text gets the "N．" prefix SYSMES[1433 + (n + 1) % 10].
    pub(super) fn cmd_selr(&mut self, value: i32, text: &str, disabled: bool) {
        let n = self.sel_opts.len();
        let prefix = self.sysmes_line(1433 + (n + 1) % 10);
        self.sel_opts.push(ChoiceOpt { value, text: format!("{prefix}{text}"), disabled });
    }

    /// SELD / SELS / SELA.
    pub(super) fn cmd_select(&mut self, name: &str, var: String) {
        let kind = match name {
            "SELS" => ChoiceKind::Hint,
            "SELA" => ChoiceKind::Save,
            _ => ChoiceKind::Page,
        };
        let opts = match kind {
            ChoiceKind::Page => std::mem::take(&mut self.sel_opts),
            // sub_92800 / sub_92930: two buttons, values 0 and 1 (off_AE09D8 = 0x10000)
            _ => (0..2).map(|value| ChoiceOpt { value, text: String::new(), disabled: false }).collect(),
        };
        if opts.is_empty() {
            tracing::debug!("{name} without options");
            return;
        }
        if kind == ChoiceKind::Page {
            // SELD writes every option to the message history as a type 12 entry (the backlog lists them)
            self.page.choice_texts.extend(opts.iter().map(|o| o.text.clone()));
        } else {
            // sub_83B20: the message text goes (sub_53AB0(1)), F823 = 0, F1103 = 1
            self.page.visible = false;
            self.set_var("F823", 0);
            self.set_var("F1103", 1);
        }
        self.start_choice(Choice::new(var, kind, opts));
        if self.auto_choice {
            let n = self.choice.as_ref().map_or(1, |c| c.opts.len());
            let seed = std::env::var("KNIFE_SEED").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            let pick = (self.pc.wrapping_mul(31) + self.scene_index * 7 + seed) % n.max(1);
            tracing::info!("auto choice {pick}/{n}");
            self.pick_choice(pick);
            self.choice = None;
            self.wait = Wait::None;
        }
    }

    /// Show a choice (also when a rewind restores one).
    pub(super) fn start_choice(&mut self, mut c: Choice) {
        if c.kind == ChoiceKind::Page {
            // sub_84E50: the line after the page pen (dword_AFEC7C, from sub_391A0), one more when the page
            // has messages (F823 >= 1); each option one line pitch below the last
            let mut y = if self.page.chars.is_empty() { TOP_BASELINE } else { self.page.pen.1 + LAYOUT.line_height };
            if self.var("F823") >= 1 {
                y += LAYOUT.line_height;
            }
            c.top = y;
            c.placed = c
                .opts
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let mut placed = Vec::new();
                    let runs = text::parse_markup(&o.text);
                    let base = y + i as f32 * LAYOUT.line_height;
                    text::layout(&self.font, &LAYOUT, &runs, LAYOUT.left, base, 0, &mut placed);
                    placed
                })
                .collect();
        }
        self.choice = Some(c);
        self.wait = Wait::Choice;
    }

    pub fn in_choice(&self) -> bool {
        self.choice.is_some()
    }

    /// The choice takes input (state 3).
    fn choice_input(&self) -> bool {
        self.choice.as_ref().is_some_and(|c| c.state == 3)
    }

    /// A / a click: during the reveal show all the text (state 2); then pick the focused option.
    pub(super) fn choice_confirm(&mut self) {
        let Some(c) = &mut self.choice else { return };
        if c.state == 2 {
            for (r, p) in c.reveal.iter_mut().zip(&c.placed) {
                *r = p.len() as f32;
            }
            return;
        }
        if c.state == 3
            && let Some(h) = c.hover
        {
            self.pick_choice(h);
        }
    }

    fn pick_choice(&mut self, i: usize) {
        let Some(c) = &mut self.choice else { return };
        if c.opts[i].disabled {
            return;
        }
        let (kind, var, value) = (c.kind, c.var.clone(), c.opts[i].value);
        c.hover = Some(i);
        match kind {
            ChoiceKind::Page => {
                c.state = 4;
                c.flash = 1.0;
            }
            // SELA Yes: fade out, then the save screen
            ChoiceKind::Save => c.state = if i == 1 { 5 } else { 4 },
            ChoiceKind::Hint => c.state = 4,
        }
        if kind != ChoiceKind::Save {
            self.set_var(&var, value);
            // sub_83E50 / sub_85690 keep the index in off_AE0AA0
            self.last_pick = i as i32;
        }
        // sub_83E50 plays system sound 4 for a SELD pick, sub_85690 / sub_85C20 sound 1 for a button
        self.play_sys_sound(if kind == ChoiceKind::Page { 4 } else { 1 });
    }

    /// System sound 0 when the focus moved (sub_83E50 / sub_85690).
    fn choice_moved(&mut self, before: Option<usize>) {
        if self.choice.as_ref().is_some_and(|c| c.hover != before) {
            self.play_sys_sound(0);
        }
    }

    fn choice_hover(&self) -> Option<usize> {
        self.choice.as_ref().and_then(|c| c.hover)
    }

    /// Up / down (sub_83E50 state 3: wrap, skipping disabled options; from no focus down takes the first
    /// option, up the last). SELS / SELA (sub_85690) toggle the two buttons.
    pub fn choice_move(&mut self, delta: i32) {
        let before = self.choice_hover();
        self.choice_move_inner(delta);
        self.choice_moved(before);
    }

    fn choice_move_inner(&mut self, delta: i32) {
        if !self.choice_input() {
            return;
        }
        let Some(c) = &mut self.choice else { return };
        if c.kind != ChoiceKind::Page {
            let h = match (c.hover, delta < 0) {
                (Some(1), true) | (None, false) => 0,
                (Some(0), false) => 1,
                (Some(_), false) => 0,
                (_, true) => 1,
            };
            c.set_hover(Some(h));
            return;
        }
        let n = c.opts.len() as i32;
        let mut h = c.hover.map_or(if delta > 0 { -1 } else { n }, |h| h as i32);
        for _ in 0..n {
            h = (h + delta.signum()).rem_euclid(n);
            if !c.opts[h as usize].disabled {
                break;
            }
        }
        c.set_hover(Some(h as usize));
    }

    /// Left / right on the SELS / SELA buttons: No (0) / Yes (1).
    pub fn choice_side(&mut self, right: bool) {
        let before = self.choice_hover();
        if self.choice_input()
            && let Some(c) = &mut self.choice
            && c.kind != ChoiceKind::Page
        {
            c.set_hover(Some(usize::from(right)));
        }
        self.choice_moved(before);
    }

    /// Option rows (focus objects 217 + i at (88, y)) / the dialog buttons, in canvas pixels.
    fn choice_rects(&self) -> Vec<[f32; 4]> {
        let Some(c) = &self.choice else { return Vec::new() };
        match c.kind {
            ChoiceKind::Page => (0..c.opts.len())
                .map(|i| {
                    let top = c.top + i as f32 * LAYOUT.line_height - ASCENT;
                    let w = c.placed[i].last().map_or(0.0, |p| p.x + p.width() - LAYOUT.left);
                    [LAYOUT.left, top, w, LAYOUT.line_height]
                })
                .collect(),
            _ => DIALOG_BUTTONS.iter().map(|&(x, _)| [x * UX, DIALOG_BUTTON_Y * UY, 216.0, 56.0]).collect(),
        }
    }

    pub(super) fn choice_hover_at(&mut self, x: f32, y: f32) {
        if !self.choice_input() {
            return;
        }
        let h = self.choice_rects().iter().position(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]);
        let before = self.choice_hover();
        if let Some(c) = &mut self.choice
            && let Some(h) = h
            && !c.opts[h].disabled
        {
            c.set_hover(Some(h));
        }
        self.choice_moved(before);
    }

    pub(super) fn update_choice(&mut self, dt: f32) {
        let f = dt / (1000.0 / 60.0);
        let k = usize::from(self.line_read);
        let ms = self.settings.text_ms[k];
        let instant = ms <= 0.0 || self.settings.page_at_once[k];
        let Some(c) = &mut self.choice else { return };
        c.hover_t = (c.hover_t + 0.1 * f).clamp(0.0, 1.0);
        c.pulse += c.pulse_v * f;
        if c.pulse > 1.0 {
            c.pulse = 1.0;
            c.pulse_v = -PULSE_STEP;
        } else if c.pulse < 1.0 / 3.0 {
            c.pulse = 1.0 / 3.0;
            c.pulse_v = PULSE_STEP;
        }
        // sub_84E50 / sub_3B2D0: 16.667 / text speed characters per frame (all at speed 0)
        for (r, p) in c.reveal.iter_mut().zip(&c.placed) {
            *r = if instant { p.len() as f32 } else { (*r + 16.667 / ms * f).min(p.len() as f32) };
        }
        let page = c.kind == ChoiceKind::Page;
        match c.state {
            // SELD fades in at 0.04 per frame, SELS / SELA at 0.1
            1 => {
                c.alpha += if page { 0.04 } else { 0.1 } * f;
                if c.alpha >= 1.0 {
                    c.alpha = 1.0;
                    c.state = if page { 2 } else { 3 };
                }
            }
            2 => {
                if c.all_revealed() {
                    c.state = 3;
                }
            }
            4 => {
                c.alpha = (c.alpha - if page { 0.028 } else { 0.1 } * f).max(0.0);
                let done = if page {
                    c.flash -= 0.02 * f;
                    c.flash <= 0.0
                } else {
                    c.alpha <= 0.0
                };
                if done {
                    self.choice = None;
                    self.wait = Wait::None;
                }
            }
            5 => {
                c.alpha -= 0.1 * f;
                if c.alpha <= 0.0 {
                    c.alpha = 0.0;
                    c.state = 6;
                }
            }
            _ => {}
        }
    }

    /// SELA: the save screen should open now (state 6 -> 7).
    pub fn take_choice_save(&mut self) -> bool {
        match &mut self.choice {
            Some(c) if c.state == 6 => {
                c.state = 7;
                true
            }
            _ => false,
        }
    }

    /// SELA: the save screen closed (sub_50FC0 returned 3; states 8 -> 4 end the choice).
    pub fn choice_save_closed(&mut self) {
        if let Some(c) = &mut self.choice
            && c.state == 7
        {
            c.state = 4;
        }
    }

    /// SELS / SELA dialog for the UI: SYSMES message, focused button, alpha and window scale
    /// (sub_85900: 1 + (1 - a) * 0.05 opening, 1 - (1 - a) * 0.05 closing).
    pub fn choice_dialog(&self) -> Option<(usize, Option<usize>, f32, f32)> {
        let c = self.choice.as_ref()?;
        let msg = match c.kind {
            ChoiceKind::Page => return None,
            ChoiceKind::Hint => (1445 + self.var("F027")).max(0) as usize,
            ChoiceKind::Save => 216,
        };
        if c.alpha <= 0.0 {
            return None;
        }
        let scale = match c.state {
            1 => 1.0 + (1.0 - c.alpha) * 0.05,
            4 | 5 => 1.0 - (1.0 - c.alpha) * 0.05,
            _ => 1.0,
        };
        Some((msg, c.hover, c.alpha, scale))
    }

    /// Texture 5: the option texts (text slots 160+, rows of 64 px) gaussian-blurred (sub_A0A90(.., 2.0)).
    /// Drawn here at the options' own canvas positions; the glow takes each option's band.
    pub(super) fn render_choice_glow(&mut self, r: &mut Renderer, enc: &mut wgpu::CommandEncoder) {
        let Some(c) = &mut self.choice else { return };
        if c.kind != ChoiceKind::Page || !c.glow_dirty {
            return;
        }
        c.glow_dirty = false;
        let mut quads = Vec::new();
        // SELD (0x800e4) gives slots 160+ the option text behind "@c9" with flags 0x201000: 0x200000 forces the
        // glyph edge on (sub_A7790 -> sub_C1060 / sub_C1074) and @c9 makes the edge colour dword_34DCB0[3] =
        // white (sub_BDC2C: @c1..5 fill colours dword_3C99D2, @c6..9 edge colours), 0x1000 shows it at once.
        // So the glow source is the glyph and its edge, all white; the edge is the message text's rim.
        for p in c.placed.iter().flatten() {
            for (dx, dy) in [(0.0, 0.0), (-2.0, 0.0), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0), (-1.5, -1.5), (1.5, -1.5), (-1.5, 1.5), (1.5, 1.5)] {
                if let Some(q) = self.font.placed_quad(r, p, dx, dy, [1.0; 4]) {
                    quads.push(q);
                }
            }
        }
        let text_rt = self.choice_text_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "choice text"));
        r.draw(enc, text_rt, Some([0.0; 4]), &quads);
        let glow = self.choice_glow.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "choice glow"));
        // sub_A0A90(0, -2, 2.0): target texture 5, sigma 2 taps (13 taps) spaced 1.5 px of 1920, h + v
        let step = 1.5 * CANVAS_W as f32 / 1920.0;
        r.blur(enc, &text_rt.tex, &self.blur_rt, [step, 0.0], 2.0);
        r.blur(enc, &self.blur_rt.tex, glow, [0.0, step], 2.0);
    }

    /// sub_84E50 (640x448 units): the backdrop, the option texts, the focus glow / underline / sparkle and the
    /// pick's ring, flash and lens flare (effect 30 = additive).
    pub(super) fn choice_quads(&mut self, r: &Renderer, out: &mut Vec<Quad>, page_alpha: f32) {
        let Some(c) = &self.choice else { return };
        if c.kind != ChoiceKind::Page {
            return;
        }
        if self.select_parts.is_none() {
            // texture ids 142..147 = SELECT_PARTS_JA 0..5: altermenu_flash, lensflare, underline, ring,
            // sparkle, bg
            self.select_parts = Some(match self.assets.ui_textures(r, "SELECT_PARTS_JA") {
                Ok(t) => t.into_iter().map(|(_, t)| t).collect(),
                Err(e) => {
                    tracing::warn!("SELECT_PARTS_JA: {e:#}");
                    Vec::new()
                }
            });
        }
        let parts = self.select_parts.clone().unwrap_or_default();
        let Some(c) = &self.choice else { return };
        let tex = |i: usize| parts.get(i).cloned();
        let size = |t: &TexRef| (t.width as f32, t.height as f32);
        let rect = |t: &TexRef, x: f32, y: f32, sx: f32, sy: f32, col: [f32; 4]| {
            let (w, h) = size(t);
            let uv = t.uv_max();
            Quad::rect(t.clone(), [x, y, w * sx, h * sy], col).with_uv_rect(0.0, 0.0, uv[0], uv[1])
        };
        let add = |mut q: Quad| {
            q.blend = Blend::Add;
            q
        };
        let a = c.alpha;
        // texture 147 over the screen (scale 1/3 x 0.41481 of 640x448 = native pixels)
        if let Some(t) = tex(5) {
            out.push(rect(&t, 0.0, 0.0, 1.0, 1.0, [1.0, 1.0, 1.0, a]));
        }
        let pitch = LAYOUT.line_height;
        for i in 0..c.opts.len() {
            let base = c.top + i as f32 * pitch;
            let top = base - ASCENT;
            // in state 4 only the picked option keeps its text
            if c.state != 4 || c.hover == Some(i) {
                let shown = c.reveal[i];
                for p in &c.placed[i] {
                    let mut vis = (shown - p.order as f32).clamp(0.0, 1.0);
                    if !self.settings.text_fade_in && vis > 0.0 {
                        vis = 1.0;
                    }
                    if vis > 0.0 {
                        Self::rimmed_glyph(&mut self.font, r, out, p, vis * page_alpha);
                    }
                }
            }
            let focus = c.hover == Some(i);
            if !focus && c.prev != Some(i) {
                continue;
            }
            let t = if focus { c.hover_t } else { 1.0 - c.hover_t };
            let ga = a * t * 0.5;
            if ga > 0.0
                && let Some(g) = &self.choice_glow
            {
                // the texture 5 row (64 px) at (82.667, y - 3.3185): the text sits 16 / 8 px into its row
                let red = if c.state == 4 { 0.75294 } else { 0.43922 };
                let (x0, y0) = (82.667 * UX, top - 3.3185 * UY);
                let (u0, v0) = (x0 / CANVAS_W as f32, y0 / CANVAS_H as f32);
                let q = Quad::rect(g.tex.clone(), [x0, y0, CANVAS_W as f32 - x0, 64.0], [red, 1.0, 1.0, ga])
                    .with_uv_rect(u0, v0, 1.0, (y0 + 64.0) / CANVAS_H as f32);
                out.push(add(q));
            }
            if focus {
                // texture 144 underline at (71.333, y + 11.615), x scale (t * 0.9 + 0.1) / 3
                if let Some(u) = tex(2) {
                    out.push(add(rect(&u, 71.333 * UX, top + 11.615 * UY, c.hover_t * 0.9 + 0.1, 1.0, [1.0, 1.0, 1.0, ga])));
                }
                // texture 146 sparkle at (-65.52, y - 30.696), pulsing
                if let Some(s) = tex(4) {
                    out.push(add(rect(&s, -65.52 * UX, top - 30.696 * UY, 1.0, 1.0, [1.0, 1.0, 1.0, a * c.pulse])));
                }
            }
        }
        if c.state == 4 {
            let picked = c.hover.unwrap_or(0) as f32;
            let first = c.top - ASCENT;
            let v47 = 1.0 - (c.flash.max(0.0) * std::f32::consts::FRAC_PI_2).sin();
            // texture 145 ring centred at (86, y0 + 24.474 + pitch * picked), scale v47 * 1.4 + 0.1, turning
            // once as v47 runs 0 -> 1 (sub_B0480(v47 * 65535))
            if let Some(t) = tex(3) {
                let k = v47 * 1.4 + 0.1;
                let (w, h) = size(&t);
                let (hw, hh) = (w * k / 2.0, h * k / 2.0);
                let (cx, cy) = (86.0 * UX, first + 24.474 * UY + pitch * picked);
                let (s, co) = (v47 * std::f32::consts::TAU).sin_cos();
                let uv = t.uv_max();
                let mut q = Quad::rect(t.clone(), [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, a]).with_uv_rect(0.0, 0.0, uv[0], uv[1]);
                for (p, (dx, dy)) in q.pos.iter_mut().zip([(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]) {
                    *p = [cx + dx * co - dy * s, cy + dx * s + dy * co];
                }
                out.push(add(q));
            }
            // texture 142 flash over the screen, texture 143 lens flare at (5, y0 - 377.9 + pitch * picked)
            if let Some(t) = tex(0) {
                out.push(add(rect(&t, 0.0, 0.0, 1.0, 1.0, [1.0, 1.0, 1.0, a])));
            }
            if let Some(t) = tex(1) {
                out.push(add(rect(&t, 5.0 * UX, first - 377.9 * UY + pitch * picked, 1.0, 1.0, [1.0, 0.75, 1.0, a])));
            }
        }
    }
}

/// SELS / SELA buttons (sub_85900: objects 217 / 218 from unk_65BBC0): No at x 241, Yes at x 327 (640x448
/// units, top-left), dialog_btn_ja frames as in the confirm dialog.
pub const DIALOG_BUTTONS: [(f32, bool); 2] = [(241.0, false), (327.0, true)];
pub const DIALOG_BUTTON_Y: f32 = 238.52;

