//! The host contract suite (chapter 10, "Where cross-repository tests live"), behind the
//! `host-tests` feature: what every [`Host`] must do, checked through the raw trait, without
//! the interpreter's [`Guard`](crate::host::Guard), so a host that relied on the guard fails.
//!
//! A host supplies a [`Fixture`]: a constructor for a fresh host whose session has a given
//! scope, the calls that reach its sink tools and its cancellation cases, a way to answer a
//! pending approval and to stop the session, and its scripted model endpoint. Then:
//!
//! ```ignore
//! #[test]
//! fn contract() {
//!     ratio_eval::host_tests::run_all(&MyFixture::new());
//! }
//! ```
//!
//! The cases:
//!
//! - **frames**: `open_frame` refuses a child scope wider than its parent in each dimension
//!   (tools, mode, rules, paths, location, every budget, `sees`, depth, width, heap,
//!   attended), checked against the host's own record of the parent, also when the parent
//!   `FrameCtx` passed in was forged wider, and when the kind was forged;
//! - **sinks**: a call with the secrecy flag set is refused at every sink (`Denied { by:
//!   secrecy }`): the network, command and write tools, and a hosted model;
//! - **cancellation**: an `abort` call is acknowledged within a bound (`aborted`; `abandoned`
//!   for a remote one); every tool of the table declares a bound and an unlisted tool or an
//!   MCP tool claiming to be read-only is `drain`; a never-ending `drain` call is failed at its
//!   declared bound, whatever its token; an approval-pending call is withdrawn, never runs, and
//!   a later answer changes nothing; an answer before the withdrawal starts the call; the host
//!   bounds a call by its deadline; after a session stop, a cancelled call drains.
//!
//! The scheduler's own cancellation cases (`race` and the drain fence, re-parenting, `defer`)
//! run in this crate's tests against [`crate::scripted::ScriptedHost`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::frame::{
    BudgetKind, Caps, Context as Ctx, Folder, FrameCtx, FrameKind, FrameOrigin, Ledgers, Location,
    Mode, Paths, Rules, Scope, Sees, SeesItem, Settings, ToolPat, ToolSet,
};
use crate::host::{
    CancelClass, CancelOutcome, DeniedBy, EffectCtx, EffectKey, Host, HostError, HostFuture,
    MAX_DRAIN, ModelRef, ModelRequest, OpFamily, Operation, RatioEvent, ToolInvocation,
};
use crate::machine::CancelToken;
use crate::value::Value;

/// The calls a host's tools answer, for the suite.
#[derive(Clone, Debug)]
pub struct Calls {
    /// A network tool (`web_fetch`), a command tool (`run_command`) and a write tool
    /// (`write_file`, to a path outside `ratio.secret_paths`), each with a secret argument.
    pub network: ToolInvocation,
    pub command: ToolInvocation,
    pub write: ToolInvocation,
    /// `tools.mcp_call` and `tools.call_tool`, which can reach any tool by name.
    pub mcp: ToolInvocation,
    pub any_tool: ToolInvocation,
    /// An `abort` tool that takes long (at least a few seconds).
    pub slow_abort: ToolInvocation,
    /// An `abort` tool that reaches a remote server and takes long.
    pub slow_remote: ToolInvocation,
    /// A `drain` tool that never finishes by itself, declaring a short bound (at most a few
    /// seconds).
    pub never_drain: ToolInvocation,
    /// A call that waits for the user's approval.
    pub needs_approval: ToolInvocation,
}

/// What a host provides to run the suite.
pub trait Fixture {
    /// A fresh host whose session has `scope`, rooted at the frame path `turn`.
    fn host(&self, scope: &Scope) -> Arc<dyn Host>;
    /// The calls of the cases.
    fn calls(&self) -> Calls;
    /// The names of the tools in the host's table.
    fn tool_table(&self, host: &Arc<dyn Host>) -> Vec<String>;
    /// Answers the pending approval of `call` (`false`: the answer was discarded).
    fn answer_approval(&self, host: &Arc<dyn Host>, approve: bool, for_session: bool) -> bool;
    /// Whether the approval tool is now allowed for the session, and whether a denial of it
    /// was remembered.
    fn remembered(&self, host: &Arc<dyn Host>) -> (bool, bool);
    /// `session.control stop`.
    fn stop_session(&self, host: &Arc<dyn Host>);
    /// A hosted model the scripted endpoint answers.
    fn hosted_model(&self) -> ModelRef;
    /// A harness the host has loaded, as the origin its frames carry (its package, the
    /// package's IR hash and its key): the suite opens frames of it, and forges others.
    fn loaded_harness(&self, host: &Arc<dyn Host>) -> FrameOrigin;
}

/// The engine operations that are secrecy sinks (briefs, steering and questions to other
/// agents, memory writes, eval metrics, scores and labels), each with one secret argument.
#[must_use]
pub fn sink_operations() -> Vec<Operation> {
    let secret = Value::text("the key").with_labels(crate::labels::Labels::source(0));
    [
        ("agents.spawn", "brief"),
        ("agents.steer", "text"),
        ("agents.queue", "text"),
        ("agents.ask", "question"),
        ("memory.remember", "text"),
        ("memory.edit", "text"),
        ("memory.propose", "text"),
        ("eval.metric", "value"),
        ("eval.score", "value"),
        ("eval.label", "value"),
    ]
    .into_iter()
    .map(|(b, arg)| Operation {
        family: OpFamily::of(b),
        builtin: Arc::from(b),
        args: vec![(Arc::from(arg), secret.clone())],
        secret: true,
    })
    .collect()
}

/// `session.emit` with a secret: an output-only sink.
#[must_use]
pub fn sink_event() -> RatioEvent {
    RatioEvent {
        builtin: Arc::from("session.emit"),
        key: EffectKey {
            frame: Arc::from("turn"),
            branch: Arc::from(""),
            site: ([0; 32], 99),
            occurrence: 0,
        },
        args: vec![(
            Arc::from("data"),
            Value::text("the key").with_labels(crate::labels::Labels::source(0)),
        )],
        secret: true,
    }
}

/// Runs a future on the current thread, polling it until it resolves or `limit` passes.
fn run_for<F: Future>(f: F, limit: Duration) -> Option<F::Output> {
    let start = Instant::now();
    let mut f = std::pin::pin!(f);
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return Some(v);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A narrow session scope: every dimension can be widened.
///
/// # Panics
///
/// Never: its patterns and folder are well formed.
#[must_use]
pub fn narrow_scope() -> Scope {
    Scope {
        tools: ToolSet::of([
            ToolPat::parse("read_file").expect("a pattern"),
            ToolPat::parse("write_file").expect("a pattern"),
            ToolPat::parse("web_fetch").expect("a pattern"),
            ToolPat::parse("run_command").expect("a pattern"),
            ToolPat::parse("slow_*").expect("a pattern"),
            ToolPat::parse("never_drain").expect("a pattern"),
            ToolPat::parse("needs_approval").expect("a pattern"),
            ToolPat::parse("mcp__*").expect("a pattern"),
        ]),
        mode: Mode::Default,
        rules: Rules {
            deny: [Arc::from("run_command(rm *)")].into_iter().collect(),
            ..Rules::default()
        },
        paths: Paths::Within(
            [Folder::parse("/work").expect("a folder")]
                .into_iter()
                .collect(),
        ),
        location: Location::Local,
        budget: Caps {
            tokens: Some(10_000),
            cost: Some(1_000_000),
            wall: Some(60_000_000),
            steps: Some(1_000_000),
            model_calls: Some(10),
            tool_calls: Some(10),
        },
        sees: Sees::ALL.without(SeesItem::Secrets),
        depth: 0,
        max_depth: 2,
        width: 4,
        heap: Some(1 << 20),
        attended: false,
        memory_writes: false,
        context: Ctx::Fresh,
    }
}

fn ctx(path: &str, parent: Option<&str>, kind: FrameKind, harness: &str, scope: Scope) -> FrameCtx {
    ctx_from(path, parent, kind, harness, scope, FrameOrigin::Entry)
}

fn ctx_from(
    path: &str,
    parent: Option<&str>,
    kind: FrameKind,
    harness: &str,
    scope: Scope,
    origin: FrameOrigin,
) -> FrameCtx {
    let (ledgers, node) = Ledgers::new(scope.budget, scope.width.max(1));
    FrameCtx {
        origin,
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

fn root(scope: &Scope) -> FrameCtx {
    ctx("turn", None, FrameKind::Entry, "turn", scope.clone())
}

fn child_of(scope: &Scope) -> Scope {
    let mut c = scope.clone();
    c.depth += 1;
    c
}

/// Each way to widen a child of the narrow scope, by dimension.
fn widenings(parent: &Scope) -> Vec<(&'static str, Scope)> {
    let base = child_of(parent);
    let w = |f: &dyn Fn(&mut Scope)| {
        let mut s = base.clone();
        f(&mut s);
        s
    };
    let mut out = vec![
        ("tools", w(&|s| s.tools = ToolSet::all())),
        ("mode", w(&|s| s.mode = Mode::Full)),
        ("rules", w(&|s| s.rules = Rules::default())),
        ("paths", w(&|s| s.paths = Paths::Any)),
        ("location", w(&|s| s.location = Location::Any)),
        ("sees", w(&|s| s.sees = Sees::ALL)),
        ("depth", w(&|s| s.depth = parent.depth)),
        ("depth limit", w(&|s| s.max_depth = parent.max_depth + 1)),
        ("width", w(&|s| s.width = parent.width + 1)),
        ("heap", w(&|s| s.heap = None)),
        ("attended", w(&|s| s.attended = true)),
    ];
    for k in BudgetKind::ALL {
        let mut s = base.clone();
        s.budget.set(k, None);
        out.push((k.name(), s));
    }
    out
}

fn refused_by_attenuation(r: &Result<(), HostError>) -> bool {
    matches!(
        r,
        Err(HostError::Denied {
            by: DeniedBy::Attenuation,
            ..
        })
    )
}

/// `open_frame` admits a child within its parent and refuses one wider in any dimension,
/// against its own record of the parent: also when the parent `FrameCtx` was forged wider,
/// and when a harness call is passed off as a block.
///
/// # Panics
///
/// When the host fails the case.
pub fn frames(f: &dyn Fixture) {
    let scope = narrow_scope();
    let host = f.host(&scope);
    let parent = root(&scope);
    let known = f.loaded_harness(&host);
    let FrameOrigin::Harness {
        package,
        entry_hash,
        key,
    } = known.clone()
    else {
        panic!("the fixture's loaded harness must be a harness origin");
    };
    let harness = |path: &str, scope: Scope, origin: &FrameOrigin| {
        ctx_from(
            path,
            Some("turn"),
            FrameKind::Harness,
            "h",
            scope,
            origin.clone(),
        )
    };
    assert!(
        host.open_frame(&parent, &harness("turn/h@s1#0", child_of(&scope), &known))
            .is_ok(),
        "a loaded harness within its parent must open"
    );
    // A forged parent: its context claims the widest scope; the host's record is narrow.
    let forged = ctx("turn", None, FrameKind::Entry, "turn", Scope::session());
    for (i, (dim, wide)) in widenings(&scope).into_iter().enumerate() {
        let child = harness(&format!("turn/w@s{}#0", i + 10), wide, &known);
        assert!(
            refused_by_attenuation(&host.open_frame(&parent, &child)),
            "a child wider in `{dim}` must be refused"
        );
        assert!(
            refused_by_attenuation(&host.open_frame(&forged, &child)),
            "a child wider in `{dim}` than the host's record must be refused under a forged parent"
        );
    }
    // A harness the host never loaded: another IR hash, or another key.
    let mut other_hash = entry_hash;
    other_hash[0] ^= 0xff;
    for (what, origin) in [
        (
            "another package hash",
            FrameOrigin::Harness {
                package: Arc::clone(&package),
                entry_hash: other_hash,
                key: Arc::clone(&key),
            },
        ),
        (
            "another harness",
            FrameOrigin::Harness {
                package: Arc::clone(&package),
                entry_hash,
                key: Arc::from("main.ratio::never_loaded"),
            },
        ),
    ] {
        assert!(
            refused_by_attenuation(
                &host.open_frame(&parent, &harness("turn/x@s5#0", child_of(&scope), &origin))
            ),
            "a harness frame of {what} must be refused"
        );
    }
    // A block at a site of a loaded package opens.
    let block = |path: &str, hash: [u8; 32]| {
        ctx_from(
            path,
            Some("turn"),
            FrameKind::Block,
            "with",
            scope.clone(),
            FrameOrigin::Block { site: (hash, 7) },
        )
    };
    assert!(
        host.open_frame(&parent, &block("turn/with@s6#0", entry_hash))
            .is_ok(),
        "a block of a loaded package must open"
    );
    // A harness call passed off consistently as a block (kind `Block`, harness `with`, the
    // parent's depth): within its parent in every dimension, it fails on its origin, a site of
    // a package the host never loaded.
    assert!(
        refused_by_attenuation(&host.open_frame(&parent, &block("turn/with@s2#0", other_hash))),
        "a harness frame forged consistently as a block must be refused by its origin"
    );
    // An origin that does not fit the kind.
    let mismatched = ctx_from(
        "turn/h@s3#0",
        Some("turn"),
        FrameKind::Harness,
        "h",
        child_of(&scope),
        FrameOrigin::Block {
            site: (entry_hash, 7),
        },
    );
    assert!(
        refused_by_attenuation(&host.open_frame(&parent, &mismatched)),
        "a harness frame with a block's origin must be refused"
    );
    // A parent the host never opened.
    let unknown = ctx(
        "turn/ghost@s3#0",
        Some("turn"),
        FrameKind::Harness,
        "ghost",
        child_of(&scope),
    );
    let grandchild = ctx(
        "turn/ghost@s3#0/h@s4#0",
        Some("turn/ghost@s3#0"),
        FrameKind::Harness,
        "h",
        child_of(&child_of(&scope)),
    );
    assert!(
        host.open_frame(&unknown, &grandchild).is_err(),
        "a child of a frame the host has no record of must be refused"
    );
}

fn effect(n: u32, deadline: Option<Instant>) -> (EffectCtx, CancelToken) {
    let token = CancelToken::new();
    (
        EffectCtx {
            key: EffectKey {
                frame: Arc::from("turn"),
                branch: Arc::from(""),
                site: ([0; 32], n),
                occurrence: 0,
            },
            cancel: token.clone(),
            deadline,
        },
        token,
    )
}

fn secret_call(mut c: ToolInvocation) -> ToolInvocation {
    c.secret = true;
    c.args = c
        .args
        .into_iter()
        .map(|(n, v)| {
            let l = v.labels() | crate::labels::Labels::source(0);
            (n, v.with_labels(l))
        })
        .collect();
    c
}

fn denied_secrecy<T>(r: Option<&Result<T, HostError>>) -> bool {
    matches!(
        r,
        Some(Err(HostError::Denied {
            by: DeniedBy::Secrecy,
            ..
        }))
    )
}

/// Every secrecy sink refuses a call whose secrecy flag is set, through the raw trait.
///
/// # Panics
///
/// When the host fails the case.
pub fn sinks(f: &dyn Fixture) {
    let scope = narrow_scope();
    let host = f.host(&scope);
    let frame = root(&scope);
    let calls = f.calls();
    for (what, call) in [
        ("network", calls.network),
        ("command", calls.command),
        ("write", calls.write),
        ("mcp_call", calls.mcp),
        ("call_tool", calls.any_tool),
    ] {
        let (cx, _) = effect(1, None);
        let r = run_for(
            Arc::clone(&host).call_tool(frame.clone(), cx, secret_call(call)),
            Duration::from_secs(10),
        );
        assert!(
            denied_secrecy(r.as_ref()),
            "the {what} sink must refuse a secret: {r:?}"
        );
    }
    let (cx, _) = effect(2, None);
    let req = ModelRequest {
        builtin: Arc::from("model.generate"),
        model: f.hosted_model(),
        prompt: Value::text("the key is").with_labels(crate::labels::Labels::source(0)),
        max_tokens: Some(16),
        options: Vec::new(),
        secret: true,
    };
    let r = run_for(
        Arc::clone(&host).generate(frame.clone(), cx, req),
        Duration::from_secs(10),
    );
    assert!(
        denied_secrecy(r.as_ref()),
        "a hosted model must refuse a secret: {:?}",
        r.map(|r| r.map(|m| m.text))
    );
    for op in sink_operations() {
        let (cx, _) = effect(3, None);
        let what = Arc::clone(&op.builtin);
        let r = run_for(
            Arc::clone(&host).operate(frame.clone(), cx, op),
            Duration::from_secs(10),
        );
        assert!(denied_secrecy(r.as_ref()), "`{what}` must refuse a secret");
    }
    let r = host.emit(&frame, sink_event());
    assert!(
        denied_secrecy(Some(&r)),
        "`session.emit` must refuse a secret"
    );
}

/// The cancellation cases.
///
/// # Panics
///
/// When the host fails the case.
pub fn cancellation(f: &dyn Fixture) {
    let scope = narrow_scope();
    let calls = f.calls();
    let frame = root(&scope);
    let fresh = || f.host(&scope);

    // Every tool of the table is bounded; unlisted and server-claimed read-only tools drain.
    let host = fresh();
    for t in f.tool_table(&host) {
        let c = host.tool_class(&frame, &t);
        assert!(
            c.is_bounded(),
            "`{t}` must declare a bound of at most {MAX_DRAIN:?}"
        );
    }
    for t in ["no_such_tool", "mcp__server__claims_read_only"] {
        let c = host.tool_class(&frame, t);
        assert_eq!(c.class, CancelClass::Drain, "`{t}` must be drain");
        assert!(c.is_bounded());
    }

    // An abort call is acknowledged within a bound.
    let (cx, token) = effect(3, None);
    let fut = Arc::clone(&host).call_tool(frame.clone(), cx, calls.slow_abort.clone());
    let cancel_at = Instant::now() + Duration::from_millis(50);
    let r = run_for(
        Fire {
            fut,
            token: token.clone(),
            at: cancel_at,
        },
        Duration::from_secs(5),
    );
    assert!(
        matches!(r, Some(Err(HostError::Cancelled(CancelOutcome::Aborted)))),
        "an abort call must be aborted and acknowledged: {:?}",
        r.map(|r| r.map(|o| o.value))
    );

    // A remote one is abandoned.
    let (cx, token) = effect(4, None);
    let fut = Arc::clone(&host).call_tool(frame.clone(), cx, calls.slow_remote.clone());
    let r = run_for(
        Fire {
            fut,
            token,
            at: Instant::now() + Duration::from_millis(50),
        },
        Duration::from_secs(5),
    );
    assert!(
        matches!(r, Some(Err(HostError::Cancelled(CancelOutcome::Abandoned)))),
        "a remote abort call must be abandoned"
    );

    // A never-ending drain call is failed at its declared bound, cancelled or not.
    let bound = host
        .tool_class(&frame, &calls.never_drain.tool)
        .max_running
        .expect("a drain tool declares a bound");
    let (cx, token) = effect(5, None);
    token.cancel();
    let start = Instant::now();
    let r = run_for(
        Arc::clone(&host).call_tool(frame.clone(), cx, calls.never_drain.clone()),
        bound + Duration::from_secs(5),
    );
    assert!(r.is_some(), "a drain call must end at its bound");
    assert!(start.elapsed() >= bound, "a drain call must not be aborted");
    assert!(
        matches!(r, Some(Err(HostError::Timeout { .. }))),
        "a drain call at its bound fails"
    );

    // The host bounds a call by its deadline.
    let (cx, _) = effect(6, Some(Instant::now() + Duration::from_millis(100)));
    let r = run_for(
        Arc::clone(&host).call_tool(frame.clone(), cx, calls.slow_abort.clone()),
        Duration::from_secs(5),
    );
    assert!(
        matches!(r, Some(Err(HostError::Timeout { .. }))),
        "a call must end at its deadline"
    );

    // An approval-pending call is withdrawn and never runs; a later answer changes nothing.
    let host = fresh();
    let (cx, token) = effect(7, None);
    let fut = Arc::clone(&host).call_tool(frame.clone(), cx, calls.needs_approval.clone());
    let r = run_for(
        Fire {
            fut,
            token,
            at: Instant::now() + Duration::from_millis(30),
        },
        Duration::from_secs(5),
    );
    assert!(
        matches!(r, Some(Err(HostError::Cancelled(CancelOutcome::Withdrawn)))),
        "an approval-pending call must be withdrawn"
    );
    assert!(
        !f.answer_approval(&host, true, true),
        "an answer after the withdrawal must be discarded"
    );
    assert_eq!(
        f.remembered(&host),
        (false, false),
        "a discarded answer changes no session approval and records no denial"
    );

    // An answer before the withdrawal starts the call.
    let host = fresh();
    let (cx, _) = effect(8, None);
    let fut = Arc::clone(&host).call_tool(frame.clone(), cx, calls.needs_approval.clone());
    let h2 = Arc::clone(&host);
    let r = run_for(
        Answer {
            fut,
            answer: Some(Box::new(move || f.answer_approval(&h2, true, false))),
        },
        Duration::from_secs(5),
    );
    assert!(
        matches!(r, Some(Ok(_))),
        "an approved call runs: {:?}",
        r.map(|r| r.map(|o| o.value))
    );

    // After a session stop, a cancelled call drains instead of aborting.
    let host = fresh();
    let (cx, token) = effect(9, None);
    let fut = Arc::clone(&host).call_tool(frame, cx, calls.slow_abort);
    f.stop_session(&host);
    let r = run_for(
        Fire {
            fut,
            token,
            at: Instant::now() + Duration::from_millis(30),
        },
        // Still running (draining) when the wait ends counts as draining too.
        Duration::from_millis(500),
    );
    assert!(
        !matches!(
            r,
            Some(Err(HostError::Cancelled(
                CancelOutcome::Aborted | CancelOutcome::Abandoned
            )))
        ),
        "after a session stop every in-flight call drains"
    );
}

/// Every case.
///
/// # Panics
///
/// When the host fails a case.
pub fn run_all(f: &dyn Fixture) {
    frames(f);
    sinks(f);
    cancellation(f);
}

/// Fires `token` at `at` while polling `fut`.
struct Fire<T> {
    fut: HostFuture<T>,
    token: CancelToken,
    at: Instant,
}

impl<T> Future for Fire<T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if Instant::now() >= self.at {
            self.token.cancel();
        }
        self.fut.as_mut().poll(cx)
    }
}

/// Answers the pending approval on the second poll.
struct Answer<'a, T> {
    fut: HostFuture<T>,
    answer: Option<Box<dyn FnOnce() -> bool + 'a>>,
}

impl<T> Future for Answer<'_, T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let r = self.fut.as_mut().poll(cx);
        if r.is_pending()
            && let Some(a) = self.answer.take()
        {
            a();
        }
        r
    }
}

/// The fixture for [`crate::scripted::ScriptedHost`].
#[derive(Default)]
pub struct ScriptedFixture {
    made: std::sync::Mutex<Vec<Made>>,
}

type Made = (Arc<dyn Host>, Arc<crate::scripted::ScriptedHost>);

impl ScriptedFixture {
    fn scripted(&self, host: &Arc<dyn Host>) -> Arc<crate::scripted::ScriptedHost> {
        let made = self
            .made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        made.iter()
            .find(|(d, _)| Arc::ptr_eq(d, host))
            .map(|(_, s)| Arc::clone(s))
            .expect("a host this fixture made")
    }
}

fn call(tool: &str, builtin: &str, args: Vec<(&str, Value)>) -> ToolInvocation {
    ToolInvocation {
        tool: Arc::from(tool),
        builtin: Arc::from(builtin),
        args: args.into_iter().map(|(n, v)| (Arc::from(n), v)).collect(),
        secret: false,
        in_defer: false,
    }
}

impl Fixture for ScriptedFixture {
    fn host(&self, scope: &Scope) -> Arc<dyn Host> {
        use crate::host::ToolClass;
        use crate::scripted::{Behaviour, ScriptedHost};
        let h = Arc::new(ScriptedHost::new("turn", scope.clone()));
        let ok = Value::text("done");
        h.tool(
            "read_file",
            ToolClass::abort(false),
            Behaviour::Reply(ok.clone()),
        )
        .tool(
            "write_file",
            ToolClass::drain(Duration::from_secs(60)),
            Behaviour::Reply(ok.clone()),
        )
        .tool(
            "web_fetch",
            ToolClass::abort(true),
            Behaviour::Reply(ok.clone()),
        )
        .tool(
            "run_command",
            ToolClass::drain(Duration::from_secs(600)),
            Behaviour::Reply(ok.clone()),
        )
        .tool(
            "slow_read",
            ToolClass::abort(false),
            Behaviour::After(Duration::from_secs(30), ok.clone()),
        )
        .tool(
            "slow_remote",
            ToolClass::abort(true),
            Behaviour::After(Duration::from_secs(30), ok.clone()),
        )
        .tool(
            "never_drain",
            ToolClass::drain(Duration::from_millis(300)),
            Behaviour::Never,
        )
        .tool(
            "needs_approval",
            ToolClass::drain(Duration::from_secs(60)),
            Behaviour::Approval(ok),
        )
        .card(crate::host::ModelCard {
            name: Arc::from("hosted"),
            location: Location::Cloud,
            card: crate::frame::Card::default(),
        });
        let dynh: Arc<dyn Host> = h.clone();
        self.made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((Arc::clone(&dynh), h));
        dynh
    }

    fn calls(&self) -> Calls {
        let secret = |s: &str| Value::text(s);
        Calls {
            network: call(
                "web_fetch",
                "tools.web_fetch",
                vec![("url", secret("https://x"))],
            ),
            command: call(
                "run_command",
                "tools.run_command",
                vec![("command", secret("echo"))],
            ),
            write: call(
                "write_file",
                "tools.write_file",
                vec![
                    ("path", Value::text("/work/out.txt")),
                    ("content", secret("k")),
                ],
            ),
            mcp: call(
                "mcp__srv__t",
                "tools.mcp_call",
                vec![
                    ("server", Value::text("srv")),
                    ("tool", Value::text("t")),
                    ("arguments", secret("{}")),
                ],
            ),
            any_tool: call(
                "read_file",
                "tools.call_tool",
                vec![
                    ("name", Value::text("read_file")),
                    ("arguments", secret("{}")),
                ],
            ),
            slow_abort: call(
                "slow_read",
                "tools.call_tool",
                vec![("name", Value::text("slow_read"))],
            ),
            slow_remote: call(
                "slow_remote",
                "tools.call_tool",
                vec![("name", Value::text("slow_remote"))],
            ),
            never_drain: call(
                "never_drain",
                "tools.call_tool",
                vec![("name", Value::text("never_drain"))],
            ),
            needs_approval: call(
                "needs_approval",
                "tools.call_tool",
                vec![("name", Value::text("needs_approval"))],
            ),
        }
    }

    fn tool_table(&self, host: &Arc<dyn Host>) -> Vec<String> {
        self.scripted(host)
            .tool_table()
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    fn answer_approval(&self, host: &Arc<dyn Host>, approve: bool, for_session: bool) -> bool {
        self.scripted(host)
            .pending_approvals()
            .last()
            .is_some_and(|(_, g)| g.answer(approve, for_session))
    }

    fn remembered(&self, host: &Arc<dyn Host>) -> (bool, bool) {
        let (allowed, denied) = self.scripted(host).remembered();
        (
            allowed.contains("needs_approval"),
            denied.contains("needs_approval"),
        )
    }

    fn stop_session(&self, host: &Arc<dyn Host>) {
        self.scripted(host).stop_session();
    }

    fn hosted_model(&self) -> ModelRef {
        ModelRef(Arc::from("hosted"))
    }

    fn loaded_harness(&self, host: &Arc<dyn Host>) -> FrameOrigin {
        let origin = FrameOrigin::Harness {
            package: Arc::from("app"),
            entry_hash: [0x11; 32],
            key: Arc::from("main.ratio::h"),
        };
        self.scripted(host)
            .load_packages([(Arc::from("app"), [0x11; 32])], [origin.clone()]);
        origin
    }
}
