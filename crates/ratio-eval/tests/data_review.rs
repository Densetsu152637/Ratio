//! Regressions from the stage 8 security review: work bounded by the charge for `json.validate`
//! (shared sub-schemas), labels of `json.patch` `copy`/`move` and of composite map keys, errors
//! that carry the instance's labels, record `==` that is linear, cheap small calls, and a failing
//! `json.schema` that is charged for its work.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "test programs name values briefly"
)]

mod support;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use ratio_eval::{Labels, Limits, Outcome, Value};
use support::{SECRET, prepared, run, run_logged, run_with};

fn scan(f: &ratio_eval::Finished) -> Labels {
    match &f.outcome {
        Outcome::Returned(v) => v.scan(),
        Outcome::Raised(e) => e.value().scan(),
    }
}

#[test]
fn validate_through_shared_schemas_is_bounded_by_its_charge() {
    let p = prepared(
        r"fn v(j: Str, s: Str) -> Int { json.validate(json.parse(j), json.parse(s)).len() }",
    );
    let chain = |n: u32| {
        let mut defs: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    r##""a{i}":{{"allOf":[{{"$ref":"#/$defs/a{j}"}},{{"$ref":"#/$defs/a{j}"}}]}}"##,
                    j = i + 1
                )
            })
            .collect();
        defs.push(format!(r#""a{n}":{{}}"#));
        format!(
            r##"{{"$ref":"#/$defs/a0","$defs":{{{}}}}}"##,
            defs.join(",")
        )
    };
    // 2^20 visits of a few-hundred-node schema: refused, quickly, and charged what it was paid.
    let start = Instant::now();
    let f = run(&p, "v", vec![Value::text("1"), Value::text(&chain(20))]);
    let Outcome::Raised(e) = &f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert!(f.steps < 200, "{} steps", f.steps);
    // A small chain is validated.
    let f = run(&p, "v", vec![Value::text("1"), Value::text(&chain(3))]);
    assert!(matches!(f.outcome, Outcome::Returned(_)));
}

#[test]
fn patch_copy_and_move_carry_the_labels_of_the_from_path() {
    let p = prepared(
        r#"
fn control(xs: [Int]) -> Json? {
  let d = json.set(json.parse(r"{}"), "/x", json.encode(xs))
  json.get(d, "/x/0")
}
fn copied(xs: [Int]) -> Json? {
  let d = json.set(json.parse(r"{}"), "/x", json.encode(xs))
  let r = json.patch(d, json.parse(r"""[{"op":"copy","from":"/x/0","path":"/y"}] """))
  json.get(r, "/y")
}
fn moved(xs: [Int]) -> Json? {
  let d = json.set(json.parse(r"{}"), "/x", json.encode(xs))
  let r = json.patch(d, json.parse(r"""[{"op":"move","from":"/x/0","path":"/y"}] """))
  json.get(r, "/y")
}
fn moved_whole(xs: [Int]) -> Json {
  let d = json.set(json.parse(r"{}"), "/x", json.encode(xs))
  json.patch(d, json.parse(r"""[{"op":"move","from":"/x/0","path":"/y"},{"op":"remove","path":"/x"}] """))
}
"#,
    );
    let xs = || {
        Value::list(vec![Value::int(42)])
            .unwrap()
            .with_labels(SECRET)
    };
    for name in ["control", "copied", "moved", "moved_whole"] {
        let f = run(&p, name, vec![xs()]);
        assert!(scan(&f).contains(SECRET), "{name} dropped the label");
    }
}

#[test]
fn composite_map_keys_carry_their_interior_labels() {
    let p = prepared(
        r"
fn has_(s: Int) -> Bool { map.has(map.from([((5, 0), 1)]), (s, 0)) }
fn get_(s: Int) -> Int? { map.get(map.from([((5, 0), 1)]), (s, 0)) }
fn len_from(s: Int) -> Int { map.len(map.from([((s, 0), 1), ((5, 0), 2)])) }
fn len_merge(s: Int) -> Int { map.len(map.merge(map.from([((s, 0), 1)]), map.from([((5, 0), 2)]))) }
fn len_set(s: Int) -> Int { map.len(map.set(map.from([((5, 0), 1)]), (s, 0), 2)) }
fn len_remove(s: Int) -> Int { map.len(map.remove(map.from([((5, 0), 1)]), (s, 0))) }
fn len_unique(s: Int) -> Int { list.len(list.unique([(s, 0), (5, 0)])) }
fn keys_of(s: Int) -> [(Int, Int)] { map.keys(map.from([((s, 0), 1), ((5, 0), 2)])) }
fn filtered(s: Int) -> Int { map.len(map.filter(map.from([((s, 0), 1), ((5, 0), 2)]), |k, v| v > 1)) }
fn mapped(s: Int) -> Int { map.len(map.map_values(map.from([((s, 0), 1)]), |v| v)) }
",
    );
    for name in [
        "has_",
        "get_",
        "len_from",
        "len_merge",
        "len_set",
        "len_remove",
        "len_unique",
        "keys_of",
        "filtered",
        "mapped",
    ] {
        let f = run(&p, name, vec![Value::int(5).with_labels(SECRET)]);
        assert!(scan(&f).contains(SECRET), "{name} dropped the label");
    }
}

#[test]
fn validate_errors_carry_the_instances_labels() {
    let p = prepared(
        r##"
fn verr(s: Str) -> Untrusted<Str> {
  try {
    let n = json.validate(json.parse(s), json.parse(r"""{"minimum":"x"} """)).len()
    "no error"
  } catch e {
    e.message
  }
}
fn deep(s: Str) -> Int { json.validate(json.parse(s), json.parse(r"""{"items":{"$ref":"#"}} """)).len() }
fn negative_multiple() -> Int { json.validate(json.parse("4"), json.parse(r"""{"multipleOf":-2} """)).len() }
"##,
    );
    let f = run(&p, "verr", vec![Value::text("5").with_labels(SECRET)]);
    assert!(scan(&f).contains(SECRET));
    // A recursive schema reaches as deep as `json.parse` does (128).
    let nested = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let f = run(&p, "deep", vec![Value::text(&nested(120))]);
    assert!(matches!(f.outcome, Outcome::Returned(_)), "120 deep");
    let f = run(&p, "negative_multiple", vec![]);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Invalid");
}

#[test]
fn small_validations_do_not_start_threads() {
    let p = prepared(
        r#"
fn many(n: Int) -> Int { list.range(0, n).fold(0, |a, i| a + json.validate(json.parse("1"), json.parse("true")).len()) }
fn many_eq(n: Int) -> Int { list.range(0, n).fold(0, |a, i| if json.equal(json.parse("1"), json.parse("true")) { a } else { a + 1 }) }
fn big(j: Json, s: Json) -> Int { json.validate(j, s).len() }
"#,
    );
    let t = Instant::now();
    let a = run(&p, "many", vec![Value::int(20_000)]);
    let validating = t.elapsed();
    let t = Instant::now();
    let b = run(&p, "many_eq", vec![Value::int(20_000)]);
    let comparing = t.elapsed();
    assert!(matches!(a.outcome, Outcome::Returned(_)) && matches!(b.outcome, Outcome::Returned(_)));
    assert!(
        validating < comparing * 8 + Duration::from_millis(200),
        "validate {validating:?} against equal {comparing:?}"
    );
    // A walk that restarts on the large stack still counts its work for the caller.
    if cfg!(debug_assertions) {
        let nested = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        let parse = prepared("fn j(s: Str) -> Json { json.parse(s) }");
        let doc = support::value(&parse, "j", vec![Value::text(&nested(60))]);
        let schema = support::value(
            &parse,
            "j",
            vec![Value::text(r##"{"items":{"$ref":"#"}}"##)],
        );
        let (f, log) = run_logged(&p, "big", vec![doc, schema], Limits::default());
        assert!(matches!(f.outcome, Outcome::Returned(_)));
        let w = log
            .iter()
            .find(|w| w.name == "json.validate")
            .expect("a validate");
        assert!(w.work >= 60, "{}", w.work);
    }
}

#[test]
fn nested_records_compare_in_linear_time() {
    let p = prepared(
        r"
enum Tree { Leaf, Node { kids: [Tree] } }
fn build(n: Int) -> Tree { list.range(0, n).fold(Tree.Leaf, |t, i| Tree.Node { kids: [t] }) }
fn e(n: Int) -> Bool { let a = build(n); let b = build(n); a == b }
fn ix(n: Int) -> Int? { let a = build(n); list.index_of([a], build(n)) }
",
    );
    let start = Instant::now();
    let f = run(&p, "e", vec![Value::int(60)]);
    assert!(matches!(&f.outcome, Outcome::Returned(v) if v.as_bool() == Some(true)));
    let f = run(&p, "ix", vec![Value::int(60)]);
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_failing_schema_derivation_is_charged_for_its_work() {
    let mut src = String::new();
    let levels = 11;
    for i in 0..levels {
        let _ = writeln!(
            src,
            "type T{i} = {{ a: T{j}, b: T{j}, c: T{j}, d: T{j} }}",
            j = i + 1
        );
    }
    let _ = writeln!(src, "type T{levels} = {{ z: Int }}");
    src.push_str(
        "fn s() -> Bool { try { let j = json.schema::<T0>(); true } catch e { false } }\n",
    );
    let p = prepared(&src);
    let f = run_with(&p, "s", Vec::new(), Limits::default());
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert!(
        f.steps > 10_000,
        "only {} steps for the whole derivation",
        f.steps
    );
}

/// Violations of `schema` by the JSON text `instance`, or the error kind.
fn violations(
    p: &std::sync::Arc<ratio_eval::Prepared>,
    instance: &str,
    schema: &str,
) -> Result<i64, String> {
    let f = run(p, "v", vec![Value::text(instance), Value::text(schema)]);
    match f.outcome {
        Outcome::Returned(v) => Ok(v.as_int().unwrap()),
        Outcome::Raised(e) => Err(e.kind(p)),
    }
}

/// `pattern` reads as ECMAScript reads it where the `re` syntax differs, or is refused.
#[test]
fn patterns_mean_what_ecmascript_means_or_are_refused() {
    let p = prepared(
        r"fn v(j: Str, s: Str) -> Int { json.validate(json.parse(j), json.parse(s)).len() }",
    );
    let pat = |pattern: &str| format!("{{\"pattern\":\"{pattern}\"}}");
    let cases: [(&str, &str, Result<i64, &str>); 12] = [
        // `.` stops at every line terminator.
        ("\"a\\rb\"", "^a.b$", Ok(1)),
        ("\"a\\u2028b\"", "^a.b$", Ok(1)),
        ("\"axb\"", "^a.b$", Ok(0)),
        // `\d` and `\w` are ASCII; `\s` is ECMAScript's (U+FEFF in, U+0085 out).
        ("\"\\u0663\"", "^\\\\d$", Ok(1)),
        ("\"\\u00e9\"", "^\\\\w$", Ok(1)),
        ("\"\\ufeff\"", "^\\\\s$", Ok(0)),
        ("\"\\u0085\"", "^\\\\s$", Ok(1)),
        // Refused rather than read differently.
        ("\"a\"", "\\\\ba", Err("Invalid")),
        ("\"x\"", "[]x]", Err("Invalid")),
        ("\"x\"", "[^]", Err("Invalid")),
        // A character outside the basic plane is one character, as ECMAScript's `u` flag reads it.
        ("\"\\ud83d\\ude00\"", "^.$", Ok(0)),
        ("\"a\"", "(?=a)a", Err("Invalid")),
    ];
    for (instance, pattern, want) in cases {
        let got = violations(&p, instance, &pat(pattern));
        let want = want.map_err(str::to_owned);
        assert_eq!(got, want, "{instance} against {pattern}");
    }
}

/// A pattern of repeated wide classes is compiled by the linear engine, which charges its
/// program (the old matcher copied a class 4,000 times for 130 MB), and a long text against it
/// is charged by its length times the program, before it runs.
#[test]
fn a_wide_repeated_class_is_charged_and_bounded() {
    let p = prepared(
        r"fn v(j: Str, s: Str) -> Int { json.validate(json.parse(j), json.parse(s)).len() }",
    );
    let class: String = "a".repeat(4050) + "b";
    let schema = format!("{{\"pattern\":\"(?:[{class}]{{1000}}){{4}}\"}}");
    for len in [100usize, 400] {
        let start = Instant::now();
        let f = run(
            &p,
            "v",
            vec![
                Value::text(&format!("\"{}\"", "a".repeat(len))),
                Value::text(&schema),
            ],
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        assert!(
            f.heap_peak < 32 * 1024 * 1024,
            "{} bytes of heap",
            f.heap_peak
        );
        match f.outcome {
            Outcome::Returned(_) => {}
            // Refused as too big a program, or too much work for what was paid.
            Outcome::Raised(e) => assert!(
                e.kind(&p) == "Invalid" || e.budget(&p).as_deref() == Some("steps"),
                "{}",
                e.kind(&p)
            ),
        }
    }
}

/// A chain of `$defs` that each apply the next one twice, ending in `leaf`.
fn doubling_chain(n: u32, leaf: &str) -> String {
    let mut defs: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r##""a{i}":{{"allOf":[{{"$ref":"#/$defs/a{j}"}},{{"$ref":"#/$defs/a{j}"}}]}}"##,
                j = i + 1
            )
        })
        .collect();
    defs.push(format!(r#""a{n}":{leaf}"#));
    format!(
        r##"{{"$ref":"#/$defs/a0","$defs":{{{}}}}}"##,
        defs.join(",")
    )
}

/// The work inside one check (an `enum`'s comparisons, `uniqueItems`' map) is paid for, not
/// only the checks: a shared sub-schema reached over and over cannot do unpaid work.
#[test]
fn work_inside_one_check_is_bounded_by_the_charge() {
    let p = prepared(
        r"fn v(j: Str, s: Str) -> Int { json.validate(json.parse(j), json.parse(s)).len() }",
    );
    let values: Vec<String> = (0..800).map(|i| i.to_string()).collect();
    let enum_leaf = format!("{{\"enum\":[{}]}}", values.join(","));
    let items: Vec<String> = (0..100).map(|i| i.to_string()).collect();
    let list = format!("[{}]", items.join(","));
    let cases = [
        ("799", doubling_chain(16, &enum_leaf)),
        (list.as_str(), doubling_chain(16, r#"{"uniqueItems":true}"#)),
        ("1", doubling_chain(16, r#"{"const":[1,2,3]}"#)),
        (
            r#"{"a":1,"b":2}"#,
            doubling_chain(
                16,
                r#"{"required":["a","b","c"],"dependentRequired":{"a":["b"]}}"#,
            ),
        ),
    ];
    for (instance, schema) in cases {
        let start = Instant::now();
        let f = run(&p, "v", vec![Value::text(instance), Value::text(&schema)]);
        let Outcome::Raised(e) = &f.outcome else {
            panic!("returned for {instance:.20}")
        };
        assert_eq!(e.budget(&p).as_deref(), Some("steps"), "{instance:.20}");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
    }
    // The same checks, unshared, are validated.
    let f = run(
        &p,
        "v",
        vec![Value::text(&list), Value::text(r#"{"uniqueItems":true}"#)],
    );
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    let f = run(&p, "v", vec![Value::text("799"), Value::text(&enum_leaf)]);
    assert!(matches!(f.outcome, Outcome::Returned(_)));
}

/// Class syntax of the `re` engine that ECMAScript reads differently is escaped or refused.
#[test]
fn class_syntax_keeps_its_ecmascript_meaning() {
    let p = prepared(
        r"fn v(j: Str, s: Str) -> Int { json.validate(json.parse(j), json.parse(s)).len() }",
    );
    let w = "\\".repeat(2) + "w";
    let cases: Vec<(String, String, Result<i64, &str>)> = vec![
        ("\"&\"".into(), "^[a&&b]$".into(), Ok(0)),
        ("\"a\"".into(), "^[a&&b]$".into(), Ok(0)),
        ("\"x\"".into(), "^[a&&b]$".into(), Ok(1)),
        ("\"~\"".into(), "^[a~~b]$".into(), Ok(0)),
        ("\"[\"".into(), "^[a[b]$".into(), Err("Invalid")),
        ("\"_\"".into(), format!("^[{w}-z]$"), Err("Invalid")),
        ("\"-\"".into(), format!("^[{w}-]$"), Ok(0)),
        ("\"b\"".into(), "^[a-c]$".into(), Ok(0)),
        ("\"-\"".into(), "^[a--b]$".into(), Err("Invalid")),
        ("\"-\"".into(), format!("^[a-{w}]$"), Err("Invalid")),
    ];
    for (instance, pattern, want) in cases {
        let schema = format!("{{\"pattern\":\"{pattern}\"}}");
        let got = violations(&p, &instance, &schema);
        assert_eq!(
            got,
            want.map_err(str::to_owned),
            "{instance} against {pattern}"
        );
    }
}

/// The machine's own map paths (`m[k]`, `m[k] = v`, `in`) carry the labels inside a composite
/// key, and of every key on the path.
#[test]
fn machine_map_paths_carry_interior_key_labels() {
    let p = prepared(
        &r"
fn index(s: Int) -> Int { map.from([((5, 0), 1)])[(s, 0)] }
fn missing(s: Int) -> Int {
  try { map.from([((5, 0), 1)])[(s + 1, 0)] } catch e { 0 }
}
fn missing_error(s: Int) -> Untrusted<Str> {
  try { let n = map.from([((5, 0), 1)])[(s + 1, 0)]; 'none' } catch e { e.message }
}
fn assign(s: Int) -> Int {
  var m = map.from([((5, 0), 1)])
  m[(s, 0)] = 2
  map.len(m)
}
fn member(s: Int) -> Bool { (s, 0) in map.from([((5, 0), 1)]) }
fn stored(s: Int) -> Bool { (5, 0) in map.from([((s, 0), 1)]) }
"
        .replace('\'', "\""),
    );
    for name in ["index", "assign", "member", "stored", "missing_error"] {
        let f = run(&p, name, vec![Value::int(5).with_labels(SECRET)]);
        assert!(scan(&f).contains(SECRET), "{name} dropped the label");
    }
}
