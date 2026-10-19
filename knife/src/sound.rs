//! Script-level audio: BGM (decoded in the background), sound effect channels and voices.

use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
};

use knife_audio::{Audio, Pcm, Voice};

pub struct Volumes {
    pub master: f32,
    pub bgm: f32,
    pub se: f32,
    pub voice: f32,
    pub bgm_mute: bool,
    pub se_mute: bool,
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
            master: 1.0,
            // config defaults 40 / 40 (0x46B080), linear x 0.01 (sub_9B7F0)
            bgm: 0.4,
            se: 0.4,
            voice: 1.0,
            bgm_mute: false,
            se_mute: false,
        }
    }
}

struct Bgm {
    id: i32,
    voice: Option<Voice>,
    /// script volume 0..1 (before category volume)
    volume: f32,
    base: f32,
}

struct PendingBgm {
    id: i32,
    generation: u64,
    volume: f32,
    fade_in: f64,
}

pub struct Sound {
    audio: Audio,
    pub vol: Volumes,
    bgm: Option<Bgm>,
    se: HashMap<i32, (Voice, f32)>,
    /// looping effects by channel (name, volume) so saves can restore them
    loops: HashMap<i32, (String, f32)>,
    voice: Option<Voice>,
    movie: Option<Voice>,
    /// sound players 4..7 for system sounds; sub_ACEF0 and sub_ACFA0 each cycle them with their own counter
    /// (dword_18C04E8 / dword_18C04EC), stopping what the player was playing
    sys: [Option<Voice>; 4],
    sys_next: [usize; 2],
    bgm_cache: HashMap<i32, (Arc<Pcm>, f32)>,
    tx: mpsc::Sender<(u64, i32, anyhow::Result<(Pcm, f32)>)>,
    rx: mpsc::Receiver<(u64, i32, anyhow::Result<(Pcm, f32)>)>,
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
            loops: HashMap::new(),
            voice: None,
            movie: None,
            sys: Default::default(),
            sys_next: [0; 2],
            bgm_cache: HashMap::new(),
            tx,
            rx,
            pending: None,
            generation: 0,
        }
    }

    /// `MPLY`: play BGM `id` (volume 0..1, fade in seconds). `loader` decodes in the background.
    pub fn play_bgm(
        &mut self,
        id: i32,
        volume: f32,
        fade_in: f64,
        loader: impl FnOnce() -> anyhow::Result<(Pcm, f32)> + Send + 'static,
    ) {
        if let Some(b) = &self.bgm
            && b.id == id
            && b.voice.as_ref().is_some_and(|v| !v.finished())
        {
            self.set_bgm_volume(volume, fade_in);
            return;
        }
        self.stop_bgm(0.0);
        self.generation += 1;
        if let Some((pcm, base)) = self.bgm_cache.get(&id).cloned() {
            self.start_bgm(id, pcm, base, volume, fade_in);
            return;
        }
        self.pending = Some(PendingBgm {
            id,
            generation: self.generation,
            volume,
            fade_in,
        });
        let tx = self.tx.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let _ = tx.send((generation, id, loader()));
        });
    }

    fn start_bgm(&mut self, id: i32, pcm: Arc<Pcm>, base: f32, volume: f32, fade_in: f64) {
        let v = self.audio.play(pcm, volume * base * self.vol.bgm_level() * self.vol.master, fade_in, true);
        self.bgm = Some(Bgm {
            id,
            voice: Some(v),
            volume,
            base,
        });
    }

    /// Call once per frame.
    pub fn update(&mut self) {
        while let Ok((generation, id, res)) = self.rx.try_recv() {
            match res {
                Ok((pcm, base)) => {
                    let pcm = Arc::new(pcm);
                    if self.bgm_cache.len() >= 4 {
                        self.bgm_cache.clear();
                    }
                    self.bgm_cache.insert(id, (pcm.clone(), base));
                    if let Some(p) = self.pending.take_if(|p| p.generation == generation) {
                        self.start_bgm(id, pcm, base, p.volume, p.fade_in);
                    }
                }
                Err(e) => {
                    tracing::warn!("bgm {id}: {e:#}");
                    self.pending.take_if(|p| p.generation == generation);
                }
            }
        }
        self.se.retain(|_, (v, _)| !v.finished());
    }

    pub fn stop_bgm(&mut self, fade: f64) {
        self.pending = None;
        if let Some(b) = self.bgm.take()
            && let Some(v) = b.voice
        {
            v.stop(fade);
        }
    }

    /// Music player pause: holds the BGM at its position.
    pub fn set_bgm_paused(&mut self, paused: bool) {
        if let Some(v) = self.bgm.as_ref().and_then(|b| b.voice.as_ref()) {
            v.set_paused(paused);
        }
    }

    pub fn set_bgm_volume(&mut self, volume: f32, secs: f64) {
        let (m, c) = (self.vol.master, self.vol.bgm_level());
        if let Some(p) = &mut self.pending {
            p.volume = volume;
        }
        if let Some(b) = &mut self.bgm {
            b.volume = volume;
            if let Some(v) = &b.voice {
                v.set_volume(volume * b.base * c * m, secs);
            }
        }
    }

    pub fn play_se(&mut self, ch: i32, name: &str, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool) {
        if let Some((v, _)) = self.se.remove(&ch) {
            v.stop(0.0);
        }
        if looping {
            self.loops.insert(ch, (name.to_string(), volume));
        } else {
            self.loops.remove(&ch);
        }
        let v = self.audio.play(pcm, volume * self.vol.se_level() * self.vol.master, fade_in, looping);
        self.se.insert(ch, (v, volume));
    }

    pub fn stop_se(&mut self, ch: i32, fade: f64) {
        if ch < 0 {
            self.loops.clear();
        } else {
            self.loops.remove(&ch);
        }
        if ch < 0 {
            for (_, (v, _)) in self.se.drain() {
                v.stop(fade);
            }
        } else if let Some((v, _)) = self.se.remove(&ch) {
            v.stop(fade);
        }
    }

    pub fn looping_effects(&self) -> Vec<(i32, String, f32)> {
        self.loops.iter().map(|(c, (n, v))| (*c, n.clone(), *v)).collect()
    }

    pub fn bgm_state(&self) -> Option<(i32, f32)> {
        self.bgm
            .as_ref()
            .map(|b| (b.id, b.volume))
            .or(self.pending.as_ref().map(|p| (p.id, p.volume)))
    }

    pub fn stop_all(&mut self) {
        self.stop_bgm(0.3);
        self.stop_se(-1, 0.3);
        self.stop_voice();
        self.stop_movie_audio();
    }

    pub fn set_se_volume(&mut self, ch: i32, volume: f32, secs: f64) {
        let k = self.vol.se_level() * self.vol.master;
        for (c, (n, vol)) in self.loops.iter_mut() {
            let _ = n;
            if ch < 0 || *c == ch {
                *vol = volume;
            }
        }
        for (c, (v, vol)) in self.se.iter_mut() {
            if ch < 0 || *c == ch {
                *vol = volume;
                v.set_volume(volume * k, secs);
            }
        }
    }

    pub fn se_playing(&self, ch: i32) -> bool {
        if ch < 0 {
            self.se.values().any(|(v, _)| !v.finished())
        } else {
            self.se.get(&ch).is_some_and(|(v, _)| !v.finished())
        }
    }

    /// A system sound on the next sound player of counter `k` (0 sub_ACEF0, 1 sub_ACFA0) at `volume`
    /// (already scaled by its category).
    pub fn play_sys(&mut self, k: usize, pcm: Arc<Pcm>, volume: f32) {
        let slot = self.sys_next[k];
        self.sys_next[k] = (slot + 1) % 4;
        if let Some(v) = self.sys[slot].take() {
            v.stop(0.0);
        }
        self.sys[slot] = Some(self.audio.play(pcm, volume * self.vol.master, 0.0, false));
    }

    /// The SE volume (`se_level`), for sub_ACFA0.
    pub fn se_volume(&self) -> f32 {
        self.vol.se_level()
    }

    /// A voice at `gain` times the voice volume (the speaker's config row volume).
    pub fn play_voice_gain(&mut self, pcm: Arc<Pcm>, gain: f32) {
        self.stop_voice();
        self.voice = Some(self.audio.play(pcm, gain * self.vol.voice * self.vol.master, 0.0, false));
    }

    pub fn stop_voice(&mut self) {
        if let Some(v) = self.voice.take() {
            v.stop(0.05);
        }
    }

    pub fn play_movie_audio(&mut self, pcm: Arc<Pcm>) {
        self.stop_movie_audio();
        self.movie = Some(self.audio.play(pcm, self.vol.bgm_level() * self.vol.master, 0.0, false));
    }

    pub fn stop_movie_audio(&mut self) {
        if let Some(v) = self.movie.take() {
            v.stop(0.1);
        }
    }

    pub fn voice_playing(&self) -> bool {
        self.voice.as_ref().is_some_and(|v| !v.finished())
    }

    /// Re-apply category volumes after a settings change.
    pub fn apply_volumes(&mut self) {
        let m = self.vol.master;
        if let Some(b) = &self.bgm
            && let Some(v) = &b.voice
        {
            v.set_volume(b.volume * b.base * self.vol.bgm_level() * m, 0.1);
        }
        for (v, vol) in self.se.values() {
            v.set_volume(vol * self.vol.se_level() * m, 0.1);
        }
    }
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}
