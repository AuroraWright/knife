//! Flowchart marks of a scene JUMP (0x7f5f0 -> sub_95790): the node of the scene being left (SCENE_NODE,
//! sub_92D00) to the node of the target scene id (sub_92D30). sub_93D10 handles special pairs, often through
//! hidden nodes (497..530) picked by route bits or the last choice; otherwise sub_954B0 marks the pair's
//! edge flags (flow_table::EDGES) in the global words 400+ and the playthrough words F600+, and F110 becomes
//! the target node.
//!
//! sub_93D10 is ported statement by statement: offsets are the exe's byte offsets into the progress words
//! (off_AE04E0: RDST bits at word 110, RDED bits at 300, edge flags at 400) and the F words (off_AE0290).

use super::{Game, set_bit};
use crate::{flow::EDGE_BITS, flow_table::EDGES, save_tables::SCENE_NODE};

/// sub_92D30: the flowchart node of a scene id (scene ids 1..9999; -1 = none).
fn scene_id_node(id: i32) -> i32 {
    if !(1..=9999).contains(&id) {
        return -1;
    }
    crate::flow_table::SCENE_ID_NODE.iter().find(|&&(s, _)| s == id).map_or(-1, |&(_, n)| n)
}

impl Game {
    /// sub_95790 at a JUMP from scene number `scene` to scene id `target`.
    pub(super) fn jump_marks(&mut self, scene: usize, target: i32) {
        // sub_92D00: scene numbers 1..527
        let from = scene.checked_sub(1).and_then(|i| SCENE_NODE.get(i)).map_or(-1, |&n| i32::from(n));
        let to = scene_id_node(target);
        if !self.jump_special(from, to) && to >= 1 && (1..=530).contains(&from) {
            self.mark_edge(from, to);
        }
        tracing::debug!("JUMP marks: node {from} -> {to} (scene id {target}), F110 = {}", self.var("F110"));
    }

    /// Progress word `off / 2` |= `mask`, `off` a byte offset into off_AE04E0.
    fn prog_or(&mut self, off: usize, mask: u16) {
        let w = off / 2;
        for b in (0..16).filter(|b| mask >> b & 1 != 0) {
            let (bits, i) = match w {
                110..300 => (&mut self.nodes, (w - 110) * 16 + b),
                300..400 => (&mut self.endings, (w - 300) * 16 + b),
                _ => (&mut self.edges, (w - 400) * 16 + b),
            };
            if w >= 400 && i >= EDGE_BITS {
                continue;
            }
            set_bit(bits, i);
        }
    }

    /// F word at byte offset `off` into off_AE0290.
    fn fw(&self, off: usize) -> i32 {
        self.var(&format!("F{:03}", off / 2))
    }

    /// The byte at byte offset `off` into off_AE0290 (little endian words).
    fn fb(&self, off: usize) -> i32 {
        self.fw(off & !1) >> (off % 2 * 8) & 0xFF
    }

    fn f_or(&mut self, off: usize, mask: i32) {
        let v = self.fw(off) | mask;
        self.set_var(&format!("F{:03}", off / 2), v);
    }

    /// `if (F24) F110 = n` (init_proc = byte 0x30, loc_DC = byte 0xDC).
    fn f110_if_24(&mut self, n: i32) {
        if self.fw(0x30) != 0 {
            self.set_var("F110", n);
        }
    }

    /// sub_954B0: the first EDGES record for (from, to) sets its flags (each only after the one before it is
    /// set) in the global and the playthrough edge bits; F110 becomes `to` either way.
    fn mark_edge(&mut self, from: i32, to: i32) {
        if let Some(e) = EDGES.iter().find(|e| e[0] == from && e[1] == to) {
            for (k, &f) in e[2..].iter().enumerate() {
                if k > 0 && f < 1 {
                    break;
                }
                if let Ok(i) = usize::try_from(f - 564)
                    && i < EDGE_BITS
                {
                    set_bit(&mut self.edges, i);
                    self.set_play_bit('E', i);
                }
            }
        }
        self.set_var("F110", to);
    }

    /// sub_93D10: true when the pair needs no plain sub_954B0 edge.
    fn jump_special(&mut self, a1: i32, a2: i32) -> bool {
        // the last choice's option / button index (off_AE0AA0, written by sub_83E50 / sub_85690)
        let pick = self.last_pick;
        let (v4, a2) = match a1 {
            24 => return a2 == 371,
            51 => {
                if a2 != 69 {
                    return false;
                }
                self.prog_or(0x11C, 0x80);
                self.f_or(0x212, 0x80);
                self.f110_if_24(519);
                self.prog_or(0x298, 0x80);
                self.prog_or(0x350, 0x800);
                self.mark_edge(51, 519);
                (519, 69)
            }
            52 => {
                if a2 != 312 {
                    return false;
                }
                (66, a2)
            }
            55 => {
                if a2 != 58 || self.fb(0x1A0) & 2 == 0 {
                    return false;
                }
                (57, 58)
            }
            57 => return a2 == 55,
            66 => {
                if a2 == 53 {
                    self.prog_or(0x11E, 4);
                    self.f_or(0x216, 4);
                    self.f110_if_24(530);
                    self.prog_or(0x29A, 4);
                    self.mark_edge(66, 530);
                    (530, 53)
                } else if a2 != 52 {
                    return false;
                } else {
                    return self.fw(0x64) != 0;
                }
            }
            67 => {
                if a2 != 66 {
                    return false;
                }
                if self.fb(0x1A2) & 0x10 != 0 {
                    (68, 66)
                } else if self.fb(0x1DE) & 0x80 == 0 {
                    return false;
                } else {
                    (311, 66)
                }
            }
            68 | 311 => return a2 == 67,
            69 => {
                if a2 != 311 {
                    return false;
                }
                (519, a2)
            }
            87 | 168 | 252 | 253 | 271 | 272 | 325 | 326 | 515 => return false,
            98 => {
                if a2 != 203 {
                    return false;
                }
                (202, a2)
            }
            101 => {
                if a2 != 102 {
                    return false;
                }
                let n = match pick {
                    1 => {
                        self.prog_or(0x11A, 0x40);
                        self.f_or(0x20E, 0x40);
                        self.f110_if_24(502);
                        self.prog_or(0x296, 0x40);
                        502
                    }
                    0 => {
                        self.prog_or(0x11A, 0x20);
                        self.f_or(0x20E, 0x20);
                        self.f110_if_24(501);
                        self.prog_or(0x296, 0x20);
                        501
                    }
                    _ => {
                        self.prog_or(0x11A, 0x80);
                        self.f_or(0x20E, 0x80);
                        self.f110_if_24(503);
                        self.prog_or(0x296, 0x80);
                        503
                    }
                };
                self.mark_edge(101, n);
                (n, 102)
            }
            104 => {
                if a2 != 107 || self.fb(0x1AC) & 2 == 0 {
                    return false;
                }
                (105, 107)
            }
            105 => return a2 == 104 || a2 == 204,
            108 => return a2 == 322,
            126 => {
                if a2 != 129 || self.fb(0x1B0) & 0x80 == 0 {
                    return false;
                }
                (127, 129)
            }
            127 => return a2 == 126,
            136 => {
                if a2 == 137 {
                    self.prog_or(0x334, 0x400);
                }
                return false;
            }
            149 => {
                if a2 != 341 || self.fb(0x1B8) & 1 == 0 {
                    return false;
                }
                (152, 341)
            }
            152 => return a2 == 149,
            166 => {
                if a2 != 342 {
                    return false;
                }
                let n = if pick != 0 {
                    self.prog_or(0x11A, 0x200);
                    self.f_or(0x210, 2);
                    self.f110_if_24(505);
                    self.prog_or(0x296, 0x200);
                    505
                } else {
                    self.prog_or(0x11A, 0x100);
                    self.f_or(0x210, 1);
                    self.f110_if_24(504);
                    self.prog_or(0x296, 0x100);
                    504
                };
                self.mark_edge(166, n);
                (n, 342)
            }
            169 => {
                if a2 != 323 || self.fb(0x1BC) & 1 == 0 {
                    return false;
                }
                (168, 323)
            }
            172 => {
                if a2 != 11 {
                    return false;
                }
                self.prog_or(0x11A, 0x400);
                self.f_or(0x210, 4);
                self.f110_if_24(506);
                self.prog_or(0x296, 0x400);
                self.mark_edge(172, 506);
                (506, 11)
            }
            181 => {
                if a2 != 184 {
                    return false;
                }
                self.prog_or(0x11C, 0x100);
                self.f_or(0x214, 1);
                self.f110_if_24(520);
                self.prog_or(0x298, 0x100);
                self.mark_edge(181, 520);
                if self.fw(0x6C) == 0 {
                    return true;
                }
                self.prog_or(0x350, 0x4000);
                (520, 184)
            }
            184 => {
                if a2 != 314 {
                    return false;
                }
                (520, a2)
            }
            202 => return a2 == 98,
            204 => {
                if a2 != 207 || self.fb(0x1C4) & 0x20 == 0 {
                    return false;
                }
                (205, 207)
            }
            205 => return a2 == 105,
            213 => {
                if a2 == 215 {
                    self.prog_or(0x342, 2);
                }
                return false;
            }
            221 => {
                if a2 != 222 {
                    return false;
                }
                let (mask, f, n) = match pick {
                    1 => (0x4000, 0x40, 510),
                    0 => (0x2000, 0x20, 509),
                    _ => (0x8000, 0x80, 511),
                };
                self.prog_or(0x11A, mask);
                self.f_or(0x210, f);
                self.f110_if_24(n);
                self.prog_or(0x296, mask);
                self.mark_edge(221, n);
                (n, 222)
            }
            234 => {
                if a2 != 236 && a2 != 235 {
                    return false;
                }
                self.prog_or(0x11C, 1);
                self.f_or(0x212, 1);
                self.f110_if_24(512);
                self.prog_or(0x298, 1);
                self.mark_edge(234, 512);
                (512, a2)
            }
            250 => return a2 == 257,
            251 => {
                if !matches!(a2, 252 | 253 | 254 | 325 | 326) {
                    return false;
                }
                self.prog_or(0x11C, 2);
                self.f_or(0x212, 2);
                self.f110_if_24(513);
                self.prog_or(0x298, 2);
                self.mark_edge(251, 513);
                (513, a2)
            }
            256 => {
                if a2 != 259 || self.fb(0x1D2) & 4 == 0 {
                    return false;
                }
                (258, 259)
            }
            258 => return a2 == 256,
            261 => {
                if a2 == 262 {
                    self.prog_or(0x36A, 0x100);
                }
                return false;
            }
            269 => {
                if a2 == 483 {
                    self.prog_or(0x11C, 4);
                    self.f_or(0x212, 4);
                    self.f110_if_24(514);
                    self.prog_or(0x298, 4);
                } else if a2 == 270 {
                    self.prog_or(0x34A, 0x20);
                    self.f_or(0x212, 4);
                    self.prog_or(0x11C, 4);
                    self.f110_if_24(514);
                    self.prog_or(0x298, 4);
                } else {
                    return false;
                }
                self.mark_edge(269, 514);
                (514, a2)
            }
            270 | 483 => return a2 == 523,
            274 => {
                if a2 != 276 && a2 != 275 {
                    return false;
                }
                self.prog_or(0x11C, 0x10);
                self.f_or(0x212, 0x10);
                self.f110_if_24(516);
                self.prog_or(0x298, 0x10);
                self.mark_edge(274, 516);
                if a2 == 276 {
                    (516, 276)
                } else {
                    self.mark_edge(516, 275);
                    self.prog_or(0x36A, 0x80);
                    return true;
                }
            }
            275 | 276 => return a2 == 521,
            277 | 278 => {
                if a2 != 280 && a2 != 279 {
                    return false;
                }
                self.prog_or(0x11C, 0x40);
                self.f_or(0x212, 0x40);
                self.f110_if_24(518);
                self.prog_or(0x298, 0x40);
                self.mark_edge(a1, 518);
                if a2 == 280 {
                    (518, 280)
                } else {
                    self.mark_edge(518, 279);
                    self.prog_or(0x36A, 0x20);
                    return true;
                }
            }
            279 | 280 => return a2 == 1196,
            312 => {
                if a2 == 493 {
                    self.prog_or(0x36E, 4);
                }
                return false;
            }
            322 => {
                if a2 != 112 || self.fb(0x1AC) & 0x10 == 0 {
                    return false;
                }
                (108, 112)
            }
            324 => return a2 == 329 && self.fb(0x1D6) & 1 != 0,
            329 => {
                if a2 != 274 {
                    return false;
                }
                if self.fb(0x1D4) & 0x80 != 0 {
                    (271, 274)
                } else if self.fb(0x1D6) & 1 == 0 {
                    return false;
                } else {
                    (272, 274)
                }
            }
            331 => {
                if a2 == 330 {
                    self.prog_or(0x11C, 0x400);
                    self.prog_or(0x298, 0x400);
                    self.prog_or(0x36A, 4);
                }
                return false;
            }
            355 => {
                if a2 == 20 {
                    self.prog_or(0x33C, 0x800);
                }
                return false;
            }
            369 => {
                if a2 != 29 {
                    return false;
                }
                let (mask, n) = match pick {
                    1 => (8, 499),
                    0 => (4, 498),
                    _ => (0x10, 500),
                };
                self.prog_or(0x11A, mask);
                self.f_or(0x20E, i32::from(mask));
                self.f110_if_24(n);
                self.prog_or(0x296, mask);
                self.mark_edge(369, n);
                (n, 29)
            }
            371 => return a2 == 24,
            410 => {
                if a2 != 220 {
                    return false;
                }
                let (mask, f, n) = if pick != 0 { (0x1000, 0x10, 508) } else { (0x800, 8, 507) };
                self.prog_or(0x11A, mask);
                self.f_or(0x210, f);
                self.f110_if_24(n);
                self.prog_or(0x296, mask);
                self.mark_edge(410, n);
                (n, 220)
            }
            439 => {
                if a2 == 327 {
                    self.prog_or(0x354, 1);
                }
                return false;
            }
            446 | 447 => return a2 == 517,
            463 => {
                if a2 != 300 {
                    return false;
                }
                // nodes 525 and 526
                self.prog_or(0x11C, 0x6000);
                self.f_or(0x214, 0x60);
                self.f110_if_24(526);
                self.prog_or(0x298, 0x6000);
                self.mark_edge(463, 525);
                self.mark_edge(525, 526);
                (526, 300)
            }
            478 => {
                if a2 == 331 {
                    self.prog_or(0x36A, 2);
                    return false;
                }
                if a2 != 330 {
                    return false;
                }
                self.prog_or(0x11C, 0x400);
                self.f_or(0x214, 4);
                self.f110_if_24(522);
                self.prog_or(0x298, 0x400);
                self.mark_edge(478, 522);
                (522, 330)
            }
            480 => {
                if a2 != 309 {
                    return false;
                }
                // nodes 524 and 527
                self.prog_or(0x11C, 0x9000);
                self.f_or(0x214, 0x90);
                self.f110_if_24(527);
                self.prog_or(0x298, 0x9000);
                self.mark_edge(480, 524);
                self.mark_edge(524, 527);
                (527, 309)
            }
            486 => {
                if a2 != 1 {
                    return false;
                }
                self.prog_or(0x11E, 2);
                self.f_or(0x216, 2);
                self.f110_if_24(529);
                self.prog_or(0x29A, 2);
                self.mark_edge(486, 529);
                self.mark_edge(529, 1);
                return true;
            }
            517 => {
                if a2 != 278 && a2 != 277 {
                    return false;
                }
                self.prog_or(0x11C, 0x20);
                self.f_or(0x212, 0x20);
                self.f110_if_24(517);
                self.prog_or(0x298, 0x20);
                let from = if self.fw(0x200) & 0x40 != 0 { 446 } else { 447 };
                self.mark_edge(from, 517);
                (517, a2)
            }
            518 => return a2 == 279 || a2 == 280,
            521 => {
                if a2 != 447 && a2 != 446 {
                    return false;
                }
                self.prog_or(0x11C, 0x200);
                self.f_or(0x214, 2);
                self.f110_if_24(521);
                self.prog_or(0x298, 0x200);
                if self.fb(0x1D6) & 8 != 0 {
                    self.mark_edge(275, 521);
                    if a2 == 446 {
                        self.prog_or(0x36A, 0x40);
                    }
                } else {
                    self.mark_edge(276, 521);
                }
                (521, a2)
            }
            523 => {
                if a2 == 515 {
                    // nodes 523 and 515
                    self.f_or(0x214, 8);
                    self.prog_or(0x11C, 0x808);
                    self.f110_if_24(523);
                    self.prog_or(0x298, 0x808);
                    self.f_or(0x212, 8);
                    self.f110_if_24(515);
                    if self.fb(0x1D4) & 0x40 != 0 {
                        self.mark_edge(270, 523);
                        self.prog_or(0x36A, 0x400);
                    } else {
                        self.mark_edge(483, 523);
                    }
                    (523, 515)
                } else if a2 == 271 {
                    self.prog_or(0x11C, 0x800);
                    self.f_or(0x214, 8);
                    self.f110_if_24(523);
                    self.prog_or(0x298, 0x800);
                    let from = if self.fw(0x1D4) & 0x40 == 0 { 483 } else { 270 };
                    self.mark_edge(from, 523);
                    (523, 271)
                } else {
                    return false;
                }
            }
            905 => return a2 == 523,
            1196 => {
                if a2 != 281 {
                    return false;
                }
                (if self.fb(0x1D6) & 0x80 != 0 { 279 } else { 280 }, 281)
            }
            // LABEL_13: into the prologue's second half (node 486) through 485 and 528
            _ => {
                if a2 != 486 {
                    return false;
                }
                self.f_or(0x216, 1);
                self.prog_or(0x29A, 1);
                self.prog_or(0x11E, 1);
                self.prog_or(0x118, 0x20);
                self.f_or(0x20A, 0x20);
                self.f110_if_24(485);
                self.prog_or(0x294, 0x20);
                self.mark_edge(485, 528);
                (528, 486)
            }
        };
        // LABEL_300
        self.mark_edge(v4, a2);
        true
    }
}
