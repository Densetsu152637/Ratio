//! Deterministic floating-point functions for `math` (chapter 06, "IEEE 754 binary64
//! throughout, no fused operations, so results are identical on every machine").
//!
//! `+ - * /`, `sqrt`, `floor`, `ceil`, `trunc` and conversions are exactly specified by IEEE 754,
//! so every platform computes the same bits. `exp`, `ln`, `log2`, `log10` and `pow` are not:
//! the C library or the CPU gives different last bits on different machines. They are therefore
//! written out here, from the algorithms of fdlibm (Sun Microsystems, public domain), using
//! only the exactly specified operations and bit manipulation, never `f64::exp` and friends and
//! never a fused multiply-add. The functions are accurate to better than one unit in the last
//! place (the tests compare them with the platform's library), exact where the result is
//! exactly representable (`pow(2, 10)`, `log2(8)`, `log10(1000)`), and follow the special
//! cases of IEEE 754 (`pow(x, 0) = 1`, `pow(1, y) = 1` is NaN-free, and so on).
//!
//! Every function is O(1): a fixed number of operations whatever the argument.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    clippy::excessive_precision,
    clippy::float_cmp,
    clippy::approx_constant,
    reason = "fdlibm's constants and bit tricks, kept as published so they can be compared"
)]

const HUGE: f64 = 1.0e300;
const TINY: f64 = 1.0e-300;

fn hi(x: f64) -> i32 {
    (x.to_bits() >> 32) as u32 as i32
}

fn lo(x: f64) -> u32 {
    x.to_bits() as u32
}

fn with_hi(x: f64, h: i32) -> f64 {
    f64::from_bits((u64::from(h as u32) << 32) | (x.to_bits() & 0xffff_ffff))
}

fn with_lo(x: f64, l: u32) -> f64 {
    f64::from_bits((x.to_bits() & 0xffff_ffff_0000_0000) | u64::from(l))
}

fn from_words(h: i32, l: u32) -> f64 {
    f64::from_bits((u64::from(h as u32) << 32) | u64::from(l))
}

/// `2^n` as a float for `n` in the normal range.
fn pow2(n: i32) -> f64 {
    f64::from_bits(((i64::from(n) + 1023) as u64) << 52)
}

/// `y · 2^k`, rounding once even when the result is subnormal (`scalbn`).
fn scalbn(mut y: f64, mut k: i32) -> f64 {
    // Split the scaling so each factor is a normal power of two: at most three multiplications,
    // and the last one is the only one that can round.
    if k > 1023 {
        y *= pow2(1023);
        k -= 1023;
        if k > 1023 {
            y *= pow2(1023);
            k -= 1023;
            if k > 1023 {
                k = 1023;
            }
        }
    } else if k < -1022 {
        // 2^-1022 · 2^53 keeps the intermediate result normal.
        y *= pow2(-1022) * pow2(53);
        k += 1022 - 53;
        if k < -1022 {
            y *= pow2(-1022) * pow2(53);
            k += 1022 - 53;
            if k < -1022 {
                k = -1022;
            }
        }
    }
    y * pow2(k)
}

// ---- exp ----

const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01;
const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10;
const INV_LN2: f64 = 1.442_695_040_888_963_387_00e+00;
const P1: f64 = 1.666_666_666_666_660_190_37e-01;
const P2: f64 = -2.777_777_777_701_559_338_42e-03;
const P3: f64 = 6.613_756_321_437_934_361_17e-05;
const P4: f64 = -1.653_390_220_546_525_153_90e-06;
const P5: f64 = 4.138_136_797_057_238_460_39e-08;

/// `e` to the `x`.
#[must_use]
pub(crate) fn exp(mut x: f64) -> f64 {
    const O_THRESHOLD: f64 = 7.097_827_128_933_839_730_96e+02;
    const U_THRESHOLD: f64 = -7.451_332_191_019_411_084_20e+02;
    let mut hx = hi(x);
    let xsb = ((hx >> 31) & 1) as usize;
    hx &= 0x7fff_ffff;
    let (mut hi_part, mut lo_part) = (0.0, 0.0);
    let k: i32;
    if hx >= 0x4086_2E42 {
        // |x| >= 709.78...
        if hx >= 0x7ff0_0000 {
            if (hx & 0xf_ffff) as u32 | lo(x) != 0 {
                return x + x; // NaN
            }
            return if xsb == 0 { x } else { 0.0 };
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE;
        }
        if x < U_THRESHOLD {
            return TINY * TINY;
        }
    }
    if hx > 0x3fd6_2e42 {
        // |x| > 0.5 ln 2
        if hx < 0x3FF0_A2B2 {
            // and |x| < 1.5 ln 2
            let (h, l) = if xsb == 0 {
                (LN2_HI, LN2_LO)
            } else {
                (-LN2_HI, -LN2_LO)
            };
            hi_part = x - h;
            lo_part = l;
            k = 1 - xsb as i32 - xsb as i32;
        } else {
            let half = if xsb == 0 { 0.5 } else { -0.5 };
            k = (INV_LN2 * x + half) as i32;
            let t = f64::from(k);
            hi_part = x - t * LN2_HI;
            lo_part = t * LN2_LO;
        }
        x = hi_part - lo_part;
    } else if hx < 0x3e30_0000 {
        // |x| < 2^-28
        if HUGE + x > 1.0 {
            return 1.0 + x;
        }
        k = 0;
    } else {
        k = 0;
    }
    let t = x * x;
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo_part - (x * c) / (2.0 - c)) - hi_part);
    scalbn(y, k)
}

// ---- ln ----

const LG1: f64 = 6.666_666_666_666_735_130e-01;
const LG2: f64 = 3.999_999_999_940_941_908e-01;
const LG3: f64 = 2.857_142_874_366_239_149e-01;
const LG4: f64 = 2.222_219_843_214_978_396e-01;
const LG5: f64 = 1.818_357_216_161_805_012e-01;
const LG6: f64 = 1.531_383_769_920_937_332e-01;
const LG7: f64 = 1.479_819_860_511_658_591e-01;
const TWO54: f64 = 1.801_439_850_948_198_400_00e+16;

/// The natural logarithm.
#[must_use]
pub(crate) fn ln(mut x: f64) -> f64 {
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        // x < 2^-1022
        if (hx & 0x7fff_ffff) as u32 | lx == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x95f64) & 0x10_0000;
    x = with_hi(x, hx | (i ^ 0x3ff0_0000)); // normalise x or x/2
    k += i >> 20;
    let f = x - 1.0;
    if (0x000f_ffff & (2 + hx)) < 3 {
        // |f| < 2^-20
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            let dk = f64::from(k);
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - 0.333_333_333_333_333_33 * f);
        if k == 0 {
            return f - r;
        }
        let dk = f64::from(k);
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }
    let s = f / (2.0 + f);
    let dk = f64::from(k);
    let z = s * s;
    let i = hx - 0x6147a;
    let w = z * z;
    let j = 0x6b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let i = i | j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

// ---- log2 and log10 ----

/// The base-10 logarithm (`log10(10^n) = n` for the powers of ten that are exact).
#[must_use]
pub(crate) fn log10(mut x: f64) -> f64 {
    const IVLN10: f64 = 4.342_944_819_032_518_166_68e-01;
    const LOG10_2HI: f64 = 3.010_299_956_636_117_713_06e-01;
    const LOG10_2LO: f64 = 3.694_239_077_158_930_786_16e-13;
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        if (hx & 0x7fff_ffff) as u32 | lx == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    let i = ((k as u32) & 0x8000_0000) >> 31;
    hx = (hx & 0x000f_ffff) | ((0x3ff - i as i32) << 20);
    let y = f64::from(k + i as i32);
    x = with_hi(x, hx);
    let z = y * LOG10_2LO + IVLN10 * ln(x);
    z + y * LOG10_2HI
}

/// The base-2 logarithm (exact for powers of two).
#[must_use]
pub(crate) fn log2(mut x: f64) -> f64 {
    const IVLN2_HI: f64 = 1.442_695_040_721_446_275_71e+00;
    const IVLN2_LO: f64 = 1.675_171_316_488_651_183_53e-10;
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        if (hx & 0x7fff_ffff) as u32 | lx == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    if hx == 0x3ff0_0000 && lo(x) == 0 {
        return 0.0;
    }
    // x = m · 2^k with m in [sqrt(2)/2, sqrt(2)).
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x95f64) & 0x10_0000;
    x = with_hi(x, hx | (i ^ 0x3ff0_0000));
    k += i >> 20;
    let y = f64::from(k);
    let l = ln(x);
    // l · (1/ln 2) in two parts, so that the product with the high part of 1/ln 2 is exact.
    let l_hi = f64::from_bits(l.to_bits() & 0xffff_ffff_0000_0000);
    let l_lo = l - l_hi;
    let mut val_hi = l_hi * IVLN2_HI;
    let mut val_lo = (l_lo + l_hi) * IVLN2_LO + l_lo * IVLN2_HI;
    let w = y + val_hi;
    val_lo += (y - w) + val_hi;
    val_hi = w;
    val_lo + val_hi
}

// ---- pow ----

/// `x` to the `y`, with the special cases of IEEE 754 and C99 (`pow(1, y) = 1`,
/// `pow(x, 0) = 1`, `pow(-1, ±inf) = 1`).
#[must_use]
#[allow(clippy::too_many_lines, reason = "one routine, as in fdlibm")]
pub(crate) fn pow(x: f64, y: f64) -> f64 {
    const BP: [f64; 2] = [1.0, 1.5];
    const DP_H: [f64; 2] = [0.0, 5.849_624_872_207_641_601_56e-01];
    const DP_L: [f64; 2] = [0.0, 1.350_039_202_129_748_971_28e-08];
    const TWO53: f64 = 9_007_199_254_740_992.0;
    const L1: f64 = 5.999_999_999_999_946_487_25e-01;
    const L2: f64 = 4.285_714_285_785_501_842_52e-01;
    const L3: f64 = 3.333_333_298_183_774_329_18e-01;
    const L4: f64 = 2.727_281_238_085_340_064_89e-01;
    const L5: f64 = 2.306_607_457_755_617_540_67e-01;
    const L6: f64 = 2.069_750_178_003_384_177_84e-01;
    const LG2: f64 = 6.931_471_805_599_452_862_27e-01;
    const LG2_H: f64 = 6.931_471_824_645_996_093_75e-01;
    const LG2_L: f64 = -1.904_654_299_957_768_045_25e-09;
    const OVT: f64 = 8.008_566_259_537_294_437_2e-17;
    const CP: f64 = 9.617_966_939_259_755_543_29e-01;
    const CP_H: f64 = 9.617_967_009_544_372_558_59e-01;
    const CP_L: f64 = -7.028_461_650_952_758_265_16e-09;
    const IVLN2: f64 = 1.442_695_040_888_963_387_00e+00;
    const IVLN2_H: f64 = 1.442_695_021_629_333_496_09e+00;
    const IVLN2_L: f64 = 1.925_962_991_126_617_468_87e-08;

    let hx = hi(x);
    let lx = lo(x);
    let hy = hi(y);
    let ly = lo(y);
    let ix = hx & 0x7fff_ffff;
    let iy = hy & 0x7fff_ffff;

    // x**0 = 1, even for NaN.
    if (iy as u32) | ly == 0 {
        return 1.0;
    }
    // 1**y = 1, even for NaN.
    if hx == 0x3ff0_0000 && lx == 0 {
        return 1.0;
    }
    // NaN arguments.
    if ix > 0x7ff0_0000
        || (ix == 0x7ff0_0000 && lx != 0)
        || iy > 0x7ff0_0000
        || (iy == 0x7ff0_0000 && ly != 0)
    {
        return x + y;
    }
    // Whether y is an odd integer when x < 0: 0 not an integer, 1 odd, 2 even.
    let mut yisint = 0i32;
    if hx < 0 {
        if iy >= 0x4340_0000 {
            yisint = 2;
        } else if iy >= 0x3ff0_0000 {
            let k = (iy >> 20) - 0x3ff;
            if k > 20 {
                let j = ly >> (52 - k);
                if (j << (52 - k)) == ly {
                    yisint = 2 - (j & 1) as i32;
                }
            } else if ly == 0 {
                let j = iy >> (20 - k);
                if (j << (20 - k)) == iy {
                    yisint = 2 - (j & 1);
                }
            }
        }
    }
    // Special values of y.
    if ly == 0 {
        if iy == 0x7ff0_0000 {
            // y is +-inf
            if (ix - 0x3ff0_0000) as u32 | lx == 0 {
                return 1.0; // (+-1)**+-inf = 1 (C99)
            } else if ix >= 0x3ff0_0000 {
                // (|x| > 1)**+-inf = inf, 0
                return if hy >= 0 { y } else { 0.0 };
            }
            // (|x| < 1)**-,+inf = inf, 0
            return if hy < 0 { -y } else { 0.0 };
        }
        if iy == 0x3ff0_0000 {
            // y is +-1
            return if hy < 0 { 1.0 / x } else { x };
        }
        if hy == 0x4000_0000 {
            return x * x;
        }
        if hy == 0x3fe0_0000 && hx >= 0 {
            return x.sqrt();
        }
    }
    let mut ax = x.abs();
    // Special values of x.
    if lx == 0 && (ix == 0x7ff0_0000 || ix == 0 || ix == 0x3ff0_0000) {
        let mut z = ax; // +-0, +-inf, +-1
        if hy < 0 {
            z = 1.0 / z;
        }
        if hx < 0 {
            if (ix - 0x3ff0_0000) | yisint == 0 {
                z = f64::NAN; // (-1)**non-int
            } else if yisint == 1 {
                z = -z; // (x<0)**odd
            }
        }
        return z;
    }
    let n_sign = ((hx as u32) >> 31) as i32 - 1; // 0 for x < 0, -1 otherwise
    // (x<0)**(non-int) is NaN.
    if n_sign | yisint == 0 {
        return f64::NAN;
    }
    // The sign of the result: -1 for (negative)**(odd integer).
    let s = if n_sign | (yisint - 1) == 0 {
        -1.0
    } else {
        1.0
    };

    let (t1, t2);
    if iy > 0x41e0_0000 {
        // |y| > 2^31
        if iy > 0x43f0_0000 {
            // |y| > 2^64: must overflow or underflow
            if ix <= 0x3fef_ffff {
                return if hy < 0 { HUGE * HUGE } else { TINY * TINY };
            }
            if ix >= 0x3ff0_0000 {
                return if hy > 0 { HUGE * HUGE } else { TINY * TINY };
            }
        }
        // Over- or underflow if x is not close to one.
        if ix < 0x3fef_ffff {
            return if hy < 0 {
                s * HUGE * HUGE
            } else {
                s * TINY * TINY
            };
        }
        if ix > 0x3ff0_0000 {
            return if hy > 0 {
                s * HUGE * HUGE
            } else {
                s * TINY * TINY
            };
        }
        // |1 - x| <= 2^-20: log(x) by x - x^2/2 + x^3/3 - x^4/4.
        let t = ax - 1.0;
        let w = (t * t) * (0.5 - t * (0.333_333_333_333_333_333_333_3 - t * 0.25));
        let u = IVLN2_H * t;
        let v = t * IVLN2_L - w * IVLN2;
        let mut a = u + v;
        a = with_lo(a, 0);
        t1 = a;
        t2 = v - (a - u);
    } else {
        let mut n = 0i32;
        let mut ix = ix;
        // Subnormal x.
        if ix < 0x0010_0000 {
            ax *= TWO53;
            n -= 53;
            ix = hi(ax);
        }
        n += (ix >> 20) - 0x3ff;
        let j = ix & 0x000f_ffff;
        // The interval.
        ix = j | 0x3ff0_0000;
        let k: usize = if j <= 0x3988E {
            0
        } else if j < 0xBB67A {
            1
        } else {
            n += 1;
            ix -= 0x0010_0000;
            0
        };
        ax = with_hi(ax, ix);
        // ss = s_h + s_l = (x - 1)/(x + 1) or (x - 1.5)/(x + 1.5).
        let u = ax - BP[k];
        let v = 1.0 / (ax + BP[k]);
        let ss = u * v;
        let s_h = with_lo(ss, 0);
        // t_h = ax + bp[k], high part.
        let t_h = from_words(
            ((ix >> 1) | 0x2000_0000) + 0x0008_0000 + ((k as i32) << 18),
            0,
        );
        let t_l = ax - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        // log(ax).
        let s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        let s2 = s_h * s_h;
        let t_h = with_lo(3.0 + s2 + r, 0);
        let t_l = r - ((t_h - 3.0) - s2);
        // u + v = ss * (1 + ...)
        let u = s_h * t_h;
        let v = s_l * t_h + t_l * ss;
        // 2/(3 log 2) * (ss + ...)
        let p_h = with_lo(u + v, 0);
        let p_l = v - (p_h - u);
        let z_h = CP_H * p_h; // cp_h + cp_l = 2/(3 log 2)
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        // log2(ax) = n + dp_h + z_h + z_l
        let t = f64::from(n);
        let a = with_lo(((z_h + z_l) + DP_H[k]) + t, 0);
        t1 = a;
        t2 = z_l - (((a - t) - DP_H[k]) - z_h);
    }
    // Split y into y1 + y2 and compute (y1 + y2) * (t1 + t2).
    let y1 = with_lo(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let z = p_l + p_h;
    let j = hi(z);
    let i = lo(z);
    if j >= 0x4090_0000 {
        // z >= 1024
        if (j - 0x4090_0000) as u32 | i != 0 || p_l + OVT > z - p_h {
            return s * HUGE * HUGE;
        }
    } else if (j & 0x7fff_ffff) >= 0x4090_cc00
        && ((j as u32).wrapping_sub(0xc090_cc00) | i != 0 || p_l <= z - p_h)
    {
        // z <= -1075
        return s * TINY * TINY;
    }
    // 2**(p_h + p_l)
    let i = j & 0x7fff_ffff;
    let mut k = (i >> 20) - 0x3ff;
    let mut n = 0i32;
    if i > 0x3fe0_0000 {
        // |z| > 0.5: n = [z + 0.5]
        n = j + (0x0010_0000 >> (k + 1));
        k = ((n & 0x7fff_ffff) >> 20) - 0x3ff;
        let t = from_words(n & !(0x000f_ffff >> k), 0);
        n = ((n & 0x000f_ffff) | 0x0010_0000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    let t = with_lo(p_l + p_h, 0);
    let u = t * LG2_H;
    let v = (p_l - (t - p_h)) * LG2 + t * LG2_L;
    let z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1 = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let r = (z * t1) / (t1 - 2.0) - (w + z * w);
    let z = 1.0 - (r - z);
    let out = scalbn(z, n);
    s * out
}

// ---- rounding to decimals ----

/// `10^d` for `d` in `0..=22`: every one of them is exactly a double.
const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// The most decimals `round` looks at: a double has at most 1074 digits after the point, but
/// at 340 every double that is not an integer has long since been decided.
pub(crate) const MAX_DIGITS: i64 = 340;

/// `x · p` as an unevaluated sum `hi + lo` with no rounding error (Dekker's product with
/// Veltkamp's splitting; no fused multiply-add).
fn two_product(x: f64, p: f64) -> (f64, f64) {
    const SPLIT: f64 = 134_217_729.0; // 2^27 + 1
    fn split(a: f64) -> (f64, f64) {
        let c = SPLIT * a;
        let big = c - a;
        let h = c - big;
        (h, a - h)
    }
    let hi = x * p;
    let (xh, xl) = split(x);
    let (ph, pl) = split(p);
    let lo = ((xh * ph - hi) + xh * pl + xl * ph) + xl * pl;
    (hi, lo)
}

/// `x` rounded to `digits` decimals, half away from zero, judged on the exact binary value of
/// `x`: `round(2.675, 2)` is `2.67` because the double nearest 2.675 is just below it.
///
/// For `digits` in `0..=22` the scaling `x · 10^digits` is computed exactly (as a sum of two
/// doubles), so the decision is exact; for a negative count (rounding to tens, hundreds, ...,
/// up to 10^22) the quotient is rounded once before the decision; beyond 22 decimals a double
/// of magnitude 10^-5 or more is returned as it is, and a smaller one is rounded through its
/// exact decimal expansion. A non-finite `x` is returned as it is.
#[must_use]
pub(crate) fn round_digits(x: f64, digits: i64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if digits >= 0 {
        if digits <= 22 {
            let p = POW10[digits as usize];
            let y = x * p;
            // Past 2^52 every double is an integer: `x` has no decimals left to round.
            if !y.is_finite() || y.abs() >= 4_503_599_627_370_496.0 {
                return x;
            }
            let (y, e) = two_product(x, p);
            // `y + e` is x · 10^digits exactly; decide the rounding of it.
            let r = round_half_away_pair(y, e);
            let out = r / p;
            return if out == 0.0 { 0.0f64.copysign(x) } else { out };
        }
        // Beyond 22 decimals: identity when the spacing of the doubles near x is wider than
        // the decimal quantum (the common case), the exact decimal expansion otherwise.
        let d = digits.min(MAX_DIGITS);
        let spacing = x.abs() * f64::EPSILON;
        if let Ok(quantum) = format!("1e-{d}").parse::<f64>()
            && quantum <= spacing / 4.0
        {
            return x;
        }
        // `{:.N}` prints the exact expansion rounded correctly (ties to even: a tie needs the
        // expansion to end in 5 at exactly this digit, which for these tiny values cannot
        // happen below 2^-1074 spacing).
        let text = format!("{:.*}", d as usize, x);
        return text.parse::<f64>().unwrap_or(x);
    }
    // Negative digits: round to a multiple of 10^-digits.
    let d = digits.saturating_neg().min(22);
    let p = POW10[d as usize];
    let q = x / p;
    let r = q.abs().floor();
    let frac = q.abs() - r;
    let up = frac >= 0.5;
    let r = if up { r + 1.0 } else { r };
    let out = (r * p).copysign(x);
    if out.is_finite() { out } else { x }
}

/// Rounds `y + e` (an exact value written as a sum, `|e| <= ulp(y) / 2`, `|y| < 2^52`) half
/// away from zero.
fn round_half_away_pair(y: f64, e: f64) -> f64 {
    let a = y.abs();
    let signed_e = if y < 0.0 { -e } else { e };
    let r = a.floor();
    // `a - r` and `- 0.5` are exact, so the sign of the sum with the error term is the sign of
    // the exact difference between the fraction and a half: at or above a half rounds up.
    let up = (a - r - 0.5) + signed_e >= 0.0;
    let r = if up { r + 1.0 } else { r };
    r.copysign(y)
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, reason = "test inputs")]
mod tests {
    use super::*;

    fn ulps(a: f64, b: f64) -> u64 {
        if a == b || (a.is_nan() && b.is_nan()) {
            return 0;
        }
        if a.is_nan() || b.is_nan() || a.is_infinite() != b.is_infinite() {
            return u64::MAX;
        }
        let key = |x: f64| {
            let bits = x.to_bits() as i64;
            if bits < 0 { i64::MIN - bits } else { bits }
        };
        key(a).abs_diff(key(b))
    }

    /// A small deterministic generator (xorshift64*), so the sweeps are the same everywhere.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }

        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.unit()
        }
    }

    #[test]
    fn exp_agrees_with_the_platform_within_one_ulp_and_is_exact_at_zero() {
        let mut rng = Rng(1);
        let mut worst = 0;
        for _ in 0..20_000 {
            let x = rng.range(-700.0, 700.0);
            worst = worst.max(ulps(exp(x), x.exp()));
        }
        for _ in 0..20_000 {
            let x = rng.range(-2.0, 2.0);
            worst = worst.max(ulps(exp(x), x.exp()));
        }
        assert!(worst <= 2, "exp is {worst} ulps from the platform");
        assert_eq!(exp(0.0).to_bits(), 1.0f64.to_bits());
        assert!(exp(f64::NAN).is_nan());
        assert_eq!(exp(f64::INFINITY), f64::INFINITY);
        assert_eq!(exp(f64::NEG_INFINITY).to_bits(), 0.0f64.to_bits());
        assert_eq!(exp(710.0), f64::INFINITY);
        assert_eq!(exp(-746.0).to_bits(), 0.0f64.to_bits());
    }

    #[test]
    fn ln_log2_and_log10_agree_with_the_platform() {
        let mut rng = Rng(2);
        let mut worst = (0, 0, 0);
        for _ in 0..30_000 {
            let x = rng.range(0.0, 1.0e6);
            let y = (rng.range(-300.0, 300.0)).exp2();
            for v in [x, y] {
                worst.0 = worst.0.max(ulps(ln(v), v.ln()));
                worst.1 = worst.1.max(ulps(log2(v), v.log2()));
                worst.2 = worst.2.max(ulps(log10(v), v.log10()));
            }
        }
        assert!(worst.0 <= 2, "ln is {} ulps off", worst.0);
        assert!(worst.1 <= 2, "log2 is {} ulps off", worst.1);
        assert!(worst.2 <= 2, "log10 is {} ulps off", worst.2);
    }

    #[test]
    fn logarithms_are_exact_where_the_answer_is_an_integer() {
        for n in -1074..=1023 {
            let x = if n >= -1022 {
                pow2(n)
            } else {
                f64::from_bits(1u64 << (n + 1074))
            };
            assert_eq!(log2(x).to_bits(), f64::from(n).to_bits(), "log2(2^{n})");
        }
        for n in 0..=22 {
            assert_eq!(
                log10(POW10[n as usize]).to_bits(),
                f64::from(n).to_bits(),
                "log10(10^{n})"
            );
        }
        assert_eq!(ln(1.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(ln(0.0), f64::NEG_INFINITY);
        assert!(ln(-1.0).is_nan());
        assert!(log2(-1.0).is_nan());
        assert!(log10(f64::NAN).is_nan());
        assert_eq!(log10(f64::INFINITY), f64::INFINITY);
        assert_eq!(log2(5e-324).to_bits(), (-1074.0f64).to_bits());
    }

    #[test]
    fn pow_agrees_with_the_platform_and_is_exact_for_exact_results() {
        let mut rng = Rng(3);
        let mut worst = 0;
        for _ in 0..40_000 {
            let x = rng.range(0.0, 50.0);
            let y = rng.range(-30.0, 30.0);
            worst = worst.max(ulps(pow(x, y), x.powf(y)));
            let x = -rng.range(0.0, 50.0);
            let y = (rng.range(-20.0, 20.0)).round();
            worst = worst.max(ulps(pow(x, y), x.powf(y)));
        }
        assert!(worst <= 2, "pow is {worst} ulps off");
        for (x, y, want) in [
            (2.0f64, 10.0f64, 1024.0f64),
            (10.0, 2.0, 100.0),
            (10.0, 15.0, 1e15),
            (3.0, 4.0, 81.0),
            (-2.0, 3.0, -8.0),
            (-2.0, 4.0, 16.0),
            (4.0, 0.5, 2.0),
            (2.0, -2.0, 0.25),
            (2.0, 0.0, 1.0),
            (0.0, 0.0, 1.0),
            (f64::NAN, 0.0, 1.0),
            (1.0, f64::NAN, 1.0),
            (-1.0, f64::INFINITY, 1.0),
        ] {
            assert_eq!(pow(x, y).to_bits(), want.to_bits(), "pow({x}, {y})");
        }
        assert!(pow(-2.0, 0.5).is_nan());
        assert!(pow(f64::NAN, 1.0).is_nan());
        assert_eq!(pow(0.0, -1.0), f64::INFINITY);
        assert_eq!(pow(-0.0, -1.0), f64::NEG_INFINITY);
        assert_eq!(pow(2.0, 1024.0), f64::INFINITY);
        assert_eq!(pow(2.0, -1075.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(pow(2.0, -1074.0).to_bits(), 1);
        assert_eq!(pow(0.5, f64::INFINITY).to_bits(), 0.0f64.to_bits());
        assert_eq!(pow(2.0, f64::INFINITY), f64::INFINITY);
        assert_eq!(pow(2.0, f64::NEG_INFINITY).to_bits(), 0.0f64.to_bits());
    }

    /// Bit patterns of fixed inputs. They are the same on every platform and every compiler
    /// version, because only exactly specified operations are used; a change to the
    /// algorithms must change them deliberately.
    #[test]
    fn fixed_bit_patterns() {
        let cases: [(&str, f64, u64); 10] = [
            ("exp(1)", exp(1.0), 0x4005_bf0a_8b14_576a),
            ("exp(-1)", exp(-1.0), 0x3fd7_8b56_362c_ef38),
            ("exp(0.5)", exp(0.5), 0x3ffa_6129_8e1e_069c),
            ("ln(2)", ln(2.0), 0x3fe6_2e42_fefa_39ef),
            ("ln(10)", ln(10.0), 0x4002_6bb1_bbb5_5516),
            ("log2(10)", log2(10.0), 0x400a_934f_0979_a371),
            ("log10(2)", log10(2.0), 0x3fd3_4413_509f_79ff),
            ("pow(2, 0.5)", pow(2.0, 0.5), 0x3ff6_a09e_667f_3bcd),
            ("pow(10, -3)", pow(10.0, -3.0), 0x3f50_624d_d2f1_a9fc),
            ("pow(1.1, 10)", pow(1.1, 10.0), 0x4004_bffc_0c03_023e),
        ];
        for (name, got, want) in cases {
            assert_eq!(got.to_bits(), want, "{name}: got {:#018x}", got.to_bits());
        }
    }

    #[test]
    fn round_is_decided_on_the_exact_value() {
        assert_eq!(round_digits(2.5, 0), 3.0);
        assert_eq!(round_digits(-2.5, 0), -3.0);
        assert_eq!(round_digits(0.5, 0), 1.0);
        assert_eq!(round_digits(-0.5, 0).to_bits(), (-1.0f64).to_bits());
        assert_eq!(round_digits(1.45, 1), 1.4); // the double nearest 1.45 is below it
        assert_eq!(round_digits(2.675, 2), 2.67);
        assert_eq!(round_digits(1.005, 2), 1.0);
        assert_eq!(round_digits(0.125, 2), 0.13); // exactly half: away from zero
        assert_eq!(round_digits(-0.125, 2), -0.13);
        assert_eq!(round_digits(3.14159, 3), 3.142);
        assert_eq!(round_digits(1234.5678, -2), 1200.0);
        assert_eq!(round_digits(1250.0, -2), 1300.0);
        assert_eq!(round_digits(-1250.0, -2), -1300.0);
        assert_eq!(round_digits(0.1 + 0.2, 15), 0.3);
        assert_eq!(round_digits(4e15 + 0.5, 0), 4e15 + 1.0);
        assert_eq!(round_digits(1e300, 5), 1e300);
        assert_eq!(round_digits(1.0e-30, 40), 1.0e-30);
        assert_eq!(round_digits(0.1, 30), 0.1);
        assert_eq!(round_digits(0.4, 0).to_bits(), 0.0f64.to_bits());
        assert_eq!(round_digits(-0.4, 0).to_bits(), (-0.0f64).to_bits());
        assert!(round_digits(f64::NAN, 2).is_nan());
        assert_eq!(round_digits(f64::INFINITY, 2), f64::INFINITY);
        assert_eq!(round_digits(7.0, i64::MAX), 7.0);
        assert_eq!(round_digits(7.5, i64::MIN), 0.0);
    }

    /// A checksum of the bits of 20,000 results per function over fixed pseudo-random
    /// inputs: any platform whose library or compiler changes a single bit fails here.
    #[test]
    fn sweep_checksums() {
        fn fnv(f: impl Fn(&mut Rng) -> f64) -> u64 {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            for _ in 0..20_000 {
                h = (h ^ f(&mut rng).to_bits()).wrapping_mul(0x0100_0000_01b3);
            }
            h
        }
        let got = [
            fnv(|r| exp(r.range(-50.0, 50.0))),
            fnv(|r| ln(r.range(1e-6, 1e6))),
            fnv(|r| log2(r.range(1e-6, 1e6))),
            fnv(|r| log10(r.range(1e-6, 1e6))),
            fnv(|r| pow(r.range(0.0, 100.0), r.range(-8.0, 8.0))),
            fnv(|r| round_digits(r.range(-1e6, 1e6), (r.next() % 9) as i64)),
        ];
        assert_eq!(
            got,
            [
                0xd463_b31f_1d40_e240,
                0x1765_cb88_87bf_4fc4,
                0xe247_56e6_469d_be68,
                0x52fb_8c08_7ade_fd11,
                0x51f3_569b_d01a_64e6,
                0x0c2c_630f_7fbd_ed16,
            ]
        );
    }
}
