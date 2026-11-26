//! Frames in the evaluator (stage 10): `sees` refusals of labelled values crossing into frames
//! without `secrets`, the frame context every effect carries to the host, harness entries
//! opened from a host's session scope, and the error kind of every refusal.

mod support;

use std::sync::Arc;

use ratio_eval::frame::{FrameKind, SeesItem, Session, ToolPat, ToolSet};
use ratio_eval::{CancelToken, Limits, Outcome, Prepared, Suspend, Value};

const SOURCE: &str = r#"
harness takes(s: Str) -> Int { 3 }
harness takes_secret(s: Str) -> Int with { sees: { secrets: true } } { 4 }
harness takes_list(xs: [Str]) -> Int { 5 }

harness isolated() -> Int {
  let f = tools.read_file("notes.txt")
  1
}

harness trusted() -> Int with { sees: { secrets: true } } {
  let f = tools.read_file("notes.txt")
  2
}

harness planner() -> Int with { sees: { secrets: true }, budget: { tokens: 5k } } {
  isolated()
}

fn pass_plain(s: Str) -> Int { takes(s) }
fn pass_secret(s: Str) -> Int { takes_secret(s) }
fn pass_nested(s: Str) -> Int { takes_list(["a", s]) }
fn reads_isolated() -> Int { isolated() }
fn reads_trusted() -> Int { trusted() }
fn reads_nested() -> Int { planner() }

fn catches(s: Str) -> Str {
  try {
    takes(s)
    "passed"
  } catch e {
    if e.kind is Denied { "denied" } else { "other" }
  }
}

harness entry() -> Int with { tools: [web_fetch] } { meta.depth() }
"#;

fn refusal(p: &Arc<Prepared>, outcome: &Outcome) -> (String, Option<String>) {
    match outcome {
        Outcome::Raised(e) => (e.kind(p), e.denied_by(p)),
        Outcome::Returned(v) => panic!("returned {}", ratio_eval::to_json_text(p, v)),
    }
}

#[test]
fn a_labelled_argument_is_refused_by_a_frame_without_secrets() {
    let p = support::prepared(SOURCE);
    let out = support::run(&p, "pass_plain", vec![support::secret("key")]).outcome;
    assert_eq!(refusal(&p, &out), ("Denied".into(), Some("sees".into())));
    // A label deep inside a container is found too.
    let out = support::run(&p, "pass_nested", vec![support::secret("key")]).outcome;
    assert_eq!(refusal(&p, &out), ("Denied".into(), Some("sees".into())));
    // The refusal is an ordinary catchable error in the caller.
    let v = support::value(&p, "catches", vec![support::secret("key")]);
    assert_eq!(v.as_text(), Some("denied"));
    // Unlabelled, it passes; a frame that sees secrets takes the labelled one.
    let v = support::value(&p, "pass_plain", vec![Value::text("plain")]);
    assert_eq!(v.as_int(), Some(3));
    let v = support::value(&p, "pass_secret", vec![support::secret("key")]);
    assert_eq!(v.as_int(), Some(4));
}

/// Runs `name`, answering every effect with `answer` and checking its frame with `check`.
fn drive(
    p: &Arc<Prepared>,
    name: &str,
    answer: &dyn Fn() -> Result<Value, ratio_eval::RtError>,
    check: &dyn Fn(&ratio_eval::EffectRequest),
) -> Outcome {
    let entry = p.find("main.ratio", name).unwrap();
    let mut eval = ratio_eval::evaluate(
        Arc::clone(p),
        entry,
        Vec::new(),
        Limits::default(),
        CancelToken::new(),
    )
    .unwrap();
    let m = eval.machine();
    loop {
        match m.run() {
            Suspend::Yield => {}
            Suspend::Wait(w) => w.wait(),
            Suspend::Effect(req) => {
                check(&req);
                m.resume_effect(answer());
            }
            Suspend::Done(o) => return o,
        }
    }
}

#[test]
fn an_effect_whose_result_is_labelled_is_refused_in_a_frame_without_secrets() {
    let p = support::prepared(SOURCE);
    let secret = || Ok(support::secret("SECRET=1"));
    let out = drive(&p, "reads_isolated", &secret, &|req| {
        assert_eq!(&*req.builtin, "tools.read_file");
        assert_eq!(req.frame.kind, FrameKind::Harness);
        assert!(req.frame.path.starts_with("turn/isolated@s"));
        assert_eq!(req.frame.parent.as_deref(), Some("turn"));
        assert!(!req.frame.scope.sees.has(SeesItem::Secrets));
        assert_eq!(req.frame.scope.depth, 1);
    });
    assert_eq!(refusal(&p, &out), ("Denied".into(), Some("sees".into())));
    // The same effect in a frame that sees secrets returns.
    let out = drive(&p, "reads_trusted", &secret, &|req| {
        assert!(req.frame.scope.sees.has(SeesItem::Secrets));
    });
    assert!(matches!(out, Outcome::Returned(v) if v.as_int() == Some(2)));
    // An unlabelled result passes the isolated frame.
    let plain = || Ok(Value::text("hello"));
    let out = drive(&p, "reads_isolated", &plain, &|_| {});
    assert!(matches!(out, Outcome::Returned(v) if v.as_int() == Some(1)));
}

#[test]
fn an_effect_failing_with_a_labelled_error_is_refused_in_a_frame_without_secrets() {
    let p = support::prepared(SOURCE);
    // A host error quoting a secret (a tool's output excerpt, say).
    let failing = || Err(labelled_error());
    let out = drive(&p, "reads_isolated", &failing, &|_| {});
    assert_eq!(refusal(&p, &out), ("Denied".into(), Some("sees".into())));
}

/// A catchable error with a labelled message, as a host's failure might carry.
fn labelled_error() -> ratio_eval::RtError {
    let src = r#"harness h(s: Str) -> Int with { sees: { secrets: true } } { raise fail("t", s) }
fn f(s: Str) -> Int { h(s) }"#;
    let q = support::prepared(src);
    match support::run(&q, "f", vec![support::secret("SECRET")]).outcome {
        Outcome::Raised(e) => e,
        Outcome::Returned(_) => panic!("expected an error"),
    }
}

#[test]
fn the_frame_context_carries_the_attenuated_scope_and_ledger() {
    let p = support::prepared(SOURCE);
    let out = drive(&p, "reads_nested", &|| Ok(Value::text("ok")), &|req| {
        // `isolated` inside `planner`: a fresh frame sees nothing, and its token cap is at
        // most its caller's.
        let f = &req.frame;
        assert!(f.path.contains("/planner@s") && f.path.contains("/isolated@s"));
        assert_eq!(f.scope.depth, 2);
        assert_eq!(f.scope.sees, ratio_eval::frame::Sees::NONE);
        assert_eq!(f.scope.budget.tokens, Some(5_000));
        assert_eq!(f.ledgers.remaining(f.node).tokens, Some(5_000));
        assert!(f.settings.title.is_none());
    });
    assert!(matches!(out, Outcome::Returned(v) if v.as_int() == Some(1)));
}

#[test]
fn a_harness_entry_opens_its_frame_from_the_hosts_session() {
    let p = support::prepared(SOURCE);
    let entry = p.find("main.ratio", "entry").unwrap();
    let run = |session: &Session| {
        let eval = ratio_eval::evaluate_in(
            Arc::clone(&p),
            entry,
            Vec::new(),
            Limits::default(),
            CancelToken::new(),
            session,
        )
        .unwrap();
        ratio_eval::block_on(eval).outcome
    };
    let mut session = Session {
        root: Arc::from("task"),
        ..Session::default()
    };
    assert!(matches!(run(&session), Outcome::Returned(v) if v.as_int() == Some(0)));
    // A session without the tool the entry needs: refused before the body runs.
    session.scope.tools = ToolSet::of([ToolPat::parse("read_file").unwrap()]);
    assert_eq!(
        refusal(&p, &run(&session)),
        ("Denied".into(), Some("attenuation".into()))
    );
}

#[test]
fn every_refusal_has_its_error_kind() {
    let src = r#"
harness need(n: Int) -> Int with { tools: [web_fetch] } { n }
harness narrow() -> Int with { tools: [read_file] } { need(1) }
harness down(n: Int) -> Int { down(n + 1) }
harness spin() -> Int with { budget: { steps: 50 } } { var i = 0; loop { i = i + 1 }; i }
harness bad() -> Int with { budget: { tokens: -1 } } { 1 }
harness star() -> Int with { tools: ["mcp__*__x"] } { 1 }
fn tools_denied() -> Int { narrow() }
fn too_deep() -> Int { down(0) }
fn steps_caught() -> Int { try { spin() } catch e { -1 } }
fn bad_budget() -> Int { bad() }
fn bad_pattern() -> Int { star() }
"#;
    let p = support::prepared(src);
    let kind = |name: &str| match support::run(&p, name, Vec::new()).outcome {
        Outcome::Raised(e) => (e.kind(&p), e.denied_by(&p).or_else(|| e.budget(&p))),
        Outcome::Returned(v) => ("returned".into(), Some(ratio_eval::to_json_text(&p, &v))),
    };
    assert_eq!(
        kind("tools_denied"),
        ("Denied".into(), Some("attenuation".into()))
    );
    assert_eq!(kind("too_deep"), ("Budget".into(), Some("depth".into())));
    assert_eq!(kind("steps_caught"), ("returned".into(), Some("-1".into())));
    assert_eq!(kind("bad_budget").0, "Invalid");
    assert_eq!(kind("bad_pattern").0, "Invalid");
}
