//! Baseline JPEG as the Switch's `nn::image::JpegDecoder::Decode` decodes it: the SDK's IJG libjpeg 9
//! (`jpeg_CreateDecompress` version 90) with JCS_RGB output, JDCT_ISLOW and no fancy upsampling or block
//! smoothing. Every image of the three games is baseline, 8-bit, three components without subsampling (Tsukihime
//! allpac / w_allpac / w_allpaccg, Witch on the Holy Night jallpac / jallpachd); `decode` does those and greyscale
//! bit for bit and leaves any other JPEG to the caller.

use anyhow::{Context, Result, bail, ensure};

/// The zigzag position's coefficient in natural order (`jpeg_natural_order`).
const NATURAL: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47,
    55, 62, 63,
];

pub fn is_jpeg(data: &[u8]) -> bool {
    data.starts_with(&[0xFF, 0xD8])
}

/// A Huffman table (`jpeg_make_d_derived_tbl`): the codes of each length and a 9-bit lookup.
struct Huffman {
    /// (length, symbol) by the next 9 bits, length 0 for a longer code
    fast: Vec<(u8, u8)>,
    /// by length: the largest code (-1 none) and the index of the first code's symbol less the first code
    maxcode: [i32; 18],
    valoff: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Result<Huffman> {
        ensure!(counts.iter().map(|&c| usize::from(c)).sum::<usize>() == values.len(), "bad Huffman table");
        let mut fast = vec![(0u8, 0u8); 512];
        let mut maxcode = [-1i32; 18];
        let mut valoff = [0i32; 17];
        let (mut code, mut k) = (0i32, 0usize);
        for len in 1..=16 {
            let n = usize::from(counts[len - 1]);
            valoff[len] = k as i32 - code;
            for _ in 0..n {
                if len <= 9 {
                    let shift = 9 - len;
                    for low in 0..1usize << shift {
                        fast[((code as usize) << shift) | low] = (len as u8, values[k]);
                    }
                }
                code += 1;
                k += 1;
            }
            if n > 0 {
                maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        // the sentinel past the longest code
        maxcode[17] = i32::MAX;
        Ok(Huffman { fast, maxcode, valoff, values })
    }
}

/// The entropy-coded bits (jdhuff's `fill_bit_buffer`): FF 00 is FF, and past a marker or the end of the data
/// libjpeg reads zeros.
struct Bits<'a> {
    d: &'a [u8],
    p: usize,
    acc: u64,
    n: u32,
    marker: bool,
}

impl Bits<'_> {
    fn fill(&mut self) {
        while self.n <= 56 {
            let b = if self.marker || self.p >= self.d.len() {
                0
            } else if self.d[self.p] == 0xFF {
                if self.d.get(self.p + 1) == Some(&0) {
                    self.p += 2;
                    0xFF
                } else {
                    self.marker = true;
                    0
                }
            } else {
                self.p += 1;
                self.d[self.p - 1]
            };
            self.acc |= u64::from(b) << (56 - self.n);
            self.n += 8;
        }
    }

    fn bits(&mut self, k: u32) -> i32 {
        if k == 0 {
            return 0;
        }
        self.fill();
        let v = (self.acc >> (64 - k)) as i32;
        self.acc <<= k;
        self.n -= k;
        v
    }

    fn decode(&mut self, h: &Huffman) -> Result<u8> {
        self.fill();
        let (len, sym) = h.fast[(self.acc >> 55) as usize];
        if len > 0 {
            self.acc <<= len;
            self.n -= u32::from(len);
            return Ok(sym);
        }
        let mut len = 10;
        let mut code = (self.acc >> 54) as i32;
        while code > h.maxcode[len] {
            len += 1;
            code = (self.acc >> (64 - len)) as i32;
        }
        ensure!(len <= 16, "corrupt Huffman code");
        self.acc <<= len;
        self.n -= len as u32;
        h.values.get((h.valoff[len] + code) as usize).copied().context("corrupt Huffman code")
    }

    /// `HUFF_EXTEND` of the next `s` bits.
    fn extend(&mut self, s: u8) -> i32 {
        let v = self.bits(u32::from(s));
        if s > 0 && v < 1 << (s - 1) { v - (1 << s) + 1 } else { v }
    }

    /// Past the next RSTn marker (`read_restart_marker`), the bit buffer emptied.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        self.marker = false;
        while self.p + 1 < self.d.len() && !(self.d[self.p] == 0xFF && (0xD0..=0xD7).contains(&self.d[self.p + 1])) {
            self.p += 1;
        }
        self.p = (self.p + 2).min(self.d.len());
    }
}

struct Component {
    id: u8,
    quant: usize,
    /// the IDCT's output samples, whole blocks
    plane: Vec<u8>,
    dc: i32,
    dc_table: usize,
    ac_table: usize,
}

/// libjpeg 9's `jpeg_idct_islow` (CONST_BITS 13, PASS1_BITS 2) of one dequantized block into `out` (stride
/// `stride`), its range limit the table of RANGE_CENTER 512 (`prepare_range_limit_table`, IDCT_range_limit).
fn idct_islow(coef: &[i16; 64], quant: &[u16; 64], out: &mut [u8], stride: usize) {
    const CONST_BITS: i32 = 13;
    const PASS1_BITS: i32 = 2;
    const F0_298: i64 = 2446;
    const F0_390: i64 = 3196;
    const F0_541: i64 = 4433;
    const F0_765: i64 = 6270;
    const F0_899: i64 = 7373;
    const F1_175: i64 = 9633;
    const F1_501: i64 = 12299;
    const F1_847: i64 = 15137;
    const F1_961: i64 = 16069;
    const F2_053: i64 = 16819;
    const F2_562: i64 = 20995;
    const F3_072: i64 = 25172;
    let deq = |i: usize| i64::from(coef[i]) * i64::from(quant[i]);
    // the odd part of either pass from its inputs 7, 5, 3, 1
    let odd = |t0: i64, t1: i64, t2: i64, t3: i64| {
        let (z2, z3) = (t0 + t2, t1 + t3);
        let z1 = (z2 + z3) * F1_175;
        let z2 = z2 * -F1_961 + z1;
        let z3 = z3 * -F0_390 + z1;
        let z1 = (t0 + t3) * -F0_899;
        let (t0, t3) = (t0 * F0_298 + z1 + z2, t3 * F1_501 + z1 + z3);
        let z1 = (t1 + t2) * -F2_562;
        let (t1, t2) = (t1 * F2_053 + z1 + z3, t2 * F3_072 + z1 + z2);
        [t0, t1, t2, t3]
    };
    let mut ws = [0i64; 64];
    for c in 0..8 {
        let mut z2 = deq(c) << CONST_BITS;
        let z3 = deq(32 + c) << CONST_BITS;
        z2 += 1 << (CONST_BITS - PASS1_BITS - 1);
        let (t0, t1) = (z2 + z3, z2 - z3);
        let (z2, z3) = (deq(16 + c), deq(48 + c));
        let z1 = (z2 + z3) * F0_541;
        let (t2, t3) = (z1 + z2 * F0_765, z1 - z3 * F1_847);
        let (t10, t13, t11, t12) = (t0 + t2, t0 - t2, t1 + t3, t1 - t3);
        let [o0, o1, o2, o3] = odd(deq(56 + c), deq(40 + c), deq(24 + c), deq(8 + c));
        let s = CONST_BITS - PASS1_BITS;
        ws[c] = (t10 + o3) >> s;
        ws[56 + c] = (t10 - o3) >> s;
        ws[8 + c] = (t11 + o2) >> s;
        ws[48 + c] = (t11 - o2) >> s;
        ws[16 + c] = (t12 + o1) >> s;
        ws[40 + c] = (t12 - o1) >> s;
        ws[24 + c] = (t13 + o0) >> s;
        ws[32 + c] = (t13 - o0) >> s;
    }
    let limit = |v: i64| -> u8 { ((((v >> (CONST_BITS + PASS1_BITS + 3)) & 1023) - 384).clamp(0, 255)) as u8 };
    for r in 0..8 {
        let w = &ws[r * 8..r * 8 + 8];
        let z2 = w[0] + ((512 << (PASS1_BITS + 3)) + (1 << (PASS1_BITS + 2)));
        let z3 = w[4];
        let (t0, t1) = ((z2 + z3) << CONST_BITS, (z2 - z3) << CONST_BITS);
        let z1 = (w[2] + w[6]) * F0_541;
        let (t2, t3) = (z1 + w[2] * F0_765, z1 - w[6] * F1_847);
        let (t10, t13, t11, t12) = (t0 + t2, t0 - t2, t1 + t3, t1 - t3);
        let [o0, o1, o2, o3] = odd(w[7], w[5], w[3], w[1]);
        let o = &mut out[r * stride..r * stride + 8];
        o[0] = limit(t10 + o3);
        o[7] = limit(t10 - o3);
        o[1] = limit(t11 + o2);
        o[6] = limit(t11 - o2);
        o[2] = limit(t12 + o1);
        o[5] = limit(t12 - o1);
        o[3] = limit(t13 + o0);
        o[4] = limit(t13 - o0);
    }
}

/// libjpeg 9's YCbCr -> RGB (`build_ycc_rgb_table`, SCALEBITS 16; the SDK's tables 91881, 116130, -46802,
/// -22553: v9's FIX(0.344136286) where libjpeg 6 / 8 take FIX(0.34414) = 22554).
fn ycc_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (cb, cr) = (i64::from(cb) - 128, i64::from(cr) - 128);
    let y = i64::from(y);
    let r = y + ((91881 * cr + 32768) >> 16);
    let g = y + ((-22553 * cb + 32768 + -46802 * cr) >> 16);
    let b = y + ((116130 * cb + 32768) >> 16);
    [r, g, b].map(|v| v.clamp(0, 255) as u8)
}

/// Decode a baseline JPEG to (width, height, RGB). Errors for what `nn::image` would decode otherwise
/// (progressive, arithmetic, subsampled, 12-bit) as well as for corrupt data.
pub fn decode(data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    ensure!(is_jpeg(data), "not a JPEG");
    let mut quant = [[0u16; 64]; 4];
    let mut dc: [Option<Huffman>; 4] = Default::default();
    let mut ac: [Option<Huffman>; 4] = Default::default();
    let mut comps: Vec<Component> = Vec::new();
    let (mut w, mut h) = (0usize, 0usize);
    let (mut jfif, mut adobe) = (false, None);
    let mut restart = 0usize;
    let mut p = 2;
    loop {
        while data.get(p) == Some(&0xFF) && data.get(p + 1) == Some(&0xFF) {
            p += 1;
        }
        ensure!(data.get(p) == Some(&0xFF), "marker expected");
        let m = *data.get(p + 1).context("truncated JPEG")?;
        p += 2;
        if m == 0xD9 {
            break;
        }
        if (0xD0..=0xD7).contains(&m) || m == 0x01 {
            continue;
        }
        let len = usize::from(u16::from_be_bytes(data.get(p..p + 2).context("truncated JPEG")?.try_into()?));
        let seg = data.get(p + 2..p + len).context("truncated JPEG")?;
        p += len;
        match m {
            0xE0 if seg.starts_with(b"JFIF\0") => jfif = true,
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => adobe = Some(seg[11]),
            0xDB => {
                let mut s = seg;
                while !s.is_empty() {
                    let (pq, tq) = (s[0] >> 4, usize::from(s[0] & 15));
                    ensure!(tq < 4, "bad DQT");
                    let n = if pq == 0 { 64 } else { 128 };
                    let t = s.get(1..1 + n).context("bad DQT")?;
                    for (k, &z) in NATURAL.iter().enumerate() {
                        quant[tq][z] = if pq == 0 { u16::from(t[k]) } else { u16::from_be_bytes([t[2 * k], t[2 * k + 1]]) };
                    }
                    s = &s[1 + n..];
                }
            }
            0xC0 | 0xC1 => {
                ensure!(seg.len() >= 6 && seg[0] == 8, "not an 8-bit JPEG");
                h = usize::from(u16::from_be_bytes([seg[1], seg[2]]));
                w = usize::from(u16::from_be_bytes([seg[3], seg[4]]));
                let n = usize::from(seg[5]);
                ensure!(w > 0 && h > 0 && (n == 1 || n == 3) && seg.len() >= 6 + 3 * n, "unsupported JPEG frame");
                let (bw, bh) = (w.div_ceil(8), h.div_ceil(8));
                for c in seg[6..6 + 3 * n].chunks(3) {
                    ensure!(c[1] == 0x11 && c[2] < 4, "subsampled JPEG");
                    comps.push(Component { id: c[0], quant: usize::from(c[2]), plane: vec![0; bw * bh * 64], dc: 0, dc_table: 0, ac_table: 0 });
                }
            }
            0xC2..=0xCF if m != 0xC4 && m != 0xC8 && m != 0xCC => bail!("not a baseline JPEG (SOF{})", m - 0xC0),
            0xC4 => {
                let mut s = seg;
                while s.len() >= 17 {
                    let (class, id) = (s[0] >> 4, usize::from(s[0] & 15));
                    ensure!(id < 4 && class < 2, "bad DHT");
                    let counts: [u8; 16] = s[1..17].try_into()?;
                    let n: usize = counts.iter().map(|&c| usize::from(c)).sum();
                    let values = s.get(17..17 + n).context("bad DHT")?.to_vec();
                    let t = Huffman::new(&counts, values)?;
                    if class == 0 { dc[id] = Some(t) } else { ac[id] = Some(t) }
                    s = &s[17 + n..];
                }
            }
            0xDD => restart = usize::from(u16::from_be_bytes(seg.get(..2).context("bad DRI")?.try_into()?)),
            0xDA => {
                ensure!(!comps.is_empty(), "scan before frame");
                let n = usize::from(seg[0]);
                let mut scan = Vec::with_capacity(n);
                for c in seg.get(1..1 + 2 * n).context("bad SOS")?.chunks(2) {
                    let i = comps.iter().position(|k| k.id == c[0]).context("scan of no component")?;
                    comps[i].dc_table = usize::from(c[1] >> 4);
                    comps[i].ac_table = usize::from(c[1] & 15);
                    comps[i].dc = 0;
                    scan.push(i);
                }
                let (bw, bh) = (w.div_ceil(8), h.div_ceil(8));
                let mut bits = Bits { d: data, p, acc: 0, n: 0, marker: false };
                let mut coef = [0i16; 64];
                let blocks = bw * bh;
                for b in 0..blocks {
                    if restart > 0 && b > 0 && b % restart == 0 {
                        bits.restart();
                        for &i in &scan {
                            comps[i].dc = 0;
                        }
                    }
                    for &i in &scan {
                        let comp = &mut comps[i];
                        let (dct, act) = (dc[comp.dc_table].as_ref().context("missing DC table")?, ac[comp.ac_table].as_ref().context("missing AC table")?);
                        coef.fill(0);
                        let t = bits.decode(dct)?;
                        comp.dc += bits.extend(t);
                        coef[0] = comp.dc as i16;
                        let mut k = 1;
                        while k < 64 {
                            let rs = bits.decode(act)?;
                            let (r, s) = (usize::from(rs >> 4), rs & 15);
                            if s > 0 {
                                k += r;
                                let v = bits.extend(s);
                                coef[NATURAL[k.min(63)]] = v as i16;
                            } else if r == 15 {
                                k += 15;
                            } else {
                                break;
                            }
                            k += 1;
                        }
                        let (bx, by) = (b % bw, b / bw);
                        let stride = bw * 8;
                        idct_islow(&coef, &quant[comp.quant], &mut comp.plane[by * 8 * stride + bx * 8..], stride);
                    }
                }
                // on to the marker after the scan
                p = bits.p;
                while p + 1 < data.len() && !(data[p] == 0xFF && data[p + 1] != 0 && !(0xD0..=0xD7).contains(&data[p + 1])) {
                    p += 1;
                }
            }
            _ => {}
        }
    }
    ensure!(!comps.is_empty(), "no frame");
    let stride = w.div_ceil(8) * 8;
    let mut out = Vec::with_capacity(w * h * 3);
    // default_decompress_parms: three components are YCbCr unless their ids are R G B, or an Adobe marker without
    // JFIF says transform 0
    let rgb = comps.len() == 3
        && match (comps[0].id, comps[1].id, comps[2].id) {
            (1, 2, 3) => false,
            (b'R', b'G', b'B') => true,
            _ => !jfif && adobe == Some(0),
        };
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x;
            out.extend_from_slice(&match comps.len() {
                1 => [comps[0].plane[i]; 3],
                _ if rgb => [comps[0].plane[i], comps[1].plane[i], comps[2].plane[i]],
                _ => ycc_rgb(comps[0].plane[i], comps[1].plane[i], comps[2].plane[i]),
            });
        }
    }
    Ok((w as u32, h as u32, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ycc_matches_libjpeg9() {
        // v9's Cb -> G (22553) gives y 100 / cb 2 / cr 97 green 165, libjpeg 6 / 8's 22554 166
        assert_eq!(ycc_rgb(128, 128, 128), [128, 128, 128]);
        assert_eq!(ycc_rgb(100, 2, 97)[1], 165);
        assert_eq!(ycc_rgb(255, 255, 255), [255, 121, 255]);
    }

    #[test]
    fn idct_dc_only() {
        let mut coef = [0i16; 64];
        coef[0] = 10;
        let mut q = [1u16; 64];
        q[0] = 8;
        let mut out = [0u8; 64];
        idct_islow(&coef, &q, &mut out, 8);
        // DC 80: 80 / 8 + 128
        assert!(out.iter().all(|&v| v == 138));
    }
}
