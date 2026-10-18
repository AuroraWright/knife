//! Minimal BFSAR (NintendoSDK atk sound archive) reader: enough to resolve a wave
//! sound by name to its FWAV and decode it.
//!
//! Chain: STRG name -> item id (type 1 = sound) -> INFO sound info -> file id (FWSD)
//! -> FWSD wave id table -> (wave archive id, wave index) -> wave archive info ->
//! file id (FWAR) -> FWAV -> DSP-ADPCM/PCM16 channels.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use crate::codec::{Pcm, decode_dsp};

fn u8_(d: &[u8], o: usize) -> Result<u8> {
    d.get(o).copied().context("bfsar: out of range")
}
fn u16_(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(d.get(o..o + 2).context("bfsar: out of range")?.try_into()?))
}
fn u32_(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("bfsar: out of range")?.try_into()?))
}
fn i32_(d: &[u8], o: usize) -> Result<i32> {
    Ok(u32_(d, o)? as i32)
}

/// Section offsets of a Nintendo binary file (`FSAR`, `FWSD`, `FWAR`, `FWAV`).
fn sections(d: &[u8], magic: &[u8; 4]) -> Result<HashMap<u16, (usize, usize)>> {
    if d.get(..4) != Some(magic) {
        bail!("expected {}", String::from_utf8_lossy(magic));
    }
    let n = u16_(d, 0x10)? as usize;
    let mut m = HashMap::new();
    for i in 0..n {
        let o = 0x14 + i * 12;
        m.insert(u16_(d, o)?, (u32_(d, o + 4)? as usize, u32_(d, o + 8)? as usize));
    }
    Ok(m)
}

/// Reference table: u32 count, then {u16 type, u16 pad, i32 offset} relative to the table.
fn ref_table(d: &[u8], at: usize) -> Result<Vec<Option<usize>>> {
    let n = u32_(d, at)? as usize;
    (0..n)
        .map(|i| {
            let o = at + 4 + i * 8;
            let off = i32_(d, o + 4)?;
            Ok((off >= 0 && u16_(d, o)? != 0).then(|| at + off as usize))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct StreamSound {
    /// Relative to romfs, e.g. `stream/MOON01_BGM01.opus`.
    pub path: String,
    pub loop_range: Option<(usize, usize)>,
    pub volume: f32,
}

pub struct SoundArchive {
    data: Vec<u8>,
    names: HashMap<String, u32>,
    info: usize,
    file_body: usize,
    sounds: Vec<Option<usize>>,
    warcs: Vec<Option<usize>>,
    files: Vec<Option<usize>>,
}

impl SoundArchive {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let secs = sections(&data, b"FSAR")?;
        let (strg, _) = *secs.get(&0x2000).context("no STRG")?;
        let (info, _) = *secs.get(&0x2001).context("no INFO")?;
        let (file, _) = *secs.get(&0x2002).context("no FILE")?;

        // STRG: string table + patricia tree
        let sbody = strg + 8;
        let str_tab = sbody + u32_(&data, sbody + 4)? as usize;
        let tree = sbody + u32_(&data, sbody + 12)? as usize;
        let n_str = u32_(&data, str_tab)? as usize;
        let mut strings = Vec::with_capacity(n_str);
        for i in 0..n_str {
            let o = str_tab + 4 + i * 12;
            let off = u32_(&data, o + 4)? as usize;
            let size = u32_(&data, o + 8)? as usize;
            let s = data.get(str_tab + off..str_tab + off + size).context("bad string")?;
            let s = s.split(|&b| b == 0).next().unwrap_or_default();
            strings.push(String::from_utf8_lossy(s).to_ascii_lowercase());
        }
        let n_nodes = u32_(&data, tree + 4)? as usize;
        let mut names = HashMap::new();
        for i in 0..n_nodes {
            let o = tree + 8 + i * 20;
            let leaf = u16_(&data, o)? & 1 != 0;
            let sid = u32_(&data, o + 12)?;
            let item = u32_(&data, o + 16)?;
            if leaf && let Some(s) = strings.get(sid as usize) {
                names.insert(s.clone(), item);
            }
        }

        let ibody = info + 8;
        let refs = |ty: u16| -> Result<usize> {
            for k in 0..8 {
                let o = ibody + k * 8;
                if u16_(&data, o)? == ty {
                    return Ok(ibody + i32_(&data, o + 4)? as usize);
                }
            }
            bail!("INFO table {ty:#x} missing")
        };
        let sounds = ref_table(&data, refs(0x2100)?)?;
        let warcs = ref_table(&data, refs(0x2103)?)?;
        let files = ref_table(&data, refs(0x2106)?)?;
        Ok(Self {
            data,
            names,
            info,
            file_body: file + 8,
            sounds,
            warcs,
            files,
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains_key(&name.to_ascii_lowercase())
    }

    /// Bytes of an internal file by file id.
    fn file(&self, id: u32) -> Result<&[u8]> {
        let fi = self.files.get(id as usize).copied().flatten().context("bad file id")?;
        // file info: ref {u16 type, pad, i32 offset} to location
        let ty = u16_(&self.data, fi)?;
        if ty != 0x220C {
            bail!("file {id} is external");
        }
        let loc = fi + i32_(&self.data, fi + 4)? as usize;
        // internal location: ref to data (relative to FILE body), u32 size
        let off = i32_(&self.data, loc + 4)?;
        let size = u32_(&self.data, loc + 8)? as usize;
        if off < 0 {
            bail!("file {id} not stored in archive");
        }
        let start = self.file_body + off as usize;
        self.data.get(start..start + size).context("file out of range")
    }

    /// Decode a wave sound by name (e.g. `WSD_moon_se03001`).
    pub fn load_wave_sound(&self, name: &str) -> Result<Pcm> {
        let item = *self.names.get(&name.to_ascii_lowercase()).with_context(|| format!("sound {name} not found"))?;
        if item >> 24 != 1 {
            bail!("{name} is not a sound");
        }
        let si = self.sounds.get((item & 0xFFFFFF) as usize).copied().flatten().context("bad sound index")?;
        let file_id = u32_(&self.data, si)?;
        let fwsd = self.file(file_id)?;
        let (warc_item, wave_index) = fwsd_first_wave(fwsd)?;
        let wi = self.warcs.get((warc_item & 0xFFFFFF) as usize).copied().flatten().context("bad warc")?;
        let warc_file = u32_(&self.data, wi)?;
        let fwar = self.file(warc_file)?;
        let fwav = fwar_wave(fwar, wave_index as usize)?;
        decode_fwav(fwav)
    }

    #[allow(dead_code)]
    fn info_offset(&self) -> usize {
        self.info
    }

    pub fn sound_count(&self) -> usize {
        self.sounds.len()
    }

    /// External path, loop range (in frames) and volume (0..1) of a stream sound by index.
    pub fn stream_sound(&self, index: usize) -> Result<StreamSound> {
        let si = self.sounds.get(index).copied().flatten().context("bad sound index")?;
        let file_id = u32_(&self.data, si)?;
        let volume = u8_(&self.data, si + 8)? as f32 / 127.0;
        let detail_ty = u16_(&self.data, si + 0x0C)?;
        if detail_ty != 0x2201 {
            bail!("sound {index} is not a stream sound");
        }
        let detail = si + i32_(&self.data, si + 0x10)? as usize;
        let mut loop_range = None;
        if u16_(&self.data, detail + 24)? != 0 {
            let ext = detail + i32_(&self.data, detail + 28)? as usize;
            let start = u32_(&self.data, ext + 4)?;
            let end = u32_(&self.data, ext + 8)?;
            if end > start {
                loop_range = Some((start as usize, end as usize));
            }
        }
        let fi = self.files.get(file_id as usize).copied().flatten().context("bad file id")?;
        if u16_(&self.data, fi)? != 0x220D {
            bail!("stream file is not external");
        }
        let loc = fi + i32_(&self.data, fi + 4)? as usize;
        let end = self.data[loc..].iter().position(|&b| b == 0).context("bad path")?;
        let path = String::from_utf8_lossy(&self.data[loc..loc + end]).into_owned();
        Ok(StreamSound { path, loop_range, volume })
    }
}

fn fwsd_first_wave(d: &[u8]) -> Result<(u32, u32)> {
    let secs = sections(d, b"FWSD")?;
    let (info, _) = *secs.get(&0x6800).context("FWSD INFO missing")?;
    let body = info + 8;
    // ref to wave id table (type 0x0100)
    let tab = body + i32_(d, body + 4)? as usize;
    let n = u32_(d, tab)?;
    if n == 0 {
        bail!("FWSD without waves");
    }
    Ok((u32_(d, tab + 4)?, u32_(d, tab + 8)?))
}

fn fwar_wave(d: &[u8], index: usize) -> Result<&[u8]> {
    let secs = sections(d, b"FWAR")?;
    let (info, _) = *secs.get(&0x6800).context("FWAR INFO missing")?;
    let (file, _) = *secs.get(&0x6801).context("FWAR FILE missing")?;
    let n = u32_(d, info + 8)? as usize;
    if index >= n {
        bail!("wave index out of range");
    }
    let e = info + 12 + index * 12;
    let off = u32_(d, e + 4)? as usize;
    let size = u32_(d, e + 8)? as usize;
    let start = file + 8 + off;
    d.get(start..start + size).context("FWAV out of range")
}

pub fn decode_fwav(d: &[u8]) -> Result<Pcm> {
    let secs = sections(d, b"FWAV")?;
    let (info, _) = *secs.get(&0x7000).context("FWAV INFO missing")?;
    let (data, _) = *secs.get(&0x7001).context("FWAV DATA missing")?;
    let b = info + 8;
    let encoding = u8_(d, b)?;
    let looping = u8_(d, b + 1)? != 0;
    let rate = u32_(d, b + 4)?;
    let loop_start = u32_(d, b + 8)? as usize;
    let samples = u32_(d, b + 12)? as usize;
    let chans = ref_table(d, b + 20)?;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    for ci in chans.into_iter().flatten() {
        let sample_off = i32_(d, ci + 4)? as usize;
        let sdata = d.get(data + 8 + sample_off..).context("bad sample offset")?;
        let ch = match encoding {
            2 => {
                let adpcm = ci + i32_(d, ci + 12)? as usize;
                let mut coefs = [0i16; 16];
                for (k, c) in coefs.iter_mut().enumerate() {
                    *c = u16_(d, adpcm + k * 2)? as i16;
                }
                let h0 = u16_(d, adpcm + 34)? as i16;
                let h1 = u16_(d, adpcm + 36)? as i16;
                decode_dsp(sdata, samples, &coefs, (h0, h1))
            }
            1 => (0..samples)
                .map(|i| u16_(sdata, i * 2).map(|v| v as i16 as f32 / 32768.0).unwrap_or(0.0))
                .collect(),
            0 => (0..samples)
                .map(|i| sdata.get(i).map(|&v| v as i8 as f32 / 128.0).unwrap_or(0.0))
                .collect(),
            e => bail!("unsupported FWAV encoding {e}"),
        };
        channels.push(ch);
    }
    if channels.is_empty() {
        bail!("FWAV without channels");
    }
    let frames = (0..samples)
        .map(|i| {
            let l = channels[0].get(i).copied().unwrap_or(0.0);
            let r = channels.get(1).and_then(|c| c.get(i)).copied().unwrap_or(l);
            [l, r]
        })
        .collect();
    Ok(Pcm {
        sample_rate: rate,
        frames,
        loop_range: looping.then_some((loop_start, samples)),
    })
}
