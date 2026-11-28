//! The scheduler (stage 11a): real concurrency, the width and admission order, cancellation
//! by class, the drain fence and re-parenting, `defer`, timeouts and deadlines, budgets
//! under concurrency, and the guard at every secrecy sink, against `ScriptedHost`.

#![allow(
    clippy::many_single_char_names,
    reason = "programs, hosts, finishes and calls named briefly"
)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratio_eval::frame::Scope;
use ratio_eval::host::{CancelOutcome, FrameOutcome, Host, ToolClass};
use ratio_eval::scripted::{Behaviour, ScriptedHost};
use ratio_eval::{CancelToken, Finished, Limits, Outcome, Prepared, Value};

fn host() -> Arc<ScriptedHost> {
    Arc::new(ScriptedHost::new("turn", Scope::session()))
}

fn run_on(p: &Arc<Prepared>, name: &str, args: Vec<Value>, h: &Arc<ScriptedHost>) -> Finished {
    let entry = p.find("main.ratio", name).expect("an entry");
    h.load(p);
    let session = h.session();
    let dynh: Arc<dyn Host> = Arc::clone(h) as Arc<dyn Host>;
    let eval = ratio_eval::evaluate_with(
        Arc::clone(p),
        entry,
        args,
        Limits::default(),
        CancelToken::new(),
        &session,
        dynh,
    )
    .expect("starts");
    ratio_eval::block_on(eval)
}

fn returned(p: &Prepared, f: &Finished) -> String {
    match &f.outcome {
        Outcome::Returned(v) => ratio_eval::to_json_text(p, v),
        Outcome::Raised(e) => panic!("raised {} {:?}", e.kind(p), e.message()),
    }
}

fn raised(p: &Prepared, f: &Finished) -> (String, Option<String>) {
    match &f.outcome {
        Outcome::Returned(v) => panic!("returned {}", ratio_eval::to_json_text(p, v)),
        Outcome::Raised(e) => (e.kind(p), e.budget(p).or_else(|| e.denied_by(p))),
    }
}

const SKILLS: &str = r#"
fn two() -> (FileText, FileText) {
  par { tools.read_file("a"); tools.read_file("b") }
}

fn fan(xs: [Int]) -> [FileText] {
  with { limits: { width: 2 } } {
    par_map(xs, |i: Int| tools.read_file("s{i}"), limit: 10)
  }
}

fn nested() -> [[Int]] {
  with { limits: { width: 2 } } {
    par_map([1, 2, 3], |i: Int| par_map([1, 2, 3], |j: Int| { tools.read_file("n"); i * j }), limit: 3)
  }
}
"#;

#[test]
fn branches_really_overlap() {
    let p = support::prepared(SKILLS);
    let h = host();
    // The tool answers only once two calls are in flight at once.
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::Barrier(2, Value::text("ok")),
    );
    let f = run_on(&p, "two", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#"["ok","ok"]"#);
    assert_eq!(h.peak_in_flight(), 2);
}

#[test]
fn running_branches_never_exceed_the_width_and_start_in_branch_path_order() {
    let p = support::prepared(SKILLS);
    let h = host();
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::After(Duration::from_millis(5), Value::text("ok")),
    )
    .jitter(7);
    let xs = Value::list((0..9).map(Value::int).collect()).unwrap();
    let f = run_on(&p, "fan", vec![xs], &h);
    assert_eq!(returned(&p, &f).matches("ok").count(), 9);
    assert!(h.peak_in_flight() <= 2, "width 2: {}", h.peak_in_flight());
    let order: Vec<String> = h
        .seen()
        .iter()
        .map(|s| s.args[0].1.as_text().unwrap_or_default().to_owned())
        .collect();
    let want: Vec<String> = (0..9).map(|i| format!("s{i}")).collect();
    assert_eq!(order, want, "branches start in branch-path order");
}

#[test]
fn nested_fan_out_at_width_two_completes() {
    let p = support::prepared(SKILLS);
    let h = host();
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::Reply(Value::text("ok")),
    )
    .jitter(3);
    let f = run_on(&p, "nested", Vec::new(), &h);
    assert_eq!(returned(&p, &f), "[[1,2,3],[2,4,6],[3,6,9]]");
    assert!(h.peak_in_flight() <= 2);
}

const CANCEL: &str = r#"
fn race_write() -> Str {
  let w = race { { tools.write_file("/w/a", "slow"); "write" }; "fast" }
  tools.write_file("/w/b", "later")
  w
}

harness b() -> Str {
  tools.write_file("/w/deep", "slow")
  "b"
}

harness a() -> Str {
  b()
}

fn nested_race() -> Str {
  race { a(); "fast" }
}

fn deferred() -> Str {
  defer { tools.write_file("/w/defer", "now") }
  race { { tools.write_file("/w/a", "slow"); "write" }; "fast" }
}

fn timed() -> Str {
  with timeout 100ms {
    tools.run_command("sleep")
    "finished"
  } else {
    "timed out"
  }
}

fn raced_command() -> Str {
  race { { tools.run_command("build"); "built" }; "fast" }
}

fn aborted() -> Str {
  race { { tools.read_file("slow"); "slow" }; "fast" }
}
"#;

fn writes(h: &ScriptedHost, slow: Duration) {
    // A write of "slow" content takes `slow`; others are immediate.
    h.tool(
        "write_file",
        ToolClass::drain(Duration::from_secs(60)),
        Behaviour::After(slow, Value::unit()),
    );
}

fn seen_path(h: &ScriptedHost, path: &str) -> ratio_eval::scripted::Seen {
    h.seen()
        .into_iter()
        .find(|s| s.args.iter().any(|(_, v)| v.as_text() == Some(path)))
        .unwrap_or_else(|| panic!("no call for {path}"))
}

#[test]
fn race_returns_before_a_losing_drain_write_which_later_writes_and_the_turn_wait_for() {
    let p = support::prepared(CANCEL);
    let h = host();
    writes(&h, Duration::from_millis(300));
    let start = Instant::now();
    let f = run_on(&p, "race_write", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#""fast""#);
    let a = seen_path(&h, "/w/a");
    let b = seen_path(&h, "/w/b");
    // The later write waited behind the fence for the outstanding drain.
    assert!(b.started >= a.finished.expect("the drain finished"));
    // The turn completed only after it.
    assert!(start.elapsed() >= Duration::from_millis(300));
    assert!(h.ended().iter().any(|(_, o)| *o == CancelOutcome::Drained));
}

#[test]
fn a_cancelled_nested_harness_closes_at_once_and_its_write_is_reparented() {
    let p = support::prepared(CANCEL);
    let h = host();
    writes(&h, Duration::from_millis(300));
    let f = run_on(&p, "nested_race", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#""fast""#);
    let closed = h.closed();
    let deep = seen_path(&h, "/w/deep");
    // Both harness frames closed, cancelled, while the write still ran.
    assert!(
        closed
            .iter()
            .filter(|(path, o)| *o == FrameOutcome::Cancelled && path.contains("@s"))
            .count()
            >= 2,
        "{closed:?}"
    );
    assert!(deep.finished.is_some());
}

#[test]
fn a_defer_block_is_not_fenced() {
    let p = support::prepared(CANCEL);
    let h = host();
    writes(&h, Duration::from_millis(400));
    let f = run_on(&p, "deferred", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#""fast""#);
    let a = seen_path(&h, "/w/a");
    let d = seen_path(&h, "/w/defer");
    assert!(
        d.started < a.finished.expect("finished"),
        "the defer write ran while the drain was outstanding"
    );
}

#[test]
fn a_command_is_killed_when_its_timeout_fires_and_the_host_receives_the_deadline() {
    let p = support::prepared(CANCEL);
    let h = host();
    h.tool(
        "run_command",
        ToolClass::drain(Duration::from_secs(600)),
        Behaviour::Never,
    );
    let start = Instant::now();
    let f = run_on(&p, "timed", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#""timed out""#);
    assert!(start.elapsed() < Duration::from_secs(5));
    let seen = h.seen();
    assert!(seen[0].deadline.is_some(), "the host receives the deadline");
}

#[test]
fn a_command_is_not_aborted_by_a_race() {
    let p = support::prepared(CANCEL);
    let h = host();
    h.tool(
        "run_command",
        ToolClass::drain(Duration::from_secs(600)),
        Behaviour::After(Duration::from_millis(200), Value::unit()),
    );
    let f = run_on(&p, "raced_command", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#""fast""#);
    assert_eq!(
        h.ended(),
        vec![(h.seen()[0].key.clone(), CancelOutcome::Drained)]
    );
}

#[test]
fn an_abort_call_is_aborted_by_a_race() {
    let p = support::prepared(CANCEL);
    let h = host();
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::After(Duration::from_secs(30), Value::text("slow")),
    );
    let start = Instant::now();
    let f = run_on(&p, "aborted", Vec::new(), &h);
    assert_eq!(returned(&p, &f), r#""fast""#);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(h.ended()[0].1, CancelOutcome::Aborted);
}

const BUDGET: &str = r#"
fn three() -> Str {
  with { budget: { tool_calls: 2 } } {
    par { tools.read_file("a"); tools.read_file("b"); tools.read_file("c") }
  }
  "done"
}
"#;

#[test]
fn concurrent_tool_calls_never_pass_a_cap() {
    let p = support::prepared(BUDGET);
    let h = host();
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::Reply(Value::text("x")),
    )
    .jitter(11);
    let f = run_on(&p, "three", Vec::new(), &h);
    assert_eq!(raised(&p, &f), ("Budget".into(), Some("tool_calls".into())));
    assert_eq!(h.seen().len(), 2);
}

const SINKS: &str = r#"
fn fetch(s: Str) -> Str { tools.web_fetch(s); "sent" }
fn command(s: Str) -> Str { tools.run_command(s); "sent" }
fn write(s: Str) -> Str { tools.write_file("/w/out", s); "sent" }
fn write_secret_path(s: Str) -> Str { tools.write_file("/w/.env", s); "sent" }
fn mcp(s: Str) -> Str { tools.mcp_call("srv", "t", json.parse(s)); "sent" }
fn any_tool(s: Str) -> Str { tools.call_tool("read_file", json.parse(s)); "sent" }
fn ask_agent(s: Str) -> Str { agents.ask("helper", s); "sent" }
fn emit(s: Str) -> Str { session.emit("e", json.parse(s)); "sent" }
fn hosted(s: Str) -> Untrusted<Str> { model.generate(prompt { task "summarise"; data s }) }
fn local(s: Str) -> Untrusted<Str> { model.generate(prompt { task "summarise"; data s }, opts: { model: "local" }) }
fn as_model(s: Str) -> Untrusted<Str> { model.generate_as::<Str>(prompt { task "summarise"; data s }) }
enum Mood { happy, sad }
fn classified(s: Str) -> Str { model.classify::<Mood>(s); "sent" }
fn dispatched(s: Str, c: Conversation, t: ToolCall) -> Str { tools.dispatch(c, t); "sent" }
"#;

#[test]
fn every_secrecy_sink_refuses_a_labelled_value_at_the_guard() {
    let p = support::prepared(SINKS);
    for name in [
        "fetch",
        "command",
        "write",
        "mcp",
        "any_tool",
        "ask_agent",
        "emit",
        "hosted",
        "as_model",
        "classified",
        "dispatched",
    ] {
        let h = host();
        h.card(ratio_eval::host::ModelCard {
            name: Arc::from("default"),
            location: ratio_eval::frame::Location::Cloud,
            card: ratio_eval::frame::Card::default(),
        });
        let mut args = vec![support::secret("\"token\"")];
        if name == "dispatched" {
            // A model's call on a conversation, its arguments secret.
            let call = Value::record(vec![
                ("id", Value::text("1")),
                ("name", Value::text("web_fetch")),
                ("arguments", support::secret("https://x")),
            ])
            .unwrap();
            args.extend([Value::unit(), call]);
        }
        let f = run_on(&p, name, args, &h);
        assert_eq!(
            raised(&p, &f),
            ("Denied".into(), Some("secrecy".into())),
            "`{name}`"
        );
        assert!(h.seen().is_empty(), "`{name}` reached the host");
        assert!(
            h.model_calls().is_empty() && h.events().is_empty(),
            "`{name}`"
        );
    }
}

#[test]
fn allowed_flows_pass_the_guard_with_the_flag_set() {
    let p = support::prepared(SINKS);
    let h = host();
    h.secret_path("*.env").tool(
        "write_file",
        ToolClass::drain(Duration::from_secs(5)),
        Behaviour::Reply(Value::unit()),
    );
    let f = run_on(&p, "write_secret_path", vec![support::secret("k")], &h);
    assert_eq!(returned(&p, &f), r#""sent""#);
    assert!(h.seen()[0].secret, "the per-call flag is set");

    let h = host();
    h.card(ratio_eval::host::ModelCard {
        name: Arc::from("local"),
        location: ratio_eval::frame::Location::Local,
        card: ratio_eval::frame::Card {
            local: true,
            ..ratio_eval::frame::Card::default()
        },
    })
    .reply("summary", ratio_eval::Labels::NONE);
    let f = run_on(&p, "local", vec![support::secret("k")], &h);
    assert_eq!(returned(&p, &f), r#""summary""#);
    assert!(h.model_calls()[0].secret);
}
