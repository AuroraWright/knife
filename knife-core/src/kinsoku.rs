//! The Japanese build's message line breaking (sub_BF0F8's line loop and sub_C471C), for text slots with a
//! per-line character count (+101005 clear: Japanese and Chinese text). sub_BF0F8 copies the slot's tagged text
//! into a plain copy (+63648: the characters, the ASCII run tags 0x10 / 0x11, 0x0A and 0x0C; ruby text and the
//! other tags left out; an exfont run as its glyph repeated) and walks it line by line over UTF-8 pointers:
//! nn::font's breaker (sub_148988) gives the line's last character from the width (`measure`), the line's
//! character count (full-width characters 1, half-width ones a half, rounded up) is held to the slot's N, and
//! sub_C471C applies the kinsoku lists and gives the next line's start. This is a byte-level port: the exe
//! packs two half-width characters into one 4-byte buffer and steps back over 2-byte characters by bytes.
//! The overseas build's jp_BF0F8 runs the same loop with the same lists; its rest count also skips tags 26 / 27
//! and turns -1 when the rest ends inside an ASCII run, which its jp_BDC2C (tags 16 / 17 only, the last run
//! closed) never gives. Witch on the Holy Night's (`WITCH`) differs: its own lists, the run tags 0x1A / 0x1B
//! too (jp_BDC2C: 0x1B opens a run starting "!!!", 0x1A one starting with two of "!?", 0x10 the others), its
//! own count of half-width characters, a line ending inside a run counting N, a plainer cut back to N - 2, and
//! sub_C471C never ending a line on two packed ASCII characters.

use crate::linebreak::LineBreak;

/// A build's kinsoku: the lists sub_A7790 gives the slot, the allowance and the build's loop.
pub struct Rules {
    /// characters that may not start a line (sub_C3A3C, +101792; Witch on the Holy Night jp_C3A4C, +101800)
    pub no_start: &'static str,
    /// characters that may not end a line (sub_C3A4C, +101800; jp_C3A5C, +101808)
    pub no_end: &'static str,
    /// characters that hang past the line's end (sub_C3A5C, +101808; sub_9D6B8, +101816)
    pub hang: &'static str,
    /// +67788, how far a line's count may run past N for its last characters: sub_BD474(slot, 1) on every text
    /// but a new slot's first (jp_BC694 zeroes it when sub_A7790 makes the slot)
    pub allow: i32,
    pub witch: bool,
}

/// Tsukihime (both builds): unk_31DDB0 / unk_2F764C / unk_29A164
pub const TSUKIHIME: Rules = Rules {
    no_start: ".,-!?:;！？⁉⁈‼⁇．，。、：；」』）】》”’ー～ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ々",
    no_end: "・「『（【“‘",
    hang: ".,-!?:;！？⁉⁈‼⁇．，。、：；」』）】》”’ー～ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ々…―─—Œœ",
    allow: 1,
    witch: false,
};

/// Witch on the Holy Night: unk_250081 / unk_238644 / unk_2292B4
pub const WITCH: Rules = Rules {
    no_start: ".,-!?:;！？．，。、：；」』）】》”’ー～ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ々",
    no_end: "・「『（【“‘",
    hang: ".,-!?:;！？．，。、：；」』）】》”’ー～ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ々…怒格魔余殺场―─—Œœ",
    allow: 1,
    witch: true,
};

/// unk_3C99B6 / unk_3C99B9: the exfont glyphs a line never splits a pair of (`œ` the dash run's)
const PAIR_A: &[u8] = "Œ".as_bytes();
const PAIR_B: &[u8] = "œ".as_bytes();

/// The slot's measure for the breaker: the plain text's cell widths from `start`. sub_C7240's writer starts each
/// line outside an ASCII run (Switch: a line that begins inside a run measures its letters at the full cell
/// until the run's next tag, half cells after a 0x10), and a character runs past the width when its cell ends
/// past it (a line's first character always fits).
pub struct Measure<'a> {
    /// the fixed cell (+100900) and the character spacing (+100912): a run tag halves the cell and scales the
    /// spacing by `run_spacing` (+100932: 0.5 for Japanese, 1.0 for Chinese)
    pub cell: f32,
    pub spacing: f32,
    pub run_spacing: f32,
    /// Witch on the Holy Night's proportional runs (jp_DD0D8: tags 0x10 and 0x1B clear the fixed width, 0x1A
    /// keeps the half cell): a letter's own advance at the run's x scale
    pub prop: Option<&'a dyn Fn(char) -> f32>,
}

/// A line's characters per line (sub_BD7EC / sub_BF0F8 from +768): N = floor((W - g / 2) / (g + spacing)) + 1.
pub fn per_line(width: f32, m: &Measure) -> i32 {
    let (g, s) = (m.cell, m.spacing);
    let mut v = width + g * 0.5;
    if v < g {
        return 0;
    }
    let mut n = 0;
    loop {
        let a = v - g;
        n += 1;
        v = a - s;
        if a < s || v < g {
            return n;
        }
    }
}

type Buf = [u8; 4];

fn char_len(b: u8) -> usize {
    match b {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn packed(c: char) -> Buf {
    let mut b = [0; 4];
    c.encode_utf8(&mut b);
    b
}

fn in_list(list: &str, b: &Buf) -> bool {
    list.chars().any(|c| packed(c) == *b)
}

fn strlen(b: &Buf) -> usize {
    b.iter().position(|&v| v == 0).unwrap_or(4)
}

/// memcmp(a, pair, n) == 0 for the first n bytes (the pair's NUL past its 2 bytes)
fn starts(a: &Buf, pair: &[u8], n: usize) -> bool {
    (0..n).all(|i| a[i] == pair.get(i).copied().unwrap_or(0))
}

/// A printable ASCII character (0x20..=0x7E)
fn ascii(b: u8) -> bool {
    b.wrapping_sub(32) <= 0x5E
}

struct Text<'a> {
    t: &'a [u8],
    r: &'a Rules,
}

impl Text<'_> {
    /// An ASCII run tag: 0x10 / 0x11, Witch on the Holy Night's 0x1A / 0x1B too (mask 0xC030000)
    fn tag(&self, b: u8) -> bool {
        b & 0xFE == 0x10 || self.r.witch && b & 0xFE == 0x1A
    }

    fn at(&self, p: usize) -> u8 {
        self.t.get(p).copied().unwrap_or(0)
    }

    /// nn::util::PickOutCharacterFromUtf8String: the character's bytes over the start of `out`
    fn pick(&self, out: &mut Buf, p: &mut usize) {
        let n = char_len(self.at(*p));
        for (i, o) in out.iter_mut().enumerate().take(n) {
            *o = self.at(*p + i);
        }
        *p += n;
    }

    fn get(&self, p: &mut usize) -> Buf {
        let mut b = [0; 4];
        self.pick(&mut b, p);
        b
    }

    /// sub_BF0F8's line count: the tags left out, "!?" one full-width character, other multi-byte characters
    /// 1 each and single bytes (the control codes included) a half each, rounded up. Witch on the Holy Night
    /// (`count_witch`) counts -1 for text ending inside a run.
    fn count(&self, from: usize, to: usize) -> i32 {
        if self.r.witch {
            return self.count_witch(from, to);
        }
        let (mut full, mut half) = (0, 0);
        let mut p = from;
        while p < to {
            let c = self.get(&mut p);
            if self.tag(c[0]) {
                continue;
            }
            if c[0] == 0 {
                break;
            }
            if c[0] == b'!' && self.at(p) == b'?' {
                p += 1;
                full += 1;
            } else if c[1] != 0 {
                full += 1;
            } else {
                half += 1;
            }
        }
        if half != 0 { full + (half + 1) / 2 } else { full }
    }

    /// jp_BF0F8's count: no "!?" pair; 8 half-width characters count 8, 13 count 5 (4 when the first run opens
    /// after 15 or more full-width ones), others under 9 a half each less two for a run opened by 0x1B, rounded
    /// up; -1 when the last tag opens a run.
    fn count_witch(&self, from: usize, to: usize) -> i32 {
        let (mut full, mut half, mut open, mut opened, mut bang3) = (0, 0, false, 99999, false);
        let mut p = from;
        while p < to {
            let c = self.get(&mut p);
            if self.tag(c[0]) {
                let m = opened.min(full);
                open = c[0] != 0x11;
                if open {
                    opened = m;
                }
                bang3 |= c[0] == 0x1B;
                continue;
            }
            if c[0] == 0 {
                break;
            }
            if c[1] != 0 {
                full += 1;
            } else {
                half += 1;
            }
        }
        if half >= 9 {
            full += if half == 13 { if opened < 15 { 5 } else { 4 } } else { (half + 1) >> 1 };
        } else if half == 8 {
            full += 8;
        } else if half != 0 {
            full += (if bang3 { half - 2 } else { half } + 1) / 2;
        }
        if open { -1 } else { full }
    }

    /// The first character past the width from `start` (`Measure`), stopping at a line feed.
    /// The measuring tag processor (sub_DD0D8; Witch on the Holy Night's jp_DD0D8) keeps one save of the
    /// writer: a run tag saves the cell fraction and spacing fraction and halves both (the spacing by
    /// `run_spacing`), 0x11 restores them, so a raw 0x10 before a run leaves the text after it at half
    /// cells; 0x05 / 0x07 / 0x08 drop the character after them, and 0x0D and 0x03 the text up to their 0x06 /
    /// 0x04.
    fn overflow(&self, start: usize, end: usize, width: f32, m: &Measure) -> Option<usize> {
        let (mut x, mut first) = (0.0f32, true);
        // (cell fraction, spacing fraction, proportional) and its save
        let mut pen = (1.0f32, 1.0f32, false);
        let mut save = pen;
        let mut p = start;
        while p < end {
            let q = p;
            let c = self.get(&mut p);
            match c[0] {
                0x11 => pen = (save.0, save.1, false),
                b if self.tag(b) => {
                    save = pen;
                    pen = (save.0 * 0.5, save.1 * m.run_spacing, b != 0x1A);
                }
                0x0A => return None,
                0x05 | 0x07 | 0x08 => {
                    self.get(&mut p);
                }
                0x0D | 0x03 => {
                    let stop = if c[0] == 0x0D { 0x06 } else { 0x04 };
                    while p < end && self.get(&mut p)[0] != stop {}
                }
                1..=0x1F => {}
                _ => {
                    let w = match (pen.0 < 1.0, pen.2, m.prop) {
                        (true, true, Some(f)) => f(std::str::from_utf8(&c[..strlen(&c)]).ok().and_then(|s| s.chars().next()).unwrap_or(' ')),
                        _ => m.cell * pen.0,
                    };
                    if !first && x + w > width {
                        return Some(q);
                    }
                    first = false;
                    x += w + m.spacing * pen.1;
                }
            }
        }
        None
    }

    /// sub_148988: the line's last character (a byte offset), or `end` when the rest fits.
    fn last_on_line(&self, lb: &LineBreak, start: usize, end: usize, width: f32, m: &Measure) -> usize {
        // with the rest in the width the line still ends at a mandatory break (UAX #14 class BK: a line feed,
        // a tag 12)
        let Some(over) = self.overflow(start, end, width, m) else {
            return self.t[start..end].iter().position(|&b| b == 0x0A || b == 0x0C).map_or(end, |i| start + i);
        };
        let s = std::str::from_utf8(&self.t[start..end]).unwrap_or_default();
        let offs: Vec<usize> = s.char_indices().map(|(i, _)| start + i).collect();
        let chars: Vec<char> = s.chars().collect();
        let o = offs.iter().position(|&b| b == over).unwrap_or(chars.len() - 1);
        offs[lb.last_on_line(&chars, 0, Some(o))]
    }

    /// sub_C471C(slot, last, count, n): the next line's start after the line ending at `a2` with `count`
    /// characters, or `None` (no break there).
    fn next_line(&self, a2: usize, count: i32, n: i32) -> Option<usize> {
        let end = self.t.len();
        // v9: the characters from a2 to the end
        let mut v9 = 0;
        let mut p = a2;
        while p < end {
            self.get(&mut p);
            v9 += 1;
        }
        if v9 < self.r.allow && in_list(self.r.no_start, &self.get(&mut { a2 })) {
            return None;
        }
        if n < 0 {
            return Some(a2 + char_len(self.at(a2)));
        }
        let v11 = n - count;
        if n < count {
            let mut q = a2;
            let c = self.get(&mut q);
            return Some(if in_list(self.r.no_start, &c) { q } else { a2 });
        }
        let mut v10 = a2;
        if self.at(a2) == b' ' {
            return Some(a2 + 1);
        }
        if v11 >= 1 {
            // the line takes up to n - count more characters, up to a run tag, a line feed or a character
            // that may not end a line
            let mut v48 = a2;
            let (mut v14, mut v15, mut v16) = ([0u8; 4], 0, 0);
            let v17 = loop {
                let v17 = v10;
                let c = self.get(&mut v48);
                if self.tag(c[0]) {
                    break v17;
                }
                if c[0] == b'\n' || in_list(self.r.no_end, &c) {
                    return Some(v17);
                }
                v15 += 1;
                if v15 == v11 {
                    v10 = v48;
                    return self.next_line_tail(a2, v10, count, n, v9);
                }
                v10 = v48;
                v16 = v17;
                v14 = c;
            };
            return Some(if in_list(self.r.no_end, &v14) { v16 } else { v17 });
        }
        self.next_line_tail(a2, v10, count, n, v9)
    }

    /// sub_C471C from LABEL_33: the characters at `v10` (two half-width ones packed into one) and the two after
    /// them against the lists.
    fn next_line_tail(&self, v7: usize, v10: usize, v23: i32, a4: i32, v9: i32) -> Option<usize> {
        let v11 = a4 - v23;
        let mut v48 = v10;
        let mut s4 = [0u8; 4];
        let mut s = [0u8; 4];
        let mut s1 = [0u8; 4];
        self.pick(&mut s4, &mut v48);
        if self.tag(s4[0]) {
            self.pick(&mut s4, &mut v48);
        }
        let v20 = s4[1];
        if s4[1] == 0 && s4[0] == b' ' {
            return Some(v48);
        }
        let mut v45 = v48;
        self.pick(&mut s, &mut v48);
        let retag = self.tag(s[0])
            || ((s[1] | v20) == 0 && {
                s4[1] = s[0];
                v45 = v48;
                self.pick(&mut s, &mut v48);
                self.tag(s[0])
            });
        if retag {
            v45 = v48;
            self.pick(&mut s, &mut v48);
        }
        self.pick(&mut s1, &mut v48);
        if self.tag(s1[0]) {
            self.pick(&mut s1, &mut v48);
        }
        if s[0] == b'\n' {
            return None;
        }
        let (v24, v41, v39) = (strlen(&s4), strlen(&s), strlen(&s1));
        let v25 = !starts(&s4, PAIR_A, v24);
        if (!v25 && starts(&s, PAIR_A, v41)) || (starts(&s4, PAIR_B, v24) && starts(&s, PAIR_B, v41)) {
            if v9 >= self.r.allow + v11 {
                if v25 {
                    if starts(&s4, PAIR_B, v24) && v41 == 0 {
                        return Some(v48);
                    }
                } else if v41 == 0 {
                    return Some(v48);
                }
            }
            let pair1 = starts(&s1, PAIR_A, v39) || starts(&s1, PAIR_B, v39);
            if v11 <= 1 {
                if pair1 {
                    return Some(v7);
                }
                if v11 == 0 {
                    return Some(v10);
                }
            }
            if pair1 {
                return Some(v10);
            }
        }
        // v28: the character after the next may hang; v30: s4 may end a line (in Witch on the Holy Night not two
        // packed ASCII characters); v32: s4 may start one; v33: s may start one
        let v28 = !in_list(self.r.hang, &s1);
        let v30 = !in_list(self.r.no_end, &s4) && !(self.r.witch && ascii(s4[0]) && ascii(s4[1]));
        let v32 = !in_list(self.r.no_start, &s4);
        let v33 = !in_list(self.r.no_start, &s);
        if in_list(self.r.hang, &s) && v28 {
            let mut q = v45;
            self.pick(&mut s4, &mut q);
            return Some(q);
        }
        if v30 && v33 {
            return (strlen(&s) > 0).then_some(v45);
        }
        if v33 || !v30 {
            return (!v30 && v33).then_some(v10);
        }
        if v32 {
            return Some(v10);
        }
        if !v28 {
            if v11 < 1 {
                return Some(v10);
            }
            let mut q = v7;
            for _ in 0..v11 - 1 {
                self.get(&mut q);
            }
            return Some(q);
        }
        let mut q = v10;
        self.get(&mut q);
        self.get(&mut q);
        Some(q)
    }

    /// sub_BF0F8's cut for a line counting more than n: back to n - 2 characters from `start` (two half-width
    /// ones counting one, a run's odd one at its 0x11), then the line's last character from there.
    fn cut(&self, start: usize, n: i32) -> usize {
        if self.r.witch {
            return self.cut_witch(start, n);
        }
        let v130 = n - 2;
        let mut v172 = start;
        if v130 >= 1 {
            let (mut v132, mut v133) = (0, false);
            'walk: loop {
                let c = loop {
                    let c = self.get(&mut v172);
                    if !(1..=0x1F).contains(&c[0]) {
                        break c;
                    }
                    let ne17 = c[0] != 0x11;
                    let v137 = ne17 | !v133;
                    v133 &= ne17;
                    v132 = v132 - i32::from(v137) + 1;
                    if v132 >= v130 {
                        break 'walk;
                    }
                };
                let v135 = if c[1] != 0 {
                    v132
                } else {
                    let r = v132 - i32::from(!v133);
                    v133 = !v133;
                    r
                };
                v132 = v135 + 1;
                if v135 + 1 >= v130 {
                    break;
                }
            }
        }
        let mut v171 = v172;
        let mut c = self.get(&mut v171);
        if (1..=0x1F).contains(&c[0]) {
            loop {
                v172 = v171;
                c = self.get(&mut v171);
                if !(1..=0x1F).contains(&c[0]) {
                    break;
                }
            }
        }
        v171 = v172;
        let mut c = self.get(&mut v171);
        if c[1] == 0 {
            self.get(&mut v171);
            let mut v167 = v171;
            c = self.get(&mut v167);
            if (1..=0x1F).contains(&c[0]) {
                loop {
                    v171 = v167;
                    c = self.get(&mut v167);
                    if !(1..=0x1F).contains(&c[0]) {
                        break;
                    }
                }
            }
        }
        if in_list(self.r.no_end, &c) { v172 } else { v171 }
    }

    /// jp_BF0F8's cut: n - 2 characters from `start`, control codes not counted, two half-width ones one (the
    /// eighth one more), then past the character there unless it may not end a line.
    fn cut_witch(&self, start: usize, n: i32) -> usize {
        let mut p = start;
        if n >= 3 {
            let lim = n - 2;
            let (mut v161, mut v162, mut v163) = (0, 0, false);
            'walk: loop {
                let c = loop {
                    let c = self.get(&mut p);
                    if !(1..=0x1F).contains(&c[0]) {
                        break c;
                    }
                    if v161 >= lim {
                        break 'walk;
                    }
                };
                let mut v165 = v161 - i32::from(!v163) + i32::from(v162 == 7);
                if c[1] != 0 {
                    v165 = v161;
                } else {
                    v163 = !v163;
                    v162 += 1;
                }
                v161 = v165 + 1;
                if v161 >= lim {
                    break;
                }
            }
        }
        let mut q = p;
        let c = self.get(&mut q);
        if in_list(self.r.no_end, &c) { p } else { q }
    }
}

/// The least room sub_BF0F8 lets a line go on with after a tag 12: 20 / 1920 of the screen width (English text,
/// whose N is -1, also needs `ROOM_MIN_ENGLISH`), in px at 1920.
pub const ROOM_MIN: f32 = 20.0;
/// 120 / 1920 of the screen width (Witch on the Holy Night's jp_BF0F8: 140)
pub const ROOM_MIN_ENGLISH: f32 = 120.0;
pub const ROOM_MIN_ENGLISH_WITCH: f32 = 140.0;

/// sub_BF0F8's lines.
pub struct Breaks {
    /// the byte offsets where a new line starts
    pub starts: Vec<usize>,
    /// how many of the text's tags 12 sub_BF0F8 turned into line breaks: each one makes the first tag 12 left
    /// in the slot's tagged text a 31, which draws nothing
    pub cut: usize,
}

/// The byte offsets of `text` (sub_BF0F8's plain copy) where sub_BF0F8 starts a new line: the first line
/// `first` px wide, the others `width`. `base` is the slot's x for tag 12 (+101032) less its own (+512): when
/// the breaker ends a line at a tag 12 (0x0C, `@x` or the character), the line goes on from there over the room
/// the base leaves in the slot (the whole width from a base at or past its right edge, the tag's x then the
/// slot's own), 20 px more before a character of the HANG list (Tsukihime), with as many characters as that
/// room takes; with less than `ROOM_MIN` the line breaks at the tag instead.
pub fn breaks(lb: &LineBreak, text: &[u8], first: f32, width: f32, base: f32, m: &Measure, r: &Rules) -> Breaks {
    let t = Text { t: text, r };
    let end = text.len();
    let full = per_line(width, m);
    let mut out = Vec::new();
    let mut cut = 0;
    let (mut start, mut w, mut n) = (0, first, per_line(first, m));
    while start < end {
        let mut brk = t.last_on_line(lb, start, end, w, m);
        if brk >= end {
            break;
        }
        // a line never ends inside a run of a pair glyph: on to the run's end
        loop {
            let mut q = brk;
            let c = t.get(&mut q);
            let pair = if c == packed('Œ') { PAIR_A } else if c == packed('œ') { PAIR_B } else { break };
            if q < 2 * pair.len() {
                break;
            }
            let mut back = q - 2 * pair.len();
            if t.get(&mut back) != c {
                break;
            }
            brk = q;
        }
        match t.at(brk) {
            0x0C => {
                let mut room = width - if base >= width { 0.0 } else { base.max(0.0) };
                let mut q = brk + 1;
                if !r.witch && in_list(r.hang, &t.get(&mut q)) {
                    room += ROOM_MIN;
                }
                if room < ROOM_MIN {
                    out.push(brk);
                    cut += 1;
                    w = width;
                } else {
                    (w, n) = (room, per_line(room, m));
                }
                start = brk + 1;
                continue;
            }
            // the count stays the line's
            0x0A => {
                start = brk + 1;
                w = width;
                continue;
            }
            _ => {}
        }
        let mut next = brk;
        let last = t.get(&mut next);
        let mut count = t.count(start, next);
        // Witch on the Holy Night: a line ending inside a run counts N
        if count < 0 {
            count = n;
        }
        if count <= n {
            // the rest hangs on this line when it is one character that may not start a line, within the
            // allowance
            let rest = t.count(next, end);
            let cap = n.min(full);
            if rest + count <= cap + r.allow && (rest < 1 || in_list(r.no_start, &t.get(&mut { next }))) {
                break;
            }
            n = cap;
        } else if !in_list(r.hang, &last) {
            brk = t.cut(start, n);
            count -= 1;
        }
        let nl = t.next_line(brk, count, n);
        if let Some(p) = nl.filter(|&p| t.at(p) != 0) {
            out.push(p);
        }
        start = nl.unwrap_or_else(|| brk + char_len(t.at(brk)));
        (w, n) = (width, full);
    }
    Breaks { starts: out, cut }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exe::Nso;

    fn lines(s: &str) -> Vec<String> {
        lines_at(s, 0.0).0
    }

    /// `lines` in a slot whose tag 12 base is `base` px from its x, with the count of tags 12 made line breaks
    fn lines_at(s: &str, base: f32) -> (Vec<String>, usize) {
        let lb = LineBreak::load(&Nso::from_bundle(include_bytes!("../../knife/data/exe_tables_overseas.bin")).unwrap()).unwrap();
        // the message slot: 464 units (1392 px) of 45.6 px cells and 4 px spacing
        let m = Measure { cell: 45.6, spacing: 4.0, run_spacing: 0.5, prop: None };
        // sub_BDC2C's ASCII run tags
        let mut t = String::new();
        let mut run = false;
        for c in s.chars() {
            let a = (' '..='~').contains(&c);
            if a != run {
                t.push(if a { '\u{10}' } else { '\u{11}' });
                run = a;
            }
            t.push(c);
        }
        if run {
            t.push('\u{11}');
        }
        let b = breaks(&lb, t.as_bytes(), 1392.0, 1392.0, base, &m, &TSUKIHIME);
        let mut out = Vec::new();
        let mut from = 0;
        for p in b.starts.into_iter().chain([t.len()]) {
            out.push(t[from..p].chars().filter(|&c| c >= ' ').collect::<String>());
            from = p;
        }
        (out, b.cut)
    }

    #[test]
    fn tag_12_and_line_feeds() {
        let kana = "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわをんアイウエオカキクケコサシスセソタチツ";
        let kana: Vec<char> = kana.chars().collect();
        let k = |a: usize, b: usize| kana[a..b].iter().collect::<String>();
        // a line ends at a line feed even when the rest fits, and the lines after it break
        assert_eq!(lines(&format!("あ\n{}", k(0, 60))), vec![format!("あ{}", k(0, 28)), k(28, 56), k(56, 60)]);
        // with 1.2 px of room after the base the line breaks at the tag, which draws nothing
        assert_eq!(lines_at(&format!("{}\u{c}{}", k(0, 3), k(3, 6)), 1390.8), (vec![k(0, 3), k(3, 6)], 1));
        // 20 px more before a character of the HANG list: on from the base, where it hangs alone (N 0)
        assert_eq!(lines_at(&format!("{}\u{c}。{}", k(0, 3), k(3, 6)), 1390.8), (vec![format!("{}。", k(0, 3)), k(3, 6)], 0));
        // 392 px of room: N 8 (sub_C471C fills the line to it past the 7 cells in the width), on from the base
        assert_eq!(lines_at(&format!("\u{c}{}", k(0, 10)), 1000.0), (vec![k(0, 8), k(8, 10)], 0));
        // a base past the slot's right edge: the whole width
        assert_eq!(lines_at(&format!("{}\u{c}{}", k(0, 20), k(20, 50)), 1400.0), (vec![format!("{}{}", k(0, 20), k(20, 48)), k(48, 50)], 0));
    }

    #[test]
    fn latin_text_in_japanese_slots() {
        // Switch (Astris, the Japanese build with these lines in script_text.mrg)
        let x = "x".repeat(54);
        let cases: Vec<(String, Vec<String>)> = vec![
            ("Mes yeux se plissent malgré moi face à ces couleurs\u{3000}effrontées.".into(),
             vec!["Mes yeux se plissent malgré moi face à ces couleurs".into(), "\u{3000}effrontées.".into()]),
            ("Un courant d'air frais s'invite par la fenêtre ouverte, et les rideaux ondulent doucement.".into(),
             vec!["Un courant d'air frais s'invite par la fenêtre ouverte,".into(), " et les rideaux ondulent ".into(), "doucement.".into()]),
            ("Anticonstitutionnellement, incompréhensiblement, extraordinairement et merveilleusement.".into(),
             vec!["Anticonstitutionnellement, incompréhensiblement, ".into(), "extraordinairement et ".into(), "merveilleusement.".into()]),
            ("Il faisait froid ce soir-là ; la lune, pâle et silencieuse, se reflétait sur les flaques d'eau qui parsemaient la rue déserte.".into(),
             vec!["Il faisait froid ce soir-là ; la lune, pâle et ".into(), "silencieuse, se reflétait sur les ".into(), "flaques d'eau qui parsemaient ".into(), "la rue déserte.".into()]),
            ("あの日、僕は窓の外を見ていた。Je ne savais pas encore ce qui m'attendait là-bas, au loin.".into(),
             vec!["あの日、僕は窓の外を見ていた。Je ne savais pas encore ce".into(), " qui m'attendait là-bas, au loin.".into()]),
            ("« Qu'est-ce que tu racontes ? » demanda-t-elle, les sourcils froncés, sans comprendre !".into(),
             vec!["« Qu'est-ce que tu racontes ? » demanda-t-elle, les ".into(), "sourcils froncés, sans comprendre !".into()]),
            // the last line ends at the run's 0x11, so a line feed goes in before it (Switch: the next message a line
            // further down)
            (format!("{x} {}", "b".repeat(80)), vec![format!("{x} "), "b".repeat(43), "b".repeat(37), String::new()]),
            ("b".repeat(80), vec!["b".repeat(56), "b".repeat(24)]),
            // (Switch: the click-wait icon at the start of the line after)
            (format!("{x} {} {}", "b".repeat(30), "b".repeat(5)), vec![format!("{x} "), format!("{} {}", "b".repeat(30), "b".repeat(5)), String::new()]),
            (format!("{x} {} {}", "b".repeat(29), "b".repeat(5)), vec![format!("{x} "), format!("{} ", "b".repeat(29)), "b".repeat(5)]),
            (format!("{x} {}é{}", "b".repeat(20), "b".repeat(30)), vec![format!("{x} "), format!("{}é{}", "b".repeat(20), "b".repeat(25)), "b".repeat(5)]),
            (format!("{x} {}日本{}", "b".repeat(10), "b".repeat(40)), vec![format!("{x} "), format!("{}日本", "b".repeat(10)), "b".repeat(40)]),
            (format!("日本語の文章{}", "a".repeat(60)), vec!["日本語の文章".into(), "a".repeat(56), "a".repeat(4)]),
            (format!("{x} {}", ["bb"; 13].join("-")), vec![format!("{x} "), ["bb"; 13].join("-"), String::new()]),
            (format!("{x} {}, bbbb. bbb!", "b".repeat(27)), vec![format!("{x} "), format!("{}, ", "b".repeat(27)), "bbbb. bbb!".into()]),
            (format!("{x} {}", ["a"; 40].join(" ")), vec![format!("{x} "), "a ".repeat(15), "a ".repeat(15), ["a"; 10].join(" ")]),
            (["aaaa"; 30].join(" "), vec!["aaaa ".repeat(11), "aaaa ".repeat(6), "aaaa ".repeat(6), "aaaa ".repeat(6), "aaaa".into()]),
            (format!("{x} é{}", ["a"; 30].join(" ")), vec![format!("{x} "), format!("é{} ", ["a"; 27].join(" ")), "a a a".into()]),
            // the last word's closing tag forbids a break after it, so the line ends before it
            (format!("{x} {}", ["cc"; 30].join(" ")), vec![format!("{x} "), "cc ".repeat(10), "cc ".repeat(10), "cc ".repeat(9), "cc".into()]),
            (format!("{}é{}", "abc ".repeat(40), ["abc"; 10].join(" ")),
             vec!["abc ".repeat(14), "abc ".repeat(7), "abc ".repeat(7), "abc ".repeat(7), format!("{}éabc abc abc abc ", "abc ".repeat(5)), ["abc"; 6].join(" ")]),
            (format!("{} {}", "b".repeat(40), "b".repeat(40)), vec![format!("{} ", "b".repeat(40)), "b".repeat(40), String::new()]),
        ];
        for (s, want) in cases {
            assert_eq!(lines(&s), want, "{s}");
        }
    }
}
