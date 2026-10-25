//! Voice ids (sub_23C0): the exe's voice tables (off_AE0248 -> off_44D858) give every voice file an id; the ids
//! run contiguously from VOICE_ID_BASE, one name per line of voice_ids.txt (dumped from the exe).
//! Witch on the Holy Night's tables (off_6322D8 = 0x2E7028, keyed by off_2E7808) are in voice_ids_witch.txt:
//! an `@key` line per table in key order, then `name id` (hex) in table order.

use std::{collections::HashMap, sync::OnceLock};

const VOICE_ID_BASE: i32 = 0x0100_0083;
const NAMES: &str = include_str!("voice_ids.txt");

fn names() -> &'static [&'static str] {
    static V: OnceLock<Vec<&'static str>> = OnceLock::new();
    V.get_or_init(|| NAMES.lines().collect())
}

/// The id sub_23C0 returns for a voice name (case-insensitive), or -1.
pub fn id(name: &str) -> i32 {
    if crate::assets::witch() {
        return witch_id(name);
    }
    static M: OnceLock<HashMap<&'static str, i32>> = OnceLock::new();
    let m = M.get_or_init(|| names().iter().enumerate().map(|(i, n)| (*n, VOICE_ID_BASE + i as i32)).collect());
    m.get(name.to_ascii_uppercase().as_str()).copied().unwrap_or(-1)
}

/// The voice name of an id.
pub fn name(id: i32) -> Option<&'static str> {
    if crate::assets::witch() {
        return witch().names.get(&id).copied();
    }
    names().get(usize::try_from(id.checked_sub(VOICE_ID_BASE)?).ok()?).copied()
}

struct WitchTables {
    /// (key, entries (name, id) in table order)
    tables: Vec<(&'static str, Vec<(&'static str, i32)>)>,
    /// id -> the first name with it in table order (a trailing comma dropped), the one with a stream file
    names: HashMap<i32, &'static str>,
}

fn witch() -> &'static WitchTables {
    static T: OnceLock<WitchTables> = OnceLock::new();
    T.get_or_init(|| {
        let mut tables: Vec<(&str, Vec<(&str, i32)>)> = Vec::new();
        let mut names = HashMap::new();
        for line in include_str!("voice_ids_witch.txt").lines() {
            if let Some(key) = line.strip_prefix('@') {
                tables.push((key, Vec::new()));
            } else if let Some((n, id)) = line.split_once(' ')
                && let (Some(t), Ok(id)) = (tables.last_mut(), u32::from_str_radix(id, 16))
            {
                t.1.push((n, id as i32));
                names.entry(id as i32).or_insert(n.trim_end_matches(','));
            }
        }
        WitchTables { tables, names }
    })
}

/// Witch on the Holy Night's sub_23C0 (0x2240): the text after the 3-letter speaker prefix, upper-cased, picks
/// the table (sub_20F0: the key is what precedes `_nnnn`, `_nnnnX`, `_nnnn_X` or `_nnnn_XX`, and the four
/// characters after it must read as a number); the id is that of the table's first name the voice name (as
/// written) is a prefix of (memcmp over the voice name's length), 0 when none is.
fn witch_id(name: &str) -> i32 {
    let s = name.as_bytes();
    if s.is_empty() {
        return -1;
    }
    let dest: Vec<u8> = s.get(4..).unwrap_or_default().to_ascii_uppercase();
    let len = dest.len();
    let at = |i: usize| dest.get(i).copied().unwrap_or(0);
    let key_len = if len >= 3 && at(len - 3) == b'_' {
        len.checked_sub(8)
    } else if len >= 2 && at(len - 2) == b'_' {
        len.checked_sub(7)
    } else if len >= 1 && at(len - 1) > b'9' {
        len.checked_sub(6)
    } else {
        len.checked_sub(5)
    };
    let Some(key_len) = key_len else { return -1 };
    let Some((_, table)) = witch().tables.iter().find(|(k, _)| k.as_bytes() == &dest[..key_len]) else {
        return -1;
    };
    let digits = (1..=4).fold(0i32, |n, k| 10 * n + i32::from(at(key_len + k)));
    if digits - 53328 < 0 {
        return -1;
    }
    table.iter().find(|(n, _)| n.as_bytes().starts_with(s)).map_or(0, |e| e.1)
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

    #[test]
    fn witch_ids() {
        // values from Witch on the Holy Night's tables (0x2E7028); a name that prefixes an earlier entry takes
        // its id, and the comma-terminated names still match
        let id = super::witch_id;
        assert_eq!(id("A10_1DOT5_2_0000"), 0x0100_0041);
        assert_eq!(id("F70_1_0_0000"), 0x0100_24B6);
        assert_eq!(id("F70_1_0_0000_A"), 0x0100_24B6);
        assert_eq!(id("B30_B_4_0000"), 0x0100_1C52);
        assert_eq!(id("A10_1_9_0000"), -1);
        assert_eq!(id("A10_1_1_9999"), 0);
        assert_eq!(super::witch().names.get(&0x0100_23CA), Some(&"C40_5A_11_0000_C"));
    }
}
