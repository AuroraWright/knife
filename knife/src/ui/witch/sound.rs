//! Witch on the Holy Night's Sound mode (楽曲, the title's Extra menu): jp_46660 sets it up (objects 265..293:
//! the five transport buttons, the three page tabs and the 21 list slots, the extra / sound atlases, the heard
//! tracks), jp_43BF0 runs it (jd_B2D550 states 10..14) and jp_44B70 draws it (task 23 through jp_424D0, with
//! jp_424D0's black on top); state 12 hands back to the title as the Extra menu left it (sub_6C170).
//!
//! 56 tracks on three pages of 3 x 7 slots, each slot a panel with the title baked in (textures 3644..3655,
//! one set of normal / focused / on / on-and-focused per page). The pad moves through the tabs, the list and the
//! buttons (◀◀ ▶ ■ ▶▶ and the repeat mode); A or a tap on the cursor's object plays / stops / switches; ZL / ZR
//! step through the heard tracks, Y stops, X switches between playing the tracks in turn (each for its length)
//! and repeating one, L / R or a flick turn the page. The playing track's title and composer show over the
//! buttons, a light sweeping over them, and the cursor's object pulses.

use std::sync::OnceLock;

use knife_render::{Blend, Quad, RenderTarget, Renderer, Shade, TexRef};

use super::super::{H, Screen, Ui, W};
use super::objs::{WFrame, WStates};
use super::{DOWN, LEFT, MASK, Pen, RIGHT, UP, WScr, objtab};
use crate::game::Game;

/// jd_34F75C (24-byte rows): the BGM number, the page (its panel textures), the panel's cell in the page's
/// title atlas (and its row in the title strips), the length in seconds (twice) and the composer strip.
const TRACKS: [(i32, usize, usize, i32, usize); 56] = [
    (2, 0, 0, 193, 4), (3, 0, 1, 134, 4), (4, 0, 2, 304, 4), (5, 0, 3, 192, 4), (6, 0, 4, 174, 4),
    (7, 0, 5, 308, 4), (8, 0, 6, 117, 4), (9, 0, 7, 303, 4), (10, 0, 8, 115, 4), (11, 0, 9, 122, 4),
    (12, 0, 10, 145, 4), (13, 0, 11, 126, 4), (14, 0, 12, 138, 4), (15, 0, 13, 122, 4), (16, 0, 14, 145, 4),
    (17, 0, 15, 122, 4), (18, 0, 16, 197, 4), (19, 0, 17, 118, 4), (20, 0, 18, 126, 4), (45, 2, 2, 129, 4),
    (21, 0, 20, 106, 2), (22, 1, 0, 128, 2), (23, 1, 1, 120, 2), (24, 1, 2, 117, 2), (25, 1, 3, 118, 2),
    (26, 1, 4, 163, 1), (27, 1, 5, 139, 1), (28, 1, 6, 138, 1), (29, 1, 7, 128, 1), (30, 1, 8, 144, 1),
    (31, 1, 9, 97, 1), (32, 1, 10, 127, 1), (33, 1, 11, 135, 3), (34, 1, 12, 175, 3), (35, 1, 13, 141, 3),
    (36, 1, 14, 78, 3), (37, 1, 15, 130, 3), (38, 1, 16, 64, 1), (39, 1, 17, 145, 1), (40, 1, 18, 152, 3),
    (41, 1, 19, 224, 2), (42, 1, 20, 109, 2), (43, 2, 0, 64, 1), (44, 2, 1, 125, 4), (46, 2, 3, 152, 4),
    (47, 2, 4, 134, 4), (48, 2, 5, 141, 4), (49, 2, 6, 190, 4), (51, 2, 7, 142, 4), (50, 2, 8, 127, 4),
    (59, 2, 9, 133, 4), (60, 2, 10, 106, 4), (61, 2, 11, 139, 4), (62, 2, 12, 190, 4), (63, 2, 13, 301, 4),
    (65, 2, 14, 113, 4),
];

/// sub_28170: the page a BGM is listed on (dword_260A18 for the numbers 2..65 in the mask), else the page on view.
const PAGE_OF: [usize; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 2, 2, 0, 2, 2, 2, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 2, 2, 0, 2,
];
const PAGE_MASK: u64 = 0xBE03_FFFF_FFFF_FFFF;

/// dword_3CC8A8[2 k + 27]: the composer strips of texture 3640 (k 1..4).
const COMPOSER: [[f32; 4]; 5] = [
    [0.0, 0.0, 0.0, 0.0],
    [8.0, 8.0, 608.0, 56.0],
    [8.0, 64.0, 608.0, 112.0],
    [8.0, 120.0, 608.0, 168.0],
    [8.0, 176.0, 608.0, 224.0],
];

/// The step's speed (jp_B0190 x 0.7 at 60 Hz).
const F: f32 = 0.7;

/// The list slot `n`'s title strip in its page's title texture (dword_3CC8A8[2 n + 210]).
fn title_src(n: usize) -> [f32; 4] {
    let (x, row) = if n < 15 { (8.0, n) } else { (616.0, n - 15) };
    let w = if n < 15 { 600.0 } else { 1152.0 };
    let y = 8.0 + 70.0 * row as f32;
    [x, y, x + w, y + 62.0]
}

/// jp_46480's frames for each track: its page's panel textures (normal, focused, on, on and focused, pressed =
/// focused) at its cell (528 x 72 cells, 14 to a column).
fn slot_states() -> &'static [WStates; 56] {
    static S: OnceLock<[WStates; 56]> = OnceLock::new();
    S.get_or_init(|| {
        std::array::from_fn(|k| {
            let (_, kind, n, _, _) = TRACKS[k];
            let base = 3644 + 4 * kind as i32;
            let (x, y) = (528 * (n / 14) as i16 + 8, 72 * (n % 14) as i16 + 8);
            let frame = |tex: i32| -> &'static [WFrame] {
                Box::leak(Box::new([WFrame { tex, flags: 0x10, blend: 0, d: [0.0, 0.0], src: [x, y, x + 518, y + 66], color: 0xFFFF_FFFF }]))
            };
            [frame(base + 2), frame(base + 3), frame(base), frame(base + 1), frame(base + 3)]
        })
    })
}

pub struct WSnd {
    /// jd_B2D550 (10..14, 0 gone)
    st: u32,
    /// jd_B2D5E8: the screen's alpha
    fade: f32,
    /// jd_B2D550 + 4: jp_424D0's black over everything
    black: f32,
    /// jd_B2D5D8 / dword_6771D0: the page on view and the one a turn goes to
    page: usize,
    to_page: usize,
    /// jd_B2D608: the turn's progress (out 0 -> 1, in 1 -> 0); jd_B2D60C: 2 leaving, 1 entering, 0 settled;
    /// jd_B2D54C: towards a later page
    turn: f32,
    turning: u8,
    fwd: bool,
    /// LODWORD(qword_6771A8): the cursor (object - 265: 0..4 the buttons, 5..7 the tabs, 8..28 the slots, -1
    /// none) and HIDWORD its objects' alpha
    cur: i32,
    objs_a: f32,
    /// jd_B2D548: a track plays
    playing: bool,
    /// off_6325E0: the BGM the mode played last (-1 none)
    bgm: i32,
    /// qword_6771B8: the light's x (0..320) and the title strip's alpha
    light_x: f32,
    title_a: f32,
    /// dword_6771C0: the cursor's pulse (radians)
    phase: f32,
    /// jd_B2D540 + 360: each track's BGM once heard (or after the ending), else -1
    avail: [i32; 56],
    /// dword_6771C4: frames before the track end is checked again; dword_6771CC: frames the track has played
    /// so far, up to 30
    hold: i32,
    steady: i32,
    /// the playing stream's frames (jp_9CA70 reads the player's 60 Hz count)
    frames: u32,
    /// jd_B2D5FC: the current track, kept from visit to visit
    track: usize,
    /// jd_58FE84: the tracks play in turn (1, the start) or one repeats (0), kept from visit to visit
    seq: bool,
    /// dword_3CC8D0: the track that has played 30 frames (-1 none), which the turn moves on from
    confirmed: i32,
    /// the two surfaces of the light (jo_AE0C40 + 0 / + 856: its mask and its strokes)
    rt: Option<[TexRef; 2]>,
}

impl Default for WSnd {
    fn default() -> Self {
        Self {
            st: 0,
            fade: 0.0,
            black: 0.0,
            page: 0,
            to_page: 0,
            turn: 0.0,
            turning: 0,
            fwd: false,
            cur: 1,
            objs_a: 0.0,
            playing: false,
            bgm: -1,
            light_x: 0.0,
            title_a: 0.0,
            phase: 0.0,
            avail: [-1; 56],
            hold: 0,
            steady: 0,
            frames: 0,
            track: 0,
            seq: true,
            confirmed: -1,
            rt: None,
        }
    }
}

impl WSnd {
    fn heard(&self, k: usize) -> bool {
        self.avail.get(k).is_some_and(|&b| b >= 0)
    }

    /// The next (or previous) heard track after the current one, wrapping (the current one when none is).
    fn step_track(&self, next: bool) -> usize {
        let mut k = self.track as i32;
        for _ in 0..56 {
            k = if next {
                if k <= 54 { k + 1 } else { 0 }
            } else if k < 1 {
                55
            } else {
                k - 1
            };
            if self.heard(k as usize) {
                break;
            }
        }
        k as usize
    }
}

impl Ui {
    /// jp_46660 (jp_48AC0 mode 0, result 6): the title's BGM stops, the heard tracks, the objects, the
    /// textures; state 10.
    pub(in crate::ui) fn wsnd_init(&mut self, game: &mut Game, r: &Renderer) {
        let s = &mut self.w.snd;
        s.fade = 0.0;
        s.page = 0;
        game.sound.stop_bgm(0.0);
        s.playing = false;
        s.light_x = 0.0;
        s.title_a = 0.0;
        s.phase = 0.0;
        s.black = 0.0;
        // jp_45FE0: the cursor on ▶, the tracks heard (jp_77740: bit 3200 + n, n <= 0x207) or all after the
        // ending (jp_8D1C0)
        s.cur = 1;
        s.objs_a = 0.0;
        let complete = super::title::complete(game);
        for (k, t) in TRACKS.iter().enumerate() {
            let heard = t.0 <= 0x207 && crate::game::get_bit(&game.heard, t.0 as usize);
            s.avail[k] = if heard || complete { t.0 } else { -1 };
        }
        let seq = s.seq;
        self.w.objs.install(objtab::SOUND);
        self.w.objs.set_states(269, if seq { &objtab::SOUND_SEQ } else { &objtab::SOUND_ONE });
        for id in 265..=293 {
            self.w.objs.scale(id, 0.33333, 0.41481);
            if let Some(o) = self.w.objs.get(id) {
                o.alpha = 0.0;
            }
        }
        self.wsnd_relabel();
        self.w.tex.load(game, r, objtab::RES_EXTRA);
        self.w.tex.load(game, r, objtab::RES_SOUND);
        // the button guide's help_key / help_txt (textures 76 / 77, loaded with the story's display parts)
        if self.w.tex.get(76).is_none() {
            self.w.tex.load(game, r, objtab::RES_DISP);
        }
        // texture 242: border_parts #4 (alphagradation256), the light's stroke
        if self.w.tex.get(242).is_none() {
            self.w.tex.load(game, r, &[(242, "BORDER_PARTS", 4)]);
        }
        self.w.snd.st = 10;
        self.w.guide.on = true;
        self.w.scr = WScr::Sound;
        self.screen = Screen::Witch;
        self.stack.clear();
    }

    /// jp_46480: the page's slots take their tracks' panels (the blank panel past the list or for a track not
    /// heard), none on.
    fn wsnd_relabel(&mut self) {
        let s = &self.w.snd;
        let states = slot_states();
        let slots: Vec<Option<usize>> = (0..21).map(|i| Some(i + 21 * s.page).filter(|&k| k <= 55 && s.heard(k))).collect();
        for (i, k) in slots.into_iter().enumerate() {
            let id = 273 + i as i32;
            self.w.objs.set_states(id, k.map_or(&objtab::SOUND_EMPTY, |k| &states[k]));
            self.w.objs.flags(id, 0, 2);
        }
    }

    /// The exe's play (inlined at each caller): with a track playing, a different BGM crossfades in (the old
    /// one out at 0.8333 points a frame, jp_9C9B0; the new one in from silence at 1.6667, jp_9C6D0) and the
    /// page follows it (sub_28170); without, it starts at once (jp_9C470). The pad and touch paths follow the
    /// track's page in any case.
    fn wsnd_play(&mut self, game: &mut Game, bgm: i32, follow: bool) {
        let s = &mut self.w.snd;
        let mut follow = follow;
        if s.playing {
            if s.bgm != bgm {
                game.sound.stop_bgm_rate(0.83333);
                game.play_bgm(bgm, 1.0, 1.0);
                s.frames = 0;
                follow = true;
            }
        } else {
            game.play_bgm(bgm, 1.0, 0.0);
            s.frames = 0;
        }
        s.bgm = bgm;
        s.playing = true;
        if follow {
            self.wsnd_follow(bgm);
        }
    }

    /// sub_28170's page turn after a play: to the track's page (state 14).
    fn wsnd_follow(&mut self, bgm: i32) {
        let s = &mut self.w.snd;
        let v = bgm - 2;
        let p = if (0..64).contains(&v) && (PAGE_MASK >> v) & 1 != 0 { PAGE_OF[v as usize] } else { s.page };
        if p != s.page {
            s.to_page = p;
            s.turn = 0.0;
            s.st = 14;
            s.fwd = s.page < p;
        }
    }

    /// jp_9C9B0(1.1111): ■, the track fades out.
    fn wsnd_stop(&mut self, game: &mut Game) {
        game.sound.stop_bgm_rate(1.1111);
        self.w.snd.playing = false;
        self.w.snd.hold = 60;
    }

    /// One frame of jp_43BF0; true once state 12 has handed back to the title.
    #[allow(clippy::approx_constant)]
    pub(in crate::ui) fn wsnd_step(&mut self, game: &mut Game, r: &Renderer) -> bool {
        let mut done = false;
        match self.w.snd.st {
            10 => {
                let s = &mut self.w.snd;
                s.fade += F * 0.024;
                if s.fade >= 1.0 {
                    s.fade = 1.0;
                    self.w.objs.group_flags(21, 1, 0);
                    self.w.objs.group_flags(22, 1, 0);
                    if s.cur >= 0 {
                        self.w.objs.focus = s.cur + 265;
                    }
                    s.st = 11;
                }
                s.objs_a = (s.objs_a + 0.04).min(1.0);
            }
            11 => {
                self.wsnd_input(game);
                if self.w.snd.st == 11 {
                    self.wsnd_auto(game);
                }
            }
            12 => {
                let s = &mut self.w.snd;
                s.fade -= 0.024;
                if s.fade <= 0.0 {
                    s.fade = 0.0;
                    // jp_9C470 / jp_9C6D0(jp_8E1C0()): the title's BGM, crossfading in over a track still playing
                    let title = if super::title::complete(game) { 42 } else { 65 };
                    if s.playing {
                        if s.bgm != title {
                            game.sound.stop_bgm_rate(0.83333);
                            game.play_bgm(title, 1.0, 1.0);
                        }
                    } else {
                        game.play_bgm(title, 1.0, 0.0);
                    }
                    s.bgm = -1;
                    s.st = 0;
                    s.playing = false;
                    s.rt = None;
                    // jp_425A0
                    self.w.objs.remove_group(21);
                    self.w.objs.remove_group(22);
                    done = true;
                }
                let s = &mut self.w.snd;
                s.black = 1.0 - s.fade;
            }
            13 => {
                let s = &mut self.w.snd;
                s.turn += F * -0.072;
                if s.turn <= 0.0 {
                    s.turn = 0.0;
                    self.w.objs.group_flags(21, 1, 0);
                    self.w.objs.group_flags(22, 1, 0);
                    s.st = 11;
                    s.turning = 0;
                }
            }
            14 => {
                let s = &mut self.w.snd;
                if s.page == s.to_page {
                    s.st = 11;
                } else {
                    if s.turn == 0.0 {
                        self.w.objs.group_flags(21, 0, 1);
                        self.w.objs.group_flags(22, 0, 1);
                        game.play_sys_sound(2);
                        s.turning = 2;
                    }
                    let s = &mut self.w.snd;
                    s.turn += F * 0.072;
                    if s.turn >= 1.0 {
                        s.turn = 1.0;
                        s.page = s.to_page;
                        s.st = 13;
                        s.turning = 1;
                        self.wsnd_relabel();
                        let s = &mut self.w.snd;
                        let base = 21 * s.page as i32;
                        if s.cur + base - 8 >= 56 {
                            s.cur = 63 - base;
                            self.w.objs.focus = 328 - base;
                        }
                    }
                }
            }
            _ => {}
        }
        // every state: the title strip and the light follow the playing track, the pulse turns
        let s = &mut self.w.snd;
        if s.playing {
            s.frames += 1;
            s.title_a = (s.title_a + 0.033333).min(1.0);
            s.light_x += 1.5;
            if s.light_x >= 320.0 {
                s.light_x -= 320.0;
            }
        } else {
            s.title_a = (s.title_a - 0.011111).max(0.0);
            if s.title_a > 0.0 {
                s.light_x += 1.5;
                if s.light_x >= 320.0 {
                    s.light_x -= 320.0;
                }
            } else {
                s.light_x = 0.0;
            }
        }
        s.phase += 0.05236;
        if s.phase >= 6.2832 {
            s.phase -= 6.2832;
        }
        if done {
            self.w.scr = WScr::None;
            self.wt_from_extra(game, r);
        }
        done
    }

    /// jp_46770: a track that ended (or, playing in turn, ran its length) moves on to the next heard one
    /// once it had played 30 frames, or plays again when one repeats.
    fn wsnd_auto(&mut self, game: &mut Game) {
        let s = &mut self.w.snd;
        if s.hold >= 1 {
            s.hold -= 1;
            return;
        }
        if !s.playing {
            s.confirmed = -1;
            return;
        }
        let on = game.sound.bgm_state().is_some();
        let len = TRACKS[s.track].3;
        let ended = (s.seq && (1000 * s.frames / 60) as i64 >= 1000 * i64::from(len)) || !on;
        if ended {
            s.steady = 0;
            if s.seq {
                if s.confirmed == s.track as i32 {
                    s.track = s.step_track(true);
                }
                let bgm = TRACKS[s.track].0;
                self.wsnd_play(game, bgm, false);
                self.w.snd.hold = 60;
            } else if s.heard(s.track) {
                // jp_9C610 then jp_9C470: from the start
                let bgm = TRACKS[s.track].0;
                game.sound.stop_bgm(0.0);
                game.play_bgm(bgm, 1.0, 0.0);
                s.frames = 0;
                s.bgm = bgm;
                s.hold = 60;
            }
        } else if s.steady > 29 {
            s.confirmed = s.track as i32;
        } else {
            s.steady += 1;
        }
    }

    /// Play track `k` (it becomes the current one) and hold the end check for 60 frames.
    fn wsnd_play_track(&mut self, game: &mut Game, k: usize) {
        self.w.snd.track = k;
        self.wsnd_play(game, TRACKS[k].0, true);
        self.w.snd.steady = 0;
        self.w.snd.hold = 60;
    }

    /// X / the repeat button: the tracks in turn or one repeating.
    fn wsnd_mode(&mut self, game: &mut Game) {
        let s = &mut self.w.snd;
        s.seq = !s.seq;
        let seq = s.seq;
        self.w.objs.set_states(269, if seq { &objtab::SOUND_SEQ } else { &objtab::SOUND_ONE });
        game.play_sys_sound(2);
    }

    /// jp_469C0: state 11's input.
    fn wsnd_input(&mut self, game: &mut Game) {
        let inp = &self.w.inp;
        let (pressed, repeat, touch, flick) = (inp.pressed, inp.repeat, inp.touch, inp.flick);
        if pressed & MASK[1] != 0 || self.w.guide.tapped {
            self.w.snd.st = 12;
            game.play_sys_sound(1);
            self.w.objs.group_flags(21, 0, 1);
            self.w.objs.group_flags(22, 0, 1);
            self.w.snd.hold = 60;
            self.w.guide.on = false;
            return;
        }
        if pressed & 0x8000 != 0 {
            // Y: stop
            self.wsnd_stop(game);
        } else if pressed & 0x1000 != 0 {
            // X: the repeat mode
            self.wsnd_mode(game);
        } else if pressed & 0x400 != 0 || flick & 2 != 0 {
            // L / a right flick: the page before
            let s = &mut self.w.snd;
            s.fwd = false;
            s.to_page = if s.page >= 1 { s.page - 1 } else { 2 };
            s.turn = 0.0;
            s.st = 14;
            return;
        } else if pressed & 0x800 != 0 || flick & 1 != 0 {
            // R / a left flick: the page after
            let s = &mut self.w.snd;
            s.to_page = if s.page <= 1 { s.page + 1 } else { 0 };
            s.turn = 0.0;
            s.fwd = true;
            s.st = 14;
            return;
        } else if pressed & 0x300 != 0 {
            // ZL / ZR: the heard track before / after
            let k = self.w.snd.step_track(pressed & 0x100 == 0);
            self.wsnd_play_track(game, k);
        }
        // the pointer's object becomes the cursor
        let hit = self.w.objs.hit;
        let base = 21 * self.w.snd.page as i32;
        let mut moved = false;
        let mut v8 = self.w.snd.cur;
        if (0..=28).contains(&(hit - 265)) {
            if hit + base > 328 {
                v8 = -1;
                self.w.snd.cur = -1;
            } else if self.w.snd.cur != hit - 265 {
                self.w.snd.cur = hit - 265;
                game.play_sys_sound(2);
                self.w.objs.focus = hit;
                moved = true;
                v8 = hit - 265;
            }
        }
        let region = match v8 {
            8.. => 2,
            5..=7 => 1,
            0..=4 => 0,
            _ => 3,
        };
        let tapped = !moved && hit == v8 + 265 && touch & 2 != 0;
        if pressed & MASK[0] != 0 || tapped {
            self.wsnd_activate(game, v8);
            return;
        }
        let s = &mut self.w.snd;
        let to = if repeat & LEFT != 0 {
            let p = pressed & LEFT != 0;
            match region {
                2 if v8 < 9 => match p {
                    false => v8,
                    true if base < 36 => 28,
                    true => 63 - base,
                },
                1 if v8 < 6 => if p { 7 } else { v8 },
                0 if v8 < 1 => if p { 4 } else { v8 },
                0..=2 => v8 - 1,
                _ => v8,
            }
        } else if repeat & RIGHT != 0 {
            let p = pressed & RIGHT != 0;
            match region {
                2 if v8 - 7 >= (56 - base).min(21) => if p { 8 } else { v8 },
                1 if v8 > 6 => if p { 5 } else { v8 },
                0 if v8 > 3 => if p { 0 } else { v8 },
                0..=2 => v8 + 1,
                _ => v8,
            }
        } else if repeat & UP != 0 {
            let p = pressed & UP != 0;
            match region {
                // into the list's bottom row, up to a row with a track
                0 if p => {
                    let mut v38 = ((v8 as f32 * 3.0) / 5.0) as i32 + 26;
                    loop {
                        let v39 = v38 + base - 8;
                        let mut v40 = if v38 < 11 { 8 } else { v38 - 3 };
                        if v39 <= 55 {
                            v40 = v38;
                        }
                        if v39 < 56 || v38 <= 10 {
                            break v40;
                        }
                        v38 = v40;
                    }
                }
                1 if p => (((v8 - 5) as f32 * 5.0) / 3.0).ceil() as i32,
                2 if v8 > 10 => v8 - 3,
                2 if p => v8.max(8) - 3,
                3 => 0,
                _ => v8,
            }
        } else if repeat & DOWN != 0 {
            let p = pressed & DOWN != 0;
            match region {
                2 if v8 <= 25 && v8 + base - 5 < 56 => v8 + 3,
                2 if p => ((((v8 - 8) % 3) as f32 * 5.0) / 3.0).ceil() as i32,
                1 if p => {
                    let k = v8 - 5;
                    if k + base <= 55 { k + 8 } else { 8 }
                }
                0 if p => ((v8 as f32 * 3.0) / 5.0) as i32 + 5,
                3 => 0,
                _ => v8,
            }
        } else {
            return;
        };
        s.cur = to;
        self.w.objs.focus = to + 265;
        if to != v8 {
            game.play_sys_sound(2);
        }
    }

    /// A / a tap on the cursor's object.
    fn wsnd_activate(&mut self, game: &mut Game, v8: i32) {
        let s = &self.w.snd;
        match v8 {
            0 => {
                let k = s.step_track(false);
                self.wsnd_play_track(game, k);
            }
            1 => {
                if s.heard(s.track) {
                    let k = s.track;
                    self.wsnd_play_track(game, k);
                }
            }
            2 => self.wsnd_stop(game),
            3 => {
                let k = s.step_track(true);
                self.wsnd_play_track(game, k);
            }
            4 => self.wsnd_mode(game),
            5..=7 => {
                let s = &mut self.w.snd;
                s.to_page = (v8 - 5) as usize;
                s.turn = 0.0;
                s.st = 14;
                s.fwd = s.page < s.to_page;
            }
            8..=28 => {
                let k = (v8 - 8) as usize + 21 * s.page;
                if k <= 55 && s.heard(k) {
                    self.wsnd_play_track(game, k);
                }
            }
            _ => {}
        }
    }

    /// jp_47570: the objects' alpha, on-states and the slots' entry / page turn motion.
    fn wsnd_objects(&mut self) {
        let s = &self.w.snd;
        let a = s.objs_a * s.objs_a;
        let (playing, seq, page, fade, turning, turn, fwd, track) = (s.playing, s.seq, s.page, s.fade, s.turning, s.turn, s.fwd, s.track);
        let objs = &mut self.w.objs;
        for id in 265..=272 {
            if let Some(o) = objs.get(id) {
                o.alpha = a;
            }
            objs.flags(id, 0, 2);
        }
        // ▶ on while a track plays, ■ otherwise; the repeat button on while one repeats
        objs.flags(if playing { 266 } else { 267 }, 2, 0);
        objs.set_states(269, if seq { &objtab::SOUND_SEQ } else { &objtab::SOUND_ONE });
        if !seq {
            objs.flags(269, 2, 0);
        }
        objs.flags(270 + page as i32, 2, 0);
        for i in 0..21 {
            let id = 273 + i;
            let v15 = i as f32;
            let v16 = 1.0 - ((v15 / -20.0 + 2.0) * fade).clamp(0.0, 1.0);
            let v17 = v16 * (v16 * 4.0);
            let mut x = v17;
            let mut v19 = 0.0;
            if turning != 0 {
                let t = (v15 + v15) / 20.0;
                let (v21, v22, v23) = if turning == 1 {
                    ((t + 1.0) * turn, (3.0 - t) * turn, -40.0)
                } else {
                    ((3.0 - t) * turn, (t + 1.0) * turn, 40.0)
                };
                let (v21, v22): (f32, f32) = (v21.clamp(0.0, 1.0), v22.clamp(0.0, 1.0));
                if fwd {
                    x = v17 + v21 * (v21 * -v23);
                    v19 = v21;
                } else {
                    x = v17 + v22 * (v22 * v23);
                    v19 = v22;
                }
            }
            let y = (i / 3) as f32 * 44.8 + 63.052 + v17;
            let alpha = if i + 21 * page as i32 >= 56 { 0.0 } else { (1.0 - v19) * ((1.0 - v16 * v16) * (1.0 - v19)) };
            objs.place(id, (i % 3) as f32 * 193.33 + 42.667 + x, y, alpha);
            objs.flags(id, 0, 2);
        }
        if (21 * page..21 * page + 21).contains(&track) {
            objs.flags((track - 21 * page + 273) as i32, 2, 0);
        }
    }

    /// jp_424D0 for Sound mode: jp_44B70 and the black.
    pub(in crate::ui) fn wsnd_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, 1.0]));
        if self.w.snd.fade > 0.0 {
            self.wsnd_draw_screen(game, r, out);
        }
        let b = self.w.snd.black;
        if b > 0.0 {
            out.push(Quad::rect(r.white.clone(), [0.0, 0.0, W, H], [0.0, 0.0, 0.0, b.min(1.0)]));
        }
    }

    /// jp_44B70: the backdrop, the guide, the bar, the playing track's title, the page arrows, the objects,
    /// the light over the playing track's pieces and the cursor's pulse.
    fn wsnd_draw_screen(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let s = &self.w.snd;
        let (fade, title_a, cur) = (s.fade, s.title_a, s.cur);
        let tex = |id| self.w.tex.get(id);
        let mut pen = Pen::default();
        pen.color(fade, 1.0, 1.0, 1.0);
        pen.scale(0.33333, 0.41481);
        for (id, x, y) in [(3624, 0.0, 0.0), (3625, 450.6667, -47.288887), (3626, 546.6667, 69.68889), (3637, 0.0, 0.0)] {
            if let Some(t) = tex(id) {
                pen.spr(out, &t, 16, None, x, y);
            }
        }
        self.wg_draw(out, &pen, 8, 0.0, 0.0);
        const BAR: [f32; 4] = [0.0, 2.0, 1920.0, 17.0];
        if let Some(t) = tex(3634) {
            pen.spr(out, &t, 16, Some(BAR), 0.0, 398.22);
        }
        // the pieces the light passes over (jp_B7010 / jp_B6F60 mode 1: into surface 0)
        let mut mask = Vec::new();
        let mut white = pen;
        white.color(1.0, 1.0, 1.0, 1.0);
        if let Some(t) = tex(3634) {
            white.spr(&mut mask, &t, 16, Some(BAR), 0.0, 398.22);
        }
        let k = self.w.snd.track;
        let (_, kind, n, _, comp) = TRACKS[k.min(55)];
        let comp_src = COMPOSER.get(comp).copied().unwrap_or([0.0; 4]);
        if title_a > 0.0 && k <= 55 {
            let mut p = pen;
            p.color(title_a, 1.0, 1.0, 1.0);
            if let Some(t) = tex(3640) {
                p.spr(out, &t, 16, Some(comp_src), 40.0, 362.55);
            }
            if let Some(t) = tex(3635) {
                p.spr(out, &t, 16, None, 36.666668, 376.23703);
            }
            if let Some(t) = tex(3641 + kind as i32) {
                p.spr(out, &t, 16, Some(title_src(n)), 80.0, 379.14);
            }
        }
        if title_a > 0.0 {
            if let Some(t) = tex(3640) {
                white.spr(&mut mask, &t, 16, Some(comp_src), 40.0, 362.55);
            }
            if let Some(t) = tex(3635) {
                white.spr(&mut mask, &t, 16, None, 36.666668, 376.24);
            }
            if let Some(t) = tex(3641 + kind as i32) {
                white.spr(&mut mask, &t, 16, Some(title_src(n)), 80.0, 379.14);
            }
            // off_3CC9E8: ▶'s frame; off_3CCA38: a slot's panel at the playing track's cell
            if let Some(t) = tex(3639) {
                white.spr(&mut mask, &t, 16, Some([242.0, 136.0, 454.0, 188.0]), 125.33, 404.03);
            }
            let s = &self.w.snd;
            if k / 21 == s.page
                && s.turning == 0
                && let Some(t) = tex(3636)
            {
                let (x, y) = ((k % 3) as f32 * 193.33 + 42.667, (k / 3 % 7) as f32 * 44.8 + 63.052);
                white.spr(&mut mask, &t, 16, Some([8.0, 8.0, 526.0, 74.0]), x, y);
            }
        }
        // the page arrows: lit while the cursor is on a tab, else grey
        let objs_a = self.w.snd.objs_a;
        if objs_a > 0.0 {
            let on = (5..=7).contains(&cur);
            let g = if on { 1.0 } else { 0.5 };
            let row = if on { 88.0 } else { 8.0 };
            let mut p = pen;
            p.color(objs_a, g, g, g);
            if let Some(t) = tex(3628) {
                p.spr(out, &t, 16, Some([8.0, row, 80.0, row + 72.0]), 464.0, 19.081);
                p.spr(out, &t, 16, Some([88.0, row, 160.0, row + 72.0]), 596.0, 19.081);
            }
            self.wsnd_objects();
            let tex = |id| self.w.tex.get(id);
            let p = Pen::default();
            self.w.objs.draw(&p, 21, out, &tex);
            self.w.objs.draw(&p, 22, out, &tex);
        }
        // the light: strokes of texture 242's middle row (half its width, 1080 times its height, centred) every
        // 320 units from x - 64 into surface 1, added through surface 0 at half the title's alpha (jp_B2900)
        let light_x = self.w.snd.light_x;
        if fade >= 1.0 && title_a > 0.0 {
            let [mask_rt, light_rt] = self.w.snd.rt.get_or_insert_with(|| {
                let mk = |l| r.create_target(W as u32, H as u32, l).tex;
                [mk("sound light mask"), mk("sound light")]
            });
            let (mask_rt, light_rt) = (mask_rt.clone(), light_rt.clone());
            for q in &mut mask {
                q.blend = Blend::Over;
            }
            let mut light = Vec::new();
            if let Some(t) = self.w.tex.get(242) {
                let mut p = Pen::default();
                p.scale(0.5, 1080.0);
                let mut x = light_x - 64.0;
                while x < 840.0 {
                    p.spr(&mut light, &t, 0x34, Some([0.0, 127.0, 255.0, 128.0]), x, 362.55);
                    x += 320.0;
                }
            }
            for q in &mut light {
                q.blend = Blend::Over;
            }
            let mut m = Quad::rect(mask_rt.clone(), [0.0, 0.0, W, H], [1.0; 4]);
            m.blend = Blend::MaskAlpha;
            light.push(m);
            game.ui_passes.push((RenderTarget { tex: mask_rt }, Some([0.0; 4]), mask));
            game.ui_passes.push((RenderTarget { tex: light_rt.clone() }, Some([0.0; 4]), light));
            let mut q = Quad::rect(light_rt, [0.0, 0.0, W, H], [1.0, 1.0, 1.0, title_a * 0.5]);
            q.shade = Shade::Fill;
            q.blend = Blend::Add;
            out.push(q);
        }
        // the cursor's pulse: (0.25, 0.75, 1) through its object's shape (surface 0, cleared), added at
        // fade x (sin + 1) / 2 x 0.6
        let s = &self.w.snd;
        let shape = match cur {
            0..=4 => Some((3639, [242.0, 136.0, 454.0, 188.0], cur as f32 * 83.333 + 42.0, 404.03)),
            5..=7 => Some((3627, [0.0, 0.0, 78.0, 72.0], (cur - 5) as f32 * 33.333 + 496.0, 19.081)),
            8.. if s.turning == 0 => {
                let c = cur - 8;
                Some((3636, [8.0, 8.0, 526.0, 74.0], (c % 3) as f32 * 193.33 + 42.667, (c / 3 % 7) as f32 * 44.8 + 63.052))
            }
            _ => None,
        };
        if let Some((id, src, x, y)) = shape
            && let Some(t) = self.w.tex.get(id)
        {
            let mut p = Pen::default();
            p.scale(0.33333, 0.41481);
            p.color(fade * ((s.phase.sin() + 1.0) * 0.5 * 0.6), 0.25, 0.75, 1.0);
            let n = out.len();
            p.spr(out, &t, 16, Some(src), x, y);
            for q in &mut out[n..] {
                q.shade = Shade::Fill;
                q.blend = Blend::Add;
            }
        }
    }
}
