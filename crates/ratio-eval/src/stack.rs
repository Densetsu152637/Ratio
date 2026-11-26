//! The big stack the preparation pass runs on: it walks the IR recursively, and the IR's
//! depth is bounded by the parser's nesting limit, as for the other passes. The evaluator
//! itself never recurses over the program (see [`crate::machine`]).

use std::sync::Mutex;

const BIG_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Runs `work` on a thread with a large stack, or here if none can be started.
pub(crate) fn run<T: Send>(work: impl FnOnce() -> T + Send) -> Option<T> {
    let slot = Mutex::new(Some(work));
    let take = |slot: &Mutex<Option<_>>| slot.lock().ok().and_then(|mut guard| guard.take());
    let spawned = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(BIG_STACK_BYTES)
            .name("ratio-prepare".to_owned())
            .spawn_scoped(scope, || take(&slot).map(|work| work()))
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    });
    match spawned {
        Ok(done) => done,
        Err(_) => take(&slot).map(|work| work()),
    }
}
