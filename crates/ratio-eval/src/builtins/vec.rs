//! `vec` (chapter 06, `data`): embedding vectors, `[Float]`.
//!
//! Sums run in order with plain `+` and `*` (no fused operations), so a result has the same bits
//! on every machine. Lengths and norms are computed on the vector **scaled by its largest
//! component**, so a vector of 1e200s has the norm it should instead of overflowing to
//! infinity, and one of 1e-200s is not mistaken for the zero vector.
//!
//! Mismatched dimensions raise `Invalid` before any work is done (a length is O(1)).
//! `cosine` of a zero vector is 0.0 (the angle is undefined; `top_k` ranks such a vector last
//! among finite scores). A `NaN` score sorts after every other and is written as the
//! canonical `NaN`. `dot`, `norm`, `cosine`, `add`, `normalize` and `mean` are `linear` in the
//! weight of their inputs; `top_k` is `nlogn` in the total weight of the query and the
//! vectors, which covers its `m · d` multiplications and its sort of `m` scores.
//!
//! A result carries the labels of everything that decided it: every component of both vectors,
//! the dimension, and for `top_k` the count `k`.

use std::cmp::Ordering;

use super::data3::{
    CHUNK, Ticker, charged, float_val, map_polled, poll, raise_invalid, sort_polled,
};
use super::{Args, Sizes, arg, int, list, new_list, opt, s_weight_first, weight};
use crate::error::RtError;
use crate::heap::{Charge, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::value::{Repr, Value};
use crate::work;

pub(super) fn s_weight_two_lists(a: &Args) -> Sizes {
    Sizes {
        n: weight(a, 0).saturating_add(weight(a, 1)),
        ..Sizes::default()
    }
}

/// The size of `top_k`: the query and every vector.
pub(super) fn s_top_k(a: &Args) -> Sizes {
    s_weight_two_lists(a)
}

/// A vector read into memory, its slots charged to the heap first.
struct Floats {
    v: Vec<f64>,
    _charge: Charge,
}

fn floats(cx: &Cx<'_>, x: &Value) -> Result<Floats, RtError> {
    let l = list(cx, x)?;
    let charge = charged(cx, cost::SLOT.saturating_mul(l.len()))?;
    let mut v = Vec::with_capacity(usize::try_from(l.len()).unwrap_or(0));
    let mut bad = false;
    let mut cur = l.cursor();
    let mut ticker = Ticker::default();
    while let Some(e) = cur.next() {
        match e.repr {
            Repr::Float(f) => v.push(f),
            _ => bad = true,
        }
        ticker.tick(cx, 1)?;
    }
    work::add(l.len());
    if bad {
        return Err(raise_invalid(cx, "a Vector holds Floats", x.quick()));
    }
    Ok(Floats { v, _charge: charge })
}

/// Two vectors of the same dimension, or `Invalid` (before reading either).
fn pair(cx: &Cx<'_>, a: &Value, b: &Value) -> Result<(Floats, Floats), RtError> {
    let (la, lb) = (list(cx, a)?, list(cx, b)?);
    if la.len() != lb.len() {
        return Err(raise_invalid(
            cx,
            &format!("vectors of {} and {} dimensions", la.len(), lb.len()),
            a.quick() | b.quick(),
        ));
    }
    Ok((floats(cx, a)?, floats(cx, b)?))
}

/// The length of a vector as `(m, s)` with `m · s` the length: the plain `(1, sqrt(Σ x²))`
/// when the sum of squares neither overflows nor underflows, else `m` the largest absolute
/// component and `s` the length of the vector scaled by `1/m`, so a vector of 1e200s has the
/// length it should and one of 1e-200s is not taken for the zero vector. The zero vector is
/// `(0, 0)`.
fn scaled(cx: &Cx<'_>, v: &[f64]) -> Result<(f64, f64), RtError> {
    let mut ss = 0.0f64;
    for chunk in v.chunks(CHUNK) {
        poll(cx)?;
        for x in chunk {
            ss += x * x;
        }
    }
    if ss.is_finite() && ss >= 1e-290 {
        return Ok((1.0, ss.sqrt()));
    }
    let mut m = 0.0f64;
    for chunk in v.chunks(CHUNK) {
        poll(cx)?;
        for x in chunk {
            let a = x.abs();
            if a.is_nan() {
                return Ok((f64::NAN, f64::NAN));
            }
            if a > m {
                m = a;
            }
        }
    }
    if m == 0.0 {
        return Ok((0.0, 0.0));
    }
    let mut ss = 0.0f64;
    for chunk in v.chunks(CHUNK) {
        poll(cx)?;
        for x in chunk {
            let y = x / m;
            ss += y * y;
        }
    }
    Ok((m, ss.sqrt()))
}

fn make(cx: &Cx<'_>, v: &[f64], labels: Labels) -> Result<Value, RtError> {
    // The list of values is built from a buffer of one slot per component.
    let _items = charged(cx, cost::SLOT.saturating_mul(v.len() as u64))?;
    let items = map_polled(cx, v, |x| float_val(*x, Labels::NONE))?;
    new_list(cx, items, labels)
}

pub(super) fn dot(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (x, y) = pair(cx, a, b)?;
    let mut sum = 0.0f64;
    for (cx_, cy_) in x.v.chunks(CHUNK).zip(y.v.chunks(CHUNK)) {
        poll(cx)?;
        for (p, q) in cx_.iter().zip(cy_) {
            sum += p * q;
        }
    }
    Ok(float_val(sum, a.quick() | b.quick()))
}

pub(super) fn norm(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let a = arg(cx, args, 0)?;
    let x = floats(cx, a)?;
    let (m, s) = scaled(cx, &x.v)?;
    work::add(2 * x.v.len() as u64);
    Ok(float_val(m * s, a.quick()))
}

pub(super) fn normalize(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let a = arg(cx, args, 0)?;
    let x = floats(cx, a)?;
    let (m, s) = scaled(cx, &x.v)?;
    let labels = a.quick();
    if m == 0.0 {
        return Err(raise_invalid(
            cx,
            "the zero vector cannot be normalized",
            labels,
        ));
    }
    work::add(2 * x.v.len() as u64);
    // x / (m · s) written as (x / m) / s: no overflow when m · s would.
    let out = charged(cx, cost::SLOT.saturating_mul(x.v.len() as u64))?;
    let v: Vec<f64> = map_polled(cx, &x.v, |c| (c / m) / s)?;
    let r = make(cx, &v, labels);
    drop(out);
    r
}

pub(super) fn cosine(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (x, y) = pair(cx, a, b)?;
    let labels = a.quick() | b.quick();
    work::add(6 * x.v.len() as u64);
    Ok(float_val(cosine_of(cx, &x.v, &y.v)?, labels))
}

/// The cosine of the angle between two vectors of one dimension, in `[-1, 1]`: `dot / sqrt(|x|² ·
/// |y|²)` when those are neither too large nor too small to be written, else computed on the
/// scaled vectors; 0.0 when either is the zero vector, `NaN` for a `NaN` component.
fn cosine_of(cx: &Cx<'_>, x: &[f64], y: &[f64]) -> Result<f64, RtError> {
    let (mut dot, mut xx, mut yy) = (0.0f64, 0.0f64, 0.0f64);
    for (cx_, cy_) in x.chunks(CHUNK).zip(y.chunks(CHUNK)) {
        poll(cx)?;
        for (p, q) in cx_.iter().zip(cy_) {
            dot += p * q;
            xx += p * p;
            yy += q * q;
        }
    }
    let both = xx * yy;
    if dot.is_finite() && both.is_finite() && both >= 1e-290 {
        return Ok((dot / both.sqrt()).clamp(-1.0, 1.0));
    }
    let ((ma, sa), (mb, sb)) = (scaled(cx, x)?, scaled(cx, y)?);
    if ma.is_nan() || mb.is_nan() {
        return Ok(f64::NAN);
    }
    if ma == 0.0 || mb == 0.0 {
        return Ok(0.0);
    }
    let mut sum = 0.0f64;
    for (cx_, cy_) in x.chunks(CHUNK).zip(y.chunks(CHUNK)) {
        poll(cx)?;
        for (p, q) in cx_.iter().zip(cy_) {
            sum += (p / ma) * (q / mb);
        }
    }
    Ok((sum / (sa * sb)).clamp(-1.0, 1.0))
}

pub(super) fn add(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (x, y) = pair(cx, a, b)?;
    let out = charged(cx, cost::SLOT.saturating_mul(x.v.len() as u64))?;
    let pairs: Vec<(f64, f64)> = x.v.iter().copied().zip(y.v.iter().copied()).collect();
    let v: Vec<f64> = map_polled(cx, &pairs, |(p, q)| p + q)?;
    let r = make(cx, &v, a.quick() | b.quick());
    drop(out);
    r
}

pub(super) fn mean(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let vs = arg(cx, args, 0)?;
    let l = list(cx, vs)?;
    let labels = vs.quick();
    if l.is_empty() {
        return Ok(Value::none().with_labels(labels));
    }
    let n = l.len();
    let first = l.get(0).cloned().unwrap_or_else(Value::unit);
    let dims = list(cx, &first)?.len();
    // The running sums: one component per dimension, charged before they are allocated.
    let _acc_charge = charged(cx, cost::SLOT.saturating_mul(dims))?;
    let mut acc = vec![0.0f64; usize::try_from(dims).unwrap_or(0)];
    let mut bad = None;
    let mut ticker = Ticker::default();
    let mut cur = l.cursor();
    while let Some(v) = cur.next() {
        match &v.repr {
            Repr::List(inner) if inner.len() == dims => {
                let mut ok = true;
                let mut inner_cur = inner.cursor();
                let mut i = 0;
                while let Some(e) = inner_cur.next() {
                    match e.repr {
                        Repr::Float(f) => acc[i] += f,
                        _ => ok = false,
                    }
                    i += 1;
                    ticker.tick(cx, 1)?;
                }
                poll(cx)?;
                work::add(dims);
                if !ok {
                    bad = Some("a Vector holds Floats");
                    break;
                }
            }
            Repr::List(_) => {
                bad = Some("vectors of different dimensions");
                break;
            }
            _ => {
                bad = Some("expected a list of vectors");
                break;
            }
        }
    }
    if let Some(why) = bad {
        return Err(raise_invalid(cx, why, labels));
    }
    #[allow(clippy::cast_precision_loss, reason = "a count of vectors")]
    let count = n as f64;
    for chunk in acc.chunks_mut(CHUNK) {
        poll(cx)?;
        for x in chunk {
            *x /= count;
        }
    }
    make(cx, &acc, labels)
}

/// Best score first, `NaN` last, ties by index.
fn rank(a: &(f64, u32), b: &(f64, u32)) -> Ordering {
    match (a.0.is_nan(), b.0.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal),
    }
    .then(a.1.cmp(&b.1))
}

pub(super) fn top_k(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (query, vs) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let k = opt(args, 2);
    let labels = query.quick() | vs.quick() | k.map_or(Labels::NONE, |k| k.labels);
    let want = match k {
        Some(k) => int(cx, k)?,
        None => 5,
    };
    let Ok(want) = u64::try_from(want) else {
        return Err(raise_invalid(
            cx,
            "`vec.top_k` needs `k` of 0 or more",
            labels,
        ));
    };
    let q = floats(cx, query)?;
    let l = list(cx, vs)?;
    // One (score, index) pair per vector.
    let _scores_charge = charged(cx, cost::SLOT.saturating_mul(l.len()))?;
    let mut scores: Vec<(f64, u32)> = Vec::with_capacity(usize::try_from(l.len()).unwrap_or(0));
    let mut cur = l.cursor();
    let mut index = 0u32;
    while let Some(v) = cur.next() {
        poll(cx)?;
        let Repr::List(inner) = &v.repr else {
            return Err(raise_invalid(cx, "expected a list of vectors", labels));
        };
        if inner.len() != q.v.len() as u64 {
            return Err(raise_invalid(
                cx,
                &format!(
                    "vector {index} has {} dimensions, the query {}",
                    inner.len(),
                    q.v.len()
                ),
                labels,
            ));
        }
        let x = floats(cx, &v)?;
        work::add(6 * x.v.len() as u64);
        scores.push((cosine_of(cx, &q.v, &x.v)?, index));
        index = index.saturating_add(1);
    }
    work::add(
        (scores.len() as u64).saturating_mul(crate::ops::log_work(scores.len() as u64).max(1)),
    );
    sort_polled(cx, &mut scores, rank)?;
    let take = usize::try_from(want)
        .unwrap_or(usize::MAX)
        .min(scores.len());
    let _out_charge = charged(cx, cost::SLOT.saturating_mul(take as u64))?;
    let mut out = Vec::with_capacity(take);
    for (score, i) in scores.into_iter().take(take) {
        out.push(
            Value::new_tuple(
                cx.heap,
                vec![Value::int(i64::from(i)), float_val(score, Labels::NONE)],
            )
            .map_err(|f| cx.fault(f, labels))?,
        );
    }
    new_list(cx, out, labels)
}

/// The size `norm` and `normalize` are charged on.
pub(super) fn s_vec(a: &Args) -> Sizes {
    s_weight_first(a)
}
