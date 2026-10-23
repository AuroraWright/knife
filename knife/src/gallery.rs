//! CG gallery: which CGs are unlocked (cGalleryMain, sub_4470) and where their thumbnails are.

use crate::gallery_table::{CG_ALIASES, CG_GROUPS, CG_GROUPS_OVERSEAS, CG_ORIGINS, CgEntry};

/// Thumbnail atlas `cg_thumb_all` (CG_THUMB_PARTS_JA): 36 x 55 cells of 154 x 88, indexed by entry id.
pub const THUMB_COLS: u32 = 36;
pub const THUMB_W: f32 = 154.0;
pub const THUMB_H: f32 = 88.0;
/// RDED bit that unlocks every CG (set by the final Q&A scene).
const ALL_CG_BIT: usize = 557;

#[derive(Clone, Copy)]
pub struct Entry {
    pub name: &'static str,
    pub thumb: u16,
    /// the source rect within the image (sub_BE84: entry +12 .. +24)
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// A group's entries in the loaded build.
fn group(g: usize) -> Option<(u32, &'static [CgEntry])> {
    let &(cond, entries) = CG_GROUPS.get(g)?;
    if crate::assets::overseas()
        && let Some(&(_, e)) = CG_GROUPS_OVERSEAS.iter().find(|(n, _)| *n == g)
    {
        return Some((cond, e));
    }
    Some((cond, entries))
}

pub fn entry(g: usize, i: usize) -> Option<Entry> {
    let &(name, _, thumb, width, height) = group(g)?.1.get(i)?;
    let (x, y) = CG_ORIGINS.iter().find(|o| o.0 == name).map_or((0, 0), |o| (o.1, o.2));
    Some(Entry { name, thumb, x, y, width, height })
}

/// Overseas sub_BE84: a CG that stacks a strip per language shows the game language's.
pub fn language_strip(name: &str) -> bool {
    crate::assets::overseas() && crate::gallery_table::CG_LANGUAGE_STRIPS.contains(&name)
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
fn image_seen(seen: &impl Fn(&str) -> bool, name: &str) -> bool {
    if seen(name) {
        return true;
    }
    let Some((_, aliases)) = CG_ALIASES.iter().find(|(n, _)| *n == name) else { return false };
    aliases.iter().any(|a| {
        seen(a)
            || (a.len() >= 6
                && !a.starts_with("IMG")
                && ['A', 'L', 'M', 'S'].iter().any(|c| {
                    let mut s = a.to_string();
                    s.replace_range(4..5, &c.to_string());
                    seen(&s)
                }))
    })
}

/// Unlocked entry indices of every group (empty = locked).
pub fn unlocked(seen: impl Fn(&str) -> bool, node: impl Fn(usize) -> bool, ending: impl Fn(usize) -> bool) -> Vec<Vec<usize>> {
    let all = ending(ALL_CG_BIT);
    (0..CG_GROUPS.len())
        .filter_map(group)
        .map(|(cond, entries)| {
            if !group_allowed(cond, &node) {
                return Vec::new();
            }
            (0..entries.len()).filter(|&i| all || image_seen(&seen, entries[i].0)).collect()
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

/// Ending gallery (cGalleryEnding, records at 0x3374E8 = {scene id, RDST node bit, confirm}):
/// (node bit, scene id, SYSMES id of the play confirmation). Main endings first, then the 21 dead ends.
pub const ENDINGS: &[(usize, i32, usize)] = &[
    (544, 1011, 1482),
    (555, 1022, 1493),
    (556, 1023, 1494),
    (533, 1000, 1471),
    (534, 1001, 1472),
    (535, 1002, 1473),
    (536, 1003, 1474),
    (537, 1004, 1475),
    (538, 1005, 1476),
    (539, 1006, 1477),
    (540, 1007, 1478),
    (541, 1008, 1479),
    (542, 1009, 1480),
    (543, 1010, 1481),
    (545, 1012, 1483),
    (546, 1013, 1484),
    (547, 1014, 1485),
    (548, 1015, 1486),
    (549, 1016, 1487),
    (550, 1017, 1488),
    (551, 1018, 1489),
    (552, 1019, 1490),
    (553, 1020, 1491),
    (554, 1021, 1492),
    // object 418: QA_25, shown once every ending is completed (sub_926F0(1): RDED 533..556 all set)
    (557, 1024, 1495),
];

/// The overseas build's ending records (jd_3374E8, 20 bytes: scene id, the Japanese node bit, two completed
/// nodes, confirm): an entry is reached once either node (-1 = none) is completed (RDED, G300 bits,
/// sub_922C0), not by its RDST bit; sub_FF40 (QA_25, the congratulation banner) needs all 24.
pub const EN_ENDING_DONE: [(i32, i32); 24] = [
    (14, -1), (300, -1), (309, -1), (187, -1), (31, -1), (316, -1), (317, -1), (92, -1),
    (94, -1), (138, -1), (151, -1), (319, -1), (170, -1), (171, -1), (190, -1), (321, -1),
    (257, 250), (328, -1), (283, -1), (285, -1), (288, -1), (291, -1), (304, -1), (308, -1),
];
