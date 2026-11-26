//! Ratio's core interpreter: stage 6 of the implementation plan (chapter 10), the second half.
//!
//! [`prepare`] turns a lowered [`ratio_ir::Program`] into its [`Prepared`] form, and
//! [`evaluate`] runs an entry of it as a [`Future`](std::future::Future) ([`Evaluation`]):
//! the pure language, with run-time **secrecy labels** on every value and their propagation
//! through every operator and through `Json` ([`Value`], [`Labels`]), label summaries
//! ([`sink::check`]), persistent lists and maps ([`List`], [`Map`]), steps, heap and
//! call-depth accounting ([`Limits`]), cost-class charging of built-ins ([`builtins`]), a
//! yield at every 1,000-step checkpoint, cancellation ([`CancelToken`]), and the error rules of
//! chapter 03 (`Budget` and `Cancelled` cannot be caught; `defer` runs on every exit). Effects
//! are explicit suspensions of the [`Machine`] ([`Suspend::Effect`]), which stage 11's host
//! answers.

#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    reason = "interpreter code names values, patterns and nodes briefly (v, p, n, k)"
)]
#![allow(
    clippy::match_same_arms,
    reason = "the evaluator keeps one arm per kind of node or value, even where two agree"
)]
#![allow(
    clippy::too_many_lines,
    reason = "dispatch over every kind of node reads best as one match"
)]

pub mod builtins;
mod driver;
mod drop;
mod error;
mod heap;
mod json;
mod labels;
mod machine;
mod ops;
mod plist;
mod pmap;
mod pool;
mod prepare;
#[cfg(test)]
mod properties;

mod re;
pub mod sink;
mod stack;
mod value;

use std::sync::Arc;

pub use driver::{Evaluation, Finished, block_on};
pub use error::{Class as ErrorClass, RtError};
pub use heap::{Heap, HeapLedger};
pub use json::MAX_JSON_DEPTH;
pub use labels::Labels;
pub use machine::{
    BuiltinWork, CHECKPOINT, CancelToken, EffectRequest, Limits, Machine, Outcome, StartError,
    Suspend,
};
pub use plist::List;
pub use pmap::Map;
pub use pool::{
    JOBS_IN_FLIGHT, OFFLOAD_ELEMENTS, OFFLOAD_TEXT_BYTES, Pool, PoolSession, Thresholds, Waiter,
};
pub use prepare::{Entry, PrepareError, Prepared, prepare};
pub use value::{Fault, MAX_DEPTH, MAX_ELEMS, MAX_STR_BYTES, Str, Value};

/// Starts an evaluation of `entry` with `args`.
///
/// # Errors
///
/// When `entry` is not a function the core interpreter can run, or the arguments do not fit.
pub fn evaluate(
    prepared: Arc<Prepared>,
    entry: Entry,
    args: Vec<Value>,
    limits: Limits,
    cancel: CancelToken,
) -> Result<Evaluation, StartError> {
    Machine::new(prepared, entry, args, limits, cancel).map(Evaluation::new)
}

/// The value as compact JSON, without its type (as the conformance runner prints results):
/// records and maps as objects, lists and tuples as arrays, variants by `snake_case` name.
#[must_use]
pub fn to_json_text(prepared: &Prepared, value: &Value) -> String {
    let mut out = String::new();
    json::write(prepared, value, &mut out);
    out
}
