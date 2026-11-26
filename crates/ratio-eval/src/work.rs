//! Debug-build work counters (chapter 10, stage 7: "debug-build work counters stay within a
//! fixed multiple of each built-in's charge").
//!
//! The persistent structures, the comparisons and the built-ins' own loops add the units of
//! work they do (an element visited, a node built, a character compared) to a per-thread
//! counter. The machine resets it before each pure built-in call and reads it after, on the
//! thread that ran the call (inline or pooled), adding it to the body's own `Cx::work`; the
//! total goes to `Machine::work_log`, where tests compare it with 64 times the steps the call
//! was charged (a step pays for 64 units, chapter 06, `data`). A body that runs part of its
//! work on another thread adds that thread's [`total`] back with [`add`]. Release builds count
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

// ---- strings: work grows with the length compared, hashed or copied ----

/// The units comparing two texts for equality costs: one, and their length when the lengths
/// agree (when they differ the comparison stops at once).
#[must_use]
pub fn eq_cost(a: &str, b: &str) -> u64 {
    1 + if a.len() == b.len() {
        a.len() as u64
    } else {
        0
    }
}

/// The units comparing two texts in order costs: one plus the shorter's length.
#[must_use]
pub fn cmp_cost(a: &str, b: &str) -> u64 {
    1 + a.len().min(b.len()) as u64
}

/// The units hashing (or copying, or escaping) a text costs: one plus its length.
#[must_use]
pub fn len_cost(s: &str) -> u64 {
    1 + s.len() as u64
}

/// `a == b`, counted.
pub(crate) fn str_eq(a: &str, b: &str) -> bool {
    add(eq_cost(a, b));
    a == b
}

/// `a.cmp(b)`, counted.
pub(crate) fn str_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    add(cmp_cost(a, b));
    a.cmp(b)
}

/// Whether `key` (a text value) is `name`, counted.
pub(crate) fn key_is(key: &crate::value::Value, name: &str) -> bool {
    key.as_text().is_some_and(|k| str_eq(k, name))
}

/// Counts hashing the keys of `entries` into a map and looking each up once.
pub(crate) fn hashed(entries: &[(crate::value::Value, crate::value::Value)]) {
    for (k, _) in entries {
        add(2 * len_cost(k.as_text().unwrap_or("")));
    }
}
