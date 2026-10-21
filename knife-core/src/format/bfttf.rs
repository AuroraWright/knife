//! BFTTF: the Switch shared system fonts (nn::pl::GetSharedFont; e.g. `nintendo_udsg-r_std_003.bfttf` from
//! system title 0100000000000811). A TTF XORed in little-endian u32 words with 0x06186249: word 0 is
//! 0x18029A7F ^ key, word 1 the big-endian TTF size ^ key, the TTF follows.

const KEY: u32 = 0x0618_6249;
const MAGIC: u32 = 0x1802_9A7F;

/// The TTF inside a BFTTF, or `None` if `data` is not one.
pub fn decode(data: &[u8]) -> Option<Vec<u8>> {
    let word = |i: usize| Some(u32::from_le_bytes(data.get(i..i + 4)?.try_into().ok()?) ^ KEY);
    if word(0)? != MAGIC {
        return None;
    }
    let size = word(4)?.swap_bytes() as usize;
    let mut out: Vec<u8> = data.get(8..)?.chunks(4).flat_map(|c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        (u32::from_le_bytes(w) ^ KEY).to_le_bytes()
    }).collect();
    out.truncate(size);
    Some(out)
}
