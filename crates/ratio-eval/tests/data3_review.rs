//! Regression tests for the stage 9 security review, round 1 (probes p1 to p9 of the review).

#![allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    reason = "each test is one probe, written out"
)]

mod support;

use std::time::{Duration, Instant};

use ratio_eval::{Limits, Outcome, Value, work};
use support::{SECRET, prepared, run, run_with};

fn big() -> Limits {
    Limits {
        steps: 4_000_000_000,
        heap: 64 * 1024 * 1024,
        call_depth: 512,
    }
}

fn ints(n: usize) -> Value {
    Value::list((0..i64::try_from(n).unwrap()).map(Value::int).collect()).unwrap()
}

/// Runs `main` and returns the machine's work log: the work every pure built-in call counted
/// and the steps it was charged.
fn logged(
    p: &std::sync::Arc<ratio_eval::Prepared>,
    name: &str,
    args: Vec<Value>,
    cancel: ratio_eval::CancelToken,
) -> (ratio_eval::Finished, Vec<ratio_eval::BuiltinWork>) {
    let entry = p.find("main.ratio", name).unwrap();
    let mut eval =
        ratio_eval::evaluate(std::sync::Arc::clone(p), entry, args, big(), cancel).unwrap();
    let f = ratio_eval::block_on(&mut eval);
    let log = if work::ENABLED {
        eval.machine().work_log().to_vec()
    } else {
        Vec::new()
    };
    (f, log)
}

/// Every logged call stayed within 16 units of work per unit of charge.
fn within_charge(name: &str, log: &[ratio_eval::BuiltinWork]) {
    for w in log {
        assert!(
            w.work <= 16 * 64 * w.steps,
            "{name}: `{}` counted {} units for {} steps",
            w.name,
            w.work,
            w.steps
        );
    }
}

/// F1: a template that fails after writing pays for what it did, however often it is caught.
#[test]
fn f1_a_failing_template_pays_for_its_work() {
    let body = "A".repeat(14);
    let src = format!(
        "fn once(xs: [Int]) -> Int {{ match result(|| text.len(template.render(r\"{{{{#xs}}}}{{{{#xs}}}}{{{{#xs}}}}{body}{{{{/xs}}}}{{{{/xs}}}}{{{{/xs}}}}{{{{nope}}}}\", {{ xs: xs }}))) {{ Ok {{ value }} => value, Err {{ .. }} => 0 }} }}\n\
         fn main(xs: [Int], reps: [Int]) -> Int {{ list.fold(reps, 0, |a, i| a + once(xs)) }}\n"
    );
    let p = prepared(&src);
    let f = run_with(&p, "main", vec![ints(100), ints(20)], big());
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    // 20 × 10^6 innermost visits and 14 MB of text written: at least that is paid.
    assert!(
        f.steps > 20 * 14_000_000 / 64,
        "charged only {} steps",
        f.steps
    );
}

/// F2: `diff.unified` charges its header names and the text it writes.
#[test]
fn f2_unified_charges_headers_and_output() {
    let p = prepared(
        "fn once(h: Str) -> Int { text.len(diff.unified(\"a\", \"b\", context: 0, from: h, to: h)) }\n\
         fn main(h: Str, reps: [Int]) -> Int { list.fold(reps, 0, |a, i| a + once(h)) }",
    );
    let h = Value::text(&"x".repeat(8 * 1024 * 1024 - 64));
    let (f, log) = logged(&p, "main", vec![h, ints(3)], ratio_eval::CancelToken::new());
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    // 16 MiB written each time: 262,144 steps at least.
    assert!(f.steps > 3 * 262_000, "charged only {} steps", f.steps);
    within_charge("unified", &log);
}

/// F3: a CSV with long header names and many rows is not quadratic in the key length.
#[test]
fn f3_csv_rows_do_not_compare_the_header_again() {
    let p = prepared("fn main(s: Str) -> Int { list.len(codec.csv_parse(s)) }");
    let l = 256 * 1024;
    let mut s = String::new();
    s.push_str(&"P".repeat(l));
    s.push_str("1,");
    s.push_str(&"P".repeat(l));
    s.push_str("2\n");
    for _ in 0..20_000 {
        s.push_str(",\n");
    }
    let start = Instant::now();
    let (f, log) = logged(
        &p,
        "main",
        vec![Value::text(&s)],
        ratio_eval::CancelToken::new(),
    );
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert!(start.elapsed() < Duration::from_secs(10));
    within_charge("csv_parse", &log);
    // A hostile ratio of rows to key length is cancelled between rows.
    let cancel = ratio_eval::CancelToken::new();
    cancel.cancel();
    let entry = p.find("main.ratio", "main").unwrap();
    let eval = ratio_eval::evaluate(
        std::sync::Arc::clone(&p),
        entry,
        vec![Value::text(&s)],
        big(),
        cancel,
    )
    .unwrap();
    let f = ratio_eval::block_on(eval);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Cancelled");
}

/// F4: looking into `none` or a scalar in a template keeps that value's labels.
#[test]
fn f4_template_lookups_through_none_keep_labels() {
    let p = prepared(
        "fn main(s: Str) -> [Untrusted<Str>] {\n\
           let recs = list.filter([{ b: true, k: \"x\" }], |r| r.k == s)\n\
           let a = list.first(recs)\n\
           [template.render(r\"{{#a.b}}X{{/a.b}}\", { a: a }), template.render(r\"{{#xs}}{{#b}}X{{/b}}{{/xs}}\", { xs: [a] })]\n\
         }",
    );
    for s in ["x", "y"] {
        let f = run(&p, "main", vec![Value::text(s).with_labels(SECRET)]);
        let Outcome::Returned(v) = f.outcome else {
            panic!("raised")
        };
        for (i, it) in v.elements().unwrap().iter().enumerate() {
            assert!(
                it.scan().contains(SECRET),
                "s={s}: element {i} lost the label"
            );
        }
    }
}

/// F5: a relative path never turns into a drive path.
#[test]
fn f5_relative_paths_stay_relative() {
    let p = prepared(
        "fn main(u: Str) -> [Untrusted<Str>] { [\n\
           path.normalize(\"a/../C:/x\"),\n\
           text.show(path.is_absolute(path.normalize(\"a/../C:/x\"))),\n\
           text.show(path.relative_to(\"a/C:/x\", \"a\")),\n\
           text.show(path.relative_to(\"a/c:x\", \"a\")),\n\
           path.normalize(\"./C:\\\\Windows\"),\n\
           path.normalize(\"a/../c:\"),\n\
           text.show(path.relative_to(\"a/b\", \"a\")),\n\
           text.show(path.relative_to(\"/a/C:/x\", \"/a\")),\n\
           text.show(path.relative_to(\"C:/a/D:/x\", \"C:/a\")),\n\
           text.show(path.relative_to(\"C:a/D:/x\", \"C:a\")),\n\
           text.show(path.relative_to(\"//s/sh/C:/x\", \"//s/sh\")),\n\
           path.normalize(\"C:/a/../x\"),\n\
         ] }",
    );
    let f = run(&p, "main", vec![Value::text("")]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    let got: Vec<String> = v
        .elements()
        .unwrap()
        .iter()
        .map(|x| x.as_text().unwrap().to_owned())
        .collect();
    assert_eq!(
        got,
        [
            "./C:/x",
            "false",
            "none",
            "none",
            "./C:/Windows",
            "./c:",
            "b",
            "none",
            "none",
            "none",
            "none",
            "C:/x"
        ]
    );
}

/// F6: `file:` hosts as the WHATWG parser reads them; hosts of every scheme in lower case.
#[test]
fn f6_file_hosts() {
    let p = prepared(
        "fn main(u: Str) -> [Untrusted<Str>] { [\n\
           text.show(url.host(\"file://C:/x\")), text.show(url.host(\"file://c|/x\")),\n\
           text.show(url.parse(\"file://u@h/x\")), text.show(url.host(\"file://localhost/x\")),\n\
           text.show(url.host(\"file://LOCALHOST/x\")), text.show(url.host(\"file://h/x\")),\n\
           text.show(url.host(\"foo://H.COM/\")), text.show(url.host(\"http://LocalHost/\")),\n\
           text.show(url.parse(\"foo://H.COM/\")?.host), text.show(url.parse(\"HTTP://H.COM/\")?.host),\n\
         ] }",
    );
    let f = run(&p, "main", vec![Value::text("")]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    let got: Vec<String> = v
        .elements()
        .unwrap()
        .iter()
        .map(|x| x.as_text().unwrap().to_owned())
        .collect();
    assert_eq!(
        got,
        [
            "none",
            "none",
            "none",
            "none",
            "none",
            "h",
            "h.com",
            "localhost",
            "H.COM",
            "h.com"
        ]
    );
}

/// F7: equal values hash alike, `-0.0` and `0.0` included.
#[test]
fn f7_negative_zero_hashes_as_zero() {
    let p = prepared(
        "fn main(z: Float) -> [Untrusted<Str>] { [hash.stable(z), hash.stable(0.0), hash.stable([z]), hash.stable([0.0])] }",
    );
    let f = run(&p, "main", vec![Value::float(-0.0)]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    let e = v.elements().unwrap();
    assert_eq!(e[0].as_text(), e[1].as_text());
    assert_eq!(e[2].as_text(), e[3].as_text());
}

/// A user's own `DiffOp`, `Url` and `Vector` do not break the built-ins (probe p9).
#[test]
fn p9_user_names_that_shadow_the_prelude() {
    let p = prepared(
        "enum DiffOp { zzz, equal }\ntype Url = Int\ntype Vector = Str\n\
         fn main(a: Str) -> [Untrusted<Str>] { let u: Url = 3\n let v: Vector = \"x\"\n [text.show(diff.lines(a, \"x\\ny\")), text.show(diff.stats(diff.lines(a, \"y\"))), text.show(u), v] }",
    );
    let f = run(&p, "main", vec![Value::text("x\nz")]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    let e = v.elements().unwrap();
    assert!(e[0].as_text().unwrap().contains("\"op\":\"delete\""));
    assert!(e[1].as_text().unwrap().contains("\"added\""));
}

/// LV-7: the `vec` and `math` aggregates look at the cancellation flag as they work, so a
/// token cancelled before the call stops it within a chunk of work, not after the whole input.
#[test]
fn lv7_aggregates_stop_when_cancelled() {
    let p = prepared(
        "fn top(q: [Float], vs: [[Float]]) -> Int { list.len(vec.top_k(q, vs, 10)) }\n\
         fn median(xs: [Float]) -> Int { text.show(math.median(xs)).len() }\n\
         fn mode(xs: [Int]) -> Int { text.show(math.mode(xs)).len() }\n\
         fn norm(xs: [Float]) -> Int { text.show(vec.norm(xs)).len() }\n\
         fn softmax(xs: [Float]) -> Int { list.len(math.softmax(xs)) }",
    );
    let floats = |n: usize| {
        Value::list(
            (0..n)
                .map(|i| Value::float(((i * 7919) % 1000) as f64))
                .collect(),
        )
        .unwrap()
    };
    let vectors = Value::list((0..2000).map(|_| floats(500)).collect()).unwrap();
    let cases: Vec<(&str, Vec<Value>)> = vec![
        ("top", vec![floats(500), vectors]),
        ("median", vec![floats(1_000_000)]),
        ("mode", vec![ints(1_000_000)]),
        ("norm", vec![floats(1_000_000)]),
        ("softmax", vec![floats(1_000_000)]),
    ];
    for (name, args) in cases {
        let cancel = ratio_eval::CancelToken::new();
        cancel.cancel();
        let (f, log) = logged(&p, name, args, cancel);
        let Outcome::Raised(e) = f.outcome else {
            panic!("{name} returned")
        };
        assert_eq!(e.kind(&p), "Cancelled", "{name}");
        for w in &log {
            assert!(
                w.work < 400_000,
                "{name}: `{}` did {} units of work after cancellation",
                w.name,
                w.work
            );
        }
    }
}
