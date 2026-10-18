//! CG gallery: which CGs are unlocked (cGalleryMain, sub_4470) and where their thumbnails are.

use std::collections::HashSet;

use crate::gallery_table::{CG_ALIASES, CG_GROUPS};

/// Thumbnail atlas `cg_thumb_all` (CG_THUMB_PARTS_JA): 36 x 55 cells of 154 x 88, indexed by entry id.
pub const THUMB_COLS: u32 = 36;
pub const THUMB_W: f32 = 154.0;
pub const THUMB_H: f32 = 88.0;
/// RDED bit that unlocks every CG (set by the final Q&A scene).
const ALL_CG_BIT: usize = 557;

pub struct Entry {
    pub name: &'static str,
    pub thumb: u16,
    pub width: u16,
    pub height: u16,
}

pub fn group_count() -> usize {
    CG_GROUPS.len()
}

pub fn entry(group: usize, i: usize) -> Option<Entry> {
    let &(name, _, thumb, width, height) = CG_GROUPS.get(group)?.1.get(i)?;
    Some(Entry { name, thumb, width, height })
}

/// sub_B58C: some groups additionally need a flowchart node (RDST bit).
fn group_allowed(cond: u32, node: impl Fn(usize) -> bool) -> bool {
    let n = match cond {
        1 => 2,
        2 => 3,
        3 => 4,
        4 => 5,
        5 => 383,
        6 => 208,
        7 => 7,
        8 => 8,
        9 => 9,
        10 => 226,
        11 => 10,
        12 => 11,
        13 => 12,
        14 => 13,
        15 => return node(275) || node(276),
        16 => 14,
        17 => 297,
        18 => 300,
        19 => 309,
        _ => return true,
    };
    node(n)
}

/// sub_51E0 / sub_77610: an image counts as seen when it (or, for gallery-only copies, one of
/// its script aliases) was shown. Sprite aliases match any size letter (A/L/M/S) at position 4.
fn image_seen(seen: &HashSet<String>, name: &str) -> bool {
    if seen.contains(name) {
        return true;
    }
    let Some((_, aliases)) = CG_ALIASES.iter().find(|(n, _)| *n == name) else { return false };
    aliases.iter().any(|a| {
        seen.contains(*a)
            || (a.len() >= 6
                && !a.starts_with("IMG")
                && ['A', 'L', 'M', 'S'].iter().any(|c| {
                    let mut s = a.to_string();
                    s.replace_range(4..5, &c.to_string());
                    seen.contains(&s)
                }))
    })
}

/// Unlocked entry indices of every group (empty = locked).
pub fn unlocked(seen: &HashSet<String>, node: impl Fn(usize) -> bool, ending: impl Fn(usize) -> bool) -> Vec<Vec<usize>> {
    let all = ending(ALL_CG_BIT);
    CG_GROUPS
        .iter()
        .map(|(cond, entries)| {
            if !group_allowed(*cond, &node) {
                return Vec::new();
            }
            (0..entries.len()).filter(|&i| all || image_seen(seen, entries[i].0)).collect()
        })
        .collect()
}

/// Music gallery entries (sub_19EC4): index into MUSIC when unlocked.
pub fn music_unlocked(heard: impl Fn(i32) -> bool, node: impl Fn(usize) -> bool, movie_flags: u16) -> Vec<bool> {
    crate::gallery_table::MUSIC
        .iter()
        .map(|&(bgm, _, _, alts)| {
            heard(bgm)
                || alts.iter().any(|&a| a > 0 && heard(a))
                || match bgm {
                    80 => movie_flags & 1 != 0,
                    124 => movie_flags & 2 != 0,
                    82 => node(560),
                    126 => node(562),
                    _ => false,
                }
        })
        .collect()
}

/// Movie gallery (cGalleryMovie, sub_17DD0 / sub_18EA4): plays movie `index + 4`;
/// the openings need their in-game viewing flag.
pub const MOVIES: &[(usize, Option<u16>)] = &[(4, None), (5, Some(0)), (6, Some(1))];

/// Ending gallery (cGalleryEnding): (flowchart node bit, scene id). Main endings first
/// (dword_3374EC), then the 21 dead ends (unk_337510).
pub const ENDINGS: &[(usize, i32)] = &[
    (544, 1022),
    (555, 1023),
    (556, 1000),
    (533, 1001),
    (534, 1002),
    (535, 1003),
    (536, 1004),
    (537, 1005),
    (538, 1006),
    (539, 1007),
    (540, 1008),
    (541, 1009),
    (542, 1010),
    (543, 1012),
    (545, 1013),
    (546, 1014),
    (547, 1015),
    (548, 1016),
    (549, 1017),
    (550, 1018),
    (551, 1019),
    (552, 1020),
    (553, 1021),
    (554, 1024),
];
