//! Readers for the game's data formats.

pub mod archive;
pub mod bffnt;
pub mod bfttf;
pub mod bntx;
pub mod bntx_decode;
pub mod jpeg;
pub mod mzp;
pub mod mzx;
pub mod nx;
pub mod script;
pub mod sfnt;

/// Map a game file into memory: the archives are hundreds of MB, of which a load reads a few KB.
pub fn map(path: &std::path::Path) -> std::io::Result<memmap2::Mmap> {
    let file = std::fs::File::open(path)?;
    // SAFETY: the game files are read-only inputs and not modified while mapped.
    unsafe { memmap2::Mmap::map(&file) }
}
