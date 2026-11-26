//! Charging and limits of the stage 9 built-ins: the charge formulas of chapter 06, the work a
//! built-in does against its charge (debug builds count it), large inputs refused before
//! running, buffers charged to the heap before they are allocated, and floats with fixed bit
//! patterns.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::format_collect,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "tests build inputs by count, as tables, and name them briefly"
)]

mod support;

use std::time::{Duration, Instant};

use ratio_eval::{Labels, Limits, Outcome, Value, work};
use support::{SECRET, prepared, raised, run, run_with};

fn steps_of(source: &str, args: Vec<Value>) -> u64 {
    let p = prepared(source);
    let f = run(&p, "main", args);
    assert!(
        matches!(f.outcome, Outcome::Returned(_)),
        "the program raised: {source}"
    );
    f.steps
}

fn lines(n: usize, tag: &str) -> String {
    (0..n).map(|i| format!("{tag}{i}\n")).collect()
}

// ---- the charge formulas ----

/// The steps of a call at two sizes differ by the difference of its charge: the program's own
/// steps cancel.
#[test]
fn the_charges_follow_chapter_06() {
    // const: 1 step whatever the argument.
    let c = |x: f64| {
        steps_of(
            "fn main(x: Float) -> Float { math.sqrt(x) }",
            vec![Value::float(x)],
        )
    };
    assert_eq!(c(2.0), c(1e300));

    // linear, `n + m`: sha256 of n characters has an output of 64.
    let sha = |n: usize| {
        steps_of(
            "fn main(s: Str) -> Str { hash.sha256(s) }",
            vec![Value::text(&"a".repeat(n))],
        )
    };
    let ceil = |x: usize| x.div_ceil(64) as u64;
    assert_eq!(sha(64_000) - sha(64), ceil(64_000 + 64) - ceil(64 + 64));
    assert_eq!(sha(1_000_000) - sha(64_000), ceil(1_000_064) - ceil(64_064));

    // log: not in this stage. nlogn, `n · ⌈log2(n + 1)⌉` over the list's weight (its elements
    // and one): median of n floats.
    let median = |n: usize| {
        let xs: Vec<Value> = (0..n).map(|i| Value::float(i as f64)).collect();
        steps_of(
            "fn main(xs: [Float]) -> Float? { math.median(xs) }",
            vec![Value::list(xs).unwrap()],
        )
    };
    let w = |n: usize| {
        let weight = n + 1;
        let log = usize::BITS as usize - weight.leading_zeros() as usize;
        (weight * log).div_ceil(64) as u64
    };
    assert_eq!(median(5000) - median(50), w(5000) - w(50));

    // product, `n₁ · n₂ + c₁ + c₂`: lines of two texts.
    let diff = |n1: usize, n2: usize| {
        let (a, b) = (lines(n1, "x"), lines(n2, "x"));
        let c = a.chars().count() + b.chars().count();
        let steps = steps_of(
            "fn main(a: Str, b: Str) -> Int { list.len(diff.lines(a, b)) }",
            vec![Value::text(&a), Value::text(&b)],
        );
        // The product, and the characters counted first (debited as `linear`).
        (steps, ((n1 * n2 + c).div_ceil(64) + c.div_ceil(64)) as u64)
    };
    // The output of `diff.lines` is a list (no output part for `product`).
    let (s1, c1) = diff(100, 100);
    let (s2, c2) = diff(400, 300);
    assert_eq!(s2 - s1, c2 - c1);
}

// ---- large inputs raise Budget without running ----

#[test]
fn a_large_diff_raises_budget_before_it_runs() {
    let p = prepared(
        r"
fn lines(a: Str, b: Str) -> Int { list.len(diff.lines(a, b)) }
fn words(a: Str, b: Str) -> Int { list.len(diff.words(a, b)) }
fn unified(a: Str, b: Str) -> Int { text.len(diff.unified(a, b)) }
",
    );
    // 20,000 lines each: 4 × 10^8 cells, 6.25 million steps, over the 5 million allowed. The
    // built-in would take seconds; it is refused first and only the body is charged.
    let a = lines(20_000, "a");
    let b = lines(20_000, "b");
    for name in ["lines", "unified"] {
        let start = Instant::now();
        let f = run(&p, name, vec![Value::text(&a), Value::text(&b)]);
        assert!(start.elapsed() < Duration::from_secs(2), "{name} ran");
        let Outcome::Raised(e) = f.outcome else {
            panic!("{name} returned")
        };
        assert_eq!(e.budget(&p).as_deref(), Some("steps"), "{name}");
        assert!(f.steps < 20_000, "{name} charged {}", f.steps);
        assert!(f.heap_peak < 1 << 20, "{name} allocated {}", f.heap_peak);
    }
    let words: String = (0..20_000).map(|i| format!("a{i} ")).collect();
    let more: String = (0..20_000).map(|i| format!("b{i} ")).collect();
    let f = run(&p, "words", vec![Value::text(&words), Value::text(&more)]);
    let Outcome::Raised(e) = f.outcome else {
        panic!("words returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
    assert!(f.steps < 20_000);
    // The same inputs are refused outright on a budget that cannot pay, and run on one that can.
    let small = lines(300, "a");
    let small_b = lines(300, "b");
    let f = run(
        &p,
        "lines",
        vec![Value::text(&small), Value::text(&small_b)],
    );
    assert!(matches!(f.outcome, Outcome::Returned(_)));
    assert!(f.steps > 300 * 300 / 64, "charged only {} steps", f.steps);
    let tight = Limits {
        steps: 300 * 300 / 64 - 100,
        ..Limits::default()
    };
    let kind = raised(
        &p,
        "lines",
        vec![Value::text(&small), Value::text(&small_b)],
        tight,
    );
    assert_eq!(kind, "Budget");
}

#[test]
fn a_diff_with_one_empty_side_is_cheap_however_long_the_other() {
    // n₁ · n₂ is 0, so only the characters are charged; the work is linear too.
    let p = prepared("fn main(a: Str, b: Str) -> Int { list.len(diff.lines(a, b)) }");
    let big = lines(100_000, "x");
    let f = run(&p, "main", vec![Value::text(&big), Value::text("")]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_int(), Some(100_000));
    assert!(f.steps < 3 * (big.len() as u64 / 64 + 100));
}

#[test]
fn diffs_of_similar_inputs_are_found_and_minimal() {
    let p = prepared(
        r"
fn main(a: Str, b: Str) -> [Int] {
  let d = diff.lines(a, b)
  let s = diff.stats(d)
  [s.added, s.removed, s.unchanged]
}
",
    );
    // 3,000 lines, every tenth one changed: the script has 300 deletions and 300 insertions.
    let a: String = (0..3000)
        .map(|i| {
            if i % 10 == 5 {
                format!("old{i}\n")
            } else {
                format!("l{i}\n")
            }
        })
        .collect();
    let b: String = (0..3000)
        .map(|i| {
            if i % 10 == 5 {
                format!("new{i}\n")
            } else {
                format!("l{i}\n")
            }
        })
        .collect();
    let f = run(&p, "main", vec![Value::text(&a), Value::text(&b)]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    let got: Vec<i64> = v
        .elements()
        .unwrap()
        .iter()
        .map(|x| x.as_int().unwrap())
        .collect();
    assert_eq!(got, vec![300, 300, 2700]);
}

// ---- the work stays within a fixed multiple of the charge ----

/// The most units of work (one per byte, element or cell) a built-in may count per unit of
/// charge; a step pays for 64.
const MULTIPLE: u64 = 16;

struct Work {
    name: &'static str,
    source: String,
    args: Vec<Value>,
}

fn work_row(name: &'static str, params: &str, call: &str, args: Vec<Value>) -> Work {
    Work {
        name,
        source: format!("fn main({params}) -> Untrusted<Str> {{ text.show({call}) }}\n"),
        args,
    }
}

fn big_text() -> String {
    "ab cd\nefg h\n".repeat(20_000)
}

fn big_floats(n: usize) -> Value {
    Value::list(
        (0..n)
            .map(|i| Value::float(((i * 7919) % 1000) as f64 / 10.0))
            .collect(),
    )
    .unwrap()
}

fn big_ints(n: usize) -> Value {
    Value::list(
        (0..n)
            .map(|i| Value::int(((i * 7919) % 1000) as i64))
            .collect(),
    )
    .unwrap()
}

fn big_vectors(count: usize, dims: usize) -> Value {
    Value::list(
        (0..count)
            .map(|i| {
                Value::list(
                    (0..dims)
                        .map(|j| Value::float(((i * 31 + j * 17) % 97) as f64 / 7.0))
                        .collect(),
                )
                .unwrap()
            })
            .collect(),
    )
    .unwrap()
}

fn work_rows() -> Vec<Work> {
    let t = big_text();
    let a = lines(1500, "line");
    let b: String = (0..1500)
        .map(|i| {
            if i % 7 == 0 {
                format!("changed{i}\n")
            } else {
                format!("line{i}\n")
            }
        })
        .collect();
    let patch = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n line0\n-line1\n+other\n line2\n";
    let url = format!(
        "http://Example.com:8080/a/b/../c/{}?{}#frag",
        "seg/".repeat(200),
        (0..300)
            .map(|i| format!("k{i}=v{i}"))
            .collect::<Vec<_>>()
            .join("&")
    );
    let path: String = "a/b/../c/./".repeat(5000);
    let parts = Value::list((0..5000).map(|_| Value::text("seg/x")).collect()).unwrap();
    let csv: String = std::iter::once("a,b,c,d\n".to_owned())
        .chain((0..3000).map(|i| format!("{i},\"q,{i}\",x,\"y\"\"z\"\n")))
        .collect();
    let mut rows = vec![
        work_row(
            "math.mean",
            "xs: [Float]",
            "math.mean(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.median",
            "xs: [Float]",
            "math.median(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.mode",
            "xs: [Int]",
            "math.mode(xs)",
            vec![big_ints(20_000)],
        ),
        work_row(
            "math.variance",
            "xs: [Float]",
            "math.variance(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.stdev",
            "xs: [Float]",
            "math.stdev(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.percentile",
            "xs: [Float]",
            "math.percentile(xs, 90.0)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.softmax",
            "xs: [Float]",
            "math.softmax(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.argmax",
            "xs: [Float]",
            "math.argmax(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.argmin",
            "xs: [Float]",
            "math.argmin(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.entropy",
            "xs: [Float]",
            "math.entropy(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "math.pow",
            "x: Float, y: Float",
            "math.pow(x, y)",
            vec![Value::float(1.1), Value::float(7.5)],
        ),
        work_row(
            "math.round",
            "x: Float",
            "math.round(x, 300)",
            vec![Value::float(1e-5)],
        ),
        work_row(
            "math.exp",
            "x: Float",
            "math.exp(x)",
            vec![Value::float(3.5)],
        ),
        work_row(
            "hash.sha256",
            "s: Str",
            "hash.sha256(s)",
            vec![Value::text(&t)],
        ),
        work_row(
            "hash.blake3",
            "s: Str",
            "hash.blake3(s)",
            vec![Value::text(&t)],
        ),
        work_row(
            "hash.stable",
            "xs: [Float]",
            "hash.stable(xs)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "hash.short",
            "xs: [Int]",
            "hash.short(xs)",
            vec![big_ints(20_000)],
        ),
        work_row(
            "diff.lines",
            "a: Str, b: Str",
            "diff.lines(a, b)",
            vec![Value::text(&a), Value::text(&b)],
        ),
        work_row(
            "diff.words",
            "a: Str, b: Str",
            "diff.words(a, b)",
            vec![Value::text(&a), Value::text(&b)],
        ),
        work_row(
            "diff.unified",
            "a: Str, b: Str",
            "diff.unified(a, b)",
            vec![Value::text(&a), Value::text(&b)],
        ),
        work_row(
            "diff.apply",
            "a: Str, p: Str",
            "diff.apply(a, p)",
            vec![Value::text(&a), Value::text(patch)],
        ),
        work_row(
            "diff.stats",
            "a: Str, b: Str",
            "diff.stats(diff.lines(a, b))",
            vec![Value::text(&a), Value::text(&b)],
        ),
        work_row(
            "template.render",
            "xs: [Int], n: Str",
            template_call(),
            vec![big_ints(20_000), Value::text("n")],
        ),
        work_row(
            "template.names",
            "t: Str",
            "template.names(t)",
            vec![Value::text(&"{{a}} {{#b}}x{{/b}} ".repeat(10_000))],
        ),
        work_row(
            "vec.dot",
            "a: [Float], b: [Float]",
            "vec.dot(a, b)",
            vec![big_floats(20_000), big_floats(20_000)],
        ),
        work_row(
            "vec.norm",
            "a: [Float]",
            "vec.norm(a)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "vec.normalize",
            "a: [Float]",
            "vec.normalize(a)",
            vec![big_floats(20_000)],
        ),
        work_row(
            "vec.cosine",
            "a: [Float], b: [Float]",
            "vec.cosine(a, b)",
            vec![big_floats(20_000), big_floats(20_000)],
        ),
        work_row(
            "vec.add",
            "a: [Float], b: [Float]",
            "vec.add(a, b)",
            vec![big_floats(20_000), big_floats(20_000)],
        ),
        work_row(
            "vec.mean",
            "vs: [[Float]]",
            "vec.mean(vs)",
            vec![big_vectors(300, 64)],
        ),
        work_row(
            "vec.top_k",
            "q: [Float], vs: [[Float]]",
            "vec.top_k(q, vs, 10)",
            vec![big_floats(64), big_vectors(300, 64)],
        ),
        work_row("path.join", "ps: [Str]", "path.join(ps)", vec![parts]),
        work_row(
            "path.parent",
            "p: Str",
            "path.parent(p)",
            vec![Value::text(&path)],
        ),
        work_row(
            "path.name",
            "p: Str",
            "path.name(p)",
            vec![Value::text(&path)],
        ),
        work_row(
            "path.normalize",
            "p: Str",
            "path.normalize(p)",
            vec![Value::text(&path)],
        ),
        work_row(
            "path.relative_to",
            "p: Str, b: Str",
            "path.relative_to(p, \"a\")",
            vec![Value::text(&path), Value::text("a")],
        ),
        work_row(
            "url.parse",
            "u: Str",
            "url.parse(u)",
            vec![Value::text(&url)],
        ),
        work_row("url.host", "u: Str", "url.host(u)", vec![Value::text(&url)]),
        work_row(
            "url.encode",
            "s: Str",
            "url.encode(s)",
            vec![Value::text(&t)],
        ),
        work_row(
            "url.decode",
            "s: Str",
            "url.decode(s)",
            vec![Value::text(&"%C3%A9a%20".repeat(30_000))],
        ),
        work_row(
            "codec.base64_encode",
            "s: Str",
            "codec.base64_encode(s)",
            vec![Value::text(&t)],
        ),
        work_row(
            "codec.base64_decode",
            "s: Str",
            "codec.base64_decode(codec.base64_encode(s))",
            vec![Value::text(&t)],
        ),
        work_row(
            "codec.hex_encode",
            "s: Str",
            "codec.hex_encode(s)",
            vec![Value::text(&t)],
        ),
        work_row(
            "codec.hex_decode",
            "s: Str",
            "codec.hex_decode(codec.hex_encode(s))",
            vec![Value::text(&t)],
        ),
        work_row(
            "codec.csv_parse",
            "s: Str",
            "codec.csv_parse(s)",
            vec![Value::text(&csv)],
        ),
        work_row(
            "codec.csv_write",
            "s: Str",
            "codec.csv_write(codec.csv_parse(s))",
            vec![Value::text(&csv)],
        ),
    ];
    // url.with_query and csv_write over a map built in the program from a list of counters.
    rows.push(work_row(
        "url.with_query",
        "xs: [Int]",
        "url.with_query(\"http://a/?k1=x\", list.fold(xs, map.set({}, \"x\", \"v\"), |m, i| map.set(m, \"k{i}\", \"v\")))",
        vec![big_ints(1500)],
    ));
    rows
}

fn template_call() -> &'static str {
    // `n` is the one name looked up, once per element.
    "template.render(r\"{{#xs}}[{{n}}:{{.}}] {{/xs}}{{n}}\", { xs: xs, n: n })"
}

#[test]
fn work_stays_within_a_fixed_multiple_of_the_charge() {
    if !work::ENABLED {
        return; // release builds count nothing
    }
    let mut problems = Vec::new();
    let mut table = Vec::new();
    for row in work_rows() {
        // The three task built-ins are not in the machine's log (it records pure calls); their
        // charges are checked by the steps they must pay.
        if matches!(row.name, "template.render" | "hash.stable" | "hash.short") {
            continue;
        }
        let p = prepared(&row.source);
        let entry = p.find("main.ratio", "main").unwrap();
        let mut eval = ratio_eval::evaluate(
            std::sync::Arc::clone(&p),
            entry,
            row.args,
            Limits::default(),
            ratio_eval::CancelToken::new(),
        )
        .unwrap();
        let f = ratio_eval::block_on(&mut eval);
        if let Outcome::Raised(e) = &f.outcome {
            problems.push(format!(
                "{}: raised {} ({:?})",
                row.name,
                e.kind(&p),
                e.message()
            ));
            continue;
        }
        // The built-in's own entry of the log: its counted work against its charge.
        let Some(w) = eval
            .machine()
            .work_log()
            .iter()
            .find(|w| w.name == row.name)
            .copied()
        else {
            problems.push(format!("{}: not in the work log", row.name));
            continue;
        };
        let ratio = w.work as f64 / (64.0 * w.steps as f64);
        table.push(format!(
            "{:<22} {:>9} steps {:>10} units {ratio:>6.2}",
            row.name, w.steps, w.work
        ));
        if w.work > MULTIPLE * 64 * w.steps {
            problems.push(format!(
                "{}: {} units of work for {} steps",
                row.name, w.work, w.steps
            ));
        }
    }
    eprintln!("{}", table.join("\n"));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A template whose walk does far more than `n + m` work (a section that writes nothing, over
/// two lists, one inside the other) pays for it, in instalments, before doing it.
#[test]
fn a_template_that_writes_nothing_still_pays_for_its_walk() {
    let p = prepared(
        "fn main(xs: [Int]) -> Int { text.len(template.render(r\"{{#xs}}{{#xs}}{{/xs}}{{/xs}}\", { xs: xs })) }",
    );
    let xs = || big_ints(1500);
    // 1,500 × 1,500 iterations of the inner section: 2.25 million visits for a template and a
    // list of 1,500 elements.
    let f = run(&p, "main", vec![xs()]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_int(), Some(0));
    assert!(f.steps > 2_250_000 / 64, "charged only {} steps", f.steps);
    if work::ENABLED {
        work::reset();
        let _ = run(&p, "main", vec![xs()]);
        assert!(
            work::total() <= MULTIPLE * 64 * f.steps,
            "{} units",
            work::total()
        );
    }
    // A budget that cannot pay stops it early, with little work done.
    let tight = Limits {
        steps: 5_000,
        ..Limits::default()
    };
    work::reset();
    let kind = {
        let f = run_with(&p, "main", vec![xs()], tight);
        let Outcome::Raised(e) = f.outcome else {
            panic!("returned")
        };
        assert_eq!(e.budget(&p).as_deref(), Some("steps"));
        e.kind(&p)
    };
    assert_eq!(kind, "Budget");
    if work::ENABLED {
        assert!(
            work::total() <= 4 * 64 * 5_000 + 1_000_000,
            "{} units",
            work::total()
        );
    }
}

// ---- refusals and the heap ----

#[test]
fn a_refused_encoding_allocates_nothing() {
    let p = prepared(
        "fn main(s: Str) -> Int { text.len(codec.base64_encode(s)) }\nfn hex(s: Str) -> Int { text.len(codec.hex_encode(s)) }\nfn enc(s: Str) -> Int { text.len(url.encode(s)) }",
    );
    // 16,000,000 bytes in, 21.3 MB out: more than a string may be.
    let big = "ab".repeat(8_000_000);
    let f = run(&p, "main", vec![Value::text(&big)]);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Invalid");
    assert!(
        f.heap_peak < 1 << 20,
        "the refusal allocated {}",
        f.heap_peak
    );
    let f = run(&p, "hex", vec![Value::text(&"a".repeat(8_388_609))]);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Invalid");
    assert!(f.heap_peak < 1 << 20);
    let f = run(&p, "enc", vec![Value::text(&"/".repeat(6_000_000))]);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Invalid");
    assert!(f.heap_peak < 1 << 20);
    // A result of exactly 16 MiB is allowed (a hex encoding of 8 MiB).
    let f = run(&p, "hex", vec![Value::text(&"a".repeat(8_388_608))]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("refused")
    };
    assert_eq!(v.as_int(), Some(16_777_216));
}

#[test]
fn buffers_are_charged_to_the_heap_as_they_grow() {
    // Rendering and canonical JSON stop at the heap limit, not after it.
    let p = prepared(
        "fn stable(xs: [Float]) -> Int { text.len(hash.stable(xs)) }\nfn render(xs: [Int]) -> Int { text.len(template.render(r\"{{#xs}}0123456789{{/xs}}\", { xs: xs })) }\nfn csv(s: Str) -> Int { list.len(codec.csv_parse(s)) }",
    );
    let heap = 2 << 20;
    let limits = Limits {
        heap,
        ..Limits::default()
    };
    let xs = Value::list((0..400_000).map(|i| Value::float(f64::from(i))).collect());
    let xs = xs.unwrap();
    let cells = "a,b,c,d,e,f,g,h\n".to_owned() + &"1,2,3,4,5,6,7,8\n".repeat(200_000);
    for (name, arg) in [
        ("stable", xs),
        ("render", big_ints(500_000)),
        ("csv", Value::text(&cells)),
    ] {
        let f = run_with(&p, name, vec![arg], limits);
        let Outcome::Raised(e) = f.outcome else {
            panic!("{name} returned")
        };
        assert_eq!(e.budget(&p).as_deref(), Some("heap"), "{name}");
        assert!(
            f.heap_peak <= heap,
            "{name}: peak {} over {heap}",
            f.heap_peak
        );
    }
}

// ---- floats ----

/// `math` and `vec` results as bit patterns: the same on every platform, because only
/// exactly specified operations are used. A change here must be deliberate.
#[test]
fn float_results_have_fixed_bit_patterns() {
    let p = prepared(
        r"
fn main(x: Float) -> [Float] {
  [
    math.exp(x), math.exp(-x), math.ln(x), math.log2(x), math.log10(x), math.pow(x, 0.5),
    math.pow(x, 1.5), math.pow(x, -2.25), math.pow(10.0, -3.0), math.sqrt(x),
    math.round(x * 1000.0, 2), math.mean([x, 2.0, 3.5]) ?? 0.0, math.stdev([x, 2.0, 3.5]) ?? 0.0,
    math.median([x, 2.0, 3.5, 0.1]) ?? 0.0, math.percentile([x, 2.0, 3.5, 0.1], 37.5) ?? 0.0,
    vec.norm([x, 2.0, 3.5]), vec.cosine([x, 2.0, 3.5], [3.5, 2.0, x * 2.0]),
    math.entropy([x, 2.0, 3.5]), math.softmax([x, 2.0, 3.5])[0], math.variance([x, 2.0, 3.5]) ?? 0.0,
    math.exp(700.0), math.ln(1e-300), math.pow(2.0, 1000.0), math.log2(1e300),
  ]
}
",
    );
    let f = run(&p, "main", vec![Value::float(1.7)]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    let got: Vec<u64> = v
        .elements()
        .unwrap()
        .iter()
        .map(|x| x.as_float().unwrap().to_bits())
        .collect();
    let want: Vec<u64> = BITS.to_vec();
    let shown: Vec<String> = got.iter().map(|b| format!("{b:#018x}")).collect();
    assert_eq!(got, want, "{shown:#?}");
}

const BITS: [u64; 24] = [
    0x4015_e552_770d_f8a6,
    0x3fc7_622c_78a9_8a07,
    0x3fe0_fae8_1914_a991,
    0x3fe8_7f42_b972_949d,
    0x3fcd_7f59_aa5b_ecb9,
    0x3ff4_dc87_d614_51da,
    0x4001_bb73_75f7_abf9,
    0x3fd3_64e4_8754_ad19,
    0x3f50_624d_d2f1_a9fc,
    0x3ff4_dc87_d614_51da,
    0x409a_9000_0000_0000,
    0x4003_3333_3333_3333,
    0x3fee_dc14_25e8_0867,
    0x3ffd_9999_9999_999a,
    0x3ffb_cccc_cccc_cccd,
    0x4011_7fed_4678_7823,
    0x3fee_4e5c_2468_ca19,
    0x3ff8_2c9d_ad47_a8fe,
    0x3fbe_7a5d_0553_d63a,
    0x3fed_c28f_5c28_f5c3,
    0x7f0d_945d_f4f8_ec8e,
    0xc085_9634_47f8_7fb5,
    0x7e70_0000_0000_0000,
    0x408f_24a0_9f1a_8b89,
];

#[test]
fn labels_are_untouched_by_the_float_canonicalisation() {
    let p = prepared("fn main(x: Float) -> Float { math.sqrt(x) }");
    let f = run(&p, "main", vec![Value::float(-1.0).with_labels(SECRET)]);
    let Outcome::Returned(v) = f.outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_float().unwrap().to_bits(), 0x7ff8_0000_0000_0000);
    assert_eq!(v.labels(), Labels::source(0));
}

/// The longest common subsequence of two token lists, by the textbook dynamic program.
fn lcs(a: &[String], b: &[String]) -> usize {
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            table[i][j] = if a[i - 1] == b[j - 1] {
                table[i - 1][j - 1] + 1
            } else {
                table[i - 1][j].max(table[i][j - 1])
            };
        }
    }
    table[a.len()][b.len()]
}

/// `diff.lines` and `diff.words` are shortest edit scripts that rebuild both sides, and
/// `diff.apply` of `diff.unified` rebuilds the second, for many random small inputs.
#[test]
fn diffs_are_shortest_scripts_that_rebuild_both_sides() {
    let p = prepared(
        r"
fn lines(a: Str, b: Str) -> Untrusted<Str> { text.show(diff.lines(a, b)) }
fn words(a: Str, b: Str) -> Untrusted<Str> { text.show(diff.words(a, b)) }
fn round(a: Str, b: Str, c: Int) -> Str { diff.apply(a, diff.unified(a, b, context: c)) }
",
    );
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u64| {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d) % n
    };
    for case in 0..400 {
        let alphabet = 2 + next(3);
        let mut side = |len: u64| -> Vec<String> {
            (0..next(len + 1))
                .map(|_| ((b'a' + u8::try_from(next(alphabet)).unwrap()) as char).to_string())
                .collect()
        };
        let (a, b) = (side(14), side(14));
        for (name, joiner) in [("lines", "\n"), ("words", " ")] {
            let ta = a.join(joiner) + if a.is_empty() { "" } else { joiner };
            let tb = b.join(joiner) + if b.is_empty() { "" } else { joiner };
            let f = run(&p, name, vec![Value::text(&ta), Value::text(&tb)]);
            let Outcome::Returned(v) = f.outcome else {
                panic!("{name} raised on case {case}")
            };
            let json = v.as_text().unwrap().to_owned();
            let entries: Vec<(String, String)> = json
                .split("{\"op\":\"")
                .skip(1)
                .map(|e| {
                    let (op, rest) = e.split_once("\",\"text\":\"").unwrap();
                    (op.to_owned(), rest.split('"').next().unwrap().to_owned())
                })
                .collect();
            let keep = |ops: &[&str]| -> Vec<String> {
                entries
                    .iter()
                    .filter(|(o, _)| ops.contains(&o.as_str()))
                    .map(|(_, t)| t.clone())
                    .collect()
            };
            assert_eq!(keep(&["equal", "delete"]), a, "{name} case {case}: side a");
            assert_eq!(keep(&["equal", "insert"]), b, "{name} case {case}: side b");
            let equal = entries.iter().filter(|(o, _)| o == "equal").count();
            assert_eq!(equal, lcs(&a, &b), "{name} case {case}: not shortest");
        }
        // The patch applies exactly and rebuilds the second text, whatever the context.
        let ta = a.join("\n") + if a.is_empty() { "" } else { "\n" };
        let tb = b.join("\n") + if b.is_empty() { "" } else { "\n" };
        for context in [0, 1, 3] {
            let f = run(
                &p,
                "round",
                vec![Value::text(&ta), Value::text(&tb), Value::int(context)],
            );
            let Outcome::Returned(v) = f.outcome else {
                panic!("round raised on case {case} (context {context})")
            };
            assert_eq!(
                v.as_text(),
                Some(tb.as_str()),
                "case {case} context {context}"
            );
        }
    }
}

/// Text built piece by piece stops at 16 MiB (`Invalid`) and is charged for what it wrote, as a
/// result would be: the steps of the 16 MiB are debited before the refusal.
#[test]
fn text_over_16_mib_is_refused_and_paid_for() {
    let p = prepared(
        r#"
fn render(s: Str) -> Int { text.len(template.render(r"{{#xs}}{{s}}{{/xs}}", { xs: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20], s: s })) }
fn stable(s: Str) -> Int { text.len(hash.stable([s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s])) }
fn csv(s: Str) -> Int { text.len(codec.csv_write([{ a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }, { a: s }])) }
fn join(s: Str) -> Int { text.len(path.join([s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s])) }
"#,
    );
    // Twenty copies of 1 MiB: 20 MiB would be written.
    let mib = Value::text(&"x".repeat(1 << 20));
    for name in ["render", "stable", "csv", "join"] {
        let f = run(&p, name, vec![mib.clone()]);
        let Outcome::Raised(e) = f.outcome else {
            panic!("{name} returned")
        };
        assert_eq!(e.kind(&p), "Invalid", "{name}");
        // At least what was written (about 16 MiB, 262,144 steps) was paid for.
        assert!(
            f.steps >= (16 << 20) / 64 - 100,
            "{name} was charged only {} steps",
            f.steps
        );
        assert!(f.heap_peak < 40 << 20, "{name}: peak {}", f.heap_peak);
    }
    // A budget that cannot pay for it raises Budget instead, still without finishing the text.
    let tight = Limits {
        steps: 100_000,
        ..Limits::default()
    };
    let f = run_with(&p, "render", vec![mib], tight);
    let Outcome::Raised(e) = f.outcome else {
        panic!("returned")
    };
    assert_eq!(e.budget(&p).as_deref(), Some("steps"));
}
