//! Regression tests for the stage 10 security review (round 1, probes P1 to P9): effects
//! and secrets in harness defaults and configuration, secrets in settings, `with` blocks that
//! drop `secrets`, the cost of opening a frame, freed ledger nodes, and which scope may catch
//! a `Budget`.

mod support;

use std::sync::Arc;

use ratio_eval::{CancelToken, Limits, Outcome, Prepared, Suspend, Value};

/// The checker's codes for a source, which must not check cleanly.
fn refused_codes(source: &str) -> Vec<String> {
    let loaded = support::load_files(&[("main.ratio", source)]);
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics);
    let checked = ratio_check::check(&loaded);
    assert!(!checked.is_clean(), "the program checked cleanly");
    checked
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn p1_p5_p6_defaults_and_configuration_are_effect_free() {
    // P1: an effect in a default would run with the caller's tools, before the frame opens.
    let p1 = r#"
harness h(x: Untrusted<Str> = tools.read_file("/etc/passwd").text) -> Untrusted<Str>
  with { tools: [grep] }
{ x }
fn main() -> Untrusted<Str> { h() }
"#;
    // P5: a default calling a function-valued parameter (the caller's closure).
    let p5 = r#"
harness h(f: fn() -> Untrusted<Str>, s: Untrusted<Str> = f()) -> Untrusted<Str> { s }
fn main() -> Untrusted<Str> { h(|| tools.read_file(".env").text) }
"#;
    // P6: the declared configuration calling a function-valued parameter.
    let p6 = r#"
harness h(f: fn() -> Str) -> Str with { title: f() } { meta.frame().title }
fn main() -> Str { h(|| { let r = tools.read_file("x"); "t" }) }
"#;
    for (name, src) in [("P1", p1), ("P5", p5), ("P6", p6)] {
        let codes = refused_codes(src);
        assert!(codes.iter().any(|c| c == "C0016"), "{name}: {codes:?}");
    }
}

fn kind_of(p: &Arc<Prepared>, o: &Outcome) -> (String, Option<String>) {
    match o {
        Outcome::Raised(e) => (e.kind(p), e.denied_by(p).or_else(|| e.budget(p))),
        Outcome::Returned(v) => ("returned".into(), Some(ratio_eval::to_json_text(p, v))),
    }
}

/// Runs `name`, failing the test if it reaches the host.
fn run_no_host(p: &Arc<Prepared>, name: &str, args: Vec<Value>, limits: Limits) -> Outcome {
    let entry = p.find("main.ratio", name).unwrap();
    let mut eval =
        ratio_eval::evaluate(Arc::clone(p), entry, args, limits, CancelToken::new()).unwrap();
    let m = eval.machine();
    loop {
        match m.run() {
            Suspend::Yield => {}
            Suspend::Wait(w) => w.wait(),
            Suspend::Effect(req) => panic!("`{name}` reached the host with `{}`", req.builtin),
            Suspend::Branches(_) | Suspend::Fence(_) => {
                panic!("hand-driven machines run no concurrent constructs")
            }
            Suspend::Done(o) => return o,
        }
    }
}

fn sees(p: &Arc<Prepared>, name: &str) -> (String, Option<String>) {
    let o = run_no_host(p, name, vec![support::secret("SECRET")], Limits::default());
    kind_of(p, &o)
}

fn denied_sees() -> (String, Option<String>) {
    ("Denied".into(), Some("sees".into()))
}

#[test]
fn p2_secrets_in_settings_are_refused_like_arguments() {
    let p = support::prepared(
        r#"
harness child() -> Str { meta.frame().title }
harness reads() -> Int { let x = tools.read_file("a"); 1 }
harness declared(s: Str) -> Str with { title: s } { "x" }
harness inherits() -> Str { "x" }
harness system_holder(s: Str) -> Str
  with { sees: { secrets: true }, system: prompt { instruct "${s}" } }
{
  inherits()
}
harness title_holder(s: Str) -> Str with { sees: { secrets: true }, title: s } {
  with { sees: { secrets: false } } { "in" }
}
harness secret_ok(s: Str) -> Str with { sees: { secrets: true }, title: s } { meta.frame().title }

fn title_override(s: Str) -> Str { child() with { title: s } }
fn title_to_host(s: Str) -> Int { reads() with { title: s } }
fn system_override(s: Str) -> Str { child() with { system: prompt { instruct "${s}" } } }
fn title_declared(s: Str) -> Str { declared(s) }
fn system_inherited(s: Str) -> Str { system_holder(s) }
fn title_inherited_by_block(s: Str) -> Str { title_holder(s) }
fn allowed(s: Str) -> Str { secret_ok(s) }
fn tools_override(s: Str) -> Str { child() with { permissions: { deny: [s] } } }
"#,
    );
    for name in [
        "title_override",
        "title_to_host",
        "system_override",
        "title_declared",
        "system_inherited",
        "title_inherited_by_block",
        "tools_override",
    ] {
        assert_eq!(sees(&p, name), denied_sees(), "{name}");
    }
    // A frame that sees secrets may carry a labelled title; the label stays on it.
    let o = run_no_host(&p, "allowed", vec![support::secret("T")], Limits::default());
    assert!(matches!(&o, Outcome::Returned(v) if !v.labels().is_empty()));
}

#[test]
fn p3_a_block_without_secrets_cannot_read_a_secret_local() {
    let p = support::prepared(
        r#"
fn blk(s: Str) -> Str { with { sees: { secrets: false } } { s } }
harness hs(s: Str) -> Str with { sees: { secrets: true } } {
  with { sees: { secrets: false } } { s }
}
fn viah(s: Str) -> Str { hs(s) }
fn captured(s: Str) -> Str {
  let f = || s
  with { sees: { secrets: false } } { f() }
}
fn plain(s: Str) -> Str { with { sees: { secrets: false } } { "fine" } }
fn captures_in_block(s: Str) -> Str {
  with { sees: { secrets: false } } {
    let f = || s
    "made a closure over it"
  }
}
fn compound(s: Str) -> Str {
  var t = s
  with { sees: { secrets: false } } {
    t += "!"
    "appended to it"
  }
}
fn into_part(s: Str) -> Str {
  var xs = [s]
  with { sees: { secrets: false } } {
    xs[0] = "x"
    "wrote into it"
  }
}
fn overwrite(s: Str) -> Str {
  var t = s
  with { sees: { secrets: false } } {
    t = "replaced"
  }
  "a plain `=` does not read the place"
}
"#,
    );
    for name in [
        "blk",
        "viah",
        "captured",
        "captures_in_block",
        "compound",
        "into_part",
    ] {
        assert_eq!(sees(&p, name), denied_sees(), "{name}");
    }
    assert_eq!(sees(&p, "plain").0, "returned");
    assert_eq!(sees(&p, "overwrite").0, "returned");
}

#[test]
fn p7_configuration_lists_are_bounded_and_opening_is_charged() {
    let p = support::prepared(
        r#"
harness h(ts: [Str]) -> Int with { permissions: { paths: ts } } { 1 }
fn main(n: Int) -> Int {
  var xs: [Str] = []
  var i = 0
  while i < n { xs = list.push(xs, "/p/t${i}"); i = i + 1 }
  h(xs)
}
"#,
    );
    let o = run_no_host(&p, "main", vec![Value::int(16_000)], Limits::default());
    assert_eq!(kind_of(&p, &o).0, "Invalid");
    // At the limit, the meets are paid for in steps: 256 folders cost far more than 1.
    let steps = |n: i64| {
        let entry = p.find("main.ratio", "main").unwrap();
        let eval = ratio_eval::evaluate(
            Arc::clone(&p),
            entry,
            vec![Value::int(n)],
            Limits::default(),
            CancelToken::new(),
        )
        .unwrap();
        let f = ratio_eval::block_on(eval);
        assert!(matches!(f.outcome, Outcome::Returned(_)));
        f.steps
    };
    let (one, many) = (steps(1), steps(256));
    let loop_cost = (many - one) / 255;
    assert!(
        many - one > 255 * 20 + 60_000,
        "opening was not charged per comparison: {one} vs {many} ({loop_cost} per iteration)"
    );
}

/// The time of the fastest of `runs` runs of `f`.
fn fastest(runs: u32, mut f: impl FnMut()) -> std::time::Duration {
    (0..runs)
        .map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap()
}

/// Debug builds count the comparisons frame opening makes (`frame::work_done`): opening
/// with 256 folders, and with 256 tools under a parent with 64, makes no more than a fixed
/// multiple of the steps charged for the whole call, and charges well above a plain call.
#[cfg(debug_assertions)]
#[test]
fn opening_a_frame_makes_work_within_a_fixed_multiple_of_its_charge() {
    // 256 tools asked for (a literal list) under a parent with 64, and 256 folders.
    let wanted: Vec<String> = (0..256).map(|i| format!("tool_{i}")).collect();
    let parent: Vec<String> = (0..64).map(|i| format!("tool_{}", i * 3)).collect();
    let source = format!(
        r"
harness paths(ts: [Str]) -> Int with {{ permissions: {{ paths: ts }} }} {{ 1 }}
harness tools() -> Int with {{ tools: {{ need: [], want: [{}] }} }} {{ 1 }}
harness narrow() -> Int with {{ tools: [{}] }} {{ tools() }}
harness plain() -> Int {{ 1 }}
fn open_paths(xs: [Str]) -> Int {{ paths(xs) }}
fn open_tools() -> Int {{ narrow() }}
fn open_plain() -> Int {{ plain() }}
",
        wanted.join(", "),
        parent.join(", ")
    );
    let p = support::prepared(&source);
    let measure = |name: &str, args: Vec<Value>| {
        let entry = p.find("main.ratio", name).unwrap();
        let eval = ratio_eval::evaluate(
            Arc::clone(&p),
            entry,
            args,
            Limits::default(),
            CancelToken::new(),
        )
        .unwrap();
        let before = ratio_eval::frame::work_done();
        let f = ratio_eval::block_on(eval);
        let work = ratio_eval::frame::work_done() - before;
        assert!(matches!(f.outcome, Outcome::Returned(_)), "{name}");
        (work, f.steps)
    };
    let (plain_work, plain_steps) = measure("open_plain", Vec::new());
    let folders = (0..256)
        .map(|i| Value::text(&format!("/repo/area{i}/sub")))
        .collect();
    let cases = [
        ("open_paths", vec![Value::list(folders).unwrap()]),
        ("open_tools", Vec::new()),
    ];
    for (name, args) in cases {
        let (work, steps) = measure(name, args);
        assert!(
            work > 10 * plain_work.max(1),
            "{name}: only {work} comparisons"
        );
        assert!(
            work <= 2 * steps,
            "{name}: {work} comparisons for {steps} steps (a plain call: {plain_work} for              {plain_steps})"
        );
    }
}

/// The wall-clock version, for manual runs on a quiet machine (`--ignored`): CI runners are too
/// noisy for it.
#[test]
#[ignore = "wall-clock timing; run by hand on a quiet machine"]
fn opening_a_frame_costs_time_in_proportion_to_its_charge() {
    // A harness with 256 folders, opened once, against a pure loop, both charged in steps:
    // the opening may take at most a fixed multiple of the loop's time per step.
    let p = support::prepared(
        r"
harness h(ts: [Str]) -> Int with { permissions: { paths: ts } } { 1 }
fn open(xs: [Str]) -> Int { h(xs) }
fn spin(n: Int) -> Int {
  var i = 0
  while i < n { i = i + 1 }
  i
}
",
    );
    let folders: Vec<Value> = (0..256)
        .map(|i| Value::text(&format!("/repo/area{i}/sub")))
        .collect();
    let xs = Value::list(folders).unwrap();
    let run = |name: &str, arg: Value| {
        let entry = p.find("main.ratio", name).unwrap();
        let eval = ratio_eval::evaluate(
            Arc::clone(&p),
            entry,
            vec![arg],
            Limits::default(),
            CancelToken::new(),
        )
        .unwrap();
        let f = ratio_eval::block_on(eval);
        assert!(matches!(f.outcome, Outcome::Returned(_)), "{name}");
        f.steps
    };
    let open_steps = run("open", xs.clone());
    let spin_steps = run("spin", Value::int(100_000));
    let open_time = fastest(5, || {
        run("open", xs.clone());
    });
    let spin_time = fastest(5, || {
        run("spin", Value::int(100_000));
    });
    let per_open = open_time.as_secs_f64() / f64::from(u32::try_from(open_steps).unwrap());
    let per_spin = spin_time.as_secs_f64() / f64::from(u32::try_from(spin_steps).unwrap());
    assert!(
        per_open <= 8.0 * per_spin,
        "opening: {open_steps} steps in {open_time:?}; the loop: {spin_steps} in {spin_time:?}"
    );
}

#[test]
fn p8_a_loop_of_with_blocks_keeps_the_ledger_tree_small() {
    let p = support::prepared(
        r"
fn main(n: Int) -> Int {
  var i = 0
  while i < n { with { budget: { tokens: 5 } } { i = i + 1 } }
  i
}
",
    );
    let entry = p.find("main.ratio", "main").unwrap();
    let mut eval = ratio_eval::evaluate(
        Arc::clone(&p),
        entry,
        vec![Value::int(20_000)],
        Limits::default(),
        CancelToken::new(),
    )
    .unwrap();
    let m = eval.machine();
    let out = loop {
        match m.run() {
            Suspend::Yield => {}
            Suspend::Wait(w) => w.wait(),
            Suspend::Effect(_) => panic!("no effects"),
            Suspend::Branches(_) | Suspend::Fence(_) => {
                panic!("hand-driven machines run no concurrent constructs")
            }
            Suspend::Done(o) => break o,
        }
    };
    assert!(matches!(out, Outcome::Returned(v) if v.as_int() == Some(20_000)));
    assert!(
        m.frame().ledgers.slots() < 8,
        "{}",
        m.frame().ledgers.slots()
    );
}

#[test]
fn p9_a_budget_is_caught_only_outside_the_scope_whose_budget_ran_out() {
    let p = support::prepared(
        r"
harness h1() -> Int with { budget: { steps: 200 } } {
  try { var i = 0; loop { i = i + 1 }; 0 } catch e { -1 }
}
fn c1() -> Int { try { h1() } catch e { -2 } }
harness h2() -> Int with { budget: { steps: 300 } } {
  let r = try { with { budget: { steps: 100000 } } { var i = 0; loop { i = i + 1 }; 0 } } catch e { -1 }
  r
}
fn c2() -> Int { try { h2() } catch e { -2 } }
harness h3() -> Int with { budget: { steps: 300 } } {
  let r = try { with { budget: { steps: 50 } } { var i = 0; loop { i = i + 1 }; 0 } } catch e { -1 }
  r
}
harness h4() -> Int { try { var i = 0; loop { i = i + 1 }; 0 } catch e { -1 } }
fn c4() -> Int { try { h4() } catch e { -2 } }
harness h5() -> Int with { budget: { steps: 200 } } {
  let r = result(|| { var i = 0; loop { i = i + 1 }; 0 })
  7
}
fn c5() -> Int { try { h5() } catch e { -2 } }
harness inner() -> Int with { budget: { steps: 100 } } { var i = 0; loop { i = i + 1 }; 0 }
harness h6() -> Int with { budget: { steps: 400 } } { let a = try { inner() } catch e { -1 }; a }
",
    );
    let limits = Limits {
        steps: 20_000,
        ..Limits::default()
    };
    let got = |name: &str| kind_of(&p, &run_no_host(&p, name, Vec::new(), limits));
    let returned = |v: &str| ("returned".to_owned(), Some(v.to_owned()));
    // The harness's own cap: not caught inside it, caught by its caller.
    assert_eq!(got("c1"), returned("-2"));
    // A block asking for more than its harness has left is bound by the harness's cap.
    assert_eq!(got("c2"), returned("-2"));
    // A block's own lower cap: caught by the code around the block.
    assert_eq!(got("h3"), returned("-1"));
    // The turn's: never caught.
    assert_eq!(got("c4"), ("Budget".to_owned(), Some("steps".to_owned())));
    // `result` inside the frame cannot swallow it either.
    assert_eq!(got("c5"), returned("-2"));
    // A callee's own cap: its caller catches it.
    assert_eq!(got("h6"), returned("-1"));
}

/// The merge with stage 7: a built-in that debits as it goes (an `re` iteration) is given the
/// frame's steps left, not the turn's, and the `Budget` it raises is the frame's own: the
/// harness cannot catch it and its caller can, inline and on the pool alike.
#[test]
fn a_pure_built_in_runs_out_of_its_frames_steps_and_only_the_caller_catches_it() {
    use ratio_eval::{Pool, Thresholds};
    let p = support::prepared(
        r#"
harness h(s: Str) -> Int with { budget: { steps: 3000 } } {
  try { re.find_all(".*z|a", s, limit: 1000000).len() } catch e { -1 }
}
fn direct(s: Str) -> Int { h(s) }
fn caught(s: Str) -> Int {
  try { h(s) } catch e { if e.kind is Budget { -2 } else { -3 } }
}
"#,
    );
    let pool = Pool::with_thresholds(
        1,
        Thresholds {
            text_bytes: 0,
            elements: 0,
            steps: 0,
        },
    );
    let session = pool.session();
    for pooled in [false, true] {
        let run = |name: &str| {
            let entry = p.find("main.ratio", name).unwrap();
            let mut eval = ratio_eval::evaluate(
                Arc::clone(&p),
                entry,
                vec![Value::text(&"a".repeat(2_000))],
                Limits::default(),
                CancelToken::new(),
            )
            .unwrap();
            if pooled {
                eval = eval.offload_to(session.clone());
            }
            let f = ratio_eval::block_on(&mut eval);
            #[cfg(debug_assertions)]
            {
                let log = eval.machine().work_log();
                assert_eq!(
                    log.iter().any(|w| w.name == "re.find_all" && w.offloaded),
                    pooled,
                    "{log:?}"
                );
                // It stopped at the frame's 3,000 steps, not at the turn's 5,000,000: the
                // work it did is bounded by the frame's cap, not the whole quadratic scan.
                let work: u64 = log
                    .iter()
                    .filter(|w| w.name == "re.find_all")
                    .map(|w| w.work)
                    .sum();
                assert!(work > 0 && work <= 3_000 * 64 * 8, "work {work}: {log:?}");
            }
            f.outcome
        };
        assert_eq!(
            kind_of(&p, &run("direct")),
            ("Budget".to_owned(), Some("steps".to_owned())),
            "pooled: {pooled}"
        );
        assert_eq!(
            kind_of(&p, &run("caught")),
            ("returned".to_owned(), Some("-2".to_owned())),
            "pooled: {pooled}"
        );
    }
}
