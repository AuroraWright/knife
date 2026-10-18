//! Data tables compiled into the game executable (`exefs/main`).
//!
//! These cannot be shipped, so they are read from the user's own copy of the
//! (uncompressed) NSO at startup. Addresses are for build ID `f4b3318d561e0345`.

use anyhow::{Context, Result, bail};

pub const BUILD_ID_PREFIX: [u8; 8] = [0xf4, 0xb3, 0x31, 0x8d, 0x56, 0x03, 0x1e, 0x45];

const IMAGE_NAMES: u64 = 0x5e02a4; // [u8; 32] records
const IMAGE_INFO: u64 = 0x46f830; // 72-byte records
const IMAGE_COUNT: usize = 14713;
const SCENE_IDS: u64 = 0x350D58; // u16
const SCENE_ID_COUNT: usize = 549;

/// Read-only view of an uncompressed NSO's segments by virtual address.
pub struct Nso {
    data: Vec<u8>,
    segs: [(u64, u64, u64); 3], // (file_off, vaddr, size)
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
        let mut segs = [(0, 0, 0); 3];
        for (i, s) in segs.iter_mut().enumerate() {
            let o = 0x10 + i * 0x10;
            let r = |k: usize| u32::from_le_bytes(data[o + k..o + k + 4].try_into().unwrap()) as u64;
            *s = (r(0), r(4), r(8));
        }
        Ok(Self { data, segs })
    }

    pub fn build_id(&self) -> &[u8] {
        &self.data[0x40..0x60]
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
}

pub struct ExeTables {
    pub images: Vec<ImageInfo>,
    pub scene_ids: Vec<u16>,
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
            });
        }
        let scene_ids = (0..SCENE_ID_COUNT)
            .map(|i| nso.u16(SCENE_IDS + 2 * i as u64).unwrap_or(0xFFFF))
            .collect();
        Ok(Self { images, scene_ids })
    }

    pub fn scene_index(&self, id: u16) -> Option<usize> {
        self.scene_ids.iter().position(|&s| s == id)
    }
}
