//! A timer future on the standard library alone: the first poll starts a thread that parks
//! until the deadline (or until the future is dropped, which unparks it at once), then wakes
//! the latest waker. Hosts with a runtime use its timers instead ([`super::Host::timer`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread::{self, Thread};
use std::time::Instant;

#[derive(Debug, Default)]
struct Shared {
    fired: AtomicBool,
    dropped: AtomicBool,
    waker: Mutex<Option<Waker>>,
    thread: Mutex<Option<Thread>>,
}

/// Resolves at its deadline.
#[derive(Debug)]
pub struct Sleep {
    at: Instant,
    shared: Option<Arc<Shared>>,
}

impl Sleep {
    #[must_use]
    pub fn until(at: Instant) -> Sleep {
        Sleep { at, shared: None }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.at {
            return Poll::Ready(());
        }
        if let Some(s) = &self.shared {
            if s.fired.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            *s.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
            if s.fired.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            return Poll::Pending;
        }
        let shared = Arc::new(Shared::default());
        *shared.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
        let at = self.at;
        let s = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("ratio-timer".to_owned())
            .spawn(move || {
                loop {
                    if s.dropped.load(Ordering::Acquire) {
                        return;
                    }
                    let now = Instant::now();
                    if now >= at {
                        break;
                    }
                    thread::park_timeout(at - now);
                }
                s.fired.store(true, Ordering::Release);
                let w = s
                    .waker
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if let Some(w) = w {
                    w.wake();
                }
            });
        match handle {
            Ok(h) => {
                *shared.thread.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(h.thread().clone());
            }
            // No thread: poll again (busy, but correct).
            Err(_) => cx.waker().wake_by_ref(),
        }
        self.shared = Some(shared);
        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some(s) = &self.shared {
            s.dropped.store(true, Ordering::Release);
            if let Some(t) = s
                .thread
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                t.unpark();
            }
        }
    }
}
