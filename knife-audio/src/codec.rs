//! Decoders: Nintendo Opus (`.opus` in `stream/`) and DSP-ADPCM (sound effects in the BFSAR).

use anyhow::{Context, Result, bail};

/// Decoded interleaved stereo audio.
#[derive(Clone, Debug, Default)]
pub struct Pcm {
    pub sample_rate: u32,
    /// Stereo frames (left, right).
    pub frames: Vec<[f32; 2]>,
    /// Loop region in frames, if the source loops.
    pub loop_range: Option<(usize, usize)>,
}

fn u32le(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("truncated")?.try_into()?))
}
fn u32be(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(d.get(o..o + 4).context("truncated")?.try_into()?))
}

/// Nintendo Opus: header chunk `0x80000001`, data chunk `0x80000004` holding
/// `u32be size, u32be final_range, packet` records.
pub fn decode_nopus(data: &[u8]) -> Result<Pcm> {
    if u32le(data, 0)? != 0x8000_0001 {
        bail!("not a Nintendo Opus file");
    }
    let channels = data[9] as usize;
    let sample_rate = u32le(data, 0x0C)?;
    let data_off = u32le(data, 0x10)? as usize;
    let pre_skip = u32le(data, 0x1C)? as usize;
    if u32le(data, data_off)? != 0x8000_0004 {
        bail!("Nintendo Opus data chunk missing");
    }
    let data_len = u32le(data, data_off + 4)? as usize;
    let mut p = data_off + 8;
    let end = (p + data_len).min(data.len());
    let ch = if channels == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
    let mut dec = opus::Decoder::new(sample_rate, ch)?;
    let mut buf = vec![0f32; 5760 * channels.max(1)];
    let mut frames = Vec::new();
    while p + 8 <= end {
        let size = u32be(data, p)? as usize;
        p += 8;
        let Some(packet) = data.get(p..p + size) else { break };
        p += size;
        let n = dec.decode_float(packet, &mut buf, false)?;
        if channels == 1 {
            frames.extend(buf[..n].iter().map(|&s| [s, s]));
        } else {
            frames.extend(buf[..n * 2].as_chunks::<2>().0.iter().map(|c| [c[0], c[1]]));
        }
    }
    let skip = pre_skip.min(frames.len());
    frames.drain(..skip);
    Ok(Pcm {
        sample_rate,
        frames,
        loop_range: None,
    })
}

/// Decode one channel of GameCube/Wii/Switch DSP-ADPCM.
pub fn decode_dsp(data: &[u8], samples: usize, coefs: &[i16; 16], hist: (i16, i16)) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples);
    let (mut h1, mut h2) = (hist.0 as i32, hist.1 as i32);
    let mut i = 0;
    'frames: for frame in data.chunks(8) {
        let header = frame[0];
        let scale = 1i32 << (header & 0xF);
        let ci = ((header >> 4) & 7) as usize;
        let (c1, c2) = (coefs[ci * 2] as i32, coefs[ci * 2 + 1] as i32);
        for &b in &frame[1..] {
            for nib in [b >> 4, b & 0xF] {
                if i >= samples {
                    break 'frames;
                }
                let n = if nib >= 8 { nib as i32 - 16 } else { nib as i32 };
                let s = ((n * scale) << 11) + 1024 + c1 * h1 + c2 * h2;
                let s = (s >> 11).clamp(-32768, 32767);
                h2 = h1;
                h1 = s;
                out.push(s as f32 / 32768.0);
                i += 1;
            }
        }
    }
    out
}
