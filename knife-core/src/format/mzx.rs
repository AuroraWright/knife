//! `MZX0` compression (2-byte-word LZ variant with a 64-entry ring buffer).
//!
//! Script data is additionally bit-inverted after decompression (`scene_load`
//! inverts every byte), which [`decompress_inverted`] does.

use anyhow::{Result, bail};

pub fn is_mzx(data: &[u8]) -> bool {
    data.len() >= 8 && &data[..4] == b"MZX0"
}

pub fn decompressed_len(data: &[u8]) -> Option<usize> {
    is_mzx(data).then(|| u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize)
}

pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let Some(out_len) = decompressed_len(data) else {
        bail!("not an MZX0 stream");
    };
    let mut out: Vec<u8> = Vec::with_capacity(out_len + 2);
    let mut ring = [[0u8; 2]; 64];
    let mut ring_pos = 0usize;
    let mut last = [0u8; 2];
    let mut clear = 0i32;
    let mut p = 8usize;

    while out.len() < out_len && p < data.len() {
        if clear <= 0 {
            clear = 0x1000;
            last = [0, 0];
        }
        let flags = data[p];
        p += 1;
        let arg = (flags >> 2) as usize;
        match flags & 3 {
            0 => {
                for _ in 0..=arg {
                    out.extend_from_slice(&last);
                }
            }
            1 => {
                let Some(&k) = data.get(p) else { break };
                p += 1;
                let dist = (k as usize + 1) * 2;
                if dist > out.len() {
                    bail!("MZX back-reference out of range");
                }
                for _ in 0..(arg + 1) * 2 {
                    let b = out[out.len() - dist];
                    out.push(b);
                }
                let n = out.len();
                last = [out[n - 2], out[n - 1]];
            }
            2 => {
                last = ring[arg];
                out.extend_from_slice(&last);
            }
            _ => {
                for _ in 0..=arg {
                    if p + 2 > data.len() {
                        break;
                    }
                    last = [data[p], data[p + 1]];
                    p += 2;
                    ring[ring_pos] = last;
                    ring_pos = (ring_pos + 1) % 64;
                    out.extend_from_slice(&last);
                }
            }
        }
        clear -= 1;
    }
    out.truncate(out_len);
    Ok(out)
}

/// Decompress and invert every byte, as the engine does for scene scripts.
pub fn decompress_inverted(data: &[u8]) -> Result<Vec<u8>> {
    let mut v = decompress(data)?;
    for b in &mut v {
        *b = !*b;
    }
    Ok(v)
}
