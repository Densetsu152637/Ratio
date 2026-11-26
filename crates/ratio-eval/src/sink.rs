//! The sink check (chapter 03, "Secrecy"; chapter 06, `data`: label summaries).
//!
//! Stage 6 has no sinks of its own (they are effects: stage 11's host and the guard the
//! interpreter wraps around it); this is the check that guard runs on every value crossing to
//! a sink. A value whose own labels and summary are clear passes in O(1); otherwise its leaves
//! are scanned, and it is refused when any is labelled. Debug builds scan every value and
//! assert that the scan agrees with the summary: no labelled leaf under a clear bit.

use crate::labels::Labels;
use crate::value::Value;

/// Checks `value` for a sink that refuses labelled values: `Err` with the labels found.
///
/// # Errors
///
/// When some part of the value is labelled.
///
/// # Panics
///
/// In debug builds, when the label summary misses a labelled leaf (an interpreter bug).
pub fn check(value: &Value) -> Result<(), Labels> {
    let quick = value.quick();
    if cfg!(debug_assertions) {
        let scanned = value.scan();
        assert!(
            quick.contains(scanned),
            "label summary {quick:?} misses labels {scanned:?}: a propagation bug"
        );
    }
    if quick.is_empty() {
        return Ok(());
    }
    let scanned = value.scan();
    if scanned.is_empty() {
        Ok(())
    } else {
        Err(scanned)
    }
}
