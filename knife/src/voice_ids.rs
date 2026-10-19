//! Voice ids (sub_23C0): the exe's voice tables (off_AE0248 -> off_44D858) give every voice file an id; the ids
//! run contiguously from VOICE_ID_BASE, one name per line of voice_ids.txt (dumped from the exe).

use std::{collections::HashMap, sync::OnceLock};

const VOICE_ID_BASE: i32 = 0x0100_0083;
const NAMES: &str = include_str!("voice_ids.txt");

fn names() -> &'static [&'static str] {
    static V: OnceLock<Vec<&'static str>> = OnceLock::new();
    V.get_or_init(|| NAMES.lines().collect())
}

/// The id sub_23C0 returns for a voice name (case-insensitive), or -1.
pub fn id(name: &str) -> i32 {
    static M: OnceLock<HashMap<&'static str, i32>> = OnceLock::new();
    let m = M.get_or_init(|| names().iter().enumerate().map(|(i, n)| (*n, VOICE_ID_BASE + i as i32)).collect());
    m.get(name.to_ascii_uppercase().as_str()).copied().unwrap_or(-1)
}

/// The voice name of an id.
pub fn name(id: i32) -> Option<&'static str> {
    names().get(usize::try_from(id.checked_sub(VOICE_ID_BASE)?).ok()?).copied()
}

#[cfg(test)]
mod tests {
    #[test]
    fn exe_ids() {
        // values from the exe tables (off_44D858)
        assert_eq!(super::id("A00_01_00_0000_0"), 0x0100_0083);
        assert_eq!(super::id("d10_00_00_0000_0"), 0x0100_3DA2);
        assert_eq!(super::id("B90_00_01_0000_0"), 0x0100_3AED);
        assert_eq!(super::name(0x0100_0142), Some("A00_01_01_0000_0"));
        assert_eq!(super::id("NOPE"), -1);
    }
}
