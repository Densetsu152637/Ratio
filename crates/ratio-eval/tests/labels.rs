//! Run-time secrecy labels (chapter 03, "Secrecy"): every operator propagates the labels of
//! its operands to its result, `Json` keeps them on its leaves, reading out of a container
//! keeps the container's, and only `declassify` removes them.

mod support;

use std::sync::Arc;

use ratio_eval::{Labels, Limits, Machine, Outcome, Prepared, Suspend, Value, sink};
use support::{SECRET, prepared, run, secret, secret_int, value};

/// The labels anywhere in what `name` returns for `args`, checked against the summary.
fn labels(p: &Arc<Prepared>, name: &str, args: Vec<Value>) -> Labels {
    let v = value(p, name, args);
    assert!(v.summaries_hold(), "`{name}`: a summary misses a label");
    assert!(
        v.quick().contains(v.scan()),
        "`{name}`: quick() misses a label"
    );
    v.scan()
}

fn assert_secret(p: &Arc<Prepared>, name: &str, args: Vec<Value>) {
    assert_eq!(labels(p, name, args), SECRET, "`{name}` dropped the label");
}

fn assert_clean(p: &Arc<Prepared>, name: &str, args: Vec<Value>) {
    assert!(
        labels(p, name, args).is_empty(),
        "`{name}` invented a label"
    );
}

const OPERATORS: &str = r#"
fn neg(a: Int) -> Int { -a }
fn not_(a: Bool) -> Bool { not a }
fn add(a: Int, b: Int) -> Int { a + b }
fn sub(a: Int, b: Int) -> Int { a - b }
fn mul(a: Int, b: Int) -> Int { a * b }
fn div(a: Int, b: Int) -> Int { a / b }
fn rem(a: Int, b: Int) -> Int { a % b }
fn fadd(a: Float, b: Float) -> Float { a + b }
fn dmul(a: Duration, b: Int) -> Duration { a * b }
fn madd(a: Money, b: Money) -> Money { a + b }
fn eq(a: Int, b: Int) -> Bool { a == b }
fn ne(a: Str, b: Str) -> Bool { a != b }
fn lt(a: Int, b: Int) -> Bool { a < b }
fn le(a: Str, b: Str) -> Bool { a <= b }
fn gt(a: Int, b: Int) -> Bool { a > b }
fn ge(a: Int, b: Int) -> Bool { a >= b }
fn concat(a: Str, b: Str) -> Str { a + b }
fn interp(a: Str) -> Str { "<{a}>" }
fn spec(a: Int) -> Str { "{a:>6}" }
fn range_in(a: Int) -> Bool { 3 in 0..a }
fn text_in(a: Str) -> Bool { "x" in a }
fn list_in(a: Int) -> Bool { a in [1, 2] }
fn map_in(a: Str) -> Bool { let m: Map<Str, Int> = { k: 1 }; a in m }
fn and_(a: Bool) -> Bool { true and a }
fn or_(a: Bool) -> Bool { false or a }
fn coalesce_none(a: Int?) -> Int { a ?? 0 }
fn list_eq(a: Int) -> Bool { [a] == [1] }
fn list_add(a: Int) -> [Int] { [1] + [a] }
fn index(a: Int) -> Int { [10, 20, 30][a] }
fn index_of_secret_list(a: Int) -> Int { let xs = [a, 2]; xs[1] }
fn map_index(a: Str) -> Int { let m: Map<Str, Int> = { k: 1, j: 2 }; m[a] }
fn field(a: Int) -> Int { let r = { x: a, y: 2 }; r.x }
fn other_field(a: Int) -> Int { let r = { x: a, y: 2 }; r.y }
fn spread(a: Int) -> { x: Int, y: Int } { let r = { x: a, y: 2 }; { ..r, y: 3 } }
fn opt_chain(a: { n: Int }?) -> Int? { a?.n }
fn is_(a: Result<Int>) -> Bool { a is Ok }
fn tuple_part(a: Int) -> Int { let (x, y) = (a, 1); x }
fn list_pattern(a: Int) -> [Int] { match [a, 2, 3] { [_, ..rest] => rest, _ => [] } }
fn for_loop(a: [Int]) -> Int { var n = 0; for x in a { n += x }; n }
fn closure_capture(a: Int) -> Int { let f = || a + 1; f() }
fn assign_index(a: Int) -> [Int] { var xs = [1, 2]; xs[a] = 5; xs }
fn caught_message(a: Str) -> Untrusted<Str> { try { raise fail("t", a) } catch e { e.message } }
fn variant(a: Int) -> Result<Int> { Ok { value: a } }
fn list_lit(a: Int) -> [Int] { [1, a] }
"#;

#[test]
fn every_operator_propagates_labels() {
    let p = prepared(OPERATORS);
    let s = secret_int;
    let i = Value::int;
    assert_secret(&p, "neg", vec![s(2)]);
    assert_secret(&p, "not_", vec![Value::bool(true).with_labels(SECRET)]);
    for op in ["add", "sub", "mul", "div", "rem", "eq", "lt", "gt", "ge"] {
        assert_secret(&p, op, vec![s(7), i(2)]);
        assert_secret(&p, op, vec![i(7), s(2)]);
        assert_clean(&p, op, vec![i(7), i(2)]);
    }
    assert_secret(
        &p,
        "fadd",
        vec![Value::float(1.0), Value::float(2.0).with_labels(SECRET)],
    );
    assert_secret(&p, "dmul", vec![Value::duration(5), s(3)]);
    assert_secret(
        &p,
        "madd",
        vec![Value::money(5).with_labels(SECRET), Value::money(1)],
    );
    for op in ["ne", "le", "concat"] {
        assert_secret(&p, op, vec![secret("a"), Value::text("b")]);
        assert_secret(&p, op, vec![Value::text("a"), secret("b")]);
        assert_clean(&p, op, vec![Value::text("a"), Value::text("b")]);
    }
    assert_secret(&p, "interp", vec![secret("x")]);
    assert_secret(&p, "spec", vec![s(4)]);
    assert_secret(&p, "range_in", vec![s(5)]);
    assert_secret(&p, "text_in", vec![secret("xyz")]);
    assert_secret(&p, "list_in", vec![s(1)]);
    assert_secret(&p, "map_in", vec![secret("k")]);
    assert_secret(&p, "and_", vec![Value::bool(true).with_labels(SECRET)]);
    assert_secret(&p, "or_", vec![Value::bool(false).with_labels(SECRET)]);
    // `a ?? 0` takes the default because `a` is none: the choice is labelled.
    assert_secret(&p, "coalesce_none", vec![Value::none().with_labels(SECRET)]);
    assert_secret(&p, "coalesce_none", vec![s(3)]);
    assert_secret(&p, "list_eq", vec![s(1)]);
    assert_secret(&p, "list_add", vec![s(1)]);
    // The index selects the element, so the element read is labelled.
    assert_secret(&p, "index", vec![s(1)]);
    assert_secret(&p, "map_index", vec![secret("j")]);
    assert_secret(&p, "field", vec![s(1)]);
    assert_secret(&p, "spread", vec![s(1)]);
    assert_secret(&p, "tuple_part", vec![s(1)]);
    assert_secret(&p, "for_loop", vec![Value::list(vec![i(1), s(2)]).unwrap()]);
    assert_secret(
        &p,
        "for_loop",
        vec![Value::list(vec![i(1), i(2)]).unwrap().with_labels(SECRET)],
    );
    assert_secret(&p, "closure_capture", vec![s(1)]);
    assert_secret(&p, "assign_index", vec![s(1)]);
    assert_secret(&p, "caught_message", vec![secret("why")]);
    assert_secret(&p, "variant", vec![s(3)]);
    assert_secret(&p, "list_lit", vec![s(3)]);
    let record = Value::record(vec![("n", s(1))]).unwrap();
    assert_secret(&p, "opt_chain", vec![record]);
    assert_secret(&p, "opt_chain", vec![Value::none().with_labels(SECRET)]);
    let ok = value(&p, "variant", vec![s(4)]);
    assert_secret(&p, "is_", vec![ok.with_labels(SECRET)]);
    // Parts the secret did not reach stay clean.
    assert_clean(&p, "other_field", vec![s(1)]);
    assert_clean(&p, "index_of_secret_list", vec![s(1)]);
    assert_clean(&p, "list_pattern", vec![s(1)]);
}

const BUILTINS: &str = r#"
fn len(a: Str) -> Int { text.len(a) }
fn upper(a: Str) -> Str { text.upper(a) }
fn trimmed(a: Str) -> Str { text.trim(a) }
fn contains(a: Str) -> Bool { text.contains(a, "x") }
fn split(a: Str) -> (Str, Str)? { text.split_once(a, "=") }
fn joined(a: Str) -> Str { text.join(["x", a], ",") }
fn shown(a: Int) -> Untrusted<Str> { text.show([a]) }
fn list_len(a: [Int]) -> Int { list.len(a) }
fn list_get(a: Int) -> Int? { list.get([1, 2], a) }
fn pushed(a: Int) -> [Int] { [1].push(a) }
fn mapped(a: Int) -> [Int] { [1, 2].map(|x| x + a) }
fn filtered(a: Int) -> [Int] { [1, 2, 3].filter(|x| x > a) }
fn filter_mapped(a: Int) -> [Int] { [1, 2, 3].filter_map(|x| list.get([x], a)) }
fn folded(a: Int) -> Int { [1, 2].fold(a, |s, x| s + x) }
fn sorted(a: Int) -> [Int] { [3, 1, 2].sort_by(|x| x * a) }
fn taken(a: Int) -> [Int] { [1, 2, 3].take(a) }
fn summed(a: Int) -> Int { list.sum([1, a]) }
fn has(a: Int) -> Bool { [1, 2].contains(a) }
fn map_get(a: Str) -> Int? { let m: Map<Str, Int> = { k: 1 }; map.get(m, a) }
fn map_set(a: Str) -> Int { let m: Map<Str, Int> = { k: 1 }; list.len(map.keys(map.set(m, a, 2))) }
fn abs_(a: Int) -> Int { math.abs(a) }
fn min_(a: Int) -> Int { math.min(a, 5) }
fn encoded(a: Str) -> Json { json.encode::<{ s: Str, n: Int }>({ s: a, n: 1 }) }
fn round_trip(a: Str) -> Untrusted<Str> { json.decode::<{ s: Str, n: Int }>(json.encode::<{ s: Str, n: Int }>({ s: a, n: 1 })).s }
fn untouched(a: Str) -> Int { json.decode::<{ s: Str, n: Int }>(json.encode::<{ s: Str, n: Int }>({ s: a, n: 1 })).n }
fn parsed(a: Str) -> Json? { json.get(json.parse(a), "/k") }
fn result_of(a: Int) -> Result<Int> { result(|| a) }
"#;

#[test]
fn built_ins_and_json_propagate_labels() {
    let p = prepared(BUILTINS);
    let s = secret_int;
    for f in ["len", "upper", "trimmed", "contains", "split", "joined"] {
        assert_secret(&p, f, vec![secret("a=b x")]);
    }
    assert_secret(&p, "shown", vec![s(1)]);
    assert_secret(
        &p,
        "list_len",
        vec![
            Value::list(vec![Value::int(1)])
                .unwrap()
                .with_labels(SECRET),
        ],
    );
    for f in [
        "list_get",
        "pushed",
        "mapped",
        "filtered",
        "filter_mapped",
        "folded",
        "sorted",
        "taken",
        "summed",
        "has",
        "abs_",
        "min_",
        "result_of",
    ] {
        assert_secret(&p, f, vec![s(1)]);
    }
    assert_secret(&p, "map_get", vec![secret("k")]);
    assert_secret(&p, "map_set", vec![secret("z")]);
    // `Json` keeps labels on its leaves, through `encode`, `decode` and `parse`.
    assert_secret(&p, "encoded", vec![secret("x")]);
    assert_secret(&p, "round_trip", vec![secret("x")]);
    assert_clean(&p, "untouched", vec![secret("x")]);
    assert_secret(&p, "parsed", vec![secret("{\"k\": 1}")]);
    // A clean list's length is clean even with a secret element: its shape is not secret.
    assert_clean(&p, "list_len", vec![Value::list(vec![s(1)]).unwrap()]);
}

#[test]
fn errors_raised_by_operations_on_secrets_are_labelled() {
    let p = prepared(
        r#"fn f(a: Int) -> Untrusted<Str> { try { [1][a]; "none" } catch e { e.message } }"#,
    );
    assert_secret(&p, "f", vec![secret_int(9)]);
    let raised = run(&p, "f", vec![secret_int(0)]);
    assert!(matches!(raised.outcome, Outcome::Returned(_)));
}

/// Drives a machine by hand, answering its effects with `answer`.
fn drive(
    p: &Arc<Prepared>,
    name: &str,
    args: Vec<Value>,
    answer: &dyn Fn(&ratio_eval::EffectRequest) -> Result<Value, ratio_eval::RtError>,
) -> Outcome {
    let entry = p.find("main.ratio", name).unwrap();
    let mut m = Machine::new(
        Arc::clone(p),
        entry,
        args,
        Limits::default(),
        ratio_eval::CancelToken::new(),
    )
    .unwrap();
    loop {
        match m.run() {
            Suspend::Yield => {}
            Suspend::Wait(w) => w.wait(),
            Suspend::Effect(req) => {
                // Every argument crossing to the host passes the sink check (debug builds
                // assert the summaries agree with a full scan).
                for a in req.args.iter().flatten() {
                    let _ = sink::check(a);
                }
                let r = answer(&req);
                m.resume_effect(r);
            }
            Suspend::Branches(_) | Suspend::Fence(_) => {
                panic!("hand-driven machines run no concurrent constructs")
            }
            Suspend::Done(o) => return o,
        }
    }
}

#[test]
fn only_declassify_removes_labels() {
    let src = r#"
type Note = { text: Str, n: Int }
fn open(x: Secret<Note>) -> Note { declassify(x, because: "the user agreed") }
"#;
    let p = prepared(src);
    let note = Value::record(vec![("text", secret("pin")), ("n", secret_int(4))])
        .unwrap()
        .with_labels(SECRET);
    assert_eq!(note.scan(), SECRET);
    // The host answers only whether the user agreed; the interpreter removes the labels.
    let o = drive(&p, "open", vec![note.clone()], &|req| {
        assert_eq!(&*req.builtin, "declassify");
        Ok(Value::bool(true))
    });
    let Outcome::Returned(v) = o else {
        panic!("raised")
    };
    assert!(v.scan().is_empty());
    assert_eq!(v.field("text").unwrap().as_text(), Some("pin"));
    assert!(sink::check(&v).is_ok());
    // Refused: `Denied`, and nothing comes back.
    let o = drive(&p, "open", vec![note.clone()], &|_| Ok(Value::bool(false)));
    let Outcome::Raised(e) = o else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Denied");
    // A host cannot hand back a different, unlabelled value: any answer but a Bool is refused.
    let o = drive(&p, "open", vec![note], &|_| Ok(Value::text("forged")));
    let Outcome::Raised(e) = o else {
        panic!("returned")
    };
    assert_eq!(e.kind(&p), "Invalid");
}

#[test]
fn the_sink_check_refuses_labelled_values_and_skips_clean_ones() {
    let clean = Value::list(vec![Value::text("a"), Value::int(1)]).unwrap();
    assert!(sink::check(&clean).is_ok());
    assert!(!clean.may_be_labelled());
    let inner = Value::list(vec![Value::text("a"), secret("b")]).unwrap();
    let outer = Value::tuple(vec![
        Value::int(1),
        Value::record(vec![("x", inner)]).unwrap(),
    ])
    .unwrap();
    assert!(outer.may_be_labelled());
    assert_eq!(sink::check(&outer), Err(SECRET));
}

#[test]
fn defaults_and_effect_free_code_never_label() {
    let p = prepared("fn main() -> [Str] { [\"a\", \"b\"].map(|x| x + \"!\") }");
    assert!(labels(&p, "main", Vec::new()).is_empty());
}
