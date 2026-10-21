//! `allscr.mrg` (scene scripts) and `script_text.mrg` (message lines).

use anyhow::{Context, Result, bail};

use super::{mzp::Mzp, mzx};

/// All message lines for one language.
pub struct ScriptText {
    lines: Vec<String>,
}

impl ScriptText {
    /// `slot` selects the language pair: 0 = Japanese (the only filled slot).
    pub fn parse(data: &[u8], slot: usize) -> Result<Self> {
        let mzp = Mzp::new(data)?;
        let offsets = mzp.get(slot * 2).context("missing offset table")?;
        let text = mzp.get(slot * 2 + 1).context("missing text block")?;
        let offs: Vec<usize> = offsets
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_be_bytes(c) as usize)
            .collect();
        let mut lines = Vec::with_capacity(offs.len());
        for (i, &start) in offs.iter().enumerate() {
            let end = offs.get(i + 1).copied().unwrap_or(text.len()).min(text.len());
            let raw = text.get(start..end).unwrap_or_default();
            let s = String::from_utf8_lossy(raw);
            lines.push(s.trim_end_matches(['\r', '\n']).to_string());
        }
        Ok(Self { lines })
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.lines.get(index).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Scene scripts. Entry 0 is the scene name table (32-byte records), entries 1–2
/// are nested tables, scenes start at entry 3 (MZX, then inverted).
pub struct SceneArchive {
    pub names: Vec<String>,
    scenes: Vec<Vec<u8>>,
    /// Raw nested containers at entries 1 and 2.
    pub aux: Vec<Vec<u8>>,
}

impl SceneArchive {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mzp = Mzp::new(data)?;
        let names_raw = mzp.get(0).context("missing scene name table")?;
        let names = names_raw
            .chunks(32)
            .map(|r| {
                let end = r.iter().position(|&b| b == 0 || b == b'\r').unwrap_or(r.len());
                String::from_utf8_lossy(&r[..end]).into_owned()
            })
            .take_while(|s| !s.is_empty())
            .collect::<Vec<_>>();
        let mut aux = Vec::new();
        for i in 1..3.min(mzp.len()) {
            aux.push(mzp.get(i).unwrap_or_default().to_vec());
        }
        let mut scenes = Vec::new();
        for i in 3..mzp.len() {
            let raw = mzp.get(i).unwrap_or_default();
            let text = if mzx::is_mzx(raw) {
                mzx::decompress_inverted(raw)?
            } else {
                raw.to_vec()
            };
            scenes.push(text);
        }
        if scenes.is_empty() {
            bail!("no scenes in allscr");
        }
        Ok(Self { names, scenes, aux })
    }

    pub fn scene_count(&self) -> usize {
        self.scenes.len()
    }

    /// Scene by archive index (0 = first scene, i.e. mrgd00 entry 3).
    pub fn scene(&self, index: usize) -> Option<&[u8]> {
        self.scenes.get(index).map(Vec::as_slice)
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }
}
