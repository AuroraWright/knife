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

pub struct Assets {
    pub romfs: PathBuf,
    pub allpac: Archive,
    pub allpachd: Option<Archive>,
    pub allpaccg: Option<Archive>,
    pub allui: Archive,
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
            allpac: Archive::open(romfs, "allpac")?,
            allpachd: Archive::open(romfs, "allpachd").ok(),
            allpaccg: Archive::open(romfs, "allpaccg").ok(),
            allui: Archive::open(romfs, "allui")?,
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

    fn image_bytes(&self, name: &str) -> Option<Vec<u8>> {
        for a in [Some(&self.allpac), self.allpaccg.as_ref(), Some(&self.allui), self.allpachd.as_ref()]
            .into_iter()
            .flatten()
        {
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
        let key = name.to_ascii_uppercase();
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let img = match self.load_image(r, &key) {
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

    fn load_image(&self, r: &Renderer, name: &str) -> Result<Image> {
        let data = self.image_bytes(name).context("not found")?;
        let tex = upload_image(r, &data, name)?;
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

/// Upload an archive image (BNTX, JPEG or PNG).
pub fn upload_image(r: &Renderer, data: &[u8], name: &str) -> Result<TexRef> {
    if bntx::is_bntx(data) {
        let t = bntx::parse(data)?.into_iter().next().context("empty BNTX")?;
        return upload_bntx(r, &t);
    }
    let img = image::load_from_memory(data).context("decoding image")?.to_rgba8();
    Ok(r.upload(img.width(), img.height(), Upload::Rgba8(img.as_raw()), name))
}
