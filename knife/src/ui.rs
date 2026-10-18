//! System screens: title, in-game menu, save/load, config, backlog and confirmation dialogs.

use std::collections::HashMap;

use knife_render::{CANVAS_H, CANVAS_W, Quad, Renderer, TexRef};

use crate::{
    game::Game,
    save::{self, GlobalData, SaveData},
    text::FontAtlas,
};

const W: f32 = CANVAS_W as f32;
const H: f32 = CANVAS_H as f32;
const SLOTS_PER_PAGE: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Confirm {
    Title,
    Exit,
    Overwrite(usize),
    Load(usize),
    Jump(u16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Screen {
    Title,
    Game,
    Menu,
    SaveLoad { save: bool, page: usize },
    Config,
    Backlog { scroll: usize },
    Flowchart { page: usize },
    /// gallery hub: CG / music / movie / ending
    Extra,
    Gallery { page: usize },
    Music { page: usize },
    Movies,
    /// a gallery movie is playing
    MoviePlay,
    Endings,
    /// viewing the `pos`-th unlocked CG of a gallery group
    CgView { group: usize, pos: usize },
    Confirm(Confirm),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    NewGame,
    Continue,
    OpenLoad,
    OpenSave,
    OpenConfig,
    OpenFlow,
    Node(u16),
    OpenExtra,
    OpenGallery,
    Cg(usize),
    NextCg,
    OpenMusic,
    Track(usize),
    OpenMovies,
    Movie(usize),
    OpenEndings,
    Ending(usize),
    Exit,
    ToTitle,
    CloseMenu,
    Slot(usize),
    PrevPage,
    NextPage,
    Back,
    Setting(usize, i32),
    Yes,
    No,
}

pub struct Ui {
    pub screen: Screen,
    /// screen to return to from a sub screen
    stack: Vec<Screen>,
    hover: Option<usize>,
    mouse: (f32, f32),
    tex: HashMap<String, TexRef>,
    pub global: GlobalData,
    slots: Vec<Option<SaveData>>,
    thumbs: HashMap<usize, Option<TexRef>>,
    pub quit: bool,
    toast: Option<(String, f32)>,
    /// screenshot of the game taken when the menu opened (used for save thumbnails)
    pending_thumb: Option<Vec<u8>>,
    title_movie: bool,
    /// unlocked entries per CG gallery group, refreshed when the gallery opens
    cg_unlocked: Vec<Vec<usize>>,
    music_ok: Vec<bool>,
    playing_track: Option<usize>,
    /// SYSMES_TEXT strings (music titles), loaded on first use
    sysmes: Vec<String>,
}

fn rect_hit(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.0 < r[0] + r[2] && p.1 >= r[1] && p.1 < r[1] + r[3]
}

const SETTINGS: &[&str] = &[
    "Text speed",
    "Auto delay",
    "Master volume",
    "BGM volume",
    "SE volume",
    "Voice volume",
    "Skip",
];
const SETTING_ROW: f32 = 95.0;
const FLOW_PER_PAGE: usize = 10;
const CG_COLS: usize = 6;
const CG_PER_PAGE: usize = 24;
const MUSIC_PER_PAGE: usize = 16;

impl Ui {
    pub fn new(game: &mut Game, r: &Renderer) -> Self {
        let mut tex = HashMap::new();
        for stem in ["TITLE_PARTS_JA", "MENU_PARTS_JA"] {
            match game.assets.ui_textures(r, stem) {
                Ok(list) => tex.extend(list),
                Err(e) => tracing::warn!("ui textures {stem}: {e:#}"),
            }
        }
        let global = save::read_global();
        let mut ui = Self {
            screen: Screen::Title,
            stack: Vec::new(),
            hover: None,
            mouse: (0.0, 0.0),
            tex,
            global,
            slots: Vec::new(),
            thumbs: HashMap::new(),
            quit: false,
            toast: None,
            pending_thumb: None,
            title_movie: false,
            cg_unlocked: Vec::new(),
            music_ok: Vec::new(),
            playing_track: None,
            sysmes: Vec::new(),
        };
        ui.apply_global(game);
        ui.refresh_slots();
        ui
    }

    fn apply_global(&self, game: &mut Game) {
        // merge: in-memory state may be newer than the last stored global data
        for (k, v) in &self.global.vars {
            game.vars.entry(k.clone()).or_insert(*v);
        }
        let g = &self.global;
        if let Some(v) = g.text_speed {
            game.settings.text_speed = v;
        }
        if let Some(v) = g.auto_delay_ms {
            game.settings.auto_delay_ms = v;
        }
        if let Some(v) = g.skip_unread {
            game.settings.skip_unread = v;
        }
        let or = |a: &mut Vec<u64>, b: &[u64]| {
            if a.len() < b.len() {
                a.resize(b.len(), 0);
            }
            for (x, y) in a.iter_mut().zip(b) {
                *x |= y;
            }
        };
        or(&mut game.read, &g.read);
        or(&mut game.nodes, &g.nodes);
        or(&mut game.endings, &g.endings);
        for (n, rec) in &g.node_states {
            game.node_states.entry(*n).or_insert_with(|| rec.clone());
        }
        game.seen.extend(g.seen.iter().cloned());
        or(&mut game.heard, &g.heard);
        game.movie_flags |= g.movie_flags;
        let vol = &mut game.sound.vol;
        vol.master = g.master.unwrap_or(vol.master);
        vol.bgm = g.bgm.unwrap_or(vol.bgm);
        vol.se = g.se.unwrap_or(vol.se);
        vol.voice = g.voice.unwrap_or(vol.voice);
        game.sound.apply_volumes();
    }

    pub fn store_global(&mut self, game: &Game) {
        self.global.vars = game.global_vars();
        self.global.text_speed = Some(game.settings.text_speed);
        self.global.auto_delay_ms = Some(game.settings.auto_delay_ms);
        self.global.skip_unread = Some(game.settings.skip_unread);
        self.global.read = game.read.clone();
        self.global.nodes = game.nodes.clone();
        self.global.endings = game.endings.clone();
        self.global.node_states = game.node_states.clone();
        let mut seen: Vec<String> = game.seen.iter().cloned().collect();
        seen.sort();
        self.global.seen = seen;
        self.global.heard = game.heard.clone();
        self.global.movie_flags = game.movie_flags;
        let v = &game.sound.vol;
        self.global.master = Some(v.master);
        self.global.bgm = Some(v.bgm);
        self.global.se = Some(v.se);
        self.global.voice = Some(v.voice);
        if let Err(e) = save::write_global(&self.global) {
            tracing::warn!("saving global data: {e:#}");
        }
    }

    fn refresh_gallery(&mut self, game: &Game) {
        use crate::game::get_bit;
        self.cg_unlocked = crate::gallery::unlocked(&game.seen, |n| get_bit(&game.nodes, n), |n| get_bit(&game.endings, n));
    }

    fn refresh_slots(&mut self) {
        self.slots = (0..save::SLOTS).map(save::read_slot).collect();
        self.thumbs.clear();
    }

    fn toast(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), 2000.0));
    }

    /// Debug: open a screen by name on top of the current one.
    pub fn debug_open(&mut self, game: &mut Game, r: &Renderer, name: &str) {
        let gallery = ["music", "movies", "endings", "gallery"].contains(&name)
            || ["cg:", "track:", "movie:", "ending:"].iter().any(|p| name.starts_with(p));
        if gallery {
            self.act(game, r, Action::OpenExtra);
        }
        let open = match name {
            "extra" => Some(Action::OpenExtra),
            "music" => Some(Action::OpenMusic),
            "movies" => Some(Action::OpenMovies),
            "endings" => Some(Action::OpenEndings),
            _ => None,
        };
        if let Some(a) = open {
            self.act(game, r, a);
            return;
        }
        if let Some(n) = name.strip_prefix("track:").and_then(|n| n.parse().ok()) {
            self.act(game, r, Action::OpenMusic);
            self.act(game, r, Action::Track(n));
            return;
        }
        if let Some(n) = name.strip_prefix("ending:").and_then(|n| n.parse().ok()) {
            self.act(game, r, Action::OpenEndings);
            self.act(game, r, Action::Ending(n));
            return;
        }
        if let Some(n) = name.strip_prefix("movie:").and_then(|n| n.parse().ok()) {
            self.act(game, r, Action::OpenMovies);
            self.act(game, r, Action::Movie(n));
            return;
        }
        if name == "gallery" || name.starts_with("cg:") {
            self.act(game, r, Action::OpenGallery);
            if let Some(g) = name.strip_prefix("cg:").and_then(|g| g.parse().ok()) {
                self.act(game, r, Action::Cg(g));
            }
            return;
        }
        let s = match name {
            "menu" => Screen::Menu,
            "config" => Screen::Config,
            "save" => Screen::SaveLoad { save: true, page: 0 },
            "load" => Screen::SaveLoad { save: false, page: 0 },
            "backlog" => Screen::Backlog { scroll: 0 },
            "flowchart" => Screen::Flowchart { page: 0 },
            _ => {
                tracing::warn!("unknown screen {name}");
                return;
            }
        };
        self.push(s);
    }

    fn push(&mut self, s: Screen) {
        self.stack.push(self.screen);
        self.screen = s;
        self.hover = None;
    }

    fn pop(&mut self) {
        self.screen = self.stack.pop().unwrap_or(Screen::Game);
        self.hover = None;
    }

    pub fn in_game(&self) -> bool {
        self.screen == Screen::Game
    }

    // ---- layout ------------------------------------------------------------------

    fn items(&self, game: &Game) -> Vec<([f32; 4], Action)> {
        match self.screen {
            Screen::Title => {
                let labels = [
                    Action::NewGame,
                    Action::Continue,
                    Action::OpenLoad,
                    Action::OpenFlow,
                    Action::OpenExtra,
                    Action::OpenConfig,
                    Action::Exit,
                ];
                labels
                    .iter()
                    .enumerate()
                    .map(|(i, a)| ([1280.0, 440.0 + i as f32 * 84.0, 420.0, 70.0], *a))
                    .collect()
            }
            Screen::Menu => {
                let acts = [
                    Action::OpenSave,
                    Action::OpenLoad,
                    Action::OpenConfig,
                    Action::OpenFlow,
                    Action::ToTitle,
                ];
                acts.iter()
                    .enumerate()
                    .map(|(i, a)| ([W - 290.0, 240.0 + i as f32 * 120.0, 240.0, 104.0], *a))
                    .chain(std::iter::once(([0.0, 0.0, W - 312.0, H], Action::CloseMenu)))
                    .collect()
            }
            Screen::SaveLoad { page, .. } => {
                let mut v: Vec<([f32; 4], Action)> = (0..SLOTS_PER_PAGE)
                    .map(|i| {
                        let (col, row) = ((i % 2) as f32, (i / 2) as f32);
                        (
                            [140.0 + col * 840.0, 190.0 + row * 270.0, 800.0, 240.0],
                            Action::Slot(page * SLOTS_PER_PAGE + i),
                        )
                    })
                    .collect();
                v.push(([140.0, 1000.0, 200.0, 60.0], Action::PrevPage));
                v.push(([1580.0, 1000.0, 200.0, 60.0], Action::NextPage));
                v.push(([860.0, 1000.0, 200.0, 60.0], Action::Back));
                v
            }
            Screen::Config => {
                let mut v = Vec::new();
                for i in 0..SETTINGS.len() {
                    let y = 220.0 + i as f32 * SETTING_ROW;
                    v.push(([1000.0, y, 90.0, 80.0], Action::Setting(i, -1)));
                    v.push(([1500.0, y, 90.0, 80.0], Action::Setting(i, 1)));
                }
                v.push(([860.0, 940.0, 200.0, 70.0], Action::Back));
                v
            }
            Screen::Flowchart { page } => {
                let nodes = Self::flow_nodes(game);
                let mut v: Vec<([f32; 4], Action)> = nodes
                    .iter()
                    .skip(page * FLOW_PER_PAGE)
                    .take(FLOW_PER_PAGE)
                    .enumerate()
                    .map(|(i, n)| ([200.0, 190.0 + i as f32 * 76.0, 1520.0, 68.0], Action::Node(*n)))
                    .collect();
                v.push(([140.0, 1000.0, 200.0, 60.0], Action::PrevPage));
                v.push(([1580.0, 1000.0, 200.0, 60.0], Action::NextPage));
                v.push(([860.0, 1000.0, 200.0, 60.0], Action::Back));
                v
            }
            Screen::Gallery { page } => {
                let n = crate::gallery::group_count();
                let mut v: Vec<([f32; 4], Action)> = (page * CG_PER_PAGE..((page + 1) * CG_PER_PAGE).min(n))
                    .map(|g| {
                        let i = g % CG_PER_PAGE;
                        let (col, row) = ((i % CG_COLS) as f32, (i / CG_COLS) as f32);
                        ([104.0 + col * 290.0, 180.0 + row * 196.0, 262.0, 150.0], Action::Cg(g))
                    })
                    .collect();
                v.push(([140.0, 1000.0, 200.0, 60.0], Action::PrevPage));
                v.push(([1580.0, 1000.0, 200.0, 60.0], Action::NextPage));
                v.push(([860.0, 1000.0, 200.0, 60.0], Action::Back));
                v
            }
            Screen::CgView { .. } => vec![([0.0, 0.0, W, H], Action::NextCg)],
            // gallerymenu_btn rows: CG Gallery, Music Player, Flowchart, Movie, Ending List, Title
            Screen::Extra => [
                Action::OpenGallery,
                Action::OpenMusic,
                Action::OpenFlow,
                Action::OpenMovies,
                Action::OpenEndings,
                Action::Back,
            ]
            .iter()
            .enumerate()
            .map(|(i, a)| ([W - 296.0, 220.0 + i as f32 * 120.0, 266.0, 105.0], *a))
            .chain(std::iter::once(([0.0, 0.0, W - 312.0, H], Action::Back)))
            .collect(),
            Screen::Music { page } => {
                let n = crate::gallery_table::MUSIC.len();
                let mut v: Vec<([f32; 4], Action)> = (page * MUSIC_PER_PAGE..((page + 1) * MUSIC_PER_PAGE).min(n))
                    .map(|i| {
                        let k = i % MUSIC_PER_PAGE;
                        let (col, row) = ((k / 8) as f32, (k % 8) as f32);
                        ([140.0 + col * 840.0, 190.0 + row * 96.0, 800.0, 84.0], Action::Track(i))
                    })
                    .collect();
                v.push(([140.0, 1000.0, 200.0, 60.0], Action::PrevPage));
                v.push(([1580.0, 1000.0, 200.0, 60.0], Action::NextPage));
                v.push(([860.0, 1000.0, 200.0, 60.0], Action::Back));
                v
            }
            Screen::Movies => {
                let mut v: Vec<([f32; 4], Action)> = (0..crate::gallery::MOVIES.len())
                    .map(|i| ([560.0, 300.0 + i as f32 * 150.0, 800.0, 120.0], Action::Movie(i)))
                    .collect();
                v.push(([860.0, 1000.0, 200.0, 60.0], Action::Back));
                v
            }
            Screen::MoviePlay => vec![([0.0, 0.0, W, H], Action::Back)],
            Screen::Endings => {
                // cGalleryEnding layout (sub_DAF8 / sub_DDA4): three route icons, then BAD END plates
                let mut v: Vec<([f32; 4], Action)> = (0..crate::gallery::ENDINGS.len())
                    .map(|i| {
                        let rc = if i < 3 {
                            [i as f32 * 480.0 + 416.0, 120.0, 432.0, 544.0]
                        } else {
                            let b = i - 3;
                            [(b & 7) as f32 * 180.0 + 384.0, (b >> 3) as f32 * 96.0 + 680.0, 180.0, 88.0]
                        };
                        (rc, Action::Ending(i))
                    })
                    .collect();
                v.push(([860.0, 1000.0, 200.0, 60.0], Action::Back));
                v
            }
            Screen::Confirm(_) => vec![
                ([620.0, 600.0, 300.0, 90.0], Action::Yes),
                ([1000.0, 600.0, 300.0, 90.0], Action::No),
            ],
            Screen::Backlog { .. } => vec![([0.0, 0.0, W, H], Action::Back)],
            Screen::Game => {
                let _ = game;
                Vec::new()
            }
        }
    }

    /// Reached flowchart nodes in story order (scene, then position in the scene).
    fn flow_nodes(game: &Game) -> Vec<u16> {
        // natural order of scene names (01_00_ARC01_2 < 01_00_ARC01_10), then position in the scene
        let key = |name: &str| -> Vec<(u32, String)> {
            name.split('_').map(|p| (p.parse().unwrap_or(u32::MAX), p.to_string())).collect()
        };
        let mut v: Vec<(Vec<(u32, String)>, usize, u16)> = game
            .node_states
            .iter()
            .map(|(n, r)| (key(game.assets.scene_name(r.scene)), r.pc, *n))
            .collect();
        v.sort();
        v.into_iter().map(|(_, _, n)| n).collect()
    }

    fn setting_value(game: &Game, i: usize) -> f32 {
        match i {
            0 => game.settings.text_speed,
            1 => game.settings.auto_delay_ms,
            2 => game.sound.vol.master,
            3 => game.sound.vol.bgm,
            4 => game.sound.vol.se,
            5 => game.sound.vol.voice,
            _ => game.settings.skip_unread as u8 as f32,
        }
    }

    fn setting_label(game: &Game, i: usize) -> String {
        let v = Self::setting_value(game, i);
        match i {
            0 => {
                if v <= 0.0 {
                    "instant".into()
                } else {
                    format!("{v:.0} chars/s")
                }
            }
            1 => format!("{:.1} s", v / 1000.0),
            6 => if v > 0.0 { "all text" } else { "read text only" }.into(),
            _ => format!("{:.0}%", v * 100.0),
        }
    }

    fn change_setting(game: &mut Game, i: usize, dir: i32) {
        let d = dir as f32;
        match i {
            0 => {
                let v = &mut game.settings.text_speed;
                *v = if *v <= 0.0 && dir < 0 {
                    0.0
                } else if *v <= 0.0 {
                    200.0
                } else {
                    let n = *v + d * 20.0;
                    if n > 200.0 { 0.0 } else { n.max(20.0) }
                };
            }
            1 => game.settings.auto_delay_ms = (game.settings.auto_delay_ms + d * 250.0).clamp(250.0, 5000.0),
            6 => game.settings.skip_unread = !game.settings.skip_unread,
            _ => {
                let vol = &mut game.sound.vol;
                let v = match i {
                    2 => &mut vol.master,
                    3 => &mut vol.bgm,
                    4 => &mut vol.se,
                    _ => &mut vol.voice,
                };
                *v = (*v + d * 0.1).clamp(0.0, 1.0);
                game.sound.apply_volumes();
            }
        }
    }

    // ---- input -------------------------------------------------------------------

    pub fn mouse_move(&mut self, game: &mut Game, x: f32, y: f32) {
        self.mouse = (x, y);
        if self.screen == Screen::Game {
            game.mouse_move(x, y);
            return;
        }
        self.hover = self.items(game).iter().position(|(r, _)| rect_hit(*r, (x, y)));
    }

    pub fn click(&mut self, game: &mut Game, r: &Renderer) {
        if self.screen == Screen::Game {
            game.advance();
            return;
        }
        let items = self.items(game);
        if let Some((_, a)) = items.iter().find(|(rc, _)| rect_hit(*rc, self.mouse)) {
            self.act(game, r, *a);
        }
    }

    /// Right click / escape.
    pub fn cancel(&mut self, game: &mut Game, r: &Renderer) {
        match self.screen {
            Screen::Game => {
                if game.can_save() || game.in_choice() {
                    self.pending_thumb = Some(game.screenshot(r));
                }
                self.push(Screen::Menu);
            }
            Screen::Title => self.push(Screen::Confirm(Confirm::Exit)),
            _ => self.act(game, r, Action::Back),
        }
    }

    pub fn key_nav(&mut self, game: &mut Game, r: &Renderer, dy: i32, dx: i32, enter: bool) {
        if self.screen == Screen::Game {
            if dy != 0 {
                game.choice_move(dy);
            }
            if enter {
                game.advance();
            }
            return;
        }
        if let Screen::Backlog { scroll } = &mut self.screen {
            if dy < 0 {
                *scroll += 1;
            } else if dy > 0 {
                if *scroll == 0 {
                    self.pop();
                } else {
                    *scroll -= 1;
                }
            }
            return;
        }
        if self.screen == Screen::Config && dx != 0 {
            if let Some(h) = self.hover
                && let Some((_, Action::Setting(i, _))) = self.items(game).get(h)
            {
                Self::change_setting(game, *i, dx);
            }
            return;
        }
        if matches!(self.screen, Screen::Gallery { .. } | Screen::Music { .. })
            && dx != 0
        {
            let a = if dx < 0 { Action::PrevPage } else { Action::NextPage };
            self.act(game, r, a);
            return;
        }
        if let Screen::SaveLoad { save, page } = self.screen
            && dx != 0
        {
            let pages = save::SLOTS / SLOTS_PER_PAGE;
            let page = ((page as i32 + dx).rem_euclid(pages as i32)) as usize;
            self.screen = Screen::SaveLoad { save, page };
            return;
        }
        let n = self.items(game).len();
        if n == 0 {
            return;
        }
        if dy != 0 {
            let cur = self.hover.map_or(if dy > 0 { -1 } else { 0 }, |h| h as i32);
            self.hover = Some((cur + dy).rem_euclid(n as i32) as usize);
        }
        if enter && let Some(h) = self.hover {
            let a = self.items(game)[h].1;
            self.act(game, r, a);
        }
    }

    /// Mouse wheel: up opens the backlog in game.
    pub fn wheel(&mut self, game: &mut Game, up: bool) {
        match &mut self.screen {
            Screen::Game => {
                if up && !game.backlog.is_empty() {
                    self.push(Screen::Backlog { scroll: 0 });
                } else if !up && !game.in_choice() {
                    game.advance();
                }
            }
            Screen::Backlog { scroll } => {
                if up {
                    *scroll = (*scroll + 1).min(game.backlog.len().saturating_sub(1));
                } else if *scroll == 0 {
                    self.pop();
                } else {
                    *scroll -= 1;
                }
            }
            _ => {}
        }
    }

    fn act(&mut self, game: &mut Game, r: &Renderer, a: Action) {
        match a {
            Action::NewGame => {
                game.reset();
                self.apply_global(game);
                game.start_scene(0, None);
                self.screen = Screen::Game;
                self.stack.clear();
            }
            Action::Continue => {
                let latest = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| s.as_ref().map(|s| (s.timestamp, i)))
                    .max();
                match latest {
                    Some((_, i)) => self.load(game, r, i),
                    None => self.toast("No save data"),
                }
            }
            Action::OpenLoad => {
                self.refresh_slots();
                self.push(Screen::SaveLoad { save: false, page: 0 });
            }
            Action::OpenSave => {
                if game.can_save() || game.in_choice() {
                    self.refresh_slots();
                    self.push(Screen::SaveLoad { save: true, page: 0 });
                } else {
                    self.toast("Cannot save here");
                }
            }
            Action::OpenConfig => self.push(Screen::Config),
            Action::OpenFlow => {
                if Self::flow_nodes(game).is_empty() {
                    self.toast("No scenes reached yet");
                } else {
                    self.push(Screen::Flowchart { page: 0 });
                }
            }
            Action::Node(n) => self.push(Screen::Confirm(Confirm::Jump(n))),
            Action::OpenExtra => {
                if !self.tex.contains_key("gallerymenu_btn") {
                    match game.assets.ui_textures(r, "GALLERY_PARTS_JA") {
                        Ok(list) => self.tex.extend(list),
                        Err(e) => tracing::warn!("gallery parts: {e:#}"),
                    }
                }
                self.push(Screen::Extra);
            }
            Action::OpenMusic => {
                use crate::game::get_bit;
                if self.sysmes.is_empty() {
                    self.sysmes = game.assets.sysmes();
                }
                self.music_ok = crate::gallery::music_unlocked(
                    |n| n > 0 && get_bit(&game.heard, n as usize),
                    |n| get_bit(&game.nodes, n),
                    game.movie_flags,
                );
                self.push(Screen::Music { page: 0 });
            }
            Action::Track(i) => {
                if self.music_ok.get(i).copied().unwrap_or(false) {
                    if self.playing_track == Some(i) {
                        game.play_bgm(0, 1.0, 0.5);
                        self.playing_track = None;
                    } else {
                        game.play_bgm(crate::gallery_table::MUSIC[i].0, 1.0, 0.3);
                        self.playing_track = Some(i);
                    }
                }
            }
            Action::OpenMovies => self.push(Screen::Movies),
            Action::Movie(i) => {
                let (movie, flag) = crate::gallery::MOVIES[i];
                if flag.is_none_or(|f| game.movie_flags & (1 << f) != 0) {
                    game.play_bgm(0, 1.0, 0.3);
                    self.playing_track = None;
                    game.gallery_movie = true;
                    game.play_movie(r, crate::movie::MOVIES[movie]);
                    self.title_movie = false;
                    self.push(Screen::MoviePlay);
                }
            }
            Action::OpenEndings => self.push(Screen::Endings),
            Action::Ending(i) => {
                use crate::game::get_bit;
                let (bit, id) = crate::gallery::ENDINGS[i];
                if get_bit(&game.nodes, bit)
                    && let Some(scene) = game.assets.scene_by_id(id)
                {
                    self.store_global(game);
                    game.reset();
                    self.apply_global(game);
                    game.start_scene(scene, None);
                    self.screen = Screen::Game;
                    self.stack.clear();
                    self.title_movie = false;
                }
            }
            Action::OpenGallery => {
                if !self.tex.contains_key("cg_thumb_all") {
                    match game.assets.ui_textures(r, "CG_THUMB_PARTS_JA") {
                        Ok(list) => self.tex.extend(list),
                        Err(e) => tracing::warn!("gallery thumbnails: {e:#}"),
                    }
                }
                self.refresh_gallery(game);
                self.push(Screen::Gallery { page: 0 });
            }
            Action::Cg(g) => {
                if self.cg_unlocked.get(g).is_some_and(|e| !e.is_empty()) {
                    self.push(Screen::CgView { group: g, pos: 0 });
                }
            }
            Action::NextCg => {
                if let Screen::CgView { group, pos } = self.screen {
                    let n = self.cg_unlocked.get(group).map_or(0, Vec::len);
                    if pos + 1 < n {
                        self.screen = Screen::CgView { group, pos: pos + 1 };
                    } else {
                        self.pop();
                    }
                }
            }
            Action::Exit => self.push(Screen::Confirm(Confirm::Exit)),
            Action::ToTitle => self.push(Screen::Confirm(Confirm::Title)),
            Action::CloseMenu | Action::Back => {
                match self.screen {
                    Screen::Music { .. } if self.playing_track.take().is_some() => game.play_bgm(0, 1.0, 0.5),
                    Screen::MoviePlay => {
                        game.stop_movie();
                        game.gallery_movie = false;
                    }
                    _ => {}
                }
                self.pop();
            }
            Action::Slot(i) => {
                let Screen::SaveLoad { save, .. } = self.screen else { return };
                if save {
                    if self.slots.get(i).is_some_and(|s| s.is_some()) {
                        self.push(Screen::Confirm(Confirm::Overwrite(i)));
                    } else {
                        self.save(game, r, i);
                    }
                } else if self.slots.get(i).is_some_and(|s| s.is_some()) {
                    if self.stack.contains(&Screen::Game) {
                        self.push(Screen::Confirm(Confirm::Load(i)));
                    } else {
                        self.load(game, r, i);
                    }
                }
            }
            Action::PrevPage | Action::NextPage => {
                let d = if a == Action::PrevPage { -1 } else { 1 };
                if let Screen::Music { page } = self.screen {
                    let pages = crate::gallery_table::MUSIC.len().div_ceil(MUSIC_PER_PAGE);
                    let page = ((page as i32 + d).rem_euclid(pages as i32)) as usize;
                    self.screen = Screen::Music { page };
                }
                if let Screen::Gallery { page } = self.screen {
                    let pages = crate::gallery::group_count().div_ceil(CG_PER_PAGE);
                    let page = ((page as i32 + d).rem_euclid(pages as i32)) as usize;
                    self.screen = Screen::Gallery { page };
                }
                if let Screen::Flowchart { page } = self.screen {
                    let pages = Self::flow_nodes(game).len().div_ceil(FLOW_PER_PAGE).max(1);
                    let page = ((page as i32 + d).rem_euclid(pages as i32)) as usize;
                    self.screen = Screen::Flowchart { page };
                }
                if let Screen::SaveLoad { save, page } = self.screen {
                    let pages = save::SLOTS / SLOTS_PER_PAGE;
                    let d = if a == Action::PrevPage { -1 } else { 1 };
                    let page = ((page as i32 + d).rem_euclid(pages as i32)) as usize;
                    self.screen = Screen::SaveLoad { save, page };
                }
            }
            Action::Setting(i, d) => {
                Self::change_setting(game, i, d);
                self.store_global(game);
            }
            Action::Yes => {
                let Screen::Confirm(c) = self.screen else { return };
                self.pop();
                match c {
                    Confirm::Exit => {
                        self.store_global(game);
                        self.quit = true;
                    }
                    Confirm::Title => self.to_title(game),
                    Confirm::Overwrite(i) => self.save(game, r, i),
                    Confirm::Load(i) => self.load(game, r, i),
                    Confirm::Jump(n) => {
                        self.store_global(game);
                        if game.jump_node(n) {
                            self.screen = Screen::Game;
                            self.stack.clear();
                            self.title_movie = false;
                        }
                    }
                }
            }
            Action::No => self.pop(),
        }
    }

    fn save(&mut self, game: &mut Game, _r: &Renderer, slot: usize) {
        let Some(data) = game.snapshot() else {
            self.toast("Cannot save here");
            return;
        };
        let thumb = self.pending_thumb.as_ref().map(|rgba| {
            let img = image::RgbaImage::from_raw(CANVAS_W, CANVAS_H, rgba.clone()).expect("screenshot size");
            image::imageops::resize(&img, 320, 180, image::imageops::FilterType::Triangle).into_raw()
        });
        match save::write_slot(slot, &data, thumb.as_deref().map(|t| (t, 320, 180))) {
            Ok(()) => self.toast(format!("Saved to slot {slot}")),
            Err(e) => self.toast(format!("Save failed: {e}")),
        }
        self.store_global(game);
        self.refresh_slots();
    }

    pub fn debug_save(&mut self, game: &mut Game, r: &Renderer, slot: usize) -> bool {
        if !game.can_save() {
            return false;
        }
        self.pending_thumb = Some(game.screenshot(r));
        self.save(game, r, slot);
        tracing::info!("debug save to {slot}: {:?}", self.toast);
        true
    }

    pub fn debug_load(&mut self, game: &mut Game, r: &Renderer, slot: usize) {
        self.load(game, r, slot);
    }

    pub fn quick_save(&mut self, game: &mut Game, r: &Renderer) {
        if self.screen != Screen::Game {
            return;
        }
        self.pending_thumb = Some(game.screenshot(r));
        self.save(game, r, save::QUICK_SLOT);
    }

    pub fn quick_load(&mut self, game: &mut Game, r: &Renderer) {
        if matches!(self.screen, Screen::Game | Screen::Title) && self.slots.first().is_some_and(|s| s.is_some()) {
            self.load(game, r, save::QUICK_SLOT);
        }
    }

    fn load(&mut self, game: &mut Game, r: &Renderer, slot: usize) {
        let Some(data) = save::read_slot(slot) else {
            self.toast("Empty slot");
            return;
        };
        game.reset();
        self.apply_global(game);
        game.restore(r, &data);
        self.screen = Screen::Game;
        self.stack.clear();
        self.title_movie = false;
    }

    fn to_title(&mut self, game: &mut Game) {
        self.store_global(game);
        game.reset();
        self.screen = Screen::Title;
        self.stack.clear();
        self.title_movie = false;
    }

    /// Per-frame bookkeeping.
    pub fn update(&mut self, game: &mut Game, r: &Renderer, dt: f32) {
        if let Some((_, t)) = &mut self.toast {
            *t -= dt;
            if *t <= 0.0 {
                self.toast = None;
            }
        }
        if self.screen == Screen::Title && !self.title_movie {
            self.title_movie = true;
            game.play_title_movie(r);
        }
        if game.autosave_request && game.in_choice() {
            game.autosave_request = false;
            if let Some(data) = game.snapshot() {
                let rgba = game.screenshot(r);
                let img = image::RgbaImage::from_raw(CANVAS_W, CANVAS_H, rgba).expect("screenshot size");
                let thumb = image::imageops::resize(&img, 320, 180, image::imageops::FilterType::Triangle).into_raw();
                if let Err(e) = save::write_slot(save::QUICK_SLOT, &data, Some((&thumb, 320, 180))) {
                    tracing::warn!("autosave: {e:#}");
                }
                self.slots.get_mut(save::QUICK_SLOT).map(|s| *s = Some(data));
                self.thumbs.remove(&save::QUICK_SLOT);
            }
        }
        if self.screen == Screen::MoviePlay && !game.movie_active() {
            game.gallery_movie = false;
            self.pop();
        }
        if self.screen == Screen::Game && game.ended {
            self.to_title(game);
        }
    }

    pub fn game_paused(&self) -> bool {
        !matches!(self.screen, Screen::Game)
    }

    // ---- drawing -----------------------------------------------------------------

    /// Draw a UI texture by name; `src` is a pixel rectangle inside it (whole texture if None).
    fn sprite(&self, out: &mut Vec<Quad>, name: &str, dst: [f32; 4], src: Option<[f32; 4]>) -> bool {
        let Some(t) = self.tex.get(name) else { return false };
        let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
        let [x, y, w, h] = src.unwrap_or([0.0, 0.0, t.width as f32, t.height as f32]);
        out.push(Quad::rect(t.clone(), dst, [1.0; 4]).with_uv_rect(x / tw, y / th, (x + w) / tw, (y + h) / th));
        true
    }

    /// Gallery sub screen backdrop: GALLERY_PARTS background and the mode's title banner.
    fn gallery_panel(&self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, banner: &str, fallback: &str) {
        out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.02, 0.08, 1.0]));
        self.sprite(out, "config_innerbg", [0.0, 0.0, W, H], None);
        if !self.sprite(out, banner, [100.0, 40.0, 720.0, 80.0], None) {
            Self::label(game.font_mut(), r, out, fallback, 64.0, [0.0, 50.0, W, 90.0], [0.7, 0.9, 1.0, 1.0]);
        }
    }

    fn thumb(&mut self, r: &Renderer, slot: usize) -> Option<TexRef> {
        self.thumbs
            .entry(slot)
            .or_insert_with(|| {
                let img = image::open(save::thumb_path(slot)).ok()?.to_rgba8();
                Some(r.upload(img.width(), img.height(), knife_render::Upload::Rgba8(img.as_raw()), "thumb"))
            })
            .clone()
    }

    fn label(font: &mut FontAtlas, r: &Renderer, out: &mut Vec<Quad>, s: &str, size: f32, rect: [f32; 4], color: [f32; 4]) {
        let w = font.measure(s, size);
        let x = rect[0] + (rect[2] - w) / 2.0;
        let y = rect[1] + rect[3] / 2.0 + size * 0.36;
        font.draw_str(r, out, s, size, x, y, color);
    }

    pub fn draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let white = r.white.clone();
        let items = self.items(game);
        let hover = self.hover;
        let screen = self.screen;
        let hovered = |i: usize| hover == Some(i);
        match screen {
            Screen::Game => {}
            Screen::Title => {
                if !game.movie_playing() {
                    if let Some(t) = self.tex.get("title_bg1") {
                        out.push(Quad::rect(t.clone(), [0.0, 0.0, W, H], [1.0; 4]));
                    }
                }
                if let Some(t) = self.tex.get("title_titlelog") {
                    let uv = t.uv_max();
                    out.push(Quad::rect(t.clone(), [260.0, 200.0, 560.0, 648.0], [1.0; 4]).with_uv_rect(0.0, 0.0, uv[0], uv[1]));
                }
                let labels = ["New Game", "Continue", "Load", "Flowchart", "Extra", "Config", "Exit"];
                let font = game.font_mut();
                for (i, (rc, _)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0, 1.0, 1.0, 0.9] };
                    Self::label(font, r, out, labels[i], 46.0, *rc, c);
                }
            }
            Screen::Menu => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 0.35]));
                if let Some(t) = self.tex.get("menu_window") {
                    let uv = t.uv_max();
                    out.push(Quad::rect(t.clone(), [W - 312.0, 0.0, 312.0, H], [1.0; 4]).with_uv_rect(0.0, 0.0, uv[0], uv[1]));
                }
                let btn = self.tex.get("menu_btn_ja").cloned();
                for (i, (rc, a)) in items.iter().enumerate() {
                    if *a == Action::CloseMenu {
                        continue;
                    }
                    // atlas rows: Save, Load, Config, FlowChart, Title; columns: normal / hover / press
                    let row = match a {
                        Action::OpenSave => 0,
                        Action::OpenLoad => 1,
                        Action::OpenConfig => 2,
                        Action::OpenFlow => 3,
                        _ => 4,
                    };
                    if let Some(t) = &btn {
                        let col = if hovered(i) { 1.0 } else { 0.0 };
                        let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
                        let (u0, v0) = ((col * 240.0 + 20.0) / tw, (row as f32 * 104.0 + 10.0) / th);
                        let (u1, v1) = ((col * 240.0 + 260.0) / tw, (row as f32 * 104.0 + 114.0) / th);
                        out.push(Quad::rect(t.clone(), *rc, [1.0; 4]).with_uv_rect(u0, v0, u1, v1));
                    }
                }
            }
            Screen::SaveLoad { save, page } => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.02, 0.03, 0.08, 0.92]));
                let title = if save { "Save" } else { "Load" };
                Self::label(game.font_mut(), r, out, title, 64.0, [0.0, 60.0, W, 90.0], [0.7, 0.9, 1.0, 1.0]);
                for (i, (rc, a)) in items.iter().enumerate() {
                    match a {
                        Action::Slot(slot) => {
                            let bg = if hovered(i) { [0.25, 0.35, 0.55, 0.9] } else { [0.1, 0.12, 0.2, 0.9] };
                            out.push(Quad::rect(white.clone(), *rc, bg));
                            let thumb_rect = [rc[0] + 20.0, rc[1] + 30.0, 320.0, 180.0];
                            let info = self.slots.get(*slot).cloned().flatten();
                            match &info {
                                Some(d) => {
                                    if let Some(t) = self.thumb(r, *slot) {
                                        out.push(Quad::rect(t, thumb_rect, [1.0; 4]));
                                    } else {
                                        out.push(Quad::rect(white.clone(), thumb_rect, [0.0, 0.0, 0.0, 1.0]));
                                    }
                                    let font = game.font_mut();
                                    let x = rc[0] + 370.0;
                                    let label = if *slot == save::QUICK_SLOT { "Auto".to_string() } else { format!("No.{slot:03}") };
                                    font.draw_str(r, out, &format!("{label}  {}", save::format_time(d.timestamp)), 30.0, x, rc[1] + 70.0, [0.8, 0.9, 1.0, 1.0]);
                                    // preview: wrap to two lines
                                    let chars: Vec<char> = d.preview.chars().collect();
                                    for (li, chunk) in chars.chunks(16).take(3).enumerate() {
                                        let s: String = chunk.iter().collect();
                                        font.draw_str(r, out, &s, 26.0, x, rc[1] + 120.0 + li as f32 * 40.0, [1.0; 4]);
                                    }
                                }
                                None => {
                                    let label = if *slot == save::QUICK_SLOT { "Auto".to_string() } else { format!("No.{slot:03}") };
                                    Self::label(game.font_mut(), r, out, &format!("{label}  ---"), 30.0, *rc, [0.6, 0.6, 0.7, 1.0]);
                                }
                            }
                        }
                        _ => {
                            let s = match a {
                                Action::PrevPage => "<",
                                Action::NextPage => ">",
                                _ => "Back",
                            };
                            let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                            Self::label(game.font_mut(), r, out, s, 42.0, *rc, c);
                        }
                    }
                }
                let pages = save::SLOTS / SLOTS_PER_PAGE;
                Self::label(game.font_mut(), r, out, &format!("{}/{}", page + 1, pages), 34.0, [400.0, 1000.0, 360.0, 60.0], [0.8, 0.8, 0.9, 1.0]);
            }
            Screen::Config => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.02, 0.03, 0.08, 0.92]));
                Self::label(game.font_mut(), r, out, "Config", 64.0, [0.0, 60.0, W, 90.0], [0.7, 0.9, 1.0, 1.0]);
                for (i, name) in SETTINGS.iter().enumerate() {
                    let y = 220.0 + i as f32 * SETTING_ROW;
                    let value = Self::setting_label(game, i);
                    let font = game.font_mut();
                    font.draw_str(r, out, name, 40.0, 360.0, y + 55.0, [1.0; 4]);
                    Self::label(font, r, out, &value, 40.0, [1090.0, y, 410.0, 80.0], [0.8, 0.95, 1.0, 1.0]);
                }
                for (i, (rc, a)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                    let s = match a {
                        Action::Setting(_, d) if *d < 0 => "<",
                        Action::Setting(..) => ">",
                        _ => "Back",
                    };
                    Self::label(game.font_mut(), r, out, s, 44.0, *rc, c);
                }
            }
            Screen::Flowchart { page } => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.02, 0.03, 0.08, 0.94]));
                Self::label(game.font_mut(), r, out, "FlowChart", 64.0, [0.0, 60.0, W, 90.0], [0.7, 0.9, 1.0, 1.0]);
                let mut last_phase = None;
                for (i, (rc, a)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                    match a {
                        Action::Node(n) => {
                            let bg = if hovered(i) { [0.2, 0.3, 0.5, 0.9] } else { [0.1, 0.12, 0.2, 0.8] };
                            out.push(Quad::rect(white.clone(), *rc, bg));
                            // node picture (sub_13FD4): cell of flow_thumb_all_nx, or flow_sam_non
                            let thumb = [rc[0] + 170.0, rc[1] + 2.0, 104.0, 64.0];
                            if hovered(i) {
                                self.sprite(out, "flow_cur", [thumb[0] - 6.0, thumb[1] - 8.0, 116.0, 80.0], Some([116.0, 0.0, 116.0, 80.0]));
                            }
                            let cell = crate::gallery_table::FLOW_THUMBS
                                .binary_search_by_key(n, |&(k, _)| k)
                                .ok()
                                .map(|j| crate::gallery_table::FLOW_THUMBS[j].1 as f32);
                            let drawn = cell.is_some_and(|t| {
                                let src = [(t % 20.0).floor() * 104.0 + 1.0, (t / 20.0).floor() * 64.0 + 1.0, 102.0, 62.0];
                                self.sprite(out, "flow_thumb_all_nx", thumb, Some(src))
                            });
                            if !drawn {
                                self.sprite(out, "flow_sam_non", thumb, None);
                            }
                            let (phase, name, preview) = game.node_info(*n).unwrap_or_default();
                            let font = game.font_mut();
                            if last_phase != Some(phase) {
                                font.draw_str(r, out, &format!("Phase {phase}"), 30.0, rc[0] + 20.0, rc[1] + 46.0, [0.6, 0.85, 1.0, 1.0]);
                                last_phase = Some(phase);
                            }
                            font.draw_str(r, out, &name, 30.0, rc[0] + 300.0, rc[1] + 46.0, c);
                            font.draw_str(r, out, &preview, 30.0, rc[0] + 720.0, rc[1] + 46.0, [0.85, 0.85, 0.9, 1.0]);
                        }
                        Action::PrevPage => Self::label(game.font_mut(), r, out, "<", 44.0, *rc, c),
                        Action::NextPage => Self::label(game.font_mut(), r, out, ">", 44.0, *rc, c),
                        _ => Self::label(game.font_mut(), r, out, "Back", 44.0, *rc, c),
                    }
                }
                let pages = Self::flow_nodes(game).len().div_ceil(FLOW_PER_PAGE).max(1);
                Self::label(game.font_mut(), r, out, &format!("{}/{}", page + 1, pages), 34.0, [400.0, 1000.0, 360.0, 60.0], [0.8, 0.8, 0.9, 1.0]);
            }
            Screen::Gallery { page } => {
                self.gallery_panel(game, r, out, "cg_title", "CG Gallery");
                let atlas = self.tex.get("cg_thumb_all").cloned();
                for (i, (rc, a)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                    match a {
                        Action::Cg(g) => {
                            let unlocked = self.cg_unlocked.get(*g).and_then(|e| e.first()).copied();
                            let frame = [rc[0] - 4.0, rc[1] - 4.0, rc[2] + 8.0, rc[3] + 8.0];
                            if hovered(i) {
                                self.sprite(out, "cg_cursor", [rc[0] - 10.0, rc[1] - 10.0, rc[2] + 20.0, rc[3] + 20.0], Some([260.0, 0.0, 260.0, 160.0]));
                            } else {
                                out.push(Quad::rect(white.clone(), frame, [0.25, 0.3, 0.45, 1.0]));
                            }
                            match (unlocked.and_then(|e| crate::gallery::entry(*g, e)), &atlas) {
                                (Some(e), Some(t)) => {
                                    use crate::gallery::{THUMB_COLS, THUMB_H, THUMB_W};
                                    let (col, row) = ((e.thumb as u32 % THUMB_COLS) as f32, (e.thumb as u32 / THUMB_COLS) as f32);
                                    let (tw, th) = (t.texture.size().width as f32, t.texture.size().height as f32);
                                    let (u0, v0) = (col * THUMB_W / tw, row * THUMB_H / th);
                                    let (u1, v1) = ((col + 1.0) * THUMB_W / tw, (row + 1.0) * THUMB_H / th);
                                    out.push(Quad::rect(t.clone(), *rc, [1.0; 4]).with_uv_rect(u0, v0, u1, v1));
                                    let n = self.cg_unlocked[*g].len();
                                    if n > 1 {
                                        let font = game.font_mut();
                                        font.draw_str(r, out, &format!("{n}"), 26.0, rc[0] + rc[2] - 40.0, rc[1] + rc[3] - 10.0, [1.0; 4]);
                                    }
                                }
                                _ => {
                                    out.push(Quad::rect(white.clone(), *rc, [0.06, 0.07, 0.12, 1.0]));
                                    Self::label(game.font_mut(), r, out, &format!("{:03}", g + 1), 30.0, *rc, [0.35, 0.38, 0.5, 1.0]);
                                }
                            }
                        }
                        Action::PrevPage => Self::label(game.font_mut(), r, out, "<", 44.0, *rc, c),
                        Action::NextPage => Self::label(game.font_mut(), r, out, ">", 44.0, *rc, c),
                        _ => Self::label(game.font_mut(), r, out, "Back", 44.0, *rc, c),
                    }
                }
                let pages = crate::gallery::group_count().div_ceil(CG_PER_PAGE);
                let got = self.cg_unlocked.iter().filter(|e| !e.is_empty()).count();
                let info = format!("{}/{}   {got}/{}", page + 1, pages, crate::gallery::group_count());
                Self::label(game.font_mut(), r, out, &info, 34.0, [380.0, 1000.0, 420.0, 60.0], [0.8, 0.8, 0.9, 1.0]);
            }
            Screen::Extra => {
                if !game.movie_playing() {
                    self.sprite(out, "title_bg1", [0.0, 0.0, W, H], None);
                }
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 0.35]));
                self.sprite(out, "gallerymenu_window", [W - 312.0, 0.0, 312.0, H], None);
                for (i, (rc, a)) in items.iter().enumerate() {
                    let row = match a {
                        Action::OpenGallery => 0,
                        Action::OpenMusic => 1,
                        Action::OpenFlow => 2,
                        Action::OpenMovies => 3,
                        Action::OpenEndings => 4,
                        _ if rc[0] == 0.0 => continue,
                        _ => 5,
                    };
                    let col = if hovered(i) { 1.0 } else { 0.0 };
                    let src = [col * 800.0 / 3.0, row as f32 * 632.0 / 6.0, 800.0 / 3.0, 632.0 / 6.0];
                    self.sprite(out, "gallerymenu_btn", *rc, Some(src));
                }
            }
            Screen::Music { page } => {
                self.gallery_panel(game, r, out, "music_title", "Music");
                for (i, (rc, a)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                    match a {
                        Action::Track(t) => {
                            let playing = self.playing_track == Some(*t);
                            let bg = if playing {
                                [0.25, 0.4, 0.6, 0.95]
                            } else if hovered(i) {
                                [0.2, 0.3, 0.5, 0.9]
                            } else {
                                [0.1, 0.12, 0.2, 0.8]
                            };
                            out.push(Quad::rect(white.clone(), *rc, bg));
                            let (_, title, ms, _) = crate::gallery_table::MUSIC[*t];
                            let ok = self.music_ok.get(*t).copied().unwrap_or(false);
                            let name = if ok { self.sysmes.get(title as usize).cloned().unwrap_or_default() } else { "？？？".into() };
                            let font = game.font_mut();
                            let tc = if ok { c } else { [0.45, 0.48, 0.6, 1.0] };
                            font.draw_str(r, out, &format!("{:02}", t + 1), 32.0, rc[0] + 24.0, rc[1] + 54.0, [0.6, 0.85, 1.0, 1.0]);
                            font.draw_str(r, out, &name, 36.0, rc[0] + 100.0, rc[1] + 56.0, tc);
                            if ok {
                                let secs = ms / 1000;
                                font.draw_str(r, out, &format!("{}:{:02}", secs / 60, secs % 60), 30.0, rc[0] + rc[2] - 110.0, rc[1] + 54.0, [0.8, 0.8, 0.9, 1.0]);
                            }
                        }
                        Action::PrevPage => Self::label(game.font_mut(), r, out, "<", 44.0, *rc, c),
                        Action::NextPage => Self::label(game.font_mut(), r, out, ">", 44.0, *rc, c),
                        _ => Self::label(game.font_mut(), r, out, "Back", 44.0, *rc, c),
                    }
                }
                let pages = crate::gallery_table::MUSIC.len().div_ceil(MUSIC_PER_PAGE);
                let got = self.music_ok.iter().filter(|&&b| b).count();
                let info = format!("{}/{}   {got}/{}", page + 1, pages, crate::gallery_table::MUSIC.len());
                Self::label(game.font_mut(), r, out, &info, 34.0, [380.0, 1000.0, 420.0, 60.0], [0.8, 0.8, 0.9, 1.0]);
            }
            Screen::Movies => {
                self.gallery_panel(game, r, out, "movie_title", "Movie");
                for (i, (rc, a)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                    match a {
                        Action::Movie(m) => {
                            let bg = if hovered(i) { [0.2, 0.3, 0.5, 0.9] } else { [0.1, 0.12, 0.2, 0.8] };
                            out.push(Quad::rect(white.clone(), *rc, bg));
                            let (movie, flag) = crate::gallery::MOVIES[*m];
                            let ok = flag.is_none_or(|f| game.movie_flags & (1 << f) != 0);
                            let name = if ok {
                                match crate::movie::MOVIES[movie] {
                                    "prologue" => "Prologue",
                                    "op1_arc" => "Opening 1",
                                    "op2_ciel" => "Opening 2",
                                    other => other,
                                }
                            } else {
                                "？？？"
                            };
                            let tc = if ok { c } else { [0.45, 0.48, 0.6, 1.0] };
                            Self::label(game.font_mut(), r, out, name, 46.0, *rc, tc);
                        }
                        _ => Self::label(game.font_mut(), r, out, "Back", 44.0, *rc, c),
                    }
                }
            }
            Screen::MoviePlay => {}
            Screen::Endings => {
                use crate::game::get_bit;
                self.gallery_panel(game, r, out, "ending_title", "Ending");
                for (i, (rc, a)) in items.iter().enumerate() {
                    let c = if hovered(i) { [0.55, 0.95, 1.0, 1.0] } else { [1.0; 4] };
                    match a {
                        Action::Ending(e) => {
                            let ok = get_bit(&game.nodes, crate::gallery::ENDINGS[*e].0);
                            if *e < 3 {
                                let icon = if ok { ["ending_icon_Arcueid", "ending_icon_Ciel_1", "ending_icon_Ciel_2"][*e] } else { "ending_icon_non" };
                                if hovered(i) && ok {
                                    out.push(Quad::rect(white.clone(), [rc[0] - 6.0, rc[1] - 6.0, rc[2] + 12.0, rc[3] + 12.0], [0.4, 0.9, 1.0, 0.8]));
                                }
                                self.sprite(out, icon, *rc, None);
                            } else {
                                // ending_badend: plates at (b&7)*180+8, row*96+296 (reached) or +8 (black)
                                let b = *e - 3;
                                let y0 = if ok { 296.0 } else { 8.0 };
                                let src = [(b & 7) as f32 * 180.0 + 8.0, (b >> 3) as f32 * 96.0 + y0, 180.0, 88.0];
                                if hovered(i) && ok {
                                    out.push(Quad::rect(white.clone(), [rc[0] + 74.0, rc[1] - 4.0, 116.0, 76.0], [0.4, 0.9, 1.0, 0.8]));
                                }
                                self.sprite(out, "ending_badend", *rc, Some(src));
                            }
                        }
                        _ => Self::label(game.font_mut(), r, out, "Back", 44.0, *rc, c),
                    }
                }
            }
            Screen::CgView { group, pos } => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
                let e = self.cg_unlocked.get(group).and_then(|v| v.get(pos)).and_then(|&i| crate::gallery::entry(group, i));
                if let Some(e) = e
                    && let Some(img) = game.assets.image(r, e.name)
                {
                    // fit the whole picture on screen (scrolling CGs are larger than 1920x1080)
                    let (w, h) = (e.width.max(1) as f32, e.height.max(1) as f32);
                    let s = (W / w).min(H / h);
                    let (dw, dh) = (w * s, h * s);
                    let uv = img.tex.uv_max();
                    out.push(Quad::rect(img.tex.clone(), [(W - dw) / 2.0, (H - dh) / 2.0, dw, dh], [1.0; 4]).with_uv_rect(0.0, 0.0, uv[0], uv[1]));
                }
            }
            Screen::Backlog { scroll } => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.05, 0.85]));
                let lines: Vec<String> = game.backlog.iter().rev().skip(scroll).take(12).cloned().collect();
                let font = game.font_mut();
                let mut y = 1000.0;
                for l in lines.iter() {
                    let text = l.replace('\n', " ");
                    let chars: Vec<char> = text.chars().collect();
                    let rows: Vec<String> = chars.chunks(36).map(|c| c.iter().collect()).collect();
                    for row in rows.iter().rev() {
                        if y < 60.0 {
                            break;
                        }
                        font.draw_str(r, out, row, 36.0, 200.0, y, [1.0, 1.0, 1.0, 0.95]);
                        y -= 52.0;
                    }
                    y -= 24.0;
                }
            }
            Screen::Confirm(c) => {
                out.push(Quad::rect(white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 0.6]));
                out.push(Quad::rect(white.clone(), [460.0, 340.0, 1000.0, 400.0], [0.08, 0.1, 0.18, 0.95]));
                let msg = match c {
                    Confirm::Title => "Return to the title screen?".to_string(),
                    Confirm::Exit => "Quit the game?".to_string(),
                    Confirm::Overwrite(i) => format!("Overwrite slot {i}?"),
                    Confirm::Load(i) => format!("Load slot {i}?"),
                    Confirm::Jump(n) => format!("Jump to node {n}? Unsaved progress is lost."),
                };
                Self::label(game.font_mut(), r, out, &msg, 44.0, [460.0, 400.0, 1000.0, 120.0], [1.0; 4]);
                for (i, (rc, a)) in items.iter().enumerate() {
                    let bg = if hovered(i) { [0.3, 0.45, 0.7, 0.95] } else { [0.15, 0.18, 0.3, 0.95] };
                    out.push(Quad::rect(white.clone(), *rc, bg));
                    let s = if *a == Action::Yes { "Yes" } else { "No" };
                    Self::label(game.font_mut(), r, out, s, 42.0, *rc, [1.0; 4]);
                }
            }
        }
        // key guide (the exe's cGalleryGuide / system guide bar)
        let guide = match screen {
            Screen::Title | Screen::Game | Screen::MoviePlay | Screen::CgView { .. } => None,
            Screen::Gallery { .. } | Screen::Music { .. } | Screen::SaveLoad { .. } | Screen::Flowchart { .. } => {
                Some("Enter / Click: Select    Esc / Right click: Back    Left / Right: Page")
            }
            Screen::Config => Some("Left / Right: Change    Esc / Right click: Back"),
            Screen::Backlog { .. } => Some("Wheel / Up / Down: Scroll    Esc / Right click: Back"),
            _ => Some("Enter / Click: Select    Esc / Right click: Back"),
        };
        if let Some(g) = guide {
            let font = game.font_mut();
            let w = font.measure(g, 24.0);
            font.draw_str(r, out, g, 24.0, W - w - 40.0, 36.0, [0.75, 0.82, 0.95, 0.85]);
        }
        if let Some((t, left)) = &self.toast {
            let a = (left / 300.0).min(1.0);
            let t = t.clone();
            out.push(Quad::rect(white.clone(), [0.0, 980.0, W, 80.0], [0.0, 0.0, 0.0, 0.6 * a]));
            Self::label(game.font_mut(), r, out, &t, 36.0, [0.0, 980.0, W, 80.0], [1.0, 1.0, 1.0, a]);
        }
    }
}
