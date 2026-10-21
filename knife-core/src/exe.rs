//! Data tables compiled into the game executable (`exefs/main`), for build ID `f4b3318d561e0345`.
//!
//! knife does not read the executable: `kdu exe-tables` copies the address ranges in `BUNDLED` out of the
//! uncompressed NSO into a zlib-compressed bundle (`knife/data/exe_tables.bin`) that is compiled into knife, and
//! `Nso::from_bundle` serves them by virtual address as the NSO would.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};

pub const BUILD_ID_PREFIX: [u8; 8] = [0xf4, 0xb3, 0x31, 0x8d, 0x56, 0x03, 0x1e, 0x45];

const IMAGE_NAMES: u64 = 0x5e02a4; // [u8; 32] records
const IMAGE_INFO: u64 = 0x46f830; // 72-byte records
/// STRA haze parameters (sub_91520): 10 floats per id, ids 1..=557.
const HAZE: u64 = 0x352bd4;
pub const HAZE_COUNT: usize = 557;
const IMAGE_COUNT: usize = 14713;
const SCENE_IDS: u64 = 0x350D58; // u16
const SCENE_ID_COUNT: usize = 549;

/// STMA motion keys (64-byte records), motion key ends and lengths (u32 per motion id) and field masks
/// (u64 per motion id, `qword_36AB38`, sub_99960).
pub const MOTION_KEYS: u64 = 0x65CE18;
pub const MOTION_KEY_COUNT: usize = 0x118E1;
pub const MOTION_ENDS: u64 = 0x3B1940;
pub const MOTION_LENS: u64 = 0x399F40;
pub const MOTION_COUNT: usize = 0xBD * 128;
pub const MOTION_MASKS: u64 = 0x36AB38;
pub const MOTION_MASK_COUNT: usize = 0x5E81;

/// Every executable range knife reads: (virtual address, length).
pub const BUNDLED: &[(u64, usize)] = &[
    (IMAGE_NAMES, 32 * IMAGE_COUNT),
    (IMAGE_INFO, 72 * IMAGE_COUNT),
    (SCENE_IDS, 2 * SCENE_ID_COUNT),
    (HAZE, 40 * (HAZE_COUNT + 1)),
    (MOTION_KEYS, 64 * MOTION_KEY_COUNT),
    (MOTION_ENDS, 4 * MOTION_COUNT),
    (MOTION_LENS, 4 * MOTION_COUNT),
    (MOTION_MASKS, 8 * MOTION_MASK_COUNT),
];

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

    /// The `BUNDLED` ranges of this executable, zlib-compressed: magic, build ID, range count (u32), then per
    /// range its address (u64), length (u32) and bytes.
    pub fn bundle(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(BUNDLE_MAGIC);
        out.extend_from_slice(&self.build_id);
        out.extend_from_slice(&(BUNDLED.len() as u32).to_le_bytes());
        for &(va, len) in BUNDLED {
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
    pub images: Vec<ImageInfo>,
    pub scene_ids: Vec<u16>,
    /// Indexed by STRA id; entry 0 is unused.
    pub haze: Vec<[f32; 10]>,
}

impl ExeTables {
    pub fn load(nso: &Nso) -> Result<Self> {
        if nso.build_id()[..8] != BUILD_ID_PREFIX {
            bail!(
                "unsupported executable build {:02x?}; this reimplementation targets {:02x?}",
                &nso.build_id()[..8],
                BUILD_ID_PREFIX
            );
        }
        let mut images = Vec::with_capacity(IMAGE_COUNT);
        for i in 0..IMAGE_COUNT {
            let name_raw = nso
                .bytes(IMAGE_NAMES + 32 * i as u64, 32)
                .context("image name table out of range")?;
            let end = name_raw.iter().position(|&b| b == 0).unwrap_or(32);
            let name = String::from_utf8_lossy(&name_raw[..end]).into_owned();
            let rec = IMAGE_INFO + 72 * i as u64;
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
            });
        }
        let scene_ids = (0..SCENE_ID_COUNT)
            .map(|i| nso.u16(SCENE_IDS + 2 * i as u64).unwrap_or(0xFFFF))
            .collect();
        let haze = (0..=HAZE_COUNT)
            .map(|id| std::array::from_fn(|k| nso.f32(HAZE + 40 * id as u64 + 4 * k as u64).unwrap_or(0.0)))
            .collect();
        Ok(Self { images, scene_ids, haze })
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

    /// The bundled ranges read back as the executable's own bytes (needs `switch_exefs/uncompressed_main`).
    #[test]
    fn bundle_matches_executable() {
        let Ok(data) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../switch_exefs/uncompressed_main")) else {
            return;
        };
        let nso = Nso::parse(data).unwrap();
        let bundled = Nso::from_bundle(include_bytes!("../../knife/data/exe_tables.bin")).unwrap();
        assert_eq!(bundled.build_id(), nso.build_id());
        for &(va, len) in BUNDLED {
            assert_eq!(bundled.bytes(va, len), nso.bytes(va, len), "{va:#x}");
        }
    }
}
