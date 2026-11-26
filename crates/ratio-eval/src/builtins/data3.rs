//! Helpers shared by the stage 9 built-ins (`math`, `hash`, `diff`, `template`, `vec`, `path`,
//! `url`, `codec`): argument reading, refusals, heap-charged buffers and the text builder.
//!
//! The rules every one of those built-ins follows, and these helpers make easy to follow:
//!
//! * A refusal is `Invalid`, raised before anything proportional to the input is allocated.
//! * A buffer proportional to the input is charged to the heap ([`charged`]) when it is
//!   allocated, and given back when it is dropped.
//! * Text that is built piece by piece goes through a [`TextBuf`]: it stops at 16 MiB, charges
//!   what it has written to the heap as it grows, and a refusal pays the steps of what it wrote
//!   (`Cx::debit`) as a returned result would.
//! * Checked arithmetic on every size that comes from an argument.

use std::sync::Arc;

use crate::error::{Kind, RtError, raise};
use crate::heap::{Charge, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::value::{Fault, Layout, MAX_STR_BYTES, Value};
use crate::work;

/// The canonical quiet `NaN`: the hardware's choice of sign and payload differs, so every
/// `Float` a stage 9 built-in returns has this one. Decided on the bits, not with a float
/// comparison: the optimiser may treat every `NaN` as the same and drop a float-level test.
pub(super) fn canon(f: f64) -> f64 {
    let bits = f.to_bits();
    if bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0 {
        f64::from_bits(0x7ff8_0000_0000_0000)
    } else {
        f
    }
}

/// A `Float` result with these labels.
pub(super) fn float_val(f: f64, labels: Labels) -> Value {
    Value::float(canon(f)).with_labels(labels)
}

/// An `Invalid` error whose message describes data with these labels.
pub(super) fn raise_invalid(cx: &Cx<'_>, message: &str, labels: Labels) -> RtError {
    raise(cx.p, Kind::Invalid, message, labels, cx.loc)
}

/// A `Float` argument.
pub(super) fn float_arg(cx: &Cx<'_>, v: &Value) -> Result<f64, RtError> {
    v.as_float()
        .ok_or_else(|| raise_invalid(cx, "expected a Float", v.labels))
}

/// A `Bool` argument.
pub(super) fn bool_arg(cx: &Cx<'_>, v: &Value) -> Result<bool, RtError> {
    v.as_bool()
        .ok_or_else(|| raise_invalid(cx, "expected a Bool", v.labels))
}

/// Charges `bytes` to the heap: the buffer is allocated after this returns, and the returned
/// charge is held for as long as the buffer lives.
pub(super) fn charged(cx: &Cx<'_>, bytes: u64) -> Result<Charge, RtError> {
    let mut c = Charge::none();
    c.grow(cx.heap, bytes)
        .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
    Ok(c)
}

/// A record with these fields, which must be given in name order, built on the heap.
pub(super) fn record(
    cx: &Cx<'_>,
    layout: &Arc<Layout>,
    items: Vec<Value>,
    labels: Labels,
) -> Result<Value, RtError> {
    Value::new_record(cx.heap, Arc::clone(layout), items)
        .map(|v| v.with_labels(labels))
        .map_err(|f| cx.fault(f, labels))
}

/// The layout of a record with these field names, in name order.
pub(super) fn layout(names: &[&str]) -> Arc<Layout> {
    debug_assert!(names.windows(2).all(|w| w[0] < w[1]), "names in order");
    Arc::new(Layout {
        names: names.iter().map(|n| Arc::from(*n)).collect(),
    })
}

/// How much a [`TextBuf`] charges at a time.
const BLOCK: u64 = 64 * 1024;

/// Text being built piece by piece, never longer than 16 MiB, its bytes charged to the heap
/// as they are written (in blocks of 64 KiB).
#[derive(Debug)]
pub(super) struct TextBuf {
    s: String,
    charge: Charge,
    charged: u64,
    labels: Labels,
}

impl TextBuf {
    /// An empty buffer; a refusal's message carries `labels`.
    pub(super) fn new(labels: Labels) -> TextBuf {
        TextBuf {
            s: String::new(),
            charge: Charge::none(),
            charged: 0,
            labels,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.s.len()
    }

    /// Makes room for `more` bytes: refuses a text over 16 MiB (paying for what was written)
    /// or one the heap cannot hold.
    fn room(&mut self, cx: &mut Cx<'_>, more: usize) -> Result<(), RtError> {
        let total = self
            .s
            .len()
            .checked_add(more)
            .filter(|t| *t <= MAX_STR_BYTES);
        let Some(total) = total else {
            cx.debit = cx.debit.max((self.s.len() as u64).div_ceil(64));
            return Err(too_long(cx, self.labels));
        };
        let total = total as u64;
        if total > self.charged {
            let want = total.div_ceil(BLOCK) * BLOCK;
            self.charge
                .grow(cx.heap, want - self.charged)
                .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
            self.charged = want;
        }
        Ok(())
    }

    pub(super) fn push_str(&mut self, cx: &mut Cx<'_>, t: &str) -> Result<(), RtError> {
        self.room(cx, t.len())?;
        self.s.push_str(t);
        work::add(t.len() as u64);
        Ok(())
    }

    pub(super) fn push(&mut self, cx: &mut Cx<'_>, c: char) -> Result<(), RtError> {
        self.room(cx, c.len_utf8())?;
        self.s.push(c);
        work::add(1);
        Ok(())
    }

    /// The text as a value on the heap, with the buffer's charge given back.
    pub(super) fn finish(self, cx: &Cx<'_>) -> Result<Value, RtError> {
        let TextBuf {
            s, charge, labels, ..
        } = self;
        let v = Value::new_text(cx.heap, s)
            .map(|v| v.with_labels(labels))
            .map_err(|f| cx.fault(f, labels));
        drop(charge);
        v
    }
}

/// Text built in one go (its size is known): charged to the heap before it is made.
pub(super) fn text_value(cx: &Cx<'_>, s: String, labels: Labels) -> Result<Value, RtError> {
    super::new_text(cx, s, labels)
}

/// `Cancelled`, for a long built-in that finds the flag set.
pub(super) fn cancelled(cx: &Cx<'_>) -> RtError {
    raise(
        cx.p,
        Kind::Cancelled,
        "the evaluation was cancelled",
        Labels::NONE,
        cx.loc,
    )
}

/// The refusal of a text over 16 MiB.
pub(super) fn too_long(cx: &Cx<'_>, labels: Labels) -> RtError {
    raise_invalid(cx, "the text is longer than 16 MiB", labels)
}

/// A text copied from a slice of an argument: the copy is charged to the heap before it is
/// made, so a refusal comes before the allocation, not after.
pub(super) fn text_of(cx: &Cx<'_>, s: &str, labels: Labels) -> Result<Value, RtError> {
    let _copy = charged(cx, s.len() as u64)?;
    super::new_text(cx, s.to_owned(), labels)
}

// ---- cancellation ----

/// Units of work between two looks at the cancellation flag (as stages 7 and 8 do).
pub(super) const CHUNK: usize = 64_000;

/// `Cancelled` if the evaluation was cancelled.
pub(super) fn poll(cx: &Cx<'_>) -> Result<(), RtError> {
    if cx.cancel.is_cancelled() {
        Err(cancelled(cx))
    } else {
        Ok(())
    }
}

/// Counts work and looks at the cancellation flag every [`CHUNK`] units.
#[derive(Default)]
pub(super) struct Ticker(usize);

impl Ticker {
    pub(super) fn tick(&mut self, cx: &Cx<'_>, units: usize) -> Result<(), RtError> {
        self.0 = self.0.saturating_add(units);
        if self.0 >= CHUNK {
            self.0 = 0;
            poll(cx)?;
        }
        Ok(())
    }
}

/// `v` mapped by `f`, looking at the cancellation flag between chunks.
pub(super) fn map_polled<T, U>(
    cx: &Cx<'_>,
    v: &[T],
    f: impl Fn(&T) -> U,
) -> Result<Vec<U>, RtError> {
    let mut out = Vec::with_capacity(v.len());
    for chunk in v.chunks(CHUNK) {
        poll(cx)?;
        out.extend(chunk.iter().map(&f));
    }
    Ok(out)
}

/// A stable merge sort that looks at the cancellation flag every [`CHUNK`] elements: runs of
/// 256 are sorted in place, then merged pairwise through a scratch copy (charged to the heap
/// first), so no single step is long.
pub(super) fn sort_polled<T: Copy>(
    cx: &Cx<'_>,
    v: &mut [T],
    cmp: impl Fn(&T, &T) -> std::cmp::Ordering,
) -> Result<(), RtError> {
    const RUN: usize = 256;
    let n = v.len();
    let mut ticker = Ticker::default();
    for run in v.chunks_mut(RUN) {
        run.sort_by(&cmp);
        work::add(run.len() as u64 * 8);
        ticker.tick(cx, run.len() * 8)?;
    }
    if n <= RUN {
        return Ok(());
    }
    let _scratch = charged(cx, cost::SLOT.saturating_mul(n as u64))?;
    let mut other: Vec<T> = v.to_vec();
    let mut in_v = true;
    let mut width = RUN;
    while width < n {
        let (from, to): (&[T], &mut [T]) = if in_v {
            (&*v, other.as_mut_slice())
        } else {
            (other.as_slice(), &mut *v)
        };
        let mut start = 0;
        while start < n {
            let mid = (start + width).min(n);
            let end = (start + 2 * width).min(n);
            let (mut i, mut j, mut k) = (start, mid, start);
            while i < mid && j < end {
                if cmp(&from[j], &from[i]).is_lt() {
                    to[k] = from[j];
                    j += 1;
                } else {
                    to[k] = from[i];
                    i += 1;
                }
                k += 1;
                ticker.tick(cx, 1)?;
            }
            while i < mid {
                to[k] = from[i];
                i += 1;
                k += 1;
            }
            while j < end {
                to[k] = from[j];
                j += 1;
                k += 1;
            }
            ticker.tick(cx, end - start)?;
            work::add((end - start) as u64);
            start = end;
        }
        in_v = !in_v;
        width *= 2;
    }
    if !in_v {
        v.copy_from_slice(&other);
    }
    Ok(())
}
