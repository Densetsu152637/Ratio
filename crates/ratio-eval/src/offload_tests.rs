//! The pool and the machine together (chapter 07: "heap is accounted before dispatch, and a
//! cancelled branch's job keeps its place, heap and steps until it ends"). These tests hold
//! the pool's only worker with a job of their own, which only the crate can submit.

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use ratio_names::{Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source};

use crate::machine::{CancelToken, Limits, Machine, Outcome, Suspend};
use crate::pool::{Pool, Thresholds};
use crate::prepare::Prepared;
use crate::value::Value;

fn prepared(source: &str) -> Arc<Prepared> {
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let mut registry = MemoryRegistry::new();
    let rel = RelPath::parse("main.ratio").unwrap();
    registry.set_entry(&package, rel.clone());
    registry.add_file(&package, rel, source.as_bytes().to_vec());
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap());
    assert!(loaded.is_clean());
    let checked = ratio_check::check(&loaded);
    assert!(checked.is_clean(), "{:?}", checked.diagnostics);
    crate::prepare(&ratio_ir::lower(&loaded, &checked).unwrap()).unwrap()
}

fn live(m: &Machine) -> u64 {
    m.heap().ledger().map_or(0, crate::heap::HeapLedger::live)
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_cancelled_branch_keeps_its_jobs_place_steps_and_heap_until_the_job_ends() {
    let pool = Pool::with_thresholds(
        1,
        Thresholds {
            text_bytes: 1024,
            elements: 1024,
        },
    );
    // Hold the only worker.
    let (release, gate) = mpsc::channel::<()>();
    let blocker = pool.session().submit(move || {
        let _ = gate.recv();
    });
    let session = pool.session();
    let p = prepared(r#"fn main(s: Str) -> Bool { re.is_match("(a|b)+c", s) }"#);
    let entry = p.find("main.ratio", "main").unwrap();
    let cancel = CancelToken::new();
    let mut m = Machine::new(
        Arc::clone(&p),
        entry,
        vec![Value::text(&"ab".repeat(4096))],
        Limits::default(),
        cancel.clone(),
    )
    .unwrap();
    m.set_offload(Some(session.clone()));
    let before = live(&m);
    let waiter = loop {
        match m.run() {
            Suspend::Yield => {}
            Suspend::Wait(w) => break w,
            other => panic!("expected a wait, got {other:?}"),
        }
    };
    // Dispatched: in flight, its steps debited and its working memory charged before it ran.
    assert!(!waiter.is_ready());
    assert_eq!(session.in_flight(), 1);
    let re = crate::re::Regex::new("(a|b)+c").unwrap();
    let scratch = crate::re::scratch_bytes(re.insts(), 0);
    assert!(
        live(&m) >= before + scratch,
        "the job's heap was not charged"
    );
    let steps = m.steps();
    assert!(
        steps > 8192 * re.weight() / 64,
        "charged only {steps} steps"
    );

    // Cancelling stops the wait at once.
    cancel.cancel();
    let Suspend::Done(Outcome::Raised(e)) = m.run() else {
        panic!("the cancelled machine kept waiting")
    };
    assert_eq!(e.kind(&p), "Cancelled");
    assert_eq!(m.steps(), steps, "the job's steps were given back");
    let held = live(&m);
    assert!(held >= scratch);

    // The job keeps its place: one more offload of the session fills the cap, the next waits.
    let a = session.submit(|| 1);
    let b = session.submit(|| 2);
    assert_eq!(session.in_flight(), 2);
    assert_eq!(session.queued(), 1);
    assert_eq!(live(&m), held, "the job's heap was given back early");

    // When it ends, its place and its heap are released.
    release.send(()).unwrap();
    blocker.waiter().wait();
    for h in [&a, &b] {
        h.waiter().wait();
    }
    eventually("the session's jobs to end", || session.in_flight() == 0);
    assert_eq!(live(&m), held - scratch);
    assert_eq!(m.steps(), steps);
}

/// A waker that records being woken.
struct Flag(Arc<std::sync::atomic::AtomicBool>);
impl std::task::Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn a_waiting_evaluation_frees_its_thread_and_wakes_when_the_job_ends() {
    let pool = Pool::with_thresholds(
        1,
        Thresholds {
            text_bytes: 16,
            elements: 16,
        },
    );
    let (release, gate) = mpsc::channel::<()>();
    let _blocker = pool.session().submit(move || {
        let _ = gate.recv();
    });
    let p = prepared(r"fn main(s: Str) -> Str { text.upper(s) }");
    let entry = p.find("main.ratio", "main").unwrap();
    let eval = crate::evaluate(
        Arc::clone(&p),
        entry,
        vec![Value::text(&"x".repeat(64))],
        Limits::default(),
        CancelToken::new(),
    )
    .unwrap()
    .offload_to(pool.session());
    let mut eval = Box::pin(eval);
    let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waker = std::task::Waker::from(Arc::new(Flag(Arc::clone(&woken))));
    let mut cx = std::task::Context::from_waker(&waker);
    // Pending while the job waits for the worker: the thread is free.
    assert!(std::future::Future::poll(eval.as_mut(), &mut cx).is_pending());
    assert!(!woken.load(std::sync::atomic::Ordering::SeqCst));
    release.send(()).unwrap();
    eventually("the waker", || {
        woken.load(std::sync::atomic::Ordering::SeqCst)
    });
    let std::task::Poll::Ready(done) = std::future::Future::poll(eval.as_mut(), &mut cx) else {
        panic!("still pending after the wake")
    };
    let Outcome::Returned(v) = done.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_text(), Some("X".repeat(64).as_str()));
}
