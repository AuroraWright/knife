//! BNTX texture containers (Switch native). Only what the game uses is supported:
//! 2D textures, mip 0, block-linear (swizzled) layout. Almost every texture is
//! BC7 sRGB (format `0x2006`).

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    Rgba8Unorm,
    Rgba8Srgb,
    Bc1Srgb,
    Bc1Unorm,
    Bc3Srgb,
    Bc3Unorm,
    Bc7Srgb,
    Bc7Unorm,
    /// single channel (rule images)
    Bc4Unorm,
    R8Unorm,
}

impl TexFormat {
    fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0x0b01 => Self::Rgba8Unorm,
            0x0b06 => Self::Rgba8Srgb,
            0x1a01 => Self::Bc1Unorm,
            0x1a06 => Self::Bc1Srgb,
            0x1c01 => Self::Bc3Unorm,
            0x1c06 => Self::Bc3Srgb,
            0x2001 => Self::Bc7Unorm,
            0x2006 => Self::Bc7Srgb,
            0x1d01 => Self::Bc4Unorm,
            0x0201 => Self::R8Unorm,
            _ => return None,
        })
    }

    /// (block width/height in pixels, bytes per block)
    pub fn block(self) -> (u32, u32) {
        match self {
            Self::Rgba8Unorm | Self::Rgba8Srgb => (1, 4),
            Self::R8Unorm => (1, 1),
            Self::Bc1Srgb | Self::Bc1Unorm | Self::Bc4Unorm => (4, 8),
            Self::Bc3Srgb | Self::Bc3Unorm | Self::Bc7Srgb | Self::Bc7Unorm => (4, 16),
        }
    }

    pub fn is_srgb(self) -> bool {
        matches!(
            self,
            Self::Rgba8Srgb | Self::Bc1Srgb | Self::Bc3Srgb | Self::Bc7Srgb
        )
    }
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub format: TexFormat,
    pub width: u32,
    pub height: u32,
    /// Linear (deswizzled) mip-0 data in the texture's native block format.
    pub data: Vec<u8>,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

pub fn is_bntx(data: &[u8]) -> bool {
    data.len() > 0x40 && &data[..4] == b"BNTX"
}

pub fn parse(data: &[u8]) -> Result<Vec<Texture>> {
    if !is_bntx(data) || &data[0x20..0x24] != b"NX  " {
        bail!("not a BNTX file");
    }
    let count = u32_at(data, 0x24) as usize;
    let info_ptrs = u64_at(data, 0x28) as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let brti = u64_at(data, info_ptrs + i * 8) as usize;
        if &data[brti..brti + 4] != b"BRTI" {
            bail!("bad BRTI block");
        }
        let raw_format = u32_at(data, brti + 0x1C);
        let Some(format) = TexFormat::from_raw(raw_format) else {
            bail!("unsupported BNTX format {raw_format:#x}");
        };
        let width = u32_at(data, brti + 0x24);
        let height = u32_at(data, brti + 0x28);
        let block_height_log2 = u32_at(data, brti + 0x34) & 7;
        let tile_mode = u16_at(data, brti + 0x12);
        let name_addr = u64_at(data, brti + 0x60) as usize;
        let ptrs_addr = u64_at(data, brti + 0x70) as usize;
        let mip0 = u64_at(data, ptrs_addr) as usize;

        let name = if name_addr + 2 <= data.len() {
            let len = u16_at(data, name_addr) as usize;
            String::from_utf8_lossy(&data[name_addr + 2..name_addr + 2 + len]).into_owned()
        } else {
            String::new()
        };

        let (bw, bpb) = format.block();
        let wb = width.div_ceil(bw);
        let hb = height.div_ceil(bw);
        let linear = if tile_mode == 1 {
            // pitch-linear
            let len = (wb * hb * bpb) as usize;
            data[mip0..mip0 + len].to_vec()
        } else {
            deswizzle(&data[mip0..], wb, hb, bpb, 1 << block_height_log2)
        };
        out.push(Texture {
            name,
            format,
            width,
            height,
            data: linear,
        });
    }
    Ok(out)
}

/// Tegra X1 block-linear to linear, in units of format blocks.
/// `block_height` is in GOBs (1, 2, 4, 8, 16, 32).
pub fn deswizzle(src: &[u8], width_blocks: u32, height_blocks: u32, bpb: u32, block_height: u32) -> Vec<u8> {
    let row_bytes = (width_blocks * bpb) as usize;
    let mut out = vec![0u8; row_bytes * height_blocks as usize];
    // BRTI stores the block height actually used for mip 0, so no adjustment here.
    let width_gobs = row_bytes.div_ceil(64);
    let bh = block_height as usize;
    for y in 0..height_blocks as usize {
        let gob_row_base = (y / (8 * bh)) * 512 * bh * width_gobs + ((y % (8 * bh)) / 8) * 512;
        let yy = y % 8;
        let y_part = (yy / 2) * 64 + (yy % 2) * 16;
        for xb in 0..width_blocks as usize {
            let x_bytes = xb * bpb as usize;
            // all formats used have bpb in {1,4,8,16}, so a block never straddles 16-byte lines
            let gob = gob_row_base + (x_bytes / 64) * 512 * bh;
            let x = x_bytes % 64;
            let off = gob + (x / 32) * 256 + y_part + ((x % 32) / 16) * 32 + (x % 16);
            let dst = y * row_bytes + x_bytes;
            let n = bpb as usize;
            if off + n <= src.len() {
                out[dst..dst + n].copy_from_slice(&src[off..off + n]);
            }
        }
    }
    out
}
