//! knife: a reimplementation of the Tsukihime (2021) engine.
//!
//! This is the whole program as a library: the `knife` executable calls [`run`], and on Android the
//! `knife-android` library, which NativeActivity loads, calls [`run_android`].

mod assets;
mod flow;
mod flow_table;
mod gallery;
mod grade;
mod ini;
mod gallery_table;
mod fx;
mod game;
mod gamepad;
mod motion;
mod motion_loops;
mod save;
mod save_tables;
mod savedata;
mod movie;
mod platform;
mod sound;
mod stage;
mod text;
mod text_stream;
mod ui;
mod voice_ids;
mod witch;

#[cfg(target_os = "android")]
pub use winit::platform::android::activity::AndroidApp;

use std::{path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use clap::Parser;
use knife_render::{CANVAS_H, CANVAS_W, Renderer};
use ui::Pad;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

// Options marked "Debug" (and --scene, --label, --auto) exist only with `--features debug`.
#[derive(Parser, Clone)]
struct Cli {
    /// The game data: the extracted romfs directory, with the Switch's shared system font
    /// (`nintendo_udsg-r_std_003.bfttf`, system title 0100000000000811) added.
    ///
    /// Defaults to "assets" next to the executable (inside a macOS app bundle, also
    /// Contents/Resources/assets).
    #[arg(short, long)]
    assets_dir: Option<PathBuf>,
    /// Files to use in place of the game data's: a file here replaces the one at the same path in the assets
    /// folder (an archive's .hed, .nam or .mrg, a movie, a stream, a font, ...); a font folder with files of its
    /// own replaces the game data's. Used only if the folder exists.
    ///
    /// Defaults to "mod_assets" next to the executable (inside a macOS app bundle, also
    /// Contents/Resources/mod_assets).
    #[arg(short, long)]
    mod_assets_dir: Option<PathBuf>,
    /// Play in this language: ja, en, zc (Simplified Chinese) or zt (Traditional Chinese). It stands in for the
    /// console language and replaces the config's language setting, as the overseas config's language
    /// buttons do. The Japanese data has Japanese only.
    ///
    /// Defaults to the config's setting; a new save takes the system language.
    #[arg(short, long, value_parser = parse_language)]
    language: Option<i16>,
    /// Read and write saves in this file (the Switch's savedata000 format).
    ///
    /// Defaults to "knife/savedata000" ("knife/overseas/savedata000" for the overseas data,
    /// "knife/witch/savedata000" for Witch on the Holy Night) in the OS application data directory (e.g.
    /// ~/Library/Application Support on macOS, %APPDATA% on Windows).
    #[arg(short, long)]
    save_file: Option<PathBuf>,
    /// Serve displayed text over WebSocket at HOST:PORT (e.g. 127.0.0.1:6677)
    ///
    /// Every connected client receives each message as plain text, and each choice's options one per line.
    #[arg(short, long, value_name = "HOST:PORT")]
    text_websocket: Option<String>,
    /// Render at this resolution: "window" draws at the window's resolution (the largest 16:9 area that fits,
    /// at least 1920x1080), "original" at the Switch's 1920x1080, scaled to the window, for output that matches
    /// the Switch pixel for pixel.
    ///
    /// Defaults to window (original with --shots).
    #[arg(short, long, value_enum)]
    resolution: Option<Resolution>,
    /// Start in this scene (allscr name or index) instead of START_SCRIPT
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    scene: Option<String>,
    /// Label to start at within --scene
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    label: Option<String>,
    /// Debug: save a screenshot to this directory every --shot-every ms
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    shots: Option<PathBuf>,
    #[cfg_attr(feature = "debug", arg(long, default_value_t = 2000))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    shot_every: u32,
    /// Debug: take the first screenshot at this many ms
    #[cfg_attr(feature = "debug", arg(long, default_value_t = 0))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    shot_from: u32,
    /// Debug: exit after this many ms
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    exit_after: Option<u32>,
    /// Start in auto-advance mode
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    auto: bool,
    /// Debug: simulate a held skip key
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    skip: bool,
    /// Debug: save to --save-slot after this many ms
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    save_at: Option<u32>,
    /// Debug: skip (as --skip) to this command index of the start scene, then save to --save-slot at the
    /// first message there and exit
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    save_pc: Option<usize>,
    /// Debug: the slot --save-at and --save-pc write (0-based; 99 is past the listed slots)
    #[cfg_attr(feature = "debug", arg(long, default_value_t = 99))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    save_slot: usize,
    /// Debug: load this slot at startup
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    load_slot: Option<usize>,
    /// Debug: stay on the title screen even with --shots
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    title: bool,
    /// Debug: answer every choice at once (story choices vary with the pc and KNIFE_SEED; the save / hint
    /// questions answer No)
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    auto_choice: bool,
    /// Debug: behave as if the skip key is held (stops at unread text, unlike --skip)
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    hold_skip: bool,
    /// Debug: open a UI screen at startup (menu, config, save, load, backlog, flowchart, extra, gallery, cg:GROUP, music, track:N, movies, movie:N, endings, ending:N)
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    screen: Option<String>,
    /// Debug: open --screen only after this many ms (e.g. to show a backlog after --auto)
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    screen_after: Option<u32>,
    /// Debug: jump to a reached flowchart node at startup
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    jump_node: Option<u16>,
    /// Skip the TYPE-MOON logo movie and the notice shown before the title
    #[arg(long)]
    skip_intro: bool,
    /// Let every story movie be skipped (B, Plus, A or a tap), even unwatched ones the Switch plays through
    #[arg(long)]
    skip_movies: bool,
    /// Debug: set script variables after the start, comma-separated NAME=VALUE (e.g. F822=0)
    #[cfg_attr(feature = "debug", arg(long, value_delimiter = ','))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    var: Vec<String>,
    /// Debug: run the launch sequence even with debug options (e.g. --shots)
    #[cfg_attr(feature = "debug", arg(long))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    boot: bool,
    /// Debug: scripted input, comma-separated MS:KEY: a controller button (up, down, left, right, a, b, x, y, l, r,
    /// zl, zr, minus, plus, r3, lup, ldown, lleft, lright (left stick), rup, rdown (right stick): tapped, or held
    /// from NAME+ to NAME-), menu, click,
    /// move/X/Y in canvas pixels, press / release of the left button, wheel/N notches of the mouse wheel (+ up),
    /// trackpad/PX a trackpad scroll of PX canvas pixels (+ up)
    #[cfg_attr(feature = "debug", arg(long, value_delimiter = ','))]
    #[cfg_attr(not(feature = "debug"), arg(skip))]
    keys: Vec<String>,
}

impl Cli {
    /// The selected assets directory, or the default next to the executable.
    fn assets_dir(&self) -> PathBuf {
        self.assets_dir.clone().unwrap_or_else(|| default_dir(ASSETS_DIR_NAME))
    }

    /// The selected mod folder, or the default next to the executable, when it exists.
    fn mod_assets_dir(&self) -> Option<PathBuf> {
        Some(self.mod_assets_dir.clone().unwrap_or_else(|| default_dir(MOD_ASSETS_DIR_NAME))).filter(|dir| dir.is_dir())
    }

    /// `--resolution`, by default the window's, or original for --shots (the comparisons with the Switch's captures)
    /// and on Android (a phone's screen gains nothing from a larger canvas, and it costs heat and battery).
    fn resolution(&self) -> Resolution {
        let original = self.shots.is_some() || cfg!(target_os = "android");
        self.resolution.unwrap_or(if original { Resolution::Original } else { Resolution::Window })
    }
}

const ASSETS_DIR_NAME: &str = "assets";
const MOD_ASSETS_DIR_NAME: &str = "mod_assets";

/// Folders searched for the default assets directory: the executable's own folder (on Android, the app's files
/// folder, see `platform::executable`), then, inside a macOS app bundle (`Name.app/Contents/MacOS/<executable>`),
/// `Contents/Resources`.
fn executable_dirs(executable: &std::path::Path) -> Vec<PathBuf> {
    let Some(directory) = executable.parent() else {
        return Vec::new();
    };
    let mut dirs = vec![directory.to_path_buf()];
    if directory.ends_with("Contents/MacOS")
        && let Some(contents) = directory.parent()
        && contents.parent().and_then(std::path::Path::extension).is_some_and(|e| e.eq_ignore_ascii_case("app"))
    {
        dirs.push(contents.join("Resources"));
    }
    dirs
}

/// The first existing folder `name` among [`executable_dirs`], else the one next to the executable.
fn default_dir(name: &str) -> PathBuf {
    let Some(executable) = platform::executable() else {
        return PathBuf::from(name);
    };
    let candidates: Vec<_> = executable_dirs(&executable).into_iter().map(|dir| dir.join(name)).collect();
    candidates.iter().find(|path| path.is_dir()).or(candidates.first()).cloned().unwrap_or_else(|| PathBuf::from(name))
}

struct App {
    cli: Cli,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    game: Option<game::Game>,
    ui: Option<ui::Ui>,
    last: Instant,
    cursor: (f64, f64),
    elapsed: f32,
    next_shot: f32,
    shot_n: u32,
    gamepads: gamepad::Gamepads,
    /// W / A / S / D held: the left stick at full tilt
    wasd: [bool; 4],
    /// K / L held: the right stick up / down at full tilt
    kl: [bool; 2],
    /// --keys given: the real pointer is ignored, so a scripted run does not follow the mouse over its window
    scripted: bool,
    /// The game language the window title was last set for (`Assets::title`)
    title_language: Option<usize>,
    /// The window is fully covered or minimised (`WindowEvent::Occluded`): no redraws come, so `about_to_wait`
    /// runs the frames on a 60 Hz timer and nothing is presented
    occluded: bool,
    /// When the next frame runs while `occluded`
    hidden_tick: Instant,
    /// The finger that stands for the left mouse button (the first one down; Android's touch screen)
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    finger: Option<u64>,
}

/// A 1920x1080 window, shrunk to 90% of the monitor (or its work area) at 16:9 when it does not fit, centred.
#[cfg_attr(target_os = "android", allow(dead_code))]
fn centered_window(
    area: winit::dpi::PhysicalSize<u32>,
    origin: winit::dpi::PhysicalPosition<i32>,
    scale: f64,
) -> (winit::dpi::PhysicalSize<u32>, winit::dpi::PhysicalPosition<i32>) {
    // room for the title bar, menu bar and dock or taskbar
    const MAX_FRACTION: f64 = 0.9;
    let (aw, ah) = (f64::from(area.width), f64::from(area.height));
    let fit = (aw * MAX_FRACTION / (1920.0 * scale)).min(ah * MAX_FRACTION / (1080.0 * scale)).min(1.0);
    let (w, h) = ((1920.0 * scale * fit).round(), (1080.0 * scale * fit).round());
    (
        winit::dpi::PhysicalSize::new(w as u32, h as u32),
        winit::dpi::PhysicalPosition::new(origin.x + ((aw - w) / 2.0) as i32, origin.y + ((ah - h) / 2.0) as i32),
    )
}

/// The monitor's work area (without the taskbar or docked app bars), in physical pixels.
#[cfg(windows)]
fn work_area(
    monitor: &winit::monitor::MonitorHandle,
) -> Option<(winit::dpi::PhysicalSize<u32>, winit::dpi::PhysicalPosition<i32>)> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    use winit::platform::windows::MonitorHandleExtWindows;
    // SAFETY: MONITORINFO is plain data; all zeroes is a valid value.
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: winit's handle is a live HMONITOR and cbSize matches `info`.
    if unsafe { GetMonitorInfoW(monitor.hmonitor(), &mut info) } == 0 {
        return None;
    }
    let work = info.rcWork;
    Some((
        winit::dpi::PhysicalSize::new((work.right - work.left).max(0) as u32, (work.bottom - work.top).max(0) as u32),
        winit::dpi::PhysicalPosition::new(work.left, work.top),
    ))
}

impl App {
    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        #[cfg_attr(target_os = "android", allow(unused_mut))]
        let mut attrs = Window::default_attributes()
            .with_title(assets::TITLE)
            .with_inner_size(winit::dpi::LogicalSize::new(1920.0, 1080.0));
        #[cfg(windows)]
        let mut centering_area = None;
        // (an Android window is the whole screen)
        #[cfg(not(target_os = "android"))]
        if let Some(monitor) = el.primary_monitor().or_else(|| el.available_monitors().next()) {
            #[cfg(windows)]
            let (area_size, area_position) = work_area(&monitor).unwrap_or_else(|| (monitor.size(), monitor.position()));
            #[cfg(not(windows))]
            let (area_size, area_position) = (monitor.size(), monitor.position());
            let (size, position) = centered_window(area_size, area_position, monitor.scale_factor());
            attrs = attrs.with_inner_size(size).with_position(position);
            #[cfg(windows)]
            {
                // shown once the frame is centred below
                attrs = attrs.with_visible(false);
                centering_area = Some((area_size, area_position));
            }
        }
        let window = Arc::new(el.create_window(attrs)?);
        // the position places the frame's corner: centre the whole frame (title bar, borders) once its size is
        // known
        #[cfg(windows)]
        if let Some((area_size, area_position)) = centering_area {
            let outer = window.outer_size();
            let offset = |area: u32, outer: u32| ((area as i32 - outer as i32) / 2).max(0);
            window.set_outer_position(winit::dpi::PhysicalPosition::new(
                area_position.x + offset(area_size.width, outer.width),
                area_position.y + offset(area_size.height, outer.height),
            ));
            window.set_visible(true);
        }
        // a launch from a terminal does not always activate the app
        #[cfg(target_os = "macos")]
        window.focus_window();
        let size = window.inner_size();
        let mut renderer = Renderer::new(window.clone(), size.width.max(1), size.height.max(1))?;
        renderer.set_follow_window(self.cli.resolution() == Resolution::Window);
        let roots = assets::Roots { base: self.cli.assets_dir(), mods: self.cli.mod_assets_dir() };
        if let Some(mods) = &roots.mods {
            tracing::info!("Using mod assets in {}", mods.display());
        } else if let Some(dir) = &self.cli.mod_assets_dir {
            tracing::warn!("mod assets folder {} not found", dir.display());
        }
        let nso = assets::exe_tables(assets::detect_build(&roots.base))?;
        let language = savedata::SaveFile::load().language();
        let assets = assets::Assets::open(&roots, &nso, language)?;
        match assets.soft_lut_rows() {
            Ok(rows) => renderer.set_soft_lut(&rows),
            Err(e) => tracing::warn!("clip inset mask: {e:#}"),
        }
        let motions = motion::MotionTable::load(&nso)?;
        // sub_AB0F0: the story font is the first file of the language folder, the bubble font of `<folder>3`
        let (font, bubble_font) = game::Game::load_fonts(&assets, &renderer)?;
        let mut game = game::Game::new(assets, motions, font, &renderer);
        // the QA bubble text font (Game::msg_font)
        if let Some(f) = bubble_font {
            game.set_bubble_font(f);
        }
        let start = match &self.cli.scene {
            Some(s) => s
                .parse::<usize>()
                .ok()
                .or_else(|| game.assets.scenes.find(s))
                .with_context(|| format!("unknown scene {s}"))?,
            None => 0,
        };
        let mut ui = ui::Ui::new(&mut game, &renderer);
        let debug = self.cli.scene.is_some()
            || self.cli.shots.is_some()
            || self.cli.skip
            || self.cli.title
            || self.cli.screen.is_some()
            || self.cli.load_slot.is_some()
            || self.cli.jump_node.is_some();
        if !self.cli.title && (self.cli.scene.is_some() || self.cli.shots.is_some() || self.cli.skip) && !self.cli.boot {
            game.start_scene(start, self.cli.label.as_deref());
            if start != 0 {
                // START_SCRIPT's lasting state: pages keep their earlier messages (sub_391A0); saves without
                // it show each message in its own window on the Switch
                game.set_var_pub("F822", 1);
            }
            for v in &self.cli.var {
                match v.split_once('=').and_then(|(k, n)| Some((k, n.parse().ok()?))) {
                    Some((k, n)) => game.set_var_pub(k, n),
                    None => tracing::warn!("--var {v}: expected NAME=VALUE"),
                }
            }
            ui.screen = ui::Screen::Game;
        } else if !debug || self.cli.boot || (self.cli.title && assets::witch()) {
            // (--title on WoH too: its title has nothing to show until jp_8D2E0 loads TITLE_PARTS and lays the
            // menu out)
            ui.boot(&mut game, &renderer, self.cli.skip_intro);
        }
        game.auto = self.cli.auto;
        game.skip_movies = self.cli.skip_movies;
        game.skip = self.cli.skip || self.cli.save_pc.is_some();
        game.skip_all = game.skip;
        game.auto_choice = self.cli.auto_choice;
        if let Some(n) = self.cli.jump_node {
            if game.jump_node(n) {
                ui.screen = ui::Screen::Game;
            } else {
                tracing::warn!("node {n} not reached");
            }
        }
        if let Some(name) = &self.cli.screen
            && self.cli.load_slot.is_none()
            && self.cli.screen_after.is_none()
        {
            ui.debug_open(&mut game, &renderer, name);
        }
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.game = Some(game);
        self.ui = Some(ui);
        self.last = Instant::now();
        Ok(())
    }

    /// Window pixel position -> canvas coordinates (accounts for letterboxing).
    fn to_canvas(&self, x: f64, y: f64) -> (f32, f32) {
        let Some(w) = &self.window else { return (0.0, 0.0) };
        let s = w.inner_size();
        let (sw, sh) = (s.width as f64, s.height as f64);
        let scale = (sw / CANVAS_W as f64).min(sh / CANVAS_H as f64);
        let (ox, oy) = ((sw - CANVAS_W as f64 * scale) / 2.0, (sh - CANVAS_H as f64 * scale) / 2.0);
        (((x - ox) / scale) as f32, ((y - oy) / scale) as f32)
    }

    fn frame(&mut self) {
        let (Some(r), Some(g), Some(ui)) = (self.renderer.as_mut(), self.game.as_mut(), self.ui.as_mut()) else {
            return;
        };
        // the window title follows the game language (startup, the config's language buttons)
        if self.title_language != Some(g.assets.language()) {
            self.title_language = Some(g.assets.language());
            if let Some(w) = &self.window {
                w.set_title(g.assets.title());
            }
        }
        let now = Instant::now();
        // with --shots the game runs on a fixed 60 fps clock, so shots and --keys land at exact game times however
        // long a frame (and its screenshot) takes
        let dt = if self.cli.shots.is_some() { 1000.0 / 60.0 } else { (now - self.last).as_secs_f32() * 1000.0 };
        self.last = now;
        // controllers: the same buttons as the keyboard
        let events = self.gamepads.poll(dt / 1000.0 * 60.0);
        ui.stick = stick(self.gamepads.left(), self.wasd);
        ui.rstick = rstick(self.gamepads.right_y(), self.kl);
        for (b, pressed, repeat) in events {
            ui.pad(g, r, b, pressed, repeat);
        }
        g.paused = ui.game_paused();
        let (held, advance) = ui.held_input();
        g.set_held(held, advance);
        g.flow_replay = ui.flow_replay();
        if self.cli.hold_skip {
            g.set_skip(ui.in_game());
        }
        // --save-pc and --skip keep skipping past what stops a skip on the way (a TOMP movie, sub_6EF30)
        if self.cli.skip || self.cli.save_pc.is_some_and(|pc| g.current_pc() < pc) {
            g.skip = true;
            g.skip_all = true;
        }
        if !g.paused {
            ui.rewind_before_script(g);
        }
        // a stalled frame (the machine waking, a blocked present) counts at most 100 ms, so the fixed 60 fps
        // steps of the game and the menus don't run a long burst to catch up
        let step = dt.min(100.0);
        ui.story_input(g, r, step);
        g.update(r, step);
        ui.update(g, r, step);
        // nn::atk's update after the frame callback, then the present (sub_FC064)
        g.sound.flush();
        g.system_menu_shown = ui.system_menu_shown();
        g.render(r, &mut |g, r, out| ui.draw(g, r, out));
        self.elapsed += dt;
        if let Some(pc) = self.cli.save_pc
            && g.current_pc() >= pc
        {
            g.skip = false;
            g.skip_all = false;
            if ui.debug_save(g, r, self.cli.save_slot) {
                self.cli.save_pc = None;
                self.cli.exit_after = Some(0);
            }
        }
        if let Some(t) = self.cli.save_at
            && self.elapsed >= t as f32
        {
            tracing::debug!("save_at: wait {:?}", g.wait);
            if ui.debug_save(g, r, self.cli.save_slot) {
                self.cli.save_at = None;
            }
        }
        if let Some(t) = self.cli.screen_after
            && self.elapsed >= t as f32
        {
            self.cli.screen_after = None;
            if let Some(name) = &self.cli.screen {
                ui.debug_open(g, r, name);
            }
        }
        if let Some(slot) = self.cli.load_slot.take() {
            ui.debug_load(g, r, slot);
            // --screen applies on top of the loaded game
            if let Some(name) = &self.cli.screen {
                ui.debug_open(g, r, name);
            }
        }
        while let Some((t, k)) = self.cli.keys.first().and_then(|e| e.split_once(':'))
            && t.parse::<f32>().is_ok_and(|t| self.elapsed >= t)
        {
            // a button name taps it; with a trailing + it stays down until the same name with a trailing -
            if let Some(i) = ["rup", "rdown"].iter().position(|&n| n == k.trim_end_matches(['+', '-'])) {
                let b = if i == 0 { Pad::Up } else { Pad::Down };
                for down in [true, false] {
                    if (down && !k.ends_with('-')) || (!down && !k.ends_with('+')) {
                        self.kl[i] = down;
                        ui.rstick = rstick(self.gamepads.right_y(), self.kl);
                        ui.pad(g, r, b, down, false);
                    }
                }
                self.cli.keys.remove(0);
                continue;
            }
            if let Some(b) = pad_name(k.trim_end_matches(['+', '-'])) {
                // a left stick name tilts the stick like its WASD key
                let l = [Pad::LUp, Pad::LDown, Pad::LLeft, Pad::LRight].iter().position(|&p| p == b);
                if !k.ends_with('-') {
                    if let Some(i) = l {
                        self.wasd[i] = true;
                        ui.stick = stick(self.gamepads.left(), self.wasd);
                    }
                    ui.pad(g, r, b, true, false);
                }
                if !k.ends_with('+') {
                    if let Some(i) = l {
                        self.wasd[i] = false;
                        ui.stick = stick(self.gamepads.left(), self.wasd);
                    }
                    ui.pad(g, r, b, false, false);
                }
                self.cli.keys.remove(0);
                continue;
            }
            match k {
                "menu" => ui.cancel(g, r),
                "click" => ui.click(g, r),
                "press" => ui.mouse_down(g, r),
                "release" => ui.mouse_up(g, r),
                _ if k.starts_with("wheel/") => {
                    if let Ok(n) = k[6..].parse::<f32>() {
                        ui.wheel(n, n, false);
                    }
                }
                // a trackpad's travel in canvas pixels (+ up), as MouseScrollDelta::PixelDelta at 1:1
                _ if k.starts_with("trackpad/") => {
                    if let Ok(px) = k[9..].parse::<f32>() {
                        ui.wheel(px / 40.0, px / ui::SAVE_WHEEL_ROW, true);
                    }
                }
                _ if k.starts_with("move/") => {
                    let v: Vec<f32> = k[5..].split('/').filter_map(|n| n.parse().ok()).collect();
                    if let [x, y] = v[..] {
                        ui.mouse_move(g, x, y);
                    }
                }
                _ => tracing::warn!("--keys: unknown key {k}"),
            }
            self.cli.keys.remove(0);
        }
        if let Some(dir) = &self.cli.shots
            && self.elapsed >= self.next_shot
        {
            self.next_shot += self.cli.shot_every as f32;
            let rgba = g.screenshot(r);
            let path = dir.join(format!("shot_{:04}.png", self.shot_n));
            self.shot_n += 1;
            let _ = std::fs::create_dir_all(dir);
            let (w, h) = g.canvas_size();
            if let Err(e) = image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8) {
                tracing::warn!("screenshot: {e}");
            }
            tracing::info!("shot {} scene {} pc {}\n{}", path.display(), g.scene_index, g.current_pc(), g.dump_layers());
        }
    }

    /// One frame, then --exit-after's check.
    fn tick(&mut self, el: &ActiveEventLoop) {
        self.frame();
        if self.cli.exit_after.is_some_and(|ms| self.elapsed >= ms as f32) {
            if let (Some(g), Some(ui)) = (&self.game, &mut self.ui) {
                ui.store_global(g);
            }
            el.exit();
        }
    }
}

impl ApplicationHandler for App {
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if !self.occluded || self.game.is_none() {
            return;
        }
        let now = Instant::now();
        if now >= self.hidden_tick {
            self.tick(el);
            self.hidden_tick = (self.hidden_tick + std::time::Duration::from_micros(16_667)).max(now);
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.hidden_tick));
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(e) = self.init(el) {
                tracing::error!("startup failed: {e:#}");
                eprintln!("startup failed: {e:#}");
                el.exit();
            }
            return;
        }
        // coming back to the foreground (Android): the window gets a new surface, and the sound comes back
        if let (Some(w), Some(r)) = (&self.window, &mut self.renderer) {
            let size = w.inner_size();
            if let Err(e) = r.resume(w.clone(), size.width.max(1), size.height.max(1)) {
                tracing::error!("surface: {e:#}");
                el.exit();
                return;
            }
            w.request_redraw();
        }
        if let Some(g) = &mut self.game {
            g.sound.set_muted(false);
        }
        // the time spent away is no frame
        self.last = Instant::now();
    }

    /// Going to the background (Android takes the window's surface away; nothing may draw into it, and no
    /// frames run, so the sound is muted rather than left to play on).
    fn suspended(&mut self, _el: &ActiveEventLoop) {
        if let Some(r) = &mut self.renderer {
            r.suspend();
        }
        if let Some(g) = &mut self.game {
            g.sound.set_muted(true);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let (Some(g), Some(ui)) = (&self.game, &mut self.ui) {
                    ui.store_global(g);
                }
                el.exit();
            }
            WindowEvent::Resized(s) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(s.width.max(1), s.height.max(1));
                }
            }
            WindowEvent::RedrawRequested => {
                self.tick(el);
                // the game runs at 60 Hz: on a 90 / 120 Hz screen, do not draw (and heat the phone) for
                // frames it has no use for
                #[cfg(target_os = "android")]
                if let Some(rest) = std::time::Duration::from_micros(16_667).checked_sub(self.last.elapsed()) {
                    std::thread::sleep(rest);
                }
                if !self.occluded
                    && let Some(w) = &self.window
                {
                    w.request_redraw();
                }
            }
            // a hidden window's presents don't wait for the display (or stall for a second each), and the time
            // spent so would come back as a burst of catch-up steps: run the hidden frames on a timer instead
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if let Some(r) = &mut self.renderer {
                    r.hidden = occluded;
                }
                if occluded {
                    self.hidden_tick = Instant::now();
                    el.set_control_flow(ControlFlow::WaitUntil(self.hidden_tick));
                } else {
                    el.set_control_flow(ControlFlow::Poll);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } if self.scripted => {}
            #[cfg(target_os = "android")]
            WindowEvent::Touch(_) if self.scripted => {}
            // a finger is the left mouse button, which the game takes for the Switch's touch screen (elsewhere the
            // OS turns touches into mouse events itself)
            #[cfg(target_os = "android")]
            WindowEvent::Touch(touch) => {
                use winit::event::TouchPhase;
                let tracked = self.finger == Some(touch.id);
                let (x, y) = self.to_canvas(touch.location.x, touch.location.y);
                let (Some(g), Some(ui), Some(r)) = (&mut self.game, &mut self.ui, &self.renderer) else { return };
                match touch.phase {
                    // only the first finger counts; a game controller's stick, which Android reports as moves of
                    // a pointer without a press, never starts one
                    TouchPhase::Started if self.finger.is_none() => {
                        self.finger = Some(touch.id);
                        self.cursor = (touch.location.x, touch.location.y);
                        ui.mouse_move(g, x, y);
                        ui.mouse_down(g, r);
                    }
                    TouchPhase::Moved if tracked => {
                        self.cursor = (touch.location.x, touch.location.y);
                        ui.mouse_move(g, x, y);
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled if tracked => {
                        self.finger = None;
                        self.cursor = (touch.location.x, touch.location.y);
                        if touch.phase == TouchPhase::Ended {
                            ui.mouse_move(g, x, y);
                        }
                        ui.mouse_up(g, r);
                    }
                    _ => {}
                }
            }
            // Android's Back button and a game controller's buttons: winit has no key codes for them (the
            // d-pad it reports as the arrow keys). Android's key codes, labelled as the controller labels them.
            #[cfg(target_os = "android")]
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    physical_key: PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Android(code)),
                    state,
                    repeat,
                    ..
                },
                ..
            } => {
                let (Some(g), Some(ui), Some(r)) = (&mut self.game, &mut self.ui, &self.renderer) else { return };
                let pad = match code {
                    4 | 97 => Pad::B,    // Back, BUTTON_B
                    96 => Pad::A,        // BUTTON_A
                    99 => Pad::X,        // BUTTON_X
                    100 => Pad::Y,       // BUTTON_Y
                    102 => Pad::L,       // BUTTON_L1
                    103 => Pad::R,       // BUTTON_R1
                    104 => Pad::ZL,      // BUTTON_L2
                    105 => Pad::ZR,      // BUTTON_R2
                    106 => Pad::L3,      // BUTTON_THUMBL
                    107 => Pad::R3,      // BUTTON_THUMBR
                    108 => Pad::Plus,    // BUTTON_START
                    109 => Pad::Minus,   // BUTTON_SELECT
                    _ => return,
                };
                ui.pad(g, r, pad, state == ElementState::Pressed, repeat);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                let (x, y) = self.to_canvas(position.x, position.y);
                if let (Some(g), Some(ui)) = (&mut self.game, &mut self.ui) {
                    ui.mouse_move(g, x, y);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                let (x, y) = self.to_canvas(self.cursor.0, self.cursor.1);
                if let (Some(g), Some(ui), Some(r)) = (&mut self.game, &mut self.ui, &self.renderer) {
                    ui.mouse_move(g, x, y);
                    match button {
                        MouseButton::Left => ui.mouse_down(g, r),
                        // a right click is the B button on every screen
                        MouseButton::Right => ui.pad(g, r, Pad::B, true, false),
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                if let (Some(g), Some(ui), Some(r)) = (&mut self.game, &mut self.ui, &self.renderer) {
                    match button {
                        MouseButton::Left => ui.mouse_up(g, r),
                        MouseButton::Right => ui.pad(g, r, Pad::B, false, false),
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // in notches; a trackpad's pixels count 40 to the notch. Save list rows: a notch, or the
                // trackpad's displacement (momentum included) in `SAVE_WHEEL_ROW`s on screen
                let (dy, rows, trackpad) = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y, y, false),
                    MouseScrollDelta::PixelDelta(p) => {
                        let s = self.window.as_ref().map_or(1.0, |w| {
                            let s = w.inner_size();
                            (s.width as f32 / CANVAS_W as f32).min(s.height as f32 / CANVAS_H as f32)
                        });
                        (p.y as f32 / 40.0, p.y as f32 / (ui::SAVE_WHEEL_ROW * s), true)
                    }
                };
                // the wheel / trackpad tilts the left stick (no Switch control)
                if let Some(ui) = &mut self.ui
                    && dy != 0.0
                {
                    ui.wheel(dy, rows, trackpad);
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    physical_key: PhysicalKey::Code(code),
                    state,
                    repeat,
                    ..
                },
                ..
            } => {
                let (Some(g), Some(ui), Some(r)) = (&mut self.game, &mut self.ui, &self.renderer) else { return };
                let pressed = state == ElementState::Pressed;
                // the Switch controller on the keyboard (README.md); every screen sees the same buttons
                let pad = match code {
                    KeyCode::ArrowUp => Some(Pad::Up),
                    KeyCode::ArrowDown => Some(Pad::Down),
                    KeyCode::ArrowLeft => Some(Pad::Left),
                    KeyCode::ArrowRight => Some(Pad::Right),
                    KeyCode::Enter | KeyCode::NumpadEnter => Some(Pad::A),
                    KeyCode::Escape => Some(Pad::B),
                    KeyCode::KeyB => Some(Pad::X),
                    KeyCode::KeyM => Some(Pad::Y),
                    KeyCode::KeyH => Some(Pad::L),
                    KeyCode::KeyJ => Some(Pad::R),
                    KeyCode::KeyT => Some(Pad::ZL),
                    KeyCode::KeyY => Some(Pad::ZR),
                    KeyCode::KeyW => Some(Pad::LUp),
                    KeyCode::KeyS => Some(Pad::LDown),
                    KeyCode::KeyA => Some(Pad::LLeft),
                    KeyCode::KeyD => Some(Pad::LRight),
                    KeyCode::KeyO => Some(Pad::R3),
                    KeyCode::KeyP => Some(Pad::L3),
                    KeyCode::KeyU => Some(Pad::Minus),
                    KeyCode::KeyI => Some(Pad::Plus),
                    _ => None,
                };
                // K / L: the right stick up / down, which the game reads as the d-pad plus its tilt
                if let Some(i) = [KeyCode::KeyK, KeyCode::KeyL].iter().position(|&k| k == code) {
                    self.kl[i] = pressed;
                    ui.rstick = rstick(self.gamepads.right_y(), self.kl);
                    ui.pad(g, r, if i == 0 { Pad::Up } else { Pad::Down }, pressed, repeat);
                    return;
                }
                if let Some(b) = pad {
                    if let Some(i) = [Pad::LUp, Pad::LDown, Pad::LLeft, Pad::LRight].iter().position(|&p| p == b) {
                        self.wasd[i] = pressed;
                        ui.stick = stick(self.gamepads.left(), self.wasd);
                    }
                    ui.pad(g, r, b, pressed, repeat);
                    return;
                }
                match code {
                    KeyCode::ControlLeft => g.set_skip(pressed && ui.in_game()),
                    KeyCode::KeyF if pressed && !repeat => {
                        if let Some(w) = &self.window {
                            let fs = w.fullscreen().is_some();
                            w.set_fullscreen((!fs).then_some(winit::window::Fullscreen::Borderless(None)));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// `--resolution`
#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
enum Resolution {
    Window,
    Original,
}

/// `--language`: the folder names of the overseas data's languages (sub_2DE70's order).
fn parse_language(s: &str) -> Result<i16, String> {
    match s.to_ascii_lowercase().as_str() {
        "ja" | "jp" | "japanese" => Ok(0),
        "en" | "english" => Ok(1),
        "zc" | "zh-hans" | "zh-cn" => Ok(2),
        "zt" | "zh-hant" | "zh-tw" => Ok(3),
        _ => Err("expected ja, en, zc or zt".into()),
    }
}

/// The program: what the `knife` executable runs.
pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,wgpu=warn,naga=warn".into()),
        )
        .init();
    start(EventLoop::new()?)
}

/// The program on Android, called by `android_main` of the `knife-android` library. The activity's `AndroidApp`
/// stands in for what a desktop process has: the event loop, the folder `assets` and `knife.ini` are in (the
/// app's files folder, `Android/data/<package>/files`) and a log (logcat).
#[cfg(target_os = "android")]
pub fn run_android(app: AndroidApp) -> Result<()> {
    use winit::platform::android::EventLoopBuilderExtAndroid;
    platform::android::init(&app);
    let el = EventLoop::builder().with_android_app(app).build()?;
    start(el)
}

fn start(el: EventLoop<()>) -> Result<()> {
    // knife.ini fills the options the command line leaves out
    let cli = Cli::parse_from(ini::merged_args());
    let build = assets::detect_build(&cli.assets_dir());
    assets::select_build(build);
    save::set_file(cli.save_file.clone().unwrap_or_else(|| save::default_file(build)));
    savedata::set_system_language(cli.language.unwrap_or_else(|| assets::system_language(build)));
    if let Some(language) = cli.language {
        savedata::set_language_override(language);
    }
    // a busy port must not keep the game from starting
    if let Some(address) = &cli.text_websocket
        && let Err(error) = text_stream::start(address)
    {
        tracing::error!("Text WebSocket server disabled: {error:#}");
    }
    let shot_from = cli.shot_from as f32;
    el.set_control_flow(ControlFlow::Poll);
    let scripted = !cli.keys.is_empty();
    let mut app = App {
        cli,
        window: None,
        renderer: None,
        game: None,
        ui: None,
        last: Instant::now(),
        cursor: (0.0, 0.0),
        elapsed: 0.0,
        next_shot: shot_from,
        shot_n: 0,
        gamepads: gamepad::Gamepads::new(),
        wasd: [false; 4],
        kl: [false; 2],
        scripted,
        title_language: None,
        occluded: false,
        hidden_tick: Instant::now(),
        finger: None,
    };
    el.run_app(&mut app)?;
    Ok(())
}

/// The left stick: the controllers' tilt plus W / S / A / D (up, down, left, right) at full tilt, clamped to ±128.
fn stick(pads: [i32; 2], [up, down, left, right]: [bool; 4]) -> [i32; 2] {
    let key = |a: bool, b: bool| (i32::from(a) - i32::from(b)) * 128;
    [(pads[0] + key(right, left)).clamp(-128, 128), (pads[1] + key(up, down)).clamp(-128, 128)]
}

/// The right stick's y: the controllers' plus K (up) / L (down) at full tilt, clamped to ±128.
fn rstick(pad: i32, [up, down]: [bool; 2]) -> i32 {
    (pad + (i32::from(up) - i32::from(down)) * 128).clamp(-128, 128)
}

fn pad_name(k: &str) -> Option<Pad> {
    Some(match k {
        "up" => Pad::Up,
        "down" => Pad::Down,
        "left" => Pad::Left,
        "right" => Pad::Right,
        "a" => Pad::A,
        "b" => Pad::B,
        "x" => Pad::X,
        "y" => Pad::Y,
        "l" => Pad::L,
        "r" => Pad::R,
        "zl" => Pad::ZL,
        "zr" => Pad::ZR,
        "lup" => Pad::LUp,
        "ldown" => Pad::LDown,
        "lleft" => Pad::LLeft,
        "lright" => Pad::LRight,
        "r3" => Pad::R3,
        "l3" => Pad::L3,
        "minus" => Pad::Minus,
        "plus" => Pad::Plus,
        _ => return None,
    })
}
