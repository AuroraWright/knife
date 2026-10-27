//! Script-level audio: BGM (decoded in the background), sound effect channels and voices.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, mpsc},
};

use knife_audio::{Audio, Curve, Pcm, Voice};

/// One 60 Hz frame (ms). The app loop (sub_FC064) runs the game's frame, whose sound update (sub_9B7F0) comes
/// after the script, and then the sound manager's update: nn::atk's update (sub_106734, which flushes the frame's
/// commands and detaches finished sounds) and `nn::audio::RequestUpdateAudioRenderer`, once a frame.
const FRAME_MS: f64 = 1000.0 / 60.0;

/// How long after the first frame end that finds a sound finished the script sees it stopped. The renderer
/// update at that frame's end reports it, nn::atk's next update detaches the handle, the sound update after that
/// clears the state word (sub_EEAC4 / sub_EE07C), and the script reads it the frame after: three frames, so a
/// VCWT then SEPL starts the effect 55 / 75 ms after the voice's last sample on the Switch (Astris capture).
const ENDED_MS: f64 = 3.0 * FRAME_MS - 0.5;

/// A stream (BGM, voice) starts at nn::atk's update a frame after its command, once the loader has its first
/// blocks; an SE starts at the command's own frame end (voices measured 17.5 / 17.5 / 32.5 ms after SEs started
/// with them, a BGM 22.5 ms after the loops a load restarted with it). knife starts it at the first flush after
/// the command's frame once this much game time has passed: under a frame, so frame-time jitter at 60 Hz does not
/// add one, and a frame's worth on faster displays.
const STREAM_START_MS: f64 = FRAME_MS * 0.75;

/// Category volumes (off_AE0350 + 8 BGM, + 9 voices, + 10 SE, 0..100 from the config). The sound update
/// (sub_9B7F0) multiplies a player's volume by its category and maps the product through its `Curve`: BGM and
/// SE channels through sub_CA0 (BGM after easing 12), voices linearly.
pub struct Volumes {
    pub bgm: f32,
    pub se: f32,
    pub voice: f32,
    pub bgm_mute: bool,
    pub se_mute: bool,
    /// the movie player's volume (off_AE0350 + 7): Tsukihime's BGM copy, Witch on the Holy Night's own slider
    pub movie: Option<f32>,
}

impl Volumes {
    fn bgm_level(&self) -> f32 {
        if self.bgm_mute { 0.0 } else { self.bgm }
    }

    fn se_level(&self) -> f32 {
        if self.se_mute { 0.0 } else { self.se }
    }
}

impl Default for Volumes {
    fn default() -> Self {
        Self {
            // config defaults 40 / 40 (0x46B080), x 0.01 (sub_9B7F0)
            bgm: 0.4,
            se: 0.4,
            voice: 1.0,
            bgm_mute: false,
            se_mute: false,
            movie: None,
        }
    }
}

pub struct Bgm {
    id: i32,
    voice: Option<Voice>,
    /// script volume 0..1 (before category volume)
    volume: f32,
    /// started on a prefix of the stream: the request generation whose whole decode is still to come
    partial: Option<u64>,
}

/// A background BGM decode result: (request generation, track, PCM).
type BgmLoaded = (u64, i32, anyhow::Result<Loaded>);

enum Loaded {
    /// the stream from its start position on (source frame of the first), enough to start on
    Prefix(usize, Pcm),
    Whole(Pcm),
}

/// A BGM play (a stopped one is dropped); it starts once `pcm` is there and a frame has passed.
pub struct PendingBgm {
    id: i32,
    generation: u64,
    volume: f32,
    fade_in: f64,
    start: usize,
    /// the sound clock and the flush count at the command
    at: (f64, u64),
    /// the PCM to start on and, for a prefix, its first source frame
    pcm: Option<(Arc<Pcm>, Option<usize>)>,
}

/// A scene voice player (sound players 2 / 3, sub_9B520): its state word and sound.
#[derive(Default)]
struct VoicePlayer {
    /// the state word: set by a play, cleared by a stop or once the sound's end reaches it (`ENDED_MS`)
    on: bool,
    /// a play waiting for its stream to start: (sound clock and flush count at the command, source, volume)
    pending: Option<((f64, u64), Arc<Pcm>, f32)>,
    voice: Option<Voice>,
    /// the sound clock of the first frame end that found the sound finished
    end: Option<f64>,
}

impl VoicePlayer {
    fn stop(&mut self) {
        self.pending = None;
        if let Some(v) = self.voice.take() {
            v.stop(0.0);
        }
        self.on = false;
        self.end = None;
    }
}

/// An SE channel's player (sound player 11 + ch, off_AE0BE8 + 60 ch).
struct SeChan {
    voice: Voice,
    /// the speaker row the channel was bound to when it played at once (SEPL key 70, type 2): that row's voice
    /// volume replaces the SE category
    speaker: Option<usize>,
    /// state 2: fading out (sub_9D020), stopped when its volume comes down to 0
    fading: bool,
    /// the player's pan ramp (+28 current, +32 target, +36 rate a frame, x 1000): sub_9D110 starts it,
    /// sub_9B7F0 steps it
    pan: (f32, f32, f32),
}

/// A player's volume rate, points (0..100) a frame, in volume (0..1) a second; sub_9B7F0 steps a rate of 0 or
/// less at 0.2.
fn se_rate(per_frame: f32) -> f32 {
    (if per_frame > 0.0 { per_frame } else { 0.2 }) * 0.6
}

pub struct Sound {
    audio: Audio,
    pub vol: Volumes,
    bgm: Option<Bgm>,
    /// SE channels (sound players 11..30)
    se: HashMap<i32, SeChan>,
    /// each channel's volume word (byte_146E640 + 140): SEPL / SEVL set it to their volume, SEFD to 0,
    /// sub_5F200 to -1, a reset (sub_57240 / sub_82330) to 100; SESP leaves it. Fades take their rates from it.
    pub se_word: [i16; 20],
    /// the BGM volume word (word_146E650; Witch on the Holy Night's jd_146E650 is 255 x percent / 100): every MPLY
    /// of a track and MVOL set it, MPLY(-1) / MFAD / MSTP leave it, a load restores it; 0 from boot. The record
    /// keeps it (+36 / WoH +13276), with or without a track
    pub bgm_word: i16,
    /// each channel's loop flag word (byte_146E640 + 180): SEPL (sub_6A340, queued sub_65780) sets it from its
    /// loop argument; SEFD and an empty SEPL clear the loop id (+100) but leave it; a reset (sub_57240) and the
    /// script end (sub_5F200) clear it. The record keeps it (+14489 / WoH +13303)
    pub se_loop: [bool; 20],
    /// each channel's pan word (byte_146E640 + 220, -1000..1000): SEPL's key 18, what a load restarts a loop with
    pub se_pan: [i16; 20],
    /// each speaker row's voice volume, 0 while voices are off or the row is muted (`set_speaker_gains`)
    speaker_gain: Vec<f32>,
    /// the speaker row each channel is bound to (sub_9CF70, channel struct + 52): SEPL sets it, key 70 or none,
    /// and it stays after the sound ends, so the config SE test (sub_9CAA0 alone) plays on it
    chan_speaker: [Option<usize>; 20],
    /// the sound clock of the first frame end that found each channel's sound finished (sub_9CD30's state word
    /// clears `ENDED_MS` later)
    se_end: HashMap<i32, f64>,
    /// looping effects by channel (word100, with the loop flag word180): what a load or the seek's end restarts
    loops: HashMap<i32, String>,
    /// sound players 2 / 3: each scene voice plays on the other one (sub_9B570), so a VPL2 does not cut the
    /// last voice; the current one (dword_1536850) is the one VCWT and the voice waits read
    voices: [VoicePlayer; 2],
    cur_voice: usize,
    /// ms of game frames, advanced once a frame (`begin_frame`)
    clock: f64,
    /// `flush` calls so far: a stream starts at a flush after the one that took its command's frame
    flushes: u64,
    /// sound player 4: the config's voice test (sub_2B930), apart from the scene voices on players 2 / 3
    test_voice: Option<Voice>,
    movie: Option<Voice>,
    /// the movie's audio track, for a seek
    movie_pcm: Option<Arc<Pcm>>,
    /// sound players 4..7 for system sounds; sub_ACEF0 and sub_ACFA0 each cycle them with their own counter
    /// (dword_18C04E8 / dword_18C04EC), stopping what the player was playing
    sys: [Option<Voice>; 4],
    sys_next: [usize; 2],
    bgm_cache: HashMap<i32, Arc<Pcm>>,
    tx: mpsc::Sender<BgmLoaded>,
    rx: mpsc::Receiver<BgmLoaded>,
    pending: Option<PendingBgm>,
    generation: u64,
}

impl Sound {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            audio: Audio::new(),
            vol: Volumes::default(),
            bgm: None,
            se: HashMap::new(),
            se_word: [100; 20],
            bgm_word: 0,
            se_loop: [false; 20],
            se_pan: [0; 20],
            speaker_gain: Vec::new(),
            chan_speaker: [None; 20],
            se_end: HashMap::new(),
            loops: HashMap::new(),
            voices: Default::default(),
            cur_voice: 0,
            clock: 0.0,
            flushes: 0,
            test_voice: None,
            movie: None,
            movie_pcm: None,
            sys: Default::default(),
            sys_next: [0; 2],
            bgm_cache: HashMap::new(),
            tx,
            rx,
            pending: None,
            generation: 0,
        }
    }

    /// `MPLY`: play BGM `id` (volume 0..1, fade in seconds) from source frame `start`: the Nintendo Opus stream
    /// at `path`, looping over `loop_range` (the whole stream without one). A track not in the cache decodes in
    /// the background, a second from `start` first so that it can start in time.
    pub fn play_bgm(
        &mut self,
        id: i32,
        volume: f32,
        fade_in: f64,
        start: usize,
        path: PathBuf,
        loop_range: Option<(usize, usize)>,
    ) {
        if let Some(b) = &self.bgm
            && b.id == id
            && b.voice.as_ref().is_some_and(|v| !v.finished())
        {
            self.set_bgm_volume(volume, fade_in);
            return;
        }
        if self.pending.as_ref().is_some_and(|p| p.id == id) {
            self.set_bgm_volume(volume, fade_in);
            return;
        }
        self.stop_bgm(0.0);
        self.generation += 1;
        let cached = self.bgm_cache.get(&id).cloned();
        let generation = self.generation;
        if cached.is_none() {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let res = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|data| {
                    let (base, prefix) = knife_audio::codec::decode_nopus_part(&data, start, start + 48_000)?;
                    let _ = tx.send((generation, id, Ok(Loaded::Prefix(base, prefix))));
                    let mut pcm = knife_audio::codec::decode_nopus(&data)?;
                    let len = pcm.frames.len();
                    pcm.loop_range = Some(loop_range.map_or((0, len), |(a, b)| (a.min(len), b.min(len))));
                    Ok(Loaded::Whole(pcm))
                });
                let _ = tx.send((generation, id, res));
            });
        }
        self.pending = Some(PendingBgm {
            id,
            generation,
            volume,
            fade_in,
            start,
            at: (self.clock, self.flushes),
            pcm: cached.map(|p| (p, None)),
        });
    }

    fn start_bgm(&mut self, p: PendingBgm) {
        let Some((pcm, base)) = p.pcm else { return };
        let vol = p.volume * self.vol.bgm_level();
        let v = match base {
            Some(base) => self.audio.play_prefix(pcm, base, vol, p.fade_in, true, p.start, Curve::Bgm),
            None => self.audio.play_at(pcm, vol, p.fade_in, true, p.start, Curve::Bgm),
        };
        self.bgm = Some(Bgm {
            id: p.id,
            voice: Some(v),
            volume: p.volume,
            partial: base.map(|_| p.generation),
        });
    }

    /// Once a frame before the game's: the sound clock.
    pub fn begin_frame(&mut self, dt: f32) {
        self.clock += f64::from(dt);
    }

    /// Take in finished background decodes.
    pub fn update(&mut self) {
        while let Ok((generation, id, res)) = self.rx.try_recv() {
            match res {
                Ok(Loaded::Prefix(base, pcm)) => {
                    if let Some(p) = self.pending.as_mut().filter(|p| p.generation == generation && p.pcm.is_none()) {
                        p.pcm = Some((Arc::new(pcm), Some(base)));
                    }
                }
                Ok(Loaded::Whole(pcm)) => {
                    let pcm = Arc::new(pcm);
                    if self.bgm_cache.len() >= 4 {
                        self.bgm_cache.clear();
                    }
                    self.bgm_cache.insert(id, pcm.clone());
                    if let Some(p) = self.pending.as_mut().filter(|p| p.generation == generation) {
                        p.pcm = Some((pcm, None));
                    } else if let Some(b) = self.bgm.as_mut().filter(|b| b.partial == Some(generation)) {
                        if let Some(v) = &b.voice {
                            v.replace_source(pcm);
                        }
                        b.partial = None;
                    }
                }
                Err(e) => {
                    tracing::warn!("bgm {id}: {e:#}");
                    self.pending.take_if(|p| p.generation == generation);
                }
            }
        }
    }

    /// Once a frame after the game's (nn::atk's update): streams a frame old start, and finished sounds are
    /// noted for the state words.
    /// Once a frame after the game's and the UI's update, before the present: this frame's sound commands
    /// reach the mixer together (`Audio::flush`).
    pub fn flush(&mut self) {
        self.audio.flush();
        self.flushes += 1;
    }

    pub fn end_frame(&mut self) {
        let (clock, flushes) = (self.clock, self.flushes);
        // a stream command's frame has been flushed (a load from the UI can run before or after the game's
        // update) and a frame's time has passed
        let due = |(at, flush): (f64, u64)| clock - at >= STREAM_START_MS && flushes > flush;
        if self.pending.as_ref().is_some_and(|p| p.pcm.is_some() && due(p.at)) {
            let p = self.pending.take().unwrap();
            self.start_bgm(p);
        }
        for pl in &mut self.voices {
            if let Some((_, pcm, vol)) = pl.pending.take_if(|p| due(p.0)) {
                pl.voice = Some(self.audio.play(pcm, vol, 0.0, false));
            }
            if pl.on && pl.end.is_none() && pl.pending.is_none() && pl.voice.as_ref().is_none_or(Voice::finished) {
                pl.end = Some(clock);
            }
        }
        for (ch, c) in &self.se {
            if c.voice.finished() {
                self.se_end.entry(*ch).or_insert(clock);
            }
        }
        let ended: Vec<i32> = self.se_end.iter().filter(|(_, t)| clock - **t >= ENDED_MS).map(|(c, _)| *c).collect();
        for ch in ended {
            self.se_end.remove(&ch);
            self.se.remove(&ch);
        }
    }

    /// sub_9C460: the BGM playing or loading (-1 after a stop).
    pub fn bgm_id(&self) -> i32 {
        self.pending.as_ref().map(|p| p.id).or(self.bgm.as_ref().map(|b| b.id)).unwrap_or(-1)
    }

    pub fn stop_bgm(&mut self, fade: f64) {
        self.pending = None;
        if let Some(b) = self.bgm.take()
            && let Some(v) = b.voice
        {
            v.stop(fade);
        }
    }

    /// sub_9C9B0(rate): the exe fades the BGM out by `rate` volume points (of 100) per 60 Hz frame, so the
    /// time depends on the volume it plays at (MPLY's fade gives 1666.7 / ms, i.e. ms at full volume).
    pub fn stop_bgm_rate(&mut self, rate: f32) {
        let vol = self.bgm.as_ref().map_or(1.0, |b| b.volume);
        self.stop_bgm(f64::from(vol * 100.0 / (rate * 60.0)));
    }

    /// Take the playing BGM and a play still to start out (with `put_bgm`, a stop that leaves them alone).
    pub fn take_bgm(&mut self) -> (Option<Bgm>, Option<PendingBgm>) {
        (self.bgm.take(), self.pending.take())
    }

    pub fn put_bgm(&mut self, b: (Option<Bgm>, Option<PendingBgm>)) {
        if b.0.is_some() {
            self.bgm = b.0;
        }
        if b.1.is_some() {
            self.pending = b.1;
        }
    }

    /// Music player pause: holds the BGM at its position.
    pub fn set_bgm_paused(&mut self, paused: bool) {
        if let Some(v) = self.bgm.as_ref().and_then(|b| b.voice.as_ref()) {
            v.set_paused(paused);
        }
    }

    pub fn set_bgm_volume(&mut self, volume: f32, secs: f64) {
        let c = self.vol.bgm_level();
        if let Some(p) = &mut self.pending {
            p.volume = volume;
        }
        if let Some(b) = &mut self.bgm {
            b.volume = volume;
            if let Some(v) = &b.voice {
                v.set_volume(volume * c, secs);
            }
        }
    }

    /// The category gain and curve of an SE channel: the SE volume through sub_CA0, or for a channel bound to
    /// a speaker (SEPL key 70, type 2) that speaker's voice volume, linear (sub_9CD40 / sub_9B7F0).
    fn se_mix(&self, speaker: Option<usize>) -> (f32, Curve) {
        match speaker {
            Some(r) => (self.speaker_gain.get(r).copied().unwrap_or(0.0), Curve::Linear),
            None => (self.vol.se_level(), Curve::Se),
        }
    }

    /// Voice volume per speaker row (0 for a muted row or with voices off), for speaker-bound SE channels.
    pub fn set_speaker_gains(&mut self, gains: Vec<f32>) {
        self.speaker_gain = gains;
    }

    /// The speaker row SE channel `ch` is bound to (byte_146E640 + 80).
    pub fn chan_speaker(&self, ch: i32) -> Option<usize> {
        usize::try_from(ch).ok().and_then(|c| self.chan_speaker.get(c).copied().flatten())
    }

    /// sub_9CF70: bind SE channel `ch` to a speaker row (SEPL key 70) or to none.
    pub fn bind_speaker(&mut self, ch: i32, speaker: Option<usize>) {
        if let Some(s) = usize::try_from(ch).ok().and_then(|c| self.chan_speaker.get_mut(c)) {
            *s = speaker;
        }
    }

    /// Play on SE channel `ch` (sub_9CAA0 / sub_9CBF0): what the channel's player held stops at once, fading
    /// or not (sub_EDD28 stops the handle). `volume` 0..1, `fade_in` seconds from 0, `pan` -1..1.
    #[allow(clippy::too_many_arguments)]
    pub fn play_se(&mut self, ch: i32, name: &str, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool, pan: f32) {
        // only an immediate play (sub_9CAA0) makes a bound channel type 2; a fade-in (sub_9CBF0) leaves it type 1,
        // at the SE category through its curve (only its target is 0 for a muted row)
        let speaker = usize::try_from(ch).ok().and_then(|c| self.chan_speaker.get(c).copied().flatten()).filter(|_| fade_in <= 0.0);
        if let Some(c) = self.se.remove(&ch) {
            c.voice.stop(0.0);
        }
        self.se_end.remove(&ch);
        if looping {
            self.loops.insert(ch, name.to_string());
        } else {
            self.loops.remove(&ch);
        }
        let (k, curve) = self.se_mix(speaker);
        let voice = self.audio.play_gain(pcm, (volume.max(0.0), k, pan), fade_in, looping, curve);
        self.se.insert(ch, SeChan { voice, speaker, fading: false, pan: (0.0, 0.0, 0.0) });
    }

    /// Stop SE channel `ch` (all for a negative one) at once (sub_9CF90) and forget its loop.
    pub fn stop_se(&mut self, ch: i32) {
        if ch < 0 {
            self.loops.clear();
            for (_, c) in self.se.drain() {
                c.voice.stop(0.0);
            }
            self.se_end.clear();
        } else {
            self.loops.remove(&ch);
            if let Some(c) = self.se.remove(&ch) {
                c.voice.stop(0.0);
            }
            self.se_end.remove(&ch);
        }
    }

    /// Forget channel `ch`'s loop (word100 = -1) without touching its sound.
    pub fn forget_loop(&mut self, ch: i32) {
        self.loops.remove(&ch);
    }

    /// sub_9CFC0: the channel player's state, 0 idle, 1 playing, 2 fading out.
    pub fn se_state(&self, ch: i32) -> u8 {
        match self.se.get(&ch) {
            Some(_) if !self.se_busy(ch) => 0,
            // sub_9B7F0 idles a fading player in the update that brings it to 0
            Some(c) if c.fading && c.voice.volume() <= 0.0 => 0,
            Some(c) if c.fading => 2,
            Some(_) => 1,
            None => 0,
        }
    }

    /// The channel player's current volume (0..1 before its category), 0 when idle.
    pub fn se_player_volume(&self, ch: i32) -> f32 {
        if self.se_state(ch) == 0 {
            return 0.0;
        }
        self.se.get(&ch).map_or(0.0, |c| c.voice.volume())
    }

    /// sub_9D020 / sub_9CFE0: an active player starts fading out (state 2) at `per_frame` volume points a frame,
    /// stopping once at 0.
    pub fn fade_se(&mut self, ch: i32, per_frame: f32) {
        if self.se_state(ch) == 0 {
            return;
        }
        if let Some(c) = self.se.get_mut(&ch) {
            c.fading = true;
            c.voice.fade_out(se_rate(per_frame));
        }
    }

    /// sub_9D0B0: an active player's volume moves to `volume` (0..1) at `per_frame` points a frame (its
    /// magnitude), at once from 1666.7 up; a fading player keeps fading (it stops only at 0).
    pub fn ramp_se(&mut self, ch: i32, volume: f32, per_frame: f32) {
        if self.se_state(ch) == 0 {
            return;
        }
        if let Some(c) = self.se.get(&ch) {
            let r = per_frame.abs();
            c.voice.ramp_to(volume, if r >= 1666.7 { 0.0 } else { se_rate(r) });
        }
    }

    /// sub_9CE60: a playing (not fading) player's pan becomes `pan` (-1000..1000) at once, its ramp cleared.
    pub fn set_se_pan(&mut self, ch: i32, pan: i32) {
        if self.se_state(ch) != 1 {
            return;
        }
        if let Some(c) = self.se.get_mut(&ch) {
            c.pan = (0.0, 0.0, 0.0);
            c.voice.set_pan(pan as f32 * 0.001);
        }
    }

    /// sub_9D110: an active player's pan moves from `from` to `to` (-1000..1000) by `per_frame`.
    pub fn ramp_se_pan(&mut self, ch: i32, from: i32, to: i32, per_frame: f32) {
        if self.se_state(ch) == 0 {
            return;
        }
        if let Some(c) = self.se.get_mut(&ch) {
            c.pan = (from as f32, to as f32, per_frame);
        }
    }

    /// sub_9B7F0's pan step for `frames` frames: the current pan moves by the rate until it passes the target,
    /// which ends the ramp.
    pub fn step_se_pans(&mut self, frames: f32) {
        for c in self.se.values_mut() {
            let (cur, to, rate) = &mut c.pan;
            if *rate == 0.0 {
                continue;
            }
            *cur += *rate * frames;
            if (*rate >= 0.0 && *cur >= *to) || (*rate < 0.0 && *cur <= *to) {
                *cur = *to;
                *rate = 0.0;
            }
            c.voice.set_pan(*cur * 0.001);
        }
    }

    /// The looping SE on channel `ch` (word100 with word180 set).
    pub fn looping_on(&self, ch: i32) -> Option<&str> {
        self.loops.get(&ch).map(String::as_str)
    }

    /// A loop played straight on the channel's player, outside the script's loop id word (+100): nothing saves
    /// or restarts it.
    pub fn unlist_loop(&mut self, ch: i32) {
        self.loops.remove(&ch);
    }

    /// The looping effects (channel, name, volume word / 100), what a save restarts.
    pub fn looping_effects(&self) -> Vec<(i32, String, f32)> {
        self.loops
            .iter()
            .map(|(c, n)| (*c, n.clone(), f32::from(self.se_word.get(*c as usize).copied().unwrap_or(100)) / 100.0))
            .collect()
    }

    pub fn bgm_state(&self) -> Option<(i32, f32)> {
        self.bgm
            .as_ref()
            .map(|b| (b.id, b.volume))
            .or(self.pending.as_ref().map(|p| (p.id, p.volume)))
    }

    /// Mute everything while the app is in the background (no frames run there), or bring the sound back.
    pub fn set_muted(&mut self, muted: bool) {
        self.audio.set_muted(muted);
    }

    /// sub_5F200 / sub_5EF10: the BGM (sub_9C650: 416.67 points a frame) and every SE channel (sub_9CF90)
    /// stop at once.
    pub fn stop_all(&mut self) {
        self.stop_bgm(0.0);
        self.stop_se(-1);
        self.stop_voice();
        self.stop_movie_audio();
    }

    /// A system sound on the next sound player of counter `k` (0 sub_ACEF0, 1 sub_ACFA0) at `volume`
    /// (already scaled by its category).
    pub fn play_sys(&mut self, k: usize, pcm: Arc<Pcm>, volume: f32) {
        let slot = self.sys_next[k];
        self.sys_next[k] = (slot + 1) % 4;
        if let Some(v) = self.sys[slot].take() {
            v.stop(0.0);
        }
        self.sys[slot] = Some(self.audio.play(pcm, volume, 0.0, false));
    }

    /// The SE volume (`se_level`), for sub_ACFA0.
    pub fn se_volume(&self) -> f32 {
        self.vol.se_level()
    }

    /// sub_9C300: a voice at `gain` times the voice volume (the speaker's config row volume) on the other voice
    /// player, which drops what it played (sub_EE6B4); `stop_all` first stops both (sub_9C3B0), as a message
    /// voice and the replay do and VPL2 does not.
    pub fn play_voice_gain(&mut self, pcm: Arc<Pcm>, gain: f32, stop_all: bool) {
        if stop_all {
            self.stop_voice();
        }
        self.cur_voice ^= 1;
        let pl = &mut self.voices[self.cur_voice];
        pl.stop();
        pl.on = true;
        pl.pending = Some(((self.clock, self.flushes), pcm, gain * self.vol.voice));
    }

    /// The config voice test on player 4 (sub_9B740(4) stops the last one, sub_EE6B4 plays without a fade).
    pub fn play_test_voice(&mut self, pcm: Arc<Pcm>, gain: f32) {
        self.stop_test_voice();
        self.test_voice = Some(self.audio.play(pcm, gain * self.vol.voice, 0.0, false));
    }

    /// sub_9B740(4)
    pub fn stop_test_voice(&mut self) {
        if let Some(v) = self.test_voice.take() {
            v.stop(0.0);
        }
    }

    /// sub_9CD30: SE channel `ch`'s state word.
    pub fn se_busy(&self, ch: i32) -> bool {
        self.se.contains_key(&ch) && self.se_end.get(&ch).is_none_or(|t| self.clock - t < ENDED_MS)
    }

    /// sub_9C3B0: both voice players stop at once (sub_EE9A8: `Stop(0)`, cut at the next renderer frame) and
    /// their state words clear.
    pub fn stop_voice(&mut self) {
        for pl in &mut self.voices {
            pl.stop();
        }
    }

    pub fn play_movie_audio(&mut self, pcm: Arc<Pcm>) {
        self.stop_movie_audio();
        // the movie player takes off_AE0350 + 7 as is: the BGM copy, Witch on the Holy Night's movie slider
        let vol = self.vol.movie.unwrap_or_else(|| self.vol.bgm_level());
        self.movie = Some(self.audio.play(pcm.clone(), vol, 0.0, false));
        self.movie_pcm = Some(pcm);
    }

    /// The movie's audio from `secs` on (a movie seek).
    pub fn seek_movie_audio(&mut self, secs: f32) {
        let Some(pcm) = self.movie_pcm.clone() else { return };
        if let Some(v) = self.movie.take() {
            v.stop(0.0);
        }
        let vol = self.vol.movie.unwrap_or_else(|| self.vol.bgm_level());
        let start = (f64::from(secs) * f64::from(pcm.sample_rate)) as usize;
        if start < pcm.frames.len() {
            self.movie = Some(self.audio.play_at(pcm, vol, 0.0, false, start, Curve::Linear));
        }
    }

    pub fn stop_movie_audio(&mut self) {
        self.movie_pcm = None;
        if let Some(v) = self.movie.take() {
            v.stop(0.1);
        }
    }

    /// sub_9C400: the current voice player's state word.
    pub fn voice_playing(&self) -> bool {
        let pl = &self.voices[self.cur_voice];
        pl.on && pl.end.is_none_or(|t| self.clock - t < ENDED_MS)
    }

    /// Re-apply category volumes after a settings change.
    pub fn apply_volumes(&mut self) {
        if let Some(b) = &self.bgm
            && let Some(v) = &b.voice
        {
            v.set_volume(b.volume * self.vol.bgm_level(), 0.1);
        }
        // the category multiplies the player's volume each frame (sub_9B7F0), outside its fades
        for c in self.se.values() {
            c.voice.set_gain(self.se_mix(c.speaker).0);
        }
    }
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}
