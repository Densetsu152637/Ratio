//! The evaluator on small programs: values, control flow, errors, closures and the limits.

mod support;

use ratio_eval::{Limits, Outcome, Value, to_json_text};
use support::{prepared, raised, run, run_with, value};

fn json_of(source: &str, name: &str) -> String {
    let p = prepared(source);
    let v = value(&p, name, Vec::new());
    to_json_text(&p, &v)
}

#[test]
fn arithmetic_and_text() {
    let src = r#"
fn main() -> (Int, Float, Str, Bool) {
  let a = 7 / 2 + 10 % 4 * 3
  let b = 1.5 * 2.0
  let s = "a" + "b" + "{a}-{b}"
  (a, b, s, 2 < 3 and "x" < "y")
}
"#;
    assert_eq!(json_of(src, "main"), r#"[9,3.0,"ab9-3.0",true]"#);
}

#[test]
fn records_lists_and_maps() {
    let src = r#"
type Point = { x: Int, y: Int, tag?: Str }
fn main() -> ([Int], Point, Map<Str, Int>, Int) {
  let p: Point = { x: 1, y: 2 }
  let q = { ..p, x: 5 }
  var m: Map<Str, Int> = { b: 2, a: 1 }
  m["c"] = 3
  let xs = [1, 2, ..[3, 4], 5]
  (xs, q, m, xs[2] + list.len(xs))
}
"#;
    assert_eq!(
        json_of(src, "main"),
        r#"[[1,2,3,4,5],{"tag":null,"x":5,"y":2},{"a":1,"b":2,"c":3},8]"#
    );
}

#[test]
fn loops_closures_and_matches() {
    let src = r"
enum Shape { Circle { r: Int }, Square { side: Int }, Dot }
fn area(s: Shape) -> Int {
  match s {
    Circle { r } => 3 * r * r,
    Square { side } if side > 10 => -1,
    Square { side } => side * side,
    Dot => 0,
  }
}
fn main() -> [Int] {
  var total = 0
  for x in [1, 2, 3] { total += x }
  for i in 0..=2 { total += i }
  repeat 2 { total *= 2 }
  let base = 10
  let add = |x: Int| x + base
  let shapes = [Circle { r: 1 }, Square { side: 3 }, Square { side: 11 }, Shape.Dot]
  let areas = shapes.map(area)
  [total, add(1), ..areas]
}
";
    assert_eq!(json_of(src, "main"), "[36,11,3,9,-1,0]");
}

#[test]
fn errors_try_and_defer() {
    let src = r#"
fn risky(n: Int) -> Int {
  if n < 0 {
    raise fail("negative", "n was {n}")
  }
  n
}
fn guarded(n: Int) -> Int {
  try {
    risky(n)
  } catch {
    e if e.kind is Invalid => 0,
    e => -1,
  }
}
fn main() -> [Int] {
  var log = 0
  let r = try {
    defer { log += 1 }
    1 / 0
  } catch e {
    100
  }
  [guarded(3), guarded(-3), r, log]
}
"#;
    assert_eq!(json_of(src, "main"), "[3,-1,100,1]");
}

#[test]
fn budget_errors_cannot_be_caught() {
    let src = r"
fn spin() -> Int {
  try {
    loop {}
  } catch e {
    1
  }
}
fn deep(n: Int) -> Int { deep(n + 1) }
fn main() -> Int {
  try { deep(0) } catch e { 1 }
}
";
    let p = prepared(src);
    let limits = Limits {
        steps: 20_000,
        ..Limits::default()
    };
    assert_eq!(raised(&p, "spin", Vec::new(), limits), "Budget");
    assert_eq!(raised(&p, "main", Vec::new(), Limits::default()), "Budget");
    match run(&p, "main", Vec::new()).outcome {
        Outcome::Raised(e) => assert_eq!(e.budget(&p).as_deref(), Some("call_depth")),
        Outcome::Returned(_) => panic!("returned"),
    }
}

#[test]
fn steps_are_counted_exactly() {
    let p = prepared("fn main() -> Int { 1 + 2 }");
    // block, statement, binary, two literals
    assert_eq!(run(&p, "main", Vec::new()).steps, 5);
    let limits = Limits {
        steps: 4,
        ..Limits::default()
    };
    assert_eq!(raised(&p, "main", Vec::new(), limits), "Budget");
}

#[test]
fn arguments_reach_the_entry() {
    let p = prepared("fn main(a: Int, b: Str) -> Str { \"{b}{a}\" }");
    let v = value(&p, "main", vec![Value::int(4), Value::text("x")]);
    assert_eq!(v.as_text(), Some("x4"));
    let f = run_with(
        &p,
        "main",
        vec![Value::int(1), Value::text("y")],
        Limits::default(),
    );
    assert!(matches!(f.outcome, Outcome::Returned(_)));
}

#[test]
fn preparing_changes_no_hash_and_effects_carry_their_site() {
    let src = "fn main(x: Untrusted<Str>) -> Str { trust(x, because: \"reviewed\") }";
    let prog = support::program(src);
    let before = ratio_ir::canon::canonical_text(&prog, 0).unwrap();
    let hash = prog.packages[0].hash;
    let p = ratio_eval::prepare(&prog).unwrap();
    assert_eq!(
        ratio_ir::canon::canonical_text(&prog, 0).as_deref(),
        Some(before.as_str())
    );
    assert_eq!(prog.packages[0].hash, hash);
    let entry = p.find("main.ratio", "main").unwrap();
    let mut m = ratio_eval::Machine::new(
        std::sync::Arc::clone(&p),
        entry,
        vec![Value::text("t")],
        Limits::default(),
        ratio_eval::CancelToken::new(),
    )
    .unwrap();
    let ratio_eval::Suspend::Effect(req) = m.run() else {
        panic!("no effect")
    };
    assert_eq!(&*req.builtin, "trust");
    assert_eq!(req.site.0, hash);
    // The effect node's site is its IR node index.
    assert!(before.contains("(effect \"trust\""));
    m.resume_effect(Ok(Value::text("t")));
    let ratio_eval::Suspend::Done(Outcome::Returned(v)) = m.run() else {
        panic!("not done")
    };
    assert_eq!(v.as_text(), Some("t"));
}
