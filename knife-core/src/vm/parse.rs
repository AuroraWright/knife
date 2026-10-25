//! Parsing of decompressed scene text (`_CMD(arg,arg,...);` sequences).
//!
//! Commands are `_` followed by a 4-character name. `_Z?xxxxx(arg)` macros carry a
//! 5-character id: `ZZ`/`ZY` define labels (when the argument is non-empty) and
//! `ZM` displays a message.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Regular 4-letter command, e.g. `STCH`.
    Cmd([u8; 4]),
    /// `_ZM<id>(text)`: show a message.
    Message,
    /// `_ZZ`/`_ZY` marker (label definition or no-op).
    Marker,
}

#[derive(Debug, Clone)]
pub struct Command {
    pub op: Op,
    /// The raw name after `_` (4 chars for commands, longer for Z macros).
    pub name: String,
    pub args: Vec<String>,
    /// Byte offset of the command in the scene text.
    pub offset: usize,
    /// The offset earlier knife versions gave it, counting each 0xFF byte before it as the 3 bytes of a
    /// U+FFFD (their save records keep that)
    pub legacy_offset: usize,
}

impl Command {
    pub fn is(&self, name: &str) -> bool {
        matches!(&self.op, Op::Cmd(n) if n == name.as_bytes())
    }

    pub fn arg(&self, i: usize) -> Option<&str> {
        self.args.get(i).map(String::as_str).filter(|s| !s.is_empty())
    }

    /// Integer argument; supports simple `a+b-c` arithmetic and `0x` hex.
    pub fn int(&self, i: usize) -> Option<i32> {
        self.arg(i).and_then(eval_int)
    }

    pub fn int_or(&self, i: usize, default: i32) -> i32 {
        self.int(i).unwrap_or(default)
    }

    /// Keyed optional parameter written as `` `NNN:value ``.
    pub fn keyed(&self, key: u32) -> Option<&str> {
        self.args.iter().find_map(|a| {
            let rest = a.strip_prefix('`')?;
            let (k, v) = rest.split_once(':')?;
            (k.parse::<u32>().ok()? == key).then_some(v)
        })
    }

    pub fn keyed_int(&self, key: u32) -> Option<i32> {
        self.keyed(key).and_then(eval_int)
    }
}

pub fn eval_int(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok().map(|v| v as i32);
    }
    // a [+-] b [+-] c ..., optional leading sign; decimal fractions are truncated
    let mut total: i64 = 0;
    let mut sign: i64 = 1;
    let mut cur = String::new();
    let mut any = false;
    let flush = |cur: &mut String, total: &mut i64, sign: i64, any: &mut bool| -> Option<()> {
        if cur.is_empty() {
            return Some(());
        }
        let v: f64 = cur.parse().ok()?;
        *total += sign * v as i64;
        cur.clear();
        *any = true;
        Some(())
    };
    for c in s.chars() {
        match c {
            // a 0xFF byte (some scenes pad arguments with them): the exe's tokenizer (sub_3160) stops at any
            // character it does not know, and args_int evaluates what it read, 0 when nothing
            '\u{FFFD}' => {
                flush(&mut cur, &mut total, sign, &mut any)?;
                return Some(total as i32);
            }
            '+' | '-' if !cur.is_empty() => {
                flush(&mut cur, &mut total, sign, &mut any)?;
                sign = if c == '-' { -1 } else { 1 };
            }
            '-' => sign = -sign,
            '+' => {}
            '0'..='9' | '.' => cur.push(c),
            _ => return None,
        }
    }
    flush(&mut cur, &mut total, sign, &mut any)?;
    any.then_some(total as i32)
}

#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub commands: Vec<Command>,
    pub labels: HashMap<String, usize>,
}

impl Scene {
    pub fn parse(bytes: &[u8]) -> Self {
        // scanned as bytes, so offsets are the exe's (save records keep them): some scenes carry 0xFF bytes in
        // their arguments, which a lossy UTF-8 decoding of the whole text would widen; the syntax is ASCII and
        // never inside a multi-byte character
        let find = |from: usize, b: u8| bytes[from..].iter().position(|&c| c == b).map(|p| p + from);
        let mut commands = Vec::new();
        let mut labels = HashMap::new();
        let mut i = 0;
        // 0xFF bytes before `i`
        let (mut ff, mut ff_at) = (0, 0);
        while i < bytes.len() {
            if bytes[i] != b'_' {
                i += 1;
                continue;
            }
            let start = i;
            ff += bytes[ff_at..start].iter().filter(|&&b| b == 0xff).count();
            ff_at = start;
            let Some(open) = find(i, b'(') else { break };
            let name = String::from_utf8_lossy(&bytes[i + 1..open]);
            let name = name.as_ref();
            if name.is_empty() || name.len() > 16 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                i += 1;
                continue;
            }
            // find the closing paren that is followed by ';'
            let mut j = open + 1;
            let close = loop {
                match find(j, b')') {
                    Some(c) => {
                        if bytes.get(c + 1).is_none_or(|&b| b == b';') {
                            break c;
                        }
                        j = c + 1;
                    }
                    None => break bytes.len(),
                }
            };
            let inner = String::from_utf8_lossy(&bytes[open + 1..close.min(bytes.len())]);
            let inner = inner.as_ref();
            let args: Vec<String> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').map(|s| s.to_string()).collect()
            };
            let op = if name.len() > 4 && name.starts_with('Z') {
                match &name[..2] {
                    "ZM" => Op::Message,
                    _ => Op::Marker,
                }
            } else if name.len() == 4 {
                let b = name.as_bytes();
                Op::Cmd([b[0], b[1], b[2], b[3]])
            } else {
                i = close + 1;
                continue;
            };
            if op == Op::Marker
                && let Some(l) = args.first().filter(|s| !s.is_empty())
            {
                labels.entry(l.clone()).or_insert(commands.len());
            }
            commands.push(Command {
                op,
                name: name.to_string(),
                // message text may contain commas (it is never split)
                args: if matches!(name.get(..2), Some("ZM")) { vec![inner.to_string()] } else { args },
                offset: start,
                legacy_offset: start + 2 * ff,
            });
            i = close + 1;
        }
        Self { commands, labels }
    }

    pub fn label(&self, name: &str) -> Option<usize> {
        self.labels.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(eval_int("258-18"), Some(240));
        assert_eq!(eval_int("-1000"), Some(-1000));
        assert_eq!(eval_int("0xFF8913"), Some(0xFF8913));
        assert_eq!(eval_int("2000.0"), Some(2000));
        assert_eq!(eval_int(""), None);
        // 0xFF padding ends the expression (sub_3160)
        assert_eq!(eval_int("0\u{FFFD}\u{FFFD}"), Some(0));
        assert_eq!(eval_int("\u{FFFD}\u{FFFD}1612"), Some(0));
        assert_eq!(eval_int("12\u{FFFD}5"), Some(12));
    }

    #[test]
    fn raw_offsets() {
        // offsets count the 0xFF bytes once each, as the save records do
        let s = Scene::parse(b"_STZ4(0,1,1,1,1,0\xff\xff\xff\xff);_ZMb3901($041383);");
        assert_eq!(s.commands[1].offset, 23);
        assert_eq!(s.commands[0].int(5), Some(0));
    }

    #[test]
    fn parse_basic() {
        let s = Scene::parse(b"_ZZ01601(LBL_000);_STCH(0,img0597,258-18,154-19,1000,255,2,,CC,,);_ZMb3901($041383);_SEPL(0,1,X,,`017:50);");
        assert_eq!(s.commands.len(), 4);
        assert_eq!(s.label("LBL_000"), Some(0));
        assert!(s.commands[1].is("STCH"));
        assert_eq!(s.commands[1].int(2), Some(240));
        assert_eq!(s.commands[2].op, Op::Message);
        assert_eq!(s.commands[3].keyed_int(17), Some(50));
    }
}
