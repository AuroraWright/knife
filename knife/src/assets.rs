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
    /// turns about.
    pub fn anchor(&self) -> [f32; 2] {
        [self.center_x.trunc(), self.center_y.trunc()]
    }
}

/// Decoded pixels of an image, ready to upload.
pub enum Pixels {
    Bntx(bntx::Texture),
    Rgba(u32, u32, Vec<u8>),
}

/// An image `Assets::decode_async` read on a worker thread.
pub struct Decoded {
    key: String,
    name: String,
    strip: Option<(usize, usize)>,
    pixels: Result<Pixels>,
}

impl Decoded {
    /// The image's cache key (`Assets::image_key`).
    pub fn key(&self) -> &str {
        &self.key
    }
}

pub struct Assets {
    pub romfs: PathBuf,
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
    pub allpaccg: Option<Arc<Archive>>,
    pub allui: Arc<Archive>,
    pub tables: ExeTables,
    image_index: HashMap<String, usize>,
    pub scenes: SceneArchive,
    pub text: ScriptText,
    pub sar: SoundArchive,
    parsed: HashMap<usize, Arc<Scene>>,
    images: HashMap<String, Option<Arc<Image>>>,
    sounds: HashMap<String, Option<Arc<Pcm>>>,
}

/// The build a romfs belongs to: the overseas data names its archives `w_allpac` / `w_allui` / ... (sub_AE050).
pub fn detect_build(romfs: &Path) -> Build {
    if romfs.join("w_allpac.nam").exists() { Build::Overseas } else { Build::Japanese }
}

/// The executable's tables for a build, compiled in (`kdu exe-tables` writes them; knife_core::exe).
pub fn exe_tables(build: Build) -> Result<Nso> {
    Nso::from_bundle(match build {
        Build::Japanese => include_bytes!("../data/exe_tables.bin"),
        Build::Overseas => include_bytes!("../data/exe_tables_overseas.bin"),
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

/// The SYSMES message the overseas build inserts at 217 (the "deleted" popup, `jp_50FC0` state 25). knife keeps
/// the Japanese numbering and moves it here, past the Japanese build's 1498 messages.
pub const SYSMES_DELETED: usize = 1498;
const SYSMES_OVERSEAS_INSERT: usize = 217;

/// The loaded executable is the overseas build (for tables that differ between the builds).
static OVERSEAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn overseas() -> bool {
    OVERSEAS.load(std::sync::atomic::Ordering::Relaxed)
}

impl Assets {
    /// `language` is the config word (`cfg::LANGUAGE`); `language()` resolves it like sub_2DE70.
    pub fn open(romfs: &Path, nso: &Nso, language: i16) -> Result<Self> {
        let tables = ExeTables::load(nso)?;
        let build = tables.build;
        // sub_AE050: the overseas archives allpac / allpacml / allui / allpaccg carry a `w_` prefix
        let w = |base: &str| match build {
            Build::Overseas => format!("w_{base}"),
            Build::Japanese => base.to_string(),
        };
        let image_index = tables
            .images
            .iter()
            .enumerate()
            .map(|(i, im)| (im.name.to_ascii_uppercase(), i))
            .collect();
        let scenes = SceneArchive::parse(&std::fs::read(romfs.join("allscr.mrg")).context("reading allscr.mrg")?)?;
        let script_text = std::fs::read(romfs.join("script_text.mrg")).context("reading script_text.mrg")?;
        let sar = SoundArchive::parse(std::fs::read(romfs.join("knife1.bfsar")).context("reading knife1.bfsar")?)?;
        let allui = Arc::new(Archive::open(romfs, &w("allui"))?);
        let languages = Self::sysmes_file(&allui, build).map_or(1, |d| sysmes_u32(&d, 0).unwrap_or(1).max(1) as usize);
        let mut a = Self {
            romfs: romfs.to_path_buf(),
            build,
            language: 0,
            languages,
            text: ScriptText::parse(&script_text, 0)?,
            script_text,
            allpac: Arc::new(Archive::open(romfs, &w("allpac"))?),
            allpacml: Archive::open(romfs, &w("allpacml")).ok().map(Arc::new),
            allpachd: Archive::open(romfs, "allpachd").ok().map(Arc::new),
            allpaccg: Archive::open(romfs, &w("allpaccg")).ok().map(Arc::new),
            allui,
            tables,
            image_index,
            scenes,
            sar,
            parsed: HashMap::new(),
            images: HashMap::new(),
            sounds: HashMap::new(),
        };
        crate::text::set_line_break(a.tables.line_break.clone());
        OVERSEAS.store(a.build == Build::Overseas, std::sync::atomic::Ordering::Relaxed);
        a.set_language(language)?;
        Ok(a)
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
        let (msg, bubble) = self.font_metrics();
        crate::text::set_font_metrics(msg.line_feed, msg.ascent, msg.baseline, bubble.ascent);
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

    /// Load (and cache) an image built with an STEC colour grade and the SNX2 / SNX3 tone (`grade::Bake`). The
    /// exe keys its texture cache (sub_AAA40) the same way, by image and the pre_alter parameters.
    pub fn image_graded(&mut self, r: &Renderer, name: &str, bake: Bake) -> Option<Arc<Image>> {
        let (name, grade, key) = Self::image_key(name, bake);
        if let Some(i) = self.images.get(&key) {
            return i.clone();
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

    /// Read and decode an image for `image_graded` on a worker thread; the result arrives on `done`, for
    /// `finish_decoded` to upload and cache. Returns the cache key.
    pub fn decode_async(&self, name: &str, bake: Bake, done: std::sync::mpsc::Sender<Decoded>) {
        let (name, grade, k) = Self::image_key(name, bake);
        let archives = self.image_archives();
        let strip = self.strip(&name);
        let spawned = std::thread::Builder::new().name("image decode".into()).spawn(move || {
            let pixels = Self::decode_pixels(&archives, &name, grade, strip);
            let _ = done.send(Decoded { key: k, name, strip, pixels });
        });
        if let Err(e) = spawned {
            tracing::warn!("image decode thread: {e}");
        }
    }

    /// Upload and cache an image `decode_async` read.
    pub fn finish_decoded(&mut self, r: &Renderer, d: Decoded) {
        // a language change while it decoded makes a language-strip image stale
        if !self.images.contains_key(&d.key) && d.strip == self.strip(&d.name) {
            self.finish_image(r, d.key, &d.name, d.pixels);
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
        self.images.insert(key, img.clone());
        img
    }

    /// Drop cached images nobody else references.
    pub fn trim_images(&mut self, keep: usize) {
        if self.images.len() <= keep {
            return;
        }
        self.images.retain(|_, v| v.as_ref().is_some_and(|i| Arc::strong_count(i) > 1));
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
        if grade.is_none() && strip.is_none() && bntx::is_bntx(&data) {
            return Ok(Pixels::Bntx(bntx::parse(&data)?.into_iter().next().context("empty BNTX")?));
        }
        let (w, mut h, mut px) = decode_image(&data)?;
        if let Some((k, n)) = strip {
            let rows = h / n as u32;
            let top = (h as usize * k / n) * w as usize * 4;
            px = px[top..top + (rows * w) as usize * 4].to_vec();
            h = rows;
        }
        grade.apply(&mut px);
        Ok(Pixels::Rgba(w, h, px))
    }

    fn upload_pixels(&self, r: &Renderer, name: &str, pixels: Pixels) -> Result<Image> {
        let tex = match pixels {
            Pixels::Bntx(t) => upload_bntx(r, &t)?,
            Pixels::Rgba(w, h, px) => r.upload(w, h, Upload::Rgba8(&px), name),
        };
        let no_shake = self.image_info(name).is_some_and(|i| i.no_shake);
        let (width, height, center_x, center_y, level) = match self.image_info(name) {
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
            let tex = upload_bntx(r, &t)?;
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
            let data = Archive::open(&self.romfs, "parts")?.read(index)?;
            let t = bntx::parse(&data)?.into_iter().next().context("empty BNTX")?;
            upload_bntx(r, &t)
        };
        load().map_err(|e| tracing::warn!("parts #{index}: {e:#}")).ok()
    }

    /// The SYSMES file: SYSMES_TEXT in the Japanese allui, SYSMES_TEXT_ML in the overseas w_allui (jp_9A420).
    fn sysmes_file(allui: &Archive, build: Build) -> Option<Vec<u8>> {
        let stem = match build {
            Build::Japanese => "SYSMES_TEXT",
            Build::Overseas => "SYSMES_TEXT_ML",
        };
        allui.read(allui.find_stem(stem)?).ok()
    }

    /// The game language's system strings, indexed by the Japanese build's ids (u32 language count, u32 count,
    /// u64 0, then per language a u64 offset of a table of u64 offsets of NUL-terminated UTF-8 strings; entry 0
    /// is the language's folder name). The overseas file's message 217 moves to `SYSMES_DELETED`.
    pub fn sysmes(&self) -> Vec<String> {
        let parse = || -> Option<Vec<String>> {
            let d = Self::sysmes_file(&self.allui, self.build)?;
            let n = sysmes_u32(&d, 4)? as usize;
            let table = sysmes_u64(&d, 0x10 + 8 * self.language)?;
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
    /// "2" the UI text, "3" the bubble text).
    pub fn font_path(&self, suffix: &str) -> Option<PathBuf> {
        let dir = self.romfs.join(format!("{}{suffix}", self.language_folder()));
        let mut files: Vec<PathBuf> =
            std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        files.into_iter().next()
    }

    /// The pair font metrics at the 40 px base size (sub_CAE88, `sfnt::pair_metrics`) of the message font set
    /// (the language's three fonts sub_AB0F0 loads) and of the bubble font (`<folder>3`), each paired with the
    /// exfont: line feed / ascent / baseline position Japanese 50 / 41 / 41, English 53 / 43 / 40 (bubble ascent
    /// 41), Simplified Chinese 49 / 39 / 39, Traditional Chinese 47 / 39 / 37 (the exfont's); the Japanese ones
    /// without font files.
    fn font_metrics(&self) -> (sfnt::SetMetrics, sfnt::SetMetrics) {
        use knife_core::format::{bffnt, bfttf};
        let read = |p: PathBuf| std::fs::read(p).ok().map(|d| bfttf::decode(&d).unwrap_or(d));
        let set = |suffixes: &[&str]| {
            let m: Vec<sfnt::Metrics> =
                suffixes.iter().filter_map(|s| self.font_path(s)).filter_map(read).filter_map(|d| sfnt::metrics(&d)).collect();
            sfnt::set_metrics(&m, crate::text::BASE_SIZE)
        };
        let ex = std::fs::read(self.romfs.join("exfont_nx01.bffnt"))
            .ok()
            .and_then(|d| bffnt::Font::parse(&d).ok())
            .map(|f| sfnt::SetMetrics {
                height: i32::from(f.height),
                ascent: i32::from(f.ascent),
                line_feed: i32::from(f.line_feed),
                baseline: i32::from(f.baseline),
            });
        let ja = sfnt::SetMetrics { height: 50, ascent: 41, line_feed: 50, baseline: 41 };
        let msg = set(&["", "2", "3"]).map_or(ja, |m| sfnt::pair_metrics(m, ex));
        let bubble = set(&["3"]).map_or(msg, |m| sfnt::pair_metrics(m, ex));
        (msg, bubble)
    }

    /// The console shared font sub_AB0F0 asks for (sub_C9F3C: nn::pl::GetSharedFont with the language code of
    /// dword_3C97F0[language]: ja / en-US share the standard font, zh-Hans / zh-Hant get the Chinese ones).
    pub fn shared_font_names(&self) -> &'static [&'static str] {
        match self.language {
            2 => &["nintendo_udsg-r_org_zh-cn_003.bfttf", "nintendo_udsg-r_std_003.bfttf"],
            3 => &["nintendo_udjxh-db_zh-tw_003.bfttf", "nintendo_udsg-r_std_003.bfttf"],
            _ => &["nintendo_udsg-r_std_003.bfttf"],
        }
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
        let res = self
            .sar
            .load_wave_sound(&format!("wsd_{key}"))
            .or_else(|_| self.sar.load_wave_sound(&key))
            .or_else(|_| self.load_stream_file(&format!("stream/{name}.opus")));
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
        let data = std::fs::read(self.romfs.join(rel)).with_context(|| format!("reading {rel}"))?;
        codec::decode_nopus(&data)
    }

    pub fn voice(&self, name: &str) -> Result<Pcm> {
        self.load_stream_file(&format!("stream/{name}.opus"))
    }
}

pub fn upload_bntx(r: &Renderer, t: &bntx::Texture) -> Result<TexRef> {
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

/// Decode an archive image (BNTX, JPEG or PNG) to RGBA8: (width, height, pixels).
fn decode_image(data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    if bntx::is_bntx(data) {
        let t = bntx::parse(data)?.into_iter().next().context("empty BNTX")?;
        return Ok((t.width, t.height, bntx_decode::to_rgba8(&t)?));
    }
    let img = image::load_from_memory(data).context("decoding image")?.to_rgba8();
    Ok((img.width(), img.height(), img.into_raw()))
}

fn sysmes_u32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn sysmes_u64(d: &[u8], o: usize) -> Option<usize> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?) as usize)
}
