//! `mrgd00` containers (called MZP when stored as standalone files).
//!
//! Layout: `"mrgd00"`, `u16 count`, then `count` entries of four `u16`s
//! (`sector`, `offset`, `size_sectors`, `size_low`). Entry data starts after the
//! entry table; see [`Mzp::entry_range`] for how offsets and sizes are decoded
//! (matches `sub_B51F0` in the executable).

use anyhow::{Result, bail};

pub const MAGIC: &[u8; 6] = b"mrgd00";

#[derive(Debug, Clone, Copy)]
pub struct MzpEntry {
    pub sector: u16,
    pub offset: u16,
    pub size_sectors: u16,
    pub size_low: u16,
}

pub struct Mzp<'a> {
    data: &'a [u8],
    entries: Vec<MzpEntry>,
}

pub fn is_mzp(data: &[u8]) -> bool {
    data.len() >= 8 && &data[..6] == MAGIC
}

impl<'a> Mzp<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        if !is_mzp(data) {
            bail!("not a mrgd00 container");
        }
        let count = u16::from_le_bytes([data[6], data[7]]) as usize;
        if data.len() < 8 + count * 8 {
            bail!("truncated mrgd00 entry table");
        }
        let entries = (0..count)
            .map(|i| {
                let e = &data[8 + i * 8..16 + i * 8];
                let w = |o: usize| u16::from_le_bytes([e[o], e[o + 1]]);
                MzpEntry {
                    sector: w(0),
                    offset: w(2),
                    size_sectors: w(4),
                    size_low: w(6),
                }
            })
            .collect();
        Ok(Self { data, entries })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[MzpEntry] {
        &self.entries
    }

    fn entry_range(&self, index: usize) -> Option<(usize, usize)> {
        let e = self.entries.get(index)?;
        if e.sector == 0xFFFF {
            return None;
        }
        let base = 8 + 8 * self.entries.len();
        let w = e.offset as usize;
        let byte_offset = (w & 0xFFF) | (((w & 0xF000) >> 12) << 27);
        let start = base + ((e.sector as usize) << 11) + byte_offset;
        let size = if e.size_sectors == 0 {
            e.size_low as usize
        } else {
            (e.size_sectors as usize - 1) / 32 * 0x10000 + e.size_low as usize
        };
        let end = (start + size).min(self.data.len());
        (start <= end).then_some((start, end))
    }

    pub fn get(&self, index: usize) -> Option<&'a [u8]> {
        let (s, e) = self.entry_range(index)?;
        Some(&self.data[s..e])
    }
}
