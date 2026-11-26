//! Stage 7 (`text`, `re`, the pool): the work every built-in does stays within a fixed
//! multiple of its charge; large built-ins give the same values, labels and charges on the
//! pool as inline; a charge the steps left cannot pay raises `Budget` without running; and
//! regular expressions run in linear time whatever the pattern.

#![allow(
    clippy::many_single_char_names,
    reason = "tests name programs, values and results briefly (p, s, f, e)"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratio_eval::{
    CancelToken, Finished, Labels, Limits, Outcome, Pool, PoolSession, Prepared, Thresholds, Value,
    block_on, to_json_text,
};
use support::{SECRET, secret};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/data")
}

/// The `// arg:` values of a corpus file (JSON scalars), labelled secret.
fn args_of(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| l.strip_prefix("// arg: "))
        .map(|a| {
            let a = a.trim();
            if let Some(s) = a.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                secret(&unescape(s))
            } else {
                Value::int(a.parse().expect("an Int argument")).with_labels(SECRET)
            }
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

/// Every corpus program with its arguments.
fn programs() -> Vec<(String, Arc<Prepared>, Vec<Value>)> {
    let mut paths: Vec<PathBuf> = fs::read_dir(corpus())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ratio"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let text = fs::read_to_string(&p).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, support::prepared(&text), args_of(&text))
        })
        .collect()
}

/// Larger inputs for every `text` and `re` built-in, so the charges are well past their
/// constants.
const BIG: &str = r#"
fn main(s: Str, w: Str) -> [Untrusted<Str>] {
  let lines = text.join([s, w, s], sep: "\n")
  [
    text.show(text.len(s)), text.show(text.is_empty(s)), text.lower(s), text.upper(s),
    text.trim(s), text.trim_start(s), text.trim_end(s), text.show(text.contains(s, w)),
    text.show(text.starts_with(s, w)), text.show(text.ends_with(s, w)),
    text.show(text.find(s, "zz", from: 3)), text.show(text.rfind(s, "ab")),
    text.show(text.count(s, "ab")), text.slice(s, 10, end: -10), text.truncate(s, 50),
    text.show(text.split(s, "b")), text.show(text.split_once(s, "b")), text.show(text.lines(lines)),
    text.show(text.words(w)), text.show(text.chars(w)), text.join([s, w], sep: "--"),
    text.replace(s, "ab", "xyz"), text.replace_first(s, "ab", "x"), text.repeat(w, 20),
    text.reverse(s), text.pad_start(w, 3000, fill: "-"), text.pad_end(w, 3000),
    text.before(s, "ba"), text.after(s, "ba"), text.before_last(s, "ab"), text.after_last(s, "ab"),
    text.show(text.strip_prefix(s, "ab")), text.show(text.strip_suffix(s, "ab")),
    text.trim_end_matches(s, "b"), text.dedent(lines), text.indent(lines),
    text.wrap(w, width: 7), text.number_lines(lines), text.show(text.parse_int(s)),
    text.show(text.parse_float(s)), text.show(text.similarity(w, text.slice(s, 0, end: 300))),
    text.show(text.extract_json(s)), text.show(text.code_blocks(lines)), text.quote(s),
    text.show(re.is_match("(a|b)*c", s)), text.show(re.find("b+a", s)),
    text.show(re.find_all("ab|b", s, limit: 5000).len()),
    text.show(re.captures("(?P<x>a)(b)", s)), text.show(re.captures_all("(a)(b)", s).len()),
    re.replace("(a)b", s, "$1$1"), re.replace_all("(a)(b)", s, "${2}$1"),
    text.show(re.split("b+", s).len()), text.show(re.find_all("x*", w).len()), re.escape(w),
  ]
}
"#;

fn big_args() -> Vec<Value> {
    let s = "ab".repeat(2000) + "aab";
    let w = "word wörd ".repeat(100);
    vec![secret(&s), secret(&w)]
}

/// Runs `main`, on `pool` if given, returning the result and the machine's work log.
fn run_on(
    p: &Arc<Prepared>,
    args: Vec<Value>,
    limits: Limits,
    pool: Option<&PoolSession>,
) -> (Finished, Vec<ratio_eval::BuiltinWork>) {
    let entry = p.find("main.ratio", "main").expect("a main");
    let mut eval =
        ratio_eval::evaluate(Arc::clone(p), entry, args, limits, CancelToken::new()).unwrap();
    if let Some(session) = pool {
        eval = eval.offload_to(session.clone());
    }
    let finished = block_on(&mut eval);
    #[cfg(debug_assertions)]
    let log = eval.machine().work_log().to_vec();
    #[cfg(not(debug_assertions))]
    let log = Vec::new();
    (finished, log)
}

/// What a run gives: the value as JSON with each element's labels, or the error kind; the
/// steps; the heap peak.
fn observed(p: &Prepared, f: &Finished) -> (String, Vec<Labels>, u64, u64) {
    match &f.outcome {
        Outcome::Returned(v) => {
            let labels = v
                .elements()
                .map_or_else(|| vec![v.scan()], |es| es.iter().map(Value::scan).collect());
            (to_json_text(p, v), labels, f.steps, f.heap_peak)
        }
        Outcome::Raised(e) => (e.kind(p), Vec::new(), f.steps, f.heap_peak),
    }
}

/// A built-in's counted work never passes this multiple of what it was charged, at 64 units
/// a step.
const WORK_PER_STEP: u64 = 64 * 8;

#[cfg(debug_assertions)]
#[test]
fn work_stays_within_a_fixed_multiple_of_every_charge() {
    let mut seen = std::collections::BTreeSet::new();
    let mut worst: (f64, &str) = (0.0, "");
    let mut runs = programs();
    runs.push(("big".to_owned(), support::prepared(BIG), big_args()));
    for (name, p, args) in &runs {
        let (f, log) = run_on(p, args.clone(), Limits::default(), None);
        assert!(
            matches!(f.outcome, Outcome::Returned(_)),
            "{name}: {:?}",
            observed(p, &f).0
        );
        for w in &log {
            seen.insert(w.name);
            assert!(
                w.work <= WORK_PER_STEP * w.steps,
                "{name}: `{}` counted {} units for {} steps",
                w.name,
                w.work,
                w.steps
            );
            #[allow(clippy::cast_precision_loss, reason = "a ratio for the message")]
            let ratio = w.work as f64 / (64.0 * w.steps as f64);
            if ratio > worst.0 {
                worst = (ratio, w.name);
            }
        }
    }
    for (b, _) in ratio_eval::builtins::implemented() {
        if b.starts_with("text.") || b.starts_with("re.") {
            assert!(seen.contains(b), "`{b}` never ran");
        }
    }
    eprintln!(
        "the most work per charged unit: {:.2} ({})",
        worst.0, worst.1
    );
}

#[test]
fn offloaded_built_ins_give_the_same_values_labels_and_charges() {
    // Every offloadable built-in goes to the pool at a threshold of 0.
    let pool = Pool::with_thresholds(
        2,
        Thresholds {
            text_bytes: 0,
            elements: 0,
            steps: 0,
        },
    );
    let session = pool.session();
    let mut runs = programs();
    runs.push(("big".to_owned(), support::prepared(BIG), big_args()));
    let mut offloaded = 0;
    for (name, p, args) in &runs {
        let (inline, _) = run_on(p, args.clone(), Limits::default(), None);
        let (pooled, log) = run_on(p, args.clone(), Limits::default(), Some(&session));
        assert_eq!(
            observed(p, &inline),
            observed(p, &pooled),
            "{name}: inline and pooled runs differ"
        );
        offloaded += log.iter().filter(|w| w.offloaded).count();
    }
    if cfg!(debug_assertions) {
        assert!(offloaded > 100, "only {offloaded} calls ran on the pool");
    }
    assert_eq!(session.in_flight(), 0);
}

#[test]
fn inputs_past_the_default_threshold_run_on_the_pool_with_the_same_results() {
    let pool = Pool::new(2);
    let session = pool.session();
    let p = support::prepared(
        r#"
fn main(s: Str) -> [Untrusted<Str>] {
  [text.upper(s), text.show(re.find_all("b+", s, limit: 100000).len()), re.replace_all("(a)b", s, "$1-")]
}
"#,
    );
    let large = || vec![secret(&"ab".repeat(40_000))];
    let small = || vec![secret(&"ab".repeat(1_000))];
    for args in [large(), small()] {
        let (inline, _) = run_on(&p, args.clone(), Limits::default(), None);
        let (pooled, log) = run_on(&p, args.clone(), Limits::default(), Some(&session));
        assert_eq!(observed(&p, &inline), observed(&p, &pooled));
        if cfg!(debug_assertions) {
            // `text.upper` goes to the pool for a large text only; the iterating `re`
            // built-ins, which debit as they go, whenever the steps left are many.
            let big = args[0].as_text().unwrap().len() as u64 >= ratio_eval::OFFLOAD_TEXT_BYTES;
            for w in &log {
                if w.name == "text.upper" {
                    assert_eq!(w.offloaded, big, "{log:?}");
                }
                if w.name.starts_with("re.") {
                    assert!(w.offloaded, "{log:?}");
                }
            }
        }
    }
}

#[test]
fn a_charge_the_steps_left_cannot_pay_raises_budget_without_running() {
    let p = support::prepared(
        r#"
fn sim(a: Str, b: Str) -> Float { text.similarity(a, b) }
fn all(s: Str) -> Int { re.find_all("(a|b)*c", s).len() }
"#,
    );
    let limits = Limits {
        steps: 200_000,
        ..Limits::default()
    };
    // 20,000 × 20,000 cells would take seconds; the charge (6,250,626 steps) is refused first,
    // and so is a search over a MiB.
    let a = Value::text(&"a".repeat(20_000));
    let b = Value::text(&"b".repeat(20_000));
    let started = Instant::now();
    let f = support::run_with(&p, "sim", vec![a, b], limits);
    let Outcome::Raised(e) = &f.outcome else {
        panic!("similarity returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
    assert!(f.steps < 100, "charged {} steps", f.steps);
    let s = Value::text(&"ab".repeat(512 * 1024));
    let f = support::run_with(&p, "all", vec![s], limits);
    let Outcome::Raised(e) = &f.outcome else {
        panic!("find_all returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
    assert!(f.steps < 100, "charged {} steps", f.steps);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_quadratic_iteration_pays_as_it_goes_and_stops_at_the_step_limit() {
    // `.*z|a` rescans to the end after every match (as Rust's `regex` does): the work past
    // the search charge is debited as it is done, so the call stops at the limit.
    let p = support::prepared(
        r#"fn all(s: Str) -> Int { re.find_all(".*z|a", s, limit: 1000000).len() }"#,
    );
    let s = Value::text(&"a".repeat(20_000));
    let started = Instant::now();
    let f = support::run_with(&p, "all", vec![s.clone()], Limits::default());
    let Outcome::Raised(e) = &f.outcome else {
        panic!("a quadratic iteration returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
    assert!(started.elapsed() < Duration::from_secs(60));
    // With a short text it finishes and is charged for every rescan.
    let short = Value::text(&"a".repeat(300));
    let f = support::run_with(&p, "all", vec![short], Limits::default());
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_int(), Some(300));
    assert!(f.steps > 300 * 300 / 64, "charged only {} steps", f.steps);
}

#[test]
fn hostile_patterns_run_in_linear_time() {
    let p = support::prepared(
        r"
fn matches(p: Str, s: Str) -> Bool { re.is_match(p, s) }
fn found(p: Str, s: Str) -> Int? { re.find(p, s)?.end }
",
    );
    let limits = Limits {
        steps: Limits::MAX_STEPS,
        ..Limits::default()
    };
    let a = Value::text(&"a".repeat(1024 * 1024));
    for (pattern, entry, want) in [
        ("(a+)+$", "matches", Value::bool(true)),
        ("(a|a)*b", "matches", Value::bool(false)),
        ("(.*a){20}", "matches", Value::bool(true)),
        ("(.*a){20}", "found", Value::int(1024 * 1024)),
        ("(a*)*b", "found", Value::none()),
        ("(x+x+)+y", "matches", Value::bool(false)),
    ] {
        let started = Instant::now();
        let f = support::run_with(&p, entry, vec![Value::text(pattern), a.clone()], limits);
        let elapsed = started.elapsed();
        let Outcome::Returned(v) = f.outcome else {
            panic!("`{pattern}` raised")
        };
        assert_eq!(to_json_text(&p, &v), to_json_text(&p, &want), "`{pattern}`");
        // Generous: a backtracking engine would not finish these at all.
        assert!(
            elapsed < Duration::from_secs(120),
            "`{pattern}` took {elapsed:?}"
        );
        eprintln!("`{pattern}` on 1 MiB: {elapsed:?}, {} steps", f.steps);
    }
}

#[test]
fn oversized_programs_and_patterns_are_invalid() {
    let p = support::prepared(r"fn matches(p: Str, s: Str) -> Bool { re.is_match(p, s) }");
    for pattern in [
        "(a{1000}){1000}".to_owned(),
        r"\w{1000}".repeat(6),
        "a".repeat(4097),
        "(?=a)".to_owned(),
        r"(a)\1".to_owned(),
    ] {
        let kind = support::raised(
            &p,
            "matches",
            vec![Value::text(&pattern), Value::text("a")],
            Limits::default(),
        );
        assert_eq!(kind, "Invalid", "`{pattern}`");
    }
}

#[test]
fn a_long_fill_is_charged_and_only_its_needed_characters_are_read() {
    let p = support::prepared("fn pad(s: Str, f: Str) -> Str { text.pad_start(s, 3, fill: f) }");
    let fill = Value::text(&"xy".repeat(500_000));
    let started = Instant::now();
    let f = support::run_with(&p, "pad", vec![Value::text("a"), fill], Limits::default());
    let Outcome::Returned(v) = &f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_text(), Some("xya"));
    assert!(
        f.steps > 1_000_000 / 64,
        "the fill was not charged: {} steps",
        f.steps
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn over_long_splits_are_refused_before_their_parts_are_built() {
    let p = support::prepared(
        r#"
fn split(s: Str) -> Int { text.split(s, ",").len() }
fn lines(s: Str) -> Int { text.lines(s).len() }
fn words(s: Str) -> Int { text.words(s).len() }
fn chars(s: Str) -> Int { text.chars(s).len() }
"#,
    );
    let commas = Value::text(&",".repeat(2_000_000));
    let newlines = Value::text(&"\n".repeat(2_000_000));
    let words = Value::text(&"a ".repeat(1_500_000));
    for (entry, s) in [
        ("split", commas.clone()),
        ("lines", newlines),
        ("words", words),
        ("chars", commas),
    ] {
        let f = support::run_with(&p, entry, vec![s], Limits::default());
        let Outcome::Raised(e) = &f.outcome else {
            panic!("`{entry}` returned")
        };
        assert_eq!(e.kind(&p), "Invalid", "{entry}");
        // Only the counting pass ran: nothing was built for the parts.
        assert!(
            f.heap_peak < 1024 * 1024,
            "{entry}: heap peak {}",
            f.heap_peak
        );
    }
    // `re.split` cannot count first; each part is charged as it is built, so it stops at the
    // heap limit or the element limit.
    let p = support::prepared(r#"fn split(s: Str) -> Int { re.split(",", s).len() }"#);
    let f = support::run_with(
        &p,
        "split",
        vec![Value::text(&",".repeat(4_000_000))],
        Limits::default(),
    );
    let Outcome::Raised(e) = &f.outcome else {
        panic!("re.split returned")
    };
    assert!(
        matches!(e.kind(&p).as_str(), "Invalid" | "Budget"),
        "{}",
        e.kind(&p)
    );
    assert!(f.heap_peak <= Limits::default().heap);
}

#[test]
fn a_heavy_pattern_over_a_short_text_runs_on_the_pool_and_stops_when_cancelled() {
    let p = support::prepared(r"fn main(p: Str, s: Str) -> Bool { re.is_match(p, s) }");
    let args = || {
        vec![
            Value::text(r"(?:\w{1000}){4}z"),
            Value::text(&"a".repeat(30_000)),
        ]
    };
    // Its charge before running passes `OFFLOAD_STEPS`, so it goes to the pool though its
    // text is short.
    let pool = Pool::new(1);
    let session = pool.session();
    let (pooled, log) = run_on(&p, args(), Limits::default(), Some(&session));
    assert!(matches!(pooled.outcome, Outcome::Returned(_)));
    if cfg!(debug_assertions) {
        assert!(
            log.iter().any(|w| w.name == "re.is_match" && w.offloaded),
            "{log:?}"
        );
    }
    // Inline, a cancellation stops the search within a checkpoint's worth of work.
    let entry = p.find("main.ratio", "main").unwrap();
    let cancel = CancelToken::new();
    let eval = ratio_eval::evaluate(
        Arc::clone(&p),
        entry,
        args(),
        Limits::default(),
        cancel.clone(),
    )
    .unwrap();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        cancel.cancel();
        Instant::now()
    });
    let finished = block_on(eval);
    let ended = Instant::now();
    let cancelled_at = canceller.join().unwrap();
    let Outcome::Raised(e) = &finished.outcome else {
        panic!("not cancelled")
    };
    assert_eq!(e.kind(&p), "Cancelled");
    assert!(ended.duration_since(cancelled_at) < Duration::from_secs(1));
}

#[test]
fn refused_and_cached_patterns_are_charged_their_compile_work() {
    let p = support::prepared(
        r#"
fn fails<T>(f: fn() -> T) -> Int { match result(f) { Ok { .. } => 0, Err { .. } => 1 } }
fn once(p: Str) -> Int { fails(|| re.is_match(p, "")) }
fn twice(p: Str) -> Int { fails(|| re.is_match(p, "")) + fails(|| re.is_match(p, "")) }
"#,
    );
    for pattern in [format!("{})", "[a-z]".repeat(800)), "[a-z]".repeat(800)] {
        let one = support::run(&p, "once", vec![Value::text(&pattern)]).steps;
        let two = support::run(&p, "twice", vec![Value::text(&pattern)]).steps;
        // Sixteen units a character: at least 16 × 4,000 / 64 = 1,000 steps, on a hit as on a miss.
        assert!(one > 1_000, "`{}…` charged {one} steps", &pattern[..10]);
        assert!(
            two >= 2 * one - 20,
            "a cached pattern was charged less: {one} then {two}"
        );
    }
}

#[test]
fn match_lists_are_charged_for_their_output() {
    let p =
        support::prepared(r#"fn all(s: Str) -> Int { re.find_all("", s, limit: 1000000).len() }"#);
    let n = 100_000;
    let f = support::run_with(
        &p,
        "all",
        vec![Value::text(&"x".repeat(n))],
        Limits::default(),
    );
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_int(), Some(i64::try_from(n).unwrap() + 1));
    // Four output units a match record, beyond the search's own charge.
    assert!(f.steps > 4 * n as u64 / 64, "charged {} steps", f.steps);
}

#[test]
fn an_iteration_over_a_short_text_runs_on_the_pool_with_the_same_charges() {
    // `.*z|a` rescans after every match: the pre-charge is small, but the call debits as it
    // goes, so with a pool it never holds the evaluation's thread.
    let p = support::prepared(
        r#"fn main(s: Str) -> Int { re.find_all(".*z|a", s, limit: 1000000).len() }"#,
    );
    let args = || vec![secret(&"a".repeat(2_000))];
    let pool = Pool::new(1);
    let session = pool.session();
    let (inline, _) = run_on(&p, args(), Limits::default(), None);
    let (pooled, log) = run_on(&p, args(), Limits::default(), Some(&session));
    assert_eq!(observed(&p, &inline), observed(&p, &pooled));
    if cfg!(debug_assertions) {
        assert!(
            log.iter().any(|w| w.name == "re.find_all" && w.offloaded),
            "{log:?}"
        );
    }
    // With few steps left it stays inline, and still pays as it goes.
    let few = Limits {
        steps: 5_000,
        ..Limits::default()
    };
    let (f, log) = run_on(&p, args(), few, Some(&session));
    if cfg!(debug_assertions) {
        assert!(log.iter().all(|w| !w.offloaded), "{log:?}");
    }
    assert!(matches!(f.outcome, Outcome::Raised(_)));
}

#[test]
fn wrapping_streams_its_words() {
    let p = support::prepared("fn wrap(s: Str) -> Int { text.len(text.wrap(s)) }");
    let s = Value::text(&"a\n".repeat(2_000_000));
    let f = support::run_with(&p, "wrap", vec![s], Limits::default());
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_int(), Some(3_999_999));
    // The output and nothing proportional to the lines.
    assert!(f.heap_peak < 5 * 1024 * 1024, "heap peak {}", f.heap_peak);
}

#[test]
fn compiling_charges_every_node_it_walks() {
    let p = support::prepared(r#"fn main(p: Str) -> Bool { re.is_match(p, "a") }"#);
    let traversal = format!("(?:(?:{}a){{100}}){{100}}", "(?:)".repeat(990));
    let started = Instant::now();
    let f = support::run_with(&p, "main", vec![Value::text(&traversal)], Limits::default());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // The empty groups are dropped; the 10,000 copies of `a` exceed the program's size.
    let Outcome::Raised(e) = &f.outcome else {
        panic!("compiled")
    };
    assert_eq!(e.kind(&p), "Invalid");
    // A nested pattern that compiles is charged its copies, not only its characters.
    let nested = "(?:(?:(?:a|b|c|d)x){20}){20}";
    let f = support::run_with(&p, "main", vec![Value::text(nested)], Limits::default());
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert!(f.steps > 20 * 20 * 12 / 64, "charged {} steps", f.steps);
}
