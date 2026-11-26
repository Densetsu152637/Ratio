//! The big stack lowering and the canonical writer run on, as the checker's passes do
//! (`ratio_check`'s `stack` module): both walk the tree recursively, and the parser bounds its
//! depth, so a thread with a large stack bounds the stack for that depth with a wide margin.
//!
//! If no such thread can be started, the work runs on the caller's stack with a small depth
//! bound, and refuses deeper trees ([`crate::LowerError::TooDeep`]).

use std::sync::Mutex;

/// The stack of the thread the passes run on.
const BIG_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The nesting the lowering follows on the big stack: the parser refuses trees deeper than
/// its own limit (at most 256), and lowering counts a few levels per tree level.
pub(crate) const MAX_DEPTH: usize = 2048;

/// The nesting it follows on the caller's stack.
pub(crate) const FALLBACK_DEPTH: usize = 12;

/// Runs `work` on a thread with a large stack, passing the depth it may recurse to; if no
/// thread can be started, runs it here with [`FALLBACK_DEPTH`]. A panic in `work` (a bug) is
/// passed on. Returns `None` only if `work` was lost, which no code path causes.
pub(crate) fn run<T: Send>(work: impl FnOnce(usize) -> T + Send) -> Option<T> {
    let slot = Mutex::new(Some(work));
    let take = |slot: &Mutex<Option<_>>| slot.lock().ok().and_then(|mut guard| guard.take());
    let spawned = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(BIG_STACK_BYTES)
            .name("ratio-ir".to_owned())
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
