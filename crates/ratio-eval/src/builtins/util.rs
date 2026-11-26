//! What the stage 8 built-ins (`json`, `list`, `map`) share: a heap-charged result buffer with
//! the 1,000,000-element limit, a pacer that checks cancellation as a long loop works, a stable
//! merge sort that cannot panic on an odd order, and index arithmetic.

use std::cmp::Ordering;

use crate::error::{Kind, RtError, raise};
use crate::heap::{Charge, cost};
use crate::labels::Labels;
use crate::machine::{CHECKPOINT, Cx};
use crate::plist::List;
use crate::value::{Fault, MAX_ELEMS, Value};

/// The work between cancellation checks: one checkpoint's worth (64 units a step).
const PACE: u64 = 64 * CHECKPOINT;

/// Counts the work a long loop does (debug-build counters) and checks for cancellation every
/// checkpoint's worth of it, so a cancelled run stops within one checkpoint interval as pure
/// code does.
#[derive(Debug, Default)]
pub(super) struct Pace(u64);

impl Pace {
    pub(super) fn new() -> Pace {
        Pace(0)
    }

    /// Adds `units` of work.
    ///
    /// # Errors
    ///
    /// `Cancelled`, when the evaluation was cancelled.
    pub(super) fn tick(&mut self, cx: &Cx<'_>, units: u64) -> Result<(), RtError> {
        crate::work::add(units);
        self.0 = self.0.saturating_add(units);
        if self.0 >= PACE {
            self.0 = 0;
            if cx.cancel.is_cancelled() {
                return Err(raise(
                    cx.p,
                    Kind::Cancelled,
                    "the evaluation was cancelled",
                    Labels::NONE,
                    cx.loc,
                ));
            }
        }
        Ok(())
    }
}

/// A heap error as the machine reports it.
pub(super) fn heap_error(cx: &Cx<'_>) -> RtError {
    cx.fault(Fault::Heap, Labels::NONE)
}

/// The refusal of a list or map of more than [`MAX_ELEMS`] elements.
pub(super) fn too_many(cx: &Cx<'_>, labels: Labels) -> RtError {
    cx.fault(
        Fault::TooLarge("a list of more than 1,000,000 elements"),
        labels,
    )
}

/// A result being gathered: one heap slot per element, charged as the element arrives (or all
/// at once by [`Gather::with_capacity`], before the buffer is allocated), given back when the
/// buffer is dropped. At most [`MAX_ELEMS`] elements.
#[derive(Debug)]
pub(super) struct Gather {
    items: Vec<Value>,
    charge: Charge,
    paid: u64,
    limit: u64,
}

impl Gather {
    pub(super) fn new() -> Gather {
        Gather {
            items: Vec::new(),
            charge: Charge::none(),
            paid: 0,
            limit: MAX_ELEMS,
        }
    }

    /// A buffer of key and value slots, alternating: up to [`MAX_ELEMS`] entries.
    pub(super) fn pairs() -> Gather {
        Gather {
            limit: 2 * MAX_ELEMS,
            ..Gather::new()
        }
    }

    /// Adds an entry to a buffer made by [`Gather::pairs`].
    pub(super) fn push_pair(&mut self, cx: &Cx<'_>, k: Value, v: Value) -> Result<(), RtError> {
        self.push(cx, k)?;
        self.push(cx, v)
    }

    /// The entries of a [`Gather::pairs`] buffer, in a sorted map (keys already in strictly
    /// increasing order), whose own labels are `own`.
    pub(super) fn into_map(self, cx: &Cx<'_>, own: Labels) -> Result<Value, RtError> {
        let Gather { items, charge, .. } = self;
        let n = items.len() / 2;
        let mut it = items.into_iter();
        let mut pairs = std::iter::from_fn(|| Some((it.next()?, it.next()?)));
        let map =
            crate::pmap::Map::from_sorted(cx.heap, n, &mut pairs).map_err(|f| cx.fault(f, own))?;
        drop(charge);
        Ok(Value::new_map(map).with_labels(own))
    }

    /// A buffer for `n` elements, charged before it is allocated.
    ///
    /// # Errors
    ///
    /// Over the element limit or the heap.
    pub(super) fn with_capacity(cx: &Cx<'_>, n: u64, labels: Labels) -> Result<Gather, RtError> {
        if n > MAX_ELEMS {
            return Err(too_many(cx, labels));
        }
        let mut g = Gather::new();
        g.charge
            .grow(cx.heap, cost::SLOT.saturating_mul(n))
            .map_err(|_| heap_error(cx))?;
        g.paid = n;
        g.items
            .reserve_exact(usize::try_from(n).unwrap_or(usize::MAX));
        Ok(g)
    }

    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    pub(super) fn push(&mut self, cx: &Cx<'_>, v: Value) -> Result<(), RtError> {
        let len = self.items.len() as u64;
        if len >= self.limit {
            return Err(too_many(cx, Labels::NONE));
        }
        if len >= self.paid {
            self.charge
                .grow(cx.heap, cost::SLOT)
                .map_err(|_| heap_error(cx))?;
            self.paid = len + 1;
        }
        self.items.push(v);
        Ok(())
    }

    /// Reverses the elements gathered.
    pub(super) fn reverse(&mut self) {
        self.items.reverse();
    }

    pub(super) fn items(&self) -> &[Value] {
        &self.items
    }

    /// The elements as a list whose own labels are `own`.
    ///
    /// # Errors
    ///
    /// Over the heap or depth limits.
    pub(super) fn finish(self, cx: &Cx<'_>, own: Labels) -> Result<Value, RtError> {
        let Gather { items, charge, .. } = self;
        let list = List::from_vec(cx.heap, items).map_err(|f| cx.fault(f, own))?;
        drop(charge);
        Ok(Value::new_list(list).with_labels(own))
    }
}

/// A stable order of `0..n` and the heap charge that pays for it.
#[derive(Debug)]
pub(super) struct Order {
    pub(super) indexes: Vec<usize>,
    _charge: Charge,
}

/// Sorts `0..n` stably by `cmp`: a bottom-up merge sort that takes the right run's element
/// only when it is strictly smaller, so it never panics however odd `cmp` is (an `Int` against
/// a `Float` beyond 2^53 is not transitive), and counts its work. Two index buffers, charged
/// to the heap first.
///
/// # Errors
///
/// Over the heap, or cancelled.
pub(super) fn merge_sort(
    cx: &Cx<'_>,
    n: usize,
    cmp: &mut dyn FnMut(usize, usize) -> Ordering,
) -> Result<Order, RtError> {
    let mut charge = Charge::none();
    charge
        .grow(cx.heap, cost::SLOT.saturating_mul(2 * n as u64))
        .map_err(|_| heap_error(cx))?;
    let mut a: Vec<usize> = (0..n).collect();
    let mut b: Vec<usize> = vec![0; n];
    let mut pace = Pace::new();
    let mut width = 1usize;
    while width < n {
        let mut lo = 0usize;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut i, mut j, mut k) = (lo, mid, lo);
            while i < mid && j < hi {
                pace.tick(cx, 1)?;
                if cmp(a[j], a[i]) == Ordering::Less {
                    b[k] = a[j];
                    j += 1;
                } else {
                    b[k] = a[i];
                    i += 1;
                }
                k += 1;
            }
            while i < mid {
                b[k] = a[i];
                i += 1;
                k += 1;
            }
            while j < hi {
                b[k] = a[j];
                j += 1;
                k += 1;
            }
            lo = hi;
        }
        std::mem::swap(&mut a, &mut b);
        width = width.saturating_mul(2);
    }
    Ok(Order {
        indexes: a,
        _charge: charge,
    })
}

/// Resolves a position that may count from the end (`-1` is the last) against a length,
/// clamped to `0..=len`.
pub(super) fn clamp_index(i: i64, len: u64) -> u64 {
    if i < 0 {
        len.saturating_sub(i.unsigned_abs())
    } else {
        u64::try_from(i).unwrap_or(u64::MAX).min(len)
    }
}
