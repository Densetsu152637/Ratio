//! `ScriptedHost`: a host answering from scripts, for tests and for the host contract suite
//! (chapter 10, "Where cross-repository tests live").
//!
//! It holds a tool table (each tool's cancellation class, declared bound and behaviour), a
//! queue of model replies and model cards, operation and approval scripts, the session's
//! scope and a record of every frame it opened (checked like any host must: against its own
//! record of the parent), and the secret paths. It enforces the secrecy sinks itself from the
//! per-call flag, as an engine does at dispatch, so the contract suite can drive it without
//! the guard. It records what it saw: calls in start order with their deadlines, how each
//! cancelled call ended, the events emitted and the frames opened.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use crate::frame::{FrameCtx, FrameKind, FrameOrigin, Location, Scope, check_within};
use crate::host::{
    ApprovalRequest, CancelClass, CancelOutcome, ChatRequest, ConvId, ConvSelector, Countable,
    DeniedBy, Digest, EffectCtx, FrameOutcome, Host, HostError, HostFuture, Labelled, ModelCard,
    ModelRef, ModelRequest, ModelResponse, Operation, RatioEvent, SinkRule, ToolClass,
    ToolInvocation, ToolOutcome, Usage, sink_rule,
};
use crate::labels::Labels;
use crate::value::Value;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a scripted tool does when called.
#[derive(Clone, Debug)]
pub enum Behaviour {
    /// Answers at once.
    Reply(Value),
    /// Answers after a delay.
    After(Duration, Value),
    /// Fails (`ToolFailed`).
    Fail(String),
    /// Never finishes by itself: only its deadline, its declared bound or (for an `abort`
    /// tool) its cancellation ends it.
    Never,
    /// Waits until this many calls of the tool are in flight at once, then answers: it can
    /// only finish under real concurrency.
    Barrier(usize, Value),
    /// Answers from the list, one per call, in call order (the last repeats).
    Sequence(Vec<Result<Value, String>>),
    /// Waits for an approval first ([`ScriptedHost::pending_approvals`]); then answers.
    Approval(Value),
    /// By the text of the call's first argument: answers from that argument's list, one per
    /// call, in call order (the last repeats); an argument not listed answers `()`.
    PerArg(Vec<(String, Vec<Result<Value, String>>)>),
}

/// A tool of the scripted table.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub class: ToolClass,
    pub behaviour: Behaviour,
}

/// An engine operation the host received: its built-in and arguments.
pub type SeenOp = (String, Vec<(Arc<str>, Value)>);

/// A call the host received.
#[derive(Clone, Debug)]
pub struct Seen {
    pub tool: String,
    pub key: String,
    pub deadline: Option<Instant>,
    pub secret: bool,
    pub args: Vec<(Arc<str>, Value)>,
    /// When the call started, and when it ended (`None` while it runs).
    pub started: Instant,
    pub finished: Option<Instant>,
}

/// A pending approval, answered by a test (or withdrawn by a cancellation), atomically.
#[derive(Debug, Default)]
pub struct ApprovalGate {
    state: Mutex<GateState>,
    wakers: Mutex<Vec<Waker>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GateState {
    #[default]
    Pending,
    Answered {
        approve: bool,
        for_session: bool,
    },
    Withdrawn,
}

impl ApprovalGate {
    /// The user answers. `false` when the prompt was already withdrawn: the answer is
    /// discarded entirely.
    pub fn answer(&self, approve: bool, for_session: bool) -> bool {
        let ok = {
            let mut s = lock(&self.state);
            if *s != GateState::Pending {
                return false;
            }
            *s = GateState::Answered {
                approve,
                for_session,
            };
            true
        };
        for w in std::mem::take(&mut *lock(&self.wakers)) {
            w.wake();
        }
        ok
    }

    /// The call's scope was cancelled. `false` when the answer came first.
    fn withdraw(&self) -> bool {
        let mut s = lock(&self.state);
        if *s != GateState::Pending {
            return false;
        }
        *s = GateState::Withdrawn;
        true
    }

    fn state(&self, waker: &Waker) -> GateState {
        let s = *lock(&self.state);
        if s == GateState::Pending {
            lock(&self.wakers).push(waker.clone());
        }
        *lock(&self.state)
    }
}

#[derive(Debug, Default)]
struct Barrier {
    arrived: usize,
    wakers: Vec<Waker>,
}

/// The host's state.
#[derive(Default)]
struct State {
    tools: HashMap<String, ToolSpec>,
    replies: VecDeque<Result<Labelled<String>, String>>,
    cards: HashMap<String, ModelCard>,
    ops: HashMap<String, VecDeque<Result<Value, String>>>,
    approvals: VecDeque<bool>,
    frames: HashMap<String, (Scope, FrameKind)>,
    secret_paths: Vec<String>,
    seen: Vec<Seen>,
    ended: Vec<(String, CancelOutcome)>,
    events: Vec<RatioEvent>,
    opened: Vec<String>,
    closed: Vec<(String, FrameOutcome)>,
    sequence: HashMap<String, usize>,
    barriers: HashMap<String, Barrier>,
    gates: Vec<(String, Arc<ApprovalGate>)>,
    session_allowed: HashSet<String>,
    /// The packages loaded (name and IR hash) and the harnesses they hold.
    packages: HashSet<(Arc<str>, [u8; 32])>,
    harnesses: HashSet<FrameOrigin>,
    ops_seen: Vec<SeenOp>,
    asked: Vec<ApprovalRequest>,
    conversation_labels: HashMap<u64, Labels>,
    denied: HashSet<String>,
    model_calls: Vec<ModelRequest>,
}

/// A host answering from scripts (see the module documentation).
pub struct ScriptedHost {
    st: Mutex<State>,
    root: Arc<str>,
    seed: [u8; 32],
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    stopping: AtomicBool,
    jitter: AtomicU64,
    rogue: AtomicBool,
    frozen: AtomicBool,
    count_fails: AtomicBool,
}

impl std::fmt::Debug for ScriptedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScriptedHost")
    }
}

impl ScriptedHost {
    /// A host for a session with `scope`, whose frame paths start with `root`.
    #[must_use]
    pub fn new(root: &str, scope: Scope) -> ScriptedHost {
        let mut st = State::default();
        st.frames.insert(root.to_owned(), (scope, FrameKind::Entry));
        ScriptedHost {
            st: Mutex::new(st),
            root: Arc::from(root),
            seed: [42; 32],
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            jitter: AtomicU64::new(0),
            rogue: AtomicBool::new(false),
            frozen: AtomicBool::new(false),
            count_fails: AtomicBool::new(false),
        }
    }

    /// The session a program runs in on this host (its root and scope).
    #[must_use]
    pub fn session(&self) -> crate::frame::Session {
        let scope = lock(&self.st)
            .frames
            .get(&*self.root)
            .map_or_else(Scope::session, |(s, _)| s.clone());
        crate::frame::Session {
            scope,
            root: Arc::clone(&self.root),
        }
    }

    /// Adds a tool to the table.
    pub fn tool(&self, name: &str, class: ToolClass, behaviour: Behaviour) -> &Self {
        lock(&self.st)
            .tools
            .insert(name.to_owned(), ToolSpec { class, behaviour });
        self
    }

    /// Queues a model reply.
    pub fn reply(&self, text: &str, labels: Labels) -> &Self {
        lock(&self.st)
            .replies
            .push_back(Ok(Labelled::new(text.to_owned(), labels)));
        self
    }

    /// Adds a model card.
    pub fn card(&self, card: ModelCard) -> &Self {
        lock(&self.st).cards.insert(card.name.to_string(), card);
        self
    }

    /// Queues an answer for an engine operation.
    pub fn op(&self, builtin: &str, answer: Result<Value, String>) -> &Self {
        lock(&self.st)
            .ops
            .entry(builtin.to_owned())
            .or_default()
            .push_back(answer);
        self
    }

    /// Queues an approval answer.
    pub fn approval(&self, approve: bool) -> &Self {
        lock(&self.st).approvals.push_back(approve);
        self
    }

    /// A path pattern of `ratio.secret_paths` (`*.env` ends with `.env`; anything else is a
    /// whole path or a path's last component).
    pub fn secret_path(&self, pattern: &str) -> &Self {
        lock(&self.st).secret_paths.push(pattern.to_owned());
        self
    }

    /// Answers are delayed by a pseudo-random number of polls from `seed` (random
    /// completion orders); 0 turns it off.
    pub fn jitter(&self, seed: u64) -> &Self {
        self.jitter.store(seed, Ordering::Relaxed);
        self
    }

    /// Loads a program, as a host loads the packages it runs: their names and IR hashes, and
    /// the harnesses in them. Frames are checked against what was loaded.
    pub fn load(&self, program: &crate::Prepared) -> &Self {
        let (packages, harnesses) = program.loaded();
        self.load_packages(packages, harnesses)
    }

    /// Loads packages (name and IR hash) and the harnesses they hold.
    pub fn load_packages(
        &self,
        packages: impl IntoIterator<Item = (Arc<str>, [u8; 32])>,
        harnesses: impl IntoIterator<Item = FrameOrigin>,
    ) -> &Self {
        let mut st = lock(&self.st);
        st.packages.extend(packages);
        st.harnesses.extend(harnesses);
        self
    }

    /// Labels conversation `conv` (it holds a labelled message).
    pub fn label_conversation(&self, conv: u64, labels: Labels) -> &Self {
        lock(&self.st).conversation_labels.insert(conv, labels);
        self
    }

    /// A host that breaks the contract: it ignores cancellation tokens and declared bounds
    /// (for the scheduler's own bounds).
    pub fn rogue(&self) -> &Self {
        self.rogue.store(true, Ordering::SeqCst);
        self
    }

    /// A host whose tokenizer fails.
    pub fn failing_tokenizer(&self) -> &Self {
        self.count_fails.store(true, Ordering::SeqCst);
        self
    }

    /// A host whose timer never fires.
    pub fn frozen_timer(&self) -> &Self {
        self.frozen.store(true, Ordering::SeqCst);
        self
    }

    /// The engine operations received, with their arguments as the host saw them.
    #[must_use]
    pub fn ops_seen(&self) -> Vec<SeenOp> {
        lock(&self.st).ops_seen.clone()
    }

    /// `session.control stop`: from now on every in-flight call drains, whatever its class.
    pub fn stop_session(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    /// The tool calls received, in start order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.st).seen.clone()
    }

    /// How each cancelled call ended, by key.
    #[must_use]
    pub fn ended(&self) -> Vec<(String, CancelOutcome)> {
        lock(&self.st).ended.clone()
    }

    /// The output-only events emitted.
    #[must_use]
    pub fn events(&self) -> Vec<RatioEvent> {
        lock(&self.st).events.clone()
    }

    /// The approvals the user was asked, as the host received them.
    #[must_use]
    pub fn asked(&self) -> Vec<ApprovalRequest> {
        lock(&self.st).asked.clone()
    }

    /// The model requests received.
    #[must_use]
    pub fn model_calls(&self) -> Vec<ModelRequest> {
        lock(&self.st).model_calls.clone()
    }

    /// The frames opened, by path, in order.
    #[must_use]
    pub fn opened(&self) -> Vec<String> {
        lock(&self.st).opened.clone()
    }

    /// The frames closed, in order.
    #[must_use]
    pub fn closed(&self) -> Vec<(String, FrameOutcome)> {
        lock(&self.st).closed.clone()
    }

    /// The approvals waiting for an answer, with their tool.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<(String, Arc<ApprovalGate>)> {
        lock(&self.st).gates.clone()
    }

    /// Tools allowed for the session by an answer, and tools whose denial was remembered.
    #[must_use]
    pub fn remembered(&self) -> (HashSet<String>, HashSet<String>) {
        let st = lock(&self.st);
        (st.session_allowed.clone(), st.denied.clone())
    }

    /// The most calls in flight at once.
    #[must_use]
    pub fn peak_in_flight(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// The tool table's names and classes.
    #[must_use]
    pub fn tool_table(&self) -> Vec<(String, ToolClass)> {
        lock(&self.st)
            .tools
            .iter()
            .map(|(n, t)| (n.clone(), t.class))
            .collect()
    }

    fn matches_secret(&self, path: &str) -> bool {
        lock(&self.st).secret_paths.iter().any(|p| {
            p.strip_prefix('*').map_or_else(
                || path == p || path.rsplit(['/', '\\']).next() == Some(p.as_str()),
                |suffix| path.ends_with(suffix),
            )
        })
    }

    fn polls(&self) -> u32 {
        let mut x = self.jitter.load(Ordering::Relaxed);
        if x == 0 {
            return 0;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.jitter.store(x, Ordering::Relaxed);
        u32::try_from(x % 6).unwrap_or(0)
    }

    /// The engine's dispatch check of a secret call, from the per-call flag.
    fn refuse_secret(&self, frame: &FrameCtx, call: &ToolInvocation) -> Option<HostError> {
        if !call.secret {
            return None;
        }
        match sink_rule(&call.builtin, &call.tool) {
            SinkRule::Open => None,
            SinkRule::Refuse => Some(HostError::secrecy(format!(
                "the engine refuses a secret to `{}`",
                call.tool
            ))),
            SinkRule::UnlessSecretPath => {
                let p = call
                    .arg("path")
                    .and_then(Value::as_text)
                    .unwrap_or_default();
                (!self.is_secret_path(frame, p)).then(|| {
                    HostError::secrecy(format!("the engine refuses a secret to `{}`", call.tool))
                })
            }
        }
    }

    /// The frame's scope must allow the tool (the engine's `CallScope` check).
    fn refuse_scope(&self, frame: &FrameCtx, tool: &str) -> Option<HostError> {
        let st = lock(&self.st);
        let scope = st.frames.get(&*frame.path).map(|(s, _)| s);
        match scope {
            Some(s) if s.tools.allows(tool) => None,
            Some(_) => Some(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!("`{tool}` is outside the frame's tools"),
            }),
            None => Some(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!("the host has no record of the frame `{}`", frame.path),
            }),
        }
    }
}

/// Resolves when the first of two futures does.
struct Either<A, B>(A, B);

enum Which<T, U> {
    First(T),
    Second(U),
}

impl<A: Future + Unpin, B: Future + Unpin> Future for Either<A, B> {
    type Output = Which<A::Output, B::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(a) = Pin::new(&mut self.0).poll(cx) {
            return Poll::Ready(Which::First(a));
        }
        if let Poll::Ready(b) = Pin::new(&mut self.1).poll(cx) {
            return Poll::Ready(Which::Second(b));
        }
        Poll::Pending
    }
}

/// Never resolves.
struct Never;

impl Future for Never {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

/// Resolves after a number of polls.
struct Polls(u32);

impl Future for Polls {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            return Poll::Ready(());
        }
        self.0 -= 1;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// A sleep until `at`, or never.
fn sleep_until(at: Option<Instant>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    match at {
        Some(at) => Box::pin(crate::host::sleep::Sleep::until(at)),
        None => Box::pin(Never),
    }
}

/// Waits for a barrier of `n` calls of `tool`.
struct BarrierWait {
    host: Arc<ScriptedHost>,
    tool: String,
    n: usize,
    joined: bool,
}

impl Future for BarrierWait {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let joined = self.joined;
        let mut st = lock(&self.host.st);
        let b = st.barriers.entry(self.tool.clone()).or_default();
        if !joined {
            b.arrived += 1;
        }
        if b.arrived >= self.n {
            for w in std::mem::take(&mut b.wakers) {
                w.wake();
            }
            return Poll::Ready(());
        }
        b.wakers.push(cx.waker().clone());
        drop(st);
        self.joined = true;
        Poll::Pending
    }
}

/// Waits for an approval gate.
struct GateWait(Arc<ApprovalGate>);

impl Future for GateWait {
    type Output = GateState;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<GateState> {
        match self.0.state(cx.waker()) {
            GateState::Pending => Poll::Pending,
            s => Poll::Ready(s),
        }
    }
}

impl ScriptedHost {
    /// The call itself, by its behaviour, without cancellation.
    async fn work(
        self: Arc<Self>,
        tool: String,
        first: String,
        behaviour: Behaviour,
    ) -> Result<Value, HostError> {
        let behaviour = match behaviour {
            Behaviour::PerArg(map) => {
                let list = map
                    .into_iter()
                    .find(|(a, _)| *a == first)
                    .map_or_else(|| vec![Ok(Value::unit())], |(_, l)| l);
                return self.sequence(&format!("{tool}#{first}"), &list);
            }
            b => b,
        };
        match behaviour {
            Behaviour::Reply(v) => Ok(v),
            Behaviour::After(d, v) => {
                crate::host::sleep::Sleep::until(Instant::now() + d).await;
                Ok(v)
            }
            Behaviour::Fail(m) => Err(HostError::ToolFailed {
                message: Labelled::plain(m),
                data: Value::unit(),
            }),
            Behaviour::Never => {
                Never.await;
                Ok(Value::unit())
            }
            Behaviour::Barrier(n, v) => {
                BarrierWait {
                    host: Arc::clone(&self),
                    tool,
                    n,
                    joined: false,
                }
                .await;
                Ok(v)
            }
            Behaviour::Sequence(list) => self.sequence(&tool, &list),
            Behaviour::Approval(v) => Ok(v),
            Behaviour::PerArg(_) => Ok(Value::unit()),
        }
    }

    /// The next answer of a list, counted under `counter`.
    fn sequence(&self, counter: &str, list: &[Result<Value, String>]) -> Result<Value, HostError> {
        let i = {
            let mut st = lock(&self.st);
            let c = st.sequence.entry(counter.to_owned()).or_insert(0);
            let i = *c;
            *c += 1;
            i
        };
        match list.get(i).or(list.last()) {
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(m)) => Err(HostError::ToolFailed {
                message: Labelled::plain(m.clone()),
                data: Value::unit(),
            }),
            None => Ok(Value::unit()),
        }
    }

    async fn run_tool(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        call: ToolInvocation,
    ) -> Result<ToolOutcome, HostError> {
        let key = cx.key.to_string();
        let tool = call.tool.to_string();
        let spec = lock(&self.st).tools.get(&tool).cloned();
        lock(&self.st).seen.push(Seen {
            tool: tool.clone(),
            key: key.clone(),
            deadline: cx.deadline,
            secret: call.secret,
            args: call.args.clone(),
            started: Instant::now(),
            finished: None,
        });
        let index = lock(&self.st).seen.len() - 1;
        // The engine's own checks: the frame's scope, then secrecy from the flag.
        if let Some(e) = self
            .refuse_scope(&frame, &tool)
            .or_else(|| self.refuse_secret(&frame, &call))
        {
            return Err(e);
        }
        let Some(spec) = spec else {
            return Err(HostError::NotFound {
                message: format!("no tool `{tool}`"),
            });
        };
        let n = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        let first = call
            .args
            .first()
            .and_then(|(_, v)| v.as_text())
            .unwrap_or_default()
            .to_owned();
        let r = Arc::clone(&self)
            .dispatch(&key, &tool, &first, spec, &cx)
            .await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if let Some(s) = lock(&self.st).seen.get_mut(index) {
            s.finished = Some(Instant::now());
        }
        Polls(self.polls()).await;
        let r = r.map(|value| {
            // The source rules: a read of a secret path is labelled.
            let extra = self.source_labels(&frame, &call.builtin, &call.args);
            let own = value.labels() | extra;
            ToolOutcome {
                value: value.with_labels(own),
            }
        });
        if let Err(HostError::Cancelled(o)) = &r {
            lock(&self.st).ended.push((key, *o));
        }
        r
    }

    async fn dispatch(
        self: Arc<Self>,
        key: &str,
        tool: &str,
        first: &str,
        spec: ToolSpec,
        cx: &EffectCtx,
    ) -> Result<Value, HostError> {
        // An approval first: withdrawn atomically if the scope is cancelled before the answer.
        if let Behaviour::Approval(_) = &spec.behaviour {
            let gate = Arc::new(ApprovalGate::default());
            lock(&self.st)
                .gates
                .push((key.to_owned(), Arc::clone(&gate)));
            let w = Either(
                Box::pin(GateWait(Arc::clone(&gate))),
                Box::pin(cx.cancel.cancelled()),
            )
            .await;
            let answered = match w {
                Which::First(s) => s,
                Which::Second(()) => {
                    if gate.withdraw() {
                        // Withdrawn: the call never runs, and nothing is remembered.
                        return Err(HostError::Cancelled(CancelOutcome::Withdrawn));
                    }
                    *lock(&gate.state)
                }
            };
            if let GateState::Answered {
                approve,
                for_session,
            } = answered
            {
                let mut st = lock(&self.st);
                if approve && for_session {
                    st.session_allowed.insert(tool.to_owned());
                }
                if !approve {
                    st.denied.insert(tool.to_owned());
                    return Err(HostError::Denied {
                        by: DeniedBy::User,
                        message: format!("the user refused `{tool}`"),
                    });
                }
            }
        }
        if self.rogue.load(Ordering::SeqCst) {
            // Neither the token nor the bound ends the call.
            return Arc::clone(&self)
                .work(tool.to_owned(), first.to_owned(), spec.behaviour)
                .await;
        }
        let stopping = self.stopping.load(Ordering::SeqCst);
        let class = if stopping {
            CancelClass::Drain
        } else {
            spec.class.class
        };
        let work =
            Box::pin(Arc::clone(&self).work(tool.to_owned(), first.to_owned(), spec.behaviour));
        match class {
            CancelClass::Abort => {
                // Stopped at once when cancelled, and bounded by the deadline.
                let ended = Either(
                    work,
                    Box::pin(Either(
                        Box::pin(cx.cancel.cancelled()),
                        sleep_until(cx.deadline),
                    )),
                )
                .await;
                match ended {
                    Which::First(r) => r,
                    Which::Second(Which::First(())) => {
                        if self.stopping.load(Ordering::SeqCst) {
                            return Err(HostError::Cancelled(CancelOutcome::Drained));
                        }
                        Err(HostError::Cancelled(if spec.class.remote {
                            CancelOutcome::Abandoned
                        } else {
                            CancelOutcome::Aborted
                        }))
                    }
                    Which::Second(Which::Second(())) => Err(HostError::Timeout {
                        message: format!("`{tool}` passed its deadline"),
                    }),
                }
            }
            CancelClass::Drain => {
                // Runs to its end, its deadline or its declared bound (then failed there).
                let bound = spec.class.max_running.map(|d| Instant::now() + d);
                let limit = [bound, cx.deadline].into_iter().flatten().min();
                match Either(work, sleep_until(limit)).await {
                    Which::First(r) => {
                        if cx.cancel.is_cancelled() {
                            lock(&self.st)
                                .ended
                                .push((key.to_owned(), CancelOutcome::Drained));
                        }
                        r
                    }
                    Which::Second(()) => Err(HostError::Timeout {
                        message: format!("`{tool}` was failed at its bound"),
                    }),
                }
            }
        }
    }
}

impl Host for ScriptedHost {
    fn generate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ModelRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        Box::pin(async move {
            lock(&self.st).model_calls.push(req.clone());
            if req.secret {
                let local = self
                    .resolve_model(&frame, &req.model)
                    .is_ok_and(|c| c.location == Location::Local);
                if !local {
                    return Err(HostError::secrecy(
                        "the engine refuses a secret to a hosted model",
                    ));
                }
            }
            let r = lock(&self.st).replies.pop_front();
            let cancelled = Either(
                Box::pin(Polls(self.polls())),
                Box::pin(cx.cancel.cancelled()),
            )
            .await;
            if let Which::Second(()) = cancelled {
                return Err(HostError::Cancelled(CancelOutcome::Aborted));
            }
            match r {
                Some(Ok(text)) => {
                    let output = (text.value.len() as u64).div_ceil(4);
                    Ok(ModelResponse {
                        text,
                        usage: Usage {
                            input_tokens: 0,
                            output_tokens: output,
                            cost: 0,
                        },
                    })
                }
                Some(Err(m)) => Err(HostError::ModelFailed {
                    message: Labelled::plain(m),
                }),
                None => Err(HostError::ModelFailed {
                    message: Labelled::plain("no scripted reply left".to_owned()),
                }),
            }
        })
    }

    fn embed(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        texts: Vec<Labelled<String>>,
        _: Option<ModelRef>,
    ) -> HostFuture<Result<Vec<Vec<f32>>, HostError>> {
        #[allow(clippy::cast_precision_loss, reason = "a scripted embedding")]
        let v = texts.iter().map(|t| vec![t.value.len() as f32]).collect();
        Box::pin(async move { Ok(v) })
    }

    fn count_tokens(
        self: Arc<Self>,
        _: FrameCtx,
        what: Countable,
    ) -> HostFuture<Result<u64, HostError>> {
        let n = match &what {
            Countable::Text(t) => (t.value.len() as u64).div_ceil(4),
            Countable::Prompt(v) => v.weight().div_ceil(4),
        };
        let polls = self.polls();
        let fail = self.count_fails.load(Ordering::SeqCst);
        Box::pin(async move {
            Polls(polls).await;
            if fail {
                return Err(HostError::ModelFailed {
                    message: Labelled::plain("the tokenizer failed".to_owned()),
                });
            }
            Ok(n)
        })
    }

    fn resolve_model(&self, _: &FrameCtx, model: &ModelRef) -> Result<ModelCard, HostError> {
        lock(&self.st)
            .cards
            .get(&*model.0)
            .cloned()
            .ok_or_else(|| HostError::NotFound {
                message: format!("no model `{}`", model.0),
            })
    }

    fn call_tool(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        call: ToolInvocation,
    ) -> HostFuture<Result<ToolOutcome, HostError>> {
        Box::pin(self.run_tool(frame, cx, call))
    }

    fn tool_class(&self, _: &FrameCtx, tool: &str) -> ToolClass {
        lock(&self.st)
            .tools
            .get(tool)
            .map_or(ToolClass::UNLISTED, |t| t.class)
    }

    fn is_secret_path(&self, _: &FrameCtx, path: &str) -> bool {
        self.matches_secret(path)
    }

    fn source_labels(&self, frame: &FrameCtx, builtin: &str, args: &[(Arc<str>, Value)]) -> Labels {
        let reads = matches!(builtin, "tools.read_file" | "tools.grep" | "tools.list_dir");
        let path = args
            .iter()
            .find(|(n, _)| &**n == "path")
            .and_then(|(_, v)| v.as_text())
            .unwrap_or_default();
        if reads && self.is_secret_path(frame, path) {
            Labels::source(0)
        } else {
            Labels::NONE
        }
    }

    fn conversation(&self, _: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError> {
        Ok(ConvId(u64::from(which == ConvSelector::Main)))
    }

    fn conversation_digest(&self, _: &FrameCtx, conv: ConvId) -> Result<Digest, HostError> {
        let mut d = [0u8; 32];
        d[..8].copy_from_slice(&conv.0.to_le_bytes());
        Ok(Digest(d))
    }

    fn conversation_labels(&self, _: &FrameCtx, conv: ConvId) -> Result<Labels, HostError> {
        Ok(lock(&self.st)
            .conversation_labels
            .get(&conv.0)
            .copied()
            .unwrap_or(Labels::NONE))
    }

    fn chat(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        _: ConvId,
        req: ChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        let m = ModelRequest {
            builtin: req.builtin,
            model: req.model.unwrap_or_else(|| ModelRef(Arc::from("default"))),
            prompt: Value::unit(),
            max_tokens: req.max_tokens,
            options: req.options,
            secret: req.secret,
        };
        self.generate(frame, cx, m)
    }

    fn open_frame(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError> {
        let mut st = lock(&self.st);
        // The parent from the host's own record, never from the context passed in.
        let Some((record, _)) = st.frames.get(&*parent.path).cloned() else {
            return Err(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!("no record of the frame `{}`", parent.path),
            });
        };
        // The kind must fit the path and the origin: a block is `with`, a harness call is not;
        // an entry's frame has its parent's path, every other frame names its parent. The origin
        // is checked against what the host loaded: a harness by package, IR hash and key; a
        // block by the package of its site.
        let block = &*child.harness == "with";
        let parent_ok = match child.kind {
            FrameKind::Entry => child.path == parent.path,
            FrameKind::Harness | FrameKind::Block => child.parent.as_deref() == Some(&*parent.path),
        };
        let origin_ok = child.origin.fits(child.kind)
            && match &child.origin {
                FrameOrigin::Entry => true,
                FrameOrigin::Block { site } => st.packages.iter().any(|(_, h)| *h == site.0),
                FrameOrigin::Harness {
                    package,
                    entry_hash,
                    ..
                } => {
                    st.packages.contains(&(Arc::clone(package), *entry_hash))
                        && st.harnesses.contains(&child.origin)
                }
            };
        if (child.kind == FrameKind::Block) != block || !parent_ok || !origin_ok {
            return Err(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!("the frame `{}` does not fit its kind", child.path),
            });
        }
        if let Err(d) = check_within(&record, &child.scope, child.kind) {
            return Err(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!(
                    "the frame `{}` is wider than its parent in `{}`",
                    child.path,
                    d.name()
                ),
            });
        }
        st.frames
            .insert(child.path.to_string(), ((*child.scope).clone(), child.kind));
        st.opened.push(child.path.to_string());
        Ok(())
    }

    fn close_frame(&self, frame: &FrameCtx, outcome: FrameOutcome) {
        let mut st = lock(&self.st);
        st.frames.remove(&*frame.path);
        st.closed.push((frame.path.to_string(), outcome));
    }

    fn operate(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        op: Operation,
    ) -> HostFuture<Result<Value, HostError>> {
        let refuse = op.secret && crate::host::operation_rule(&op.builtin) == SinkRule::Refuse;
        let answer = {
            let mut st = lock(&self.st);
            st.ops_seen.push((op.builtin.to_string(), op.args.clone()));
            st.ops.get_mut(&*op.builtin).and_then(VecDeque::pop_front)
        };
        Box::pin(async move {
            if refuse {
                return Err(HostError::secrecy(format!(
                    "the engine refuses a secret to `{}`",
                    op.builtin
                )));
            }
            match answer {
                Some(Ok(v)) => Ok(v),
                Some(Err(m)) => Err(HostError::Invalid { message: m }),
                None => Err(HostError::unsupported(format!(
                    "no scripted answer for `{}`",
                    op.builtin
                ))),
            }
        })
    }

    fn approve(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        req: ApprovalRequest,
    ) -> HostFuture<Result<bool, HostError>> {
        let a = {
            let mut st = lock(&self.st);
            st.asked.push(req);
            st.approvals.pop_front()
        };
        Box::pin(async move {
            a.ok_or(HostError::Denied {
                by: DeniedBy::Unattended,
                message: "no scripted approval".to_owned(),
            })
        })
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn seed(&self) -> [u8; 32] {
        self.seed
    }

    fn timer(&self, at: Instant) -> HostFuture<()> {
        if self.frozen.load(Ordering::SeqCst) {
            return Box::pin(Never);
        }
        Box::pin(crate::host::sleep::Sleep::until(at))
    }

    fn emit(&self, _: &FrameCtx, event: RatioEvent) -> Result<(), HostError> {
        // The engine's own check: a secret at a sink (`session.emit`) is refused.
        if event.secret && crate::host::operation_rule(&event.builtin) == SinkRule::Refuse {
            return Err(HostError::secrecy(format!(
                "the engine refuses a secret to `{}`",
                event.builtin
            )));
        }
        lock(&self.st).events.push(event);
        Ok(())
    }
}
