//! Regression tests for the stage 6 security review (round 1): label laundering, work and
//! memory beyond what is charged, deep drops, the branch restriction inside `race`, and the
//! smaller findings. Each program is the review's reproduction, or a scaled-down one.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratio_eval::{ErrorClass, Limits, Outcome, Value};
use support::{SECRET, prepared, run, run_with, secret, value};

fn returned(p: &Arc<ratio_eval::Prepared>, name: &str, args: Vec<Value>) -> Value {
    value(p, name, args)
}

/// What `main` raised, and how long it took.
fn raised_in(
    p: &Arc<ratio_eval::Prepared>,
    args: Vec<Value>,
    limits: Limits,
) -> (String, Duration, u64) {
    let start = Instant::now();
    let f = run_with(p, "main", args, limits);
    let took = start.elapsed();
    match f.outcome {
        Outcome::Raised(e) => (e.kind(p), took, f.heap_peak),
        Outcome::Returned(v) => panic!("returned {}", ratio_eval::to_json_text(p, &v)),
    }
}

// ---- F1, F2 and their kin: no laundering ----

#[test]
fn f1_a_fail_tag_keeps_its_labels() {
    let p = prepared(
        r#"
fn main(s: Str) -> Str {
  try { raise fail(s, "m") } catch e {
    match e.kind {
      User { tag } => tag,
      _ => "other",
    }
  }
}
fn through_result(s: Str) -> Str {
  match result(|| { raise fail(s, "m"); 1 }) {
    Err { error } => match error.kind {
      User { tag } => tag,
      _ => "other",
    },
    Ok { .. } => "ok",
  }
}
"#,
    );
    assert_eq!(returned(&p, "main", vec![secret("t")]).scan(), SECRET);
    assert_eq!(
        returned(&p, "through_result", vec![secret("t")]).scan(),
        SECRET
    );
}

#[test]
fn f2_the_variant_json_decode_picks_carries_the_kind_labels() {
    let p = prepared(
        r"
enum Color { Red { x: Int }, Blue { x: Int } }
type W = { kind: Str, x: Int }
fn main(s: Str) -> Bool {
  let w: W = { kind: s, x: 1 }
  let c = json.decode::<Color>(json.encode::<W>(w))
  c is Red
}
",
    );
    assert_eq!(returned(&p, "main", vec![secret("red")]).scan(), SECRET);
}

#[test]
fn f2_json_object_lookups_carry_the_key_labels() {
    let p = prepared(
        r#"
fn indexed(s: Str) -> Bool {
  let m: Map<Str, Int> = {}
  let j = json.encode::<Map<Str, Int>>(m.set(s, 1))
  j["red"] != none
}
fn pointed(s: Str) -> Bool {
  let m: Map<Str, Int> = {}
  let j = json.encode::<Map<Str, Int>>(m.set(s, 1))
  json.get(j, "/red") != none
}
fn field(s: Str) -> Int {
  let m: Map<Str, Int> = {}
  let j = json.encode::<Map<Str, Int>>(m.set(s, 1))
  json.decode::<{ red: Int }>(j).red
}
"#,
    );
    for f in ["indexed", "pointed"] {
        // A hit and a miss both depend on the secret key.
        assert_eq!(
            returned(&p, f, vec![secret("red")]).scan(),
            SECRET,
            "{f} hit"
        );
        assert_eq!(
            returned(&p, f, vec![secret("blue")]).scan(),
            SECRET,
            "{f} miss"
        );
    }
    assert_eq!(returned(&p, "field", vec![secret("red")]).scan(), SECRET);
}

// ---- F3 to F7: charged before the work, and memory on the heap ----

#[test]
fn f3_rendering_stops_at_the_string_limit() {
    let name = "a".repeat(1000);
    let src = format!(
        "type R = {{ {name}: Int }}\nfn main() -> Int {{\n  let r: R = {{ {name}: 1 }}\n  var xs = [r]\n  repeat 17 {{ xs = xs + xs }}\n  let t = text.show(xs)\n  text.len(t)\n}}\n"
    );
    let p = prepared(&src);
    let (kind, took, peak) = raised_in(&p, Vec::new(), Limits::default());
    // 131,072 records of a 1,000-character field would show as 132 MB: the writer stops after
    // 16 MiB and the text is refused; the field names count towards the charge.
    assert!(kind == "Invalid" || kind == "Budget", "{kind}");
    assert!(took < Duration::from_secs(20), "{took:?}");
    assert!(peak < 64 * 1024 * 1024);
}

#[test]
fn f4_prompt_sections_are_charged_as_they_are_built() {
    let p = prepared(
        r#"
fn main() -> Int {
  let p = prompt { for i in 0..100000 { note "x" } }
  let q = prompt { for i in 0..2000 { include p } }
  1
}
"#,
    );
    let (kind, took, peak) = raised_in(&p, Vec::new(), Limits::default());
    assert_eq!(kind, "Budget");
    assert!(took < Duration::from_secs(30), "{took:?}");
    assert!(peak <= 64 * 1024 * 1024);
}

#[test]
fn f5_decoding_builds_no_path_per_element() {
    let p = prepared(
        r#"
fn main() -> Int {
  var k = "a"
  repeat 20 { k = k + k }
  var arr = "0"
  repeat 14 { arr = arr + "," + arr }
  let text = "\{\"" + k + "\": [" + arr + "]\}"
  let m = json.decode::<Map<Str, [Int]>>(json.parse(text))
  list.len(m[k])
}
fn bad() -> Int {
  var k = "a"
  repeat 20 { k = k + k }
  let text = "\{\"" + k + "\": [1, \"x\"]\}"
  let m = json.decode::<Map<Str, [Int]>>(json.parse(text))
  1
}
"#,
    );
    let start = Instant::now();
    assert_eq!(returned(&p, "main", Vec::new()).as_int(), Some(16384));
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    // A violation names the path, with a long key cut short.
    match run(&p, "bad", Vec::new()).outcome {
        Outcome::Raised(e) => {
            assert_eq!(e.kind(&p), "SchemaMismatch");
            let m = e.message().unwrap();
            assert!(m.len() < 1000, "{} bytes", m.len());
            assert!(m.contains("…/1"), "{m}");
        }
        Outcome::Returned(_) => panic!("returned"),
    }
}

#[test]
fn f6_map_key_comparisons_are_charged_by_the_key() {
    let p = prepared(
        r#"
fn main() -> Int {
  var k = "a"
  repeat 23 { k = k + k }
  let m: Map<Str, Int> = {}
  let m2 = m.set(k + "b", 1).set(k + "c", 2)
  let probe = k + "d"
  var hits = 0
  repeat 20000 {
    if map.get(m2, probe) != none { hits += 1 }
  }
  hits
}
"#,
    );
    let (kind, took, _) = raised_in(&p, Vec::new(), Limits::default());
    assert_eq!(kind, "Budget");
    assert!(took < Duration::from_secs(20), "{took:?}");
    // The operators that look keys up are charged the same way.
    let q = prepared(
        r"
fn main(k: Str) -> Int {
  var m: Map<Str, Int> = { a: 1 }
  m[k] = 2
  if k in m { m[k] } else { 0 }
}
",
    );
    let short = run(&q, "main", vec![Value::text("k")]).steps;
    let long = run(&q, "main", vec![Value::text(&"k".repeat(64_000))]).steps;
    assert!(long > short + 3 * 1000, "{short} {long}");
}

#[test]
fn f6_sort_keys_are_charged_by_their_weight() {
    let p = prepared(
        r#"
fn main(k: Str) -> [Str] {
  ["a", "b", "c", "d"].sort_by(|x| k + x)
}
"#,
    );
    let short = run(&p, "main", vec![Value::text("k")]).steps;
    let long = run(&p, "main", vec![Value::text(&"k".repeat(64_000))]).steps;
    // Four 64,000-character keys, three comparisons each: about 4 × 64,000 × 3 / 64 steps
    // for the comparisons, beyond building the keys.
    assert!(long > short + 3 * 4 * 1000, "{short} {long}");
}

#[test]
fn f7_built_in_buffers_are_charged_to_the_heap() {
    let p = prepared(
        r"
fn deep(xs: [Int], d: Int) -> Int {
  if d == 0 { return 0 }
  let ys = xs.sort_by(|x| deep(xs, d - 1))
  1
}
fn main() -> Int {
  var xs = [1]
  repeat 19 { xs = xs + xs }
  deep(xs, 40)
}
fn mapped() -> Int {
  var xs = [1]
  repeat 19 { xs = xs + xs }
  list.len(xs.map(|x| x + 1))
}
",
    );
    let (kind, _, peak) = raised_in(&p, Vec::new(), Limits::default());
    assert_eq!(kind, "Budget");
    assert!(peak <= 64 * 1024 * 1024);
    // The buffer a `map` gathers into counts while it is built.
    let small = Limits {
        heap: 4 * 1024 * 1024,
        ..Limits::default()
    };
    match run_with(&p, "mapped", Vec::new(), small).outcome {
        Outcome::Raised(e) => assert_eq!(e.budget(&p).as_deref(), Some("heap")),
        Outcome::Returned(_) => panic!("524,288 results fit in 4 MiB"),
    }
}

// ---- F8: drops never recurse ----

#[test]
fn f8_dropping_deep_values_fits_a_small_stack() {
    let p = prepared(
        r"
fn main() -> Int {
  var f = || 0
  repeat 120 {
    let g = f
    var xs = [g]
    repeat 14 { xs = xs + xs }
    f = || list.len(xs)
  }
  f()
}
",
    );
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let f = run(&p, "main", Vec::new());
            match f.outcome {
                Outcome::Returned(v) => v.as_int(),
                Outcome::Raised(_) => None,
            }
        })
        .unwrap();
    assert_eq!(worker.join().expect("no stack overflow"), Some(16384));
}

// ---- the branch restriction is fatal ----

#[test]
fn a_branch_restriction_violation_is_fatal_even_in_a_race() {
    let p = prepared(
        r"
fn main() -> Int {
  var n = 0
  let r = race {
    { n = 1; 1 };
    2
  }
  n
}
fn caught() -> Int {
  var n = 0
  try {
    par { { for i in 0..3 { n += i }; 1 }; 2 }
    0
  } catch e { 7 }
}
fn in_result() -> Bool {
  let r = result(|| {
    var n = 0
    par { { n = 5; 1 }; 2 }
    0
  })
  r is Err
}
fn indexed() -> [Int] {
  var xs = [1]
  par { { xs[0] = 7; 1 }; 2 }
  xs
}
",
    );
    for f in ["main", "caught", "in_result", "indexed"] {
        match run(&p, f, Vec::new()).outcome {
            Outcome::Raised(e) => {
                assert_eq!(e.kind(&p), "Invalid", "{f}");
                assert_eq!(e.class(), ErrorClass::Fatal, "{f}");
            }
            Outcome::Returned(v) => panic!("{f} returned {}", ratio_eval::to_json_text(&p, &v)),
        }
    }
}

// ---- smaller findings ----

#[test]
fn a_json_unicode_escape_takes_exactly_four_hex_digits() {
    let p = prepared(
        r#"
fn signed() -> Json { json.parse("\"\\u+041\"") }
fn plain() -> Json { json.parse("\"\\u0041\"") }
"#,
    );
    assert!(matches!(
        run(&p, "plain", Vec::new()).outcome,
        Outcome::Returned(_)
    ));
    match run(&p, "signed", Vec::new()).outcome {
        Outcome::Raised(e) => assert_eq!(e.kind(&p), "Invalid"),
        Outcome::Returned(_) => panic!("a signed escape was accepted"),
    }
}

#[test]
fn preparing_and_writing_the_form_have_explicit_depth_bounds() {
    // The hand-built tree is deep: build, clone and drop it on a large stack.
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(deep_tree_is_refused)
        .unwrap()
        .join()
        .unwrap();
}

fn deep_tree_is_refused() {
    use ratio_ir::{Expr, ExprKind, ItemKind, Loc, UnOp};
    let mut prog = support::program("fn main() -> Int { 0 }");
    let mut e = Expr {
        kind: ExprKind::Lit(ratio_ir::Lit::Int(1)),
        loc: Loc::default(),
        id: 0,
    };
    for _ in 0..5000 {
        e = Expr {
            kind: ExprKind::Unary(UnOp::Neg, Box::new(e)),
            loc: Loc::default(),
            id: 0,
        };
    }
    for item in prog.modules[0].items.iter_mut().flatten() {
        if let ItemKind::Fn(f) = &mut item.kind {
            f.body = e.clone();
        }
    }
    assert_eq!(
        ratio_eval::prepare(&prog).unwrap_err(),
        ratio_eval::PrepareError::TooDeep
    );
    assert!(ratio_ir::canon::canonical_text(&prog, 0).is_none());
}
