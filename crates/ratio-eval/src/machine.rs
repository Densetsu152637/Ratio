//! The evaluator: a tree walker over the prepared form with an explicit continuation stack.
//!
//! The walk keeps its own stack of continuation frames ([`K`]) on the heap instead of
//! recursing in Rust, so a program's nesting, recursion and callbacks never touch the Rust
//! stack, whoever polls the evaluation and on whatever stack. That also makes it a resumable
//! state machine: [`Machine::run`] returns at every 1,000-step checkpoint ([`Suspend::Yield`])
//! and at every effect ([`Suspend::Effect`]), and continues where it stopped. The async
//! driver ([`crate::Evaluation`]) yields to the runtime at the first and, from stage 11,
//! awaits the host at the second: effects plug in there without changing the walk.
//!
//! **Steps** (chapter 07) are charged before the work they pay for, and depend only on the
//! program and its data, never on representation state: one step for every IR expression,
//! statement and pattern node the walk enters, one for every loop iteration, a built-in's
//! cost-class charge `1 + ⌈w / 64⌉` (chapter 06, `data`), text building (`+` on text,
//! interpolation) `1 + ⌈m / 64⌉` for a result of `m` characters, a formatted interpolation
//! part `1 + ⌈(n + m) / 64⌉`, and comparisons and membership tests of containers or text
//! `⌈n / 64⌉` over the operands' weights. **Call depth** counts nested function and closure
//! calls (callbacks included). The **heap** is the live-value ledger of [`crate::heap`].
//! Exhausting any of them raises `Budget`, which no code inside the turn can catch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ratio_ir::{ArgFrom, AssignOp, BinOp, Loc, UnOp};

use crate::builtins::{BUILTINS, Class, Imp, Resume, Task, TaskStep, output_size, pre_work};
use crate::error::{Class as ErrClass, Kind, RtError, raise};
use crate::frame::stack::{FrameStack, Session};
use crate::frame::{FrameCtx, FrameKind, Request, SeesItem, Settings};
use crate::heap::{Charge, Heap};
use crate::labels::Labels;
use crate::ops::{self, steps_for};
use crate::plist::{Cursor, List};
use crate::pmap::{Map, MapCursor};
use crate::prepare::{
    FuncKind, NodeId, PCallee, PCatch, PItemKind, PKind, PPart, PPat, PPrompt, PStep, PStmt, PatId,
    Prepared,
};
use crate::value::{Fault, Repr, Section, Value};

/// The interpreter's limits (chapter 07, "Resource limits").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Interpreter steps (default 5,000,000; at most 100,000,000).
    pub steps: u64,
    /// Live heap bytes (default 64 MiB; at most 512 MiB).
    pub heap: u64,
    /// Nested function and closure calls (default 512; at most 4,096).
    pub call_depth: u32,
}

impl Limits {
    pub const MAX_STEPS: u64 = 100_000_000;
    pub const MAX_HEAP: u64 = 512 * 1024 * 1024;
    pub const MAX_CALL_DEPTH: u32 = 4096;

    /// The limits clamped to the maxima.
    #[must_use]
    pub fn clamped(self) -> Limits {
        Limits {
            steps: self.steps.min(Limits::MAX_STEPS),
            heap: self.heap.min(Limits::MAX_HEAP),
            call_depth: self.call_depth.min(Limits::MAX_CALL_DEPTH),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            steps: 5_000_000,
            heap: 64 * 1024 * 1024,
            call_depth: 512,
        }
    }
}

/// The steps between checkpoints, where the evaluation yields and checks cancellation.
pub const CHECKPOINT: u64 = 1_000;

/// A cancellation flag a host sets; the evaluation sees it at its next checkpoint.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// How an evaluation ended.
#[derive(Clone, Debug)]
pub enum Outcome {
    Returned(Value),
    Raised(RtError),
}

/// An effect the program asked for: a built-in with effects, its arguments in parameter order
/// (`None` for a default), and its site.
#[derive(Clone, Debug)]
pub struct EffectRequest {
    pub builtin: Arc<str>,
    pub args: Vec<Option<Value>>,
    /// The package's IR hash and the node index: the effect's site id.
    pub site: ([u8; 32], u32),
    pub loc: Loc,
    /// The frame the effect is made in: its path, its scope, its ledgers.
    pub frame: FrameCtx,
    /// The branch path within the frame (`c12#0.b2/c30#1.b0`; empty outside `par` and
    /// `race`).
    pub branch: String,
}

/// Why [`Machine::run`] returned.
#[derive(Debug)]
pub enum Suspend {
    /// A checkpoint: call `run` again to continue.
    Yield,
    /// An effect: answer it with [`Machine::resume_effect`], then call `run` again.
    Effect(EffectRequest),
    /// The evaluation ended.
    Done(Outcome),
}

/// What built-ins and operators see of the machine.
pub(crate) struct Cx<'a> {
    pub p: &'a Prepared,
    pub heap: &'a Heap,
    pub loc: Loc,
    pub type_args: &'a [u32],
    /// The evaluation's cancellation flag, for a long built-in to check as it works.
    pub cancel: &'a CancelToken,
    /// Steps for work a built-in did before failing (a rendering refused as too long): the
    /// machine debits them whether the built-in returns or raises.
    pub debit: u64,
}

impl Cx<'_> {
    pub(crate) fn fault(&self, f: Fault, labels: Labels) -> RtError {
        fault(self.p, f, labels, self.loc)
    }
}

fn fault(p: &Prepared, f: Fault, labels: Labels, loc: Loc) -> RtError {
    match f {
        Fault::Heap => raise(
            p,
            Kind::Budget("heap"),
            "the heap limit was reached",
            Labels::NONE,
            loc,
        ),
        Fault::Depth => raise(
            p,
            Kind::Invalid,
            "values nested more than 256 deep",
            labels,
            loc,
        ),
        Fault::TooLarge(what) => raise(
            p,
            Kind::Invalid,
            &format!("{what} is over the limit"),
            labels,
            loc,
        ),
        Fault::Invalid(m) => raise(p, Kind::Invalid, &m, labels, loc),
    }
}

#[derive(Clone, Debug, Default)]
struct Slot {
    value: Value,
    /// The branch level the slot was bound at (for the branch restriction on `var`s).
    level: u32,
}

impl Default for Value {
    fn default() -> Self {
        Value::unit()
    }
}

#[derive(Debug)]
struct Env {
    slots: Vec<Slot>,
}

#[derive(Debug)]
enum Signal {
    Raise(RtError),
    Break(Value),
    Continue,
    Return(Value),
}

#[derive(Debug)]
enum Exit {
    Value(Value),
    Signal(Signal),
}

#[derive(Debug)]
enum Iter {
    List(Cursor, Labels),
    Map(MapCursor, Labels),
    Range {
        next: i128,
        end: i128,
        labels: Labels,
    },
    Repeat(i64),
    Forever,
    While,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Cond,
    Body,
}

/// The values a frame has gathered so far (a node's children, `par` results, a section's
/// values, an assignment's indexes), charged to the heap a slot each as they arrive and given
/// back when the frame is done: a program's own nesting cannot hold memory the ledger does not
/// see.
#[derive(Debug)]
struct Held {
    vals: Vec<Value>,
    charge: Charge,
}

impl Held {
    fn new() -> Held {
        Held {
            vals: Vec::new(),
            charge: Charge::none(),
        }
    }

    fn push(&mut self, heap: &Heap, v: Value) -> Result<(), Fault> {
        self.charge.grow(heap, crate::heap::cost::SLOT)?;
        self.vals.push(v);
        Ok(())
    }

    fn len(&self) -> usize {
        self.vals.len()
    }

    fn into_vals(self) -> Vec<Value> {
        self.vals
    }
}

/// A continuation frame: what to do with the value the walk returns next.
#[derive(Debug)]
enum K {
    /// Gathering a node's children (see [`Machine::kid`]), then [`Machine::finish`].
    Collect {
        node: NodeId,
        next: u32,
        vals: Held,
    },
    Block {
        node: NodeId,
        next: u32,
        defers: Vec<NodeId>,
    },
    Let(PatId),
    Var(u32),
    Assign {
        block: NodeId,
        stmt: u32,
        next: u32,
        vals: Held,
    },
    Defers {
        pending: Vec<NodeId>,
        exit: Exit,
    },
    Discard,
    If(NodeId, Option<NodeId>),
    IfLet(NodeId),
    And(NodeId),
    Or(NodeId),
    Coalesce(NodeId),
    AddLabels(Labels),
    Opt(u32, NodeId),
    Match(NodeId),
    Guard {
        node: NodeId,
        value: Value,
        arm: u32,
        catch: bool,
    },
    LoopInit(NodeId),
    Loop {
        node: NodeId,
        iter: Iter,
        phase: Phase,
    },
    CallRet,
    Entry {
        func: u32,
        next: u32,
        args: Vec<Option<Value>>,
    },
    Task {
        task: Box<dyn Task>,
        class: Class,
        n: u64,
        loc: Loc,
    },
    Try(NodeId),
    Par {
        node: NodeId,
        next: u32,
        vals: Held,
    },
    Race {
        node: NodeId,
        next: u32,
        first: Option<RtError>,
    },
    Const(u32),
    PromptRoot,
    PromptRun {
        list: u32,
        idx: u32,
    },
    Section {
        list: u32,
        idx: u32,
        vals: Held,
    },
    PromptIf(u32, u32),
    PromptForInit {
        list: u32,
        idx: u32,
    },
    PromptFor {
        pat: PatId,
        body: u32,
        iter: Iter,
    },
    Effect,
    /// A harness call between binding its parameters and opening its frame (evaluating its
    /// declared configuration, when it has one).
    Open(Box<Opening>),
    /// The boundary of an open frame: closing it on every exit.
    FrameClose,
    /// A `with` block's configuration is being evaluated.
    WithCfg(NodeId),
    /// A call's overrides are being evaluated (after its arguments).
    Override {
        node: NodeId,
        f: Option<Value>,
        args: Vec<Option<Value>>,
    },
}

/// What opening a harness frame needs, kept from the call.
#[derive(Debug)]
struct Opening {
    func: u32,
    kind: FrameKind,
    loc: Loc,
    site: u32,
    overrides: Option<Value>,
    /// Whether any argument is labelled (refused unless the frame sees `secrets`).
    labelled: bool,
}

#[derive(Debug)]
enum Mode {
    Eval(NodeId),
    Ret(Value),
    Unwind(Signal),
    Effect(EffectRequest),
    Done,
}

/// The evaluator's state for one evaluation.
#[derive(Debug)]
pub struct Machine {
    p: Arc<Prepared>,
    ctl: Vec<K>,
    envs: Vec<Env>,
    mode: Mode,
    consts: Vec<Option<Value>>,
    prompts: Vec<Vec<Section>>,
    heap: Heap,
    limits: Limits,
    steps: u64,
    next_checkpoint: u64,
    depth: u32,
    level: u32,
    yield_due: bool,
    cancel: CancelToken,
    outcome: Option<Outcome>,
    /// The effect being awaited.
    pending: Option<EffectRequest>,
    /// The open frames (stage 10).
    frames: FrameStack,
    /// A call's overrides, for the harness it is about to enter.
    pending_override: Option<Value>,
    /// The site of the call being made (for frame paths).
    site_hint: u32,
}

/// Why an evaluation could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The entry is not a function, prompt or harness.
    NotCallable,
    /// The arguments do not fit the entry's parameters.
    Arguments,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StartError::NotCallable => "the entry is not a function the core interpreter can run",
            StartError::Arguments => "the arguments do not fit the entry's parameters",
        })
    }
}

impl std::error::Error for StartError {}

impl Machine {
    /// An evaluation of `entry` with `args`.
    ///
    /// # Errors
    ///
    /// When `entry` is not a function or prompt, or takes more arguments than given and has no
    /// defaults for the rest.
    pub fn new(
        p: Arc<Prepared>,
        entry: crate::prepare::Entry,
        args: Vec<Value>,
        limits: Limits,
        cancel: CancelToken,
    ) -> Result<Machine, StartError> {
        Machine::with_session(p, entry, args, limits, cancel, &Session::default())
    }

    /// An evaluation of `entry` with `args` in a session the host describes: its scope (what
    /// every frame is attenuated from) and the root of its frame paths. A harness entry opens
    /// its own frame from the session's scope, with its declared configuration.
    ///
    /// # Errors
    ///
    /// As [`Machine::new`].
    pub fn with_session(
        p: Arc<Prepared>,
        entry: crate::prepare::Entry,
        args: Vec<Value>,
        limits: Limits,
        cancel: CancelToken,
        session: &Session,
    ) -> Result<Machine, StartError> {
        let limits = limits.clamped();
        let item = p
            .items
            .get(entry.0 as usize)
            .ok_or(StartError::NotCallable)?;
        let PItemKind::Func(func) = item.kind else {
            return Err(StartError::NotCallable);
        };
        let f = &p.funcs[func as usize];
        if args.len() > f.params.len() {
            return Err(StartError::Arguments);
        }
        let consts = vec![None; p.items.len()];
        let mut m = Machine {
            p,
            ctl: Vec::new(),
            envs: Vec::new(),
            mode: Mode::Done,
            consts,
            prompts: Vec::new(),
            heap: Heap::with_limit(limits.heap),
            limits,
            steps: 0,
            next_checkpoint: CHECKPOINT,
            depth: 0,
            level: 0,
            yield_due: false,
            cancel,
            outcome: None,
            pending: None,
            frames: FrameStack::new(session, limits.steps),
            pending_override: None,
            site_hint: 0,
        };
        let mut given: Vec<Option<Value>> = args.into_iter().map(Some).collect();
        given.resize(m.p.funcs[func as usize].params.len(), None);
        m.enter_as(func, given, &[], Loc::default(), FrameKind::Entry);
        Ok(m)
    }

    /// The steps charged so far.
    #[must_use]
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// The run's heap ledger.
    #[must_use]
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    #[must_use]
    pub fn prepared(&self) -> &Arc<Prepared> {
        &self.p
    }

    /// The frame the evaluation is in now.
    #[must_use]
    pub fn frame(&self) -> &FrameCtx {
        &self.frames.top().ctx
    }

    /// Answers the effect the last [`Suspend::Effect`] asked for.
    ///
    /// For `declassify`, the host answers only whether the user agreed (`true`): the
    /// interpreter itself removes the labels, from the argument alone, so no host can remove
    /// a label from anything else. Any other answer is refused (`Denied { by: user }` for
    /// `false`).
    pub fn resume_effect(&mut self, result: Result<Value, RtError>) {
        if let Some(K::Effect) = self.ctl.last() {
            self.ctl.pop();
            let pending = self.pending.take();
            let result = match (pending, result) {
                (Some(req), Ok(answer)) if &*req.builtin == "declassify" => {
                    self.declassify(&req, &answer)
                }
                // A frame without `secrets` cannot run an effect whose result is labelled.
                (Some(req), Ok(answer)) if self.refuses_secret(&answer) => Err(raise(
                    &self.p,
                    Kind::Denied("sees"),
                    &format!(
                        "`{}` returned a secret into a frame whose `sees` lacks `secrets`",
                        req.builtin
                    ),
                    Labels::NONE,
                    req.loc,
                )),
                // So does a catchable error whose message or data is labelled.
                (Some(req), Err(e))
                    if e.class == ErrClass::Catchable && self.refuses_secret(&e.value) =>
                {
                    Err(raise(
                        &self.p,
                        Kind::Denied("sees"),
                        &format!(
                            "`{}` failed with a secret in a frame whose `sees` lacks `secrets`",
                            req.builtin
                        ),
                        Labels::NONE,
                        req.loc,
                    ))
                }
                (_, r) => r,
            };
            self.mode = match result {
                Ok(v) => Mode::Ret(v),
                Err(e) => Mode::Unwind(Signal::Raise(e)),
            };
        }
    }

    fn declassify(&self, req: &EffectRequest, answer: &Value) -> Result<Value, RtError> {
        match answer.as_bool() {
            Some(true) => {
                let x = req.args.first().cloned().flatten().unwrap_or_default();
                x.declassified(&self.heap)
                    .map_err(|f| fault(&self.p, f, Labels::NONE, req.loc))
            }
            Some(false) => Err(raise(
                &self.p,
                Kind::Denied("user"),
                "the user did not declassify the value",
                Labels::NONE,
                req.loc,
            )),
            None => Err(raise(
                &self.p,
                Kind::Invalid,
                "a host answered `declassify` with something other than a Bool",
                Labels::NONE,
                req.loc,
            )),
        }
    }

    /// Runs until the next checkpoint, effect or the end.
    pub fn run(&mut self) -> Suspend {
        loop {
            if self.yield_due {
                self.yield_due = false;
                if self.cancel.is_cancelled() && !matches!(self.mode, Mode::Done | Mode::Effect(_))
                {
                    let loc = Loc::default();
                    let e = raise(
                        &self.p,
                        Kind::Cancelled,
                        "the evaluation was cancelled",
                        Labels::NONE,
                        loc,
                    );
                    self.mode = Mode::Unwind(Signal::Raise(e));
                } else {
                    return Suspend::Yield;
                }
            }
            match std::mem::replace(&mut self.mode, Mode::Done) {
                Mode::Eval(n) => self.eval(n),
                Mode::Ret(v) => self.ret(v),
                Mode::Unwind(s) => self.unwind(s),
                Mode::Effect(req) => {
                    self.ctl.push(K::Effect);
                    self.pending = Some(req.clone());
                    return Suspend::Effect(req);
                }
                Mode::Done => {
                    if let Some(o) = self.outcome.take() {
                        return Suspend::Done(o);
                    }
                    // An effect is outstanding and was not answered.
                    if let Some(K::Effect) = self.ctl.last() {
                        let loc = Loc::default();
                        let e = raise(
                            &self.p,
                            Kind::Unsupported,
                            "an effect was not answered",
                            Labels::NONE,
                            loc,
                        );
                        self.resume_effect(Err(e));
                        continue;
                    }
                    let e = raise(
                        &self.p,
                        Kind::Invalid,
                        "the evaluation has ended",
                        Labels::NONE,
                        Loc::default(),
                    );
                    return Suspend::Done(Outcome::Raised(e));
                }
            }
        }
    }

    // ---- accounting ----

    /// Debits `n` steps, or raises `Budget { which: steps }` without debiting.
    fn charge(&mut self, n: u64, loc: Loc) -> Result<(), RtError> {
        let next = self.steps.saturating_add(n);
        if next > self.frames.step_limit {
            let mut e = raise(
                &self.p,
                Kind::Budget("steps"),
                "the step limit was reached",
                Labels::NONE,
                loc,
            );
            e.scope = self.frames.step_scope;
            return Err(e);
        }
        self.steps = next;
        if self.steps >= self.next_checkpoint {
            self.yield_due = true;
            self.next_checkpoint = (self.steps / CHECKPOINT + 1) * CHECKPOINT;
        }
        Ok(())
    }

    fn fail(&mut self, e: RtError) {
        self.mode = Mode::Unwind(Signal::Raise(e));
    }

    fn invalid(&mut self, message: &str, labels: Labels, loc: Loc) {
        let e = raise(&self.p, Kind::Invalid, message, labels, loc);
        self.fail(e);
    }

    fn fault_at(&mut self, f: Fault, labels: Labels, loc: Loc) {
        let e = fault(&self.p, f, labels, loc);
        self.fail(e);
    }

    // ---- locals ----

    fn get(&self, slot: u32) -> Option<Value> {
        self.envs
            .last()?
            .slots
            .get(slot as usize)
            .map(|s| s.value.clone())
    }

    fn set(&mut self, slot: u32, value: Value) {
        let level = self.level;
        if let Some(s) = self
            .envs
            .last_mut()
            .and_then(|e| e.slots.get_mut(slot as usize))
        {
            *s = Slot { value, level };
        }
    }

    // ---- patterns ----

    /// Matches `v` against pattern `pat`, binding into the current frame. Charges one step
    /// per pattern node it tests.
    fn matches(&mut self, pat: PatId, v: &Value, loc: Loc) -> Result<bool, RtError> {
        self.charge(1, loc)?;
        let p = Arc::clone(&self.p);
        let own = v.labels;
        Ok(match &p.pats[pat as usize] {
            PPat::Wild => true,
            PPat::Bind(slot, sub) => {
                if let Some(sub) = sub
                    && !self.matches(*sub, v, loc)?
                {
                    return Ok(false);
                }
                self.set(*slot, v.clone());
                true
            }
            PPat::Lit(l) => ops::eq(v, l),
            PPat::Variant(e, i, fields) => match &v.repr {
                Repr::Variant(e2, i2, _) => (e, i) == (e2, i2) && fields.is_none(),
                Repr::VariantP(vd) => {
                    if (vd.enum_, vd.index) != (*e, *i) {
                        return Ok(false);
                    }
                    match fields {
                        Some(rp) => {
                            let payload = vd.payload.clone().with_labels(own);
                            self.record_fields(&rp.fields, &payload, loc)?
                        }
                        None => true,
                    }
                }
                _ => false,
            },
            PPat::Tuple(ps) => match &v.repr {
                Repr::Tuple(a) if a.items.len() == ps.len() => {
                    for (q, x) in ps.iter().zip(a.items.iter()) {
                        if !self.matches(*q, &x.clone().with_labels(own), loc)? {
                            return Ok(false);
                        }
                    }
                    true
                }
                _ => false,
            },
            PPat::List {
                before,
                rest,
                after,
            } => {
                let Repr::List(l) = &v.repr else {
                    return Ok(false);
                };
                let (b, a) = (before.len() as u64, after.len() as u64);
                let n = l.len();
                let fits = if rest.is_some() {
                    n >= b + a
                } else {
                    n == b + a
                };
                if !fits {
                    return Ok(false);
                }
                for (i, q) in before.iter().enumerate() {
                    let x = l
                        .get(i as u64)
                        .cloned()
                        .unwrap_or_default()
                        .with_labels(own);
                    if !self.matches(*q, &x, loc)? {
                        return Ok(false);
                    }
                }
                for (i, q) in after.iter().enumerate() {
                    let x = l
                        .get(n - a + i as u64)
                        .cloned()
                        .unwrap_or_default()
                        .with_labels(own);
                    if !self.matches(*q, &x, loc)? {
                        return Ok(false);
                    }
                }
                if let Some(Some(slot)) = rest {
                    let middle = l
                        .slice(&self.heap, b, n - a)
                        .map_err(|f| fault(&p, f, own, loc))?;
                    self.set(*slot, Value::new_list(middle).with_labels(own));
                }
                true
            }
            PPat::Record(rp) => self.record_fields(&rp.fields, v, loc)?,
            PPat::Or(ps) => {
                for q in ps {
                    if self.matches(*q, v, loc)? {
                        return Ok(true);
                    }
                }
                false
            }
        })
    }

    fn record_fields(
        &mut self,
        fields: &[(Arc<str>, PatId)],
        v: &Value,
        loc: Loc,
    ) -> Result<bool, RtError> {
        if !matches!(v.repr, Repr::Record(_)) {
            return Ok(false);
        }
        for (name, q) in fields {
            let x = v
                .field(name)
                .unwrap_or_else(|| Value::none().with_labels(v.labels));
            if !self.matches(*q, &x, loc)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // ---- calls ----

    /// Enters function `func` with its arguments (`None` for a default).
    fn enter(&mut self, func: u32, args: Vec<Option<Value>>, captured: &[(u32, Value)], loc: Loc) {
        self.enter_as(func, args, captured, loc, FrameKind::Harness);
    }

    /// Enters `func`; a harness opens a frame of `kind` once its parameters are bound.
    fn enter_as(
        &mut self,
        func: u32,
        args: Vec<Option<Value>>,
        captured: &[(u32, Value)],
        loc: Loc,
        kind: FrameKind,
    ) {
        let overrides = self.pending_override.take();
        let p = Arc::clone(&self.p);
        let f = &p.funcs[func as usize];
        if self.depth >= self.limits.call_depth {
            let e = raise(
                &p,
                Kind::Budget("call_depth"),
                "the call depth limit was reached",
                Labels::NONE,
                loc,
            );
            self.fail(e);
            return;
        }
        self.depth += 1;
        let mut slots = vec![
            Slot {
                value: Value::unit(),
                level: self.level
            };
            f.slots as usize
        ];
        for (s, v) in captured {
            if let Some(slot) = slots.get_mut(*s as usize) {
                slot.value = v.clone();
            }
        }
        self.envs.push(Env { slots });
        self.ctl.push(K::CallRet);
        if f.kind == FuncKind::Harness {
            let labelled = args
                .iter()
                .flatten()
                .any(|v| crate::sink::check(v).is_err());
            self.ctl.push(K::Open(Box::new(Opening {
                func,
                kind,
                loc,
                site: self.site_hint,
                overrides,
                labelled,
            })));
        }
        self.bind_params(func, 0, args, loc);
    }

    /// Binds parameters from `next` on, evaluating defaults as needed, then runs the body.
    fn bind_params(&mut self, func: u32, mut next: u32, mut args: Vec<Option<Value>>, loc: Loc) {
        let p = Arc::clone(&self.p);
        let f = &p.funcs[func as usize];
        while let Some((pat, default)) = f.params.get(next as usize) {
            if let Some(v) = args.get_mut(next as usize).and_then(Option::take) {
                match self.matches(*pat, &v, loc) {
                    Ok(true) => next += 1,
                    Ok(false) => {
                        return self.invalid(
                            "an argument does not match its parameter",
                            v.labels,
                            loc,
                        );
                    }
                    Err(e) => return self.fail(e),
                }
            } else {
                let Some(d) = default else {
                    return self.invalid("a call without a required argument", Labels::NONE, loc);
                };
                self.ctl.push(K::Entry { func, next, args });
                self.mode = Mode::Eval(*d);
                return;
            }
        }
        self.start_body(func);
    }

    /// Runs `func`'s body; a harness first evaluates its declared configuration (with the
    /// parameters bound) and opens its frame.
    fn start_body(&mut self, func: u32) {
        let p = Arc::clone(&self.p);
        let f = &p.funcs[func as usize];
        if f.kind != FuncKind::Harness {
            self.mode = Mode::Eval(f.body);
            return;
        }
        if let Some(c) = f.config {
            // `K::Open` stays on the stack and receives the configuration.
            self.mode = Mode::Eval(c);
            return;
        }
        match self.ctl.pop() {
            Some(K::Open(o)) => self.open_harness(&o, None),
            _ => self.invalid(
                "a harness call lost its frame",
                Labels::NONE,
                Loc::default(),
            ),
        }
    }

    /// Calls a function value with its arguments in parameter order.
    fn invoke(&mut self, f: &Value, args: Vec<Option<Value>>, type_args: &[u32], loc: Loc) {
        if !f.labels.is_empty() {
            self.ctl.push(K::AddLabels(f.labels));
        }
        match &f.repr {
            Repr::Item(i) => match self.p.items.get(*i as usize).map(|it| &it.kind) {
                Some(PItemKind::Func(func)) => {
                    let func = *func;
                    self.enter(func, args, &[], loc);
                }
                _ => self.invalid("this item is not a function", Labels::NONE, loc),
            },
            Repr::Closure(c) => {
                let c = Arc::clone(c);
                self.enter(c.func, args, &c.captured, loc);
            }
            Repr::Builtin(b) => self.call_builtin(*b, &args, type_args, loc),
            _ => self.invalid("this value is not a function", f.labels, loc),
        }
    }

    fn call_builtin(&mut self, b: u16, args: &[Option<Value>], type_args: &[u32], loc: Loc) {
        // Overrides apply to harness calls only; none may wait for a later call.
        self.pending_override = None;
        let Some(builtin) = BUILTINS.get(b as usize).copied() else {
            return self.invalid("an unknown built-in", Labels::NONE, loc);
        };
        let sizes = (builtin.size)(args);
        // The part of the charge the arguments fix is debited before the built-in runs.
        if let Err(e) = self.charge(steps_for(pre_work(builtin.class, sizes)), loc) {
            return self.fail(e);
        }
        let p = Arc::clone(&self.p);
        let heap = self.heap.clone();
        let cancel = self.cancel.clone();
        let mut cx = Cx {
            p: &p,
            heap: &heap,
            loc,
            type_args,
            debit: 0,
            cancel: &cancel,
        };
        match builtin.imp {
            Imp::Pure(f) => match f(&mut cx, args) {
                // Work done before a refusal is paid for too.
                Err(e) if cx.debit > 0 => match self.charge(cx.debit, loc) {
                    Ok(()) => self.fail(e),
                    Err(budget) => self.fail(budget),
                },
                Ok(v) => {
                    if builtin.class == Class::Linear
                        && let Err(e) = self.charge_output(sizes.n, output_size(&v), loc)
                    {
                        return self.fail(e);
                    }
                    self.mode = Mode::Ret(v);
                }
                Err(e) => self.fail(e),
            },
            Imp::Task(mk) => match mk(&mut cx, args) {
                Ok(task) => {
                    self.ctl.push(K::Task {
                        task,
                        class: builtin.class,
                        n: sizes.n,
                        loc,
                    });
                    self.drive_task(Resume::Start);
                }
                Err(e) => self.fail(e),
            },
        }
    }

    /// The output part of a `linear` charge: `⌈(n + m) / 64⌉ − ⌈n / 64⌉`.
    fn charge_output(&mut self, n: u64, m: u64, loc: Loc) -> Result<(), RtError> {
        let extra = n.saturating_add(m).div_ceil(64) - n.div_ceil(64);
        self.charge(extra, loc)
    }

    /// Resumes the task on top of the stack.
    fn drive_task(&mut self, input: Resume) {
        let Some(K::Task {
            mut task,
            class,
            n,
            loc,
        }) = self.ctl.pop()
        else {
            return self.invalid("no task to resume", Labels::NONE, Loc::default());
        };
        let p = Arc::clone(&self.p);
        let heap = self.heap.clone();
        let cancel = self.cancel.clone();
        let mut cx = Cx {
            p: &p,
            heap: &heap,
            loc,
            type_args: &[],
            debit: 0,
            cancel: &cancel,
        };
        match task.resume(&mut cx, input) {
            Ok(TaskStep::Call(f, args)) => {
                self.ctl.push(K::Task {
                    task,
                    class,
                    n,
                    loc,
                });
                self.invoke(&f, args.into_iter().map(Some).collect(), &[], loc);
            }
            Ok(TaskStep::Charge(steps)) => {
                if let Err(e) = self.charge(steps, loc) {
                    return self.fail(e);
                }
                self.ctl.push(K::Task {
                    task,
                    class,
                    n,
                    loc,
                });
                self.drive_task(Resume::Charged);
            }
            Ok(TaskStep::Done(v)) => {
                if class == Class::Linear
                    && let Err(e) = self.charge_output(n, output_size(&v), loc)
                {
                    return self.fail(e);
                }
                self.mode = Mode::Ret(v);
            }
            Err(e) => self.fail(e),
        }
    }

    // ---- evaluation ----

    /// Child `i` of a node gathered by [`K::Collect`], if it has one.
    fn kid(&self, node: NodeId, i: u32) -> Option<NodeId> {
        let i = i as usize;
        match &self.p.node(node).kind {
            PKind::Concat(xs) | PKind::Tuple(xs) | PKind::Par(xs) => xs.get(i).copied(),
            PKind::Show(x, _)
            | PKind::Field(x, _)
            | PKind::Is(x, _, _)
            | PKind::Unary(_, x)
            | PKind::ToolSet(x)
            | PKind::Raise(x) => (i == 0).then_some(*x),
            PKind::Return(x) | PKind::Break(x) => x.filter(|_| i == 0),
            PKind::List(items) => items.get(i).map(|(_, x)| *x),
            PKind::Record { parts, .. } => parts.get(i).map(|part| match part {
                PPart::Field(_, x) | PPart::Spread(x) => *x,
            }),
            PKind::Map(entries) => entries.get(i).map(|(_, x)| *x),
            PKind::Variant { payload, .. } => payload.filter(|_| i == 0),
            PKind::Binary(_, a, b) | PKind::Index(a, b) => match i {
                0 => Some(*a),
                1 => Some(*b),
                _ => None,
            },
            PKind::Call { callee, args, .. } => match callee {
                PCallee::Value(f) => {
                    if i == 0 {
                        Some(*f)
                    } else {
                        args.get(i - 1).copied()
                    }
                }
                _ => args.get(i).copied(),
            },
            PKind::Effect { args, .. } => args.get(i).copied(),
            _ => None,
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of node")]
    fn eval(&mut self, node: NodeId) {
        let p = Arc::clone(&self.p);
        let n = p.node(node);
        let loc = n.loc;
        if let Err(e) = self.charge(1, loc) {
            return self.fail(e);
        }
        match &n.kind {
            PKind::Const(v) | PKind::ToolName(v) => self.mode = Mode::Ret(v.clone()),
            PKind::Local(s) => match self.get(*s) {
                Some(v) => self.mode = Mode::Ret(v),
                None => self.invalid("a local slot out of range", Labels::NONE, loc),
            },
            PKind::Item(i) => match &p.items[*i as usize].kind {
                PItemKind::Func(_) => self.mode = Mode::Ret(Value::item(*i)),
                PItemKind::Const { node: c, slots } => {
                    if let Some(v) = &self.consts[*i as usize] {
                        self.mode = Mode::Ret(v.clone());
                    } else {
                        self.envs.push(Env {
                            slots: vec![Slot::default(); *slots as usize],
                        });
                        self.ctl.push(K::Const(*i));
                        self.mode = Mode::Eval(*c);
                    }
                }
                PItemKind::State => {
                    let e = raise(
                        &p,
                        Kind::Unsupported,
                        "package state needs the host (stage 11)",
                        Labels::NONE,
                        loc,
                    );
                    self.fail(e);
                }
            },
            PKind::Builtin(b) => self.mode = Mode::Ret(Value::builtin(*b)),
            PKind::Unsupported(what) => {
                let e = raise(&p, Kind::Unsupported, what, Labels::NONE, loc);
                self.fail(e);
            }
            PKind::Closure(func) => {
                let f = &p.funcs[*func as usize];
                let mut captured = Vec::with_capacity(f.captures.len());
                for (outer, own) in &f.captures {
                    captured.push((*own, self.get(*outer).unwrap_or_default()));
                }
                match Value::new_closure(&self.heap, *func, captured) {
                    Ok(v) => self.mode = Mode::Ret(v),
                    Err(f) => self.fault_at(f, Labels::NONE, loc),
                }
            }
            PKind::Block(stmts) => {
                if stmts.is_empty() {
                    self.mode = Mode::Ret(Value::unit());
                } else {
                    self.ctl.push(K::Block {
                        node,
                        next: 0,
                        defers: Vec::new(),
                    });
                    self.block_next(Value::unit());
                }
            }
            PKind::If(c, t, e) => {
                self.ctl.push(K::If(*t, *e));
                self.mode = Mode::Eval(*c);
            }
            PKind::IfLet { value, .. } => {
                self.ctl.push(K::IfLet(node));
                self.mode = Mode::Eval(*value);
            }
            PKind::And(a, b) => {
                self.ctl.push(K::And(*b));
                self.mode = Mode::Eval(*a);
            }
            PKind::Or(a, b) => {
                self.ctl.push(K::Or(*b));
                self.mode = Mode::Eval(*a);
            }
            PKind::Coalesce(a, b) => {
                self.ctl.push(K::Coalesce(*b));
                self.mode = Mode::Eval(*a);
            }
            PKind::OptChain(o, slot, then) => {
                self.ctl.push(K::Opt(*slot, *then));
                self.mode = Mode::Eval(*o);
            }
            PKind::Match(s, _) => {
                self.ctl.push(K::Match(node));
                self.mode = Mode::Eval(*s);
            }
            PKind::For(_, x, _) | PKind::Repeat(x, _) => {
                self.ctl.push(K::LoopInit(node));
                self.mode = Mode::Eval(*x);
            }
            PKind::While(..) | PKind::WhileLet { .. } => {
                self.ctl.push(K::Loop {
                    node,
                    iter: Iter::While,
                    phase: Phase::Cond,
                });
                self.loop_next(loc);
            }
            PKind::Loop(_) => {
                self.ctl.push(K::Loop {
                    node,
                    iter: Iter::Forever,
                    phase: Phase::Body,
                });
                self.loop_next(loc);
            }
            PKind::Continue => self.mode = Mode::Unwind(Signal::Continue),
            PKind::Break(None) => self.mode = Mode::Unwind(Signal::Break(Value::unit())),
            PKind::Return(None) => self.mode = Mode::Unwind(Signal::Return(Value::unit())),
            PKind::Try(body, _) => {
                self.ctl.push(K::Try(node));
                self.mode = Mode::Eval(*body);
            }
            PKind::Prompt(list) => {
                self.prompts.push(Vec::new());
                self.ctl.push(K::PromptRoot);
                self.ctl.push(K::PromptRun {
                    list: *list,
                    idx: 0,
                });
                self.mode = Mode::Ret(Value::unit());
            }
            PKind::Race(xs) => {
                let Some(first) = xs.first() else {
                    return self.invalid("an empty race", Labels::NONE, loc);
                };
                self.frames.enter_branch(n.site);
                self.level += 1;
                self.ctl.push(K::Race {
                    node,
                    next: 1,
                    first: None,
                });
                self.mode = Mode::Eval(*first);
            }
            PKind::Par(xs) => {
                let Some(first) = xs.first() else {
                    return self.finish(node, Vec::new());
                };
                self.frames.enter_branch(n.site);
                self.level += 1;
                self.ctl.push(K::Par {
                    node,
                    next: 1,
                    vals: Held::new(),
                });
                self.mode = Mode::Eval(*first);
            }
            PKind::With(config, _) => {
                self.ctl.push(K::WithCfg(node));
                self.mode = Mode::Eval(*config);
            }
            PKind::Override(call) => self.mode = Mode::Eval(*call),
            _ => match self.kid(node, 0) {
                Some(first) => {
                    self.ctl.push(K::Collect {
                        node,
                        next: 1,
                        vals: Held::new(),
                    });
                    self.mode = Mode::Eval(first);
                }
                None => self.finish(node, Vec::new()),
            },
        }
    }

    /// Runs the next statement of the block on top of the stack (after `last`, the previous
    /// statement's value).
    fn block_next(&mut self, last: Value) {
        let p = Arc::clone(&self.p);
        let node = match self.ctl.last() {
            Some(K::Block { node, .. }) => *node,
            _ => return self.invalid("no block to continue", Labels::NONE, Loc::default()),
        };
        let pn = p.node(node);
        let PKind::Block(stmts) = &pn.kind else {
            return self.invalid("a block frame on something else", Labels::NONE, pn.loc);
        };
        loop {
            let i = match self.ctl.last_mut() {
                Some(K::Block { next, .. }) => {
                    let i = *next as usize;
                    *next += 1;
                    i
                }
                _ => return,
            };
            let Some(stmt) = stmts.get(i) else {
                // The block's value: its last statement's, when that is an expression.
                let value = match stmts.last() {
                    Some(PStmt::Expr(_)) => last,
                    _ => Value::unit(),
                };
                let Some(K::Block { defers, .. }) = self.ctl.pop() else {
                    return;
                };
                return self.exit_scope(defers, Exit::Value(value));
            };
            if let Err(e) = self.charge(1, pn.loc) {
                return self.fail(e);
            }
            match stmt {
                PStmt::Defer(b) => {
                    if let Some(K::Block { defers, .. }) = self.ctl.last_mut() {
                        defers.push(*b);
                    }
                    continue;
                }
                PStmt::Let(pat, v) => {
                    self.ctl.push(K::Let(*pat));
                    self.mode = Mode::Eval(*v);
                }
                PStmt::Var(slot, v) => {
                    self.ctl.push(K::Var(*slot));
                    self.mode = Mode::Eval(*v);
                }
                PStmt::Assign { .. } => {
                    self.ctl.push(K::Assign {
                        block: node,
                        stmt: u32::try_from(i).unwrap_or(u32::MAX),
                        next: 0,
                        vals: Held::new(),
                    });
                    self.assign_next();
                }
                PStmt::Expr(x) => self.mode = Mode::Eval(*x),
            }
            return;
        }
    }

    /// Leaves a scope: runs its `defer` blocks in reverse order, then continues with `exit`.
    fn exit_scope(&mut self, defers: Vec<NodeId>, exit: Exit) {
        if defers.is_empty() {
            self.mode = match exit {
                Exit::Value(v) => Mode::Ret(v),
                Exit::Signal(s) => Mode::Unwind(s),
            };
            return;
        }
        self.ctl.push(K::Defers {
            pending: defers,
            exit,
        });
        self.next_defer();
    }

    fn next_defer(&mut self) {
        let Some(K::Defers { pending, .. }) = self.ctl.last_mut() else {
            return;
        };
        if let Some(b) = pending.pop() {
            self.ctl.push(K::Discard);
            self.mode = Mode::Eval(b);
        } else {
            let Some(K::Defers { exit, .. }) = self.ctl.pop() else {
                return;
            };
            self.mode = match exit {
                Exit::Value(v) => Mode::Ret(v),
                Exit::Signal(s) => Mode::Unwind(s),
            };
        }
    }

    /// The assignment on top of the stack: gathers its index values, then its value.
    fn assign_next(&mut self) {
        let Some(K::Assign {
            block, stmt, next, ..
        }) = self.ctl.last_mut()
        else {
            return;
        };
        let (block, stmt_i) = (*block, *stmt);
        let p = Arc::clone(&self.p);
        let PKind::Block(stmts) = &p.node(block).kind else {
            return;
        };
        let Some(PStmt::Assign { path, value, .. }) = stmts.get(stmt_i as usize) else {
            return;
        };
        let indexes: Vec<NodeId> = path
            .iter()
            .filter_map(|s| match s {
                PStep::Index(x) => Some(*x),
                PStep::Field(_) => None,
            })
            .collect();
        let i = *next as usize;
        *next += 1;
        if let Some(x) = indexes.get(i) {
            self.mode = Mode::Eval(*x);
        } else if i == indexes.len() {
            self.mode = Mode::Eval(*value);
        } else {
            let Some(K::Assign { vals, .. }) = self.ctl.pop() else {
                return;
            };
            self.assign(block, stmt_i, vals.into_vals());
        }
    }

    /// Performs an assignment: reads the place, applies the operator, rebuilds the containers
    /// on the path (immutable values: a new value is made at each step).
    fn assign(&mut self, block: NodeId, stmt: u32, mut vals: Vec<Value>) {
        let p = Arc::clone(&self.p);
        let PKind::Block(stmts) = &p.node(block).kind else {
            return;
        };
        let Some(PStmt::Assign {
            slot,
            path,
            op,
            loc,
            ..
        }) = stmts.get(stmt as usize)
        else {
            return;
        };
        let loc = *loc;
        let Some(value) = vals.pop() else {
            return self.invalid("an assignment without a value", Labels::NONE, loc);
        };
        // The branch restriction (chapter 03): a `var` declared outside a concurrent branch
        // cannot be assigned inside it.
        let declared = self
            .envs
            .last()
            .and_then(|e| e.slots.get(*slot as usize))
            .map(|s| s.level);
        if declared != Some(self.level) {
            let mut e = raise(
                &self.p,
                Kind::Invalid,
                "a `var` declared outside a concurrent branch cannot be assigned inside it",
                Labels::NONE,
                loc,
            );
            // A program defect: fatal, so a sibling `race` branch cannot hide it.
            e.class = ErrClass::Fatal;
            return self.fail(e);
        }
        let Some(root) = self.get(*slot) else {
            return self.invalid("a local slot out of range", Labels::NONE, loc);
        };
        // Walk down the path, keeping each container.
        let mut chain = vec![root];
        let mut idx = vals.into_iter();
        let mut keys: Vec<Value> = Vec::new();
        for (i, step) in path.iter().enumerate() {
            let cur = chain.last().cloned().unwrap_or_default();
            // A plain `=` does not read the place it writes (a map key may be new).
            let reads = *op != AssignOp::Set || i + 1 < path.len();
            let next = match step {
                PStep::Field(name) => cur.field(name),
                PStep::Index(_) if !reads => {
                    keys.push(idx.next().unwrap_or_default());
                    Some(Value::unit())
                }
                PStep::Index(_) => {
                    let k = idx.next().unwrap_or_default();
                    if let Repr::Map(m) = &cur.repr
                        && let Err(e) = self.charge(ops::key_charge(m.len(), &k), loc)
                    {
                        return self.fail(e);
                    }
                    let got = self.index_value(&cur, &k, loc);
                    keys.push(k);
                    match got {
                        Ok(v) => Some(v),
                        Err(e) => return self.fail(e),
                    }
                }
            };
            let Some(next) = next else {
                return self.invalid(
                    "an assignment to a field the value does not have",
                    cur.labels,
                    loc,
                );
            };
            chain.push(next);
        }
        let old = chain.pop().unwrap_or_default();
        let new = if *op == AssignOp::Set {
            value
        } else {
            let bop = match op {
                AssignOp::Add => BinOp::Add,
                AssignOp::Sub => BinOp::Sub,
                AssignOp::Mul => BinOp::Mul,
                _ => BinOp::Div,
            };
            match self.binary(bop, &old, &value, loc) {
                Ok(v) => v,
                Err(e) => return self.fail(e),
            }
        };
        // Rebuild from the leaf up.
        let mut acc = new;
        let mut keys = keys.into_iter().rev();
        for step in path.iter().rev() {
            let container = chain.pop().unwrap_or_default();
            let rebuilt = match step {
                PStep::Field(name) => self.with_field(&container, name, acc, loc),
                PStep::Index(_) => {
                    let k = keys.next().unwrap_or_default();
                    if let Repr::Map(m) = &container.repr
                        && let Err(e) = self.charge(ops::key_charge(m.len(), &k), loc)
                    {
                        return self.fail(e);
                    }
                    self.with_index(&container, &k, acc, loc)
                }
            };
            match rebuilt {
                Ok(v) => acc = v,
                Err(e) => return self.fail(e),
            }
        }
        self.set(*slot, acc);
        self.mode = Mode::Ret(Value::unit());
    }

    fn with_field(&self, r: &Value, name: &str, v: Value, loc: Loc) -> Result<Value, RtError> {
        let Repr::Record(rd) = &r.repr else {
            return Err(raise(
                &self.p,
                Kind::Invalid,
                "a field assignment on something that is not a record",
                r.labels,
                loc,
            ));
        };
        let Some(i) = rd.layout.index(name) else {
            return Err(raise(
                &self.p,
                Kind::Invalid,
                "no such field",
                r.labels,
                loc,
            ));
        };
        let mut items = rd.items.to_vec();
        items[i] = v;
        Value::new_record(&self.heap, Arc::clone(&rd.layout), items)
            .map(|x| x.with_labels(r.labels))
            .map_err(|f| fault(&self.p, f, r.labels, loc))
    }

    fn with_index(&self, c: &Value, k: &Value, v: Value, loc: Loc) -> Result<Value, RtError> {
        // Which element changed depends on the index.
        let labels = c.labels | k.labels;
        let out = match (&c.repr, &k.repr) {
            (Repr::List(l), Repr::Int(i)) => {
                let i = u64::try_from(*i).map_err(|_| {
                    raise(
                        &self.p,
                        Kind::Invalid,
                        "a list index out of range",
                        labels,
                        loc,
                    )
                })?;
                l.set(&self.heap, i, v).map(Value::new_list)
            }
            (Repr::Map(m), _) => m.insert(&self.heap, k.clone(), v).map(Value::new_map),
            _ => Err(Fault::Invalid(
                "an index assignment on something that is not a list or map".to_owned(),
            )),
        };
        out.map(|x| x.with_labels(labels))
            .map_err(|f| fault(&self.p, f, labels, loc))
    }

    /// `c[k]`.
    fn index_value(&self, c: &Value, k: &Value, loc: Loc) -> Result<Value, RtError> {
        let labels = c.labels | k.labels;
        match (&c.repr, &k.repr) {
            (Repr::List(l), Repr::Int(i)) => u64::try_from(*i)
                .ok()
                .and_then(|i| l.get(i))
                .map(|v| v.clone().with_labels(labels))
                .ok_or_else(|| {
                    raise(
                        &self.p,
                        Kind::Invalid,
                        "a list index out of range",
                        labels,
                        loc,
                    )
                }),
            (Repr::Map(m), _) => m
                .get_entry(k)
                .map(|(stored, v)| v.clone().with_labels(labels | stored.labels))
                .ok_or_else(|| {
                    raise(
                        &self.p,
                        Kind::NotFound,
                        "no such key in the map",
                        labels,
                        loc,
                    )
                }),
            (Repr::Json(_), _) => Ok(crate::json::index(c, k).with_labels(labels)),
            _ => Err(raise(
                &self.p,
                Kind::Invalid,
                "`[…]` on something that is not a list, map or JSON",
                labels,
                loc,
            )),
        }
    }

    /// A binary operator on two values, with its charges beyond the node's step.
    fn binary(&mut self, op: BinOp, a: &Value, b: &Value, loc: Loc) -> Result<Value, RtError> {
        let labels = a.labels | b.labels;
        let deep = a.quick() | b.quick();
        let p = Arc::clone(&self.p);
        let err = |m: &str, l: Labels| raise(&p, Kind::Invalid, m, l, loc);
        match op {
            BinOp::Eq | BinOp::Ne => {
                if !(ops::is_scalar(a) && ops::is_scalar(b)) {
                    self.charge(a.weight().saturating_add(b.weight()).div_ceil(64), loc)?;
                }
                let same = ops::eq(a, b);
                Ok(Value::bool(if op == BinOp::Eq { same } else { !same }).with_labels(deep))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if !(ops::is_scalar(a) && ops::is_scalar(b)) {
                    self.charge(a.weight().saturating_add(b.weight()).div_ceil(64), loc)?;
                }
                ops::ordered(op, a, b)
                    .map(|r| Value::bool(r).with_labels(labels))
                    .ok_or_else(|| err("these values are not ordered", labels))
            }
            BinOp::In => {
                let found = match &b.repr {
                    Repr::List(l) => {
                        self.charge(steps_for(b.weight().saturating_add(a.weight())), loc)?;
                        let mut found = false;
                        l.for_each(&mut |x| found = found || ops::eq(x, a));
                        found
                    }
                    Repr::Map(m) => {
                        self.charge(ops::key_charge(m.len(), a), loc)?;
                        m.get_entry(a).is_some()
                    }
                    Repr::Range(r) => a.as_int().is_some_and(|x| ops::in_range(x, r)),
                    Repr::Str(s) => {
                        let part = a
                            .as_text()
                            .ok_or_else(|| err("`in` on text needs text", labels))?;
                        self.charge(steps_for(s.chars().saturating_add(ops::size(a))), loc)?;
                        s.as_str().contains(part)
                    }
                    _ => return Err(err("`in` needs a list, map, range or text", labels)),
                };
                Ok(Value::bool(found).with_labels(deep))
            }
            BinOp::Range | BinOp::RangeInclusive => match (a.as_int(), b.as_int()) {
                (Some(x), Some(y)) => {
                    Value::new_range(&self.heap, x, y, op == BinOp::RangeInclusive)
                        .map(|v| v.with_labels(labels))
                        .map_err(|f| fault(&p, f, labels, loc))
                }
                _ => Err(err("a range needs two Ints", labels)),
            },
            BinOp::Add if matches!((&a.repr, &b.repr), (Repr::Str(_), Repr::Str(_))) => {
                let (x, y) = (a.as_text().unwrap_or(""), b.as_text().unwrap_or(""));
                let m = match (&a.repr, &b.repr) {
                    (Repr::Str(s), Repr::Str(t)) => s.chars().saturating_add(t.chars()),
                    _ => 0,
                };
                self.charge(steps_for(m), loc)?;
                if x.len() + y.len() > crate::value::MAX_STR_BYTES {
                    return Err(fault(
                        &p,
                        Fault::TooLarge("a string longer than 16 MiB"),
                        labels,
                        loc,
                    ));
                }
                let mut s = String::with_capacity(x.len() + y.len());
                s.push_str(x);
                s.push_str(y);
                Value::new_text(&self.heap, s)
                    .map(|v| v.with_labels(labels))
                    .map_err(|f| fault(&p, f, labels, loc))
            }
            BinOp::Add if matches!((&a.repr, &b.repr), (Repr::List(_), Repr::List(_))) => {
                let (Repr::List(x), Repr::List(y)) = (&a.repr, &b.repr) else {
                    return Err(err("list `+`", labels));
                };
                self.charge(
                    steps_for(ops::log_work(x.len().saturating_add(y.len()))),
                    loc,
                )?;
                x.concat(&self.heap, y)
                    .map(|l| Value::new_list(l).with_labels(labels))
                    .map_err(|f| fault(&p, f, labels, loc))
            }
            _ => ops::arith(op, a, b)
                .map(|v| v.with_labels(labels))
                .map_err(|e| err(&e.message(op), labels)),
        }
    }

    /// Builds the value of a node from its gathered children.
    #[allow(clippy::too_many_lines, reason = "one arm per kind of node")]
    fn finish(&mut self, node: NodeId, vals: Vec<Value>) {
        let p = Arc::clone(&self.p);
        let n = p.node(node);
        let loc = n.loc;
        let heap = self.heap.clone();
        let result: Result<Value, RtError> = match &n.kind {
            PKind::Concat(_) => {
                let m: u64 = vals.iter().map(ops::size).sum();
                if let Err(e) = self.charge(steps_for(m), loc) {
                    return self.fail(e);
                }
                let labels = vals.iter().fold(Labels::NONE, |l, v| l | v.labels);
                let bytes: usize = vals.iter().map(|v| v.as_text().map_or(0, str::len)).sum();
                if bytes > crate::value::MAX_STR_BYTES {
                    return self.fault_at(
                        Fault::TooLarge("a string longer than 16 MiB"),
                        labels,
                        loc,
                    );
                }
                let mut s = String::with_capacity(bytes);
                for v in &vals {
                    s.push_str(v.as_text().unwrap_or(""));
                }
                Value::new_text(&heap, s)
                    .map(|v| v.with_labels(labels))
                    .map_err(|f| fault(&p, f, labels, loc))
            }
            PKind::Show(_, spec) => {
                let v = vals.into_iter().next().unwrap_or_default();
                let n_in = v.weight();
                if let Err(e) = self.charge(steps_for(n_in), loc) {
                    return self.fail(e);
                }
                let text = match spec {
                    Some(spec) => ops::format(&p, &v, spec),
                    None => Ok(ops::show(&p, &v)),
                };
                match text {
                    Ok(t) => {
                        if let Err(e) = self.charge_output(n_in, t.chars().count() as u64, loc) {
                            return self.fail(e);
                        }
                        Value::new_text(&heap, t)
                            .map(|x| x.with_labels(v.quick()))
                            .map_err(|f| fault(&p, f, v.quick(), loc))
                    }
                    Err(m) => Err(raise(&p, Kind::Invalid, &m, Labels::NONE, loc)),
                }
            }
            PKind::Tuple(_) => {
                Value::new_tuple(&heap, vals).map_err(|f| fault(&p, f, Labels::NONE, loc))
            }
            PKind::List(items) => {
                let mut list = List::empty();
                let mut own = Labels::NONE;
                let mut out: Result<(), Fault> = Ok(());
                for ((spread, _), v) in items.iter().zip(vals) {
                    if out.is_err() {
                        break;
                    }
                    if *spread {
                        let Repr::List(l) = &v.repr else {
                            out = Err(Fault::Invalid(
                                "a spread of something that is not a list".to_owned(),
                            ));
                            break;
                        };
                        // The spliced list's own labels decide the result's shape.
                        own |= v.labels;
                        if let Err(e) = self.charge(
                            steps_for(ops::log_work(list.len().saturating_add(l.len()))),
                            loc,
                        ) {
                            return self.fail(e);
                        }
                        match list.concat(&heap, l) {
                            Ok(x) => list = x,
                            Err(f) => out = Err(f),
                        }
                    } else {
                        match list.push(&heap, v) {
                            Ok(x) => list = x,
                            Err(f) => out = Err(f),
                        }
                    }
                }
                out.map(|()| Value::new_list(list).with_labels(own))
                    .map_err(|f| fault(&p, f, own, loc))
            }
            PKind::Record { layout, parts } => {
                let mut items = vec![Value::none(); layout.names.len()];
                for (part, v) in parts.iter().zip(vals) {
                    match part {
                        PPart::Field(slot, _) => {
                            if let Some(x) = items.get_mut(*slot as usize) {
                                *x = v;
                            }
                        }
                        PPart::Spread(_) => {
                            for (i, name) in layout.names.iter().enumerate() {
                                if let Some(x) = v.field(name) {
                                    items[i] = x;
                                }
                            }
                        }
                    }
                }
                Value::new_record(&heap, Arc::clone(layout), items)
                    .map_err(|f| fault(&p, f, Labels::NONE, loc))
            }
            PKind::Map(entries) => {
                let mut m = Map::empty();
                let mut out = Ok(());
                for ((k, _), v) in entries.iter().zip(vals) {
                    match m.insert(&heap, k.clone(), v) {
                        Ok(x) => m = x,
                        Err(f) => {
                            out = Err(f);
                            break;
                        }
                    }
                }
                out.map(|()| Value::new_map(m))
                    .map_err(|f| fault(&p, f, Labels::NONE, loc))
            }
            PKind::Variant { enum_, index, .. } => Value::new_variant(
                &heap,
                *enum_,
                *index,
                p.variant_weight(*enum_, *index),
                vals.into_iter().next(),
            )
            .map_err(|f| fault(&p, f, Labels::NONE, loc)),
            PKind::Unary(op, _) => {
                let v = vals.into_iter().next().unwrap_or_default();
                let labels = v.labels;
                let r = match (op, &v.repr) {
                    (UnOp::Not, Repr::Bool(b)) => Some(Value::bool(!b)),
                    (UnOp::Neg, Repr::Int(i)) => i.checked_neg().map(Value::int),
                    (UnOp::Neg, Repr::Float(f)) => Some(Value::float(-f)),
                    (UnOp::Neg, Repr::Duration(d)) => d.checked_neg().map(Value::duration),
                    (UnOp::Neg, Repr::Money(m)) => m.checked_neg().map(Value::money),
                    _ => None,
                };
                r.map(|r| r.with_labels(labels))
                    .ok_or_else(|| raise(&p, Kind::Invalid, "negation overflowed", labels, loc))
            }
            PKind::Binary(op, _, _) => {
                let mut it = vals.into_iter();
                let (a, b) = (it.next().unwrap_or_default(), it.next().unwrap_or_default());
                self.binary(*op, &a, &b, loc)
            }
            PKind::Index(_, _) => {
                let mut it = vals.into_iter();
                let (c, k) = (it.next().unwrap_or_default(), it.next().unwrap_or_default());
                // Indexing `Json` looks through the node: `linear` in its size; a map's key
                // comparisons walk the key.
                let extra = match &c.repr {
                    Repr::Json(_) => steps_for(ops::size(&c)),
                    Repr::Map(m) => ops::key_charge(m.len(), &k),
                    _ => 0,
                };
                if let Err(e) = self.charge(extra, loc) {
                    return self.fail(e);
                }
                self.index_value(&c, &k, loc)
            }
            PKind::Field(_, name) => {
                let v = vals.into_iter().next().unwrap_or_default();
                match &v.repr {
                    Repr::Record(_) => Ok(v
                        .field(name)
                        .unwrap_or_else(|| Value::none().with_labels(v.labels))),
                    _ => Err(raise(
                        &p,
                        Kind::Invalid,
                        "a field read on something that is not a record",
                        v.labels,
                        loc,
                    )),
                }
            }
            PKind::Is(_, e, i) => {
                let v = vals.into_iter().next().unwrap_or_default();
                let is = match &v.repr {
                    Repr::Variant(e2, i2, _) => (e2, i2) == (e, i),
                    Repr::VariantP(vd) => (vd.enum_, vd.index) == (*e, *i),
                    _ => false,
                };
                Ok(Value::bool(is).with_labels(v.labels))
            }
            PKind::Raise(_) => {
                let v = vals.into_iter().next().unwrap_or_default();
                return self.fail(RtError {
                    value: v,
                    class: ErrClass::Catchable,
                    scope: 0,
                });
            }
            PKind::Return(_) => {
                let v = vals.into_iter().next().unwrap_or_default();
                self.mode = Mode::Unwind(Signal::Return(v));
                return;
            }
            PKind::Break(_) => {
                let v = vals.into_iter().next().unwrap_or_default();
                self.mode = Mode::Unwind(Signal::Break(v));
                return;
            }
            PKind::Par(_) => {
                Value::new_tuple(&heap, vals).map_err(|f| fault(&p, f, Labels::NONE, loc))
            }
            PKind::ToolSet(_) => Ok(vals.into_iter().next().unwrap_or_default()),
            PKind::Call {
                callee,
                order,
                config,
                ..
            } => {
                let mut vals = vals;
                let f = match callee {
                    PCallee::Value(_) => {
                        if vals.is_empty() {
                            return self.invalid("a call without its callee", Labels::NONE, loc);
                        }
                        Some(vals.remove(0))
                    }
                    _ => None,
                };
                let args = arrange(order, vals);
                if let Some(c) = config {
                    // The overrides are evaluated after the arguments, in the caller's frame.
                    self.ctl.push(K::Override { node, f, args });
                    self.mode = Mode::Eval(*c);
                    return;
                }
                self.dispatch(node, f, args);
                return;
            }
            PKind::Effect { name, order, .. } => {
                let args = arrange(order, vals);
                if crate::frame::meta::answers(name) {
                    let limit = self.limits.heap;
                    match crate::frame::meta::answer(&p, &heap, &self.frames, limit, name) {
                        Ok(v) => self.mode = Mode::Ret(v),
                        Err(f) => self.fault_at(f, Labels::NONE, loc),
                    }
                    return;
                }
                if let Some(item) = crate::frame::gate(name)
                    && !self.frames.top().ctx.scope.sees.has(item)
                {
                    let e = raise(
                        &p,
                        Kind::Denied("sees"),
                        &format!("this frame's `sees` lacks `{}`", item.name()),
                        Labels::NONE,
                        loc,
                    );
                    return self.fail(e);
                }
                self.frames.flush(self.steps);
                let hash = p
                    .hashes
                    .get(loc.module as usize)
                    .copied()
                    .unwrap_or([0; 32]);
                self.mode = Mode::Effect(EffectRequest {
                    builtin: Arc::clone(name),
                    args,
                    site: (hash, n.site),
                    loc,
                    frame: self.frames.top().ctx.clone(),
                    branch: self.frames.branch_path(),
                });
                return;
            }
            _ => Err(raise(
                &p,
                Kind::Invalid,
                "a node the evaluator cannot finish",
                Labels::NONE,
                loc,
            )),
        };
        match result {
            Ok(v) => self.mode = Mode::Ret(v),
            Err(e) => self.fail(e),
        }
    }

    /// Makes the call `node` describes, with its callee value (for a call of a value) and its
    /// arguments in parameter order.
    fn dispatch(&mut self, node: NodeId, f: Option<Value>, args: Vec<Option<Value>>) {
        let p = Arc::clone(&self.p);
        let n = p.node(node);
        let loc = n.loc;
        let PKind::Call {
            callee, type_args, ..
        } = &n.kind
        else {
            return self.invalid("a call node that is not a call", Labels::NONE, loc);
        };
        self.site_hint = n.site;
        {
            {
                match callee {
                    PCallee::Item(i) => match &p.items[*i as usize].kind {
                        PItemKind::Func(func) => self.enter(*func, args, &[], loc),
                        _ => self.invalid(
                            "a call of an item that is not a function",
                            Labels::NONE,
                            loc,
                        ),
                    },
                    PCallee::Builtin(b) => self.call_builtin(*b, &args, type_args, loc),
                    PCallee::Value(_) => {
                        let f = f.unwrap_or_default();
                        self.invoke(&f, args, type_args, loc);
                    }
                    PCallee::Missing(name) => {
                        let e = raise(
                            &p,
                            Kind::Unsupported,
                            &format!("the built-in `{name}` arrives with a later stage"),
                            Labels::NONE,
                            loc,
                        );
                        self.fail(e);
                    }
                }
            }
        }
    }

    // ---- returning a value to the frame below ----

    #[allow(clippy::too_many_lines, reason = "one arm per kind of frame")]
    fn ret(&mut self, v: Value) {
        let Some(k) = self.ctl.pop() else {
            self.outcome = Some(Outcome::Returned(v));
            self.mode = Mode::Done;
            return;
        };
        let p = Arc::clone(&self.p);
        match k {
            K::Collect {
                node,
                next,
                mut vals,
            } => {
                if let Err(f) = vals.push(&self.heap, v) {
                    return self.fault_at(f, Labels::NONE, p.node(node).loc);
                }
                match self.kid(node, next) {
                    Some(x) => {
                        self.ctl.push(K::Collect {
                            node,
                            next: next + 1,
                            vals,
                        });
                        self.mode = Mode::Eval(x);
                    }
                    None => self.finish(node, vals.into_vals()),
                }
            }
            K::Block { node, next, defers } => {
                self.ctl.push(K::Block { node, next, defers });
                self.block_next(v);
            }
            K::Let(pat) => {
                let loc = Loc::default();
                match self.matches(pat, &v, loc) {
                    Ok(true) => self.block_next(Value::unit()),
                    Ok(false) => self.invalid("a `let` pattern did not match", v.labels, loc),
                    Err(e) => self.fail(e),
                }
            }
            K::Var(slot) => {
                self.set(slot, v);
                self.block_next(Value::unit());
            }
            K::Assign {
                block,
                stmt,
                next,
                mut vals,
            } => {
                if let Err(f) = vals.push(&self.heap, v) {
                    return self.fault_at(f, Labels::NONE, Loc::default());
                }
                self.ctl.push(K::Assign {
                    block,
                    stmt,
                    next,
                    vals,
                });
                self.assign_next();
            }
            K::Discard => self.next_defer(),
            K::Defers { .. } => self.invalid(
                "a value returned to a defer frame",
                Labels::NONE,
                Loc::default(),
            ),
            K::If(then, else_) => match v.as_bool() {
                Some(true) => self.mode = Mode::Eval(then),
                Some(false) => match else_ {
                    Some(e) => self.mode = Mode::Eval(e),
                    None => self.mode = Mode::Ret(Value::unit()),
                },
                None => self.invalid(
                    "an `if` condition that is not a Bool",
                    v.labels,
                    Loc::default(),
                ),
            },
            K::IfLet(node) => {
                let pn = p.node(node);
                let PKind::IfLet {
                    pat,
                    unwrap,
                    then,
                    else_,
                    ..
                } = &pn.kind
                else {
                    return;
                };
                let hit = if *unwrap && v.is_none() {
                    Ok(false)
                } else {
                    self.matches(*pat, &v, pn.loc)
                };
                match hit {
                    Ok(true) => self.mode = Mode::Eval(*then),
                    Ok(false) => match else_ {
                        Some(e) => self.mode = Mode::Eval(*e),
                        None => self.mode = Mode::Ret(Value::unit()),
                    },
                    Err(e) => self.fail(e),
                }
            }
            K::And(b) => {
                if let Some(false) = v.as_bool() {
                    self.mode = Mode::Ret(v);
                } else {
                    self.ctl.push(K::AddLabels(v.labels));
                    self.mode = Mode::Eval(b);
                }
            }
            K::Or(b) => {
                if let Some(true) = v.as_bool() {
                    self.mode = Mode::Ret(v);
                } else {
                    self.ctl.push(K::AddLabels(v.labels));
                    self.mode = Mode::Eval(b);
                }
            }
            K::Coalesce(b) => {
                if v.is_none() {
                    // Which side was taken depends on the option.
                    self.ctl.push(K::AddLabels(v.labels));
                    self.mode = Mode::Eval(b);
                } else {
                    self.mode = Mode::Ret(v);
                }
            }
            K::AddLabels(l) => self.mode = Mode::Ret(v.with_labels(l)),
            K::Opt(slot, then) => {
                if v.is_none() {
                    self.mode = Mode::Ret(v);
                } else {
                    self.ctl.push(K::AddLabels(v.labels));
                    self.set(slot, v);
                    self.mode = Mode::Eval(then);
                }
            }
            K::Match(node) => self.match_arm(node, v, 0, false),
            K::Guard {
                node,
                value,
                arm,
                catch,
            } => match v.as_bool() {
                Some(true) => {
                    let body = arm_of(&p, node, arm, catch).map(|a| a.body);
                    match body {
                        Some(b) => self.mode = Mode::Eval(b),
                        None => self.invalid("an arm out of range", Labels::NONE, Loc::default()),
                    }
                }
                _ => self.match_arm(node, value, arm + 1, catch),
            },
            K::LoopInit(node) => {
                let pn = p.node(node);
                let iter = match &pn.kind {
                    PKind::Repeat(..) => match v.as_int() {
                        Some(n) => Ok(Iter::Repeat(n.max(0))),
                        None => Err(raise(
                            &p,
                            Kind::Invalid,
                            "`repeat` needs an Int",
                            v.labels,
                            pn.loc,
                        )),
                    },
                    _ => match &v.repr {
                        Repr::List(l) => Ok(Iter::List(l.cursor(), v.labels)),
                        Repr::Map(m) => Ok(Iter::Map(m.cursor(), v.labels)),
                        Repr::Range(r) => Ok(Iter::Range {
                            next: i128::from(r.start),
                            end: i128::from(r.end) + i128::from(r.inclusive),
                            labels: v.labels,
                        }),
                        _ => Err(raise(
                            &p,
                            Kind::Invalid,
                            "`for` iterates lists, ranges and maps",
                            v.labels,
                            pn.loc,
                        )),
                    },
                };
                match iter {
                    Ok(iter) => {
                        self.ctl.push(K::Loop {
                            node,
                            iter,
                            phase: Phase::Body,
                        });
                        self.loop_next(pn.loc);
                    }
                    Err(e) => self.fail(e),
                }
            }
            K::Loop { node, iter, phase } => {
                let pn = p.node(node);
                if phase == Phase::Cond {
                    let go = match &pn.kind {
                        PKind::While(..) => match v.as_bool() {
                            Some(b) => Ok(b),
                            None => Err(raise(
                                &p,
                                Kind::Invalid,
                                "a `while` condition that is not a Bool",
                                v.labels,
                                pn.loc,
                            )),
                        },
                        PKind::WhileLet { pat, unwrap, .. } => {
                            if *unwrap && v.is_none() {
                                Ok(false)
                            } else {
                                self.matches(*pat, &v, pn.loc)
                            }
                        }
                        _ => Ok(false),
                    };
                    match go {
                        Ok(true) => {
                            let body = match &pn.kind {
                                PKind::While(_, b) | PKind::WhileLet { body: b, .. } => *b,
                                _ => return,
                            };
                            self.ctl.push(K::Loop {
                                node,
                                iter,
                                phase: Phase::Body,
                            });
                            self.mode = Mode::Eval(body);
                        }
                        Ok(false) => self.mode = Mode::Ret(Value::unit()),
                        Err(e) => self.fail(e),
                    }
                } else {
                    self.ctl.push(K::Loop { node, iter, phase });
                    self.loop_next(pn.loc);
                }
            }
            K::CallRet => {
                self.envs.pop();
                self.depth = self.depth.saturating_sub(1);
                self.mode = Mode::Ret(v);
            }
            K::Open(o) => self.open_harness(&o, Some(&v)),
            K::WithCfg(node) => self.open_block(node, &v),
            K::Override { node, f, args } => {
                self.pending_override = Some(v);
                self.dispatch(node, f, args);
            }
            K::FrameClose => {
                self.frames.close(self.steps);
                if self.refuses_secret(&v) {
                    let e = self.sees_refusal("a harness returned a secret");
                    return self.fail(e);
                }
                self.mode = Mode::Ret(v);
            }
            K::Entry { func, next, args } => {
                let pat = p.funcs[func as usize]
                    .params
                    .get(next as usize)
                    .map(|(pat, _)| *pat);
                let loc = Loc::default();
                match pat.map(|pat| self.matches(pat, &v, loc)) {
                    Some(Ok(true)) => self.bind_params(func, next + 1, args, loc),
                    Some(Ok(false)) | None => {
                        self.invalid("a default does not match its parameter", v.labels, loc);
                    }
                    Some(Err(e)) => self.fail(e),
                }
            }
            K::Task {
                task,
                class,
                n,
                loc,
            } => {
                self.ctl.push(K::Task {
                    task,
                    class,
                    n,
                    loc,
                });
                self.drive_task(Resume::Value(v));
            }
            K::Try(_) => self.mode = Mode::Ret(v),
            K::Par {
                node,
                next,
                mut vals,
            } => {
                self.level = self.level.saturating_sub(1);
                if let Err(f) = vals.push(&self.heap, v) {
                    return self.fault_at(f, Labels::NONE, p.node(node).loc);
                }
                let PKind::Par(xs) = &p.node(node).kind else {
                    return;
                };
                if let Some(x) = xs.get(next as usize) {
                    self.level += 1;
                    self.frames.set_branch(next);
                    self.ctl.push(K::Par {
                        node,
                        next: next + 1,
                        vals,
                    });
                    self.mode = Mode::Eval(*x);
                } else {
                    self.frames.leave_branch();
                    self.finish(node, vals.into_vals());
                }
            }
            K::Race { .. } => {
                // The first branch to finish successfully wins; the rest never start.
                self.level = self.level.saturating_sub(1);
                self.frames.leave_branch();
                self.mode = Mode::Ret(v);
            }
            K::Const(i) => {
                self.envs.pop();
                self.consts[i as usize] = Some(v.clone());
                self.mode = Mode::Ret(v);
            }
            K::PromptRoot => {
                let sections = self.prompts.pop().unwrap_or_default();
                match Value::new_prompt(&self.heap, sections) {
                    Ok(v) => self.mode = Mode::Ret(v),
                    Err(f) => self.fault_at(f, Labels::NONE, Loc::default()),
                }
            }
            K::PromptRun { list, idx } => self.prompt_next(list, idx),
            K::Section {
                list,
                idx,
                mut vals,
            } => {
                if let Err(f) = vals.push(&self.heap, v) {
                    return self.fault_at(f, Labels::NONE, Loc::default());
                }
                self.section_next(list, idx, vals);
            }
            K::PromptIf(then, else_) => match v.as_bool() {
                Some(true) => self.push_prompt_run(then),
                Some(false) => self.push_prompt_run(else_),
                None => self.invalid("a prompt `if` that is not a Bool", v.labels, Loc::default()),
            },
            K::PromptForInit { list, idx } => {
                let Some(PPrompt::For(pat, _, body)) = p.prompts[list as usize].get(idx as usize)
                else {
                    return;
                };
                let iter = match &v.repr {
                    Repr::List(l) => Iter::List(l.cursor(), v.labels),
                    Repr::Map(m) => Iter::Map(m.cursor(), v.labels),
                    Repr::Range(r) => Iter::Range {
                        next: i128::from(r.start),
                        end: i128::from(r.end) + i128::from(r.inclusive),
                        labels: v.labels,
                    },
                    _ => {
                        return self.invalid(
                            "a prompt `for` over something that is not iterable",
                            v.labels,
                            Loc::default(),
                        );
                    }
                };
                self.ctl.push(K::PromptFor {
                    pat: *pat,
                    body: *body,
                    iter,
                });
                self.prompt_for_next();
            }
            K::PromptFor { pat, body, iter } => {
                self.ctl.push(K::PromptFor { pat, body, iter });
                self.prompt_for_next();
            }
            K::Effect => self.invalid("an effect frame out of order", Labels::NONE, Loc::default()),
        }
    }

    /// The next iteration of the loop on top of the stack.
    fn loop_next(&mut self, loc: Loc) {
        if let Err(e) = self.charge(1, loc) {
            return self.fail(e);
        }
        let Some(K::Loop { node, iter, phase }) = self.ctl.last_mut() else {
            return;
        };
        let node = *node;
        let p = Arc::clone(&self.p);
        let pn = p.node(node);
        let item = match iter {
            Iter::List(c, labels) => c.next().map(|v| v.with_labels(*labels)),
            Iter::Map(c, labels) => {
                let labels = *labels;
                match c.next() {
                    Some((k, v)) => match Value::new_tuple(
                        &self.heap,
                        vec![k.with_labels(labels), v.with_labels(labels)],
                    ) {
                        Ok(t) => Some(t),
                        Err(f) => return self.fault_at(f, labels, loc),
                    },
                    None => None,
                }
            }
            Iter::Range { next, end, labels } => {
                if *next < *end {
                    let v = i64::try_from(*next).unwrap_or(i64::MAX);
                    *next += 1;
                    Some(Value::int(v).with_labels(*labels))
                } else {
                    None
                }
            }
            Iter::Repeat(left) => {
                if *left > 0 {
                    *left -= 1;
                    Some(Value::unit())
                } else {
                    None
                }
            }
            Iter::Forever => Some(Value::unit()),
            Iter::While => {
                *phase = Phase::Cond;
                let cond = match &pn.kind {
                    PKind::While(c, _) => *c,
                    PKind::WhileLet { value, .. } => *value,
                    _ => return,
                };
                self.mode = Mode::Eval(cond);
                return;
            }
        };
        let Some(item) = item else {
            self.ctl.pop();
            self.mode = Mode::Ret(Value::unit());
            return;
        };
        match &pn.kind {
            PKind::For(pat, _, body) => match self.matches(*pat, &item, loc) {
                Ok(true) => self.mode = Mode::Eval(*body),
                Ok(false) => self.invalid("a `for` pattern did not match", item.labels, loc),
                Err(e) => self.fail(e),
            },
            PKind::Repeat(_, body) | PKind::Loop(body) => self.mode = Mode::Eval(*body),
            _ => self.invalid("a loop frame on something else", Labels::NONE, loc),
        }
    }

    /// Tries arms from `arm` on, for a `match` or a `catch`.
    fn match_arm(&mut self, node: NodeId, value: Value, mut arm: u32, catch: bool) {
        let p = Arc::clone(&self.p);
        let loc = p.node(node).loc;
        loop {
            let Some(a) = arm_of(&p, node, arm, catch) else {
                if catch {
                    // No arm caught it: it propagates.
                    let e = RtError {
                        value,
                        class: ErrClass::Catchable,
                        scope: 0,
                    };
                    return self.fail(e);
                }
                return self.invalid("no `match` arm matched", value.labels, loc);
            };
            match self.matches(a.pat, &value, loc) {
                Ok(true) => {
                    if let Some(g) = a.guard {
                        self.ctl.push(K::Guard {
                            node,
                            value,
                            arm,
                            catch,
                        });
                        self.mode = Mode::Eval(g);
                    } else {
                        self.mode = Mode::Eval(a.body);
                    }
                    return;
                }
                Ok(false) => arm += 1,
                Err(e) => return self.fail(e),
            }
        }
    }

    // ---- prompts ----

    fn push_prompt_run(&mut self, list: u32) {
        self.ctl.push(K::PromptRun { list, idx: 0 });
        self.mode = Mode::Ret(Value::unit());
    }

    /// Continues the prompt part list `list` at `idx` (the frame was popped).
    fn prompt_next(&mut self, list: u32, idx: u32) {
        let p = Arc::clone(&self.p);
        let Some(part) = p.prompts[list as usize].get(idx as usize) else {
            // This list is done: return to the frame below.
            self.mode = Mode::Ret(Value::unit());
            return;
        };
        match part {
            PPrompt::Format(t) => {
                match Section::new(
                    &self.heap,
                    ratio_ir::SectionKind::Data,
                    Some(Arc::clone(t)),
                    Box::default(),
                ) {
                    Ok(section) => {
                        if let Some(acc) = self.prompts.last_mut() {
                            acc.push(section);
                        }
                    }
                    Err(f) => return self.fault_at(f, Labels::NONE, Loc::default()),
                }
                self.ctl.push(K::PromptRun { list, idx: idx + 1 });
                self.mode = Mode::Ret(Value::unit());
            }
            PPrompt::Section(..) => {
                self.ctl.push(K::PromptRun { list, idx: idx + 1 });
                self.section_next(list, idx, Held::new());
            }
            PPrompt::If(c, then, else_) => {
                self.ctl.push(K::PromptRun { list, idx: idx + 1 });
                self.ctl.push(K::PromptIf(*then, *else_));
                self.mode = Mode::Eval(*c);
            }
            PPrompt::For(_, iter, _) => {
                self.ctl.push(K::PromptRun { list, idx: idx + 1 });
                self.ctl.push(K::PromptForInit { list, idx });
                self.mode = Mode::Eval(*iter);
            }
        }
    }

    fn section_next(&mut self, list: u32, idx: u32, vals: Held) {
        let p = Arc::clone(&self.p);
        let Some(PPrompt::Section(kind, xs)) = p.prompts[list as usize].get(idx as usize) else {
            return;
        };
        if let Some(x) = xs.get(vals.len()) {
            self.ctl.push(K::Section { list, idx, vals });
            self.mode = Mode::Eval(*x);
            return;
        }
        let vals = vals.into_vals();
        let loc = p.node(xs.first().copied().unwrap_or(0)).loc;
        let mut built: Vec<Section> = Vec::new();
        // `include` splices the included prompt's sections: copying them is charged, steps in
        // proportion to what it copies and heap for each section.
        if *kind == ratio_ir::SectionKind::Include
            && let Some(Repr::Prompt(inner)) = vals.first().map(|v| &v.repr)
        {
            let labels = vals.first().map_or(Labels::NONE, |v| v.labels);
            let work = inner
                .sections
                .iter()
                .fold(0u64, |w, s| w.saturating_add(1 + s.values.len() as u64));
            if let Err(e) = self.charge(steps_for(work), loc) {
                return self.fail(e);
            }
            for s in &inner.sections {
                let values = s
                    .values
                    .iter()
                    .map(|v| v.clone().with_labels(labels))
                    .collect();
                match Section::new(&self.heap, s.kind, s.format.clone(), values) {
                    Ok(section) => built.push(section),
                    Err(f) => return self.fault_at(f, labels, loc),
                }
            }
        } else {
            match Section::new(&self.heap, *kind, None, vals.into_boxed_slice()) {
                Ok(section) => built.push(section),
                Err(f) => return self.fault_at(f, Labels::NONE, loc),
            }
        }
        if let Some(acc) = self.prompts.last_mut() {
            acc.extend(built);
        }
        self.mode = Mode::Ret(Value::unit());
    }

    fn prompt_for_next(&mut self) {
        let loc = Loc::default();
        if let Err(e) = self.charge(1, loc) {
            return self.fail(e);
        }
        let Some(K::PromptFor { pat, body, iter }) = self.ctl.last_mut() else {
            return;
        };
        let (pat, body) = (*pat, *body);
        let item = match iter {
            Iter::List(c, labels) => c.next().map(|v| v.with_labels(*labels)),
            Iter::Map(c, labels) => {
                let labels = *labels;
                c.next().and_then(|(k, v)| {
                    Value::new_tuple(
                        &self.heap,
                        vec![k.with_labels(labels), v.with_labels(labels)],
                    )
                    .ok()
                })
            }
            Iter::Range { next, end, labels } => {
                if *next < *end {
                    let v = i64::try_from(*next).unwrap_or(i64::MAX);
                    *next += 1;
                    Some(Value::int(v).with_labels(*labels))
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(item) = item else {
            self.ctl.pop();
            self.mode = Mode::Ret(Value::unit());
            return;
        };
        match self.matches(pat, &item, loc) {
            Ok(true) => self.push_prompt_run(body),
            Ok(false) => self.invalid("a prompt `for` pattern did not match", item.labels, loc),
            Err(e) => self.fail(e),
        }
    }

    // ---- unwinding ----

    #[allow(clippy::too_many_lines, reason = "one arm per kind of frame")]
    fn unwind(&mut self, s: Signal) {
        loop {
            let Some(k) = self.ctl.pop() else {
                match s {
                    Signal::Raise(e) => self.outcome = Some(Outcome::Raised(e)),
                    Signal::Return(v) => self.outcome = Some(Outcome::Returned(v)),
                    Signal::Break(_) | Signal::Continue => {
                        let e = raise(
                            &self.p,
                            Kind::Invalid,
                            "`break` or `continue` outside a loop",
                            Labels::NONE,
                            Loc::default(),
                        );
                        self.outcome = Some(Outcome::Raised(e));
                    }
                }
                self.mode = Mode::Done;
                return;
            };
            match k {
                K::Block { defers, .. } if !defers.is_empty() => {
                    return self.exit_scope(defers, Exit::Signal(s));
                }
                K::Defers { pending, exit } => {
                    // A `defer` block raised or jumped: the first error is kept, and the
                    // remaining defers still run.
                    let exit = match (exit, s) {
                        (Exit::Signal(Signal::Raise(first)), _) => {
                            Exit::Signal(Signal::Raise(first))
                        }
                        (_, s) => Exit::Signal(s),
                    };
                    self.ctl.push(K::Defers { pending, exit });
                    return self.next_defer();
                }
                K::Loop { node, iter, phase } => match s {
                    Signal::Break(v) => {
                        let value = if matches!(self.p.node(node).kind, PKind::Loop(_)) {
                            v
                        } else {
                            Value::unit()
                        };
                        self.mode = Mode::Ret(value);
                        return;
                    }
                    Signal::Continue => {
                        let loc = self.p.node(node).loc;
                        self.ctl.push(K::Loop { node, iter, phase });
                        return self.loop_next(loc);
                    }
                    other => return self.unwind_on(other),
                },
                K::CallRet => {
                    self.envs.pop();
                    self.depth = self.depth.saturating_sub(1);
                    match s {
                        Signal::Return(v) => {
                            self.mode = Mode::Ret(v);
                            return;
                        }
                        Signal::Raise(e) => return self.unwind_on(Signal::Raise(e)),
                        Signal::Break(_) | Signal::Continue => {
                            let e = raise(
                                &self.p,
                                Kind::Invalid,
                                "`break` or `continue` crossed a call",
                                Labels::NONE,
                                Loc::default(),
                            );
                            return self.unwind_on(Signal::Raise(e));
                        }
                    }
                }
                K::Const(_) => {
                    self.envs.pop();
                }
                K::Task {
                    task,
                    class,
                    n,
                    loc,
                } => {
                    if let Signal::Raise(e) = s {
                        if task.catches() && e.class == ErrClass::Catchable {
                            self.ctl.push(K::Task {
                                task,
                                class,
                                n,
                                loc,
                            });
                            return self.drive_task(Resume::Raised(e));
                        }
                        return self.unwind_on(Signal::Raise(e));
                    }
                }
                K::Try(node) => {
                    if let Signal::Raise(e) = s {
                        if e.class != ErrClass::Catchable {
                            return self.unwind_on(Signal::Raise(e));
                        }
                        let p = Arc::clone(&self.p);
                        let PKind::Try(_, catch) = &p.node(node).kind else {
                            return;
                        };
                        match catch {
                            PCatch::Bind(slot, body) => {
                                self.set(*slot, e.value);
                                self.mode = Mode::Eval(*body);
                            }
                            PCatch::Arms(_) => self.match_arm(node, e.value, 0, true),
                        }
                        return;
                    }
                }
                K::Par { .. } => {
                    self.level = self.level.saturating_sub(1);
                    self.frames.leave_branch();
                }
                K::FrameClose => {
                    let index = self.frames.index();
                    self.frames.close(self.steps);
                    return self.unwind_on(self.crossing(s, index));
                }
                K::Race { node, next, first } => {
                    self.level = self.level.saturating_sub(1);
                    if let Signal::Raise(e) = s {
                        if e.class != ErrClass::Catchable {
                            self.frames.leave_branch();
                            return self.unwind_on(Signal::Raise(e));
                        }
                        // A failed branch: try the next; when all fail, raise the failure of
                        // the first branch in written order.
                        let first = first.unwrap_or(e);
                        let p = Arc::clone(&self.p);
                        let PKind::Race(xs) = &p.node(node).kind else {
                            return;
                        };
                        if let Some(x) = xs.get(next as usize) {
                            self.level += 1;
                            self.frames.set_branch(next);
                            self.ctl.push(K::Race {
                                node,
                                next: next + 1,
                                first: Some(first),
                            });
                            self.mode = Mode::Eval(*x);
                        } else {
                            self.frames.leave_branch();
                            self.fail(first);
                        }
                        return;
                    }
                    self.frames.leave_branch();
                }
                K::PromptRoot => {
                    self.prompts.pop();
                }
                _ => {}
            }
        }
    }

    fn unwind_on(&mut self, s: Signal) {
        self.mode = Mode::Unwind(s);
    }

    // ---- frames (stage 10) ----

    /// Whether `v` holds a secret and the current frame's `sees` lacks `secrets`.
    fn refuses_secret(&self, v: &Value) -> bool {
        !self.frames.top().ctx.scope.sees.has(SeesItem::Secrets) && crate::sink::check(v).is_err()
    }

    fn sees_refusal(&self, what: &str) -> RtError {
        raise(
            &self.p,
            Kind::Denied("sees"),
            &format!("{what} into a frame whose `sees` lacks `secrets`"),
            Labels::NONE,
            Loc::default(),
        )
    }

    /// A signal leaving the frame that was `index` (now closed): a `Budget` of that frame's
    /// own becomes catchable by its caller (chapter 03), and a secret the caller may not see
    /// is refused.
    fn crossing(&self, s: Signal, index: u32) -> Signal {
        match s {
            Signal::Raise(mut e) => {
                if e.class == ErrClass::Budget && e.scope == index && index > 0 {
                    e.class = ErrClass::Catchable;
                    e.scope = 0;
                }
                if e.class == ErrClass::Catchable && self.refuses_secret(&e.value) {
                    Signal::Raise(self.sees_refusal("an error carrying a secret crossed"))
                } else {
                    Signal::Raise(e)
                }
            }
            Signal::Return(v) if self.refuses_secret(&v) => {
                Signal::Raise(self.sees_refusal("a secret was returned"))
            }
            Signal::Break(v) if self.refuses_secret(&v) => {
                Signal::Raise(self.sees_refusal("a secret was carried out of a block"))
            }
            s => s,
        }
    }

    /// Opens a frame of `kind` with `request`, then runs `body` in it; a refusal raises in
    /// the caller's frame.
    fn open_frame(
        &mut self,
        kind: FrameKind,
        (request, settings): (Request, Settings),
        name: &Arc<str>,
        site: u32,
        loc: Loc,
    ) -> bool {
        match self
            .frames
            .open(self.steps, kind, &request, settings, name, site, loc)
        {
            Ok(()) => true,
            Err(o) => {
                let mut e = if o.refusal.depth {
                    raise(
                        &self.p,
                        Kind::Budget("depth"),
                        &o.refusal.message,
                        Labels::NONE,
                        loc,
                    )
                } else {
                    raise(
                        &self.p,
                        Kind::Denied("attenuation"),
                        &o.refusal.message,
                        Labels::NONE,
                        loc,
                    )
                };
                if o.refusal.depth {
                    e.scope = o.scope;
                }
                self.fail(e);
                false
            }
        }
    }

    /// Reads a configuration value (`None`: nothing configured), or raises `Invalid`.
    fn config(&mut self, v: Option<&Value>, loc: Loc) -> Option<(Request, Settings)> {
        let Some(v) = v else {
            return Some((Request::default(), Settings::default()));
        };
        match crate::frame::read_config(&self.p, v) {
            Ok(c) => Some(c),
            Err(m) => {
                let e = raise(&self.p, Kind::Invalid, &m, v.labels, loc);
                self.fail(e);
                None
            }
        }
    }

    /// Opens a harness's frame (the declared configuration evaluated, when it has one) and
    /// runs its body.
    fn open_harness(&mut self, o: &Opening, declared: Option<&Value>) {
        let p = Arc::clone(&self.p);
        let f = &p.funcs[o.func as usize];
        let Some((req, settings)) = self.config(declared, o.loc) else {
            return;
        };
        let Some((over, over_settings)) = self.config(o.overrides.as_ref(), o.loc) else {
            return;
        };
        let merged = (Request::merge(req, over), over_settings.over(&settings));
        if !self.open_frame(o.kind, merged, &f.name, o.site, o.loc) {
            return;
        }
        if o.labelled && !self.frames.top().ctx.scope.sees.has(SeesItem::Secrets) {
            self.frames.close(self.steps);
            let e = self.sees_refusal("a secret argument was passed");
            return self.fail(e);
        }
        self.ctl.push(K::FrameClose);
        self.mode = Mode::Eval(f.body);
    }

    /// Opens a `with` block's frame and runs its body.
    fn open_block(&mut self, node: NodeId, cfg: &Value) {
        let p = Arc::clone(&self.p);
        let n = p.node(node);
        let PKind::With(_, body) = n.kind else {
            return self.invalid("a `with` frame on something else", Labels::NONE, n.loc);
        };
        let Some(config) = self.config(Some(cfg), n.loc) else {
            return;
        };
        if self.open_frame(FrameKind::Block, config, &Arc::from("with"), n.site, n.loc) {
            self.ctl.push(K::FrameClose);
            self.mode = Mode::Eval(body);
        }
    }
}

fn arm_of(p: &Prepared, node: NodeId, arm: u32, catch: bool) -> Option<&crate::prepare::PArm> {
    match &p.node(node).kind {
        PKind::Match(_, arms) if !catch => arms.get(arm as usize),
        PKind::Try(_, PCatch::Arms(arms)) if catch => arms.get(arm as usize),
        _ => None,
    }
}

/// The arguments in parameter order (`None` for a default) from those written.
fn arrange(order: &[ArgFrom], written: Vec<Value>) -> Vec<Option<Value>> {
    let mut written: Vec<Option<Value>> = written.into_iter().map(Some).collect();
    order
        .iter()
        .map(|o| match o {
            ArgFrom::Written(i) => written.get_mut(*i as usize).and_then(Option::take),
            ArgFrom::Default => None,
        })
        .collect()
}
