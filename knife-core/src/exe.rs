//! Data tables compiled into the game executable (`exefs/main`), for the Japanese build `f4b3318d561e0345` and the
//! overseas build `d10d239aade1d526` (knife1_overseas: English and Chinese text beside the Japanese).
//!
//! knife does not read the executable: `kdu exe-tables` copies the address ranges of `Layout::bundled` out of the
//! uncompressed NSO into a zlib-compressed bundle (`knife/data/exe_tables.bin`, `exe_tables_overseas.bin`) that is
//! compiled into knife, and `Nso::from_bundle` serves them by virtual address as the NSO would.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};

/// The two releases of the game. The overseas one is the same engine recompiled (NintendoSDK 17.5.4), so every
/// address differs; its romfs has `w_allpac` / `w_allpacml` / `w_allui` / `w_allpaccg` in place of the Japanese
/// archives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    Japanese,
    Overseas,
}

/// Where a build keeps the tables knife reads.
pub struct Layout {
    pub build: Build,
    pub build_id: [u8; 8],
    /// [u8; 32] image names and 72-byte image records
    image_names: u64,
    image_info: u64,
    image_count: usize,
    /// STRA haze parameters (sub_91520): 10 floats per id, ids 1..=557.
    haze: u64,
    /// u16 scene ids
    scene_ids: u64,
    /// STMA motion keys (64-byte records), motion key ends and lengths (u32 per motion id) and field masks
    /// (u64 per motion id, sub_99960; overseas sub_97070).
    pub motion_keys: u64,
    pub motion_ends: u64,
    pub motion_lens: u64,
    pub motion_masks: u64,
    /// nn::font's line breaking (sub_148988, UAX #14): u16 first code point, u8 length - 1, u8 class per range
    /// (LINE_BREAK_RANGES), and the 30 x 30 i32 class pair table it uses (a4 byte 5 clear; JP 0x3D3AE4)
    pub line_break_classes: u64,
    pub line_break_pairs: u64,
}

pub const HAZE_COUNT: usize = 557;
const SCENE_ID_COUNT: usize = 549;
pub const MOTION_KEY_COUNT: usize = 0x118E1;
pub const MOTION_COUNT: usize = 0xBD * 128;
pub const MOTION_MASK_COUNT: usize = 0x5E81;
/// sub_148988's binary search runs over entries 0..1986.
pub const LINE_BREAK_RANGES: usize = 1986;
pub const LINE_BREAK_CLASSES: usize = 30;

pub const JAPANESE: Layout = Layout {
    build: Build::Japanese,
    build_id: [0xf4, 0xb3, 0x31, 0x8d, 0x56, 0x03, 0x1e, 0x45],
    image_names: 0x5e02a4,
    image_info: 0x46f830,
    image_count: 14713,
    haze: 0x352bd4,
    scene_ids: 0x350D58,
    motion_keys: 0x65CE18,
    motion_ends: 0x3B1940,
    motion_lens: 0x399F40,
    motion_masks: 0x36AB38,
    line_break_classes: 0x3D0DCA,
    line_break_pairs: 0x3D3AE4,
};

/// The overseas build: 31 more images (IMG2638, IMG0570_09_09, IMG2638_01_01, IMG6000..IMG6026, IMG5030; the
/// records of the shared ones mostly unchanged), the same haze, scene id, motion end and length tables, and
/// motion keys / masks that differ in a few records.
pub const OVERSEAS: Layout = Layout {
    build: Build::Overseas,
    build_id: [0xd1, 0x0d, 0x23, 0x9a, 0xad, 0xe1, 0xd5, 0x26],
    image_names: 0x4CF9EC,
    image_info: 0x35F0B8,
    image_count: 14744,
    haze: 0x23761C,
    scene_ids: 0x235744,
    motion_keys: 0x54C940,
    motion_ends: 0x296388,
    motion_lens: 0x27E988,
    motion_masks: 0x24F580,
    line_break_classes: 0x2B58FC,
    line_break_pairs: 0x2B8614,
};

impl Layout {
    /// The layout of an executable by its build ID.
    pub fn of(build_id: &[u8]) -> Result<&'static Layout> {
        [&JAPANESE, &OVERSEAS].into_iter().find(|l| build_id.starts_with(&l.build_id)).with_context(|| {
            format!(
                "unsupported executable build {:02x?}; this reimplementation targets {:02x?} and {:02x?}",
                &build_id[..8.min(build_id.len())],
                JAPANESE.build_id,
                OVERSEAS.build_id
            )
        })
    }

    /// Every executable range knife reads: (virtual address, length).
    pub fn bundled(&self) -> [(u64, usize); 10] {
        [
            (self.image_names, 32 * self.image_count),
            (self.image_info, 72 * self.image_count),
            (self.scene_ids, 2 * SCENE_ID_COUNT),
            (self.haze, 40 * (HAZE_COUNT + 1)),
            (self.motion_keys, 64 * MOTION_KEY_COUNT),
            (self.motion_ends, 4 * MOTION_COUNT),
            (self.motion_lens, 4 * MOTION_COUNT),
            (self.motion_masks, 8 * MOTION_MASK_COUNT),
            (self.line_break_classes, 4 * LINE_BREAK_RANGES),
            (self.line_break_pairs, 4 * LINE_BREAK_CLASSES * LINE_BREAK_CLASSES),
        ]
    }
}

const BUNDLE_MAGIC: &[u8; 4] = b"KXT0";

/// Read-only view of an uncompressed NSO's segments (or of a bundle of its ranges) by virtual address.
pub struct Nso {
    data: Vec<u8>,
    segs: Vec<(u64, u64, u64)>, // (data offset, vaddr, size)
    build_id: [u8; 32],
}

impl Nso {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        if data.get(..4) != Some(b"NSO0") {
            bail!("not an NSO file");
        }
        let flags = u32::from_le_bytes(data[0xC..0x10].try_into()?);
        if flags & 7 != 0 {
            bail!("NSO is compressed; use the uncompressed main (e.g. from hactool --uncompressed)");
        }
        let segs = (0..3)
            .map(|i| {
                let o = 0x10 + i * 0x10;
                let r = |k: usize| u32::from_le_bytes(data[o + k..o + k + 4].try_into().unwrap()) as u64;
                (r(0), r(4), r(8))
            })
            .collect();
        let build_id = data[0x40..0x60].try_into()?;
        Ok(Self { data, segs, build_id })
    }

    /// The `Layout::bundled` ranges of this executable, zlib-compressed: magic, build ID, range count (u32),
    /// then per range its address (u64), length (u32) and bytes.
    pub fn bundle(&self) -> Result<Vec<u8>> {
        let ranges = self.layout()?.bundled();
        let mut out = Vec::new();
        out.extend_from_slice(BUNDLE_MAGIC);
        out.extend_from_slice(&self.build_id);
        out.extend_from_slice(&(ranges.len() as u32).to_le_bytes());
        for (va, len) in ranges {
            let b = self.bytes(va, len).with_context(|| format!("range {va:#x}+{len:#x} out of the executable"))?;
            out.extend_from_slice(&va.to_le_bytes());
            out.extend_from_slice(&(len as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&out)?;
        Ok(z.finish()?)
    }

    /// The ranges written by `bundle`.
    pub fn from_bundle(z: &[u8]) -> Result<Self> {
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(z).read_to_end(&mut raw).context("inflating the executable tables")?;
        if raw.get(..4) != Some(BUNDLE_MAGIC.as_slice()) || raw.len() < 40 {
            bail!("not an executable table bundle");
        }
        let build_id = raw[4..36].try_into()?;
        let n = u32::from_le_bytes(raw[36..40].try_into()?) as usize;
        let mut segs = Vec::with_capacity(n);
        let mut data = Vec::new();
        let mut o = 40;
        for _ in 0..n {
            let head = raw.get(o..o + 12).context("truncated bundle")?;
            let va = u64::from_le_bytes(head[..8].try_into()?);
            let len = u32::from_le_bytes(head[8..].try_into()?) as usize;
            let body = raw.get(o + 12..o + 12 + len).context("truncated bundle")?;
            segs.push((data.len() as u64, va, len as u64));
            data.extend_from_slice(body);
            o += 12 + len;
        }
        Ok(Self { data, segs, build_id })
    }

    pub fn build_id(&self) -> &[u8] {
        &self.build_id
    }

    /// The table layout of this executable's build.
    pub fn layout(&self) -> Result<&'static Layout> {
        Layout::of(&self.build_id)
    }

    pub fn bytes(&self, va: u64, len: usize) -> Option<&[u8]> {
        for &(f, v, s) in &self.segs {
            if va >= v && va + len as u64 <= v + s {
                let o = (f + va - v) as usize;
                return self.data.get(o..o + len);
            }
        }
        None
    }

    pub fn u16(&self, va: u64) -> Option<u16> {
        Some(u16::from_le_bytes(self.bytes(va, 2)?.try_into().ok()?))
    }

    pub fn u64(&self, va: u64) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(va, 8)?.try_into().ok()?))
    }

    pub fn f32(&self, va: u64) -> Option<f32> {
        Some(f32::from_le_bytes(self.bytes(va, 4)?.try_into().ok()?))
    }

    pub fn cstr(&self, va: u64) -> Option<String> {
        for &(f, v, s) in &self.segs {
            if va >= v && va < v + s {
                let o = (f + va - v) as usize;
                let end = self.data[o..].iter().position(|&b| b == 0)?;
                return Some(String::from_utf8_lossy(&self.data[o..o + end]).into_owned());
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub name: String,
    /// Display size and anchor in 640x362 screen units.
    pub width: f32,
    pub height: f32,
    pub center_x: f32,
    pub center_y: f32,
    pub kind: u8,
    /// +67 bit 0: the name ends in a `_aa_bb` variant suffix
    pub variant: bool,
    /// +70 (sub_2DFD0): size level; pivots and SBAS children of the image scale by 1 / 2^level (1..3)
    pub level: i16,
    /// +65 bit 1: the screen shake does not move the image (sub_34440 / sub_32050; the colour fills 0..6)
    pub no_shake: bool,
    /// +65 bit 0: STCH leaves the layer without the default blend F982 (cmd_STCH_impl 0x60840; the colour fills,
    /// and in the overseas build its text images)
    pub own_blend: bool,
    /// Overseas +67 bit 1 (sub_77330 there): the texture stacks one copy per game language (sub_9A700 strips,
    /// top to bottom in language order); a layer draws strip `language` (sub_374F0 / sub_375D0, sub_34440 and
    /// the save preview sub_89AD0 divide the height by the strip count). The Japanese build has no such images
    /// (its +67 bit 1, which no image sets, is the HD art flag that the overseas build moved to bit 2).
    pub language_strips: bool,
}

impl ImageInfo {
    /// sub_2DFD0's scale: level 1 / 2 / 3 -> 0.5 / 0.25 / 0.125, else 1.
    pub fn level_scale(&self) -> f32 {
        match self.level {
            1 => 0.5,
            2 => 0.25,
            3 => 0.125,
            _ => 1.0,
        }
    }
}

pub struct ExeTables {
    pub build: Build,
    pub images: Vec<ImageInfo>,
    pub scene_ids: Vec<u16>,
    /// Indexed by STRA id; entry 0 is unused.
    pub haze: Vec<[f32; 10]>,
    pub line_break: std::sync::Arc<crate::linebreak::LineBreak>,
}

impl ExeTables {
    pub fn load(nso: &Nso) -> Result<Self> {
        let l = nso.layout()?;
        let mut images = Vec::with_capacity(l.image_count);
        for i in 0..l.image_count {
            let name_raw = nso
                .bytes(l.image_names + 32 * i as u64, 32)
                .context("image name table out of range")?;
            let end = name_raw.iter().position(|&b| b == 0).unwrap_or(32);
            let name = String::from_utf8_lossy(&name_raw[..end]).into_owned();
            let rec = l.image_info + 72 * i as u64;
            let f = |o: u64| nso.f32(rec + o).unwrap_or(0.0);
            images.push(ImageInfo {
                name,
                width: f(44),
                height: f(48),
                center_x: f(52),
                center_y: f(56),
                kind: nso.bytes(rec + 68, 1).map(|b| b[0]).unwrap_or(0),
                variant: nso.bytes(rec + 67, 1).is_some_and(|b| b[0] & 1 != 0),
                level: nso.u16(rec + 70).map_or(0, |v| v as i16),
                no_shake: nso.bytes(rec + 65, 1).is_some_and(|b| b[0] & 2 != 0),
                own_blend: nso.bytes(rec + 65, 1).is_some_and(|b| b[0] & 1 != 0),
                language_strips: l.build == Build::Overseas && nso.bytes(rec + 67, 1).is_some_and(|b| b[0] & 2 != 0),
            });
        }
        let scene_ids = (0..SCENE_ID_COUNT)
            .map(|i| nso.u16(l.scene_ids + 2 * i as u64).unwrap_or(0xFFFF))
            .collect();
        let haze = (0..=HAZE_COUNT)
            .map(|id| std::array::from_fn(|k| nso.f32(l.haze + 40 * id as u64 + 4 * k as u64).unwrap_or(0.0)))
            .collect();
        let line_break = std::sync::Arc::new(crate::linebreak::LineBreak::load(nso)?);
        Ok(Self { build: l.build, images, scene_ids, haze, line_break })
    }

    /// Haze entry for a STRA id (sub_91520: ids outside 1..=557 have none).
    pub fn haze(&self, id: i32) -> Option<&[f32; 10]> {
        if (1..=HAZE_COUNT as i32).contains(&id) { self.haze.get(id as usize) } else { None }
    }

    pub fn scene_index(&self, id: u16) -> Option<usize> {
        self.scene_ids.iter().position(|&s| s == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled ranges read back as the executable's own bytes (needs `switch_exefs/uncompressed_main` and
    /// `switch_exefs_eng/uncompressed_main`).
    #[test]
    fn bundle_matches_executable() {
        let builds: [(&str, &[u8]); 2] = [
            ("switch_exefs", include_bytes!("../../knife/data/exe_tables.bin")),
            ("switch_exefs_eng", include_bytes!("../../knife/data/exe_tables_overseas.bin")),
        ];
        for (dir, bundle) in builds {
            let path = format!("{}/../{dir}/uncompressed_main", env!("CARGO_MANIFEST_DIR"));
            let Ok(data) = std::fs::read(path) else { continue };
            let nso = Nso::parse(data).unwrap();
            let bundled = Nso::from_bundle(bundle).unwrap();
            assert_eq!(bundled.build_id(), nso.build_id());
            for (va, len) in nso.layout().unwrap().bundled() {
                assert_eq!(bundled.bytes(va, len), nso.bytes(va, len), "{dir} {va:#x}");
            }
        }
    }
}
