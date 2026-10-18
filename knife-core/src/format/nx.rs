//! `NXGX` (gzip) and `NXCX` (zlib) compressed wrappers.
//!
//! Header: magic (4), `u32` uncompressed size, `u32` compressed size, 4 bytes padding.

use std::io::Read;

use anyhow::{Result, bail};

pub const HEADER_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NxKind {
    Gzip,
    Zlib,
}

pub fn kind(data: &[u8]) -> Option<NxKind> {
    match data.get(..4)? {
        b"NXGX" => Some(NxKind::Gzip),
        b"NXCX" => Some(NxKind::Zlib),
        _ => None,
    }
}

/// Total stored size (header + compressed payload), if `data` starts with an NX header.
pub fn stored_len(data: &[u8]) -> Option<usize> {
    kind(data)?;
    let csz = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?) as usize;
    Some(HEADER_LEN + csz)
}

pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let Some(k) = kind(data) else {
        bail!("not an NX stream");
    };
    let usz = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let csz = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let body = &data[HEADER_LEN..(HEADER_LEN + csz).min(data.len())];
    let mut out = Vec::with_capacity(usz);
    match k {
        NxKind::Gzip => flate2::read::GzDecoder::new(body).read_to_end(&mut out)?,
        NxKind::Zlib => flate2::read::ZlibDecoder::new(body).read_to_end(&mut out)?,
    };
    Ok(out)
}
