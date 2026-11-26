//! The scheduler: an evaluation as a `Future` (chapter 03, "Concurrency"; stage 11a).
//!
//! An [`Evaluation`] owns a tree of **tasks**, each a [`Machine`]: the entry's, and one per
//! started branch of a concurrent construct. It polls them all on the thread that polls it
//! (the interpreter has no threads of its own), and their effects run concurrently as host
//! futures: two branches' tool calls really overlap. No async runtime is needed; the future
//! runs under any executor (and [`block_on`] runs it on the standard library alone).
//!
//! - **Width.** A task holds a running slot on its frame's ledgers while it evaluates or
//!   awaits a host effect; a task waiting for its own branches is **parked** and gives its
//!   slot back, and takes one back ahead of other waiters when its branches are done. Waiting
//!   branches start in the order of their branch paths (program order, depth first), within
//!   each construct's own `limit:`. So nested fan-out never deadlocks on the width.
//! - **Admission.** Every tool and model call is admitted by reservation on its frame's
//!   ledgers before it starts (chapter 04); a model admission below the reply floor waits on
//!   the ledger's release waker (never blocking the thread), and a refusal raises
//!   `Budget { which }` in the branch.
//! - **Cancellation** reaches a branch at its next effect boundary or checkpoint. An `abort`
//!   call in flight is stopped, and the branch waits for the host's acknowledgement; a `drain`
//!   call is left running as an **outstanding drain**, re-parented to the nearest enclosing
//!   frame not itself being cancelled, and the branch unwinds at once. The **drain fence** then
//!   holds back that frame's close, every later `drain` call in the frame tree (except in
//!   `defer` blocks and `tools.command_stop`) and the end of the turn, until the drains finish
//!   within their declared bounds. Effects in `defer` blocks are not cancelled.
//! - **Deadlines.** A timeout scope's deadline is set when the scheduler first sees it, and
//!   its timer is the host's ([`Host::timer`]); every effect carries the earliest deadline of
//!   the scopes enclosing it, in its task and its ancestors, and of its frame's `wall` budget.
//! - **The tape** ([`crate::tape`]): in record mode every scheduling event is numbered on the
//!   turn sequence; in replay mode every decision (admissions, refusals, cancellation points,
//!   race winners, failing branches, timeouts) is taken from the tape instead of from timing.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use ratio_ir::Loc;

use crate::error::{Class as ErrClass, RtError};
use crate::fork::{Fence, ForkInfo, ForkKind, Joined, TimeoutScope};
use crate::frame::{
    Admission, Amounts, BudgetKind, FrameCtx, Ledgers, MIN_REPLY_TOKENS, ModelCall, NodeId,
    Reservation,
};
use crate::host::bridge::{self, Call};
use crate::host::{
    CancelClass, CancelOutcome, Countable, EffectCtx, EffectKey, Guard, Host, HostError,
    HostFuture, ToolClass,
};
use crate::machine::{CancelToken, EffectRequest, Machine, Outcome, Suspend};
use crate::tape::{Decision, Event, Tape};
use crate::value::Value;

/// How an evaluation ended, with what it spent.
#[derive(Clone, Debug)]
pub struct Finished {
    pub outcome: Outcome,
    /// The steps charged, over every branch.
    pub steps: u64,
    /// The most heap bytes live at once.
    pub heap_peak: u64,
}

type TaskId = usize;

/// What a host call answers, converted for the machine.
struct Answer {
    result: Result<Value, HostError>,
    /// What the call actually used, for its reservation's completion.
    actual: Amounts,
}

/// A host call in flight.
struct InFlight {
    fut: HostFuture<Answer>,
    req: EffectRequest,
    class: ToolClass,
    token: CancelToken,
    reservation: Option<(Ledgers, Reservation)>,
    /// Once its token has fired: the cancellation it ends with (0: the branch's; else a
    /// timeout scope's id).
    cancelling: Option<u32>,
    input_bearing: bool,
}

/// A `drain` call whose scope was cancelled, still running.
struct Drain {
    call: InFlight,
    owner: NodeId,
}

/// An effect not started yet: waiting for a model admission, or behind the fence.
struct Pending {
    req: EffectRequest,
    call: Call,
    class: ToolClass,
    /// The ledger epoch a waiting admission retries after.
    epoch: Option<(Ledgers, u64)>,
}

enum State {
    /// Holds a slot; runnable.
    Ready,
    /// Waiting for a slot: a branch not started yet, or a parked task whose construct is done.
    NeedSlot(Option<Joined>),
    Effect(Box<InFlight>),
    Waiting(Box<Pending>),
    Pool(crate::pool::Waiter),
    /// A frame close the fence holds.
    Fenced(NodeId),
    /// Waiting for its construct's branches.
    Parked,
    Done(Outcome),
}

/// A construct's branches, as they run.
struct ForkRun {
    info: ForkInfo,
    /// The construct's id on the tape: the frame and branch path it ran in, and its site.
    id: Arc<str>,
    children: Vec<Option<TaskId>>,
    next: u32,
    /// Branches started and not ended (for the construct's `limit:`).
    live: u32,
    results: Vec<Option<Result<Value, RtError>>>,
    /// The first failure by time (`All`), or a non-catchable one, and its index.
    failed: Option<(u32, RtError)>,
    winner: Option<(u32, Value)>,
    /// No further branch starts.
    closed: bool,
    steps: u64,
}

struct Task {
    machine: Option<Machine>,
    parent: Option<TaskId>,
    index: u32,
    /// The program-order key: the parent's, then `[site, occurrence, index]`.
    order: Vec<u32>,
    /// The branch's id on the tape (`frame path|branch path`).
    label: Arc<str>,
    /// The ledger node its slot is held on, while it holds one.
    slot: Option<NodeId>,
    slot_node: NodeId,
    state: State,
    /// Being cancelled as a whole.
    cancelled: bool,
    /// A timeout scope fired while the task was parked: delivered when its construct ends.
    scope_cancel: Option<u32>,
    /// The timeout scopes it has seen, with their deadlines and timers.
    scopes: Vec<(TimeoutScope, Instant, Option<HostFuture<()>>)>,
    /// The deadline inherited from the scopes enclosing its construct.
    inherited: Option<Instant>,
    fork: Option<ForkRun>,
    /// The steps of its finished construct's branches, added when it resumes.
    pending_steps: u64,
    /// In replay: the recorded point it is cancelled at.
    replay_cancel: Option<(u64, u64)>,
}

impl Task {
    fn new(machine: Option<Machine>, parent: Option<TaskId>, index: u32, node: NodeId) -> Task {
        Task {
            machine,
            parent,
            index,
            order: Vec::new(),
            label: Arc::from(""),
            slot: None,
            slot_node: node,
            state: State::NeedSlot(None),
            cancelled: false,
            scope_cancel: None,
            scopes: Vec::new(),
            inherited: None,
            fork: None,
            pending_steps: 0,
            replay_cancel: None,
        }
    }
}

/// The scheduler.
struct Scheduler {
    host: Arc<Guard>,
    tasks: Vec<Task>,
    /// Tasks waiting for a slot, by `(class, order)`: class 0 for a parked task resuming
    /// (ahead of every other waiter), 1 for a branch to start; then program order.
    queue: BTreeMap<(u8, Vec<u32>), TaskId>,
    fence: Arc<Fence>,
    drains: Vec<Drain>,
    cancel: CancelToken,
    cancel_seen: bool,
    tape: Option<Arc<dyn Tape>>,
    finished: Option<Finished>,
    started: Instant,
    yields: u64,
    /// A divergence that ends the evaluation at once.
    abort: Option<RtError>,
    /// The most tasks that held a slot at once.
    peak_running: usize,
}

/// An evaluation, as a future.
pub struct Evaluation {
    s: Scheduler,
}

impl std::fmt::Debug for Evaluation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluation")
            .field("tasks", &self.s.tasks.len())
            .field("yields", &self.s.yields)
            .finish_non_exhaustive()
    }
}

impl Evaluation {
    pub(crate) fn new(machine: Machine) -> Evaluation {
        Evaluation::with_host(machine, Arc::new(crate::host::NoHost))
    }

    /// An evaluation whose effects `host` answers, wrapped in the [`Guard`].
    pub(crate) fn with_host(mut machine: Machine, host: Arc<dyn Host>) -> Evaluation {
        let guard = Arc::new(Guard::new(host));
        let fence = Arc::new(Fence::default());
        machine.set_host(Arc::clone(&guard) as Arc<dyn Host>);
        machine.set_fence(Arc::clone(&fence));
        let cancel = machine.cancel_token().clone();
        let started = guard.now();
        let node = machine.frame().node;
        let label: Arc<str> = Arc::clone(&machine.frame().path);
        let mut root = Task::new(Some(machine), None, 0, node);
        root.label = label;
        let mut queue = BTreeMap::new();
        queue.insert((1, Vec::new()), 0);
        Evaluation {
            s: Scheduler {
                host: guard,
                tasks: vec![root],
                queue,
                fence,
                drains: Vec::new(),
                cancel,
                cancel_seen: false,
                tape: None,
                finished: None,
                started,
                yields: 0,
                abort: None,
                peak_running: 0,
            },
        }
    }

    /// Records or replays the turn on `tape` (a [`crate::tape::TapeHost`], which must also be
    /// the evaluation's host).
    #[must_use]
    pub fn with_tape(mut self, tape: Arc<dyn Tape>) -> Evaluation {
        let root = self.machine().frame().clone();
        tape.turn_start(self.s.host.seed(), &root);
        self.s.tape = Some(tape);
        self
    }

    /// How many times it has yielded at a checkpoint.
    #[must_use]
    pub fn yields(&self) -> u64 {
        self.s.yields
    }

    /// The entry's machine (branches have machines of their own).
    ///
    /// # Panics
    ///
    /// Never: the entry's machine is only taken while it runs, inside a poll.
    pub fn machine(&mut self) -> &mut Machine {
        self.s.tasks[0]
            .machine
            .as_mut()
            .expect("the entry's machine is present between polls")
    }

    /// Runs large pure built-ins on `session`'s pool ([`crate::Pool`]).
    #[must_use]
    pub fn offload_to(mut self, session: crate::PoolSession) -> Evaluation {
        self.machine().set_offload(Some(session));
        self
    }

    /// The most branches that held a running slot at once so far.
    #[must_use]
    pub fn peak_running(&self) -> usize {
        self.s.peak_running
    }
}

impl Future for Evaluation {
    type Output = Finished;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Finished> {
        self.s.poll(cx)
    }
}

/// Converts a host future's result into an [`Answer`].
fn answer<T: Send + 'static>(
    fut: HostFuture<Result<T, HostError>>,
    map: impl FnOnce(T) -> (Value, Amounts) + Send + 'static,
) -> HostFuture<Answer> {
    Box::pin(async move {
        match fut.await {
            Ok(t) => {
                let (result, actual) = map(t);
                Answer {
                    result: Ok(result),
                    actual,
                }
            }
            Err(e) => Answer {
                result: Err(e),
                actual: Amounts::default(),
            },
        }
    })
}

const TOOL_CALL: Amounts = Amounts {
    tokens: 0,
    cost: 0,
    steps: 0,
    model_calls: 0,
    tool_calls: 1,
};

impl Scheduler {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Finished> {
        loop {
            if let Some(f) = &self.finished {
                return Poll::Ready(f.clone());
            }
            if self.cancel.is_cancelled() && !self.cancel_seen {
                self.cancel_seen = true;
                self.cancel_task(0);
            }
            self.cancel.register(cx.waker());
            if self.abort.is_none()
                && let Some(msg) = self.tape.as_ref().and_then(|t| t.divergence())
            {
                self.abort = Some(self.fault(0, &msg));
            }
            let mut progressed = self.poll_waits(cx);
            progressed |= self.admit();
            let (ran, yielded) = self.run_ready(cx);
            progressed |= ran;
            if self.finish() {
                continue;
            }
            if yielded {
                self.yields += 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if !progressed {
                return Poll::Pending;
            }
        }
    }

    fn replaying(&self) -> bool {
        self.tape.as_ref().is_some_and(|t| t.replaying())
    }

    fn record(&self, ev: Event) {
        if let Some(t) = &self.tape
            && !t.replaying()
        {
            t.record(ev);
        }
    }

    // ---- waiting: effects, timers, pool jobs, admissions, the fence ----

    fn poll_waits(&mut self, cx: &mut Context<'_>) -> bool {
        let mut progressed = false;
        let mut i = 0;
        while i < self.drains.len() {
            if let Poll::Ready(a) = self.drains[i].call.fut.as_mut().poll(cx) {
                let d = self.drains.swap_remove(i);
                self.drain_done(&d, &a);
                progressed = true;
            } else {
                i += 1;
            }
        }
        for id in 0..self.tasks.len() {
            progressed |= self.poll_timers(id, cx);
            let state = std::mem::replace(&mut self.tasks[id].state, State::Ready);
            let state = match state {
                State::Effect(mut f) => match f.fut.as_mut().poll(cx) {
                    Poll::Ready(a) => {
                        self.effect_done(id, &f, a);
                        progressed = true;
                        continue;
                    }
                    Poll::Pending => State::Effect(f),
                },
                State::Pool(w) => {
                    w.register(cx.waker());
                    if w.is_ready() || self.cancel.is_cancelled() {
                        progressed = true;
                        State::Ready
                    } else {
                        State::Pool(w)
                    }
                }
                State::Waiting(p) => {
                    let ready = match &p.epoch {
                        Some((l, epoch)) => !l.register_waker(*epoch, cx.waker()),
                        None => self.fence.total() == 0,
                    };
                    if ready {
                        progressed = true;
                        let p = *p;
                        self.start_call(id, p.req, p.call, p.class, cx);
                        continue;
                    }
                    State::Waiting(p)
                }
                State::Fenced(node) => {
                    if self.fence.holds(node) {
                        State::Fenced(node)
                    } else {
                        progressed = true;
                        State::Ready
                    }
                }
                s => s,
            };
            self.tasks[id].state = state;
        }
        progressed
    }

    /// Polls a task's timeout timers; a fired one cancels its scope.
    fn poll_timers(&mut self, id: TaskId, cx: &mut Context<'_>) -> bool {
        let mut fired = Vec::new();
        for (scope, _, timer) in &mut self.tasks[id].scopes {
            if let Some(t) = timer
                && t.as_mut().poll(cx).is_ready()
            {
                *timer = None;
                fired.push(*scope);
            }
        }
        let any = !fired.is_empty();
        for scope in fired {
            self.fire_timeout(id, scope);
        }
        any
    }

    // ---- slots ----

    /// Starts or resumes waiting tasks, resuming ones first and then in branch-path order,
    /// while slots are free.
    fn admit(&mut self) -> bool {
        let mut progressed = false;
        let keys: Vec<(u8, Vec<u32>)> = self.queue.keys().cloned().collect();
        for key in keys {
            let Some(&id) = self.queue.get(&key) else {
                continue;
            };
            let Some(ledgers) = self.ledgers_of(id) else {
                continue;
            };
            let node = self.tasks[id].slot_node;
            if !ledgers.acquire_slot(node) {
                continue;
            }
            self.queue.remove(&key);
            self.tasks[id].slot = Some(node);
            progressed = true;
            let running = self.tasks.iter().filter(|t| t.slot.is_some()).count();
            self.peak_running = self.peak_running.max(running);
            let state = std::mem::replace(&mut self.tasks[id].state, State::Ready);
            if let State::NeedSlot(Some(joined)) = state {
                self.resume_parked(id, joined);
            } else if self.tasks[id].machine.is_none() {
                self.start_branch(id);
            }
        }
        progressed
    }

    fn ledgers_of(&self, id: TaskId) -> Option<Ledgers> {
        let t = &self.tasks[id];
        if let Some(m) = &t.machine {
            return Some(m.frame().ledgers.clone());
        }
        let p = t.parent?;
        self.tasks[p]
            .fork
            .as_ref()
            .map(|f| f.info.frame.ledgers.clone())
    }

    fn release_slot(&mut self, id: TaskId) {
        if let Some(node) = self.tasks[id].slot.take()
            && let Some(l) = self.ledgers_of(id)
        {
            l.release_slot(node);
        }
    }

    /// Creates the machine of a branch that has just taken its slot.
    fn start_branch(&mut self, id: TaskId) {
        let (parent, index) = (self.tasks[id].parent, self.tasks[id].index);
        let Some(p) = parent else { return };
        let child = self.tasks[p].machine.as_ref().and_then(|m| m.spawn(index));
        let Some(m) = child else {
            self.abort = Some(self.fault(p, "a branch could not be started"));
            return;
        };
        let label: Arc<str> = Arc::from(format!("{}|{}", m.frame().path, m.branch_path()));
        self.tasks[id].replay_cancel = self
            .tape
            .as_ref()
            .filter(|t| t.replaying())
            .and_then(|t| t.cancel_point(&label));
        let mut m = m;
        if let Some(t) = self.tape.as_ref().filter(|t| t.replaying()) {
            // A branch without a recorded total diverges when it ends (`check_steps`).
            if let Some(total) = t.step_total(&label) {
                m.cap_steps(total);
            }
        }
        self.tasks[id].label = label;
        self.tasks[id].machine = Some(m);
        if self.replay_due(id).is_some() {
            self.deliver_replay(id);
        }
    }

    // ---- running ----

    /// Runs each ready task for one slice. Returns whether anything ran and whether a task
    /// yielded at a checkpoint.
    fn run_ready(&mut self, cx: &mut Context<'_>) -> (bool, bool) {
        let (mut ran, mut yielded) = (false, false);
        for id in 0..self.tasks.len() {
            if self.abort.is_some() {
                break;
            }
            if !matches!(self.tasks[id].state, State::Ready) {
                continue;
            }
            let Some(mut m) = self.tasks[id].machine.take() else {
                continue;
            };
            ran = true;
            let s = m.run();
            self.tasks[id].machine = Some(m);
            self.sync_scopes(id);
            match s {
                Suspend::Yield => {
                    if self.replay_due(id).is_some() {
                        self.deliver_replay(id);
                    }
                    yielded = true;
                }
                Suspend::Effect(req) => self.effect(id, req, cx),
                Suspend::Wait(w) => {
                    if self.replay_due(id).is_some() {
                        self.deliver_replay(id);
                    } else {
                        self.tasks[id].state = State::Pool(w);
                    }
                }
                Suspend::Branches(info) => self.park(id, info),
                Suspend::Fence(node) => self.tasks[id].state = State::Fenced(node),
                Suspend::Done(outcome) => self.task_done(id, outcome),
            }
        }
        (ran, yielded)
    }

    /// Keeps the task's timeout scopes in step with its machine's: a new one gets its
    /// deadline and (live) its timer; a scope left is dropped.
    fn sync_scopes(&mut self, id: TaskId) {
        let Some(m) = &self.tasks[id].machine else {
            return;
        };
        let live = m.timeouts();
        let replaying = self.replaying();
        let now = self.host.now();
        let t = &mut self.tasks[id];
        t.scopes
            .retain(|(s, _, _)| live.iter().any(|l| l.id == s.id));
        for s in live {
            if t.scopes.iter().any(|(x, _, _)| x.id == s.id) {
                continue;
            }
            let d = Duration::from_micros(u64::try_from(s.micros).unwrap_or(0));
            let at = now + d;
            // Replay ignores deadlines: a timeout fires where the tape says.
            let timer = (!replaying).then(|| self.host.timer(at));
            t.scopes.push((s, at, timer));
        }
    }

    fn deadline(&self, id: TaskId) -> Option<Instant> {
        let t = &self.tasks[id];
        let own = t.scopes.iter().map(|(_, at, _)| *at).min();
        let wall = t.machine.as_ref().and_then(|m| {
            m.frame()
                .scope
                .budget
                .wall
                .map(|us| self.started + Duration::from_micros(us))
        });
        [own, t.inherited, wall].into_iter().flatten().min()
    }

    fn point(&self, id: TaskId) -> (u64, u64) {
        self.tasks[id]
            .machine
            .as_ref()
            .map_or((0, 0), |m| (m.effects(), m.steps()))
    }

    /// In replay: whether the task has reached a recorded cancellation point, its own
    /// (`Some(0)`) or one of its timeout scopes' (`Some(id)`).
    fn replay_due(&self, id: TaskId) -> Option<u32> {
        let tape = self.tape.as_ref().filter(|t| t.replaying())?;
        let t = &self.tasks[id];
        let point = self.point(id);
        if !t.cancelled && t.replay_cancel == Some(point) {
            return Some(0);
        }
        t.scopes
            .iter()
            .find(|(s, _, _)| tape.timeout_point(&t.label, s.id) == Some(point))
            .map(|(s, _, _)| s.id)
    }

    /// Delivers a recorded cancellation (replay): the task is at a suspension, its effect (if
    /// any) not started.
    fn deliver_replay(&mut self, id: TaskId) {
        match self.replay_due(id) {
            Some(0) => {
                self.tasks[id].cancelled = true;
                self.tasks[id].state = State::Ready;
                self.cancel_children(id);
                if let Some(m) = &mut self.tasks[id].machine {
                    m.cancel_branch(0);
                }
            }
            Some(scope) => {
                // The scope fired here: forget its point (scopes ids are not reused).
                self.tasks[id].scopes.retain(|(s, _, _)| s.id != scope);
                self.tasks[id].state = State::Ready;
                if let Some(m) = &mut self.tasks[id].machine {
                    m.cancel_branch(scope);
                }
            }
            None => {}
        }
    }

    // ---- effects ----

    fn effect(&mut self, id: TaskId, req: EffectRequest, cx: &mut Context<'_>) {
        self.tasks[id].state = State::Ready;
        if self.replay_due(id).is_some() {
            // In replay a call cancelled at this point never starts. If it had started when
            // recorded, its outcome is on the tape, verified against the entries when the
            // tape was loaded; if it had not (it waited behind the fence), there is none.
            self.deliver_replay(id);
            return;
        }
        let call = bridge::classify(&req);
        let class = bridge::class_of(&*self.host, &req.frame, &call);
        // A later drain-class call waits behind the fence before it starts (except in
        // `defer` blocks, and `tools.command_stop`).
        let exempt = req.in_defer || &*req.builtin == "tools.command_stop";
        if class.class == CancelClass::Drain
            && !exempt
            && matches!(call, Call::Tool(_) | Call::Op(_))
            && self.fence.total() > 0
        {
            self.tasks[id].state = State::Waiting(Box::new(Pending {
                req,
                call,
                class,
                epoch: None,
            }));
            return;
        }
        self.start_call(id, req, call, class, cx);
    }

    /// Admits and starts a host call.
    fn start_call(
        &mut self,
        id: TaskId,
        req: EffectRequest,
        call: Call,
        class: ToolClass,
        cx: &mut Context<'_>,
    ) {
        self.tasks[id].state = State::Ready;
        let key = req.key();
        let frame = req.frame.clone();
        let loc = req.loc;
        let input_bearing = call.input_bearing();
        // Output-only effects: checked by the guard, emitted (suppressed in replay), done.
        let call = match call {
            Call::Event(mut ev) => {
                let r = if let Some(t) = &self.tape {
                    let r = Guard::check_event(&mut ev);
                    if r.is_ok() {
                        t.output(&ev);
                    }
                    if t.replaying() {
                        r
                    } else {
                        self.host.emit_checked(&frame, ev)
                    }
                } else {
                    self.host.emit_checked(&frame, ev)
                };
                let r = r.map(|()| Value::unit()).map_err(|e| self.rt(id, e, loc));
                self.resume(id, r);
                return;
            }
            c => c,
        };
        // The guard first: a refused secret never reaches admission or the host.
        let mut call = match self.guard(&frame, call) {
            Ok(c) => c,
            Err(e) => {
                let e = self.rt(id, e, loc);
                self.resume(id, Err(e));
                return;
            }
        };
        let reservation = match self.admission(id, &key, &frame, &mut call) {
            Ok(Some(r)) => Some(r),
            Ok(None) => None,
            Err(Wait(epoch)) => {
                self.record(Event::Wait {
                    key: key.to_string(),
                });
                if frame.ledgers.register_waker(epoch, cx.waker()) {
                    self.tasks[id].state = State::Waiting(Box::new(Pending {
                        req,
                        call,
                        class,
                        epoch: Some((frame.ledgers.clone(), epoch)),
                    }));
                } else {
                    // Released meanwhile: try again at once.
                    self.start_call(id, req, call, class, cx);
                }
                return;
            }
            Err(Refusal(e)) => {
                self.resume(id, Err(e));
                return;
            }
        };
        let token = CancelToken::new();
        let ecx = EffectCtx {
            key,
            cancel: token.clone(),
            deadline: self.deadline(id),
        };
        // The guard has checked the call: its inner host answers it (in replay, the tape).
        let inner = Arc::clone(self.host.inner());
        let fut: HostFuture<Answer> = match call {
            Call::Tool(t) => answer(inner.call_tool(frame, ecx, t), |o| (o.value, TOOL_CALL)),
            Call::Model(m) => answer(inner.generate(frame, ecx, m), |r| {
                let v = Value::text(&r.text.value).with_labels(r.text.labels);
                let actual = Amounts {
                    tokens: r.usage.input_tokens.saturating_add(r.usage.output_tokens),
                    cost: r.usage.cost,
                    model_calls: 1,
                    ..Amounts::default()
                };
                (v, actual)
            }),
            Call::Approve(a) => answer(inner.approve(frame, ecx, a), |b| {
                (Value::bool(b), Amounts::default())
            }),
            Call::Op(o) => answer(inner.operate(frame, ecx, o), |v| (v, Amounts::default())),
            Call::Event(_) => return,
        };
        let ledgers = req.frame.ledgers.clone();
        let mut f = Box::new(InFlight {
            fut,
            req,
            class,
            token,
            reservation: reservation.map(|r| (ledgers, r)),
            cancelling: None,
            input_bearing,
        });
        // A first poll: an answer already there resumes the branch at once.
        match f.fut.as_mut().poll(cx) {
            Poll::Ready(a) => self.effect_done(id, &f, a),
            Poll::Pending => self.tasks[id].state = State::Effect(f),
        }
    }

    /// Admits a call by reservation (live), or takes the recorded decision (replay). A model
    /// request gets its effective `max_tokens`.
    fn admission(
        &self,
        id: TaskId,
        key: &EffectKey,
        frame: &FrameCtx,
        call: &mut Call,
    ) -> Result<Option<Reservation>, Blocked> {
        let loc = Loc::default();
        if let Some(t) = self.tape.as_ref().filter(|t| t.replaying()) {
            if matches!(call, Call::Approve(_) | Call::Op(_)) {
                return Ok(None);
            }
            return match t.admission(key) {
                Some(Decision::Admit(max)) => {
                    if let Call::Model(m) = call {
                        m.max_tokens = max;
                    }
                    Ok(None)
                }
                Some(Decision::Refuse(which)) => {
                    Err(Refusal(self.budget(id, which, frame.node, loc)))
                }
                None => Err(Refusal(self.diverged(
                    id,
                    &format!("the tape has no admission for `{key}`"),
                    loc,
                ))),
            };
        }
        match call {
            Call::Tool(_) => match frame.ledgers.admit_tool(frame.node) {
                Ok(r) => {
                    self.record(Event::admit(key, &r, None));
                    Ok(Some(r))
                }
                Err(r) => {
                    self.record(Event::refuse(key, r.which, None));
                    Err(Refusal(self.budget(id, r.which, r.node, loc)))
                }
            },
            Call::Model(m) => {
                let card = self
                    .host
                    .resolve_model(frame, &m.model)
                    .map_err(|e| Refusal(self.rt(id, e, loc)))?
                    .card;
                let input = self
                    .host
                    .count_tokens(frame, &Countable::Prompt(m.prompt.clone()));
                let mc = ModelCall {
                    input,
                    max_tokens: m.max_tokens,
                    card,
                    min_reply: MIN_REPLY_TOKENS,
                };
                match frame.ledgers.admit_model(frame.node, &mc) {
                    Admission::Admitted(r) => {
                        m.max_tokens = r.max_tokens;
                        self.record(Event::admit(key, &r, Some(mc)));
                        Ok(Some(r))
                    }
                    Admission::Wait { epoch, .. } => Err(Wait(epoch)),
                    Admission::Refused(r) => {
                        self.record(Event::refuse(key, r.which, Some(mc)));
                        Err(Refusal(self.budget(id, r.which, r.node, loc)))
                    }
                }
            }
            Call::Approve(_) | Call::Op(_) | Call::Event(_) => Ok(None),
        }
    }

    fn guard(&self, frame: &FrameCtx, call: Call) -> Result<Call, HostError> {
        Ok(match call {
            Call::Tool(mut t) => {
                self.host.check_tool(frame, &mut t)?;
                Call::Tool(t)
            }
            Call::Model(mut m) => {
                self.host.check_model(frame, &mut m)?;
                Call::Model(m)
            }
            Call::Op(mut o) => {
                Guard::check_operation(&mut o)?;
                Call::Op(o)
            }
            c => c,
        })
    }

    /// A host call answered.
    fn effect_done(&mut self, id: TaskId, f: &InFlight, a: Answer) {
        let is_tool = matches!(f.reservation, Some((_, ref r)) if r.held.tool_calls > 0);
        if let Some((l, r)) = &f.reservation {
            // A started tool call counts, whatever it answered.
            let actual = if is_tool { TOOL_CALL } else { a.actual };
            let _ = l.complete(r, actual);
            self.record(Event::Complete {
                key: f.req.key().to_string(),
                actual: actual.into(),
            });
        }
        let loc = f.req.loc;
        if let Some(scope) = f.cancelling {
            // The token fired and the host acknowledged: how the call ended is recorded.
            let outcome = match &a.result {
                Err(HostError::Cancelled(o)) => *o,
                _ => CancelOutcome::Aborted,
            };
            self.record(Event::Ended {
                key: f.req.key().to_string(),
                outcome: outcome.name().to_owned(),
                digest: None,
            });
            self.tasks[id].state = State::Ready;
            if let Some(m) = &mut self.tasks[id].machine {
                m.cancel_branch(scope);
            }
            return;
        }
        if f.input_bearing
            && let Some(t) = &self.tape
            && let Err(msg) = t.answered(&f.req, &a.result)
        {
            self.abort = Some(self.diverged(id, &msg, loc));
            return;
        }
        let r = a.result.map_err(|e| self.rt(id, e, loc));
        self.resume(id, r);
    }

    fn resume(&mut self, id: TaskId, r: Result<Value, RtError>) {
        self.tasks[id].state = State::Ready;
        if let Some(m) = &mut self.tasks[id].machine {
            m.resume_effect(r);
        }
    }

    fn machine_of(&self, id: TaskId) -> &Machine {
        self.tasks[id]
            .machine
            .as_ref()
            .or(self.tasks[0].machine.as_ref())
            .expect("the entry's machine is present")
    }

    fn rt(&self, id: TaskId, e: HostError, loc: Loc) -> RtError {
        self.machine_of(id).host_error(e, loc)
    }

    fn budget(&self, id: TaskId, which: BudgetKind, node: NodeId, loc: Loc) -> RtError {
        self.machine_of(id).budget_error(which, node, loc)
    }

    fn diverged(&self, id: TaskId, message: &str, loc: Loc) -> RtError {
        self.machine_of(id).replay_error(message, loc)
    }

    fn fault(&self, id: TaskId, message: &str) -> RtError {
        self.diverged(id, message, Loc::default())
    }

    // ---- constructs ----

    fn park(&mut self, id: TaskId, info: ForkInfo) {
        let fork_id: Arc<str> = Arc::from(format!(
            "{}|c{}#{}",
            self.tasks[id].label, info.site, info.occurrence
        ));
        let count = info.count as usize;
        self.release_slot(id);
        self.tasks[id].state = State::Parked;
        // A timeout recorded at the point it parked fires once the construct ends.
        if let Some(scope) = self.replay_due(id).filter(|s| *s != 0) {
            self.tasks[id].scopes.retain(|(s, _, _)| s.id != scope);
            self.tasks[id].scope_cancel = Some(scope);
        }
        self.tasks[id].fork = Some(ForkRun {
            info,
            id: fork_id,
            children: vec![None; count],
            next: 0,
            live: 0,
            results: (0..count).map(|_| None).collect(),
            failed: None,
            winner: None,
            closed: false,
            steps: 0,
        });
        self.queue_next(id);
    }

    /// Queues the next branches of task `p`'s construct, as many as may start.
    fn queue_next(&mut self, p: TaskId) {
        let inherited = self.deadline(p);
        loop {
            let Some(f) = &mut self.tasks[p].fork else {
                return;
            };
            if f.closed || f.next >= f.info.count || f.info.limit.is_some_and(|l| f.live >= l) {
                return;
            }
            let idx = f.next;
            f.next += 1;
            let skipped = self
                .tape
                .as_ref()
                .filter(|t| t.replaying())
                .is_some_and(|t| t.skipped(&f.id, idx));
            let (site, occ, node) = (f.info.site, f.info.occurrence, f.info.frame.node);
            let construct = Arc::clone(&f.id);
            if !skipped {
                f.live += 1;
            }
            let mut order = self.tasks[p].order.clone();
            order.extend([site, occ, idx]);
            let id = self.tasks.len();
            let mut t = Task::new(None, Some(p), idx, node);
            t.order.clone_from(&order);
            t.inherited = inherited;
            self.tasks.push(t);
            if let Some(f) = &mut self.tasks[p].fork {
                f.children[idx as usize] = Some(id);
            }
            if skipped {
                // In record it never started: it does not start in replay either.
                self.tasks[id].cancelled = true;
                let e = self.cancelled_error(p);
                self.tasks[id].state = State::Done(Outcome::Raised(e.clone()));
                if let Some(f) = &mut self.tasks[p].fork {
                    f.results[idx as usize] = Some(Err(e));
                }
                continue;
            }
            let _ = construct;
            self.queue.insert((1, order), id);
        }
    }

    fn cancelled_error(&self, id: TaskId) -> RtError {
        let mut e = self.fault(id, "the branch was cancelled before it started");
        e.class = ErrClass::Cancelled;
        e
    }

    fn task_done(&mut self, id: TaskId, outcome: Outcome) {
        self.release_slot(id);
        let steps = self.tasks[id].machine.as_ref().map_or(0, Machine::steps);
        if self.replaying() && self.tasks[id].replay_cancel.is_some() && !self.tasks[id].cancelled {
            // A branch recorded as cancelled must reach its cancellation point.
            let msg = format!(
                "the branch `{}` ended before its recorded cancellation point",
                self.tasks[id].label
            );
            self.abort = Some(self.diverged(id, &msg, Loc::default()));
        }
        let Some(p) = self.tasks[id].parent else {
            self.tasks[id].state = State::Done(outcome);
            return;
        };
        self.record(Event::Steps {
            branch: self.tasks[id].label.to_string(),
            steps,
        });
        if let Some(t) = self.tape.as_ref().filter(|t| t.replaying())
            && let Err(msg) = t.check_steps(&self.tasks[id].label, steps)
        {
            self.abort = Some(self.diverged(id, &msg, Loc::default()));
        }
        // Its frames are closed and its values are in the outcome: free the machine.
        self.tasks[id].machine = None;
        self.tasks[id].scopes.clear();
        self.tasks[id].state = State::Done(outcome.clone());
        let index = self.tasks[id].index;
        let was_cancelled = self.tasks[id].cancelled;
        let replaying = self.replaying();
        let Some(f) = &mut self.tasks[p].fork else {
            return;
        };
        f.live = f.live.saturating_sub(1);
        f.steps = f.steps.saturating_add(steps);
        let r = match outcome {
            Outcome::Returned(v) => Ok(v),
            Outcome::Raised(e) => Err(e),
        };
        let mut cancel_rest = false;
        match (&r, f.info.kind) {
            // A program defect or a divergence ends every construct around it.
            (Err(e), _) if matches!(e.class, ErrClass::Fatal | ErrClass::Replay) => {
                if f.failed.is_none() {
                    f.failed = Some((index, e.clone()));
                }
                cancel_rest = true;
            }
            (Err(e), ForkKind::All) if !was_cancelled => {
                if f.failed.is_none() {
                    f.failed = Some((index, e.clone()));
                    cancel_rest = !replaying;
                }
            }
            (Ok(v), ForkKind::Race) if !was_cancelled && f.winner.is_none() => {
                f.winner = Some((index, v.clone()));
                cancel_rest = !replaying;
            }
            (Err(e), ForkKind::Race) if !was_cancelled && e.class != ErrClass::Catchable => {
                if f.failed.is_none() {
                    f.failed = Some((index, e.clone()));
                }
                cancel_rest = true;
            }
            _ => {}
        }
        f.results[index as usize] = Some(r);
        if cancel_rest {
            f.closed = true;
            self.cancel_children(p);
        } else {
            self.queue_next(p);
        }
        self.maybe_join(p);
    }

    fn cancel_children(&mut self, p: TaskId) {
        let kids: Vec<TaskId> = self.tasks[p]
            .fork
            .as_mut()
            .map(|f| {
                f.closed = true;
                f.children.iter().flatten().copied().collect()
            })
            .unwrap_or_default();
        if self.replaying() {
            // In replay, branches are cancelled at their recorded points only, except that
            // one never started does not start.
            return;
        }
        let construct = self.tasks[p].fork.as_ref().map(|f| Arc::clone(&f.id));
        if let (Some(c), Some(f)) = (construct, &self.tasks[p].fork) {
            for i in f.next..f.info.count {
                self.record(Event::Skipped {
                    construct: c.to_string(),
                    index: i,
                });
            }
        }
        for k in kids {
            self.cancel_task(k);
        }
    }

    /// When every started branch of `p`'s construct has ended, answers it.
    fn maybe_join(&mut self, p: TaskId) {
        let Some(f) = &self.tasks[p].fork else {
            return;
        };
        let started_all = f.closed || f.next >= f.info.count;
        let all_done = f
            .children
            .iter()
            .flatten()
            .all(|&c| matches!(self.tasks[c].state, State::Done(_)));
        if !started_all || !all_done {
            return;
        }
        let Some(mut f) = self.tasks[p].fork.take() else {
            return;
        };
        let tape = self.tape.clone().filter(|t| t.replaying());
        let joined = match f.info.kind {
            ForkKind::All => {
                let fatal = f.failed.take();
                let failed = match (&tape, fatal) {
                    (_, Some((i, e))) if e.class != ErrClass::Catchable => Some((i, e)),
                    // In replay the failing branch is the recorded one.
                    (Some(t), _) => t.first_failure(&f.id).and_then(|i| {
                        f.results
                            .get(i as usize)
                            .and_then(|r| r.as_ref().and_then(|r| r.as_ref().err().cloned()))
                            .map(|e| (i, e))
                    }),
                    (None, failed) => failed,
                };
                match failed {
                    Some((i, e)) => {
                        self.record(Event::ParFailed {
                            construct: f.id.to_string(),
                            index: i,
                        });
                        Joined::Failed(e)
                    }
                    None => Joined::Values(
                        f.results
                            .into_iter()
                            .map(|r| r.and_then(Result::ok).unwrap_or_default())
                            .collect(),
                    ),
                }
            }
            ForkKind::Settled => Joined::Settled(
                f.results
                    .into_iter()
                    .map(|r| r.unwrap_or_else(|| Ok(Value::unit())))
                    .collect(),
            ),
            ForkKind::Race => {
                let winner = match &tape {
                    Some(t) => t.winner(&f.id).and_then(|i| {
                        f.results
                            .get(i as usize)
                            .and_then(|r| r.as_ref().and_then(|r| r.as_ref().ok().cloned()))
                            .map(|v| (i, v))
                    }),
                    None => f.winner.take(),
                };
                if let Some((_, e)) = f.failed.take() {
                    Joined::Failed(e)
                } else if let Some((i, v)) = winner {
                    self.record(Event::RaceWon {
                        construct: f.id.to_string(),
                        index: i,
                    });
                    Joined::Won(v)
                } else {
                    // Every branch failed: the failure of the first in written order.
                    let first = f.results.into_iter().flatten().find_map(Result::err);
                    Joined::Failed(first.unwrap_or_else(|| self.cancelled_error(p)))
                }
            }
        };
        self.tasks[p].pending_steps = f.steps;
        // Re-queued ahead of every branch waiting to start.
        self.tasks[p].state = State::NeedSlot(Some(joined));
        let order = self.tasks[p].order.clone();
        self.queue.insert((0, order), p);
    }

    fn resume_parked(&mut self, id: TaskId, joined: Joined) {
        let steps = std::mem::take(&mut self.tasks[id].pending_steps);
        let cancel = self.tasks[id].cancelled;
        let scope = self.tasks[id].scope_cancel.take();
        self.tasks[id].state = State::Ready;
        if let Some(m) = &mut self.tasks[id].machine {
            m.join(joined, steps);
            if cancel {
                m.cancel_branch(0);
            } else if let Some(s) = scope {
                m.cancel_branch(s);
            }
        }
    }

    // ---- cancellation ----

    /// Cancels task `id` and everything below it (live).
    fn cancel_task(&mut self, id: TaskId) {
        if self.tasks[id].cancelled || matches!(self.tasks[id].state, State::Done(_)) {
            return;
        }
        self.tasks[id].cancelled = true;
        // Outstanding drains owned inside it move up.
        self.reparent_from(id);
        if self.tasks[id].machine.is_none() {
            // Queued, never started: it never runs.
            let key = self
                .queue
                .iter()
                .find(|(_, v)| **v == id)
                .map(|(k, _)| k.clone());
            if let Some(k) = key {
                self.queue.remove(&k);
            }
            let p = self.tasks[id].parent.unwrap_or(0);
            let e = self.cancelled_error(p);
            if let Some(f) = &self.tasks[p].fork {
                let c = f.id.to_string();
                let index = self.tasks[id].index;
                self.record(Event::Skipped {
                    construct: c,
                    index,
                });
            }
            self.not_started(id, e);
            return;
        }
        let (effects, steps) = self.point(id);
        self.record(Event::CancelPoint {
            branch: self.tasks[id].label.to_string(),
            effects,
            steps,
        });
        let state = std::mem::replace(&mut self.tasks[id].state, State::Ready);
        match state {
            State::Effect(f) => self.cancel_effect(id, f, 0),
            State::Parked => {
                self.tasks[id].state = State::Parked;
                self.cancel_children(id);
                self.maybe_join(id);
            }
            // A parked task whose construct is done: it is cancelled when it resumes.
            State::NeedSlot(j) => self.tasks[id].state = State::NeedSlot(j),
            State::Done(o) => self.tasks[id].state = State::Done(o),
            State::Ready | State::Waiting(_) | State::Pool(_) | State::Fenced(_) => {
                if let Some(m) = &mut self.tasks[id].machine {
                    m.cancel_branch(0);
                }
            }
        }
    }

    /// A branch cancelled before it started ends without running.
    fn not_started(&mut self, id: TaskId, e: RtError) {
        let Some(p) = self.tasks[id].parent else {
            return;
        };
        self.tasks[id].state = State::Done(Outcome::Raised(e.clone()));
        let index = self.tasks[id].index;
        if let Some(f) = &mut self.tasks[p].fork {
            f.live = f.live.saturating_sub(1);
            f.results[index as usize] = Some(Err(e));
        }
        self.maybe_join(p);
    }

    /// Ends an effect in flight by its class (`scope` 0: the branch's cancellation; else a
    /// timeout scope's).
    fn cancel_effect(&mut self, id: TaskId, mut f: Box<InFlight>, scope: u32) {
        if f.req.in_defer {
            // Effects in `defer` blocks are exempt from cancellation.
            self.tasks[id].state = State::Effect(f);
            return;
        }
        f.token.cancel();
        match f.class.class {
            CancelClass::Abort => {
                // The scope returns once the host acknowledges.
                f.cancelling = Some(scope);
                self.tasks[id].state = State::Effect(f);
            }
            CancelClass::Drain => {
                let owner = self.owner(id, scope);
                let path = self.owner_path(owner);
                self.fence.hold(owner.1);
                self.record(Event::Reparent {
                    key: f.req.key().to_string(),
                    owner: path.to_string(),
                });
                self.drains.push(Drain {
                    call: *f,
                    owner: owner.1,
                });
                self.tasks[id].state = State::Ready;
                if let Some(m) = &mut self.tasks[id].machine {
                    m.cancel_branch(scope);
                }
            }
        }
    }

    /// The frame an outstanding drain of task `id` belongs to: for a timeout scope, the frame
    /// the scope was entered in; for a cancelled branch, the frame of the nearest enclosing
    /// construct whose task is not itself being cancelled; else the turn's.
    fn owner(&self, id: TaskId, scope: u32) -> (TaskId, NodeId) {
        if scope != 0
            && let Some((s, _, _)) = self.tasks[id].scopes.iter().find(|(s, _, _)| s.id == scope)
        {
            return (id, s.frame);
        }
        let mut t = id;
        while let Some(p) = self.tasks[t].parent {
            if !self.tasks[p].cancelled {
                let node = self.tasks[p]
                    .fork
                    .as_ref()
                    .map_or(self.tasks[p].slot_node, |f| f.info.frame.node);
                return (p, node);
            }
            t = p;
        }
        (0, self.turn_node())
    }

    fn turn_node(&self) -> NodeId {
        self.tasks[0]
            .machine
            .as_ref()
            .map_or(self.tasks[0].slot_node, Machine::session_node)
    }

    fn owner_path(&self, owner: (TaskId, NodeId)) -> Arc<str> {
        self.tasks[owner.0]
            .machine
            .as_ref()
            .and_then(|m| m.path_of(owner.1))
            .unwrap_or_else(|| Arc::from("turn"))
    }

    /// Moves the outstanding drains owned by frames inside task `id` to its own owner.
    fn reparent_from(&mut self, id: TaskId) {
        if self.drains.is_empty() {
            return;
        }
        let inside: Vec<NodeId> = self.frames_inside(id);
        let moving: Vec<usize> = (0..self.drains.len())
            .filter(|&i| inside.contains(&self.drains[i].owner))
            .collect();
        if moving.is_empty() {
            return;
        }
        let owner = self.owner(id, 0);
        let path = self.owner_path(owner);
        for i in moving {
            let old = self.drains[i].owner;
            self.fence.release(old);
            self.fence.hold(owner.1);
            self.drains[i].owner = owner.1;
            let key = self.drains[i].call.req.key().to_string();
            self.record(Event::Reparent {
                key,
                owner: path.to_string(),
            });
        }
    }

    /// The ledger nodes of the frames open in task `id` and its descendants, below the frame
    /// it started in.
    fn frames_inside(&self, id: TaskId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(t) = stack.pop() {
            if let Some(m) = &self.tasks[t].machine {
                out.extend(m.open_nodes());
            }
            if let Some(f) = &self.tasks[t].fork {
                stack.extend(f.children.iter().flatten().copied());
            }
        }
        let start = self.tasks[id].slot_node;
        out.retain(|n| *n != start);
        out
    }

    /// An outstanding drain finished: its reservation is completed with the actual charge.
    fn drain_done(&mut self, d: &Drain, a: &Answer) {
        self.fence.release(d.owner);
        let tool = d
            .call
            .reservation
            .as_ref()
            .is_some_and(|(_, r)| r.held.tool_calls > 0);
        if let Some((l, r)) = &d.call.reservation {
            let actual = if tool { TOOL_CALL } else { a.actual };
            let _ = l.complete(r, actual);
            self.record(Event::Complete {
                key: d.call.req.key().to_string(),
                actual: actual.into(),
            });
        }
        let outcome = match &a.result {
            Err(HostError::Cancelled(o)) => *o,
            _ => CancelOutcome::Drained,
        };
        // A drained call's discarded answer (a value or a failure) is on the tape, so replay
        // can check the outcome against it.
        if outcome == CancelOutcome::Drained
            && let Some(t) = &self.tape
        {
            let _ = t.answered(&d.call.req, &a.result);
        }
        let digest = match &a.result {
            Ok(v) => Some(crate::tape::value_digest(v).hex()),
            Err(_) => None,
        };
        self.record(Event::Ended {
            key: d.call.req.key().to_string(),
            outcome: outcome.name().to_owned(),
            digest,
        });
    }

    /// A timeout scope of task `id` fired (live).
    fn fire_timeout(&mut self, id: TaskId, scope: TimeoutScope) {
        if matches!(self.tasks[id].state, State::Done(_)) {
            return;
        }
        let (effects, steps) = self.point(id);
        self.record(Event::TimeoutFired {
            branch: self.tasks[id].label.to_string(),
            scope: scope.id,
            effects,
            steps,
        });
        let state = std::mem::replace(&mut self.tasks[id].state, State::Ready);
        match state {
            State::Effect(f) => self.cancel_effect(id, f, scope.id),
            State::Parked => {
                self.tasks[id].state = State::Parked;
                self.tasks[id].scope_cancel = Some(scope.id);
                self.cancel_children(id);
                self.maybe_join(id);
            }
            State::NeedSlot(j) => {
                self.tasks[id].state = State::NeedSlot(j);
                self.tasks[id].scope_cancel = Some(scope.id);
            }
            State::Done(o) => self.tasks[id].state = State::Done(o),
            State::Ready | State::Waiting(_) | State::Pool(_) | State::Fenced(_) => {
                if let Some(m) = &mut self.tasks[id].machine {
                    m.cancel_branch(scope.id);
                }
            }
        }
    }

    // ---- the end ----

    /// When the entry has ended and no drain is outstanding (the turn's fence), the
    /// evaluation is finished.
    fn finish(&mut self) -> bool {
        if let Some(e) = self.abort.take() {
            // A divergence ends everything at once.
            self.finished = Some(self.finished_with(Outcome::Raised(e)));
            return true;
        }
        let State::Done(o) = &self.tasks[0].state else {
            return false;
        };
        if !self.drains.is_empty() {
            return false;
        }
        let mut outcome = o.clone();
        if let Some(t) = &self.tape
            && let Err(msg) = t.turn_end(&outcome, self.machine_of(0).prepared())
        {
            outcome = Outcome::Raised(self.diverged(0, &msg, Loc::default()));
        }
        self.finished = Some(self.finished_with(outcome));
        true
    }

    fn finished_with(&self, outcome: Outcome) -> Finished {
        let m = self.machine_of(0);
        Finished {
            outcome,
            steps: m.steps(),
            heap_peak: m.heap().ledger().map_or(0, crate::heap::HeapLedger::peak),
        }
    }
}

/// Why a call could not be admitted now.
enum Blocked {
    /// Below the reply floor while reservations are held: retry after this release epoch.
    Wait(u64),
    /// Refused (or diverged, in replay).
    Refusal(RtError),
}

use Blocked::{Refusal, Wait};

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs a future to completion on the current thread (standard library only).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}
