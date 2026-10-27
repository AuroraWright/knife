//! CPU decoding of BNTX textures to RGBA8 (used by tools and as a fallback when
//! the GPU lacks BC texture support).

use anyhow::Result;

use super::bntx::{TexFormat, Texture};

pub fn to_rgba8(t: &Texture) -> Result<Vec<u8>> {
    let (w, h) = (t.width as usize, t.height as usize);
    let (bw, bpb) = t.format.block();
    if bw == 1 {
        return Ok(match t.format {
            TexFormat::R8Unorm => t.data.iter().flat_map(|&v| [v, v, v, 255]).collect(),
            _ => t.data.clone(),
        });
    }
    let wb = w.div_ceil(4);
    let hb = h.div_ceil(4);
    if t.format == TexFormat::Bc4Unorm {
        // single channel -> grey RGBA
        let pitch = wb * 4;
        let mut r8 = vec![0u8; pitch * hb * 4];
        crate::par::rows(&mut r8, pitch * 4, |first, band| {
            for (i, row) in band.chunks_mut(pitch * 4).enumerate() {
                for bx in 0..wb {
                    let src = &t.data[((first + i) * wb + bx) * 8..][..8];
                    bcdec_rs::bc4(src, &mut row[bx * 4..], pitch, false);
                }
            }
        });
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            out.extend(r8[y * pitch..y * pitch + w].iter().flat_map(|&v| [v, v, v, 255]));
        }
        return Ok(out);
    }
    let pitch = wb * 4 * 4;
    let mut full = vec![0u8; pitch * hb * 4];
    crate::par::rows(&mut full, pitch * 4, |first, band| {
        for (i, row) in band.chunks_mut(pitch * 4).enumerate() {
            for bx in 0..wb {
                let src = &t.data[((first + i) * wb + bx) * bpb as usize..][..bpb as usize];
                let dst = &mut row[bx * 16..];
                match t.format {
                    TexFormat::Bc1Srgb | TexFormat::Bc1Unorm => bcdec_rs::bc1(src, dst, pitch),
                    TexFormat::Bc3Srgb | TexFormat::Bc3Unorm => bcdec_rs::bc3(src, dst, pitch),
                    _ => bcdec_rs::bc7(src, dst, pitch),
                }
            }
        }
    });
    // crop to the real size
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        out.extend_from_slice(&full[y * pitch..y * pitch + w * 4]);
    }
    Ok(out)
}
