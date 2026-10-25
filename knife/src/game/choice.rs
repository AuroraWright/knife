//! Choices (SELR options, then SELD / SELS / SELA). SELD lists the options as numbered lines under the page
//! text (sub_83E50 update, sub_84E50 draw); SELS and SELA ask a yes / no question in the dialog window
//! (sub_848B0 / sub_84A90 update, sub_85690 / sub_85C20 input, sub_85900 / sub_85E40 draw, drawn by the UI).
//! The exe steps every timer per 60 Hz frame.

use knife_render::{Blend, BlurMode, CANVAS_H, CANVAS_W, Quad, Renderer, Shade, TexRef};

use super::{Game, UX, UY, Wait, msg_layout};
use crate::text::Layout;
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
    /// SEWI (Witch on the Holy Night, jp_92800 mode 6): the WIK chapter select, 14 figures (values 0..13) over
    /// black (sub_656A0)
    Wik,
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
    /// Witch on the Holy Night: each option's text lines (jp_A8780)
    lines: Vec<usize>,
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
            top: choice_top(),
            glow_dirty: true,
            lines: vec![1; n],
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

/// sub_84E50: the options at x 88 units (English 32, the overseas build's v4), each in a text slot 466 units
/// wide (English 566: sub_84E50 / sub_83B20 sub_AC7D0(slot, 2, ..)), with the message text style.
fn choice_x() -> f32 {
    if crate::text::english() { 32.0 } else { 88.0 }
}

fn choice_layout() -> Layout {
    let w = if crate::text::english() { 566.0 } else { 466.0 };
    Layout { left: choice_x() * UX, right: (choice_x() + w) * UX, ..msg_layout() }
}

/// The first option's baseline on an empty page (sub_38D20 gives no pen): its box at 63.881 units (English
/// 54.178).
fn choice_top() -> f32 {
    (if crate::text::english() { 54.178 } else { 63.881 }) * UY + msg_layout().ascent
}

/// Witch on the Holy Night's option text slots (SELD 0x5f318: jp_AB4D0(20 + n, text, white, 0x801000, 560, 64,
/// 0, 0, 0.65, 0.65)): 560 units wide, glyphs 0.65 of W / 32 (English 0.9 of that), (0 + 4) px character and
/// (0 + 14) px line spacing (jp_A7790's C3944), shown at once (0x1000). Laid out from x 0, baseline 0; each line
/// is centred on x 0 (jp_AB710's 0x100).
fn witch_option_layout() -> Layout {
    let base = msg_layout();
    let size = base.size * 0.65 / 0.74;
    Layout {
        left: 0.0,
        right: 560.0 * UX,
        size,
        spacing: 4.0,
        sx: 1.0,
        line_height: text::line_feed() * size / text::BASE_SIZE + 14.0,
        ascent: base.ascent * 0.65 / 0.74,
        ..base
    }
}

/// sel_win (DISP_PARTS 7, image slot 57) rects (x0, y0, x1, y1): one / two-line bars, plain (unk_4594A8 /
/// 4594A0), focused (4594D0 / 4594C0) and picked (4594D8 / 4594C8).
const SEL_WIN: [[[f32; 4]; 2]; 3] = [
    [[30.0, 44.0, 1506.0, 148.0], [30.0, 172.0, 1506.0, 322.0]],
    [[30.0, 364.0, 1506.0, 468.0], [30.0, 492.0, 1506.0, 642.0]],
    [[30.0, 684.0, 1506.0, 788.0], [30.0, 812.0, 1506.0, 962.0]],
];

impl Choice {
    /// sub_64F10: option `i`'s row top (640x448 units) and x offset. Rows start at 113.66 (26.963 higher for
    /// each option past four) and take 53.926 per text line (9 less from the second line on); while the
    /// alpha is below 1 every option but the picked one slides from / to the left (x - 320 (1 - alpha)).
    fn witch_rows(&self) -> Vec<(f32, f32)> {
        let n = self.opts.len();
        let mut y = if n <= 3 { 113.66 } else { 113.66 - 26.963 * (n - 4) as f32 };
        (0..n)
            .map(|i| {
                let picked = self.hover == Some(i) && self.state == 4;
                let x = if !picked && self.alpha < 1.0 { 320.0 - 320.0 * (2.0 - self.alpha) } else { 0.0 };
                let row = (x, y);
                let l = self.lines[i];
                y += l as f32 * 53.926 + if l > 1 { 9.0 - 9.0 * l as f32 } else { 0.0 };
                row
            })
            .collect()
    }
}

impl Game {
    /// A SYSMES line (the table is read once).
    fn sysmes_line(&mut self, i: usize) -> String {
        let assets = &self.assets;
        self.sysmes.get_or_insert_with(|| assets.sysmes()).get(i).cloned().unwrap_or_default()
    }

    /// SELR: one option. The text gets the "N．" prefix SYSMES[1433 + (n + 1) % 10]; the overseas build
    /// (EN 0x766b0, case 0x454C52) writes the script text reference "$0439xx" instead, line 43960 + n (43959
    /// from the tenth on): "１．" in Japanese, "1. " (with a space) in the other languages.
    pub(super) fn cmd_selr(&mut self, value: i32, text: &str, disabled: bool) {
        // Witch on the Holy Night's SELR (0x5aa70) keeps the text alone
        if crate::assets::witch() {
            self.sel_opts.push(ChoiceOpt { value, text: text.to_string(), disabled });
            return;
        }
        let n = self.sel_opts.len();
        let prefix = if self.assets.build == knife_core::exe::Build::Overseas {
            format!("${:06}", if n >= 9 { 43959 } else { 43960 + n })
        } else {
            self.sysmes_line(1433 + (n + 1) % 10)
        };
        self.sel_opts.push(ChoiceOpt { value, text: format!("{prefix}{text}"), disabled });
    }

    /// SELD / SELS / SELA.
    pub(super) fn cmd_select(&mut self, name: &str, var: String) {
        let kind = match name {
            "SELS" => ChoiceKind::Hint,
            "SELA" => ChoiceKind::Save,
            "SEWI" => ChoiceKind::Wik,
            _ => ChoiceKind::Page,
        };
        let opts = match kind {
            ChoiceKind::Page => std::mem::take(&mut self.sel_opts),
            // jp_92800: 14 options, values 0..13 (off_6324E0), no choice id (off_6324D8 = -1)
            ChoiceKind::Wik => (0..14).map(|value| ChoiceOpt { value, text: String::new(), disabled: false }).collect(),
            // sub_92800 / sub_92930: two buttons, values 0 and 1 (off_AE09D8 = 0x10000)
            _ => (0..2).map(|value| ChoiceOpt { value, text: String::new(), disabled: false }).collect(),
        };
        if opts.is_empty() {
            tracing::debug!("{name} without options");
            return;
        }
        if kind == ChoiceKind::Page && crate::assets::witch() {
            // Witch on the Holy Night's SELD writes no history entries
            let shown: Vec<String> = opts.iter().map(|o| self.resolve_text(&o.text)).collect();
            crate::text_stream::choices(shown.iter().map(String::as_str));
        } else if kind == ChoiceKind::Page {
            // SELD writes every option to the message history as a type 12 entry (the backlog lists them)
            // each option's record keeps F(847 + its index on the page) (`Page::extras`)
            let at = self.page.texts.len() + self.page.choice_texts.len();
            let extras: Vec<i32> = (0..opts.len()).map(|j| self.var(&format!("F{}", 847 + at + j))).collect();
            self.page.choice_extras.extend(extras);
            self.page.choice_texts.extend(opts.iter().map(|o| o.text.clone()));
            let shown: Vec<String> = opts.iter().map(|o| self.resolve_text(&o.text)).collect();
            crate::text_stream::choices(shown.iter().map(String::as_str));
        } else if kind == ChoiceKind::Wik {
            // sub_63EB0: the message text goes (jp_53AB0(1)), F823 = 0, F1103 = 1; wik_sel_parts loads into image
            // slots 4640.. (jp_9A940)
            self.clear_page(true);
            self.set_var("F823", 0);
            self.set_var("F1103", 1);
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
            // the SELA / SELS questions (save progress, hint corner) answer No; story choices vary
            let pick = if kind == ChoiceKind::Page { (self.pc.wrapping_mul(31) + self.scene_index * 7 + seed) % n.max(1) } else { 0 };
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
            let lay = choice_layout();
            let mut y = if self.page.chars.is_empty() { choice_top() } else { self.page.pen.1 + lay.line_height };
            if self.var("F823") >= 1 {
                y += lay.line_height;
            }
            c.top = y;
            self.place_choice(&mut c);
        }
        self.choice = Some(c);
        self.wait = Wait::Choice;
    }

    /// Lay out the options of a page choice from its top (text slots 20+, sub_84E50; the overseas sub_7D0C0
    /// lays them out again in the new language after a language change).
    fn place_choice(&self, c: &mut Choice) {
        if crate::assets::witch() {
            let lay = witch_option_layout();
            c.placed.clear();
            c.lines.clear();
            for o in &c.opts {
                let mut placed = Vec::new();
                let runs = text::parse_markup(&self.resolve_text(&o.text));
                text::layout(&self.font, &lay, &runs, 0.0, 0.0, 0, &mut placed);
                let mut rows: Vec<(f32, f32)> = Vec::new();
                for p in &placed {
                    let end = p.x + p.width();
                    match rows.iter_mut().find(|r| r.0 == p.baseline) {
                        Some(r) => r.1 = r.1.max(end),
                        None => rows.push((p.baseline, end)),
                    }
                }
                for p in &mut placed {
                    let w = rows.iter().find(|r| r.0 == p.baseline).map_or(0.0, |r| r.1);
                    p.x -= w / 2.0;
                }
                c.lines.push(rows.len());
                c.placed.push(placed);
            }
            c.reveal = c.placed.iter().map(|p| p.len() as f32).collect();
            return;
        }
        let lay = choice_layout();
        c.placed = c
            .opts
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let mut placed = Vec::new();
                let runs = text::parse_markup(&self.resolve_text(&o.text));
                let base = c.top + i as f32 * lay.line_height;
                text::layout(&self.font, &lay, &runs, lay.left, base, 0, &mut placed);
                placed
            })
            .collect();
    }

    /// sub_7D0C0: the shown page choice laid out again (after a language change); revealed options stay
    /// revealed.
    pub(super) fn relayout_choice(&mut self) {
        let Some(mut c) = self.choice.take() else { return };
        if c.kind == ChoiceKind::Page {
            let full: Vec<bool> = c.placed.iter().zip(&c.reveal).map(|(p, &r)| r >= p.len() as f32).collect();
            self.place_choice(&mut c);
            for ((r, p), full) in c.reveal.iter_mut().zip(&c.placed).zip(full) {
                *r = if full { p.len() as f32 } else { r.min(p.len() as f32) };
            }
        }
        self.choice = Some(c);
    }

    pub fn in_choice(&self) -> bool {
        self.choice.is_some()
    }

    /// A SELS / SELA question dialog is up (sub_85680 / sub_85C10), which turns the mode panel off.
    pub fn choice_question(&self) -> bool {
        self.choice.as_ref().is_some_and(|c| matches!(c.kind, ChoiceKind::Hint | ChoiceKind::Save))
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
            ChoiceKind::Page | ChoiceKind::Wik if crate::assets::witch() => c.state = 4,
            ChoiceKind::Page | ChoiceKind::Wik => {
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
        // sub_83E50 plays system sound 4 for a SELD pick, sub_85690 / sub_85C20 sound 1 for a button (Witch on the
        // Holy Night's jp_83E50: 0)
        self.play_sys_sound(match kind {
            ChoiceKind::Page | ChoiceKind::Wik if crate::assets::witch() => 0,
            ChoiceKind::Page | ChoiceKind::Wik => 4,
            _ => 1,
        });
    }

    /// System sound 0 when the focus moved (sub_83E50 / sub_85690; Witch on the Holy Night's jp_83E50: 2).
    fn choice_moved(&mut self, before: Option<usize>) {
        if self.choice.as_ref().is_some_and(|c| c.hover != before) {
            self.play_sys_sound(if crate::assets::witch() { 2 } else { 0 });
        }
    }

    fn choice_hover(&self) -> Option<usize> {
        self.choice.as_ref().and_then(|c| c.hover)
    }

    /// Up / down (sub_83E50 state 3: wrap, skipping disabled options; from no focus down takes the first
    /// option, up the last). SELS / SELA (sub_85690) toggle the two buttons.
    pub fn choice_move(&mut self, delta: i32, repeat: bool) {
        let before = self.choice_hover();
        self.choice_move_inner(delta, repeat);
        self.choice_moved(before);
    }

    fn choice_move_inner(&mut self, delta: i32, repeat: bool) {
        if !self.choice_input() {
            return;
        }
        let Some(c) = &mut self.choice else { return };
        // sub_65380: up takes the first figure, down the last
        if c.kind == ChoiceKind::Wik {
            c.set_hover(Some(if delta < 0 { 0 } else { 13 }));
            return;
        }
        // Witch on the Holy Night's jp_83E50 wraps only on a fresh press, not on key repeat
        if c.kind == ChoiceKind::Page && crate::assets::witch() {
            let n = c.opts.len() as i32;
            let h = c.hover.map_or(-1, |h| h as i32);
            let next = match delta < 0 {
                true if h < 1 => (!repeat).then_some(n - 1),
                true => Some(h - 1),
                false if h >= n - 1 => (!repeat).then_some(0),
                false => Some(h + 1),
            };
            if let Some(h) = next {
                c.set_hover(Some(h as usize));
            }
            return;
        }
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
    pub fn choice_side(&mut self, right: bool, repeat: bool) {
        let before = self.choice_hover();
        // sub_65380: left / right step through the figures, wrapping only on a fresh press
        if self.choice_input()
            && let Some(c) = &mut self.choice
            && c.kind == ChoiceKind::Wik
        {
            let h = c.hover.map_or(-1, |h| h as i32);
            let next = match right {
                false if h > 0 => h - 1,
                false => if repeat { 0 } else { 13 },
                true if h < 13 => h + 1,
                true => if repeat { 13 } else { 0 },
            };
            c.set_hover(Some(next as usize));
        } else if self.choice_input()
            && let Some(c) = &mut self.choice
            && !matches!(c.kind, ChoiceKind::Page | ChoiceKind::Wik)
        {
            c.set_hover(Some(usize::from(right)));
        }
        self.choice_moved(before);
    }

    /// Option rows (focus objects 217 + i at (88, y)) / the dialog buttons, in canvas pixels.
    fn choice_rects(&self) -> Vec<[f32; 4]> {
        let Some(c) = &self.choice else { return Vec::new() };
        let lay = choice_layout();
        match c.kind {
            // Witch on the Holy Night: input regions 190 + n (sub_63B20 / sub_64F10) at the row's x offset and
            // top + 22, x 35..606, 42 units high (64 for two lines)
            ChoiceKind::Page if crate::assets::witch() => c
                .witch_rows()
                .iter()
                .zip(&c.lines)
                .map(|(&(x, y), &l)| [(x + 35.0) * UX, (y + 22.0) * UY, 571.0 * UX, if l >= 2 { 64.0 } else { 42.0 } * UY])
                .collect(),
            ChoiceKind::Wik => WIK_RECTS.iter().map(|&[x0, y0, x1, y1]| [x0 * UX, y0 * UY, (x1 - x0) * UX, (y1 - y0) * UY]).collect(),
            ChoiceKind::Page => (0..c.opts.len())
                .map(|i| {
                    let top = c.top + i as f32 * lay.line_height - lay.ascent;
                    let w = c.placed[i].last().map_or(0.0, |p| p.x + p.width() - lay.left);
                    [lay.left, top, w, lay.line_height]
                })
                .collect(),
            _ => DIALOG_BUTTONS.iter().map(|&(x, _)| [x * UX, DIALOG_BUTTON_Y * UY, 216.0, 56.0]).collect(),
        }
    }

    pub(super) fn choice_hover_at(&mut self, x: f32, y: f32) {
        if !self.choice_input() {
            return;
        }
        let rects = self.choice_rects();
        // jp_AE840 takes the first region under the pointer in id order: SEWI's region 190 + k is figure
        // dword_26291C[k]
        let wik = self.choice.as_ref().is_some_and(|c| c.kind == ChoiceKind::Wik);
        let order: Vec<usize> = if wik { WIK_REGIONS.to_vec() } else { (0..rects.len()).collect() };
        let h = order.into_iter().find(|&i| rects.get(i).is_some_and(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]));
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
        // jp_83E50: with config 0x44 a shown choice takes skip mode off (AE0660 bit 1) each input frame and as
        // the WIK select finishes fading in
        let stop = crate::assets::witch() && self.settings.choice_skip_stop && c.alpha >= 1.0 && c.state == 3;
        if stop {
            self.skip_mode = false;
        }
        let Some(c) = &mut self.choice else { return };
        if c.kind == ChoiceKind::Wik {
            // jp_83E50 mode 6: 0.02 a frame in and out; at 0 the message goes (jp_53AB0(1)) and wik_sel_parts is
            // released (jp_9AC00)
            match c.state {
                1 => {
                    c.alpha += 0.02 * f;
                    if c.alpha >= 1.0 {
                        c.alpha = 1.0;
                        c.state = 3;
                    }
                }
                4 => {
                    c.alpha -= 0.02 * f;
                    if c.alpha <= 0.0 {
                        self.choice = None;
                        self.wait = Wait::None;
                        self.clear_page(true);
                        self.wik_parts = None;
                    }
                }
                _ => {}
            }
            return;
        }
        if page && crate::assets::witch() {
            // jp_83E50: 0.08 a frame in (then input) and out after the pick; at 0 the message goes (jp_53AB0(1))
            match c.state {
                1 => {
                    c.alpha += 0.08 * f;
                    if c.alpha >= 1.0 {
                        c.alpha = 1.0;
                        c.state = 3;
                    }
                }
                4 => {
                    c.alpha -= 0.08 * f;
                    if c.alpha <= 0.0 {
                        self.choice = None;
                        self.wait = Wait::None;
                        self.clear_page(true);
                    }
                }
                _ => {}
            }
            return;
        }
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
            ChoiceKind::Page | ChoiceKind::Wik => return None,
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
        let (lay, band) = (choice_layout(), self.glow_band());
        let Some(c) = &mut self.choice else { return };
        if c.kind != ChoiceKind::Page || !c.glow_dirty || crate::assets::witch() {
            return;
        }
        c.glow_dirty = false;
        let mut quads = Vec::new();
        // SELD (0x800e4) gives slots 160+ the option text behind "@c9" with flags 0x201000: 0x200000 forces the
        // glyph edge on (sub_A7790 -> sub_C1060 / sub_C1074) and @c9 makes the edge colour dword_34DCB0[3] =
        // white (sub_BDC2C: @c1..5 fill colours dword_3C99D2, @c6..9 edge colours), 0x1000 shows it at once.
        // So the glow source is the glyph and its edge, all white; the edge is the message text's rim.
        // Each option has its own row of the texture (slot 160 + n at y 8 + n * band), so a row's glow never
        // takes in the next option's.
        for (i, opt) in c.placed.iter().enumerate() {
            let row = i as f32 * band - Self::glow_row_top(c.top, i, &lay);
            for p in opt {
                for (dx, dy) in [(0.0, 0.0), (-2.0, 0.0), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0), (-1.5, -1.5), (1.5, -1.5), (-1.5, 1.5), (1.5, 1.5)] {
                    if let Some(q) = self.font.placed_quad(r, p, dx, dy + row, [1.0; 4]) {
                        quads.push(q);
                    }
                }
            }
        }
        let text_rt = self.choice_text_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "choice text"));
        r.draw(enc, text_rt, Some([0.0; 4]), &quads);
        let glow = self.choice_glow.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "choice glow"));
        // sub_A0A90(0, -2, 2.0): shader 51 whiteness, sigma 2 taps (13 taps) spaced 1.5 px of 1920. It queues
        // four passes (h, v, h, v, the last back into texture 5) like the menu blur, but the Switch glow is two
        // passes wide: against the Switch (first choice of 04_00_ARC04_1B) two passes give the glow's sum 0.97,
        // its far halo 1.015 and 1.81 mean abs, four passes a halo 1.54 too wide (2.36)
        let step = 1.5 * CANVAS_W as f32 / 1920.0;
        r.blur(enc, &text_rt.tex, &self.blur_rt, [step, 0.0], 2.0, BlurMode::White);
        r.blur(enc, &self.blur_rt.tex, glow, [0.0, step], 2.0, BlurMode::White);
    }

    /// The glow texture's rows: 64 px, 160 in the overseas build (room for two-line options).
    fn glow_band(&self) -> f32 {
        if self.assets.build == knife_core::exe::Build::Overseas { 160.0 } else { 64.0 }
    }

    /// Canvas y where option `i`'s glow row is drawn: the texture 5 row at y - 3.3185 (the text sits 8 px into
    /// its row).
    fn glow_row_top(first: f32, i: usize, lay: &Layout) -> f32 {
        first + i as f32 * lay.line_height - lay.ascent - 3.3185 * UY
    }

    /// SELECT_PARTS_JA (texture ids 142..147 = entries 0..5: altermenu_flash, lensflare, underline, ring,
    /// sparkle, bg), loaded on first use.
    fn select_parts(&mut self, r: &Renderer) -> Vec<TexRef> {
        if self.select_parts.is_none() {
            self.select_parts = Some(match self.assets.ui_textures(r, "SELECT_PARTS_JA") {
                Ok(t) => t.into_iter().map(|(_, t)| t).collect(),
                Err(e) => {
                    tracing::warn!("SELECT_PARTS_JA: {e:#}");
                    Vec::new()
                }
            });
        }
        self.select_parts.clone().unwrap_or_default()
    }

    /// The page choice's backdrop: texture 147 over the screen (scale 1/3 x 0.41481 of 640x448 = native
    /// pixels) at the choice alpha x `page_alpha` (sub_84E50 draws it straight into target 0, unlike the
    /// window and text that sub_391A0 composites later, so a menu blur capture keeps it).
    pub(super) fn choice_backdrop(&mut self, r: &Renderer, page_alpha: f32) -> Option<Quad> {
        if crate::assets::witch() {
            return None;
        }
        let alpha = self.choice.as_ref().filter(|c| c.kind == ChoiceKind::Page)?.alpha;
        let t = self.select_parts(r).get(5).cloned()?;
        let uv = t.uv_max();
        let rect = [0.0, 0.0, t.width as f32, t.height as f32];
        Some(Quad::rect(t, rect, [1.0, 1.0, 1.0, alpha * page_alpha]).with_uv_rect(0.0, 0.0, uv[0], uv[1]))
    }

    /// sub_84E50 (640x448 units): the backdrop, the option texts, the focus glow / underline / sparkle and the
    /// pick's ring, flash and lens flare (effect 30 = additive).
    pub(super) fn choice_quads(&mut self, r: &Renderer, out: &mut Vec<Quad>, page_alpha: f32) {
        let Some(c) = &self.choice else { return };
        if c.kind != ChoiceKind::Page {
            return;
        }
        if crate::assets::witch() {
            self.witch_choice_quads(r, out, page_alpha);
            return;
        }
        let parts = self.select_parts(r);
        let backdrop = self.choice_backdrop(r, page_alpha);
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
        // v6 = (1 - sub_4E9F0) * AE0A28 * a1: the caller's alpha (sub_391A0: the text window's) scales the
        // backdrop, glow, underline, sparkle and the pick's ring / flash / flare
        let a = c.alpha * page_alpha;
        out.extend(backdrop);
        let lay = choice_layout();
        let pitch = lay.line_height;
        let x = choice_x();
        let band = self.glow_band();
        for i in 0..c.opts.len() {
            let base = c.top + i as f32 * pitch;
            let top = base - lay.ascent;
            // in state 4 only the picked option keeps its text
            if c.state != 4 || c.hover == Some(i) {
                let shown = c.reveal[i];
                let first = out.len();
                for p in &c.placed[i] {
                    let mut vis = (shown - p.order as f32).clamp(0.0, 1.0);
                    if !self.settings.text_fade_in && vis > 0.0 {
                        vis = 1.0;
                    }
                    if vis > 0.0 {
                        Self::rimmed_glyph(&mut self.font, r, out, p, vis * page_alpha);
                    }
                }
                // sub_A8CB0(20 + n, n == cursor) sets the slot's +100964, the blend state its glyphs draw with
                // (jp_C6A58): the cursor's option adds (state 1, SrcAlpha / One), so its black edge leaves the
                // backdrop as it is and the white fill brightens it
                if c.hover == Some(i) {
                    for q in &mut out[first..] {
                        q.blend = Blend::Add;
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
                // the texture 5 row at (x - 5.3333, y - 3.3185): the text sits 16 / 8 px into its row
                let red = if c.state == 4 { 0.75294 } else { 0.43922 };
                let (x0, y0) = ((x - 5.3333) * UX, Self::glow_row_top(c.top, i, &lay));
                let (u0, v0) = (x0 / CANVAS_W as f32, i as f32 * band / CANVAS_H as f32);
                // effect 30 (state 1, the default shader) would weigh the glow by its own alpha too, but the
                // Switch glow is linear in it (squared it is 0.63 as bright), so its alpha is left out
                let mut q = Quad::rect(g.tex.clone(), [x0, y0, CANVAS_W as f32 - x0, band], [red, 1.0, 1.0, ga])
                    .with_uv_rect(u0, v0, 1.0, v0 + band / CANVAS_H as f32);
                q.shade = Shade::ForceAlpha;
                out.push(add(q));
            }
            if focus {
                // texture 144 underline at (x - 16.333, y + 11.615), x scale (t * 0.9 + 0.1) / 3
                if let Some(u) = tex(2) {
                    out.push(add(rect(&u, (x - 16.333) * UX, top + 11.615 * UY, c.hover_t * 0.9 + 0.1, 1.0, [1.0, 1.0, 1.0, ga])));
                }
                // texture 146 sparkle at (x - 153.33, y - 30.696), pulsing
                if let Some(s) = tex(4) {
                    out.push(add(rect(&s, (x - 153.33) * UX, top - 30.696 * UY, 1.0, 1.0, [1.0, 1.0, 1.0, a * c.pulse])));
                }
            }
        }
        if c.state == 4 {
            let picked = c.hover.unwrap_or(0) as f32;
            let first = c.top - lay.ascent;
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

/// sub_63EB0's input regions (unk_4594E0..: x0, y0, x1, y1 in 640x448 units) per figure (region 190 + k is
/// figure dword_26291C[k]).
const WIK_RECTS: [[f32; 4]; 14] = [
    [0.0, 91.0, 66.0, 447.0], [64.0, 108.0, 109.0, 447.0], [93.0, 63.0, 144.0, 447.0], [143.0, 76.0, 192.0, 447.0],
    [185.0, 41.0, 237.0, 447.0], [235.0, 68.0, 293.0, 447.0], [266.0, 119.0, 316.0, 447.0], [316.0, 141.0, 358.0, 447.0],
    [317.0, 7.0, 395.0, 447.0], [366.0, 90.0, 417.0, 447.0], [417.0, 89.0, 470.0, 447.0], [463.0, 57.0, 520.0, 447.0],
    [517.0, 87.0, 560.0, 447.0], [560.0, 91.0, 640.0, 447.0],
];

/// dword_26291C: the figure of input region 190 + k.
const WIK_REGIONS: [usize; 14] = [7, 6, 1, 0, 13, 9, 10, 12, 3, 5, 2, 11, 4, 8];

/// Figure positions (unk_262954, 640x448 units, top-left; the images at native scale, 0.33333 x 0.41481).
const WIK_POS: [(f32, f32); 14] = [
    (-10.0, 91.0), (46.0, 108.0), (58.0, 63.0), (99.0, 76.0), (140.0, 41.0), (213.0, 68.0), (253.0, 119.0),
    (303.0, 141.0), (297.0, 7.0), (353.0, 90.0), (398.0, 89.0), (441.0, 57.0), (486.0, 87.0), (530.0, 91.0),
];

/// sub_656A0's draw order of the unfocused figures.
const WIK_ORDER: [usize; 14] = [8, 4, 11, 2, 5, 3, 12, 10, 9, 13, 0, 1, 6, 7];

impl Game {
    /// sub_656A0 (SEWI, drawn after the page text): black over the screen, the figures (wik_sel_parts: entry 2n
    /// in colour for the focused one, 2n + 1 mono for the rest), then black at 1 - the choice alpha.
    pub(super) fn wik_quads(&mut self, r: &Renderer, out: &mut Vec<Quad>) {
        let Some(c) = &self.choice else { return };
        if c.kind != ChoiceKind::Wik {
            return;
        }
        let (alpha, hover) = (c.alpha, c.hover);
        if self.wik_parts.is_none() {
            self.wik_parts = Some(match self.assets.ui_textures(r, "WIK_SEL_PARTS") {
                Ok(t) => t.into_iter().map(|(_, t)| t).collect(),
                Err(e) => {
                    tracing::warn!("WIK_SEL_PARTS: {e:#}");
                    Vec::new()
                }
            });
        }
        let parts = self.wik_parts.clone().unwrap_or_default();
        let black = |a: f32| Quad::rect(r.white.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [0.0, 0.0, 0.0, a]);
        out.push(black(1.0));
        if alpha > 0.0 {
            let fig = |i: usize, colour: bool| {
                let t = parts.get(2 * i + usize::from(!colour))?;
                let (x, y) = WIK_POS[i];
                let uv = t.uv_max();
                Some(
                    Quad::rect(t.clone(), [x * UX, y * UY, t.width as f32, t.height as f32], [1.0; 4])
                        .with_uv_rect(0.0, 0.0, uv[0], uv[1]),
                )
            };
            for &i in &WIK_ORDER {
                if hover != Some(i) {
                    out.extend(fig(i, false));
                }
            }
            if let Some(h) = hover {
                out.extend(fig(h, true));
            }
        }
        if alpha < 1.0 {
            out.push(black(1.0 - alpha));
        }
    }

    /// sub_64F10 (Witch on the Holy Night, 640x448 units): per option the sel_win bar (shader 27, anchor 1 = top
    /// centre, scale 0.42333 x 0.41481) centred at x + 320, top y + 22, then its text slot centred there with its
    /// top at y + 32 (two lines: 31; English 3 lower), all at alpha choice x window alpha.
    fn witch_choice_quads(&mut self, r: &Renderer, out: &mut Vec<Quad>, page_alpha: f32) {
        let Some(c) = &self.choice else { return };
        let a = c.alpha * page_alpha;
        if a <= 0.0 {
            return;
        }
        let lay = witch_option_layout();
        let bar = self.disp.get(7).cloned();
        let rows = c.witch_rows();
        let en = text::english();
        let mut glyphs = Vec::new();
        for (i, &(x, y)) in rows.iter().enumerate() {
            let l = c.lines[i];
            let two = usize::from(l > 1);
            let look = if c.hover != Some(i) { 0 } else if c.state == 4 { 2 } else { 1 };
            if let Some(t) = &bar {
                let [x0, y0, x1, y1] = SEL_WIN[look][two];
                let (w, h) = ((x1 - x0) * 0.42333 * UX, (y1 - y0) * 0.41481 * UY);
                let (tw, th) = (t.width as f32, t.height as f32);
                out.push(
                    Quad::rect(t.clone(), [(x + 320.0) * UX - w / 2.0, (y + 22.0) * UY, w, h], [1.0, 1.0, 1.0, a])
                        .with_uv_rect(x0 / tw, y0 / th, x1 / tw, y1 / th),
                );
            }
            let top = y + if l > 1 { 31.0 } else { 32.0 } + if en { 3.0 } else { 0.0 };
            let (dx, dy) = ((x + 320.0) * UX, top * UY + lay.ascent);
            for p in &c.placed[i] {
                let mut p = *p;
                p.x += dx;
                p.baseline += dy;
                glyphs.push(p);
            }
        }
        for p in &glyphs {
            Self::rimmed_glyph(&mut self.font, r, out, p, a);
        }
    }
}

/// SELS / SELA buttons (sub_85900: objects 217 / 218 from unk_65BBC0): No at x 241, Yes at x 327 (640x448
/// units, top-left), dialog_btn_ja frames as in the confirm dialog.
pub const DIALOG_BUTTONS: [(f32, bool); 2] = [(241.0, false), (327.0, true)];
pub const DIALOG_BUTTON_Y: f32 = 238.52;

