//! The game: script interpreter, stage, message page, choices and frame rendering.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use knife_core::vm::parse::{Command, Op, Scene};
use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, RenderTarget, Renderer, Shade, TexRef};

use crate::{
    assets::Assets,
    motion::MotionTable,
    sound::Sound,
    stage::{Layer, Source, Stage, Tween},
    text::{self, FontAtlas, Layout, PlacedChar},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wait {
    None,
    /// `click`: a click ends the wait early
    Time { left: f32, click: bool },
    Click,
    Transition,
    Layer(i32),
    Choice,
    Voice,
    Movie,
    Halt,
}

struct Transition {
    dur: f32,
    t: f32,
    rule: Option<TexRef>,
}

#[derive(Default)]
pub struct Page {
    pub chars: Vec<PlacedChar>,
    /// raw texts of the messages on the page (for saves)
    texts: Vec<String>,
    pen: (f32, f32),
    order: usize,
    revealed: f32,
    visible: bool,
    alpha: f32,
    /// the page waits for a click before more text (shown as an icon)
    waiting: bool,
}

#[derive(Clone, Debug)]
pub struct ChoiceOpt {
    pub value: i32,
    pub text: String,
}

pub struct Choice {
    var: String,
    opts: Vec<ChoiceOpt>,
    hover: Option<usize>,
}

pub const LAYOUT: Layout = Layout {
    left: 250.0,
    right: 1670.0,
    size: 40.0,
    line_height: 70.0,
    ruby_size: 18.0,
};
const TOP_BASELINE: f32 = 150.0;
const BOTTOM_LIMIT: f32 = 1000.0;
const PARAGRAPH_GAP: f32 = 0.0;
const WINDOW_ALPHA: f32 = 0.6;

pub struct Settings {
    /// characters per second (0 = instant)
    pub text_speed: f32,
    pub auto_delay_ms: f32,
    /// skip also skips text that has never been read
    pub skip_unread: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            text_speed: 60.0,
            auto_delay_ms: 1500.0,
            skip_unread: false,
        }
    }
}

pub struct Game {
    pub assets: Assets,
    motions: MotionTable,
    pub stage: Stage,
    pub sound: Sound,
    font: FontAtlas,
    pub settings: Settings,

    pub scene_index: usize,
    script: Arc<Scene>,
    pc: usize,
    pub wait: Wait,
    pub vars: HashMap<String, i32>,
    page: Page,
    sel_opts: Vec<ChoiceOpt>,
    choice: Option<Choice>,
    trans: Option<Transition>,
    snapshot: bool,
    shake: Option<(f32, f32, f32)>,
    /// MSQK message-text quake: (remaining ms, amp x px, amp y px) and the current offset
    msg_quake: Option<(f32, f32, f32)>,
    msg_offset: (f32, f32),
    pub skip: bool,
    /// skip was stopped by unread text; stays off until the skip key is released
    skip_blocked: bool,
    /// debug: skip never stops at unread text (not persisted)
    pub skip_all: bool,
    /// persistent bitmaps: script_text lines read, flowchart nodes (RDST), endings (RDED)
    pub read: Vec<u64>,
    pub nodes: Vec<u64>,
    pub endings: Vec<u64>,
    pub node_states: std::collections::BTreeMap<u16, crate::save::NodeRecord>,
    /// images shown at least once (uppercase names), for CG gallery unlocks
    pub seen: std::collections::HashSet<String>,
    /// BGM numbers played at least once (music gallery, exe bit 3200+n)
    pub heard: Vec<u64>,
    /// movies watched (exe sub_92400 flags): 0 op1, 1 op2, 2 prologue, 3..5 endings 1..3
    pub movie_flags: u16,
    /// a gallery movie is playing while the game is paused
    pub gallery_movie: bool,
    pub auto: bool,
    /// debug: pick the first option of every choice
    pub auto_choice: bool,
    /// a system screen is open: freeze the script and animations
    pub paused: bool,
    auto_timer: f32,
    pub ended: bool,
    mouse: (f32, f32),
    time: f32,
    warned: HashSet<String>,
    pub backlog: Vec<String>,
    movie: Option<crate::movie::Movie>,
    /// command index of the message currently shown
    msg_pc: usize,
    stuck: (usize, f32),
    /// set when a choice appears; the UI writes the autosave slot
    pub autosave_request: bool,
    /// WNTY: 0 = line-break icon, 1 = page-break icon
    wnty: i32,
    /// animated wait icons (line break, page break): 105 frames of 39x39
    icons: Option<(TexRef, TexRef)>,

    stage_rt: RenderTarget,
    /// STNS render target 40, created on first use
    noise_rt: Option<RenderTarget>,
    old_rt: RenderTarget,
    blur_rt: RenderTarget,
    /// FADE 40: blur the stage (FADE 41 clears)
    post_blur: bool,
    canvas: RenderTarget,
}

fn rgb(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 0xFF) as f32 / 255.0,
        ((c >> 8) & 0xFF) as f32 / 255.0,
        (c & 0xFF) as f32 / 255.0,
    ]
}

/// STBR mode -> our blend/shade approximation (mode + 13 indexes the exe's shader table).
fn blend_mode(mode: i32) -> (Blend, Shade) {
    match mode {
        5 | 14 | 21 | 22 | 7 | 12 => (Blend::Add, Shade::Sprite),
        6 | 16 | 8 | 23 | 9 | 25 => (Blend::Multiply, Shade::Sprite),
        11 | 17 | 10 | 24 | 18 | 20 => (Blend::Screen, Shade::Sprite),
        13 | 15 => (Blend::Sub, Shade::Sprite),
        _ => (Blend::Alpha, Shade::Sprite),
    }
}

impl Game {
    pub fn new(assets: Assets, motions: MotionTable, font: FontAtlas, r: &Renderer) -> Self {
        let script = Arc::new(Scene::parse(b""));
        let icons = assets.ui_textures(r, "DISP_PARTS_JA").ok().and_then(|t| {
            let find = |n: &str| t.iter().find(|(name, _)| name == n).map(|(_, t)| t.clone());
            Some((find("lineBreak_a")?, find("pageBreak_a")?))
        });
        let mut g = Self::new_inner(assets, motions, font, r, script);
        g.icons = icons;
        g
    }

    fn new_inner(assets: Assets, motions: MotionTable, font: FontAtlas, r: &Renderer, script: Arc<Scene>) -> Self {
        Self {
            assets,
            motions,
            stage: Stage::new(),
            sound: Sound::new(),
            font,
            settings: Settings::default(),
            scene_index: 0,
            script,
            pc: 0,
            wait: Wait::Halt,
            vars: HashMap::new(),
            page: Page::default(),
            sel_opts: Vec::new(),
            choice: None,
            trans: None,
            snapshot: false,
            shake: None,
            msg_quake: None,
            msg_offset: (0.0, 0.0),
            skip: false,
            skip_blocked: false,
            skip_all: false,
            read: Vec::new(),
            nodes: Vec::new(),
            endings: Vec::new(),
            node_states: Default::default(),
            seen: Default::default(),
            heard: Vec::new(),
            movie_flags: 0,
            gallery_movie: false,
            auto: false,
            auto_choice: false,
            paused: false,
            auto_timer: 0.0,
            ended: false,
            mouse: (0.0, 0.0),
            time: 0.0,
            warned: HashSet::new(),
            backlog: Vec::new(),
            movie: None,
            msg_pc: 0,
            stuck: (0, 0.0),
            autosave_request: false,
            wnty: 0,
            icons: None,
            stage_rt: r.create_target(CANVAS_W, CANVAS_H, "stage"),
            noise_rt: None,
            old_rt: r.create_target(CANVAS_W, CANVAS_H, "old frame"),
            blur_rt: r.create_target(CANVAS_W, CANVAS_H, "blur"),
            post_blur: false,
            canvas: r.create_target(CANVAS_W, CANVAS_H, "canvas"),
        }
    }

    // ---- script control -------------------------------------------------------

    pub fn start_scene(&mut self, index: usize, label: Option<&str>) {
        let Some(script) = self.assets.scene(index) else {
            tracing::error!("scene {index} missing");
            self.ended = true;
            return;
        };
        tracing::info!("scene {index} {} label {label:?}", self.assets.scene_name(index));
        // debugging: `@N` starts at command index N, `@NAME(a,b)` 8 commands before the
        // first matching command
        let pc = label
            .and_then(|l| match l.strip_prefix('@') {
                Some(n) => n.parse().ok().or_else(|| {
                    script
                        .commands
                        .iter()
                        .position(|c| format!("{}({})", c.name, c.args.join(",")).starts_with(n))
                        .map(|p| p.saturating_sub(8))
                }),
                None => script.label(l),
            })
            .unwrap_or(0);
        self.scene_index = index;
        self.script = script;
        self.pc = pc;
        self.wait = Wait::None;
        self.ended = false;
    }

    pub fn current_pc(&self) -> usize {
        self.pc
    }

    fn var(&self, name: &str) -> i32 {
        self.vars.get(&name.to_ascii_uppercase()).copied().unwrap_or(0)
    }

    fn value(&self, s: Option<&str>) -> i32 {
        match s {
            Some(v) if v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') => self.var(v),
            Some(v) => knife_core::vm::parse::eval_int(v).unwrap_or(0),
            None => 0,
        }
    }

    fn set_var(&mut self, name: &str, v: i32) {
        self.vars.insert(name.to_ascii_uppercase(), v);
    }

    fn goto_label(&mut self, label: &str) {
        match self.script.label(label) {
            Some(pc) => self.pc = pc,
            None => tracing::warn!("label {label} not found in scene {}", self.scene_index),
        }
    }

    fn warn_once(&mut self, what: &str) {
        if self.warned.insert(what.to_string()) {
            tracing::info!("unimplemented: {what}");
        }
    }

    /// Hold-to-skip input; skipping stays blocked after hitting unread text until released.
    pub fn set_skip(&mut self, held: bool) {
        if !held {
            self.skip_blocked = false;
        }
        self.skip = held && !self.skip_blocked;
    }

    /// Mark a `$NNNNNN` message line as read, stopping skip at unread text.
    fn note_read(&mut self, s: &str) {
        let Some(rest) = s.strip_prefix('$') else { return };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(i) = digits.parse::<usize>() else { return };
        if !set_bit(&mut self.read, i) && self.skip && !self.settings.skip_unread && !self.skip_all {
            tracing::info!("skip stopped at unread line {i}");
            self.skip = false;
            self.skip_blocked = true;
        }
    }

    fn resolve_text(&self, s: &str) -> String {
        // `$NNNNNN` references a script_text line, possibly followed by control codes
        if let Some(rest) = s.strip_prefix('$') {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(i) = digits.parse::<usize>() {
                return format!("{}{}", self.assets.line(i), &rest[digits.len()..]);
            }
        }
        s.to_string()
    }

    // ---- input ----------------------------------------------------------------

    /// Primary action (click / enter / space).
    pub fn advance(&mut self) {
        if let Some(c) = &self.choice {
            if let Some(h) = c.hover {
                self.pick_choice(h);
            }
            return;
        }
        if self.wait == Wait::Movie {
            self.stop_movie();
            return;
        }
        if let Wait::Time { click: true, .. } = self.wait {
            self.wait = Wait::None;
            return;
        }
        match self.wait {
            Wait::Click => {
                if (self.page.revealed as usize) < self.page.order {
                    self.page.revealed = self.page.order as f32;
                } else {
                    self.finish_click();
                }
            }
            Wait::Transition => {}
            _ => {}
        }
    }

    fn finish_click(&mut self) {
        self.page.waiting = false;
        self.wait = Wait::None;
        self.auto_timer = 0.0;
        self.sound.stop_voice();
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        if self.choice.is_some() {
            let h = self.choice_hit(x, y);
            if let Some(c) = &mut self.choice
                && h.is_some()
            {
                c.hover = h;
            }
        }
    }

    pub fn choice_move(&mut self, delta: i32) {
        if let Some(c) = &mut self.choice {
            let n = c.opts.len() as i32;
            let cur = c.hover.map_or(if delta > 0 { -1 } else { 0 }, |h| h as i32);
            c.hover = Some((cur + delta).rem_euclid(n.max(1)) as usize);
        }
    }

    fn pick_choice(&mut self, i: usize) {
        if let Some(c) = self.choice.take() {
            let o = &c.opts[i];
            self.backlog.push(format!("→ {}", o.text));
            self.set_var(&c.var, o.value);
            self.wait = Wait::None;
        }
    }

    fn choice_rects(&self) -> Vec<[f32; 4]> {
        let Some(c) = &self.choice else { return Vec::new() };
        let n = c.opts.len() as f32;
        let (w, h, gap) = (1100.0, 90.0, 30.0);
        let total = n * h + (n - 1.0) * gap;
        let y0 = (CANVAS_H as f32 - total) / 2.0;
        (0..c.opts.len())
            .map(|i| [(CANVAS_W as f32 - w) / 2.0, y0 + i as f32 * (h + gap), w, h])
            .collect()
    }

    fn choice_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.choice_rects()
            .iter()
            .position(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3])
    }

    pub fn in_choice(&self) -> bool {
        self.choice.is_some()
    }

    // ---- per-frame update ---------------------------------------------------------

    pub fn update(&mut self, r: &Renderer, dt: f32) {
        self.time += dt;
        self.sound.update();
        if self.paused {
            if let Some(m) = &mut self.movie
                && (m.looping || self.gallery_movie)
            {
                m.update(r, dt);
                if m.finished {
                    self.stop_movie();
                }
            }
            return;
        }
        let fast = self.skip;
        let step = if fast { dt * 8.0 } else { dt };

        // layer tweens and motions
        self.stage.advance(step);
        for l in self.stage.layers.iter_mut().flatten() {
            if let Some(m) = &mut l.motion {
                let f = self.motions.advance(m, step);
                if let Some(x) = f.x {
                    l.x = Tween::fixed(x);
                }
                if let Some(y) = f.y {
                    l.y = Tween::fixed(y);
                }
                if let Some(a) = f.alpha {
                    l.alpha = Tween::fixed(a);
                }
                if let Some(z) = f.zoom_x {
                    l.zoom_x = Tween::fixed(z);
                }
                if let Some(z) = f.zoom_y {
                    l.zoom_y = Tween::fixed(z);
                }
                if let Some(rot) = f.rot {
                    l.rotation = Tween::fixed(rot);
                }
                if let Some(p) = f.prio {
                    l.prio = p;
                }
                if f.clip.iter().any(|c| c.is_some()) {
                    let mut c = l.clip.unwrap_or_default();
                    if let Some(v) = f.clip[0] {
                        c.x = v;
                    }
                    if let Some(v) = f.clip[1] {
                        c.y = v;
                    }
                    if let Some(v) = f.clip[2] {
                        c.w = v;
                    }
                    if let Some(v) = f.clip[3] {
                        c.h = v;
                    }
                    if let Some(v) = f.clip[4] {
                        c.rot = v / 10.0;
                    }
                    l.clip = Some(c);
                }
            }
        }
        // message quake: jitter that alternates direction every frame
        if let Some((left, ax, ay)) = &mut self.msg_quake {
            *left -= step;
            if *left <= 0.0 || fast {
                self.msg_quake = None;
                self.msg_offset = (0.0, 0.0);
            } else {
                let flip = if (self.time / 33.0) as u32 % 2 == 0 { 1.0 } else { -1.0 };
                self.msg_offset = (*ax * flip, *ay * -flip);
            }
        }
        // screen shake
        if let Some((amp, period, left)) = &mut self.shake {
            *left -= step;
            if *left <= 0.0 {
                self.shake = None;
                self.stage.shake = (0.0, 0.0);
            } else {
                let ph = (*left / period.max(1.0)) * std::f32::consts::PI;
                self.stage.shake = (0.0, ph.sin() * *amp);
            }
        }
        // transition
        if let Some(t) = &mut self.trans {
            t.t += step;
            if t.t >= t.dur || fast {
                self.trans = None;
            }
        }
        // window fade / text reveal
        let target = if self.page.visible { 1.0 } else { 0.0 };
        let k = (dt / 150.0).min(1.0);
        self.page.alpha += (target - self.page.alpha) * k.max(if fast { 1.0 } else { 0.0 });
        if (self.page.alpha - target).abs() < 0.01 {
            self.page.alpha = target;
        }
        if self.settings.text_speed <= 0.0 || fast {
            self.page.revealed = self.page.order as f32;
        } else {
            self.page.revealed = (self.page.revealed + dt / 1000.0 * self.settings.text_speed).min(self.page.order as f32);
        }

        // movie
        if let Some(m) = &mut self.movie {
            m.update(r, dt);
            if m.finished || (fast && self.wait == Wait::Movie) {
                self.stop_movie();
            }
        }

        // resolve waits
        match self.wait {
            Wait::Movie => {
                if self.movie.is_none() {
                    self.wait = Wait::None;
                }
            }
            Wait::Time { left, click } => {
                let left = left - step;
                self.wait = if left <= 0.0 || fast { Wait::None } else { Wait::Time { left, click } };
            }
            Wait::Transition => {
                if self.trans.is_none() {
                    self.wait = Wait::None;
                }
            }
            Wait::Layer(l) => {
                if fast {
                    for layer in self.stage.layers.iter_mut().flatten() {
                        layer.finish();
                    }
                }
                if !self.stage.animating(l) || fast {
                    self.wait = Wait::None;
                }
            }
            Wait::Voice => {
                if !self.sound.voice_playing() || fast {
                    self.wait = Wait::None;
                }
            }
            Wait::Click => {
                let revealed = self.page.revealed as usize >= self.page.order;
                if fast && revealed {
                    self.finish_click();
                } else if self.auto && revealed && !self.sound.voice_playing() {
                    self.auto_timer += dt;
                    if self.auto_timer >= self.settings.auto_delay_ms {
                        self.finish_click();
                    }
                }
            }
            _ => {}
        }

        // debug aid: report long waits while skipping
        if fast {
            if self.pc == self.stuck.0 {
                self.stuck.1 += dt;
                if self.stuck.1 > 5000.0 && self.stuck.1 - dt <= 5000.0 {
                    tracing::warn!(
                        "stuck at scene {} pc {} wait {:?}: {:?}",
                        self.scene_index,
                        self.pc,
                        self.wait,
                        self.script.commands.get(self.pc.saturating_sub(1)).map(|c| (&c.name, &c.args))
                    );
                }
            } else {
                self.stuck = (self.pc, 0.0);
            }
        }

        // run the script
        let mut budget = 20000;
        while self.wait == Wait::None && !self.ended && budget > 0 {
            budget -= 1;
            let Some(cmd) = self.script.commands.get(self.pc).cloned() else {
                tracing::info!("scene {} ran off the end", self.scene_index);
                self.ended = true;
                break;
            };
            self.pc += 1;
            self.exec(r, &cmd);
        }
        self.assets.trim_images(400);
    }

    // ---- commands ---------------------------------------------------------------

    fn exec(&mut self, r: &Renderer, c: &Command) {
        match &c.op {
            Op::Marker => return,
            Op::Message => {
                let raw = c.args.first().map(String::as_str).unwrap_or("");
                self.note_read(raw);
                let text = self.resolve_text(raw);
                self.show_message(&text, true);
                return;
            }
            Op::Cmd(_) => {}
        }
        let name = c.name.as_str();
        match name {
            // variables and flow
            "WKST" => {
                let v = self.value(c.arg(2));
                if let Some(var) = c.arg(0) {
                    self.set_var(var, v);
                }
            }
            "WKAD" => {
                if let Some(var) = c.arg(0) {
                    let v = self.var(var) + self.value(c.arg(1));
                    self.set_var(var, v);
                }
            }
            "WKOR" => {
                if let Some(var) = c.arg(0) {
                    let v = self.var(var) | self.value(c.arg(1));
                    self.set_var(var, v);
                }
            }
            "IF__" => {
                let a = self.value(c.arg(0));
                let b = self.value(c.arg(2));
                let cond = match c.arg(1).unwrap_or("eq") {
                    "eq" => a == b,
                    "ne" => a != b,
                    "gt" => a > b,
                    "lt" => a < b,
                    "ge" => a >= b,
                    "le" => a <= b,
                    "an" => a & b != 0,
                    op => {
                        tracing::warn!("IF__ op {op}");
                        false
                    }
                };
                if !cond && let Some(l) = c.arg(3) {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            "IFNX" | "IFP4" | "IFSC" | "IFUR" => {}
            "GOTO" => {
                if let Some(l) = c.arg(0) {
                    let l = l.to_string();
                    self.goto_label(&l);
                }
            }
            "GOTS" => {
                let v = self.value(c.arg(0));
                if let Some(l) = c.args.get(1 + v.max(0) as usize).filter(|s| !s.is_empty()) {
                    let l = l.clone();
                    self.goto_label(&l);
                } else {
                    tracing::warn!("GOTS: no label for {v}");
                }
            }
            "JUMP" => {
                let id = c.int_or(0, -1);
                match self.assets.scene_by_id(id) {
                    Some(idx) => {
                        let label = c.arg(1).map(str::to_string);
                        self.start_scene(idx, label.as_deref());
                    }
                    None => {
                        tracing::error!("JUMP to unknown scene id {id}");
                        self.ended = true;
                    }
                }
            }
            "END_" | "RTM_" | "RTMN" | "RTM2" => {
                tracing::info!("{name}: scene finished");
                self.ended = true;
            }
            "WTTM" => {
                let ms = c.int_or(0, 0) as f32;
                if ms > 0.0 {
                    self.wait = Wait::Time {
                        left: ms,
                        click: c.int_or(1, 0) == 0,
                    };
                }
            }

            // messages
            "PGST" => self.clear_page(),
            "CLO2" | "CLO3" | "CLO4" | "CLO5" => {
                self.clear_page();
                self.page.visible = false;
            }
            "WNTY" => self.wnty = c.int_or(0, 0),
            "MSAD" => {
                if let Some(a) = c.args.first() {
                    self.note_read(a);
                    let text = self.resolve_text(a);
                    let wait = text.contains("@k");
                    self.show_message(&text, wait);
                    if !wait {
                        self.wait = Wait::None;
                    }
                }
            }

            // choices
            "SELR" => {
                let value = c.int_or(0, self.sel_opts.len() as i32);
                let text = self.resolve_text(c.arg(1).unwrap_or(""));
                let text: String = text::parse_markup(&text)
                    .into_iter()
                    .map(|r| match r {
                        text::Run::Text(t) => t,
                        text::Run::Ruby { base, .. } => base,
                        text::Run::Newline => String::new(),
                    })
                    .collect();
                self.sel_opts.push(ChoiceOpt { value, text });
            }
            "SELD" | "SELA" | "SELS" => {
                let var = c.arg(0).unwrap_or("_D000").to_string();
                let mut opts = std::mem::take(&mut self.sel_opts);
                if opts.is_empty() && name == "SELS" {
                    // dead end: view Ciel-sensei's Q&A corner? (GOTS SEL_NO, SEL_YES)
                    opts = vec![
                        ChoiceOpt { value: 1, text: "Q&A corner".into() },
                        ChoiceOpt { value: 0, text: "Return to the title".into() },
                    ];
                }
                if name == "SELD" && !opts.is_empty() {
                    self.autosave_request = true;
                }
                if opts.is_empty() {
                    tracing::debug!("{name} without options");
                } else {
                    self.page.visible = false;
                    self.choice = Some(Choice { var, opts, hover: None });
                    self.wait = Wait::Choice;
                    if self.auto_choice {
                        let n = self.choice.as_ref().map_or(1, |c| c.opts.len());
                        let seed = std::env::var("KNIFE_SEED").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
                        let pick = (self.pc.wrapping_mul(31) + self.scene_index * 7 + seed) % n.max(1);
                        tracing::info!("auto choice {pick}/{n}");
                        self.pick_choice(pick);
                    }
                }
            }

            // stage
            "STCH" | "SCH2" | "STCG" => self.cmd_stch(r, c),
            "STCC" => {
                let l = c.int_or(0, -1);
                self.stage.clear(l);
            }
            "SACL" => {
                let on = c.int_or(1, 0) != 0;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.absolute = on;
                }
            }
            "SBAS" => {
                let b = c.int_or(1, -1);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.base = usize::try_from(b).ok().filter(|&b| b < crate::stage::LAYERS);
                }
            }
            "STCP" => {
                let p = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.prio = p;
                }
            }
            "STZ4" => {
                let ms = c.int_or(5, 0) as f32;
                let ease = c.int_or(6, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    let (zx0, zy0) = (c.int(1), c.int(2));
                    let (zx1, zy1) = (c.int(3).or(zx0), c.int(4).or(zy0));
                    if let Some(z) = zx1 {
                        let from = zx0.map_or(l.zoom_x.value(), |v| v as f32 / 1000.0);
                        l.zoom_x = Tween::new(from, z as f32 / 1000.0, ms, ease);
                    }
                    if let Some(z) = zy1 {
                        let from = zy0.map_or(l.zoom_y.value(), |v| v as f32 / 1000.0);
                        l.zoom_y = Tween::new(from, z as f32 / 1000.0, ms, ease);
                    }
                }
            }
            // STCL(rel, layer, ...): position tween that stops the layer's motion.
            //   rel == 0: (x0, y0, x1, y1, ms, ease)   rel != 0: (dx, dy, ms, ease) relative to current
            "STCL" => {
                let rel = c.int_or(0, 0) != 0;
                if let Some(l) = self.stage.get(c.int_or(1, -1)) {
                    l.motion = None;
                    let (cx, cy) = (l.x.value(), l.y.value());
                    let (from, to, ms, ease) = if rel {
                        let to = (cx + c.int_or(2, 0) as f32, cy + c.int_or(3, 0) as f32);
                        ((cx, cy), to, c.int_or(4, 0), c.int_or(5, 0))
                    } else {
                        let from = (c.int(2).map_or(cx, |v| v as f32), c.int(3).map_or(cy, |v| v as f32));
                        let to = (c.int(4).map_or(from.0, |v| v as f32), c.int(5).map_or(from.1, |v| v as f32));
                        (from, to, c.int_or(6, 0), c.int_or(7, 0))
                    };
                    l.x = Tween::new(from.0, to.0, ms as f32, ease);
                    l.y = Tween::new(from.1, to.1, ms as f32, ease);
                }
            }
            "STRT" => {
                let ms = c.int_or(3, 0) as f32;
                let ease = c.int_or(4, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    let from = c.int(1).map_or(l.rotation.value(), |v| v as f32 / 100.0);
                    let to = c.int(2).map_or(from, |v| v as f32 / 100.0);
                    l.rotation = Tween::new(from, to, ms, ease);
                }
            }
            "STGS" => {
                let ms = c.int_or(3, 0) as f32;
                let ease = c.int_or(4, 0);
                let layer = c.int_or(0, -1);
                if let Some(l) = self.stage.get(layer) {
                    let from = c.int(1).map_or(l.alpha.value(), |v| v as f32 / 255.0);
                    let to = c.int(2).map_or(from, |v| v as f32 / 255.0);
                    l.alpha = Tween::new(from, to, ms, ease);
                }
            }
            "STPV" => {
                let ms = c.int_or(3, 0) as f32;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    if let Some(x) = c.int(1) {
                        l.x.retarget(x as f32, ms, 0);
                    }
                    if let Some(y) = c.int(2) {
                        l.y.retarget(y as f32, ms, 0);
                    }
                }
            }
            "STCF" => {
                let l = c.int_or(0, -1);
                if self.stage.animating(l) {
                    self.wait = Wait::Layer(l);
                }
            }
            "STEC" => {
                let mode = c.int_or(1, 0);
                let color = c.int(2).map(|v| rgb(v as u32));
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    match mode {
                        1 => l.fill = Some(color.unwrap_or([0.0; 3])),
                        // other modes select engine shaders whose mapping is not yet known
                        0 => {
                            l.fill = None;
                            l.shade = Shade::Sprite;
                        }
                        _ => {}
                    }
                }
            }
            "STBR" => {
                let (blend, _) = blend_mode(c.int_or(1, 0));
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.blend = blend;
                }
            }
            // MSQK(ms, ay, ax): shake the message text (exe stores ay*448/720 and ax*640/1280, 640x448 units)
            "MSQK" => {
                let ms = c.int_or(0, 1000).clamp(0, 10000) as f32;
                let ay = c.int_or(1, 0) as f32 * 448.0 / 720.0 * CANVAS_H as f32 / 448.0;
                let ax = c.int_or(2, 0) as f32 * 640.0 / 1280.0 * CANVAS_W as f32 / 640.0;
                self.msg_quake = (ms > 0.0).then_some((ms, ax, ay));
            }
            "SPMA" => {
                let pause = c.int_or(0, 0) == 1;
                for l in self.stage.layers.iter_mut().flatten() {
                    if let Some(m) = &mut l.motion {
                        m.paused = pause;
                    }
                }
            }
            "SGMA" => {
                let id = c.int_or(1, 0);
                let key = c.int_or(2, 0).max(0) as usize;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    use crate::motion::field;
                    let mut v = vec![
                        (field::X, l.x.value().round() as i32),
                        (field::Y, l.y.value().round() as i32),
                        (field::ALPHA, (l.alpha.value() * 255.0).round() as i32),
                        (field::ROT, (l.rotation.value() * 100.0).round() as i32),
                        (field::ZOOM_X, (l.zoom_x.value() * 1000.0).round() as i32),
                        (field::ZOOM_Y, (l.zoom_y.value() * 1000.0).round() as i32),
                    ];
                    if let Some(cr) = l.clip.filter(|cr| cr.w != 0.0 || cr.h != 0.0) {
                        v.extend([(11, cr.x as i32), (12, cr.y as i32), (13, cr.w as i32), (14, cr.h as i32)]);
                    }
                    self.motions.patch_key(id, key, &v);
                }
            }
            "STMA" => {
                let id = c.int_or(1, 0);
                let start = c.int_or(3, 0) as f32;
                let exists = self.motions.exists(id);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    if id > 0 && exists {
                        let mut m = crate::motion::MotionState::new(id, start);
                        // apply the first frame immediately
                        let f = self.motions.advance(&mut m, 0.0);
                        if let Some(x) = f.x {
                            l.x = Tween::fixed(x);
                        }
                        if let Some(y) = f.y {
                            l.y = Tween::fixed(y);
                        }
                        if let Some(a) = f.alpha {
                            l.alpha = Tween::fixed(a);
                        }
                        if let Some(z) = f.zoom_x {
                            l.zoom_x = Tween::fixed(z);
                        }
                        if let Some(z) = f.zoom_y {
                            l.zoom_y = Tween::fixed(z);
                        }
                        l.motion = Some(m);
                    } else {
                        l.motion = None;
                    }
                }
            }
            "FADS" => {
                let mode = c.int_or(0, 1);
                let dur = c.int_or(1, 0) as f32;
                let rule = c.int_or(2, 0);
                self.stage.commit();
                if mode == 1 || dur <= 0.0 || self.skip {
                    self.trans = None;
                } else {
                    let rule = (rule > 0)
                        .then(|| self.assets.image(r, &format!("RULE_{rule:02}")))
                        .flatten()
                        .map(|i| i.tex.clone());
                    self.snapshot = true;
                    self.trans = Some(Transition { dur, t: 0.0, rule });
                    self.wait = Wait::Transition;
                }
            }
            "SSW2" => {
                // (layer, amp_min=7, amp_max=15, period_min_ms=1000, period_max_ms=2000,
                //  strength=1, dur_ms=-1, fade)
                let li = c.int_or(0, -1);
                let dur = c.int_or(6, -1);
                let amp = (c.int_or(1, 7) as f32, c.int_or(2, 15) as f32);
                let period = (c.int_or(3, 1000) as f32, c.int_or(4, 2000) as f32);
                let strength = c.int_or(5, 1) as f32;
                if let Some(l) = self.stage.get(li) {
                    l.sway = (dur != 0 && strength != 0.0).then(|| {
                        let seed = (li as u32 + 1).wrapping_mul(2654435761) ^ self.pc as u32;
                        crate::stage::Sway::new(amp, period, strength, if dur < 0 { -1.0 } else { dur as f32 }, seed)
                    });
                }
            }
            // SPFC(layer, image, snx2, snx3, va, vb, stec, color): effect/texture precache
            "SPFC" => {
                if let Some(name) = c.args.get(1).filter(|n| !n.is_empty()) {
                    let _ = self.assets.image(r, name);
                }
            }
            // SPRD(layer, image, ?, ?, va, vb): precache of a variant image
            "SPRD" => {
                if let Some(name) = c.args.get(1).filter(|n| !n.is_empty()) {
                    let name = match (c.int(4), c.int(5)) {
                        (Some(a), Some(b)) if (a, b) != (0, 0) => format!("{name}_{a:02}_{b:02}"),
                        _ => name.clone(),
                    };
                    let _ = self.assets.image(r, &name);
                }
            }
            "STRA" => {}
            // known, intentionally unimplemented effects (see re/NOTES.md): SOXY scissor inset,
            // STNS grain overlay, STTR per-layer rule fade, FADE post filters, FCLN/FCSP screen
            // effects, SPRC render-target release
            // SOXY: scripts always pass (layer,20,20,20,20); the scissor inset needs arg4 in 1..6 and
            // no fragment shader reads the per-draw value it sets (checked by disassembly), so no effect;
            // FCLN never appears in the scripts, so FCSP (clear) has nothing to undo
            "SOXY" | "FCLN" | "FCSP" | "SPRC" => {}
            // XSTP/XRSM: start/resume an engine system task (sub_3F600/sub_3F9B0) around the staff roll
            "XSTP" | "XRSM" => {}
            // FADE(40, ..): blur the whole stage; FADE(41): off. Other modes are engine states.
            "FADE" => match c.int_or(0, 0) {
                40 => self.post_blur = true,
                41 => self.post_blur = false,
                _ => {}
            },
            // STNS(layer, n): grain overlay (always on white img2345 layers); 0 clears
            "STNS" => {
                self.load_noise(r);
                let n = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.noise = (n > 0).then_some(n);
                }
            }
            // STTR(layer, rule): following alpha fades wipe through RULE_nn; -1/0 clears
            "STTR" => {
                let n = c.int_or(1, -1);
                let tex = (n > 0)
                    .then(|| self.assets.image(r, &format!("RULE_{n:02}")))
                    .flatten()
                    .map(|i| (n, i.tex.clone()));
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.rule = tex;
                }
            } // per-layer effect seed for the sway/raster animation; no visible effect alone
            "SQK2" => {
                let li = c.int_or(0, -1);
                let ax = c.int_or(1, 15) as f32;
                let ay = c.int_or(2, 15) as f32;
                let period = c.int_or(3, 0).max(0) as f32 + 16.0;
                let dur = c.int_or(4, -1);
                let q = (ax != 0.0 || ay != 0.0) && dur != 0;
                let quake = q.then_some(crate::stage::Quake {
                    ax,
                    ay,
                    period,
                    left: if dur < 0 { -1.0e10 } else { dur as f32 },
                    t: 0.0,
                });
                let apply = |l: &mut Layer| {
                    l.quake = quake;
                    if quake.is_none() {
                        l.offset = (0.0, 0.0);
                    }
                };
                if li < 0 {
                    self.stage.layers.iter_mut().flatten().for_each(apply);
                } else if let Some(l) = self.stage.get(li) {
                    apply(l);
                }
            }
            "SNX2" => {
                let v = c.int_or(1, 0) as f32;
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.bright = 1.0 + v / 255.0;
                }
            }
            "STRR" => {
                let v = c.int_or(1, 0);
                if let Some(l) = self.stage.get(c.int_or(0, -1)) {
                    l.motion_rot = if v == -99999 { 0.0 } else { (v % 36000) as f32 / 100.0 };
                }
            }
            "SNX4" | "FANX" | "STQK" | "TRPY" | "SNX3" => {}
            "SHOK" | "SHK2" => {
                let amp = c.int_or(1, 3) as f32;
                let period = c.int_or(2, 100) as f32;
                let count = c.int_or(3, 5).max(1) as f32;
                if !self.skip {
                    self.shake = Some((amp, period, period * count));
                }
            }

            // audio
            "SEPL" => {
                let ch = c.int_or(0, 0);
                let looping = c.int_or(1, 0) != 0;
                if let Some(n) = c.arg(2) {
                    let vol = c.keyed_int(17).unwrap_or(100) as f32 / 100.0;
                    let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                    if let Some(pcm) = self.assets.sound_effect(n) {
                        if !(self.skip && !looping) {
                            self.sound.play_se(ch, n, pcm, vol, fade, looping);
                        }
                    }
                }
            }
            "SEFD" | "SESP" => {
                let ch = c.int_or(0, -1);
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                self.sound.stop_se(ch, fade);
            }
            "SEVL" => {
                let ch = c.int_or(0, -1);
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                if let Some(v) = c.keyed_int(17) {
                    self.sound.set_se_volume(ch, v as f32 / 100.0, fade);
                }
            }
            "MPLY" => {
                let n = c.int_or(0, 0);
                let vol = c.keyed_int(17).unwrap_or(100) as f32 / 100.0;
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                self.play_bgm(n, vol, fade);
            }
            "MFAD" => {
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                self.sound.stop_bgm(fade);
            }
            "MVOL" => {
                let fade = c.keyed_int(1).unwrap_or(0) as f64 / 1000.0;
                if let Some(v) = c.keyed_int(17) {
                    self.sound.set_bgm_volume(v as f32 / 100.0, fade);
                }
            }
            "VPLY" | "VPL2" => {
                if let Some(n) = c.arg(0)
                    && !self.skip
                {
                    match self.assets.voice(n) {
                        Ok(pcm) => self.sound.play_voice(Arc::new(pcm)),
                        Err(e) => tracing::warn!("voice {n}: {e:#}"),
                    }
                }
            }
            "VCWT" => {
                if self.sound.voice_playing() {
                    self.wait = Wait::Voice;
                }
            }
            "TOMP" => {
                let n = c.int_or(0, -1);
                // sub_40480: watching these movies sets the flags the movie/music galleries check
                if let Some(f) = [(5, 0), (6, 1), (4, 2), (35, 3), (36, 4), (37, 5)].iter().find(|m| m.0 == n) {
                    self.movie_flags |= 1 << f.1;
                }
                match crate::movie::MOVIES.get(n.max(0) as usize).filter(|_| n >= 0) {
                    Some(name) if !self.skip => self.play_movie(r, name),
                    Some(_) => {}
                    None => tracing::warn!("TOMP {n}: unknown movie"),
                }
            }
            "TOMW" => {
                if self.movie.is_some() {
                    self.wait = Wait::Movie;
                }
            }
            // bookkeeping commands with no visible effect here
            "RDST" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    set_bit(&mut self.nodes, n as usize);
                    let mut vars: Vec<(String, i32)> = self
                        .vars
                        .iter()
                        .filter(|(k, v)| !k.starts_with('G') && **v != 0)
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    vars.sort();
                    let rec = crate::save::NodeRecord {
                        scene: self.scene_index,
                        pc: self.pc.saturating_sub(1),
                        vars,
                    };
                    self.node_states.insert(n as u16, rec);
                }
            }
            "RDED" => {
                if let Some(n) = c.int(0).filter(|&n| (0..0x240).contains(&n)) {
                    set_bit(&mut self.endings, n as usize);
                }
            }
            "ATSV" | "RTST" | "RDAS" | "SHAF" | "SQS2" | "SHS2" | "SSS2" | "NOP_" | "WTSV" => {}
            other => self.warn_once(other),
        }
    }

    pub fn play_movie(&mut self, r: &Renderer, name: &str) {
        let path = self.assets.romfs.join(format!("movie/{name}.mp4"));
        match crate::movie::Movie::open(r, &path, false) {
            Ok(m) => {
                if let Some(pcm) = crate::movie::decode_audio(&path) {
                    self.sound.play_movie_audio(Arc::new(pcm));
                }
                self.movie = Some(m);
            }
            Err(e) => tracing::warn!("movie {name}: {e:#}"),
        }
    }

    pub fn play_title_movie(&mut self, r: &Renderer) {
        let path = self.assets.romfs.join("movie/titlebg1.mp4");
        match crate::movie::Movie::open(r, &path, true) {
            Ok(m) => self.movie = Some(m),
            Err(e) => tracing::warn!("title movie: {e:#}"),
        }
    }

    pub fn movie_active(&self) -> bool {
        self.movie.is_some()
    }

    pub fn movie_playing(&self) -> bool {
        self.movie.as_ref().is_some_and(|m| m.has_frame())
    }

    pub fn font_mut(&mut self) -> &mut FontAtlas {
        &mut self.font
    }

    pub fn stop_movie(&mut self) {
        if let Some(mut m) = self.movie.take() {
            m.stop();
        }
        self.sound.stop_movie_audio();
    }

    pub fn play_bgm(&mut self, n: i32, vol: f32, fade: f64) {
        if n <= 0 {
            self.sound.stop_bgm(fade);
            return;
        }
        set_bit(&mut self.heard, n as usize);
        match self.assets.sar.stream_sound(n as usize - 1) {
            Ok(s) => {
                let path = self.assets.romfs.join(&s.path);
                let base = s.volume;
                let loop_range = s.loop_range;
                self.sound.play_bgm(n, vol, fade, move || {
                    let data = std::fs::read(&path)?;
                    let mut pcm = knife_audio::codec::decode_nopus(&data)?;
                    let len = pcm.frames.len();
                    pcm.loop_range = Some(loop_range.map_or((0, len), |(a, b)| (a.min(len), b.min(len))));
                    Ok((pcm, base))
                });
            }
            Err(e) => tracing::warn!("MPLY {n}: {e:#}"),
        }
    }

    fn cmd_stch(&mut self, r: &Renderer, c: &Command) {
        let li = c.int_or(0, -1);
        if c.name == "STCG" {
            return;
        }
        let Some(base) = c.arg(1).map(str::to_string) else { return };
        // copy state from a source layer (SCH2 arg 18) or the layer being replaced
        let src_layer = c.int(18).filter(|&s| s >= 0);
        let prev = src_layer
            .and_then(|s| self.stage.layers.get(s as usize).cloned().flatten())
            .or_else(|| self.stage.layers.get(li.max(0) as usize).cloned().flatten());
        let variant = match (c.int(9), c.int(10)) {
            // `_00_00` is the base image itself
            (Some(a), Some(b)) if (a, b) != (0, 0) => Some(format!("{base}_{a:02}_{b:02}")),
            _ => None,
        };
        let upper = base.to_ascii_uppercase();
        let source = match upper.as_str() {
            "BLACK" => Source::Color([0.0; 3]),
            "WHITE" => Source::Color([1.0; 3]),
            _ => {
                let img = variant
                    .as_deref()
                    .and_then(|v| self.assets.image(r, v))
                    .or_else(|| self.assets.image(r, &base));
                match img {
                    Some(i) => Source::Image(i),
                    None => return,
                }
            }
        };
        let x = c.int(2).map(|v| v as f32).or(prev.as_ref().map(|p| p.x.value())).unwrap_or(240.0);
        let y = c.int(3).map(|v| v as f32).or(prev.as_ref().map(|p| p.y.value())).unwrap_or(136.0);
        let prio = c.int(6).or(prev.as_ref().map(|p| p.prio)).unwrap_or(li * 10);
        // the exe marks shown images in a persistent bitmap (sub_774F0) for gallery unlocks
        if !matches!(upper.as_str(), "BLACK" | "WHITE") {
            self.seen.insert(upper.clone());
            if let Some(v) = &variant {
                self.seen.insert(v.to_ascii_uppercase());
            }
        }
        let mut l = Layer::new(variant.unwrap_or(base), source, x, y, prio);
        if let Some(p) = &prev {
            l.zoom_x = Tween::fixed(p.zoom_x.value());
            l.zoom_y = Tween::fixed(p.zoom_y.value());
            l.alpha = Tween::fixed(p.alpha.value());
            l.rotation = Tween::fixed(p.rotation.value());
            l.parent = p.parent;
            l.blend = p.blend;
            if src_layer.is_some() {
                l.fill = p.fill;
                l.shade = p.shade;
            }
        }
        if let Some(z) = c.int(4) {
            l.zoom_x = Tween::fixed(z as f32 / 1000.0);
            l.zoom_y = Tween::fixed(z as f32 / 1000.0);
        }
        if let Some(a) = c.int(5) {
            l.alpha = Tween::fixed(a as f32 / 255.0);
        }
        if let Some(p) = c.int(11) {
            l.parent = (p >= 0).then_some(p as usize);
        }
        if let (Some(x), Some(y), Some(w), Some(h)) = (c.int(12), c.int(13), c.int(14), c.int(15)) {
            l.clip = Some(crate::stage::ClipRect {
                x: x as f32,
                y: y as f32,
                w: w as f32,
                h: h as f32,
                rot: c.int_or(16, 0) as f32 / 10.0,
            });
        } else if let Some(p) = &prev
            && src_layer.is_some()
        {
            l.clip = p.clip;
        }
        if let Some(rot) = c.int(19) {
            l.rotation = Tween::fixed((rot % 36000) as f32 / 100.0);
        }
        self.stage.set(li, l);
    }

    // ---- messages -----------------------------------------------------------------

    fn clear_page(&mut self) {
        self.page.chars.clear();
        self.page.texts.clear();
        self.page.order = 0;
        self.page.revealed = 0.0;
        self.page.pen = (LAYOUT.left, TOP_BASELINE);
    }

    fn show_message(&mut self, text: &str, wait: bool) {
        let runs = text::parse_markup(text);
        let plain: String = runs
            .iter()
            .map(|r| match r {
                text::Run::Text(t) => t.as_str(),
                text::Run::Ruby { base, .. } => base.as_str(),
                text::Run::Newline => "\n",
            })
            .collect();
        if !plain.trim().is_empty() {
            self.backlog.push(plain);
            if self.backlog.len() > 2000 {
                self.backlog.drain(..500);
            }
        }
        self.page.visible = true;
        // lay out on a scratch copy; restart the page if it overflows
        let start = if self.page.chars.is_empty() {
            (LAYOUT.left, TOP_BASELINE)
        } else {
            (LAYOUT.left, self.page.pen.1 + LAYOUT.line_height + PARAGRAPH_GAP)
        };
        let mut placed = Vec::new();
        let (x, b, order) = text::layout(&self.font, &LAYOUT, &runs, start.0, start.1, self.page.order, &mut placed);
        self.msg_pc = self.pc.saturating_sub(1);
        if b > BOTTOM_LIMIT && !self.page.chars.is_empty() {
            self.clear_page();
            placed.clear();
            let (x, b, order) = text::layout(&self.font, &LAYOUT, &runs, LAYOUT.left, TOP_BASELINE, 0, &mut placed);
            self.page.pen = (x, b);
            self.page.order = order;
        } else {
            self.page.pen = (x, b);
            self.page.order = order;
        }
        self.page.chars.extend(placed);
        self.page.texts.push(text.to_string());
        if wait {
            self.page.waiting = true;
            self.wait = Wait::Click;
            self.auto_timer = 0.0;
        }
    }

    // ---- rendering ------------------------------------------------------------------

    fn ui_quads(&mut self, r: &Renderer) -> Vec<Quad> {
        let mut out = Vec::new();
        let a = self.page.alpha;
        if a > 0.0 {
            out.push(Quad::rect(
                r.white.clone(),
                [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32],
                [0.0, 0.0, 0.0, WINDOW_ALPHA * a],
            ));
            let revealed = self.page.revealed;
            let (ox, oy) = self.msg_offset;
            for pc in &self.page.chars {
                let pc = &text::PlacedChar { x: pc.x + ox, baseline: pc.baseline + oy, ..*pc };
                let vis = (revealed - pc.order as f32).clamp(0.0, 1.0);
                if vis <= 0.0 {
                    continue;
                }
                let alpha = vis * a;
                if let Some(q) = self.font.quad(r, pc.c, pc.size, pc.x + 2.0, pc.baseline + 2.0, [0.0, 0.0, 0.0, 0.7 * alpha]) {
                    out.push(q);
                }
                if let Some(q) = self.font.quad(r, pc.c, pc.size, pc.x, pc.baseline, [1.0, 1.0, 1.0, alpha]) {
                    out.push(q);
                }
            }
            if self.wait == Wait::Click && revealed as usize >= self.page.order {
                let (x, y) = (self.page.pen.0 + 6.0, self.page.pen.1);
                match &self.icons {
                    Some((line, page)) => {
                        let t = if self.wnty == 1 { page } else { line };
                        let frame = ((self.time / 33.3) as u32 % 105) as f32;
                        let tw = t.texture.size().width as f32;
                        let th = t.texture.size().height as f32;
                        let size = 39.0;
                        out.push(
                            Quad::rect(t.clone(), [x, y - size + 6.0, size, size], [1.0, 1.0, 1.0, a])
                                .with_uv_rect(frame * 39.0 / tw, 0.0, (frame + 1.0) * 39.0 / tw, 39.0 / th),
                        );
                    }
                    None => {
                        let blink = ((self.time / 400.0).sin() * 0.5 + 0.5) * a;
                        if let Some(q) = self.font.quad(r, '▼', 28.0, x, y, [1.0, 1.0, 1.0, blink]) {
                            out.push(q);
                        }
                    }
                }
            }
        }
        if let Some(c) = &self.choice {
            let rects = self.choice_rects();
            let opts = c.opts.clone();
            let hover = c.hover;
            out.push(Quad::rect(
                r.white.clone(),
                [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32],
                [0.0, 0.0, 0.0, 0.4],
            ));
            for (i, (o, rc)) in opts.iter().zip(rects).enumerate() {
                let hl = hover == Some(i);
                let bg = if hl { [0.35, 0.3, 0.45, 0.85] } else { [0.05, 0.05, 0.1, 0.75] };
                out.push(Quad::rect(r.white.clone(), rc, bg));
                let size = 40.0;
                let w = self.font.measure(&o.text, size);
                let x = rc[0] + (rc[2] - w) / 2.0;
                let base = rc[1] + rc[3] / 2.0 + size * 0.35;
                self.font.draw_str(r, &mut out, &o.text, size, x, base, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        out
    }

    fn load_noise(&mut self, r: &Renderer) {
        if self.stage.noise.is_none() {
            self.stage.noise = self.assets.ui_textures(r, "NOISE_PARTS").ok().and_then(|t| {
                let find = |n: &str| t.iter().find(|(name, _)| name == n).map(|(_, t)| t.clone());
                // texture ids 389/390 = NOISE_PARTS entries 1/2 (the exe's texture table at 0x572C10)
                Some([find("noise1")?, find("noisehd0")?])
            });
        }
    }

    pub fn render(&mut self, r: &mut Renderer, overlay: &mut dyn FnMut(&mut Self, &Renderer, &mut Vec<Quad>)) {
        let mut enc = r.encoder();
        if self.snapshot {
            r.copy_target(&mut enc, &self.stage_rt, &self.old_rt);
            self.snapshot = false;
        }
        if let Some(q) = self.stage.noise_pass() {
            let fresh = self.noise_rt.is_none();
            let rt = self.noise_rt.get_or_insert_with(|| r.create_target(CANVAS_W, CANVAS_H, "noise rt40"));
            if fresh {
                // nothing earlier to show through the uncovered strip yet: start from a full-screen copy
                let full = Quad::rect(q.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]);
                r.draw(&mut enc, rt, Some([0.0, 0.0, 0.0, 1.0]), &[full]);
            }
            r.draw(&mut enc, rt, None, &[q]);
            self.stage.noise_rt = Some(rt.tex.clone());
        }
        let quads = self.stage.quads(&r.white);
        r.draw(&mut enc, &self.stage_rt, Some([0.0, 0.0, 0.0, 1.0]), &quads);
        if self.post_blur {
            // separable gaussian; FADE 40 uses sigma ~3.1 in 640-wide units -> 3 px taps
            let step = CANVAS_W as f32 / 640.0;
            r.blur(&mut enc, &self.stage_rt.tex, &self.blur_rt, [step, 0.0], 3.1);
            r.blur(&mut enc, &self.blur_rt.tex, &self.stage_rt, [0.0, step], 3.1);
        }
        match &self.trans {
            Some(t) => {
                let p = (t.t / t.dur).clamp(0.0, 1.0);
                let rule = t.rule.clone();
                r.transition(&mut enc, &self.canvas, &self.old_rt.tex, &self.stage_rt.tex, rule.as_deref(), p, 0.25, false);
            }
            None => r.copy_target(&mut enc, &self.stage_rt, &self.canvas),
        }
        let mut ui = Vec::new();
        if let Some(m) = &self.movie
            && m.has_frame()
        {
            ui.push(Quad::rect(m.tex.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [1.0; 4]));
        }
        ui.extend(self.ui_quads(r));
        overlay(self, r, &mut ui);
        if !ui.is_empty() {
            r.draw(&mut enc, &self.canvas, None, &ui);
        }
        r.present(enc, &self.canvas);
    }

    // ---- save / load --------------------------------------------------------------

    /// Whether the game is at a point that can be saved (a message or a choice).
    pub fn can_save(&self) -> bool {
        matches!(self.wait, Wait::Click | Wait::Choice) && !self.ended
    }

    pub fn snapshot(&self) -> Option<crate::save::SaveData> {
        use crate::save::{LayerSave, SaveData, SeSave};
        if !self.can_save() {
            return None;
        }
        let (pc, page_texts, choice) = match self.wait {
            Wait::Click => {
                let mut t = self.page.texts.clone();
                t.pop();
                (self.msg_pc, t, None)
            }
            _ => {
                let c = self.choice.as_ref()?;
                (
                    self.pc,
                    Vec::new(),
                    Some((c.var.clone(), c.opts.iter().map(|o| (o.value, o.text.clone())).collect::<Vec<_>>())),
                )
            }
        };
        let layers = self
            .stage
            .layers
            .iter()
            .map(|l| {
                l.as_ref().filter(|l| !l.pending).map(|l| LayerSave {
                    image: match &l.source {
                        Source::Image(_) => Some(l.name.clone()),
                        Source::Color(_) => None,
                    },
                    color: match &l.source {
                        Source::Color(c) => Some(*c),
                        Source::Image(_) => None,
                    },
                    x: l.x.to,
                    y: l.y.to,
                    zoom_x: l.zoom_x.to,
                    zoom_y: l.zoom_y.to,
                    alpha: l.alpha.to,
                    rotation: l.rotation.to,
                    prio: l.prio,
                    parent: l.parent,
                    base: l.base,
                    absolute: l.absolute,
                    sway: l.sway.map(|w| [w.amp.0, w.amp.1, w.period.0, w.period.1, w.strength, w.left]),
                    rule: l.rule.as_ref().map(|(n, _)| *n),
                    noise: l.noise,
                    blend: l.blend as u8,
                    shade: l.shade as u8,
                    fill: l.fill,
                    motion: l.motion.as_ref().map(|m| (m.id, m.t)),
                    clip: l.clip.map(|c| [c.x, c.y, c.w, c.h, c.rot]),
                    bright: Some(l.bright),
                })
            })
            .collect();
        let preview = self
            .page
            .texts
            .last()
            .map(|t| {
                text::parse_markup(t)
                    .into_iter()
                    .filter_map(|r| match r {
                        text::Run::Text(t) => Some(t),
                        text::Run::Ruby { base, .. } => Some(base),
                        text::Run::Newline => None,
                    })
                    .collect::<String>()
            })
            .or_else(|| choice.as_ref().map(|(_, o)| o.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" / ")))
            .unwrap_or_default();
        Some(SaveData {
            version: 1,
            scene: self.scene_index,
            scene_name: self.assets.scene_name(self.scene_index).to_string(),
            pc,
            vars: self
                .vars
                .iter()
                .filter(|(k, _)| !k.starts_with('G'))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            layers,
            page_texts,
            choice,
            bgm: self.sound.bgm_state(),
            loops: self
                .sound
                .looping_effects()
                .into_iter()
                .map(|(channel, name, volume)| SeSave { channel, name, volume })
                .collect(),
            timestamp: crate::save::now(),
            preview,
            blur: self.post_blur,
        })
    }

    pub fn restore(&mut self, r: &Renderer, d: &crate::save::SaveData) {
        self.post_blur = d.blur;
        self.stop_movie();
        self.sound.stop_all();
        self.choice = None;
        self.sel_opts.clear();
        self.trans = None;
        self.shake = None;
        let noise = self.stage.noise.take();
        self.stage = Stage::new();
        self.stage.noise = noise;
        self.vars.retain(|k, _| k.starts_with('G'));
        for (k, v) in &d.vars {
            self.vars.insert(k.clone(), *v);
        }
        for (i, ls) in d.layers.iter().enumerate() {
            let Some(ls) = ls else { continue };
            let source = match (&ls.image, ls.color) {
                (Some(name), _) => match self.assets.image(r, name) {
                    Some(img) => Source::Image(img),
                    None => continue,
                },
                (None, Some(c)) => Source::Color(c),
                (None, None) => continue,
            };
            let mut l = Layer::new(ls.image.clone().unwrap_or_default(), source, ls.x, ls.y, ls.prio);
            l.zoom_x = Tween::fixed(ls.zoom_x);
            l.zoom_y = Tween::fixed(ls.zoom_y);
            l.alpha = Tween::fixed(ls.alpha);
            l.rotation = Tween::fixed(ls.rotation);
            l.parent = ls.parent;
            l.base = ls.base;
            l.absolute = ls.absolute;
            l.noise = ls.noise;
            if l.noise.is_some() {
                self.load_noise(r);
            }
            l.rule = ls
                .rule
                .and_then(|n| self.assets.image(r, &format!("RULE_{n:02}")).map(|i| (n, i.tex.clone())));
            l.sway = ls.sway.map(|[a0, a1, p0, p1, st, left]| {
                crate::stage::Sway::new((a0, a1), (p0, p1), st, left, i as u32 + 1)
            });
            l.blend = match ls.blend {
                1 => Blend::Add,
                2 => Blend::Sub,
                3 => Blend::Multiply,
                4 => Blend::Screen,
                5 => Blend::Opaque,
                _ => Blend::Alpha,
            };
            l.shade = match ls.shade {
                3 => Shade::Mono,
                4 => Shade::Nega,
                _ => Shade::Sprite,
            };
            l.fill = ls.fill;
            l.motion = ls.motion.map(|(id, t)| crate::motion::MotionState::new(id, t));
            l.clip = ls.clip.map(|[x, y, w, h, rot]| crate::stage::ClipRect { x, y, w, h, rot });
            l.bright = ls.bright.unwrap_or(1.0);
            l.pending = false;
            self.stage.set(i as i32, l);
        }
        if let Some((id, vol)) = d.bgm {
            self.play_bgm(id, vol, 0.5);
        }
        for se in &d.loops {
            if let Some(pcm) = self.assets.sound_effect(&se.name) {
                self.sound.play_se(se.channel, &se.name, pcm, se.volume, 0.5, true);
            }
        }
        // scene and page
        match self.assets.scene(d.scene) {
            Some(script) => {
                self.script = script;
                self.scene_index = d.scene;
            }
            None => {
                tracing::error!("save refers to missing scene {}", d.scene);
                self.ended = true;
                return;
            }
        }
        self.ended = false;
        self.clear_page();
        self.page.visible = false;
        self.page.alpha = 0.0;
        for t in &d.page_texts {
            self.show_message(t, false);
        }
        self.page.revealed = self.page.order as f32;
        self.pc = d.pc;
        self.wait = Wait::None;
        if let Some((var, opts)) = &d.choice {
            self.choice = Some(Choice {
                var: var.clone(),
                opts: opts.iter().map(|(v, t)| ChoiceOpt { value: *v, text: t.clone() }).collect(),
                hover: None,
            });
            self.wait = Wait::Choice;
        }
        self.snapshot = false;
    }

    /// Persistent (G) flags.
    pub fn global_vars(&self) -> Vec<(String, i32)> {
        self.vars.iter().filter(|(k, _)| k.starts_with('G')).map(|(k, v)| (k.clone(), *v)).collect()
    }

    pub fn reset(&mut self) {
        self.post_blur = false;
        self.stop_movie();
        self.sound.stop_all();
        self.stage = Stage::new();
        self.vars.retain(|k, _| k.starts_with('G'));
        self.clear_page();
        self.page.visible = false;
        self.page.alpha = 0.0;
        self.choice = None;
        self.sel_opts.clear();
        self.trans = None;
        self.shake = None;
        self.wait = Wait::Halt;
        self.ended = false;
    }

    /// Flowchart jump: restart at a node with the flags it was reached with.
    pub fn jump_node(&mut self, n: u16) -> bool {
        let Some(rec) = self.node_states.get(&n).cloned() else { return false };
        self.reset();
        for (k, v) in rec.vars {
            self.vars.insert(k, v);
        }
        self.start_scene(rec.scene, None);
        self.pc = rec.pc;
        self.wait = Wait::None;
        true
    }

    /// (phase, scene name, first line of text) describing a flowchart node.
    pub fn node_info(&mut self, n: u16) -> Option<(u32, String, String)> {
        let rec = self.node_states.get(&n)?;
        let name = self.assets.scene_name(rec.scene).to_string();
        let phase = name.split('_').next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let script = self.assets.scene(rec.scene)?;
        let preview = script.commands[rec.pc.min(script.commands.len())..]
            .iter()
            .find(|c| matches!(c.op, Op::Message))
            .map(|c| {
                let t = self.resolve_text(c.args.first().map(String::as_str).unwrap_or(""));
                text::parse_markup(&t)
                    .into_iter()
                    .filter_map(|r| match r {
                        text::Run::Text(t) => Some(t),
                        text::Run::Ruby { base, .. } => Some(base),
                        text::Run::Newline => None,
                    })
                    .collect::<String>()
                    .chars()
                    .take(28)
                    .collect()
            })
            .unwrap_or_default();
        Some((phase, name, preview))
    }

    pub fn dump_layers(&self) -> String {
        let mut s = String::new();
        for (i, l) in self.stage.layers.iter().enumerate() {
            if let Some(l) = l {
                s += &format!(
                    "  L{i:02} {:<24} prio {:5} xy ({:.0},{:.0}) z ({:.2},{:.2}) a {:.2} rot {:.1} {:?} {:?} fill {:?} motion {:?} pending {}{}\n",
                    l.name,
                    l.prio,
                    l.x.value(),
                    l.y.value(),
                    l.zoom_x.value(),
                    l.zoom_y.value(),
                    l.alpha.value(),
                    l.rotation.value(),
                    l.blend,
                    l.shade,
                    l.fill,
                    l.motion.as_ref().map(|m| (m.id, m.t as i32)),
                    l.pending,
                    l.sway.map_or(String::new(), |w| format!(" sway ({:.1},{:.1})", w.offset.0, w.offset.1))
                        + &l.noise.map_or(String::new(), |n| format!(" noise {n}{}", if self.stage.noise.is_some() { "" } else { " (no tex)" }))
                );
            }
        }
        s
    }

    /// RGBA8 copy of the last presented canvas.
    pub fn screenshot(&self, r: &Renderer) -> Vec<u8> {
        r.read_target(&self.canvas)
    }
}

pub fn get_bit(bits: &[u64], i: usize) -> bool {
    bits.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 != 0)
}

/// Set bit `i`; returns whether it was already set.
pub fn set_bit(bits: &mut Vec<u64>, i: usize) -> bool {
    let (w, b) = (i / 64, i % 64);
    if bits.len() <= w {
        bits.resize(w + 1, 0);
    }
    let was = bits[w] >> b & 1 != 0;
    bits[w] |= 1 << b;
    was
}
