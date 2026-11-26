//! Debug-build work counters (chapter 10, stage 7: "debug-build work counters stay within a
//! fixed multiple of each built-in's charge").
//!
//! The persistent structures, the comparisons and the built-ins' own loops add the units of
//! work they do (an element visited, a node built, a character compared) to a per-thread
//! counter. Tests reset it, run a built-in, and compare the total with 64 times the steps the
//! built-in was charged (a step pays for 64 units, chapter 06, `data`). Release builds count
//! nothing: [`add`] compiles to nothing and [`total`] is 0.

use std::cell::Cell;

thread_local! {
    static WORK: Cell<u64> = const { Cell::new(0) };
}

/// Whether this build counts work (debug builds).
pub const ENABLED: bool = cfg!(debug_assertions);

/// Adds `n` units of work.
#[inline]
pub(crate) fn add(n: u64) {
    if ENABLED {
        WORK.with(|w| w.set(w.get().saturating_add(n)));
    }
}

/// Sets the calling thread's counter to 0.
pub fn reset() {
    WORK.with(|w| w.set(0));
}

/// The calling thread's counter.
#[must_use]
pub fn total() -> u64 {
    WORK.with(Cell::get)
}
