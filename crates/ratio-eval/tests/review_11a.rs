//! Regressions for the stage 11a security review, round 1 (F1 to F6 and its notes).

#![allow(
    clippy::many_single_char_names,
    reason = "programs, hosts and tapes named briefly"
)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratio_eval::frame::{Card, FrameCtx, FrameKind, Ledgers, Location, Scope, Settings};
use ratio_eval::host::{
    ChatRequest, ConvId, DeniedBy, EffectCtx, EffectKey, Guard, Host, HostError, ModelCard,
    ModelRef, SinkRule, ToolClass, operation_rule, sink_rule,
};
use ratio_eval::scripted::{Behaviour, ScriptedHost};
use ratio_eval::tape::{RecordOptions, ReplayOptions, Tape, TapeHost};
use ratio_eval::{CancelToken, Finished, Labels, Limits, Outcome, Prepared, Value};

fn run(
    p: &Arc<Prepared>,
    name: &str,
    args: Vec<Value>,
    h: Arc<dyn Host>,
    session: &ratio_eval::frame::Session,
    tape: Option<Arc<TapeHost>>,
    grace: Option<Duration>,
) -> Finished {
    let entry = p.find("main.ratio", name).expect("an entry");
    let mut e = ratio_eval::evaluate_with(
        Arc::clone(p),
        entry,
        args,
        Limits::default(),
        CancelToken::new(),
        session,
        h,
    )
    .expect("starts");
    if let Some(t) = tape {
        e = e.with_tape(t as Arc<dyn Tape>);
    }
    if let Some(g) = grace {
        e = e.abort_grace(g);
    }
    ratio_eval::block_on(e)
}

fn shown(p: &Prepared, f: &Finished) -> String {
    match &f.outcome {
        Outcome::Returned(v) => ratio_eval::to_json_text(p, v),
        Outcome::Raised(e) => format!(
            "raise {} {}",
            e.kind(p),
            e.budget(p).or_else(|| e.denied_by(p)).unwrap_or_default()
        ),
    }
}

fn scripted() -> Arc<ScriptedHost> {
    Arc::new(ScriptedHost::new("turn", Scope::session()))
}

fn dynh(h: &Arc<ScriptedHost>) -> Arc<dyn Host> {
    Arc::clone(h) as Arc<dyn Host>
}

const SRC: &str = r#"
fn hosted_as(s: Str) -> Untrusted<Str> { model.generate_as::<Str>(prompt { task "summarise"; data s }) }
fn capped_as() -> Untrusted<Str> {
  with { budget: { model_calls: 0 } } { model.generate_as::<Str>(prompt { task "hi" }) }
}
fn leak(x: Secret<Str>) -> Str { let plain = declassify(x, because: "ok"); tools.web_fetch(plain); "sent" }
fn local(s: Str) -> Untrusted<Str> { model.generate(prompt { task "summarise"; data s }, opts: { model: "local" }) }
fn reads() -> Str { tools.read_file("a"); tools.read_file("b"); tools.read_file("c"); "read" }
fn raced_write() -> Str { race { { tools.write_file("/w/a", "x"); "w" }; "fast" } }
fn lone_write() -> Str { tools.write_file("/w/a", "x"); "w" }
fn raced_read() -> Str { race { { tools.read_file("slow"); "r" }; "fast" } }
fn approval(s: Str) -> Bool { session.request_approval(s, detail: s) }
"#;

/// F1: `model.generate_as` is a model call: the guard refuses a secret before any host
/// sees it, and it is admitted on the ledgers (a cap of no model calls refuses it).
#[test]
fn f1_every_model_built_in_is_guarded_and_admitted() {
    let p = support::prepared(SRC);
    let h = scripted();
    h.card(ModelCard {
        name: Arc::from("default"),
        location: Location::Cloud,
        card: Card::default(),
    })
    .op("model.generate_as", Ok(Value::text("reply")));
    let s = h.session();
    let f = run(
        &p,
        "hosted_as",
        vec![support::secret("TOKEN")],
        dynh(&h),
        &s,
        None,
        None,
    );
    assert_eq!(shown(&p, &f), "raise Denied secrecy");
    assert!(h.model_calls().is_empty());

    let h = scripted();
    h.card(ModelCard {
        name: Arc::from("default"),
        location: Location::Local,
        card: Card {
            local: true,
            ..Card::default()
        },
    })
    .reply("hi", Labels::NONE);
    let s = h.session();
    let f = run(&p, "capped_as", Vec::new(), dynh(&h), &s, None, None);
    assert_eq!(shown(&p, &f), "raise Budget model_calls");
    assert!(h.model_calls().is_empty());
}

/// F1: the guard fails closed: a built-in it has no rule for is a sink.
#[test]
fn f1_the_guard_fails_closed() {
    assert_eq!(operation_rule("memory.remember"), SinkRule::Refuse);
    assert_eq!(operation_rule("no.such_builtin"), SinkRule::Refuse);
    assert_eq!(
        sink_rule("tools.no_such_tool", "no_such_tool"),
        SinkRule::Refuse
    );
    assert_eq!(
        sink_rule("tools.dispatch_all", "dispatch"),
        SinkRule::Refuse
    );
    assert_eq!(sink_rule("tools.read_file", "read_file"), SinkRule::Open);
    assert_eq!(operation_rule("log.info"), SinkRule::Open);
}

fn leak_record(secret: &str) -> (String, Arc<ScriptedHost>) {
    let p = support::prepared(SRC);
    let inner = scripted();
    inner.approval(true).tool(
        "web_fetch",
        ToolClass::abort(true),
        Behaviour::Reply(Value::text("page")),
    );
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([3; 32]).unwrap(),
    ));
    let f = run(
        &p,
        "leak",
        vec![support::secret(secret)],
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "\"sent\"");
    (t.tape(), inner)
}

/// F2: a replay, even a lenient one, never answers an approval from the tape: the user is
/// asked live (here nobody can answer, so `declassify` is refused and the secret goes
/// nowhere). The tape names what was declassified by a keyed digest only.
#[test]
fn f2_approvals_are_never_answered_from_the_tape() {
    let p = support::prepared(SRC);
    let (text, _) = leak_record("old-secret");
    assert!(!text.contains("old-secret"));
    // The declassification's request digest covers (a keyed digest of) what was declassified.
    let first_q = |t: &str| {
        t.lines()
            .find_map(|l| l.split("\"q\":\"").nth(1).map(|q| q[..64].to_owned()))
            .unwrap()
    };
    let (other, _) = leak_record("another-secret");
    assert_ne!(first_q(&text), first_q(&other));
    let inner = scripted();
    inner.tool(
        "web_fetch",
        ToolClass::abort(true),
        Behaviour::Reply(Value::text("page")),
    );
    let s = inner.session();
    let t = Arc::new(
        TapeHost::replay(
            dynh(&inner),
            &text,
            ReplayOptions {
                lenient: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap(),
    );
    let f = run(
        &p,
        "leak",
        vec![support::secret("NEW-LIVE-SECRET")],
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(t),
        None,
    );
    assert!(shown(&p, &f).starts_with("raise"), "{}", shown(&p, &f));
    assert_eq!(inner.asked().len(), 1, "asked live");
    assert!(inner.seen().is_empty(), "no live call carried the secret");
}

/// F3: in a headless tape, a digest over labelled data is keyed with the session's key, so
/// the same secret under two keys gives unrelated digests; an all-zero key is refused.
#[test]
fn f3_headless_digests_over_labelled_data_are_keyed() {
    let p = support::prepared(SRC);
    assert!(RecordOptions::new([0; 32]).is_err());
    let mut qs = Vec::new();
    for key in [[1u8; 32], [2u8; 32]] {
        let inner = scripted();
        inner
            .card(ModelCard {
                name: Arc::from("local"),
                location: Location::Local,
                card: Card {
                    local: true,
                    ..Card::default()
                },
            })
            .reply("summary", Labels::NONE);
        let s = inner.session();
        let t = Arc::new(TapeHost::record(
            dynh(&inner),
            RecordOptions::new(key).unwrap().headless(true),
        ));
        let f = run(
            &p,
            "local",
            vec![support::secret("1234")],
            Arc::clone(&t) as Arc<dyn Host>,
            &s,
            Some(Arc::clone(&t)),
            None,
        );
        assert_eq!(shown(&p, &f), "\"summary\"");
        let text = t.tape();
        let q = text
            .lines()
            .find_map(|l| l.split("\"q\":\"").nth(1).map(|q| q[..66].to_owned()))
            .unwrap();
        assert!(q.starts_with("k:"), "{q}");
        qs.push(q);
    }
    assert_ne!(qs[0], qs[1]);
}

/// F4: recording stops at its size limit, the tape says it was truncated, and replay
/// refuses it.
#[test]
fn f4_a_tape_over_its_limit_is_truncated_and_refused() {
    let p = support::prepared(SRC);
    let inner = scripted();
    inner.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::Reply(Value::text(&"y".repeat(2000))),
    );
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([5; 32]).unwrap().limit(3000),
    ));
    let f = run(
        &p,
        "reads",
        Vec::new(),
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "\"read\"");
    assert!(t.truncated());
    let text = t.tape();
    assert!(text.len() < 4000, "{} bytes", text.len());
    let e = TapeHost::replay(dynh(&scripted()), &text, ReplayOptions::default()).unwrap_err();
    assert!(e.to_string().contains("truncated"), "{e}");
}

/// F5: the scheduler bounds a `drain` call by its declared bound and an unacknowledged
/// `abort` call by the grace, whatever a host does, and records how they ended.
#[test]
fn f5_the_scheduler_bounds_drains_and_abort_acknowledgements() {
    let p = support::prepared(SRC);
    // A cancelled drain past its bound: the fence releases.
    let inner = scripted();
    inner
        .tool(
            "write_file",
            ToolClass::drain(Duration::from_millis(300)),
            Behaviour::Never,
        )
        .rogue();
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([6; 32]).unwrap(),
    ));
    let start = Instant::now();
    let f = run(
        &p,
        "raced_write",
        Vec::new(),
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "\"fast\"");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(t.tape().contains("\"outcome\":\"failed\""));

    // A live drain past its bound fails with `Timeout`.
    let inner = scripted();
    inner
        .tool(
            "write_file",
            ToolClass::drain(Duration::from_millis(200)),
            Behaviour::Never,
        )
        .rogue();
    let s = inner.session();
    let start = Instant::now();
    let f = run(&p, "lone_write", Vec::new(), dynh(&inner), &s, None, None);
    assert_eq!(shown(&p, &f), "raise Timeout ");
    assert!(start.elapsed() < Duration::from_secs(5));

    // An abort call the host never acknowledges: the scope returns after the grace.
    let inner = scripted();
    inner
        .tool("read_file", ToolClass::abort(false), Behaviour::Never)
        .rogue();
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([6; 32]).unwrap(),
    ));
    let start = Instant::now();
    let f = run(
        &p,
        "raced_read",
        Vec::new(),
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        Some(Duration::from_millis(200)),
    );
    assert_eq!(shown(&p, &f), "\"fast\"");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(t.tape().contains("\"outcome\":\"unacknowledged\""));
}

fn frame(path: &str, parent: Option<&str>, kind: FrameKind, harness: &str) -> FrameCtx {
    let scope = Scope::session();
    let (ledgers, node) = Ledgers::new(scope.budget, scope.width);
    FrameCtx {
        path: Arc::from(path),
        parent: parent.map(Arc::from),
        kind,
        harness: Arc::from(harness),
        scope: Arc::new(scope),
        ledgers,
        node,
        settings: Arc::new(Settings::default()),
    }
}

/// F6: an entry's frame must have its parent's path; a frame where the host started a
/// harness is refused under any other kind, however consistently forged.
#[test]
fn f6_frame_kinds_come_from_the_hosts_own_knowledge() {
    let h = scripted();
    let root = frame("turn", None, FrameKind::Entry, "turn");
    let elsewhere = frame("other", None, FrameKind::Entry, "turn");
    assert!(
        h.open_frame(&root, &elsewhere).is_err(),
        "an entry under another path"
    );
    h.harness_at("turn/with@s9#0");
    let forged = frame("turn/with@s9#0", Some("turn"), FrameKind::Block, "with");
    assert!(h.open_frame(&root, &forged).is_err());
    let block = frame("turn/with@s8#0", Some("turn"), FrameKind::Block, "with");
    assert!(h.open_frame(&root, &block).is_ok());
}

/// Notes: an approval's labelled text reaches the host as a placeholder naming its label.
#[test]
fn approvals_show_no_secret_to_the_host() {
    let p = support::prepared(SRC);
    let h = scripted();
    h.approval(true);
    let s = h.session();
    let f = run(
        &p,
        "approval",
        vec![support::secret("the key is 42")],
        dynh(&h),
        &s,
        None,
        None,
    );
    assert_eq!(shown(&p, &f), "true");
    let asked = h.asked();
    assert_eq!(asked[0].summary.value, "[secret: source 0]");
    assert!(!asked[0].summary.is_labelled());
    assert_eq!(
        asked[0].detail.as_ref().and_then(Value::as_text),
        Some("[secret: source 0]")
    );
}

/// Notes: a call on a labelled conversation is refused to a hosted model by the guard.
#[test]
fn a_labelled_conversation_reaches_no_hosted_model() {
    let h = scripted();
    h.label_conversation(0, Labels::source(2)).card(ModelCard {
        name: Arc::from("hosted"),
        location: Location::Cloud,
        card: Card::default(),
    });
    let guard = Arc::new(Guard::new(dynh(&h)));
    let cx = EffectCtx {
        key: EffectKey {
            frame: Arc::from("turn"),
            branch: Arc::from(""),
            site: ([0; 32], 1),
            occurrence: 0,
        },
        cancel: CancelToken::new(),
        deadline: None,
    };
    let req = ChatRequest {
        builtin: Arc::from("model.chat"),
        model: Some(ModelRef(Arc::from("hosted"))),
        max_tokens: None,
        options: Vec::new(),
        secret: false,
    };
    let r = ratio_eval::block_on(guard.chat(
        frame("turn", None, FrameKind::Entry, "turn"),
        cx,
        ConvId(0),
        req,
    ));
    assert!(matches!(
        r,
        Err(HostError::Denied {
            by: DeniedBy::Secrecy,
            ..
        })
    ));
    assert!(h.model_calls().is_empty());
}
