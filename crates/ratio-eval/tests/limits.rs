//! Charging and limits: cost classes debited before a built-in runs, the heap ledger, call
//! depth, value depth, cancellation, cooperative yields, and hostile programs on small stacks.

mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use ratio_eval::{CancelToken, Limits, Outcome, Value, builtins::Class};
use support::{raised, run, run_with};

/// `text.repeat` and `text.similarity` called with their arguments: the `output` and
/// `product` classes, whose exact charges `conformance/eval/steps-output.ratio` and
/// `steps-product.ratio` also pin.
const CHARGED: &str = r"
fn rep(a: Str, n: Int) -> Str { text.repeat(a, n) }
fn sim(a: Str, b: Str) -> Float { text.similarity(a, b) }
";

/// Steps of either call without the built-in: two parameter patterns, then block, statement,
/// call and two locals.
const BODY: u64 = 7;

#[test]
fn every_class_has_a_built_in() {
    let classes: Vec<Class> = ratio_eval::builtins::implemented()
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    for c in [
        Class::Const,
        Class::Log,
        Class::NLogN,
        Class::Product,
        Class::Output,
        Class::Linear,
    ] {
        assert!(classes.contains(&c), "no built-in of class {}", c.name());
    }
}

#[test]
fn output_class_is_charged_from_the_output_its_arguments_fix() {
    let p = support::prepared(CHARGED);
    // m = 2 × 100 = 200: 1 + ⌈200 / 64⌉ = 5.
    let f = run(&p, "rep", vec![Value::text("ab"), Value::int(100)]);
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert_eq!(f.steps, BODY + 5);
}

#[test]
fn product_class_is_debited_before_running() {
    let p = support::prepared(CHARGED);
    // n₁ = n₂ = 3, c = 6: w = 15, 2 steps.
    let f = run(&p, "sim", vec![Value::text("abc"), Value::text("abd")]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert!((v.as_float().unwrap() - 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(f.steps, BODY + 2);
    // Two 20,000-character texts: w = 4 × 10⁸ + 40,000, far more than the steps left. The
    // built-in raises `Budget` without running (it would take seconds): only the body is
    // charged.
    let big = "x".repeat(20_000);
    let start = std::time::Instant::now();
    let f = run(&p, "sim", vec![Value::text(&big), Value::text(&big)]);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
    assert_eq!(f.steps, BODY);
}

#[test]
fn repeat_refuses_negative_huge_and_overlong_counts_before_allocating() {
    let p = support::prepared(CHARGED);
    for (s, n) in [
        ("ab", -1),
        ("ab", i64::MAX),
        ("abc", i64::MAX / 2),
        ("a", 16 * 1024 * 1024 + 1),
    ] {
        let f = run(&p, "rep", vec![Value::text(s), Value::int(n)]);
        let Outcome::Raised(e) = f.outcome else {
            panic!("repeat({s:?}, {n}) returned")
        };
        assert_eq!(e.kind(&p), "Invalid", "repeat({s:?}, {n})");
        // Refused at its one-step minimum charge: nothing was built.
        assert_eq!(f.steps, BODY + 1, "repeat({s:?}, {n})");
        assert!(
            f.heap_peak < 1024,
            "repeat({s:?}, {n}) allocated {}",
            f.heap_peak
        );
    }
    // Exactly 16 MiB is allowed, and charged for its output: 1 + 2^24 / 64.
    let f = run(
        &p,
        "rep",
        vec![Value::text("a"), Value::int(16 * 1024 * 1024)],
    );
    let Outcome::Returned(v) = f.outcome else {
        panic!("16 MiB was refused")
    };
    assert_eq!(v.as_text().map(str::len), Some(16 * 1024 * 1024));
    assert_eq!(f.steps, BODY + 1 + (1 << 24) / 64);
}

#[test]
fn repeat_and_similarity_carry_their_arguments_labels() {
    let p = support::prepared(CHARGED);
    let secret = ratio_eval::Labels::source(0);
    for args in [
        vec![Value::text("ab").with_labels(secret), Value::int(2)],
        vec![Value::text("ab"), Value::int(2).with_labels(secret)],
    ] {
        let Outcome::Returned(v) = run(&p, "rep", args).outcome else {
            panic!("raised")
        };
        assert_eq!(v.scan(), secret);
    }
    for args in [
        vec![Value::text("ab").with_labels(secret), Value::text("ac")],
        vec![Value::text("ab"), Value::text("ac").with_labels(secret)],
    ] {
        let Outcome::Returned(v) = run(&p, "sim", args).outcome else {
            panic!("raised")
        };
        assert_eq!(v.scan(), secret);
    }
    // Neither is a sink: labelled arguments are accepted, and nothing removes the label.
    let Outcome::Returned(v) = run(
        &p,
        "rep",
        vec![Value::text("").with_labels(secret), Value::int(0)],
    )
    .outcome
    else {
        panic!("raised")
    };
    assert_eq!(v.scan(), secret);
}

#[test]
fn the_heap_counts_live_values() {
    let p = support::prepared(
        r#"
fn grow(n: Int) -> Int {
  var xs: [Str] = []
  repeat n { xs = xs.push("0123456789abcdef0123456789abcdef") }
  list.len(xs)
}
fn churn(n: Int) -> Int {
  var total = 0
  repeat n {
    let s = "0123456789abcdef" + "0123456789abcdef"
    total += text.len(s)
  }
  total
}
"#,
    );
    let small = Limits {
        heap: 256 * 1024,
        ..Limits::default()
    };
    match run_with(&p, "grow", vec![Value::int(100_000)], small).outcome {
        Outcome::Raised(e) => assert_eq!(e.budget(&p).as_deref(), Some("heap")),
        Outcome::Returned(_) => panic!("100,000 live strings fit in 256 KiB"),
    }
    // Values built and dropped are given back: 20,000 iterations allocate far more than 256 KiB
    // in all, but little at once.
    let f = run_with(&p, "churn", vec![Value::int(20_000)], small);
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert!(f.heap_peak < 64 * 1024, "peak {}", f.heap_peak);
}

#[test]
fn call_depth_and_value_depth_are_bounded() {
    let p = support::prepared(
        r"
fn down(n: Int) -> Int { down(n + 1) }
fn chain(n: Int) -> Int {
  var f = || 0
  repeat n {
    let g = f
    f = || g() + 1
  }
  f()
}
",
    );
    let deep = Limits {
        call_depth: 4096,
        ..Limits::default()
    };
    assert_eq!(raised(&p, "down", vec![Value::int(0)], deep), "Budget");
    // Closures nesting closures: deeper than 256 is refused, so no walk or drop of a value can
    // exhaust the stack.
    assert_eq!(
        raised(&p, "chain", vec![Value::int(1000)], Limits::default()),
        "Invalid"
    );
    let Outcome::Returned(v) = run(&p, "chain", vec![Value::int(100)]).outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_int(), Some(100));
}

#[test]
fn hostile_programs_run_on_a_small_stack() {
    let p = support::prepared(
        r"
fn down(n: Int) -> Int { if n == 0 { 0 } else { 1 + down(n - 1) } }
fn chain(n: Int) -> Int {
  var f = || 0
  repeat n {
    let g = f
    f = || g() + 1
  }
  f()
}
",
    );
    // The evaluator keeps its own stack on the heap: 4,096 nested calls, and the deepest
    // value allowed, on a 256 KiB thread (a debug build).
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let limits = Limits {
                call_depth: 4096,
                ..Limits::default()
            };
            let a = run_with(&p, "down", vec![Value::int(4000)], limits);
            let b = run_with(&p, "chain", vec![Value::int(250)], limits);
            (a.outcome, b.outcome)
        })
        .unwrap();
    let (a, b) = worker.join().expect("no stack overflow");
    let (Outcome::Returned(a), Outcome::Returned(b)) = (a, b) else {
        panic!("raised")
    };
    assert_eq!((a.as_int(), b.as_int()), (Some(4000), Some(250)));
}

#[test]
fn cancellation_cannot_be_caught() {
    let p = support::prepared("fn main() -> Int { try { loop {} } catch e { 1 } }");
    let cancel = CancelToken::new();
    cancel.cancel();
    let entry = p.find("main.ratio", "main").unwrap();
    let eval =
        ratio_eval::evaluate(Arc::clone(&p), entry, Vec::new(), Limits::default(), cancel).unwrap();
    let f = ratio_eval::block_on(eval);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Cancelled");
    // It was seen at the first checkpoint.
    assert!(f.steps >= 1000 && f.steps < 1100, "{}", f.steps);
}

/// Yields to the executor once.
struct YieldNow(bool);
impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[test]
fn a_long_pure_loop_lets_another_session_progress_on_one_thread() {
    let p = support::prepared(
        r"
fn main(n: Int) -> Int {
  var i = 0
  while i < n { i += 1 }
  i
}
",
    );
    let entry = p.find("main.ratio", "main").unwrap();
    let limits = Limits {
        steps: 20_000_000,
        ..Limits::default()
    };
    let mut long = ratio_eval::evaluate(
        Arc::clone(&p),
        entry,
        vec![Value::int(750_000)],
        limits,
        CancelToken::new(),
    )
    .unwrap();
    // A second session: counts its turns until the first is done.
    let done = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let (d, c) = (Arc::clone(&done), Arc::clone(&progress));
    let mut other: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
        while !d.load(Ordering::Relaxed) {
            c.fetch_add(1, Ordering::Relaxed);
            YieldNow(false).await;
        }
    });
    // A single-threaded round-robin executor.
    let mut cx = Context::from_waker(Waker::noop());
    let mut seen_while_running = 0;
    let finished = loop {
        if let Poll::Ready(f) = Pin::new(&mut long).poll(&mut cx) {
            break f;
        }
        let _ = other.as_mut().poll(&mut cx);
        seen_while_running = progress.load(Ordering::Relaxed);
    };
    done.store(true, Ordering::Relaxed);
    let _ = other.as_mut().poll(&mut cx);
    assert!(finished.steps >= 5_000_000, "{} steps", finished.steps);
    assert!(matches!(finished.outcome, Outcome::Returned(_)));
    // It yielded at every 1,000-step checkpoint, and the other session ran each time.
    assert!(
        seen_while_running >= finished.steps / 1000 - 1,
        "{seen_while_running}"
    );
}
