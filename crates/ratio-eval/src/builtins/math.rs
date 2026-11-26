//! `math` (chapter 06, `data`): numbers and statistics.
//!
//! IEEE 754 binary64 throughout, no fused operations: the scalar functions are `const` class,
//! the statistics `linear` (one pass or two over the list) or `nlogn` (`median`, `percentile`
//! and `mode` sort a copy). `exp`, `ln`, `log2`, `log10` and `pow` come from [`super::fmath`],
//! which uses only exactly specified operations, so a result has the same bits on every
//! machine; a `NaN` result is always the canonical quiet `NaN` (`0x7ff8000000000000`), since
//! the hardware's choice of sign differs.
//!
//! A statistic over `Duration` or `Money` is in seconds or whole currency units (the
//! microsecond and micro-unit counts divided by 10^6), as `Float`.
//!
//! Every result carries the labels of everything that decided it: a scalar function the labels
//! of its operands; a statistic those of every element (a sum reads them all) and of the list.
//! Buffers proportional to a list (the sorted copy) are charged to the heap before they are
//! allocated.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "numeric code converts between Int and Float on purpose; Float equality is exact"
)]

use super::data3::{canon, charged, float_arg, float_val, raise_invalid};
use super::fmath;
use super::{Args, Sizes, arg, int, list, new_list, opt, s_weight_first};
use crate::error::RtError;
use crate::heap::{Charge, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::ops::{cmp_natural, log_work};
use crate::value::{Repr, Value};
use crate::work;

/// A number as a float: an `Int`, a `Float`, a `Duration` in seconds or `Money` in units.
fn num_f64(v: &Value) -> Option<f64> {
    match v.repr {
        Repr::Int(i) => Some(i as f64),
        Repr::Float(f) => Some(f),
        Repr::Duration(us) | Repr::Money(us) => Some(us as f64 / 1e6),
        _ => None,
    }
}

// ---- scalars ----

pub(super) fn sign(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    let s = match x.repr {
        Repr::Int(i) | Repr::Duration(i) | Repr::Money(i) => i.signum(),
        Repr::Float(f) => {
            if f.is_nan() || f == 0.0 {
                0
            } else if f < 0.0 {
                -1
            } else {
                1
            }
        }
        _ => return Err(raise_invalid(cx, "`math.sign` needs a number", x.labels)),
    };
    Ok(Value::int(s).with_labels(x.labels))
}

pub(super) fn clamp(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (x, lo, hi) = (arg(cx, args, 0)?, arg(cx, args, 1)?, arg(cx, args, 2)?);
    // Which of the three is returned depends on all of them.
    let labels = x.labels | lo.labels | hi.labels;
    let bad = |cx: &Cx<'_>| raise_invalid(cx, "`math.clamp` needs `lo` <= `hi`", labels);
    let out = match (&x.repr, &lo.repr, &hi.repr) {
        (Repr::Int(a), Repr::Int(l), Repr::Int(h)) => clamp_ord(*a, *l, *h).map(Value::int),
        (Repr::Duration(a), Repr::Duration(l), Repr::Duration(h)) => {
            clamp_ord(*a, *l, *h).map(Value::duration)
        }
        (Repr::Money(a), Repr::Money(l), Repr::Money(h)) => clamp_ord(*a, *l, *h).map(Value::money),
        (Repr::Float(a), Repr::Float(l), Repr::Float(h)) => {
            if l.is_nan() || h.is_nan() || l > h {
                None
            } else if a.is_nan() {
                Some(Value::float(f64::from_bits(0x7ff8_0000_0000_0000)))
            } else if a < l {
                Some(Value::float(*l))
            } else if a > h {
                Some(Value::float(*h))
            } else {
                Some(Value::float(*a))
            }
        }
        _ => {
            return Err(raise_invalid(
                cx,
                "`math.clamp` needs numbers of one type",
                labels,
            ));
        }
    };
    out.map(|v| v.with_labels(labels)).ok_or_else(|| bad(cx))
}

fn clamp_ord(x: i64, lo: i64, hi: i64) -> Option<i64> {
    (lo <= hi).then(|| x.clamp(lo, hi))
}

/// A float rounded to an `Int`, or why it cannot be.
fn to_int(f: f64) -> Option<i64> {
    // 2^63 is the first float past `i64::MAX`; -2^63 is `i64::MIN` exactly.
    if f.is_nan() || f >= 9_223_372_036_854_775_808.0 || f < -9_223_372_036_854_775_808.0 {
        None
    } else {
        Some(f as i64)
    }
}

fn rounded(cx: &Cx<'_>, args: &Args, f: fn(f64) -> f64, what: &str) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    let v = float_arg(cx, x)?;
    to_int(f(v))
        .map(|i| Value::int(i).with_labels(x.labels))
        .ok_or_else(|| {
            raise_invalid(
                cx,
                &format!("`math.{what}` of a number that is NaN or out of range for Int"),
                x.labels,
            )
        })
}

pub(super) fn floor(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    rounded(cx, args, f64::floor, "floor")
}

pub(super) fn ceil(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    rounded(cx, args, f64::ceil, "ceil")
}

pub(super) fn trunc(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    rounded(cx, args, f64::trunc, "trunc")
}

pub(super) fn round(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    let digits = opt(args, 1);
    let labels = x.labels | digits.map_or(Labels::NONE, |d| d.labels);
    let d = match digits {
        Some(d) => int(cx, d)?,
        None => 0,
    };
    let v = float_arg(cx, x)?;
    work::add(1 + (d.unsigned_abs().min(fmath::MAX_DIGITS as u64) / 64));
    Ok(float_val(fmath::round_digits(v, d), labels))
}

pub(super) fn to_float(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    let i = int(cx, x)?;
    Ok(float_val(i as f64, x.labels))
}

pub(super) fn sqrt(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    Ok(float_val(float_arg(cx, x)?.sqrt(), x.labels))
}

pub(super) fn exp(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    Ok(float_val(fmath::exp(float_arg(cx, x)?), x.labels))
}

pub(super) fn ln(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    Ok(float_val(fmath::ln(float_arg(cx, x)?), x.labels))
}

pub(super) fn log2(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    Ok(float_val(fmath::log2(float_arg(cx, x)?), x.labels))
}

pub(super) fn log10(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    Ok(float_val(fmath::log10(float_arg(cx, x)?), x.labels))
}

pub(super) fn is_nan(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    Ok(Value::bool(float_arg(cx, x)?.is_nan()).with_labels(x.labels))
}

/// `x` to the `y` by squaring, in at most 64 multiplications.
fn int_pow(mut base: i64, mut exp: u64) -> Option<i64> {
    let mut acc: i64 = 1;
    while exp > 0 {
        if exp & 1 == 1 {
            acc = acc.checked_mul(base)?;
        }
        exp >>= 1;
        if exp > 0 {
            base = base.checked_mul(base)?;
        }
    }
    Some(acc)
}

pub(super) fn pow(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (x, y) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = x.labels | y.labels;
    match (&x.repr, &y.repr) {
        (Repr::Int(b), Repr::Int(e)) => {
            let Ok(e) = u64::try_from(*e) else {
                return Err(raise_invalid(
                    cx,
                    "`math.pow` of an Int to a negative Int cannot stay an Int",
                    labels,
                ));
            };
            // 0, 1 and -1 never overflow, whatever the exponent.
            let out = match *b {
                0 => Some(i64::from(e == 0)),
                1 => Some(1),
                -1 => Some(if e % 2 == 0 { 1 } else { -1 }),
                b => int_pow(b, e),
            };
            out.map(|v| Value::int(v).with_labels(labels))
                .ok_or_else(|| raise_invalid(cx, "`math.pow` overflowed", labels))
        }
        (Repr::Float(_) | Repr::Int(_), Repr::Float(_) | Repr::Int(_)) => {
            let (a, b) = (float_arg_or_int(x), float_arg_or_int(y));
            Ok(float_val(fmath::pow(a, b), labels))
        }
        _ => Err(raise_invalid(cx, "`math.pow` is for Int and Float", labels)),
    }
}

fn float_arg_or_int(v: &Value) -> f64 {
    match v.repr {
        Repr::Int(i) => i as f64,
        Repr::Float(f) => f,
        _ => f64::NAN,
    }
}

// ---- statistics ----

/// The elements of `xs` as floats, in order, with the buffer charged to the heap first.
struct Nums {
    v: Vec<f64>,
    _charge: Charge,
}

fn gather(cx: &Cx<'_>, xs: &Value) -> Result<Nums, RtError> {
    let l = list(cx, xs)?;
    let charge = charged(cx, cost::SLOT.saturating_mul(l.len()))?;
    let mut v = Vec::with_capacity(usize::try_from(l.len()).unwrap_or(0));
    let mut bad = false;
    l.for_each(&mut |e| match num_f64(e) {
        Some(f) => v.push(f),
        None => bad = true,
    });
    work::add(l.len());
    if bad {
        return Err(raise_invalid(cx, "expected a list of numbers", xs.quick()));
    }
    Ok(Nums { v, _charge: charge })
}

/// Neumaier's compensated sum, in order: deterministic and robust to cancellation. A sum that
/// overflows is the plain sum (`inf`, or `NaN` for `inf - inf`): the compensation term of an
/// infinite partial sum is meaningless.
fn sum(v: &[f64]) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for &x in v {
        let t = s + x;
        if !t.is_finite() {
            return v.iter().fold(0.0, |a, b| a + b);
        }
        c += if s.abs() >= x.abs() {
            (s - t) + x
        } else {
            (x - t) + s
        };
        s = t;
    }
    s + c
}

/// The mean of `v` (not empty): the sum over the count, or, when the sum overflows although
/// every element is finite, the sum of the elements each over the count.
fn mean_of(v: &[f64]) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "a count of elements")]
    let n = v.len() as f64;
    let s = sum(v);
    if s.is_finite() || !v.iter().all(|x| x.is_finite()) {
        s / n
    } else {
        let scaled: Vec<f64> = v.iter().map(|x| x / n).collect();
        sum(&scaled)
    }
}

pub(super) fn mean(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let nums = gather(cx, xs)?;
    let labels = xs.quick();
    if nums.v.is_empty() {
        return Ok(Value::none().with_labels(labels));
    }
    Ok(float_val(mean_of(&nums.v), labels))
}

fn variance_of(v: &[f64], sample: bool) -> Option<f64> {
    let n = v.len();
    if n == 0 || (sample && n < 2) {
        return None;
    }
    let m = mean_of(v);
    let squares: Vec<f64> = v.iter().map(|x| (x - m) * (x - m)).collect();
    let denominator = if sample { n - 1 } else { n } as f64;
    Some(sum(&squares) / denominator)
}

fn spread(cx: &Cx<'_>, args: &Args, root: bool) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let sample = opt(args, 1);
    let labels = xs.quick() | sample.map_or(Labels::NONE, |s| s.labels);
    let sample = sample.and_then(Value::as_bool).unwrap_or(true);
    let nums = gather(cx, xs)?;
    // The squares are a second buffer of the same size.
    let _charge = charged(cx, cost::SLOT.saturating_mul(nums.v.len() as u64))?;
    work::add(nums.v.len() as u64);
    Ok(match variance_of(&nums.v, sample) {
        Some(var) => float_val(if root { var.sqrt() } else { var }, labels),
        None => Value::none().with_labels(labels),
    })
}

pub(super) fn variance(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    spread(cx, args, false)
}

pub(super) fn stdev(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    spread(cx, args, true)
}

/// Sorts floats with `NaN` last and `-0.0` before `0.0`: a total order, so an unstable sort
/// gives one result everywhere.
fn sort_floats(v: &mut [f64]) {
    for x in v.iter_mut() {
        *x = canon(*x);
    }
    v.sort_unstable_by(f64::total_cmp);
}

/// The work of sorting `n` elements.
fn sort_work(n: u64) -> u64 {
    n.saturating_mul(log_work(n).max(1))
}

pub(super) fn median(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let mut nums = gather(cx, xs)?;
    let labels = xs.quick();
    let n = nums.v.len();
    if n == 0 {
        return Ok(Value::none().with_labels(labels));
    }
    work::add(sort_work(n as u64));
    sort_floats(&mut nums.v);
    let m = if n % 2 == 1 {
        nums.v[n / 2]
    } else {
        f64::midpoint(nums.v[n / 2 - 1], nums.v[n / 2])
    };
    Ok(float_val(m, labels))
}

pub(super) fn percentile(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, p) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = xs.quick() | p.labels;
    let pv = float_arg(cx, p)?;
    if pv.is_nan() || !(0.0..=100.0).contains(&pv) {
        return Err(raise_invalid(
            cx,
            "`math.percentile` needs `p` in 0..100",
            labels,
        ));
    }
    let mut nums = gather(cx, xs)?;
    let n = nums.v.len();
    if n == 0 {
        return Ok(Value::none().with_labels(labels));
    }
    work::add(sort_work(n as u64));
    sort_floats(&mut nums.v);
    let rank = pv * (n - 1) as f64 / 100.0;
    let lo = rank.floor();
    let frac = rank - lo;
    let i = (lo as usize).min(n - 1);
    let j = (i + 1).min(n - 1);
    let (a, b) = (nums.v[i], nums.v[j]);
    let out = if frac == 0.0 || i == j {
        a
    } else {
        a + (b - a) * frac
    };
    Ok(float_val(out, labels))
}

pub(super) fn mode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let l = list(cx, xs)?;
    let labels = xs.quick();
    let n = l.len();
    if n == 0 {
        return Ok(Value::none().with_labels(labels));
    }
    // The elements and the index permutation are two buffers of `n` slots.
    let _charge = charged(cx, cost::SLOT.saturating_mul(n).saturating_mul(2))?;
    let items = l.to_vec();
    let mut order: Vec<u32> = (0..u32::try_from(items.len()).unwrap_or(u32::MAX)).collect();
    work::add(sort_work(n).saturating_mul(2));
    order.sort_unstable_by(|&a, &b| {
        work::add(items[a as usize].weight().min(items[b as usize].weight()));
        cmp_natural(&items[a as usize], &items[b as usize]).then(a.cmp(&b))
    });
    // Runs of equal values; the best run has the most members, then the earliest first member.
    let (mut best_count, mut best_first) = (0u64, u32::MAX);
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len()
            && cmp_natural(&items[order[start] as usize], &items[order[end] as usize]).is_eq()
        {
            end += 1;
        }
        let count = (end - start) as u64;
        let first = order[start]; // the sort is by index within a run
        if count > best_count || (count == best_count && first < best_first) {
            (best_count, best_first) = (count, first);
        }
        start = end;
    }
    Ok(items[best_first as usize].clone().with_labels(labels))
}

pub(super) fn softmax(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let t = opt(args, 1);
    let labels = xs.quick() | t.map_or(Labels::NONE, |t| t.labels);
    let temperature = match t {
        Some(t) => float_arg(cx, t)?,
        None => 1.0,
    };
    if temperature.is_nan() || temperature <= 0.0 {
        return Err(raise_invalid(
            cx,
            "`math.softmax` needs a temperature above 0",
            labels,
        ));
    }
    let nums = gather(cx, xs)?;
    let _charge = charged(cx, cost::SLOT.saturating_mul(nums.v.len() as u64))?;
    // Stable: subtract the largest value before exponentiating.
    let max = nums
        .v
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, |m, x| if x > m { x } else { m });
    let mut e: Vec<f64> = nums
        .v
        .iter()
        .map(|x| fmath::exp((x - max) / temperature))
        .collect();
    let total = sum(&e);
    for x in &mut e {
        *x /= total;
    }
    work::add(2 * e.len() as u64);
    let _items_charge = charged(cx, cost::SLOT.saturating_mul(e.len() as u64))?;
    let items = e.into_iter().map(|x| float_val(x, Labels::NONE)).collect();
    new_list(cx, items, labels)
}

/// The index of the largest (or smallest) element, the first on ties; `NaN` is skipped.
fn arg_extreme(cx: &Cx<'_>, args: &Args, largest: bool) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let l = list(cx, xs)?;
    let labels = xs.quick();
    let mut best: Option<(u64, Value)> = None;
    let mut i = 0u64;
    let mut bad = false;
    l.for_each(&mut |v| {
        match &v.repr {
            Repr::Float(f) if f.is_nan() => {}
            Repr::Int(_) | Repr::Float(_) | Repr::Duration(_) | Repr::Money(_) => {
                let better = best.as_ref().is_none_or(|(_, b)| {
                    let o = cmp_natural(v, b);
                    if largest { o.is_gt() } else { o.is_lt() }
                });
                if better {
                    best = Some((i, v.clone()));
                }
            }
            _ => bad = true,
        }
        i += 1;
    });
    work::add(l.len());
    if bad {
        return Err(raise_invalid(cx, "expected a list of numbers", labels));
    }
    Ok(match best {
        Some((i, _)) => Value::int(i64::try_from(i).unwrap_or(i64::MAX)).with_labels(labels),
        None => Value::none().with_labels(labels),
    })
}

pub(super) fn argmax(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    arg_extreme(cx, args, true)
}

pub(super) fn argmin(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    arg_extreme(cx, args, false)
}

pub(super) fn entropy(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let ps = arg(cx, args, 0)?;
    let labels = ps.quick();
    let nums = gather(cx, ps)?;
    if nums.v.iter().any(|p| *p < 0.0) {
        return Err(raise_invalid(
            cx,
            "`math.entropy` needs probabilities that are not negative",
            labels,
        ));
    }
    let total = sum(&nums.v);
    work::add(2 * nums.v.len() as u64);
    if nums.v.is_empty() || total == 0.0 {
        return Ok(float_val(0.0, labels));
    }
    let _terms_charge = charged(cx, cost::SLOT.saturating_mul(nums.v.len() as u64))?;
    let terms: Vec<f64> = nums
        .v
        .iter()
        .map(|p| {
            let q = p / total;
            if q > 0.0 { q * fmath::log2(q) } else { 0.0 }
        })
        .collect();
    // `-0.0` would print as -0: entropy is never negative.
    let h = -sum(&terms);
    Ok(float_val(if h == 0.0 { 0.0 } else { h }, labels))
}

/// The sizes the list statistics are charged on: the list's weight (its elements, one each).
pub(super) fn s_list(a: &Args) -> Sizes {
    s_weight_first(a)
}
