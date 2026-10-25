//! The TOMP movie task (vtable 0x573798: init sub_402A0, update sub_40480, draw sub_40940, end sub_40A00).
//!
//! The movie is drawn opaque with a full screen cover over it at alpha 1 - `vis`, in black, or in white
//! for movies 5 and 37 (dword_AFFCA0; the overseas build tests the language's table entry, so its English and
//! Chinese op1_arc copies start under black). `frame` is the playback position * 60 while the movie plays and then
//! keeps counting frames, so a movie shorter than its fade-out start holds its last frame until then.

use knife_render::{CANVAS_H, CANVAS_W, Quad, Renderer};

use super::Game;


pub(super) struct StoryMovie {
    /// TOMP movie number (a1+24)
    n: i32,
    /// the watched flag sub_40A00 sets when the task ends (sub_923C0)
    flag: Option<u16>,
    /// B / Plus may skip it (v6: the flagged movies only once watched)
    skippable: bool,
    /// a1+48
    state: u8,
    /// a1+52
    frame: f32,
    /// a1+60
    vis: f32,
    /// movie 4: B / Plus was pressed before its skip check (a1+88)
    skip_req: bool,
    /// B / Plus pressed since the last update
    pressed: bool,
    /// cover colour (dword_AFFCA0)
    cover: f32,
    /// the game language (the overseas prologue times its skip check and fade-out for English)
    language: usize,
    /// Witch on the Holy Night's opening task (kind 0: init sub_21AD0, update sub_21BE0, no draw, end
    /// sub_21DE0)
    witch: bool,
}

impl StoryMovie {
    /// `entry` is the movie table entry played (`movie::localized`).
    pub(super) fn new(n: i32, entry: i32, language: usize, flag: Option<u16>, skippable: bool) -> Self {
        Self {
            n,
            flag,
            skippable,
            state: 0,
            frame: 0.0,
            vis: 0.0,
            skip_req: false,
            pressed: false,
            cover: if entry | 0x20 == 0x25 { 1.0 } else { 0.0 },
            language,
            witch: false,
        }
    }

    /// Witch on the Holy Night's opening task: no watched flag, no cover, and B / Plus always counts.
    pub(super) fn witch_opening(language: usize) -> Self {
        Self { witch: true, skippable: true, ..Self::new(-1, -1, language, None, true) }
    }

    pub(super) fn is_witch(&self) -> bool {
        self.witch
    }

    /// Fade-in rate, fade-out start frame and fade-out rate per frame (sub_40480 v7 / v5 / v4).
    fn rates(&self) -> (f32, f32, f32) {
        const SLOW: f32 = 0.020833;
        match self.n {
            // overseas: 23340 in English
            4 if self.language == 1 => (1.0, 23340.0, 1.0),
            4 => (1.0, 23040.0, 1.0),
            5 => (1.0, 6420.0, 1.0),
            6 => (1.0, 6300.0, 1.0),
            13 => (1.0, 540.0, 1.0),
            35 => (1.0, 18960.0, 1.0),
            36 => (1.0, 8880.0, 1.0),
            37 => (1.0, 15240.0, 1.0),
            7 => (SLOW, 312.0, SLOW),
            39..=45 => (1.0, 618.0, 1.0),
            _ => (SLOW, 420.0, 1.0),
        }
    }

    /// The movie's watched flag, set as the task ends.
    pub(super) fn flag(&self) -> Option<u16> {
        self.flag
    }

    /// B / Plus (mask 0x4008); `force` is the debug flag that lets every movie be skipped.
    pub(super) fn press(&mut self, force: bool) {
        self.pressed = true;
        self.skippable |= force;
    }

    /// The cover drawn over the movie (sub_40940).
    pub(super) fn cover_quad(&self, r: &Renderer) -> Option<Quad> {
        if self.witch {
            return None;
        }
        let a = 1.0 - self.vis;
        (a > 0.0).then(|| {
            let c = self.cover;
            Quad::rect(r.white.clone(), [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32], [c, c, c, a])
        })
    }
}

impl Game {
    /// One update of the TOMP movie task (sub_40480); `dt` in ms.
    pub(super) fn update_story_movie(&mut self, dt: f32) {
        let Some(mut t) = self.story_movie.take() else { return };
        let end = self.step_story_movie(&mut t, dt);
        t.pressed = false;
        self.story_movie = Some(t);
        if end {
            self.stop_movie();
        }
    }

    /// Returns true when the task ends (sub_3F4A0).
    fn step_story_movie(&mut self, t: &mut StoryMovie, dt: f32) -> bool {
        if t.witch {
            return self.step_witch_opening(t, dt);
        }
        let Some(m) = &self.movie else { return true };
        let (fade_in, out_at, fade_out) = t.rates();
        let frames = dt * 0.06;
        // sub_1AE0: the playback position, 0 unless the player runs; sub_19E0: the player is still open
        let running = m.has_frame() && !m.finished;
        let pos = if running { m.position() } else { 0.0 };
        let playing = !m.closed;
        let ready = m.ready();
        t.frame = if pos > 0.0 { (pos * 60.0).floor() } else { t.frame + frames };
        let pressed = t.pressed;
        loop {
            match t.state {
                0 | 1 => {
                    // sub_1A30: wait for the movie to be ready
                    if !ready {
                        return false;
                    }
                    t.frame = 0.0;
                    t.vis = 0.0;
                    t.skip_req = false;
                    // the phase title movies play system sound 7 as they start
                    if !(t.n <= 0x2D && (1u64 << t.n) & 0x3FF8000020FF != 0) {
                        self.play_sys_sound_7();
                    }
                    t.state = 2;
                }
                2 => {
                    t.vis = (t.vis + fade_in * frames).min(1.0);
                    if t.frame >= 60.0 {
                        t.state = 3;
                        if t.n == 5 {
                            t.cover = 0.0;
                        }
                    }
                    return false;
                }
                3 => {
                    match t.n {
                        13 => {
                            if t.frame < 210.0 {
                                return false;
                            }
                            self.play_sys_sound_7();
                        }
                        // the prologue: B / Plus is only noted until 6 minutes in (overseas English: 6 m 5 s)
                        4 if t.frame < if t.language == 1 { 21900.0 } else { 21600.0 } => {
                            if !(t.skippable && pressed) {
                                return false;
                            }
                            t.skip_req = true;
                        }
                        _ => {}
                    }
                    t.state = 4;
                }
                4 => {
                    if t.n == 13 && t.frame < 330.0 {
                        return false;
                    }
                    if t.n == 4 {
                        // sub_9C960(1); sub_9C470(2, 100, 0): BGM 2 at full volume at once
                        self.play_bgm(2, 1.0, 0.0);
                    }
                    t.state = 5;
                    return false;
                }
                5 | 6 => {
                    if t.state == 5 {
                        if playing {
                            if t.skippable && (t.skip_req || pressed) {
                                t.state = 7;
                                t.frame = out_at;
                                return false;
                            }
                        } else {
                            t.state = 6;
                        }
                    }
                    if t.frame >= out_at {
                        t.vis -= fade_out * frames;
                        if t.vis < 0.0 {
                            t.vis = 0.0;
                            return !playing;
                        }
                    }
                    return t.vis <= 0.0 && !playing;
                }
                7 => {
                    t.vis -= frames / 60.0;
                    if t.vis <= 0.0 {
                        t.vis = 0.0;
                        t.state = 8;
                        self.stop_movie_playback();
                    }
                    return false;
                }
                _ => {
                    t.state = 6;
                    return false;
                }
            }
        }
    }

    /// Witch on the Holy Night's sub_21BE0, counting frames (a1+52) from the movie's start: B / Plus is taken
    /// from frame 240; at frame 11249 (or once the movie stops, or after a press) SE01004 starts looping on
    /// channel 2; a press then fades the BGM out (jp_9C9B0(0.55556)) and jumps the movie to 193 s (sub_15B0),
    /// and the task ends once the movie has closed. Returns true when it ends (jp_3F4A0).
    fn step_witch_opening(&mut self, t: &mut StoryMovie, dt: f32) -> bool {
        let Some(m) = &self.movie else { return true };
        // sub_1A30: the player runs; jp_19E0: it is still open
        let (ready, playing) = (m.ready(), !m.closed);
        t.frame += dt * 0.06;
        let pressed = t.pressed;
        loop {
            match t.state {
                // jp_B4540, then on into state 1
                0 => t.state = 1,
                1 => {
                    if ready {
                        t.frame = 0.0;
                        t.skip_req = false;
                        t.state = 2;
                    }
                    return false;
                }
                2 => {
                    if t.frame >= 240.0 {
                        t.state = 3;
                    }
                    return false;
                }
                3 => {
                    if !playing || t.frame > 11249.0 {
                        t.state = 4;
                        continue;
                    }
                    if pressed {
                        t.skip_req = true;
                        t.state = 4;
                    }
                    return false;
                }
                4 => {
                    // jp_9CF30(2, 1); jp_9CBF0(2, 2, SE01004, 50, 0, 0.1): the player's own loop at 50, rising
                    // 0.1 points a frame (its nn::atk fade 1000 frames)
                    if let Some(pcm) = self.assets.sound_effect("SE01004") {
                        self.sound.play_se(2, "SE01004", pcm, 0.5, 1666.7 / 0.1 / 1000.0, true, 0.0);
                        self.sound.unlist_loop(2);
                    }
                    t.state = if playing { 5 } else { 6 };
                    return false;
                }
                5 => {
                    if !playing {
                        self.sound.stop_bgm_rate(0.55556);
                        t.state = 6;
                    } else if t.frame >= 11580.0 {
                        t.state = 6;
                    } else if t.skip_req || pressed {
                        self.sound.stop_bgm_rate(0.55556);
                        if let Some(m) = &mut self.movie {
                            m.seek(193.0);
                        }
                        self.sound.seek_movie_audio(193.0);
                        t.state = 6;
                    }
                    return false;
                }
                _ => return !playing,
            }
        }
    }

    /// sub_ACFA0(7): system sound 7 on the SE volume.
    fn play_sys_sound_7(&mut self) {
        self.play_se_sound(7);
    }
}
