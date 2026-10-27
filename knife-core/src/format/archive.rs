//! `.hed` / `.nam` / `.mrg` archive triples (e.g. `allpac`, `allui`).
//!
//! `.hed`: 8-byte entries `u32 offset_sectors, u16 stored_sectors, u16 unpacked_sectors`,
//! terminated by an all-`0xFF` entry. `.nam`: 32-byte records, NUL-padded names ending
//! in `\r\n`. `.mrg`: raw data, 0x800-byte sectors. Archives without a `.nam`
//! (e.g. `parts`) are indexed by number only.

use std::{collections::HashMap, fs::File, path::Path};

use anyhow::{Context, Result};
use memmap2::Mmap;

pub const SECTOR: u64 = 0x800;

#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub name: String,
    pub offset: u64,
    pub stored_sectors: u16,
    pub unpacked_sectors: u16,
}

pub struct Archive {
    pub name: String,
    mrg: Mmap,
    entries: Vec<ArchiveEntry>,
    by_name: HashMap<String, usize>,
    by_stem: HashMap<String, usize>,
}

impl Archive {
    /// Open `<dir>/<base>.hed`, `.nam` (optional) and `.mrg`.
    pub fn open(dir: &Path, base: &str) -> Result<Self> {
        Self::open_with(base, |file| dir.join(file))
    }

    /// Open archive `base` with each of its files (`<base>.hed`, `.nam`, `.mrg`) at `path(file name)`.
    pub fn open_with(base: &str, path: impl Fn(&str) -> std::path::PathBuf) -> Result<Self> {
        let hed = std::fs::read(path(&format!("{base}.hed")))
            .with_context(|| format!("reading {base}.hed"))?;
        let nam = std::fs::read(path(&format!("{base}.nam"))).ok();
        let file = File::open(path(&format!("{base}.mrg")))
            .with_context(|| format!("opening {base}.mrg"))?;
        // SAFETY: the game files are read-only inputs and not modified while mapped.
        let mrg = unsafe { Mmap::map(&file)? };

        let mut entries = Vec::new();
        for (i, e) in hed.as_chunks::<8>().0.iter().enumerate() {
            let offset = u32::from_le_bytes(e[0..4].try_into().unwrap());
            if offset == 0xFFFF_FFFF {
                break;
            }
            let name = nam
                .as_ref()
                .and_then(|n| n.get(i * 32..i * 32 + 32))
                .map(|r| {
                    let end = r.iter().position(|&b| b == 0 || b == b'\r').unwrap_or(r.len());
                    String::from_utf8_lossy(&r[..end]).into_owned()
                })
                .unwrap_or_default();
            entries.push(ArchiveEntry {
                name,
                offset: offset as u64 * SECTOR,
                stored_sectors: u16::from_le_bytes([e[4], e[5]]),
                unpacked_sectors: u16::from_le_bytes([e[6], e[7]]),
            });
        }
        let mut by_name = HashMap::new();
        let mut by_stem = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            let upper = e.name.to_ascii_uppercase();
            if upper == "NOFILE.EXISTS" {
                continue;
            }
            let stem = upper.rsplit_once('.').map_or(upper.as_str(), |(s, _)| s).to_string();
            by_stem.entry(stem).or_insert(i);
            by_name.entry(upper).or_insert(i);
        }
        Ok(Self {
            name: base.to_string(),
            mrg,
            entries,
            by_name,
            by_stem,
        })
    }

    pub fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_uppercase()).copied()
    }

    /// First entry whose name without extension matches (case-insensitive).
    pub fn find_stem(&self, stem: &str) -> Option<usize> {
        self.by_stem.get(&stem.to_ascii_uppercase()).copied()
    }

    /// Raw stored bytes of an entry (sector padded unless the payload carries its own size).
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        let e = self.entries.get(index)?;
        if e.name == "NOFILE.EXISTS" {
            return None;
        }
        let start = e.offset as usize;
        let mut len = e.stored_sectors as usize * SECTOR as usize;
        let head = self.mrg.get(start..)?;
        if let Some(stored) = super::nx::stored_len(head) {
            len = stored;
        }
        let end = (start + len).min(self.mrg.len());
        self.mrg.get(start..end)
    }

    /// Entry contents with NX compression removed.
    pub fn read(&self, index: usize) -> Result<Vec<u8>> {
        let raw = self.raw(index).context("missing archive entry")?;
        if super::nx::kind(raw).is_some() {
            super::nx::decompress(raw)
        } else {
            Ok(raw.to_vec())
        }
    }

    pub fn read_by_name(&self, name: &str) -> Result<Vec<u8>> {
        let i = self
            .find(name)
            .with_context(|| format!("{name} not in {}", self.name))?;
        self.read(i)
    }
}
