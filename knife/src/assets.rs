//! Game data access: archives, exe tables, scripts, sound archive and a GPU texture cache.

use crate::grade::Bake;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use knife_audio::{Pcm, bfsar::SoundArchive, codec};
use knife_core::{
    exe::{Build, ExeTables, ImageInfo, Nso},
    format::{
        archive::Archive,
        bntx::{self, TexFormat},
        bntx_decode,
        script::{SceneArchive, ScriptText},
        sfnt,
    },
    vm::parse::Scene,
};
use knife_render::{Renderer, TexRef, Upload};

/// Image name prefix that loads the save-list thumbnail copy of an image (`Assets::thumb_name`).
pub const THUMB: &str = "THUMB:";

/// A texture together with its display metrics.
pub struct Image {
    pub tex: TexRef,
    /// Display size and centre in 640x362 units.
    pub width: f32,
    pub height: f32,
    pub center_x: f32,
    pub center_y: f32,
    /// ImageInfo +70 scale (`ImageInfo::level_scale`)
    pub level: f32,
    /// ImageInfo +65 bit 1: not moved by the screen shake
    pub no_shake: bool,
}

impl Image {
    /// The stage anchor cmd_STCH_impl gives a layer showing this image: its centre cut to whole units (stage
    /// float 221 + layer = (int)centre, shift word 2923 + 9 * layer = (int)-centre), what the draw zooms and
    /// turns about. Witch on the Holy Night's STCH (jp_60840) takes the exact centre.
    pub fn anchor(&self) -> [f32; 2] {
        if witch() { [self.center_x, self.center_y] } else { [self.center_x.trunc(), self.center_y.trunc()] }
    }
}

/// Decoded pixels of an image, ready to upload.
pub enum Pixels {
    /// As stored; `true`: still to go through pre_alter (`Renderer::upload_altered`)
    Bntx(bntx::Texture, bool),
    /// RGBA8; `Some(srgb)`: still to go through pre_alter, sampled through an sRGB view when `srgb`; then a
    /// language strip's (full height, top row)
    Rgba(u32, u32, Vec<u8>, Option<bool>, Option<(u32, u32)>),
    /// RGBA8 baked on the CPU (`Bake::apply`), sRGB-encoded like the altered texture it stands for; strip as above
    Baked(u32, u32, Vec<u8>, Option<(u32, u32)>),
}

/// An image `Assets::prefetch` is reading on a worker thread: its name, language strip, and where the pixels
/// arrive.
struct Decoding {
    name: String,
    strip: Option<(usize, usize)>,
    pixels: std::sync::mpsc::Receiver<Result<Pixels>>,
}

/// How many of the latest cached images `Assets::trim_images` keeps unreferenced (prefetched ones waiting for
/// their command).
const FRESH_IMAGES: usize = 64;
/// How many prefetched images `Assets::finish_prefetched` uploads a frame.
const UPLOADS_PER_FRAME: usize = 2;

/// The game data folders: `--assets-dir`, and `--mod-assets` when there is one, whose files take the place of
/// the game data's at the same relative path.
#[derive(Clone)]
pub struct Roots {
    pub base: PathBuf,
    pub mods: Option<PathBuf>,
}

impl Roots {
    /// The file at `rel`: the mod folder's when it has one, else the game data's.
    pub fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        let rel = rel.as_ref();
        self.mods.as_ref().map(|m| m.join(rel)).filter(|p| p.is_file()).unwrap_or_else(|| self.base.join(rel))
    }

    /// Archive `base`, each of its files from either folder (`path`).
    pub fn archive(&self, base: &str) -> Result<Archive> {
        Archive::open_with(base, |file| self.path(file))
    }
}

pub struct Assets {
    pub roots: Roots,
    /// Japanese or overseas data (`detect_build`)
    pub build: Build,
    /// The game language (sub_2DE70: config word +0x12 when it is below `languages`, else the system language):
    /// 0 Japanese, 1 English, 2 Simplified Chinese, 3 Traditional Chinese
    language: usize,
    /// sub_9A700: the language count of the SYSMES file (1 Japanese, 4 overseas)
    languages: usize,
    script_text: Vec<u8>,
    pub allpac: Arc<Archive>,
    /// The second image archive (archive id 10): empty in the Japanese data; the overseas one holds the save
    /// list pictures IMG6000..6026 and 140 other images allpac lacks, with their thumbnails
    pub allpacml: Option<Arc<Archive>>,
    pub allpachd: Option<Arc<Archive>>,
    /// Witch on the Holy Night's allpachdml (archive 11): the HD pictures of more than one language
    pub allpachdml: Option<Arc<Archive>>,
    pub allpaccg: Option<Arc<Archive>>,
    pub allui: Arc<Archive>,
    pub tables: ExeTables,
    image_index: HashMap<String, usize>,
    pub scenes: SceneArchive,
    pub text: ScriptText,
    pub sar: Arc<SoundArchive>,
    parsed: HashMap<usize, Arc<Scene>>,
    images: HashMap<String, Option<Arc<Image>>>,
    /// the images `prefetch` is decoding, by cache key
    decoding: HashMap<String, Decoding>,
    /// the most recently cached images, which `trim_images` keeps for the commands that asked ahead for them
    fresh: std::collections::VecDeque<String>,
    sounds: HashMap<String, Option<Arc<Pcm>>>,
    /// the sound effects `prefetch_sound` is decoding, by cache key
    sounds_decoding: HashMap<String, std::sync::mpsc::Receiver<Result<Pcm>>>,
}

/// The build a romfs belongs to: the overseas data names its archives `w_allpac` / `w_allui` / ... (sub_AE050),
/// Witch on the Holy Night's `jallpac` / `jallpachd` (WoH sub_89A80).
pub fn detect_build(romfs: &Path) -> Build {
    if romfs.join("w_allpac.nam").exists() {
        Build::Overseas
    } else if romfs.join("jallpac.nam").exists() {
        Build::Witch
    } else {
        Build::Japanese
    }
}

/// The executable's tables for a build, compiled in (`kdu exe-tables` writes them; knife_core::exe).
pub fn exe_tables(build: Build) -> Result<Nso> {
    Nso::from_bundle(match build {
        Build::Japanese => include_bytes!("../data/exe_tables.bin"),
        Build::Overseas => include_bytes!("../data/exe_tables_overseas.bin"),
        Build::Witch => include_bytes!("../data/exe_tables_witch.bin"),
    })
}

/// sub_2DE00: the game language of the console's desired language (nn::oe::GetDesiredLanguage, limited to the
/// languages the title declares; the Japanese build declares only Japanese): Japanese 0, Simplified Chinese 2,
/// Traditional Chinese 3, any other language 1 (English). knife takes the host locale for the console's.
pub fn system_language(build: Build) -> i16 {
    if build == Build::Japanese {
        return 0;
    }
    let locale = sys_locale::get_locale().unwrap_or_default().to_ascii_lowercase().replace('_', "-");
    let mut parts = locale.split('-');
    match parts.next().unwrap_or("") {
        "ja" => 0,
        "zh" => {
            let rest: Vec<&str> = parts.collect();
            // zh-Hant / zh-TW / zh-HK / zh-MO are Traditional, other Chinese locales Simplified
            if rest.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo")) { 3 } else { 2 }
        }
        _ => 1,
    }
}

/// The Japanese release's application title.
pub const TITLE: &str = "月姫 -A piece of blue glass moon-";
/// The overseas release's titles by game language (its control.nacp: Japanese, American English, Simplified and
/// Traditional Chinese entries; the Chinese ones write 月姬, U+59EC).
const OVERSEAS_TITLES: [&str; 4] =
    [TITLE, "TSUKIHIME A piece of blue glass moon", "月姬 -A piece of blue glass moon-", "月姬 -A piece of blue glass moon-"];

/// Witch on the Holy Night's application titles by game language.
const WITCH_TITLES: [&str; 4] = ["魔法使いの夜", "WITCH ON THE HOLY NIGHT", "魔法使之夜", "魔法使之夜"];

/// The SYSMES message the overseas build inserts at 217 (the "deleted" popup, `jp_50FC0` state 25). knife keeps
/// the Japanese numbering and moves it here, past the Japanese build's 1498 messages.
pub const SYSMES_DELETED: usize = 1498;
const SYSMES_OVERSEAS_INSERT: usize = 217;

/// The loaded executable is the overseas build (for tables that differ between the builds).
static OVERSEAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn overseas() -> bool {
    OVERSEAS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The loaded executable is Witch on the Holy Night's.
static WITCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record the build before anything reads the save file, whose layout depends on it (Assets::open sets the
/// same flags).
pub fn select_build(build: Build) {
    OVERSEAS.store(build == Build::Overseas, std::sync::atomic::Ordering::Relaxed);
    WITCH.store(build == Build::Witch, std::sync::atomic::Ordering::Relaxed);
}

pub fn witch() -> bool {
    #[cfg(test)]
    if TEST_WITCH.get() {
        return true;
    }
    WITCH.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Tests of Witch on the Holy Night's tables switch the build on for their own thread.
    pub static TEST_WITCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Assets {
    /// `language` is the config word (`cfg::LANGUAGE`); `language()` resolves it like sub_2DE70.
    pub fn open(roots: &Roots, nso: &Nso, language: i16) -> Result<Self> {
        let tables = ExeTables::load(nso)?;
        let build = tables.build;
        // sub_AE050: the overseas archives allpac / allpacml / allui / allpaccg carry a `w_` prefix; Witch on the
        // Holy Night (WoH sub_89A80) opens jallpac (8), allpacml (9), jallpachd (10), allpachdml (11), allui (12)
        // and parts (25), with no CG archive
        let w = |base: &str| match build {
            Build::Overseas => format!("w_{base}"),
            Build::Witch if base == "allpac" => "jallpac".to_string(),
            Build::Japanese | Build::Witch => base.to_string(),
        };
        let hd = if build == Build::Witch { "jallpachd" } else { "allpachd" };
        let image_index = tables
            .images
            .iter()
            .enumerate()
            .map(|(i, im)| (im.name.to_ascii_uppercase(), i))
            .collect();
        let scenes = SceneArchive::parse(&std::fs::read(roots.path("allscr.mrg")).context("reading allscr.mrg")?)?;
        let script_text = std::fs::read(roots.path("script_text.mrg")).context("reading script_text.mrg")?;
        let sar_name = if build == Build::Witch { "witch.bfsar" } else { "knife1.bfsar" };
        let sar = SoundArchive::parse(std::fs::read(roots.path(sar_name)).with_context(|| format!("reading {sar_name}"))?)?;
        let allui = Arc::new(roots.archive(&w("allui"))?);
        let languages = Self::sysmes_file(&allui, build).map_or(1, |d| sysmes_u32(&d, 0).unwrap_or(1).max(1) as usize);
        let mut a = Self {
            roots: roots.clone(),
            build,
            language: 0,
            languages,
            text: ScriptText::parse(&script_text, 0)?,
            script_text,
            allpac: Arc::new(roots.archive(&w("allpac"))?),
            allpacml: roots.archive(&w("allpacml")).ok().map(Arc::new),
            allpachd: roots.archive(hd).ok().map(Arc::new),
            allpachdml: if build == Build::Witch { roots.archive("allpachdml").ok().map(Arc::new) } else { None },
            allpaccg: if build == Build::Witch { None } else { roots.archive(&w("allpaccg")).ok().map(Arc::new) },
            allui,
            tables,
            image_index,
            scenes,
            sar: Arc::new(sar),
            parsed: HashMap::new(),
            images: HashMap::new(),
            decoding: HashMap::new(),
            fresh: Default::default(),
            sounds: HashMap::new(),
            sounds_decoding: HashMap::new(),
        };
        crate::text::set_line_break(a.tables.line_break.clone());
        OVERSEAS.store(a.build == Build::Overseas, std::sync::atomic::Ordering::Relaxed);
        WITCH.store(a.build == Build::Witch, std::sync::atomic::Ordering::Relaxed);
        // the first language may be the one the text was parsed in, which `set_language` skips: the fonts'
        // metrics still apply (Witch on the Holy Night's Japanese font pairs to a 49 px line feed, not 50)
        if !a.set_language(language)? {
            a.apply_font_metrics();
        }
        Ok(a)
    }

    /// Hand the language's font metrics (`font_metrics`) to the text layout.
    fn apply_font_metrics(&self) {
        let (msg, bubble, glyph_baseline) = self.font_metrics();
        // Tsukihime's bubble font is in the message font list (`<folder>3`), whose line feed it shares
        let bubble_feed = if self.build == Build::Witch { bubble.line_feed } else { msg.line_feed };
        crate::text::set_font_metrics(msg.line_feed, msg.ascent, msg.baseline, bubble.ascent, bubble_feed, glyph_baseline);
    }

    /// jp_9A700: the languages the data declares.
    pub fn languages(&self) -> usize {
        self.languages
    }

    /// sub_2DE70: the game language for a config word (negative or past the data's languages: the system's).
    pub fn resolve_language(&self, word: i16) -> usize {
        if word >= 0 && (word as usize) < self.languages {
            word as usize
        } else {
            system_language(self.build) as usize
        }
    }

    pub fn language(&self) -> usize {
        self.language
    }

    /// The application title in the game language, as the console shows it (`OVERSEAS_TITLES`; the Japanese
    /// release has its one title).
    pub fn title(&self) -> &'static str {
        match self.build {
            Build::Overseas => OVERSEAS_TITLES.get(self.language).copied().unwrap_or(TITLE),
            Build::Witch => WITCH_TITLES.get(self.language).copied().unwrap_or(WITCH_TITLES[0]),
            Build::Japanese => TITLE,
        }
    }

    /// Switch to the language of a config word: the script text pair (sub_53CF0 reads script_text.mrg entries
    /// 2 * language / 2 * language + 1) and the language-strip images (sub_3F130 / sub_3F1A0 rebuild them).
    /// Returns whether the language changed.
    pub fn set_language(&mut self, word: i16) -> Result<bool> {
        let language = self.resolve_language(word);
        if language == self.language && !self.text.is_empty() {
            return Ok(false);
        }
        self.text = ScriptText::parse(&self.script_text, language)?;
        self.language = language;
        crate::text::set_language(language);
        self.apply_font_metrics();
        let strips: Vec<String> = self
            .images
            .keys()
            .filter(|k| self.image_info(k.split('#').next().unwrap_or(k)).is_some_and(|i| i.language_strips))
            .cloned()
            .collect();
        for k in strips {
            self.images.remove(&k);
        }
        Ok(true)
    }

    pub fn scene(&mut self, index: usize) -> Option<Arc<Scene>> {
        if let Some(s) = self.parsed.get(&index) {
            return Some(s.clone());
        }
        let raw = self.scenes.scene(index)?;
        let s = Arc::new(Scene::parse(raw));
        self.parsed.insert(index, s.clone());
        Some(s)
    }

    pub fn scene_name(&self, index: usize) -> &str {
        self.scenes.names.get(index).map(|s| s.as_str()).unwrap_or("?")
    }

    /// Scene index for a script `JUMP` id.
    pub fn scene_by_id(&self, id: i32) -> Option<usize> {
        self.tables.scene_index(u16::try_from(id).ok()?)
    }

    pub fn line(&self, index: usize) -> &str {
        self.text.get(index).unwrap_or("")
    }

    /// Exact image table index of a name (sub_7DFB0).
    pub fn image_index_of(&self, name: &str) -> Option<usize> {
        self.image_index.get(&name.to_ascii_uppercase()).copied()
    }

    /// The image a save-list preview draws for a layer showing `name` (sub_895A0): its thumbnail copy, and
    /// for an `_aa_bb` variant (ImageInfo +67 bit 0) the base image's, the name less its last 6 characters.
    /// The exe moves the anchor by the centre difference, so the base lands centred where the variant was.
    pub fn thumb_name(&self, name: &str) -> String {
        let upper = name.to_ascii_uppercase();
        let upper = upper.strip_prefix(THUMB).unwrap_or(&upper);
        let base = match self.image_index_of(upper).and_then(|i| self.tables.images.get(i)) {
            Some(im) if im.variant && upper.len() > 6 => &upper[..upper.len() - 6],
            _ => upper,
        };
        format!("{THUMB}{base}")
    }

    pub fn image_info(&self, name: &str) -> Option<&ImageInfo> {
        let upper = name.to_ascii_uppercase();
        let upper = upper.strip_prefix(THUMB).unwrap_or(&upper).to_string();
        if let Some(&i) = self.image_index.get(&upper) {
            return self.tables.images.get(i);
        }
        // expression variants (`NAME_AA_BB`) share the body's metrics
        let mut base = upper.as_str();
        for _ in 0..2 {
            match base.rsplit_once('_') {
                Some((b, _)) => base = b,
                None => break,
            }
            if let Some(&i) = self.image_index.get(base) {
                return self.tables.images.get(i);
            }
        }
        None
    }

    /// The image archives in lookup order: jp_32360 resolves a name with jp_A90C0(name, -1), which searches
    /// allpac (archive 9), then allpacml (10), then the others; allpac and allpacml come first here for
    /// `image_bytes`' thumbnails.
    fn image_archives(&self) -> Vec<Arc<Archive>> {
        [Some(&self.allpac), self.allpacml.as_ref(), self.allpaccg.as_ref(), Some(&self.allui), self.allpachd.as_ref()]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// Witch on the Holy Night's HD lookup (jp_A90C0(name, 1)): jallpachd's list (jd_18C0514 = 2), then
    /// allpachdml's (3), then allui's (4).
    fn hd_archives(&self) -> Vec<Arc<Archive>> {
        [self.allpachd.as_ref(), self.allpachdml.as_ref(), Some(&self.allui)].into_iter().flatten().cloned().collect()
    }

    /// Whether jp_A90C0(name, 1) finds `name` (CG mode's pictures).
    pub fn has_image_hd(&self, name: &str) -> bool {
        self.hd_archives().iter().any(|a| a.find_stem(name).is_some())
    }

    /// A picture through the HD lookup (CG mode's textures 285 / 286, jp_B6A20 after sub_83E00(2 / 3)), sized
    /// by its texels: the HD files are stored at the 1920x1080 size the tables give.
    pub fn image_hd(&mut self, r: &Renderer, name: &str) -> Option<Arc<Image>> {
        let (name, grade, key) = Self::image_key(name, Bake::NONE);
        let key = format!("HD {key}");
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let pixels = Self::decode_pixels(&self.hd_archives(), &name, grade, self.strip(&name));
        let img = match pixels.and_then(|p| self.upload_pixels(r, &name, p)) {
            Ok(mut i) => {
                i.width = i.tex.width as f32;
                i.height = i.tex.height as f32;
                Some(Arc::new(i))
            }
            Err(e) => {
                tracing::warn!("image {name}: {e:#}");
                None
            }
        };
        self.images.insert(key, img.clone());
        img
    }

    fn image_bytes(archives: &[Arc<Archive>], name: &str) -> Option<Vec<u8>> {
        if let Some(stem) = name.strip_prefix(THUMB) {
            // sub_895A0: the layer's allpac entry - (bg_top.mzp + 1) + (thumstand_top.mzp + 1), a copy of the
            // art at 1/6 size (a 4x4 transparent DUMMY.NXZ for the `_aa_bb` variants and IMG7000 / IMG7001);
            // an allpacml image (archive 10 / 12) its entry - (bgml_top + 1) + (thumstandml_top + 1) there
            let ml = archives.iter().find(|a| a.name.ends_with("allpacml"));
            let (a, base, thumbs) = [(archives.first(), "BG_TOP", "THUMSTAND_TOP"), (ml, "BGML_TOP", "THUMSTANDML_TOP")]
                .into_iter()
                .find_map(|(a, base, thumbs)| a.filter(|a| a.find_stem(stem).is_some()).map(|a| (a, base, thumbs)))?;
            let i = a.find_stem(stem)?.checked_sub(a.find_stem(base)? + 1)? + a.find_stem(thumbs)? + 1;
            return a.read(i).inspect_err(|e| tracing::warn!("reading thumbnail {stem}: {e}")).ok();
        }
        for a in archives {
            if let Some(i) = a.find_stem(name) {
                match a.read(i) {
                    Ok(d) => return Some(d),
                    Err(e) => tracing::warn!("reading {name} from {}: {e}", a.name),
                }
            }
        }
        None
    }

    /// Load (and cache) an image by script name. Returns None when it does not exist.
    pub fn image(&mut self, r: &Renderer, name: &str) -> Option<Arc<Image>> {
        self.image_graded(r, name, Bake::NONE)
    }

    /// Load (and cache) a rule mask (RULE_nn), which the transitions sample as stored (`Bake::raw`).
    pub fn rule(&mut self, r: &Renderer, name: &str) -> Option<Arc<Image>> {
        self.image_graded(r, name, Bake::RAW)
    }

    /// Load (and cache) an image built with an STEC colour grade and the SNX2 / SNX3 tone (`grade::Bake`). The
    /// exe keys its texture cache (sub_AAA40) the same way, by image and the pre_alter parameters.
    pub fn image_graded(&mut self, r: &Renderer, name: &str, bake: Bake) -> Option<Arc<Image>> {
        let (name, grade, key) = Self::image_key(name, bake);
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        // one already on its way: wait for it rather than read it again
        if let Some(d) = self.decoding.remove(&key)
            && d.strip == self.strip(&name)
            && let Ok(pixels) = d.pixels.recv()
        {
            return self.finish_image(r, key, &name, pixels);
        }
        let pixels = Self::decode_pixels(&self.image_archives(), &name, grade, self.strip(&name));
        self.finish_image(r, key, &name, pixels)
    }

    /// Cache key of an image load: (upper-case name, effective bake, key).
    pub fn image_key(name: &str, bake: Bake) -> (String, Bake, String) {
        let bake = Bake { grade: if crate::grade::is_grade(bake.grade) { bake.grade } else { 0 }, ..bake };
        let name = name.to_ascii_uppercase();
        let key = format!("{name}{}", bake.key());
        (name, bake, key)
    }

    /// Whether `image_graded(name, bake)` would return without loading anything.
    pub fn image_cached(&self, name: &str, bake: Bake) -> bool {
        self.images.contains_key(&Self::image_key(name, bake).2)
    }

    /// Start reading and decoding an image for `image_graded` on a worker thread, unless it is cached or on
    /// its way; `finish_prefetched` uploads and caches it once decoded, or `image_graded` waits for it.
    pub fn prefetch(&mut self, name: &str, bake: Bake) {
        let (name, grade, k) = Self::image_key(name, bake);
        if self.images.contains_key(&k) || self.decoding.contains_key(&k) {
            return;
        }
        let archives = self.image_archives();
        let strip = self.strip(&name);
        let (done, pixels) = std::sync::mpsc::channel();
        let job = name.clone();
        let spawned = std::thread::Builder::new().name("image decode".into()).spawn(move || {
            let _ = done.send(Self::decode_pixels(&archives, &job, grade, strip));
        });
        match spawned {
            Ok(_) => {
                self.decoding.insert(k, Decoding { name, strip, pixels });
            }
            Err(e) => tracing::warn!("image decode thread: {e}"),
        }
    }

    /// Upload and cache the prefetched images (and cache the sound effects) that have been decoded.
    pub fn finish_prefetched(&mut self, r: &Renderer) {
        let mut sounds = Vec::new();
        self.sounds_decoding.retain(|k, d| match d.try_recv() {
            Ok(res) => {
                sounds.push((k.clone(), res));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        for (k, res) in sounds {
            self.cache_sound(&k.clone(), k, res);
        }
        // a few uploads a frame (a command that needs one sooner takes it at once, `image_graded`)
        let mut done = Vec::new();
        self.decoding.retain(|k, d| match (done.len() < UPLOADS_PER_FRAME).then(|| d.pixels.try_recv()) {
            None => true,
            Some(Ok(pixels)) => {
                done.push((k.clone(), d.name.clone(), d.strip, pixels));
                false
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => true,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => false,
        });
        for (k, name, strip, pixels) in done {
            // a language change while it decoded makes a language-strip image stale
            if strip == self.strip(&name) {
                self.finish_image(r, k, &name, pixels);
            }
        }
    }

    fn finish_image(&mut self, r: &Renderer, key: String, name: &str, pixels: Result<Pixels>) -> Option<Arc<Image>> {
        let img = match pixels.and_then(|p| self.upload_pixels(r, name, p)) {
            Ok(i) => Some(Arc::new(i)),
            Err(e) => {
                tracing::warn!("image {name}: {e:#}");
                None
            }
        };
        self.fresh.push_back(key.clone());
        if self.fresh.len() > FRESH_IMAGES {
            self.fresh.pop_front();
        }
        self.images.insert(key, img.clone());
        img
    }

    /// Drop cached images nobody else references.
    pub fn trim_images(&mut self, keep: usize) {
        if self.images.len() <= keep {
            return;
        }
        let fresh = &self.fresh;
        self.images.retain(|k, v| v.as_ref().is_some_and(|i| Arc::strong_count(i) > 1) || fresh.contains(k));
    }

    /// The (strip, strip count) an image draws: a language-strip image (overseas ImageInfo +67 bit 1) stacks
    /// one copy per language, and a layer draws the game language's (sub_374F0 / sub_375D0 source rectangle
    /// `[0, language * h / n, w, h / n]`, n = sub_9A700).
    fn strip(&self, name: &str) -> Option<(usize, usize)> {
        let base = name.strip_prefix(THUMB).unwrap_or(name);
        let i = self.image_index_of(base)?;
        self.tables.images.get(i).filter(|im| im.language_strips).map(|_| (self.language, self.languages))
    }

    /// The CPU side of an image load (no renderer needed): archive read, BNTX parse or image decode, grade, and
    /// the language strip.
    fn decode_pixels(archives: &[Arc<Archive>], name: &str, grade: Bake, strip: Option<(usize, usize)>) -> Result<Pixels> {
        let data = Self::image_bytes(archives, name).context("not found")?;
        if !grade.cpu() && strip.is_none() && bntx::is_bntx(&data) {
            return Ok(Pixels::Bntx(bntx::parse(&data)?.into_iter().next().context("empty BNTX")?, !grade.raw));
        }
        let (w, mut h, mut px, srgb) = decode_image(&data)?;
        let mut rows_of = None;
        if let Some((k, n)) = strip {
            let rows = h / n as u32;
            let top = h as usize * k / n;
            px = px[top * w as usize * 4..(top + rows as usize) * w as usize * 4].to_vec();
            rows_of = Some((h, top as u32));
            h = rows;
        }
        if grade.cpu() {
            grade.apply(&mut px, srgb);
            return Ok(Pixels::Baked(w, h, px, rows_of));
        }
        Ok(Pixels::Rgba(w, h, px, (!grade.raw).then_some(srgb), rows_of))
    }

    fn upload_pixels(&self, r: &Renderer, name: &str, pixels: Pixels) -> Result<Image> {
        // sub_A3180's bake scale for the story images (sub_AAA40 / sub_B6A20, the save previews' too)
        let k = 2.0 / 3.0;
        let tex = match pixels {
            Pixels::Bntx(t, true) => upload_bntx(r, &t, k)?,
            Pixels::Bntx(t, false) => upload_bntx_raw(r, &t)?,
            Pixels::Rgba(w, h, px, Some(srgb), strip) => r.upload_altered(w, h, Upload::Rgba8(&px), srgb, k, strip, name),
            Pixels::Rgba(w, h, px, None, _) => r.upload(w, h, Upload::Rgba8(&px), name),
            Pixels::Baked(w, h, px, strip) => r.upload_baked(w, h, &px, k, strip, name),
        };
        let no_shake = self.image_info(name).is_some_and(|i| i.no_shake);
        let (width, height, center_x, center_y, level) = match self.image_info(name) {
            // Witch on the Holy Night's layer draw (sub_1A090 -> jp_B0460 / sub_16210 / jp_36520) sizes the quad by
            // the altered texture, (u32)(size / (2 / 3)) px (jp_A3180, the f32 0x3F2AAAAB) in the 1920x1080 space, a language-strip
            // image's height over the strip count; the ImageInfo gives only the centre (+52 / +56) the position is
            // offset by. Its width / height can disagree (SOU_L_12_10_00_01_01: 228 x 384 units, texture 402 x 860).
            // The save thumbnails (jp_89AD0) size their 1/6 copies by texture / (1 / 6) in the 1280x720 thumbnail
            // surface, 1.5 times that in the 1920 space the previews are laid out in.
            Some(i) if witch() => {
                let n = self.strip(name).map_or(1, |(_, n)| n as u32);
                let (k, m) = if name.starts_with(THUMB) { (1.0 / 6.0, 1.5) } else { (2.0 / 3.0, 1.0) };
                let alt = |v: u32| (v as f32 / k) as u32 as f32 * m;
                let w = alt(tex.width) / 3.0;
                let h = alt(tex.height * n) / n as f32 * 362.0 / 1080.0;
                (w, h, i.center_x, i.center_y, i.level_scale())
            }
            Some(i) => (i.width, i.height, i.center_x, i.center_y, i.level_scale()),
            None => {
                // 1280x720 textures map to 640 units
                let w = tex.width as f32 / 2.0;
                let h = tex.height as f32 / 2.0 * (362.0 / 360.0);
                (w, h, w / 2.0, h / 2.0, 1.0)
            }
        };
        Ok(Image {
            tex,
            width,
            height,
            center_x,
            center_y,
            level,
            no_shake,
        })
    }

    /// Flowchart node preview: allpaccg entry `thumcg_top.mzp` + 1 + n (GALLERY_FC_THnnnn), by stem.
    pub fn flow_preview_name(&self, n: i32) -> Option<String> {
        let a = self.allpaccg.as_ref()?;
        let i = a.find_stem("THUMCG_TOP")? + 1 + usize::try_from(n).ok()?;
        let name = &a.entries().get(i)?.name;
        Some(name.split('.').next().unwrap_or(name).to_string())
    }

    /// Load a UI texture atlas (BNTX in allui) by entry stem; returns all textures by name.
    pub fn ui_textures(&self, r: &Renderer, stem: &str) -> Result<Vec<(String, TexRef)>> {
        let i = self.allui.find_stem(stem).with_context(|| format!("{stem} not in allui"))?;
        let data = self.allui.read(i)?;
        let mut out = Vec::new();
        for t in bntx::parse(&data)? {
            let tex = upload_bntx(r, &t, 1.0)?;
            out.push((t.name.clone(), tex));
        }
        // the game picks atlas images by index (the object table, sub_9A940); the other languages' atlases
        // (`NAME_<FOLDER>`) hold the same images in the same order under their own names (`title_titlelog_en`,
        // `caution_en`, `flow_info_en`, ...), so the UI also finds them under the Japanese atlas's names
        let ja_names: Vec<String> = stem
            .rsplit_once('_')
            .filter(|(_, l)| !l.eq_ignore_ascii_case("ja"))
            .and_then(|(base, _)| self.allui.find_stem(&format!("{base}_JA")))
            .and_then(|j| self.allui.read(j).ok())
            .and_then(|d| bntx::parse(&d).ok())
            .map(|ts| ts.into_iter().map(|t| t.name).collect())
            .unwrap_or_default();
        if !ja_names.is_empty() && ja_names.len() != out.len() {
            tracing::warn!("{stem}: {} images, the Japanese atlas {}", out.len(), ja_names.len());
        }
        let aliases: Vec<(String, TexRef)> = ja_names
            .into_iter()
            .zip(&out)
            .filter(|(ja, (n, _))| ja != n)
            .map(|(ja, (_, t))| (ja, t.clone()))
            .collect();
        out.extend(aliases);
        // the overseas Japanese atlas also renames some images the Japanese release has without a suffix
        // (`system_bg` -> `system_bg_ja`); the UI uses the Japanese release's names
        let aliases: Vec<(String, TexRef)> = out
            .iter()
            .filter_map(|(n, t)| {
                let base = n.strip_suffix("_ja")?;
                (!out.iter().any(|(m, _)| m == base)).then(|| (base.to_string(), t.clone()))
            })
            .collect();
        out.extend(aliases);
        Ok(out)
    }

    /// BORDER_PARTS (sub_3B9A0 loads it into texture slots 338..355): alphagradation020..512, their `_inv`
    /// and `_2` variants. For the clip inset mask, the alpha of each one's middle row from the edge to the
    /// centre texel.
    pub fn soft_lut_rows(&self) -> Result<Vec<Vec<u8>>> {
        let i = self.allui.find_stem("BORDER_PARTS").context("BORDER_PARTS not in allui")?;
        let data = self.allui.read(i)?;
        bntx::parse(&data)?
            .iter()
            .map(|t| {
                let px = bntx_decode::to_rgba8(t)?;
                let (w, r) = (t.width as usize, (t.width / 2) as usize);
                Ok((0..=r).map(|x| px[(r * w + x) * 4 + 3]).collect())
            })
            .collect()
    }

    /// parts.mrg (no name table; the boot sequence loads entries by index, sub_B4A20 archive slot 27):
    /// 0 logo_fix, 1 caution2 (both cautions on one screen), 2 caution, 3 caution1.
    pub fn parts_texture(&self, r: &Renderer, index: usize) -> Option<TexRef> {
        let load = || -> Result<TexRef> {
            let data = self.roots.archive("parts")?.read(index)?;
            let t = bntx::parse(&data)?.into_iter().next().context("empty BNTX")?;
            // sub_B5650 bakes them as story images (sub_B02A0 with a9 false)
            upload_bntx(r, &t, 2.0 / 3.0)
        };
        load().map_err(|e| tracing::warn!("parts #{index}: {e:#}")).ok()
    }

    /// The SYSMES file: SYSMES_TEXT in the Japanese allui, SYSMES_TEXT_ML in the overseas w_allui (jp_9A420);
    /// Witch on the Holy Night's SYSMES_TEXT holds four languages (no Korean, though allui has _KO atlases).
    fn sysmes_file(allui: &Archive, build: Build) -> Option<Vec<u8>> {
        let stem = match build {
            Build::Japanese | Build::Witch => "SYSMES_TEXT",
            Build::Overseas => "SYSMES_TEXT_ML",
        };
        allui.read(allui.find_stem(stem)?).ok()
    }

    /// The game language's system strings, indexed by the Japanese build's ids (u32 language count, u32 count,
    /// u64 0, then per language a u64 offset of a table of u64 offsets of NUL-terminated UTF-8 strings; entry 0
    /// is the language's folder name). The overseas file's message 217 moves to `SYSMES_DELETED`.
    pub fn sysmes(&self) -> Vec<String> {
        self.sysmes_in(self.language)
    }

    /// `sysmes` in language `lang` (jp_9A780(id, lang)).
    pub fn sysmes_in(&self, lang: usize) -> Vec<String> {
        let parse = || -> Option<Vec<String>> {
            let d = Self::sysmes_file(&self.allui, self.build)?;
            let n = sysmes_u32(&d, 4)? as usize;
            let table = sysmes_u64(&d, 0x10 + 8 * lang)?;
            let mut v = (0..n)
                .map(|i| {
                    let o = sysmes_u64(&d, table + 8 * i)?;
                    let end = d.get(o..)?.iter().position(|&b| b == 0)?;
                    Some(String::from_utf8_lossy(&d[o..o + end]).into_owned())
                })
                .collect::<Option<Vec<String>>>()?;
            if self.build == Build::Overseas && v.len() > SYSMES_OVERSEAS_INSERT {
                let deleted = v.remove(SYSMES_OVERSEAS_INSERT);
                v.resize(SYSMES_DELETED, String::new());
                v.push(deleted);
            }
            Some(v)
        };
        parse().unwrap_or_default()
    }

    /// The button guide's help_txt word cells of the game language (CONTHELP_TXT_UV.BIN, jp_9A420 / jp_9A710:
    /// i16 count, then from byte 8 per language `count` cells of i16 x0 y0 x1 y1).
    pub fn guide_words(&self) -> Vec<[u16; 4]> {
        let parse = || -> Option<Vec<[u16; 4]>> {
            let d = self.allui.read(self.allui.find_stem("CONTHELP_TXT_UV")?).ok()?;
            let n = i16::from_le_bytes(d.get(0..2)?.try_into().ok()?).max(0) as usize;
            let base = 8 + 8 * n * self.language;
            (0..n)
                .map(|i| {
                    let c = d.get(base + 8 * i..base + 8 * i + 8)?;
                    Some([0, 2, 4, 6].map(|k| u16::from_le_bytes([c[k], c[k + 1]])))
                })
                .collect()
        };
        parse().unwrap_or_default()
    }

    /// A system atlas of the game language (jp_9A940: the object table's `name.nxgz` becomes
    /// `name_<language folder>.nxgz`, found case-insensitively; SELECT_PARTS_JA alone is named in full).
    pub fn ui_atlas(&self, stem: &str) -> String {
        format!("{stem}_{}", self.language_folder().to_ascii_uppercase())
    }

    /// sub_AB0F0: the first file of the language's font folder `<folder><suffix>` (suffix "" the story text,
    /// "2" the UI text, "3" the bubble text); a mod folder with files of its own there takes the place of the
    /// game data's.
    pub fn font_path(&self, suffix: &str) -> Option<PathBuf> {
        let rel = format!("{}{suffix}", self.language_folder());
        let first = |dir: PathBuf| {
            let mut files: Vec<PathBuf> =
                std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
            files.sort();
            files.into_iter().next()
        };
        self.roots.mods.as_ref().and_then(|m| first(m.join(&rel))).or_else(|| first(self.roots.base.join(&rel)))
    }

    /// A game data file (`Roots::path`): the mod folder's when it has one.
    pub fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.roots.path(rel)
    }

    /// The pair font metrics at the 40 px base size (sub_CAE88, `sfnt::pair_metrics`) of the message font set
    /// (the language's three fonts sub_AB0F0 loads) and of the bubble font (`<folder>3`), each paired with the
    /// exfont: line feed / ascent / baseline position Japanese 50 / 41 / 41, English 53 / 43 / 40 (bubble ascent
    /// 41), Simplified Chinese 49 / 39 / 39, Traditional Chinese 47 / 39 / 37 (the exfont's); the Japanese ones
    /// without font files. Also the message set's own baseline position, its glyph cache cell's (Traditional
    /// Chinese: SungASC's 43, not the pair's 37).
    fn font_metrics(&self) -> (sfnt::SetMetrics, sfnt::SetMetrics, i32) {
        use knife_core::format::{bffnt, bfttf};
        let read = |p: PathBuf| std::fs::read(p).ok().map(|d| bfttf::decode(&d).unwrap_or(d));
        let set = |suffixes: &[&str]| {
            let m: Vec<sfnt::Metrics> =
                suffixes.iter().filter_map(|s| self.font_path(s)).filter_map(read).filter_map(|d| sfnt::metrics(&d)).collect();
            sfnt::set_metrics(&m, crate::text::BASE_SIZE)
        };
        let ex = std::fs::read(self.path("exfont_nx01.bffnt"))
            .ok()
            .and_then(|d| bffnt::Font::parse(&d).ok())
            .map(|f| sfnt::SetMetrics {
                height: i32::from(f.height),
                ascent: i32::from(f.ascent),
                line_feed: i32::from(f.line_feed),
                baseline: i32::from(f.baseline),
            });
        let ja = sfnt::SetMetrics { height: 50, ascent: 41, line_feed: 50, baseline: 41 };
        let own = set(&["", "2", "3"]);
        let msg = own.map_or(ja, |m| sfnt::pair_metrics(m, ex));
        let bubble = match set(&["3"]) {
            Some(m) => sfnt::pair_metrics(m, ex),
            // Witch on the Holy Night's bubble variant is the shared system font (`Game::load_fonts`)
            None if self.build == Build::Witch => self
                .shared_fonts()
                .first()
                .and_then(|d| sfnt::metrics(d))
                .and_then(|m| sfnt::set_metrics(&[m], crate::text::BASE_SIZE))
                .map_or(msg, |m| sfnt::pair_metrics(m, ex)),
            None => msg,
        };
        (msg, bubble, own.map_or(ja.baseline, |m| m.baseline))
    }

    /// The console shared fonts sub_AB0F0 asks for, in their order of priority (sub_C9F3C: nn::pl::GetSharedFont
    /// with the language code of dword_3C97F0[language]: ja / en-US the standard font, zh-Hans / zh-Hant the
    /// Chinese ones first). sub_CAA50 puts them all in each font set, so the later ones fill in missing glyphs.
    pub fn shared_font_names(&self) -> &'static [&'static str] {
        match self.language {
            2 => &[
                "nintendo_udsg-r_org_zh-cn_003.bfttf",
                "nintendo_udsg-r_ext_zh-cn_003.bfttf",
                "nintendo_udsg-r_std_003.bfttf",
            ],
            3 => &["nintendo_udjxh-db_zh-tw_003.bfttf", "nintendo_udsg-r_std_003.bfttf"],
            _ => &["nintendo_udsg-r_std_003.bfttf"],
        }
    }

    /// The shared fonts there are (`shared_font_names`, decoded), in order; a missing one is reported.
    pub fn shared_fonts(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for name in self.shared_font_names() {
            let path = self.path(name);
            match std::fs::read(&path) {
                Ok(d) => out.push(knife_core::format::bfttf::decode(&d).unwrap_or(d)),
                Err(e) => tracing::warn!("system font {}: {e}", path.display()),
            }
        }
        out
    }

    /// A per-language romfs file (sub_AB0F0 fonts, the save icon, movies): `<stem>` in Japanese, else
    /// `<stem>_en` / `_zc` / `_zt` (the language folder names, SYSMES entry 0).
    pub fn language_folder(&self) -> &'static str {
        ["ja", "en", "zc", "zt"].get(self.language).copied().unwrap_or("ja")
    }

    /// Decode a sound effect from the BFSAR (`MOON_SE03001` -> `WSD_moon_se03001`).
    pub fn sound_effect(&mut self, name: &str) -> Option<Arc<Pcm>> {
        let key = name.to_ascii_lowercase();
        if let Some(s) = self.sounds.get(&key) {
            return s.clone();
        }
        // one already on its way: wait for it rather than decode it again
        let res = match self.sounds_decoding.remove(&key).and_then(|d| d.recv().ok()) {
            Some(res) => res,
            None => Self::read_sound(&self.sar, &self.path(format!("stream/{name}.opus")), &key),
        };
        self.cache_sound(name, key, res)
    }

    /// Start decoding a sound effect for `sound_effect` on a worker thread, unless it is cached or on its way.
    pub fn prefetch_sound(&mut self, name: &str) {
        let key = name.to_ascii_lowercase();
        if self.sounds.contains_key(&key) || self.sounds_decoding.contains_key(&key) {
            return;
        }
        let (sar, stream, job) = (self.sar.clone(), self.path(format!("stream/{name}.opus")), key.clone());
        let (done, pcm) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("sound decode".into()).spawn(move || {
            let _ = done.send(Self::read_sound(&sar, &stream, &job));
        });
        match spawned {
            Ok(_) => {
                self.sounds_decoding.insert(key, pcm);
            }
            Err(e) => tracing::warn!("sound decode thread: {e}"),
        }
    }

    /// A sound effect: the wave sound `wsd_<name>` or `<name>` of the sound archive, else the stream file.
    fn read_sound(sar: &SoundArchive, stream: &Path, key: &str) -> Result<Pcm> {
        sar.load_wave_sound(&format!("wsd_{key}")).or_else(|_| sar.load_wave_sound(key)).or_else(|_| {
            let data = std::fs::read(stream).with_context(|| format!("reading {}", stream.display()))?;
            codec::decode_nopus(&data)
        })
    }

    fn cache_sound(&mut self, name: &str, key: String, res: Result<Pcm>) -> Option<Arc<Pcm>> {
        let pcm = match res {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::warn!("sound {name}: {e:#}");
                None
            }
        };
        if self.sounds.len() > 256 {
            self.sounds.clear();
        }
        self.sounds.insert(key, pcm.clone());
        pcm
    }

    /// A wave sound by BFSAR item id (sub_710's dword_3341A0 ids), cached with the named sounds.
    pub fn sound_effect_id(&mut self, item: u32) -> Option<Arc<Pcm>> {
        let key = format!("#{item:x}");
        if let Some(s) = self.sounds.get(&key) {
            return s.clone();
        }
        let pcm = match self.sar.load_wave_sound_id(item) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::warn!("sound {item:#x}: {e:#}");
                None
            }
        };
        if self.sounds.len() > 256 {
            self.sounds.clear();
        }
        self.sounds.insert(key, pcm.clone());
        pcm
    }

    pub fn load_stream_file(&self, rel: &str) -> Result<Pcm> {
        let data = std::fs::read(self.path(rel)).with_context(|| format!("reading {rel}"))?;
        codec::decode_nopus(&data)
    }

    pub fn voice(&self, name: &str) -> Result<Pcm> {
        self.load_stream_file(&format!("stream/{name}.opus"))
    }
}

/// Upload a BNTX texture as a sprite texture (through pre_alter, `Renderer::upload_altered`; the exe builds every
/// image slot with sub_A3180), baked at scale `k` (1 for the UI atlases).
pub fn upload_bntx(r: &Renderer, t: &bntx::Texture, k: f32) -> Result<TexRef> {
    let (label, srgb) = (&t.name, t.format.is_srgb());
    Ok(match t.format {
        TexFormat::Bc7Unorm | TexFormat::Bc7Srgb if r.bc_supported => r.upload_altered(t.width, t.height, Upload::Bc7(&t.data), srgb, k, None, label),
        TexFormat::Bc1Unorm | TexFormat::Bc1Srgb if r.bc_supported => r.upload_altered(t.width, t.height, Upload::Bc1(&t.data), srgb, k, None, label),
        TexFormat::Bc3Unorm | TexFormat::Bc3Srgb if r.bc_supported => r.upload_altered(t.width, t.height, Upload::Bc3(&t.data), srgb, k, None, label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.upload_altered(t.width, t.height, Upload::Rgba8(&rgba), srgb, k, None, label)
        }
    })
}

/// Upload a BNTX texture to be sampled as stored (rule masks).
fn upload_bntx_raw(r: &Renderer, t: &bntx::Texture) -> Result<TexRef> {
    let label = &t.name;
    Ok(match t.format {
        TexFormat::Bc7Unorm | TexFormat::Bc7Srgb if r.bc_supported => r.upload(t.width, t.height, Upload::Bc7(&t.data), label),
        TexFormat::Bc1Unorm | TexFormat::Bc1Srgb if r.bc_supported => r.upload(t.width, t.height, Upload::Bc1(&t.data), label),
        TexFormat::Bc3Unorm | TexFormat::Bc3Srgb if r.bc_supported => r.upload(t.width, t.height, Upload::Bc3(&t.data), label),
        TexFormat::R8Unorm => r.upload(t.width, t.height, Upload::R8(&t.data), label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.upload(t.width, t.height, Upload::Rgba8(&rgba), label)
        }
    })
}

/// Decode an archive image (BNTX, JPEG or PNG) to RGBA8: (width, height, pixels, sampled through an sRGB view).
/// sub_A3180 uploads JPEG / PNG decodes as "Jpeg Tex" / "Png Tex" (sub_E0CD8 format 0 -> 2822, sRGB).
fn decode_image(data: &[u8]) -> Result<(u32, u32, Vec<u8>, bool)> {
    if bntx::is_bntx(data) {
        let t = bntx::parse(data)?.into_iter().next().context("empty BNTX")?;
        return Ok((t.width, t.height, bntx_decode::to_rgba8(&t)?, t.format.is_srgb()));
    }
    let img = image::load_from_memory(data).context("decoding image")?.to_rgba8();
    Ok((img.width(), img.height(), img.into_raw(), true))
}

fn sysmes_u32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn sysmes_u64(d: &[u8], o: usize) -> Option<usize> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?) as usize)
}


