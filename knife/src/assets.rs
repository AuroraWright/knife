//! Game data access: archives, exe tables, scripts, sound archive and a GPU texture cache.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use knife_audio::{Pcm, bfsar::SoundArchive, codec};
use knife_core::{
    exe::{ExeTables, ImageInfo, Nso},
    format::{
        archive::Archive,
        bntx::{self, TexFormat},
        bntx_decode,
        script::{SceneArchive, ScriptText},
    },
    vm::parse::Scene,
};
use knife_render::{Renderer, TexRef, Upload};

/// A texture together with its display metrics.
pub struct Image {
    pub tex: TexRef,
    /// Display size and centre in 640x362 units.
    pub width: f32,
    pub height: f32,
    pub center_x: f32,
    pub center_y: f32,
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
    pub allpac: Arc<Archive>,
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

fn find_main_nso(exefs: &Path) -> Result<Vec<u8>> {
    let path = exefs.join("uncompressed_main");
    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

impl Assets {
    pub fn open(romfs: &Path, exefs: &Path) -> Result<Self> {
        let nso = Nso::parse(find_main_nso(exefs)?)?;
        let tables = ExeTables::load(&nso)?;
        let image_index = tables
            .images
            .iter()
            .enumerate()
            .map(|(i, im)| (im.name.to_ascii_uppercase(), i))
            .collect();
        let scenes = SceneArchive::parse(&std::fs::read(romfs.join("allscr.mrg")).context("reading allscr.mrg")?)?;
        let text = ScriptText::parse(&std::fs::read(romfs.join("script_text.mrg")).context("reading script_text.mrg")?, 0)?;
        let sar = SoundArchive::parse(std::fs::read(romfs.join("knife1.bfsar")).context("reading knife1.bfsar")?)?;
        Ok(Self {
            romfs: romfs.to_path_buf(),
            allpac: Arc::new(Archive::open(romfs, "allpac")?),
            allpachd: Archive::open(romfs, "allpachd").ok().map(Arc::new),
            allpaccg: Archive::open(romfs, "allpaccg").ok().map(Arc::new),
            allui: Arc::new(Archive::open(romfs, "allui")?),
            tables,
            image_index,
            scenes,
            text,
            sar,
            parsed: HashMap::new(),
            images: HashMap::new(),
            sounds: HashMap::new(),
        })
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

    pub fn image_info(&self, name: &str) -> Option<&ImageInfo> {
        let upper = name.to_ascii_uppercase();
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

    /// The image archives in lookup order.
    fn image_archives(&self) -> Vec<Arc<Archive>> {
        [Some(&self.allpac), self.allpaccg.as_ref(), Some(&self.allui), self.allpachd.as_ref()]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    fn image_bytes(archives: &[Arc<Archive>], name: &str) -> Option<Vec<u8>> {
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
        self.image_graded(r, name, 0)
    }

    /// Load (and cache) an image with an STEC colour grade applied (`grade::apply`; 0 = none). The exe keys
    /// its texture cache (sub_AAA40) the same way, by image and STEC mode.
    pub fn image_graded(&mut self, r: &Renderer, name: &str, grade: i32) -> Option<Arc<Image>> {
        let (name, grade, key) = Self::image_key(name, grade);
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let pixels = Self::decode_pixels(&self.image_archives(), &name, grade);
        self.finish_image(r, key, &name, pixels)
    }

    /// Cache key of an image load: (upper-case name, effective grade, key).
    pub fn image_key(name: &str, grade: i32) -> (String, i32, String) {
        let grade = if crate::grade::is_grade(grade) { grade } else { 0 };
        let name = name.to_ascii_uppercase();
        let key = if grade == 0 { name.clone() } else { format!("{name}#{grade}") };
        (name, grade, key)
    }

    /// Whether `image_graded(name, grade)` would return without loading anything.
    pub fn image_cached(&self, name: &str, grade: i32) -> bool {
        self.images.contains_key(&Self::image_key(name, grade).2)
    }

    /// Read and decode an image for `image_graded` on a worker thread; the result arrives on `done`, for
    /// `finish_decoded` to upload and cache. Returns the cache key.
    pub fn decode_async(&self, name: &str, grade: i32, done: std::sync::mpsc::Sender<Decoded>) {
        let (name, grade, k) = Self::image_key(name, grade);
        let archives = self.image_archives();
        let spawned = std::thread::Builder::new().name("image decode".into()).spawn(move || {
            let pixels = Self::decode_pixels(&archives, &name, grade);
            let _ = done.send(Decoded { key: k, name, pixels });
        });
        if let Err(e) = spawned {
            tracing::warn!("image decode thread: {e}");
        }
    }

    /// Upload and cache an image `decode_async` read.
    pub fn finish_decoded(&mut self, r: &Renderer, d: Decoded) {
        if !self.images.contains_key(&d.key) {
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

    /// The CPU side of an image load (no renderer needed): archive read, BNTX parse or image decode, grade.
    fn decode_pixels(archives: &[Arc<Archive>], name: &str, grade: i32) -> Result<Pixels> {
        let data = Self::image_bytes(archives, name).context("not found")?;
        if grade == 0 && bntx::is_bntx(&data) {
            return Ok(Pixels::Bntx(bntx::parse(&data)?.into_iter().next().context("empty BNTX")?));
        }
        let (w, h, mut px) = decode_image(&data)?;
        if grade != 0 {
            crate::grade::apply(grade, &mut px);
        }
        Ok(Pixels::Rgba(w, h, px))
    }

    fn upload_pixels(&self, r: &Renderer, name: &str, pixels: Pixels) -> Result<Image> {
        let tex = match pixels {
            Pixels::Bntx(t) => upload_bntx(r, &t)?,
            Pixels::Rgba(w, h, px) => r.upload(w, h, Upload::Rgba8(&px), name),
        };
        let (width, height, center_x, center_y) = match self.image_info(name) {
            Some(i) => (i.width, i.height, i.center_x, i.center_y),
            None => {
                // 1280x720 textures map to 640 units
                let w = tex.width as f32 / 2.0;
                let h = tex.height as f32 / 2.0 * (362.0 / 360.0);
                (w, h, w / 2.0, h / 2.0)
            }
        };
        Ok(Image {
            tex,
            width,
            height,
            center_x,
            center_y,
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
        Ok(out)
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

    /// SYSMES_TEXT.DAT: system strings indexed by id (u32 lang count, u32 count, u64 0, u64 table offset,
    /// then u64 offsets of NUL-terminated UTF-8 strings; entry 0 is the language code).
    pub fn sysmes(&self) -> Vec<String> {
        let parse = || -> Option<Vec<String>> {
            let d = self.allui.read(self.allui.find_stem("SYSMES_TEXT")?).ok()?;
            let u32_at = |o: usize| Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?));
            let u64_at = |o: usize| Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?) as usize);
            let n = u32_at(4)? as usize;
            let table = u64_at(0x10)?;
            (0..n)
                .map(|i| {
                    let o = u64_at(table + 8 * i)?;
                    let end = d.get(o..)?.iter().position(|&b| b == 0)?;
                    Some(String::from_utf8_lossy(&d[o..o + end]).into_owned())
                })
                .collect()
        };
        parse().unwrap_or_default()
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

    pub fn load_stream_file(&self, rel: &str) -> Result<Pcm> {
        let data = std::fs::read(self.romfs.join(rel)).with_context(|| format!("reading {rel}"))?;
        codec::decode_nopus(&data)
    }

    /// BGM for `MPLY(n)`: BFSAR sound item 0x1000000 + (n - 1).
    pub fn bgm(&self, n: i32) -> Result<(Pcm, f32)> {
        if n <= 0 {
            bail!("no bgm {n}");
        }
        let s = self.sar.stream_sound(n as usize - 1)?;
        let mut pcm = self.load_stream_file(&s.path)?;
        if let Some((a, b)) = s.loop_range {
            pcm.loop_range = Some((a, b.min(pcm.frames.len())));
        } else {
            pcm.loop_range = Some((0, pcm.frames.len()));
        }
        Ok((pcm, s.volume))
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

