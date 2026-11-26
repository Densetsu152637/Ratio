//! Heap accounting (chapter 07, "Resource limits": live values, 64 MiB by default).
//!
//! Every allocation a run makes (a string, a list node, a record, a closure) is charged to
//! the run's [`HeapLedger`] before it is made, and credited back when it is freed: the
//! allocation holds a [`Charge`] whose drop gives the bytes back. So the ledger counts live
//! values, a loop that builds and drops values does not run out, and an allocation that would
//! take the live total over the limit is refused (`Budget { which: heap }`) before it is made.
//!
//! Charges are fixed per kind of allocation ([`cost`]), never `size_of`, so the same program
//! is charged the same on every platform. Values a host builds (arguments, test data) and the
//! program's literals use an unmetered [`Heap`] and are not charged.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The fixed charges, in bytes.
pub(crate) mod cost {
    /// A value slot in a container.
    pub const SLOT: u64 = 24;
    /// A string, plus its UTF-8 bytes.
    pub const STR: u64 = 32;
    /// A tuple, record, variant payload, closure or prompt, plus its slots.
    pub const AGG: u64 = 48;
    /// A list leaf, plus its slots, or a list branch node.
    pub const LIST_NODE: u64 = 64;
    /// A map node (one entry).
    pub const MAP_NODE: u64 = 96;
    /// A JSON node.
    pub const JSON: u64 = 48;
    /// A range.
    pub const RANGE: u64 = 32;
}

/// A run's live-heap ledger.
#[derive(Debug)]
pub struct HeapLedger {
    live: AtomicU64,
    peak: AtomicU64,
    limit: u64,
}

impl HeapLedger {
    /// The bytes live now.
    #[must_use]
    pub fn live(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }

    /// The most bytes live at once so far.
    #[must_use]
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }
}

/// Where allocations are charged: a run's ledger, or nowhere (host-built values).
#[derive(Clone, Debug, Default)]
pub struct Heap(Option<Arc<HeapLedger>>);

/// The heap limit was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapExhausted;

impl Heap {
    /// A heap that charges nothing.
    #[must_use]
    pub fn unmetered() -> Heap {
        Heap(None)
    }

    /// A new ledger with this limit.
    #[must_use]
    pub fn with_limit(limit: u64) -> Heap {
        Heap(Some(Arc::new(HeapLedger {
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            limit,
        })))
    }

    /// The ledger, if metered.
    #[must_use]
    pub fn ledger(&self) -> Option<&HeapLedger> {
        self.0.as_deref()
    }

    /// Charges `bytes`, or refuses when the live total would pass the limit.
    ///
    /// # Errors
    ///
    /// [`HeapExhausted`] when the limit would be passed; nothing is charged then.
    pub fn alloc(&self, bytes: u64) -> Result<Charge, HeapExhausted> {
        let Some(ledger) = &self.0 else {
            return Ok(Charge {
                ledger: None,
                bytes: 0,
            });
        };
        let mut current = ledger.live.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(bytes);
            if next > ledger.limit {
                return Err(HeapExhausted);
            }
            match ledger.live.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    ledger.peak.fetch_max(next, Ordering::Relaxed);
                    return Ok(Charge {
                        ledger: Some(Arc::clone(ledger)),
                        bytes,
                    });
                }
                Err(seen) => current = seen,
            }
        }
    }
}

/// Bytes charged to a ledger, given back when dropped. Every metered allocation holds one.
#[derive(Debug)]
pub struct Charge {
    ledger: Option<Arc<HeapLedger>>,
    bytes: u64,
}

impl Charge {
    /// A charge of nothing (for values built outside a run).
    pub(crate) const fn none() -> Charge {
        Charge {
            ledger: None,
            bytes: 0,
        }
    }

    /// Charges `bytes` more to `heap`, held (and given back) with this charge: for buffers that
    /// grow, such as a built-in's result being gathered.
    pub(crate) fn grow(&mut self, heap: &Heap, bytes: u64) -> Result<(), HeapExhausted> {
        let mut more = heap.alloc(bytes)?;
        if self.ledger.is_none() {
            self.ledger = more.ledger.take();
        }
        if self.ledger.is_some() {
            self.bytes += std::mem::take(&mut more.bytes);
        }
        Ok(())
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        if let Some(ledger) = &self.ledger {
            ledger.live.fetch_sub(self.bytes, Ordering::Relaxed);
        }
    }
}
