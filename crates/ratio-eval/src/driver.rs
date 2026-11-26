//! The async driver: an evaluation as a `Future`.
//!
//! [`Evaluation`] polls the [`Machine`] until it ends. At every checkpoint the machine
//! returns, and the future wakes itself and returns `Pending`, so a long pure stretch gives its
//! runtime worker back every 1,000 steps (chapter 01). At an effect, stage 11 will await the
//! host here (`host.call(…).await`, then [`Machine::resume_effect`]); in stage 6 there is no
//! host, and every effect raises `Unsupported`. No async runtime is needed: [`block_on`] runs
//! an evaluation on the current thread with the standard library alone.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use crate::error::{Kind, raise};
use crate::labels::Labels;
use crate::machine::{Machine, Outcome, Suspend};

/// How an evaluation ended, with what it spent.
#[derive(Clone, Debug)]
pub struct Finished {
    pub outcome: Outcome,
    /// The steps charged.
    pub steps: u64,
    /// The most heap bytes live at once.
    pub heap_peak: u64,
}

/// An evaluation of a pure entry, as a future.
#[derive(Debug)]
pub struct Evaluation {
    machine: Machine,
    yields: u64,
}

impl Evaluation {
    pub(crate) fn new(machine: Machine) -> Evaluation {
        Evaluation { machine, yields: 0 }
    }

    /// How many checkpoints it has yielded at.
    #[must_use]
    pub fn yields(&self) -> u64 {
        self.yields
    }

    /// The machine (to drive it by hand, answering effects).
    pub fn machine(&mut self) -> &mut Machine {
        &mut self.machine
    }
}

impl Future for Evaluation {
    type Output = Finished;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Finished> {
        loop {
            match self.machine.run() {
                Suspend::Yield => {
                    self.yields += 1;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                Suspend::Effect(req) => {
                    let e = raise(
                        self.machine.prepared(),
                        Kind::Unsupported,
                        &format!(
                            "`{}` makes effects, which need a host (stage 11)",
                            req.builtin
                        ),
                        Labels::NONE,
                        req.loc,
                    );
                    self.machine.resume_effect(Err(e));
                }
                Suspend::Done(outcome) => {
                    return Poll::Ready(Finished {
                        outcome,
                        steps: self.machine.steps(),
                        heap_peak: self
                            .machine
                            .heap()
                            .ledger()
                            .map_or(0, crate::heap::HeapLedger::peak),
                    });
                }
            }
        }
    }
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs a future to completion on the current thread (standard library only).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}
