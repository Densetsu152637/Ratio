//! The big stack the checker runs on, as the syntax crate's passes do (`ratio_syntax`'s
//! `stack` module): the checker walks trees recursively, and the parser bounds their depth, so
//! a thread with a large stack bounds the stack for that depth with a wide margin.
//!
//! If no such thread can be started, the check runs on the caller's stack and is told so; it
//! then refuses expression nesting beyond [`FALLBACK_DEPTH`] (as `C0022`), like the syntax
//! crate's fallback.

use std::sync::Mutex;

/// The stack of the thread the checker runs on.
const BIG_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The nesting of expressions, patterns and types the checker follows on the big stack. The
/// parser refuses trees deeper than its own limit (at most 256), and the checker's recursion
/// costs a bounded number of frames per level; this is a second, independent bound.
pub(crate) const MAX_DEPTH: usize = 512;

/// The nesting the checker allows on the caller's stack.
pub(crate) const FALLBACK_DEPTH: usize = 12;

/// Runs `work` on a thread with a large stack, passing the depth it may recurse to; if no
/// thread can be started, runs it on this thread with [`FALLBACK_DEPTH`]. A panic in `work` (a
/// bug) is passed on. Returns `None` only if `work` was lost, which no code path causes.
pub(crate) fn run<T: Send>(work: impl FnOnce(usize) -> T + Send) -> Option<T> {
    let slot = Mutex::new(Some(work));
    let take = |slot: &Mutex<Option<_>>| slot.lock().ok().and_then(|mut guard| guard.take());
    let spawned = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(BIG_STACK_BYTES)
            .name("ratio-check".to_owned())
            .spawn_scoped(scope, || take(&slot).map(|work| work(MAX_DEPTH)))
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    });
    match spawned {
        Ok(done) => done,
        Err(_) => take(&slot).map(|work| work(FALLBACK_DEPTH)),
    }
}
