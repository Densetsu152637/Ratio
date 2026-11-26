//! The big stack that the recursive passes (the parser, the printer and the tree dump) run on.
//!
//! Recursive descent costs about a dozen frames per level of nesting (one per level of the
//! precedence ladder) and a debug build's frames are large, so a tree at the nesting limit (256
//! levels) needs more than the 2 MiB a test thread gets and, on Windows, than the 1 MiB of a main
//! thread. Each pass therefore runs on a thread of its own with [`BIG_STACK_BYTES`] of stack: the
//! nesting limit bounds the depth, this bounds the stack for that depth with a wide margin
//! (`tests/parser.rs` measures the need). The memory is address space until touched.
//!
//! If no such thread can be started, the pass runs on the caller's stack, whose size is unknown,
//! and is told so: it then refuses depth beyond [`FALLBACK_DEPTH`] (the parser reports `P0002`,
//! the printer writes `<too deep>` and the dump `(too-deep)`).

use std::sync::Mutex;

/// The stack of the thread the passes run on.
pub(crate) const BIG_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The depth a pass allows when it runs on the caller's stack: small enough for a 512 KiB
/// stack (the smallest in use) in a debug build, where nested parentheses or `if` blocks
/// overflowed it from 24 levels. `tests/parser.rs` runs every pass in this mode on such a stack.
// Must stay at or below the token reader's own clamp; the 512 KiB fallback test in
// `tests/parser.rs` guards it.
pub(crate) const FALLBACK_DEPTH: usize = 12;

/// Runs `work` on a thread with [`BIG_STACK_BYTES`] of stack, passing `true`; if no thread can be
/// started, runs it on this thread, passing `false`. A panic in `work` (a bug) is passed on to the
/// caller. Returns `None` only if `work` was lost (its slot was poisoned), which no code path here
/// causes; callers report an internal error.
pub(crate) fn run<T: Send>(work: impl FnOnce(bool) -> T + Send) -> Option<T> {
    let slot = Mutex::new(Some(work));
    let take = |slot: &Mutex<Option<_>>| slot.lock().ok().and_then(|mut guard| guard.take());
    let spawned = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(BIG_STACK_BYTES)
            .name("ratio-pass".to_owned())
            .spawn_scoped(scope, || take(&slot).map(|work| work(true)))
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    });
    match spawned {
        Ok(done) => done,
        // No thread could be started: the work is still in the slot.
        Err(_) => take(&slot).map(|work| work(false)),
    }
}
