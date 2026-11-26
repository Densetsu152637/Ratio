//! Label propagation for every stage 9 built-in (chapter 03, "Secrecy": a result carries the
//! labels of everything that decided it).
//!
//! Each row is a call with its parameters. For **every argument** of the call, and for every way
//! that argument can hold a label (the value itself; for a list, its first element), the call is
//! run with only that one label set and its result must carry it. A call that builds a map or a
//! record in the program from scalar parameters covers keys and values the same way. A second
//! table runs calls that raise `Invalid`, and the error's message must carry the labels of the
//! arguments that decided the failure.

#![allow(clippy::too_many_lines, reason = "the rows are one table")]

mod support;

use ratio_eval::{Labels, Outcome, Value};
use support::{SECRET, prepared, run};

/// Ways an argument holds its label.
const OWN: u8 = 1;
const LEAF: u8 = 2;

/// One argument: a value for each way of labelling it.
struct Arg {
    make: Box<dyn Fn(Labels, Labels) -> Value>,
    modes: u8,
}

fn float(x: f64) -> Arg {
    Arg {
        make: Box::new(move |own, _| Value::float(x).with_labels(own)),
        modes: OWN,
    }
}

fn int(x: i64) -> Arg {
    Arg {
        make: Box::new(move |own, _| Value::int(x).with_labels(own)),
        modes: OWN,
    }
}

fn boolean(x: bool) -> Arg {
    Arg {
        make: Box::new(move |own, _| Value::bool(x).with_labels(own)),
        modes: OWN,
    }
}

fn text(s: &'static str) -> Arg {
    Arg {
        make: Box::new(move |own, _| Value::text(s).with_labels(own)),
        modes: OWN,
    }
}

fn floats(xs: &'static [f64]) -> Arg {
    Arg {
        make: Box::new(move |own, leaf| {
            let items = xs
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    Value::float(*x).with_labels(if i == 0 { leaf } else { Labels::NONE })
                })
                .collect();
            Value::list(items).unwrap().with_labels(own)
        }),
        modes: if xs.is_empty() { OWN } else { OWN | LEAF },
    }
}

fn ints(xs: &'static [i64]) -> Arg {
    Arg {
        make: Box::new(move |own, leaf| {
            let items = xs
                .iter()
                .enumerate()
                .map(|(i, x)| Value::int(*x).with_labels(if i == 0 { leaf } else { Labels::NONE }))
                .collect();
            Value::list(items).unwrap().with_labels(own)
        }),
        modes: OWN | LEAF,
    }
}

fn strs(xs: &'static [&'static str]) -> Arg {
    Arg {
        make: Box::new(move |own, leaf| {
            let items = xs
                .iter()
                .enumerate()
                .map(|(i, x)| Value::text(x).with_labels(if i == 0 { leaf } else { Labels::NONE }))
                .collect();
            Value::list(items).unwrap().with_labels(own)
        }),
        modes: OWN | LEAF,
    }
}

fn vectors(xs: &'static [&'static [f64]]) -> Arg {
    Arg {
        make: Box::new(move |own, leaf| {
            let items = xs
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let inner = v
                        .iter()
                        .enumerate()
                        .map(|(j, x)| {
                            Value::float(*x).with_labels(if i == 0 && j == 0 {
                                leaf
                            } else {
                                Labels::NONE
                            })
                        })
                        .collect();
                    Value::list(inner).unwrap()
                })
                .collect();
            Value::list(items).unwrap().with_labels(own)
        }),
        modes: if xs.is_empty() { OWN } else { OWN | LEAF },
    }
}

/// A row: `name(params)` calls `call`.
struct Row {
    name: &'static str,
    params: &'static str,
    call: &'static str,
    args: Vec<Arg>,
}

fn row(name: &'static str, params: &'static str, call: &'static str, args: Vec<Arg>) -> Row {
    Row {
        name,
        params,
        call,
        args,
    }
}

fn source(r: &Row) -> String {
    format!(
        "fn main({}) -> Untrusted<Str> {{ text.show({}) }}\n",
        r.params, r.call
    )
}

/// Runs the row with argument `which` labelled in `mode`; the result's labels.
fn labels_of(r: &Row, which: usize, mode: u8) -> (Outcome, Labels) {
    let p = prepared(&source(r));
    let args: Vec<Value> = r
        .args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let (own, leaf) = match (i == which, mode) {
                (true, OWN) => (SECRET, Labels::NONE),
                (true, _) => (Labels::NONE, SECRET),
                _ => (Labels::NONE, Labels::NONE),
            };
            (a.make)(own, leaf)
        })
        .collect();
    let finished = run(&p, "main", args);
    let labels = match &finished.outcome {
        Outcome::Returned(v) => v.scan(),
        Outcome::Raised(e) => e.value().scan(),
    };
    (finished.outcome, labels)
}

fn rows() -> Vec<Row> {
    vec![
        row("math.sign", "x: Float", "math.sign(x)", vec![float(2.5)]),
        row(
            "math.clamp",
            "x: Int, lo: Int, hi: Int",
            "math.clamp(x, lo, hi)",
            vec![int(5), int(1), int(9)],
        ),
        row("math.floor", "x: Float", "math.floor(x)", vec![float(2.5)]),
        row("math.ceil", "x: Float", "math.ceil(x)", vec![float(2.5)]),
        row("math.trunc", "x: Float", "math.trunc(x)", vec![float(2.5)]),
        row(
            "math.round",
            "x: Float, d: Int",
            "math.round(x, d)",
            vec![float(2.675), int(2)],
        ),
        row("math.to_float", "x: Int", "math.to_float(x)", vec![int(3)]),
        row("math.sqrt", "x: Float", "math.sqrt(x)", vec![float(2.0)]),
        row(
            "math.pow",
            "x: Float, y: Float",
            "math.pow(x, y)",
            vec![float(2.0), float(3.0)],
        ),
        row(
            "math.pow",
            "x: Int, y: Int",
            "math.pow(x, y)",
            vec![int(2), int(3)],
        ),
        row("math.exp", "x: Float", "math.exp(x)", vec![float(1.0)]),
        row("math.ln", "x: Float", "math.ln(x)", vec![float(2.0)]),
        row("math.log2", "x: Float", "math.log2(x)", vec![float(8.0)]),
        row(
            "math.log10",
            "x: Float",
            "math.log10(x)",
            vec![float(100.0)],
        ),
        row(
            "math.is_nan",
            "x: Float",
            "math.is_nan(x)",
            vec![float(1.0)],
        ),
        row(
            "math.mean",
            "xs: [Float]",
            "math.mean(xs)",
            vec![floats(&[1.0, 2.0, 4.0])],
        ),
        row(
            "math.mean",
            "xs: [Float]",
            "math.mean(xs)",
            vec![floats(&[])],
        ),
        row(
            "math.median",
            "xs: [Float]",
            "math.median(xs)",
            vec![floats(&[3.0, 1.0, 2.0])],
        ),
        row(
            "math.mode",
            "xs: [Str]",
            "math.mode(xs)",
            vec![strs(&["a", "b", "b"])],
        ),
        row(
            "math.variance",
            "xs: [Float], s: Bool",
            "math.variance(xs, sample: s)",
            vec![floats(&[1.0, 2.0, 4.0]), boolean(true)],
        ),
        row(
            "math.stdev",
            "xs: [Float], s: Bool",
            "math.stdev(xs, sample: s)",
            vec![floats(&[1.0, 2.0, 4.0]), boolean(true)],
        ),
        row(
            "math.percentile",
            "xs: [Float], p: Float",
            "math.percentile(xs, p)",
            vec![floats(&[1.0, 2.0, 4.0]), float(50.0)],
        ),
        row(
            "math.softmax",
            "xs: [Float], t: Float",
            "math.softmax(xs, temperature: t)",
            vec![floats(&[1.0, 2.0]), float(1.0)],
        ),
        row(
            "math.argmax",
            "xs: [Int]",
            "math.argmax(xs)",
            vec![ints(&[1, 3, 2])],
        ),
        row(
            "math.argmin",
            "xs: [Int]",
            "math.argmin(xs)",
            vec![ints(&[3, 1, 2])],
        ),
        row(
            "math.entropy",
            "ps: [Float]",
            "math.entropy(ps)",
            vec![floats(&[1.0, 1.0])],
        ),
        row("hash.sha256", "s: Str", "hash.sha256(s)", vec![text("abc")]),
        row("hash.blake3", "s: Str", "hash.blake3(s)", vec![text("abc")]),
        row(
            "hash.stable",
            "v: [Int]",
            "hash.stable(v)",
            vec![ints(&[1, 2])],
        ),
        row(
            "hash.stable",
            "k: Str, v: Int",
            "hash.stable(map.set({}, k, v))",
            vec![text("a"), int(1)],
        ),
        row(
            "hash.short",
            "v: [Int], n: Int",
            "hash.short(v, n)",
            vec![ints(&[1, 2]), int(4)],
        ),
        row(
            "diff.lines",
            "a: Str, b: Str",
            "diff.lines(a, b)",
            vec![text("a\nb\n"), text("a\nc\n")],
        ),
        row(
            "diff.words",
            "a: Str, b: Str",
            "diff.words(a, b)",
            vec![text("a b"), text("a c")],
        ),
        row(
            "diff.unified",
            "a: Str, b: Str, c: Int, f: Str, t: Str",
            "diff.unified(a, b, context: c, from: f, to: t)",
            vec![text("a\nb\n"), text("a\nc\n"), int(1), text("x"), text("y")],
        ),
        row(
            "diff.apply",
            "t: Str, p: Str",
            "diff.apply(t, p)",
            vec![text("a\nb\n"), text("@@ -1,2 +1,2 @@\n a\n-b\n+c\n")],
        ),
        row(
            "diff.stats",
            "a: Str, b: Str",
            "diff.stats(diff.lines(a, b))",
            vec![text("a\nb\n"), text("a\nc\n")],
        ),
        row(
            "template.render",
            "n: Str, k: Int",
            "template.render(r\"{{n}} {{#xs}}{{.}}{{/xs}} {{k}}\", { n: n, xs: [k], k: k })",
            vec![text("Ann"), int(1)],
        ),
        row(
            "template.render",
            "k: Str, v: Str",
            "template.render(r\"{{a}}\", map.set({}, k, v))",
            vec![text("a"), text("x")],
        ),
        row(
            "template.render",
            "t: Str",
            "template.render(t, { a: 1 })",
            vec![text("{{a}}")],
        ),
        row(
            "template.render",
            "b: Bool",
            "template.render(r\"{{#f}}yes{{/f}}\", { f: b })",
            vec![boolean(true)],
        ),
        row(
            "template.names",
            "t: Str",
            "template.names(t)",
            vec![text("{{a}}")],
        ),
        row(
            "vec.dot",
            "a: [Float], b: [Float]",
            "vec.dot(a, b)",
            vec![floats(&[1.0, 2.0]), floats(&[3.0, 4.0])],
        ),
        row(
            "vec.norm",
            "a: [Float]",
            "vec.norm(a)",
            vec![floats(&[3.0, 4.0])],
        ),
        row(
            "vec.normalize",
            "a: [Float]",
            "vec.normalize(a)",
            vec![floats(&[3.0, 4.0])],
        ),
        row(
            "vec.cosine",
            "a: [Float], b: [Float]",
            "vec.cosine(a, b)",
            vec![floats(&[1.0, 2.0]), floats(&[3.0, 4.0])],
        ),
        row(
            "vec.add",
            "a: [Float], b: [Float]",
            "vec.add(a, b)",
            vec![floats(&[1.0, 2.0]), floats(&[3.0, 4.0])],
        ),
        row(
            "vec.mean",
            "vs: [[Float]]",
            "vec.mean(vs)",
            vec![vectors(&[&[1.0, 2.0], &[3.0, 4.0]])],
        ),
        row(
            "vec.mean",
            "vs: [[Float]]",
            "vec.mean(vs)",
            vec![vectors(&[])],
        ),
        row(
            "vec.top_k",
            "q: [Float], vs: [[Float]], k: Int",
            "vec.top_k(q, vs, k)",
            vec![
                floats(&[1.0, 0.0]),
                vectors(&[&[1.0, 0.0], &[0.0, 1.0]]),
                int(1),
            ],
        ),
        row(
            "path.join",
            "ps: [Str]",
            "path.join(ps)",
            vec![strs(&["a", "b"])],
        ),
        row(
            "path.parent",
            "p: Str",
            "path.parent(p)",
            vec![text("/a/b")],
        ),
        row("path.name", "p: Str", "path.name(p)", vec![text("/a/b")]),
        row("path.stem", "p: Str", "path.stem(p)", vec![text("/a/b.c")]),
        row("path.ext", "p: Str", "path.ext(p)", vec![text("/a/b.c")]),
        row("path.ext", "p: Str", "path.ext(p)", vec![text("/a/b")]),
        row(
            "path.normalize",
            "p: Str",
            "path.normalize(p)",
            vec![text("/a/../b")],
        ),
        row(
            "path.is_absolute",
            "p: Str",
            "path.is_absolute(p)",
            vec![text("/a")],
        ),
        row(
            "path.relative_to",
            "p: Str, b: Str",
            "path.relative_to(p, b)",
            vec![text("/a/b"), text("/a")],
        ),
        row(
            "path.relative_to",
            "p: Str, b: Str",
            "path.relative_to(p, b)",
            vec![text("/x"), text("/a")],
        ),
        row(
            "url.parse",
            "s: Str",
            "url.parse(s)",
            vec![text("http://a.com/p?q=1#f")],
        ),
        row(
            "url.parse",
            "s: Str",
            "url.parse(s)",
            vec![text("not a url")],
        ),
        row(
            "url.host",
            "s: Str",
            "url.host(s)",
            vec![text("http://a.com/")],
        ),
        row(
            "url.with_query",
            "s: Str, k: Str, v: Str",
            "url.with_query(s, map.set({}, k, v))",
            vec![text("http://a.com/?x=1"), text("y"), text("2")],
        ),
        row("url.encode", "s: Str", "url.encode(s)", vec![text("a b")]),
        row("url.decode", "s: Str", "url.decode(s)", vec![text("a%20b")]),
        row("url.decode", "s: Str", "url.decode(s)", vec![text("%FF")]),
        row(
            "codec.base64_encode",
            "s: Str",
            "codec.base64_encode(s)",
            vec![text("hello")],
        ),
        row(
            "codec.base64_decode",
            "s: Str",
            "codec.base64_decode(s)",
            vec![text("aGVsbG8=")],
        ),
        row(
            "codec.base64_decode",
            "s: Str",
            "codec.base64_decode(s)",
            vec![text("!!")],
        ),
        row(
            "codec.hex_encode",
            "s: Str",
            "codec.hex_encode(s)",
            vec![text("hi")],
        ),
        row(
            "codec.hex_decode",
            "s: Str",
            "codec.hex_decode(s)",
            vec![text("6869")],
        ),
        row(
            "codec.hex_decode",
            "s: Str",
            "codec.hex_decode(s)",
            vec![text("zz")],
        ),
        row(
            "codec.csv_parse",
            "s: Str, h: Bool, d: Str",
            "codec.csv_parse(s, header: h, delimiter: d)",
            vec![text("a,b\n1,2\n"), boolean(true), text(",")],
        ),
        row(
            "codec.csv_write",
            "k: Str, v: Str, d: Str",
            "codec.csv_write([map.set({}, k, v)], delimiter: d)",
            vec![text("a"), text("1"), text(",")],
        ),
    ]
}

#[test]
fn every_argument_of_every_stage_9_built_in_reaches_its_result() {
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for r in rows() {
        seen.insert(r.name);
        for (which, arg) in r.args.iter().enumerate() {
            for mode in [OWN, LEAF] {
                if arg.modes & mode == 0 {
                    continue;
                }
                let (outcome, labels) = labels_of(&r, which, mode);
                if let Outcome::Raised(_) = outcome {
                    problems.push(format!("{} raised on its sample arguments", r.call));
                } else if !labels.contains(SECRET) || labels.is_empty() {
                    problems.push(format!(
                        "{}: argument {which} labelled {} does not reach the result",
                        r.call,
                        if mode == OWN { "itself" } else { "inside" }
                    ));
                }
            }
        }
    }
    // Every built-in of the stage has a row (math.abs, min and max are stage 6's).
    for (name, _) in ratio_eval::builtins::implemented() {
        let ours = [
            "math.",
            "hash.",
            "diff.",
            "template.",
            "vec.",
            "path.",
            "url.",
            "codec.",
        ];
        if ours.iter().any(|m| name.starts_with(m))
            && !matches!(name, "math.abs" | "math.min" | "math.max")
            && !seen.contains(name)
        {
            problems.push(format!("no label row for `{name}`"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Calls that raise `Invalid`, with the arguments that decide it.
fn failures() -> Vec<(Row, &'static [usize])> {
    vec![
        (
            row(
                "math.clamp",
                "x: Int, lo: Int, hi: Int",
                "math.clamp(x, lo, hi)",
                vec![int(5), int(9), int(1)],
            ),
            &[1, 2],
        ),
        (
            row(
                "math.floor",
                "x: Float",
                "math.floor(x)",
                vec![float(1e300)],
            ),
            &[0],
        ),
        (
            row(
                "math.pow",
                "x: Int, y: Int",
                "math.pow(x, y)",
                vec![int(2), int(-1)],
            ),
            &[0, 1],
        ),
        (
            row(
                "math.percentile",
                "xs: [Float], p: Float",
                "math.percentile(xs, p)",
                vec![floats(&[1.0]), float(200.0)],
            ),
            &[1],
        ),
        (
            row(
                "math.softmax",
                "xs: [Float], t: Float",
                "math.softmax(xs, temperature: t)",
                vec![floats(&[1.0]), float(0.0)],
            ),
            &[1],
        ),
        (
            row(
                "math.entropy",
                "ps: [Float]",
                "math.entropy(ps)",
                vec![floats(&[-1.0])],
            ),
            &[0],
        ),
        (
            row(
                "hash.stable",
                "v: [Float]",
                "hash.stable(v)",
                vec![floats(&[f64::INFINITY])],
            ),
            &[0],
        ),
        (
            row(
                "diff.unified",
                "a: Str, c: Int",
                "diff.unified(a, \"b\", context: c)",
                vec![text("a"), int(-1)],
            ),
            &[1],
        ),
        (
            row(
                "diff.apply",
                "t: Str, p: Str",
                "diff.apply(t, p)",
                vec![text("a\n"), text("@@ -1 +1 @@\n-z\n+y\n")],
            ),
            &[0, 1],
        ),
        (
            row(
                "template.render",
                "n: Str",
                "template.render(r\"{{n}} {{missing}}\", { n: n })",
                vec![text("Ann")],
            ),
            &[0],
        ),
        (
            row(
                "template.names",
                "t: Str",
                "template.names(t)",
                vec![text("{{")],
            ),
            &[0],
        ),
        (
            row(
                "vec.dot",
                "a: [Float], b: [Float]",
                "vec.dot(a, b)",
                vec![floats(&[1.0]), floats(&[1.0, 2.0])],
            ),
            &[0, 1],
        ),
        (
            row(
                "vec.normalize",
                "a: [Float]",
                "vec.normalize(a)",
                vec![floats(&[0.0])],
            ),
            &[0],
        ),
        (
            row(
                "vec.top_k",
                "q: [Float], vs: [[Float]], k: Int",
                "vec.top_k(q, vs, k)",
                vec![floats(&[1.0]), vectors(&[&[1.0, 2.0]]), int(1)],
            ),
            &[0, 1],
        ),
        (
            row(
                "url.with_query",
                "s: Str",
                "url.with_query(s, {})",
                vec![text("not a url")],
            ),
            &[0],
        ),
        (
            row(
                "codec.csv_parse",
                "s: Str",
                "codec.csv_parse(s)",
                vec![text("a,b\n1\n")],
            ),
            &[0],
        ),
        (
            row(
                "codec.csv_parse",
                "d: Str",
                "codec.csv_parse(\"a\\n1\", delimiter: d)",
                vec![text("ab")],
            ),
            &[0],
        ),
        (
            row(
                "codec.csv_write",
                "k: Str",
                "codec.csv_write([map.set({}, k, \"1\"), map.set({}, \"z\", \"2\")])",
                vec![text("a")],
            ),
            &[0],
        ),
    ]
}

#[test]
fn a_failure_is_told_with_the_labels_of_what_decided_it() {
    let mut problems = Vec::new();
    for (r, deciders) in failures() {
        for &which in deciders {
            let (outcome, labels) = labels_of(&r, which, OWN);
            match outcome {
                Outcome::Raised(e) => {
                    let p = prepared(&source(&r));
                    if e.kind(&p) != "Invalid" {
                        problems.push(format!("{}: raised {}", r.call, e.kind(&p)));
                    }
                    if !labels.contains(SECRET) || labels.is_empty() {
                        problems.push(format!(
                            "{}: the error does not carry the labels of argument {which}",
                            r.call
                        ));
                    }
                }
                Outcome::Returned(_) => problems.push(format!("{}: did not raise", r.call)),
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
