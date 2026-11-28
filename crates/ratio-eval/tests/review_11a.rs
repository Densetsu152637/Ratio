//! Regressions for the stage 11a security review, round 1 (F1 to F6 and its notes).

#![allow(
    clippy::many_single_char_names,
    reason = "programs, hosts and tapes named briefly"
)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratio_eval::frame::{
    Card, FrameCtx, FrameKind, FrameOrigin, Ledgers, Location, Scope, Settings,
};
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
fn asked(s: Str) -> [UserText] {
  session.ask([{ question: s, options: ["yes", "no"], recommended: "no" }])
}
fn drained_secret(s: Str) -> Str { race { { tools.write_file("/w/.env", s); "w" }; "fast" } }
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
    .reply("hi", Labels::NONE)
    .load(&p);
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
        origin: match kind {
            FrameKind::Entry => FrameOrigin::Entry,
            FrameKind::Block => FrameOrigin::Block {
                site: ([0x22; 32], 3),
            },
            FrameKind::Harness => FrameOrigin::Harness {
                package: Arc::from("app"),
                entry_hash: [0x22; 32],
                key: Arc::from("main.ratio::h"),
            },
        },
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
    // A block of a package the host never loaded (a harness call forged consistently as a
    // block) is refused by its origin; once the package is loaded, the block opens.
    let block = frame("turn/with@s8#0", Some("turn"), FrameKind::Block, "with");
    assert!(h.open_frame(&root, &block).is_err());
    h.load_packages([(Arc::from("app"), [0x22; 32])], []);
    assert!(h.open_frame(&root, &block).is_ok());
    // A harness the host never loaded is refused, a loaded one opens.
    let mut harness = frame("turn/h@s1#0", Some("turn"), FrameKind::Harness, "h");
    let mut scope = (*harness.scope).clone();
    scope.depth += 1;
    scope.context = ratio_eval::frame::Context::Fresh;
    harness.scope = Arc::new(scope);
    assert!(h.open_frame(&root, &harness).is_err());
    h.load_packages([], [harness.origin.clone()]);
    let r = h.open_frame(&root, &harness);
    assert!(r.is_ok(), "{r:?}");
}

const HARNESS: &str = r#"
harness helper(n: Int) -> Int { n + 1 }
fn main() -> Int { helper(1) }
fn modelled() -> Untrusted<Str> { model.generate(prompt { task "hi" }) }
"#;

/// The spike's finding 1: Ratio fills every frame's origin from the IR, and a host refuses a
/// harness it did not load; loaded, the same program runs.
#[test]
fn frame_origins_come_from_the_ir_and_are_checked_against_what_was_loaded() {
    let p = support::prepared(HARNESS);
    let h = scripted();
    let s = h.session();
    let f = run(&p, "main", Vec::new(), dynh(&h), &s, None, None);
    assert_eq!(shown(&p, &f), "raise Denied attenuation");
    let (packages, harnesses) = p.loaded();
    assert_eq!(packages.len(), 1);
    let FrameOrigin::Harness {
        package,
        entry_hash,
        key,
    } = &harnesses[0]
    else {
        panic!("a harness origin");
    };
    assert_eq!(
        (&**package, entry_hash, &**key),
        (&*packages[0].0, &packages[0].1, "main.ratio::helper")
    );
    let h = scripted();
    h.load(&p);
    let s = h.session();
    let f = run(&p, "main", Vec::new(), dynh(&h), &s, None, None);
    assert_eq!(shown(&p, &f), "2");
}

/// The spike's finding 2: `count_tokens` is asynchronous; admission waits for it, a failure
/// is the model call's error, and a replay takes the recorded count without asking again.
#[test]
fn token_counts_are_awaited_and_replayed_from_the_admission() {
    let p = support::prepared(HARNESS);
    let local = || ModelCard {
        name: Arc::from("default"),
        location: Location::Local,
        card: Card {
            local: true,
            ..Card::default()
        },
    };
    // A tokenizer that takes several polls.
    let inner = scripted();
    inner.card(local()).reply("hello", Labels::NONE).jitter(5);
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([1; 32]).unwrap(),
    ));
    let f = run(
        &p,
        "modelled",
        Vec::new(),
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "\"hello\"");
    let text = t.tape();
    assert!(
        text.contains("\"model\":{\"input\":"),
        "the count is on the admission"
    );
    // A failing tokenizer fails the call, before any admission.
    let h = scripted();
    h.card(local())
        .reply("hello", Labels::NONE)
        .failing_tokenizer();
    let s = h.session();
    let f = run(&p, "modelled", Vec::new(), dynh(&h), &s, None, None);
    assert_eq!(shown(&p, &f), "raise ModelFailed ");
    assert!(h.model_calls().is_empty());
    // Replay never asks the tokenizer: it fails, and the replay still passes.
    let live = scripted();
    live.card(local()).failing_tokenizer();
    let s = live.session();
    let r = Arc::new(TapeHost::replay(dynh(&live), &text, ReplayOptions::default()).unwrap());
    let f = run(
        &p,
        "modelled",
        Vec::new(),
        Arc::clone(&r) as Arc<dyn Host>,
        &s,
        Some(r),
        None,
    );
    assert_eq!(shown(&p, &f), "\"hello\"");
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

/// R1: a replay never answers a `@user` built-in from its tape: a forged `session.ask`
/// answer is not believed; the person is asked live.
#[test]
fn r1_user_answers_are_never_taken_from_a_tape() {
    let p = support::prepared(SRC);
    let answer = |t: &str| Value::list(vec![Value::text(t)]).unwrap();
    let inner = scripted();
    inner.op("session.ask", Ok(answer("recorded")));
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([4; 32]).unwrap(),
    ));
    let f = run(
        &p,
        "asked",
        vec![Value::text("go?")],
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "[\"recorded\"]");
    // The tape is forged to answer for the user.
    let forged = t.tape().replace("recorded", "FORGED");
    let live = scripted();
    live.op("session.ask", Ok(answer("live")));
    let s = live.session();
    let r = Arc::new(TapeHost::replay(dynh(&live), &forged, ReplayOptions::default()).unwrap());
    let f = run(
        &p,
        "asked",
        vec![Value::text("go?")],
        Arc::clone(&r) as Arc<dyn Host>,
        &s,
        Some(r),
        None,
    );
    let out = shown(&p, &f);
    assert!(!out.contains("FORGED"), "{out}");
    assert_eq!(live.ops_seen().len(), 1, "the person was asked live");
}

/// Notes: a question's labelled text reaches the host redacted.
#[test]
fn questions_show_no_secret_to_the_host() {
    let p = support::prepared(SRC);
    let h = scripted();
    h.op(
        "session.ask",
        Ok(Value::list(vec![Value::text("no")]).unwrap()),
    );
    let s = h.session();
    let f = run(
        &p,
        "asked",
        vec![support::secret("is 42 the key?")],
        dynh(&h),
        &s,
        None,
        None,
    );
    assert_eq!(shown(&p, &f), "[\"no\"]");
    let tape_free = format!("{:?}", h.ops_seen());
    assert!(!tape_free.contains("42"), "{tape_free}");
    assert!(tape_free.contains("[secret: source 0]"), "{tape_free}");
}

/// R2: no plain digest over labelled data in a headless tape: a drained call's discarded
/// answer and an approval's summary are digested with the session's key.
#[test]
fn r2_no_unkeyed_digest_over_labelled_data_in_a_headless_tape() {
    let p = support::prepared(SRC);
    let inner = scripted();
    inner
        .secret_path("*.env")
        .tool(
            "write_file",
            ToolClass::drain(Duration::from_secs(10)),
            Behaviour::After(
                Duration::from_millis(100),
                Value::text("written").with_labels(Labels::source(0)),
            ),
        )
        .approval(true);
    let s = inner.session();
    let t = Arc::new(TapeHost::record(
        dynh(&inner),
        RecordOptions::new([8; 32]).unwrap().headless(true),
    ));
    let f = run(
        &p,
        "drained_secret",
        vec![support::secret("KEY=1")],
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "\"fast\"");
    let f = run(
        &p,
        "approval",
        vec![support::secret("KEY=1")],
        Arc::clone(&t) as Arc<dyn Host>,
        &s,
        Some(Arc::clone(&t)),
        None,
    );
    assert_eq!(shown(&p, &f), "true");
    let text = t.tape();
    let drained = text
        .lines()
        .find(|l| l.contains("\"outcome\":\"drained\""))
        .expect("drained");
    assert!(drained.contains("\"digest\":\"k:"), "{drained}");
    for l in text.lines().filter(|l| l.contains("\"q\":")) {
        assert!(l.contains("\"q\":\"k:"), "an unkeyed request digest: {l}");
    }
}

/// R3: the scheduler's bounds hold when the host's timer never fires.
#[test]
fn r3_bounds_hold_against_a_host_whose_timer_never_fires() {
    let p = support::prepared(SRC);
    let inner = scripted();
    inner
        .tool(
            "write_file",
            ToolClass::drain(Duration::from_millis(200)),
            Behaviour::Never,
        )
        .rogue()
        .frozen_timer();
    let s = inner.session();
    let start = Instant::now();
    let f = run(&p, "lone_write", Vec::new(), dynh(&inner), &s, None, None);
    assert_eq!(shown(&p, &f), "raise Timeout ");
    assert!(start.elapsed() < Duration::from_secs(5));
}

/// R4: the further tool calls of one effect (`key~i`, `dispatch_all`) share its answer.
#[test]
fn r4_sub_call_admissions_share_their_effects_answer() {
    let held = r#"{"tokens":0,"cost":0,"steps":0,"model_calls":0,"tool_calls":1}"#;
    let header = r#"{"tape":1,"ratio":"0.1.0","ir":1,"mode":"default","headless":false,"seed":"0000000000000000000000000000000000000000000000000000000000000000","root":"turn","caps":{},"width":16}"#;
    let text = format!(
        "{header}\n{{\"e\":\"k\",\"q\":\"x\",\"v\":[\"u\"]}}\n\
         {{\"seq\":1,\"ev\":\"admit\",\"key\":\"k\",\"frame\":\"turn\",\"held\":{held},\"max\":null}}\n\
         {{\"seq\":2,\"ev\":\"admit\",\"key\":\"k~1\",\"frame\":\"turn\",\"held\":{held},\"max\":null}}\n\
         {{\"seq\":3,\"ev\":\"complete\",\"key\":\"k\",\"actual\":{held}}}\n\
         {{\"seq\":4,\"ev\":\"complete\",\"key\":\"k~1\",\"actual\":{held}}}\n{{\"end\":\"x\"}}\n"
    );
    let t = TapeHost::replay(dynh(&scripted()), &text, ReplayOptions::default()).unwrap();
    assert_eq!(t.divergence(), None);
}
