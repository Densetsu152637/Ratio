//! Charging and limits of the stage 8 built-ins (chapter 10, stage 8, "as stage 7"):
//! debug-build work counters stay within a fixed multiple of each built-in's charge; a built-in
//! whose input-size charge exceeds the steps left raises `Budget { which: steps }` without
//! running; a loop of N pushes or sets is charged O(N log N) steps; hostile inputs run on a
//! 256 KiB stack; long built-ins stop when cancelled; results are charged to the heap before
//! they are built.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratio_eval::{CancelToken, Limits, Outcome, Prepared, Value, work};
use support::{prepared, run, run_with, value};

/// Every case applies one built-in to prepared arguments, so a function's steps are the
/// built-in's charge plus a few steps of body.
const SOURCE: &str = r#"
fn mk_map(xs: [Int]) -> Map<Int, Int> { map.from(xs.map(|x| (x, x))) }
fn mk_pairs(xs: [Int]) -> [(Int, Int)] { xs.map(|x| (x, x)) }
fn mk_xss(xs: [Int]) -> [[Int]] { list.chunk(xs, 4) }
fn mk_json(s: Str) -> Json { json.parse(s) }

fn w_len(xs: [Int]) -> Int { list.len(xs) }
fn w_is_empty(xs: [Int]) -> Bool { list.is_empty(xs) }
fn w_get(xs: [Int]) -> Int? { list.get(xs, 77) }
fn w_first(xs: [Int]) -> Int? { list.first(xs) }
fn w_last(xs: [Int]) -> Int? { list.last(xs) }
fn w_push(xs: [Int]) -> [Int] { list.push(xs, 1) }
fn w_insert(xs: [Int]) -> [Int] { list.insert(xs, 77, 1) }
fn w_remove_at(xs: [Int]) -> [Int] { list.remove_at(xs, 77) }
fn w_concat(xs: [Int]) -> [Int] { list.concat(xs, xs) }
fn w_flatten(xss: [[Int]]) -> [Int] { list.flatten(xss) }
fn w_range(n: Int) -> [Int] { list.range(0, n) }
fn w_repeat(n: Int) -> [Int] { list.repeat(1, n) }
fn w_map(xs: [Int]) -> [Int] { list.map(xs, |x| x) }
fn w_map_indexed(xs: [Int]) -> [Int] { list.map_indexed(xs, |i, x| x) }
fn w_filter(xs: [Int]) -> [Int] { list.filter(xs, |x| true) }
fn w_filter_map(xs: [Int]) -> [Int] { list.filter_map(xs, |x| x) }
fn w_flat_map(xs: [Int]) -> [Int] { list.flat_map(xs, |x| [x]) }
fn w_fold(xs: [Int]) -> Int { list.fold(xs, 0, |a, x| a + x) }
fn w_reduce(xs: [Int]) -> Int? { list.reduce(xs, |a, x| a + x) }
fn w_find(xs: [Int]) -> Int? { list.find(xs, |x| false) }
fn w_find_index(xs: [Int]) -> Int? { list.find_index(xs, |x| false) }
fn w_index_of(xs: [Int]) -> Int? { list.index_of(xs, -1) }
fn w_contains(xs: [Int]) -> Bool { list.contains(xs, -1) }
fn w_any(xs: [Int]) -> Bool { list.any(xs, |x| false) }
fn w_all(xs: [Int]) -> Bool { list.all(xs, |x| true) }
fn w_count(xs: [Int]) -> Int { list.count(xs, |x| true) }
fn w_sum(xs: [Int]) -> Int { list.sum(xs) }
fn w_min(xs: [Int]) -> Int? { list.min(xs) }
fn w_max(xs: [Int]) -> Int? { list.max(xs) }
fn w_min_by(xs: [Int]) -> Int? { list.min_by(xs, |x| x) }
fn w_max_by(xs: [Int]) -> Int? { list.max_by(xs, |x| x) }
fn w_sort(xs: [Int]) -> [Int] { list.sort(xs) }
fn w_sort_by(xs: [Int]) -> [Int] { list.sort_by(xs, |x| x) }
fn w_reverse(xs: [Int]) -> [Int] { list.reverse(xs) }
fn w_unique(xs: [Int]) -> [Int] { list.unique(xs) }
fn w_unique_by(xs: [Int]) -> [Int] { list.unique_by(xs, |x| x % 100) }
fn w_group_by(xs: [Int]) -> Map<Int, [Int]> { list.group_by(xs, |x| x % 100) }
fn w_partition(xs: [Int]) -> ([Int], [Int]) { list.partition(xs, |x| x % 2 == 0) }
fn w_chunk(xs: [Int]) -> [[Int]] { list.chunk(xs, 4) }
fn w_window(xs: [Int]) -> [[Int]] { list.window(xs, 3) }
fn w_zip(xs: [Int]) -> [(Int, Int)] { list.zip(xs, xs) }
fn w_unzip(ps: [(Int, Int)]) -> ([Int], [Int]) { list.unzip(ps) }
fn w_enumerate(xs: [Int]) -> [(Int, Int)] { list.enumerate(xs) }
fn w_take(xs: [Int]) -> [Int] { list.take(xs, 100) }
fn w_drop(xs: [Int]) -> [Int] { list.drop(xs, 100) }
fn w_take_while(xs: [Int]) -> [Int] { list.take_while(xs, |x| true) }
fn w_slice(xs: [Int]) -> [Int] { list.slice(xs, 10, -10) }

fn w_m_from(ps: [(Int, Int)]) -> Map<Int, Int> { map.from(ps) }
fn w_m_len(m: Map<Int, Int>) -> Int { map.len(m) }
fn w_m_is_empty(m: Map<Int, Int>) -> Bool { map.is_empty(m) }
fn w_m_get(m: Map<Int, Int>) -> Int? { map.get(m, 77) }
fn w_m_has(m: Map<Int, Int>) -> Bool { map.has(m, 77) }
fn w_m_set(m: Map<Int, Int>) -> Map<Int, Int> { map.set(m, 77, 1) }
fn w_m_remove(m: Map<Int, Int>) -> Map<Int, Int> { map.remove(m, 77) }
fn w_m_update(m: Map<Int, Int>) -> Map<Int, Int> { map.update(m, 77, |v| v + 1, 0) }
fn w_m_keys(m: Map<Int, Int>) -> [Int] { map.keys(m) }
fn w_m_values(m: Map<Int, Int>) -> [Int] { map.values(m) }
fn w_m_entries(m: Map<Int, Int>) -> [(Int, Int)] { map.entries(m) }
fn w_m_merge(m: Map<Int, Int>) -> Map<Int, Int> { map.merge(m, m) }
fn w_m_map_values(m: Map<Int, Int>) -> Map<Int, Int> { map.map_values(m, |v| v) }
fn w_m_filter(m: Map<Int, Int>) -> Map<Int, Int> { map.filter(m, |k, v| true) }
fn w_m_count_by(xs: [Int]) -> Map<Int, Int> { map.count_by(xs, |x| x % 100) }

fn w_j_parse(s: Str) -> Json { json.parse(s) }
fn w_j_stringify(xs: [Int]) -> Untrusted<Str> { json.stringify(xs) }
fn w_j_stringify_pretty(xs: [Int]) -> Untrusted<Str> { json.stringify(xs, true) }
fn w_j_decode(j: Json) -> Untrusted<[Int]> { json.decode::<[Int]>(j) }
fn w_j_encode(xs: [Int]) -> Json { json.encode(xs) }
fn w_j_schema() -> Json { json.schema::<[Int]>() }
fn w_j_validate(j: Json, s: Json) -> [Violation] { json.validate(j, s) }
fn w_j_get(j: Json) -> Json? { json.get(j, "/1000") }
fn w_j_set(j: Json) -> Json { json.set(j, "/10", json.parse("1")) }
fn w_j_keys(j: Json) -> [Untrusted<Str>] { json.keys(j) }
fn w_j_type_of(j: Json) -> JsonType { json.type_of(j) }
fn w_j_merge(a: Json, b: Json) -> Json { json.merge(a, b) }
fn w_j_patch(j: Json, ops: Json) -> Json { json.patch(j, ops) }
fn w_j_diff(a: Json, b: Json) -> Json { json.diff(a, b) }
fn w_j_equal(a: Json, b: Json) -> Bool { json.equal(a, b) }
"#;

/// A permutation of `0..n` that is not in order.
fn scrambled(n: i64) -> Vec<i64> {
    (0..n).map(|i| (i * 7919 + 13) % n).collect()
}

fn ints(n: i64) -> Value {
    Value::list(scrambled(n).into_iter().map(Value::int).collect()).unwrap()
}

fn array_text(n: i64, bump: Option<i64>) -> String {
    let items: Vec<String> = (0..n)
        .map(|i| {
            if Some(i) == bump {
                "-1".to_owned()
            } else {
                i.to_string()
            }
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn object_text(n: i64, offset: i64) -> String {
    let items: Vec<String> = (0..n).map(|i| format!("\"k{i}\":{}", i + offset)).collect();
    format!("{{{}}}", items.join(","))
}

/// Every prepared argument a case may use.
struct Bundle {
    xs: Value,
    xss: Value,
    ps: Value,
    m: Value,
    ja: Value,
    ja2: Value,
    jo: Value,
    jo2: Value,
    ops: Value,
    schema: Value,
    count: Value,
}

impl Bundle {
    fn new(p: &Arc<Prepared>, n: i64) -> Bundle {
        let json = |text: String| value(p, "mk_json", vec![Value::text(&text)]);
        let xs = ints(n);
        let ops: Vec<String> = (0..20)
            .map(|i| format!("{{\"op\":\"add\",\"path\":\"/new{i}\",\"value\":{i}}}"))
            .collect();
        Bundle {
            xss: value(p, "mk_xss", vec![xs.clone()]),
            ps: value(p, "mk_pairs", vec![xs.clone()]),
            m: value(p, "mk_map", vec![xs.clone()]),
            ja: json(array_text(n, None)),
            ja2: json(array_text(n, Some(n / 2))),
            jo: json(object_text(n, 0)),
            jo2: json(object_text(n / 2, 1000)),
            ops: json(format!("[{}]", ops.join(","))),
            schema: json(r#"{"type":"array","items":{"type":"integer"}}"#.to_owned()),
            count: Value::int(n),
            xs,
        }
    }

    fn args(&self, names: &[&str]) -> Vec<Value> {
        names
            .iter()
            .map(|n| match *n {
                "xs" => self.xs.clone(),
                "xss" => self.xss.clone(),
                "ps" => self.ps.clone(),
                "m" => self.m.clone(),
                "ja" => self.ja.clone(),
                "ja2" => self.ja2.clone(),
                "jo" => self.jo.clone(),
                "jo2" => self.jo2.clone(),
                "ops" => self.ops.clone(),
                "schema" => self.schema.clone(),
                "n" => self.count.clone(),
                other => panic!("no argument `{other}`"),
            })
            .collect()
    }
}

/// Each case: the function, its arguments, and whether its charge grows with the input (so a
/// big input must be refused when the steps left cannot pay for it).
const CASES: &[(&str, &[&str], bool)] = &[
    ("w_len", &["xs"], false),
    ("w_is_empty", &["xs"], false),
    ("w_get", &["xs"], false),
    ("w_first", &["xs"], false),
    ("w_last", &["xs"], false),
    ("w_push", &["xs"], false),
    ("w_insert", &["xs"], false),
    ("w_remove_at", &["xs"], false),
    ("w_concat", &["xs"], false),
    ("w_flatten", &["xss"], true),
    ("w_range", &["n"], true),
    ("w_repeat", &["n"], true),
    ("w_map", &["xs"], true),
    ("w_map_indexed", &["xs"], true),
    ("w_filter", &["xs"], true),
    ("w_filter_map", &["xs"], true),
    ("w_flat_map", &["xs"], true),
    ("w_fold", &["xs"], true),
    ("w_reduce", &["xs"], true),
    ("w_find", &["xs"], true),
    ("w_find_index", &["xs"], true),
    ("w_index_of", &["xs"], true),
    ("w_contains", &["xs"], true),
    ("w_any", &["xs"], true),
    ("w_all", &["xs"], true),
    ("w_count", &["xs"], true),
    ("w_sum", &["xs"], true),
    ("w_min", &["xs"], true),
    ("w_max", &["xs"], true),
    ("w_min_by", &["xs"], true),
    ("w_max_by", &["xs"], true),
    ("w_sort", &["xs"], true),
    ("w_sort_by", &["xs"], true),
    ("w_reverse", &["xs"], true),
    ("w_unique", &["xs"], true),
    ("w_unique_by", &["xs"], true),
    ("w_group_by", &["xs"], true),
    ("w_partition", &["xs"], true),
    ("w_chunk", &["xs"], true),
    ("w_window", &["xs"], true),
    ("w_zip", &["xs"], true),
    ("w_unzip", &["ps"], true),
    ("w_enumerate", &["xs"], true),
    ("w_take", &["xs"], false),
    ("w_drop", &["xs"], false),
    ("w_take_while", &["xs"], true),
    ("w_slice", &["xs"], false),
    ("w_m_from", &["ps"], true),
    ("w_m_len", &["m"], false),
    ("w_m_is_empty", &["m"], false),
    ("w_m_get", &["m"], false),
    ("w_m_has", &["m"], false),
    ("w_m_set", &["m"], false),
    ("w_m_remove", &["m"], false),
    ("w_m_update", &["m"], false),
    ("w_m_keys", &["m"], true),
    ("w_m_values", &["m"], true),
    ("w_m_entries", &["m"], true),
    ("w_m_merge", &["m"], true),
    ("w_m_map_values", &["m"], true),
    ("w_m_filter", &["m"], true),
    ("w_m_count_by", &["xs"], true),
    ("w_j_parse", &[], false),
    ("w_j_stringify", &["xs"], true),
    ("w_j_stringify_pretty", &["xs"], true),
    ("w_j_decode", &["ja"], true),
    ("w_j_encode", &["xs"], true),
    ("w_j_schema", &[], false),
    ("w_j_validate", &["ja", "schema"], true),
    ("w_j_get", &["ja"], true),
    ("w_j_set", &["ja"], true),
    ("w_j_keys", &["jo"], true),
    ("w_j_type_of", &["jo"], false),
    ("w_j_merge", &["jo", "jo2"], true),
    ("w_j_patch", &["jo", "ops"], true),
    ("w_j_diff", &["ja", "ja2"], true),
    ("w_j_equal", &["ja", "ja2"], true),
];

/// The most work units a built-in may do per unit its charge pays for (a step pays for 64).
const FACTOR: u64 = 8;

#[test]
fn the_counted_work_stays_within_a_multiple_of_the_charge() {
    if !work::ENABLED {
        return;
    }
    let p = prepared(SOURCE);
    let mut ratios: Vec<(f64, String)> = Vec::new();
    for n in [100, 2000] {
        let b = Bundle::new(&p, n);
        for (name, args, _) in CASES {
            let args = if *name == "w_j_parse" {
                vec![Value::text(&array_text(n, None))]
            } else {
                b.args(args)
            };
            work::reset();
            let f = run(&p, name, args);
            let done = work::total();
            assert!(
                matches!(f.outcome, Outcome::Returned(_)),
                "{name}: raised at n = {n}"
            );
            let allowed = FACTOR * 64 * f.steps;
            #[allow(clippy::cast_precision_loss, reason = "a ratio for the report")]
            let ratio = done as f64 / (64 * f.steps) as f64;
            ratios.push((ratio, format!("{name}@{n}")));
            assert!(
                done <= allowed,
                "{name} at n = {n}: {done} units of work for {} steps (at most {allowed})",
                f.steps
            );
        }
    }
    ratios.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (r, name) in ratios.iter().take(8) {
        eprintln!("work / charge {r:.2}  {name}");
    }
}

#[test]
fn the_counters_do_count() {
    if !work::ENABLED {
        return;
    }
    let p = prepared(SOURCE);
    work::reset();
    let _ = run(&p, "w_sort", vec![ints(2000)]);
    assert!(work::total() >= 2000, "{}", work::total());
}

/// A built-in whose input-size charge exceeds the steps left raises `Budget { which: steps }`
/// without running: the body is all that is charged, and no work is counted.
#[test]
fn a_charge_the_steps_left_cannot_pay_raises_budget_without_running() {
    let p = prepared(SOURCE);
    let n = 200_000;
    let b = Bundle::new(&p, n);
    let limits = Limits {
        steps: 1500,
        ..Limits::default()
    };
    let start = Instant::now();
    for (name, args, grows) in CASES {
        if !grows || *name == "w_j_parse" {
            continue;
        }
        let args = if *name == "w_range" || *name == "w_repeat" {
            vec![Value::int(1_000_000)]
        } else {
            b.args(args)
        };
        work::reset();
        let f = run_with(&p, name, args, limits);
        let Outcome::Raised(e) = &f.outcome else {
            panic!("{name} ran")
        };
        assert_eq!(e.budget(&p).as_deref(), Some("steps"), "{name}");
        assert!(f.steps <= 1500, "{name}: {} steps", f.steps);
        if work::ENABLED {
            assert!(
                work::total() < 1000,
                "{name} did {} units of work",
                work::total()
            );
        }
    }
    // Refusing all of them took no real time.
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

/// The product-class JSON built-ins (`validate`, `diff`, `patch`) are refused before running
/// when the product of the two sizes is more than the steps left can pay for.
#[test]
fn product_charges_are_debited_before_running() {
    let p = prepared(SOURCE);
    let b = Bundle::new(&p, 3000);
    let limits = Limits {
        steps: 20_000,
        ..Limits::default()
    };
    for (name, args) in [
        ("w_j_diff", vec!["ja", "ja2"]),
        ("w_j_validate", vec!["ja", "schema"]),
    ] {
        let f = run_with(&p, name, b.args(&args), limits);
        // 3,000 × 3,000 / 64 = 140,000 steps for `diff`; `validate` against a small schema
        // is paid for.
        match (name, &f.outcome) {
            ("w_j_diff", Outcome::Raised(e)) => {
                assert_eq!(e.budget(&p).as_deref(), Some("steps"));
            }
            ("w_j_validate", Outcome::Returned(_)) => {}
            (n, o) => panic!("{n}: {o:?}"),
        }
    }
}

const LOOPS: &str = r"
fn pushes(n: Int) -> Int {
  var xs: [Int] = []
  var i = 0
  while i < n {
    xs = list.push(xs, i)
    i += 1
  }
  xs.len()
}
fn sets(n: Int) -> Int {
  let blank: [(Int, Int)] = []
  var m = map.from(blank)
  var i = 0
  while i < n {
    m = map.set(m, i, i)
    i += 1
  }
  m.len()
}
fn text_sets(n: Int) -> Int {
  var m: Map<Str, Int> = {}
  var i = 0
  while i < n {
    m = map.set(m, 'k{i}', i)
    i += 1
  }
  m.len()
}
";

/// A loop of N pushes or sets is charged O(N log N) steps (here, with the loop's own steps,
/// at most 12 per iteration plus the `⌈log2⌉` factor the `log` class has per step of 64), and
/// the work it does is within the charge.
#[test]
fn a_loop_of_pushes_or_sets_is_charged_n_log_n() {
    let p = prepared(&LOOPS.replace('\'', "\""));
    for name in ["pushes", "sets", "text_sets"] {
        let mut last = 0u64;
        for n in [1000u64, 10_000, 100_000] {
            work::reset();
            let f = run(&p, name, vec![Value::int(i64::try_from(n).unwrap())]);
            assert!(matches!(f.outcome, Outcome::Returned(_)), "{name} {n}");
            let log = u64::from(64 - n.leading_zeros());
            assert!(
                f.steps <= 16 * n * log,
                "{name}: {} steps for {n} (limit {})",
                f.steps,
                16 * n * log
            );
            assert!(f.steps > last, "{name}: charges grow with n");
            last = f.steps;
            if work::ENABLED {
                assert!(
                    work::total() <= FACTOR * 64 * f.steps,
                    "{name} at {n}: {} units of work for {} steps",
                    work::total(),
                    f.steps
                );
            }
        }
    }
}

/// A map key's weight multiplies the comparisons of a set or lookup: long keys are charged
/// more, so a loop of sets with long keys still pays for the work.
#[test]
fn long_map_keys_are_charged_by_their_weight() {
    let p = prepared(
        r"
fn setting(key: Str, n: Int) -> Int {
  var m: Map<Str, Int> = {}
  var i = 0
  while i < n {
    m = map.set(m, key, i)
    i += 1
  }
  m.len()
}
",
    );
    let short = run(&p, "setting", vec![Value::text("k"), Value::int(2000)]);
    let long = run(
        &p,
        "setting",
        vec![Value::text(&"k".repeat(6400)), Value::int(2000)],
    );
    // Same map size (one key), but each set walks a 6,400-character key: 1 step per 64 more.
    assert!(
        long.steps > short.steps + 2000 * 50,
        "{} {}",
        long.steps,
        short.steps
    );
}

// ---- hostile inputs on a small stack ----

const HOSTILE: &str = r##"
fn nested(depth: Int) -> Json { json.parse(text.repeat("[", depth) + text.repeat("]", depth)) }
fn deepest(depth: Int) -> Json {
  let leaf = json.parse(text.repeat("[", 120) + text.repeat("]", 120))
  json.set(nested(120), text.repeat("/0", 119), leaf)
}
fn ops(depth: Int) -> Json {
  let d = deepest(depth)
  let other = json.set(d, text.repeat("/0", 239), json.parse("1"))
  let m = json.merge(d, other)
  let patch = json.diff(d, other)
  let applied = json.patch(d, patch)
  let schema = json.parse(r"""{"type":"array","items":{"$ref":"#"}} """)
  let violations = json.validate(nested(80), schema)
  let text = json.stringify(d)
  json.encode((json.equal(m, other), json.equal(applied, other), violations.len(), text.len()))
}
fn sorted(n: Int) -> Int {
  let xs = list.repeat(json.parse("[[[[[[[1]]]]]]]"), n)
  let ys = xs.sort()
  let zs = ys.unique()
  zs.len() + list.group_by(xs, |x| x).len()
}
"##;

#[test]
fn hostile_nesting_runs_on_a_small_stack() {
    let p = prepared(HOSTILE);
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let a = run(&p, "ops", vec![Value::int(120)]);
            let b = run(&p, "sorted", vec![Value::int(50)]);
            (a.outcome, b.outcome)
        })
        .unwrap();
    let (a, b) = worker.join().expect("no stack overflow");
    match a {
        Outcome::Returned(v) => {
            assert_eq!(v.scan(), ratio_eval::Labels::NONE, "no label was invented");
        }
        Outcome::Raised(e) => panic!(
            "`ops` raised {} ({:?})",
            e.kind(&prepared(HOSTILE)),
            e.message()
        ),
    }
    assert!(matches!(b, Outcome::Returned(_)));
}

// ---- cancellation and the heap ----

#[test]
fn a_long_built_in_stops_when_cancelled() {
    let p = prepared(SOURCE);
    for (name, arg) in [("w_sort", "xs"), ("w_unique", "xs"), ("w_m_from", "ps")] {
        let big = ints(400_000);
        let args = match arg {
            "xs" => vec![big],
            _ => vec![value(&p, "mk_pairs", vec![big])],
        };
        let cancel = CancelToken::new();
        cancel.cancel();
        let entry = p.find("main.ratio", name).unwrap();
        let start = Instant::now();
        let eval =
            ratio_eval::evaluate(Arc::clone(&p), entry, args, Limits::default(), cancel).unwrap();
        let f = ratio_eval::block_on(eval);
        let Outcome::Raised(e) = f.outcome else {
            panic!("{name} finished")
        };
        assert_eq!(e.kind(&p), "Cancelled", "{name}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{name} took {:?}",
            start.elapsed()
        );
    }
}

#[test]
fn results_are_charged_to_the_heap_before_they_are_built() {
    let p = prepared(SOURCE);
    // A million-element list does not fit in 4 MiB: it is refused (`Budget { which: heap }`)
    // before the buffer is made.
    let limits = Limits {
        heap: 4 * 1024 * 1024,
        ..Limits::default()
    };
    for (name, args) in [
        ("w_range", vec![Value::int(1_000_000)]),
        ("w_repeat", vec![Value::int(1_000_000)]),
    ] {
        let f = run_with(&p, name, args, limits);
        let Outcome::Raised(e) = f.outcome else {
            panic!("{name} fit")
        };
        assert_eq!(e.budget(&p).as_deref(), Some("heap"), "{name}");
        assert!(f.heap_peak <= limits.heap, "{name}: {}", f.heap_peak);
    }
}
