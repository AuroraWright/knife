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
    /// a mono source (both sides already at 1 / sqrt 2): nn::atk pans its one channel
    pub mono: bool,
}

fn u32le(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("truncated")?.try_into()?))
}
fn u32be(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(d.get(o..o + 4).context("truncated")?.try_into()?))
}

/// Gain of a stereo stream's channels at the centre in nn::atk's balance pan mode (every stream sound has pan
/// option 1, wave sounds 0 = dual, which keeps stereo at full level). Measured on the Switch (Astris capture): BGM
/// 26 and 72 at 0.4 and BGM 35 at 0.32 come out at 0.678 / 0.672 / 0.688 of their sub_CA0 volume, without cross-mix.
pub const STEREO_STREAM_GAIN: f32 = 0.68;

/// Nintendo Opus: header chunk `0x80000001`, data chunk `0x80000004` holding
/// `u32be size, u32be final_range, packet` records.
pub fn decode_nopus(data: &[u8]) -> Result<Pcm> {
    decode_nopus_part(data, 0, usize::MAX).map(|(_, pcm)| pcm)
}

/// Frames `from..to` of a Nintendo Opus file (after its pre-skip) and the index of the first one. Decoding starts
/// a second before `from`: after Opus's nominal 80 ms pre-roll the decoder still differs by up to 0.01 from a
/// whole-file decode, and only matches it exactly after about 0.4 s.
pub fn decode_nopus_part(data: &[u8], from: usize, to: usize) -> Result<(usize, Pcm)> {
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
    // decoded sample indices: the kept range, and where decoding starts (the packet holding the pre-roll point)
    let (keep, stop) = (from.saturating_add(pre_skip), to.saturating_add(pre_skip));
    let roll = keep.saturating_sub(sample_rate as usize);
    let mut pos = 0;
    while roll > 0 && p + 8 <= end {
        let size = u32be(data, p)? as usize;
        let Some(packet) = data.get(p + 8..p + 8 + size) else { break };
        let n = opus::packet::get_nb_samples(packet, sample_rate)?;
        if pos + n > roll {
            break;
        }
        pos += n;
        p += 8 + size;
    }
    let ch = if channels == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
    let mut dec = opus::Decoder::new(sample_rate, ch)?;
    let mut buf = vec![0f32; 5760 * channels.max(1)];
    let mut frames = Vec::new();
    while p + 8 <= end && pos < stop {
        let size = u32be(data, p)? as usize;
        p += 8;
        let Some(packet) = data.get(p..p + size) else { break };
        p += size;
        let n = dec.decode_float(packet, &mut buf, false)?;
        let (a, b) = (keep.clamp(pos, pos + n) - pos, stop.clamp(pos, pos + n) - pos);
        pos += n;
        if channels == 1 {
            // nn::atk pans a mono stream to the centre with the sqrt curve: 1 / sqrt 2 on each side (voices
            // capture at 0.70 of their volume on the Switch)
            frames.extend(buf[a..b].iter().map(|&s| [s * std::f32::consts::FRAC_1_SQRT_2; 2]));
        } else {
            frames.extend(buf[a * 2..b * 2].as_chunks::<2>().0.iter().map(|c| [c[0] * STEREO_STREAM_GAIN, c[1] * STEREO_STREAM_GAIN]));
        }
    }
    Ok((
        from,
        Pcm {
            sample_rate,
            frames,
            loop_range: None,
            mono: channels == 1,
        },
    ))
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

#[cfg(test)]
mod tests {
    /// A part decoded from inside a stream (after the pre-roll) is the same frames as the whole decode.
    #[test]
    fn part_matches_whole() {
        let Ok(data) = std::fs::read("../switch_romfs/stream/MOON01_BGM01.opus") else { return };
        let whole = super::decode_nopus(&data).unwrap();
        for from in [0, 1_340_383] {
            let (base, part) = super::decode_nopus_part(&data, from, from + 48_000).unwrap();
            assert_eq!((base, part.frames.len()), (from, 48_000));
            let err = part.frames.iter().zip(&whole.frames[from..]).map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs())).fold(0f32, f32::max);
            assert_eq!(err, 0.0, "from {from}");
        }
    }
}
