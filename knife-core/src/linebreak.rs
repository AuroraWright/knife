//! nn::font's line breaking (sub_148988; overseas sub_148988's twin at the same tables): the UAX #14 pair table
//! algorithm over the executable's class ranges and 30 x 30 class pair table. The text engine takes it for text
//! slots with flag +101005 (English text, or slot flag 0x10000, sub_A7790): sub_BF0F8 asks for the last character
//! of each line that runs past the slot width and sub_C471C breaks after it.

use anyhow::{Context, Result};

use crate::exe::{LINE_BREAK_CLASSES, LINE_BREAK_RANGES, Nso};

/// Pair table classes (UAX #14 sample implementation order).
const ID: u8 = 14;
const AL: u8 = 12;
const NS: u8 = 5;
const WJ: u8 = 22;
/// Classes outside the pair table.
const BK: u8 = 31;
const CB: u8 = 32;
const SP: u8 = 39;
/// The pair table's verdicts: a direct break, a break only across spaces, and (2 / 3) the combining-mark rules.
const DIRECT: i32 = 0;

pub struct LineBreak {
    /// (first code point, last code point, class)
    ranges: Vec<(u32, u32, u8)>,
    pairs: Vec<i32>,
}

impl LineBreak {
    pub fn load(nso: &Nso) -> Result<Self> {
        let l = nso.layout()?;
        let raw = nso.bytes(l.line_break_classes, 4 * LINE_BREAK_RANGES).context("line break classes")?;
        let ranges = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|e| {
                let start = u16::from_le_bytes([e[0], e[1]]) as u32;
                (start, start + e[2] as u32, e[3])
            })
            .collect();
        let raw = nso.bytes(l.line_break_pairs, 4 * LINE_BREAK_CLASSES * LINE_BREAK_CLASSES).context("line break pairs")?;
        let pairs = raw.as_chunks::<4>().0.iter().map(|&e| i32::from_le_bytes(e)).collect();
        Ok(Self { ranges, pairs })
    }

    /// A character's class: the last range starting at or below it (the exe's binary search over entries
    /// 0..1986), ID past its end; then AI / SA / SG (30 / 37 / 38) as AL, CJ (33) as NS and CR / LF / NL (34..36)
    /// as BK.
    fn class(&self, c: char) -> u8 {
        let c = c as u32;
        let (mut lo, mut hi) = (0usize, LINE_BREAK_RANGES);
        let entry = loop {
            let d = hi - lo;
            let mid = lo + d / 2;
            if d < 2 {
                break mid;
            }
            if c >= self.ranges[mid].0 {
                lo = mid;
            } else {
                hi = mid;
            }
            if hi <= lo {
                return ID;
            }
        };
        let (_, last, class) = self.ranges[entry];
        let class = if c <= last { class } else { ID };
        match class {
            34..=36 => BK,
            33 => NS,
            30 | 37 | 38 => AL,
            c => c,
        }
    }

    fn pair(&self, before: u8, after: u8) -> i32 {
        self.pairs.get(before as usize * LINE_BREAK_CLASSES + after as usize).copied().unwrap_or(4)
    }

    /// The index of the last character of the line starting at `start` (sub_148988): the last break opportunity
    /// up to `overflow` (the first character past the width, sub_C7240 / sub_145334; `None` when the rest fits),
    /// or the overflowing character itself when there is none. A line keeps the spaces before its break.
    pub fn last_on_line(&self, s: &[char], start: usize, overflow: Option<usize>) -> usize {
        let end = s.len();
        if start >= end {
            return start;
        }
        let limit = overflow.unwrap_or(end - 1).max(start);
        // `v33 <= v14`: the character lies past the overflowing one
        let beyond = |i: usize| overflow.is_some() && i > limit;
        let mut prev_class = match self.class(s[start]) {
            CB => ID,
            BK => return start,
            SP => WJ,
            c => c,
        };
        let mut prev = start;
        let mut candidate: Option<usize> = None;
        let mut spaces = false;
        let mut verdict = DIRECT;
        for (i, &c) in s.iter().enumerate().skip(start + 1) {
            let mut class = self.class(c);
            if class == CB {
                class = ID;
            } else {
                match class {
                    SP => {
                        // spaces keep the class before them and allow an indirect break after them
                        prev = i;
                        spaces = true;
                        verdict = 1;
                        continue;
                    }
                    BK => {
                        return if beyond(i) { prev } else { i };
                    }
                    // EB / EM without the emoji flag (a4 byte 6) are AL
                    29 => class = AL,
                    _ => {}
                }
            }
            verdict = self.pair(prev_class, class);
            if verdict == DIRECT || ((1..=2).contains(&verdict) && spaces) {
                if beyond(i) {
                    return if verdict == DIRECT && spaces { limit } else { prev };
                }
                candidate = Some(prev);
            } else if beyond(i) {
                return candidate.unwrap_or(limit);
            }
            verdict = DIRECT;
            prev_class = class;
            prev = i;
            spaces = false;
        }
        if verdict == DIRECT { candidate.unwrap_or(limit) } else { limit }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker() -> LineBreak {
        LineBreak::load(&Nso::from_bundle(include_bytes!("../../knife/data/exe_tables_overseas.bin")).unwrap()).unwrap()
    }

    fn line(lb: &LineBreak, s: &str, overflow: usize) -> String {
        let chars: Vec<char> = s.chars().collect();
        chars[..=lb.last_on_line(&chars, 0, Some(overflow))].iter().collect()
    }

    #[test]
    fn breaks_after_spaces_and_hyphens() {
        let lb = breaker();
        // "hello world": 'r' (8) overflows, so the line keeps "hello "
        assert_eq!(line(&lb, "hello world", 8), "hello ");
        // the overflowing word starts right after the space
        assert_eq!(line(&lb, "hello world", 6), "hello ");
        // a hyphen allows a break after it
        assert_eq!(line(&lb, "well-known fact", 7), "well-");
        // no opportunity: the line runs to the overflowing character
        assert_eq!(line(&lb, "abcdefgh", 4), "abcde");
        // punctuation stays with its word
        assert_eq!(line(&lb, "Yes, it is.", 9), "Yes, it ");
        // ideographs break anywhere; only characters after the overflowing one count as past the width, so the
        // overflowing character stays when a break follows it (as does a word's last letter: "ab " below)
        assert_eq!(line(&lb, "日本語の文章", 2), "日本語");
        assert_eq!(line(&lb, "ab cd", 1), "ab ");
        // a small kana (CJ, taken as NS) cannot start a line
        assert_eq!(line(&lb, "日本ょ語", 2), "日本ょ");
    }
}
