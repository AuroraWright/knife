//! iType's fixed-point helpers (the SDK's varmul_64 / vardiv_64 / muldiv_64), rounding to nearest with the
//! sign applied after the magnitude, saturating at 0x7FFFFFFF.

/// (a * b) >> shift, rounded.
pub fn varmul(a: i32, b: i32, shift: u32) -> i32 {
    if a == 0 || b == 0 {
        return 0;
    }
    let neg = (a ^ b) < 0;
    let (a, b) = (a.unsigned_abs(), b.unsigned_abs());
    let m = if a > 46339 || b > 46339 {
        ((u64::from(a) * u64::from(b) + (1 << (shift - 1))) >> shift).min(0x7FFF_FFFF) as u32
    } else {
        (a * b + (1 << (shift - 1))) >> shift
    };
    if neg { m.wrapping_neg() as i32 } else { m as i32 }
}

/// (a << shift) / b, rounded; b = 0 gives the largest value of a's sign.
pub fn vardiv(a: i32, b: i32, shift: u32) -> i32 {
    if b == 0 {
        return if a >= 0 { 0x7FFF_FFFF } else { -0x7FFF_FFFF };
    }
    if a == 0 {
        return 0;
    }
    let neg = (a ^ b) < 0;
    let (a, b) = (a.unsigned_abs(), b.unsigned_abs());
    let m = if a >= 1 << (30 - shift) {
        (((u64::from(a) << shift) + u64::from(b >> 1)) / u64::from(b)).min(0x7FFF_FFFF) as u32
    } else {
        (((a << shift) + (b >> 1)) as i32 / b as i32) as u32
    };
    if neg { m.wrapping_neg() as i32 } else { m as i32 }
}

/// a * b / c, rounded; c = 0 gives the largest value of the result's sign.
pub fn muldiv(a: i32, b: i32, c: i32) -> i32 {
    if a == 0 || b == 0 {
        return 0;
    }
    let neg = (a ^ b ^ c) < 0;
    if c == 0 {
        return if neg { -0x7FFF_FFFF } else { 0x7FFF_FFFF };
    }
    let (a, b, c) = (a.unsigned_abs(), b.unsigned_abs(), c.unsigned_abs());
    let m = if a > 46339 || b > 46339 || c > 176095 {
        ((u64::from(c >> 1) + u64::from(a) * u64::from(b)) / u64::from(c)).min(0x7FFF_FFFF) as u32
    } else {
        (a * b + (c >> 1)) / c
    };
    if neg { m.wrapping_neg() as i32 } else { m as i32 }
}

/// A vector's length, roughly: the larger component plus a third of the smaller (norm).
pub fn norm(a: i32, b: i32) -> i32 {
    let (a, b) = (a.unsigned_abs(), b.unsigned_abs());
    (if a > b { a + b / 3 } else { b + a / 3 }) as i32
}

/// (|a| << shift) / m rounded, in 64 bits when |a| reaches `wide`, saturating, with a's sign.
fn scaled(a: i32, m: u32, shift: u32, wide: u32) -> i32 {
    let v = a.unsigned_abs();
    let q = if v >= wide {
        ((u64::from(m >> 1) + (u64::from(v) << shift)) / u64::from(m)).min(0x7FFF_FFFF) as i32
    } else {
        ((m >> 1) + (v << shift)) as i32 / m as i32
    };
    if a < 0 { -q } else { q }
}

/// (v * v + 0x8000) >> 16 for |v|, in 64 bits past 46339, saturating.
fn square(v: i32) -> u32 {
    let a = v.unsigned_abs();
    if a as i32 <= 46339 {
        (a.wrapping_mul(a).wrapping_add(0x8000)) >> 16
    } else {
        ((u64::from(a) * u64::from(a) + 0x8000) >> 16).min(0x7FFF_FFFF) as u32
    }
}

/// A vector scaled to unit length, 16.16 (fixed_norm): small vectors are first brought up to 8 bits, large ones
/// down to 16, the length is a bitwise square root of the 16.16 sum of squares.
pub fn normalize(v: [i32; 2]) -> [i32; 2] {
    let [mut x, mut y] = v;
    if y == 0 {
        return [if x < 0 { -0x10000 } else { 0x10000 }, y];
    }
    if x == 0 {
        return [x, if y < 0 { -0x10000 } else { 0x10000 }];
    }
    let m = x.unsigned_abs().max(y.unsigned_abs());
    if m as i32 <= 0x10000 {
        if m <= 255 {
            x = scaled(x, m, 8, 46340);
            y = scaled(y, m, 8, 46340);
        }
    } else {
        x = scaled(x, m, 16, 0x4000);
        y = scaled(y, m, 16, 0x4000);
    }
    let x_zero = x == 0;
    let sx = if x_zero { 0 } else { square(x) };
    let sy = if y == 0 { 0 } else { square(y) };
    let s = sy.wrapping_add(sx);
    let hi = u32::from(s >> 16 != 0);
    let mut rem = if s >= 0x10000 { s.wrapping_mul(2).wrapping_sub(0x20000) } else { s.wrapping_mul(2) };
    let first = (hi << 17) | 0x8000;
    let mut r = if rem >= first { (hi << 17) | 0x10000 } else { hi << 17 };
    rem = rem.wrapping_sub(if rem >= first { first } else { 0 });
    let mut bit = 0x4000u32;
    while bit >= 4 {
        let t = r.wrapping_add(bit);
        let d = rem.wrapping_mul(2);
        let sub = if d >= t {
            r = r.wrapping_add(2 * bit);
            t
        } else {
            0
        };
        rem = d.wrapping_sub(sub);
        bit >>= 1;
    }
    let t = r.wrapping_add(2);
    let d = rem.wrapping_mul(2);
    if d >= t {
        r = r.wrapping_add(4);
    }
    let sub = if d < t { 0 } else { t };
    if r.wrapping_add(1) <= d.wrapping_sub(sub).wrapping_mul(2) {
        r = r.wrapping_add(2);
    }
    let len = r >> 1;
    if len == 0 {
        let sat = |v: i32| if v >= 0 { 0x7FFF_FFFF } else { -0x7FFF_FFFF };
        return [sat(x), sat(y)];
    }
    let div = |v: i32| scaled_round(v, len, r >> 2);
    let nx = if x_zero { 0 } else { div(x) };
    let ny = if y == 0 { y } else { div(y) };
    [nx, ny]
}

/// ((|v| << 16) + half) / len with v's sign, in 64 bits from 0x4000, saturating.
fn scaled_round(v: i32, len: u32, half: u32) -> i32 {
    let a = v.unsigned_abs();
    let q = if a >= 0x4000 {
        ((u64::from(half) + (u64::from(a) << 16)) / u64::from(len)).min(0x7FFF_FFFF) as i32
    } else {
        (half.wrapping_add(a << 16)) as i32 / len as i32
    };
    if v < 0 { -q } else { q }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding() {
        assert_eq!(varmul(0x18000, 0x10000, 16), 0x18000);
        assert_eq!(varmul(-3, 0x8000, 16), -2);
        assert_eq!(vardiv(1, 3, 16), 21845);
        assert_eq!(vardiv(-1, 0, 16), -0x7FFF_FFFF);
        assert_eq!(muldiv(7, -5, 2), -18);
    }
}
