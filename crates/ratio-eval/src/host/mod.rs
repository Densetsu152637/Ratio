//! The `Host` boundary (chapter 10, "The `Host` trait"; stage 11a).
//!
//! Everything a program does outside the interpreter goes through a [`Host`]: model and tool
//! calls, approvals, the engine's conversations, agents, memory and session, frames, the
//! clock and timers, and the output-only events (`log.*`, `session.emit`, …). The trait takes
//! `&self` or `Arc<Self>`: concurrent branches hold the host at once and their effects run
//! concurrently, so an implementation uses interior mutability and serialises per resource
//! (per conversation) itself.
//!
//! **Executors.** The evaluation, and so every host future and `timer`, may be polled from any
//! executor, not only the host's own (a test drives it with [`crate::block_on`]). A host built
//! on tokio must therefore not rely on a tokio context when its futures are polled: it spawns
//! its own work on its runtime and hands back a `Send` future that waits for the result (and
//! a timer that does not need a tokio context to fire). [`Sleep`] is a timer on the standard
//! library alone.
//!
//! **Frames and their origin.** Every frame carries its [`FrameOrigin`](crate::frame::FrameOrigin),
//! filled from the program's IR: an entry; a `with` block at a site (its package's IR hash and
//! node); or a harness, by package, the package's IR hash and the harness's key. A host checks
//! it against what it loaded, so the kind is no longer Ratio's word alone.
//!
//! **Async without a runtime.** The asynchronous methods take `self: Arc<Self>` and return a
//! [`HostFuture`], a boxed `Send + 'static` future. The trait is therefore object-safe (an
//! engine holds an `Arc<dyn Host>`, and the wrappers [`Guard`] and [`crate::tape::TapeHost`]
//! hold one too), the returned future borrows nothing (the scheduler keeps many in flight
//! next to the machines that wait for them, and a host may spawn them on its own runtime),
//! and no async runtime or `async_trait` crate is needed: the evaluation polls them with the
//! waker of whatever executor polls it.
//!
//! **Labels.** Every value crossing the boundary carries its secrecy labels: [`Value`]s carry
//! their own, text is [`Labelled`]. The interpreter wraps every host in a [`Guard`] that no host
//! can opt out of: it refuses a labelled value at every secrecy sink of chapter 03 before the
//! host sees it, and sets the per-call secrecy flag ([`ToolInvocation::secret`],
//! [`Operation::secret`], [`ModelRequest::secret`]) from the labels, so the engine can enforce
//! the sinks a second time at dispatch. A host must enforce them from the flag on its own:
//! the contract suite ([`crate::host_tests`], feature `host-tests`) drives it without the
//! guard.
//!
//! **Effects, cancellation and deadlines.** Every asynchronous call receives its
//! [`EffectCtx`]: the effect's key, a cancellation token and its deadline. The host bounds the
//! call by the deadline. When the token fires it ends the call by its tool's class
//! ([`Host::tool_class`]) and reports how it ended ([`HostError::Cancelled`] with a
//! [`CancelOutcome`]): an `abort` call is stopped and acknowledged (`aborted`, or `abandoned`
//! once it reached a remote server); a `drain` call runs to its end within its declared bound
//! (at most [`MAX_DRAIN`]) and reports `drained`; a call still waiting for an approval or a
//! hook is `withdrawn`, atomically with the user's answer, and never runs.

pub(crate) mod bridge;
mod guard;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use guard::{Guard, SinkRule, operation_rule, sink_rule};

use crate::frame::{Card, FrameCtx, Location};
use crate::labels::Labels;
use crate::machine::CancelToken;
use crate::value::Value;

/// The version of the `Host` trait and every type that crosses it (chapter 10, "Versions").
/// A host asserts the version it was written for at compile time:
///
/// ```
/// const _: () = assert!(ratio_eval::host::HOST_API_VERSION == 1);
/// ```
pub const HOST_API_VERSION: u32 = 1;

/// The longest a `drain`-class tool may declare it runs once started (chapter 10, host
/// contract): the drain fence waits for no call longer than this.
pub const MAX_DRAIN: Duration = Duration::from_secs(3600);

/// What an asynchronous host method returns: a boxed future that borrows nothing.
pub type HostFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A value with its secrecy labels.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Labelled<T> {
    pub value: T,
    pub labels: Labels,
}

impl<T> Labelled<T> {
    /// An unlabelled value.
    pub fn plain(value: T) -> Labelled<T> {
        Labelled {
            value,
            labels: Labels::NONE,
        }
    }

    pub fn new(value: T, labels: Labels) -> Labelled<T> {
        Labelled { value, labels }
    }

    /// Whether the value is secret.
    pub fn is_labelled(&self) -> bool {
        !self.labels.is_empty()
    }
}

/// An effect's key (chapter 01, "The effect tape"): the frame path, the branch path within
/// the frame, the site id (the package's IR hash and the node index) and the occurrence of
/// the site in that frame and branch path. It depends only on the program and its data.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectKey {
    pub frame: Arc<str>,
    pub branch: Arc<str>,
    pub site: ([u8; 32], u32),
    pub occurrence: u32,
}

impl fmt::Display for EffectKey {
    /// `turn/critic@s41#0/c12#0.b2/e30#1` (the site's node, then its occurrence).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.frame)?;
        if !self.branch.is_empty() {
            write!(f, "/{}", self.branch)?;
        }
        write!(f, "/e{}#{}", self.site.1, self.occurrence)
    }
}

/// What every asynchronous host call receives with its request.
#[derive(Clone, Debug)]
pub struct EffectCtx {
    pub key: EffectKey,
    /// Fires when the effect's scope is cancelled: the host ends the call by its class and
    /// reports the outcome ([`HostError::Cancelled`]). [`CancelToken::cancelled`] is a future
    /// that resolves when it fires.
    pub cancel: CancelToken,
    /// The earliest enclosing `with timeout` deadline and the end of the frame's `wall`
    /// budget: the host bounds the call by it (a request timeout; `run_command`'s effective
    /// timeout). `None`: unbounded by the program.
    pub deadline: Option<Instant>,
}

/// A tool's cancellation class (chapter 06, `tools`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelClass {
    /// Stopped at once and acknowledged.
    Abort,
    /// Runs to its end (or its deadline, or its declared bound); its result is discarded.
    Drain,
}

/// A tool's class, as the host's table gives it: an unlisted tool is `drain`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToolClass {
    pub class: CancelClass,
    /// For a `drain` tool, the longest it runs once started (at most [`MAX_DRAIN`]); the host
    /// fails the call there. `None` for `abort` tools.
    pub max_running: Option<Duration>,
    /// Whether the call reaches a remote server (`web_*`, MCP): cancelled, it is `abandoned`.
    pub remote: bool,
}

impl ToolClass {
    /// The class of an unlisted tool: `drain`, bounded at [`MAX_DRAIN`].
    pub const UNLISTED: ToolClass = ToolClass {
        class: CancelClass::Drain,
        max_running: Some(MAX_DRAIN),
        remote: false,
    };

    /// An `abort` tool.
    #[must_use]
    pub const fn abort(remote: bool) -> ToolClass {
        ToolClass {
            class: CancelClass::Abort,
            max_running: None,
            remote,
        }
    }

    /// A `drain` tool bounded at `max` (clamped to [`MAX_DRAIN`]).
    #[must_use]
    pub fn drain(max: Duration) -> ToolClass {
        ToolClass {
            class: CancelClass::Drain,
            max_running: Some(max.min(MAX_DRAIN)),
            remote: false,
        }
    }

    /// Whether the class meets the host contract: a `drain` tool declares a bound of at most
    /// [`MAX_DRAIN`].
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        match self.class {
            CancelClass::Abort => true,
            CancelClass::Drain => self.max_running.is_some_and(|d| d <= MAX_DRAIN),
        }
    }
}

/// How a cancelled call ended (chapter 03, "Cancellation"): never returned to the program,
/// recorded on the turn sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelOutcome {
    /// An `abort` call, stopped and acknowledged.
    Aborted,
    /// An `abort` call that had reached a remote server, which may still act on it.
    Abandoned,
    /// A `drain` call that ran to its end; its result is discarded.
    Drained,
    /// A call still waiting for an approval or a hook: it never ran.
    Withdrawn,
}

impl CancelOutcome {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CancelOutcome::Aborted => "aborted",
            CancelOutcome::Abandoned => "abandoned",
            CancelOutcome::Drained => "drained",
            CancelOutcome::Withdrawn => "withdrawn",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<CancelOutcome> {
        [
            CancelOutcome::Aborted,
            CancelOutcome::Abandoned,
            CancelOutcome::Drained,
            CancelOutcome::Withdrawn,
        ]
        .into_iter()
        .find(|o| o.name() == name)
    }
}

/// What refused a call (`Denied { by }`, chapter 03).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeniedBy {
    Rule,
    Mode,
    Floor,
    User,
    Hook,
    Profile,
    Attenuation,
    Unattended,
    Secrecy,
    Sees,
}

impl DeniedBy {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            DeniedBy::Rule => "rule",
            DeniedBy::Mode => "mode",
            DeniedBy::Floor => "floor",
            DeniedBy::User => "user",
            DeniedBy::Hook => "hook",
            DeniedBy::Profile => "profile",
            DeniedBy::Attenuation => "attenuation",
            DeniedBy::Unattended => "unattended",
            DeniedBy::Secrecy => "secrecy",
            DeniedBy::Sees => "sees",
        }
    }
}

/// Why a host call did not give a result.
#[derive(Clone, Debug)]
pub enum HostError {
    /// The call was refused (`Denied { by }`).
    Denied {
        by: DeniedBy,
        message: String,
    },
    /// A tool ran and reported failure (`ToolFailed`); `data` is the kind's details.
    ToolFailed {
        message: Labelled<String>,
        data: Value,
    },
    /// An endpoint error, an empty reply or a refusal (`ModelFailed`).
    ModelFailed {
        message: Labelled<String>,
    },
    /// The call's deadline, or its declared bound, elapsed (`Timeout`).
    Timeout {
        message: String,
    },
    NotFound {
        message: String,
    },
    Unsupported {
        message: String,
    },
    Invalid {
        message: String,
    },
    RateLimited {
        message: String,
    },
    /// The call's token fired, and this is how the host ended it.
    Cancelled(CancelOutcome),
}

impl HostError {
    /// `Unsupported` with a message.
    pub fn unsupported(message: impl Into<String>) -> HostError {
        HostError::Unsupported {
            message: message.into(),
        }
    }

    /// `Denied { by: secrecy }`.
    pub fn secrecy(message: impl Into<String>) -> HostError {
        HostError::Denied {
            by: DeniedBy::Secrecy,
            message: message.into(),
        }
    }
}

/// A model, by the name a program gives (`"local:qwen"`, a configured name).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelRef(pub Arc<str>);

/// What [`Host::resolve_model`] says about a model: its location (a secrecy sink when
/// `cloud`) and the card admission reads (maximum output, window, prices).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCard {
    pub name: Arc<str>,
    pub location: Location,
    pub card: Card,
}

/// A model request. The prompt is the program's `Prompt` (or text) value, with its labels;
/// rendering and the framing rules are stage 12's (chapter 05).
#[derive(Clone, Debug)]
pub struct ModelRequest {
    /// The built-in (`model.generate`, …).
    pub builtin: Arc<str>,
    pub model: ModelRef,
    pub prompt: Value,
    /// The effective `max_tokens` the admission reserved (chapter 04): the host sends it.
    pub max_tokens: Option<u64>,
    /// The other arguments, by parameter name.
    pub options: Vec<(Arc<str>, Value)>,
    /// Whether any part of the request is labelled.
    pub secret: bool,
}

/// What a model call used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Micro-units of money.
    pub cost: u64,
}

/// A model reply.
#[derive(Clone, Debug)]
pub struct ModelResponse {
    pub text: Labelled<String>,
    pub usage: Usage,
}

/// What [`Host::count_tokens`] counts.
#[derive(Clone, Debug)]
pub enum Countable {
    Text(Labelled<String>),
    Prompt(Value),
}

/// A tool call: the tool's name (`read_file`, `mcp__server__tool`), the built-in that made it,
/// the arguments by parameter name with their labels, and the per-call secrecy flag.
#[derive(Clone, Debug)]
pub struct ToolInvocation {
    pub tool: Arc<str>,
    pub builtin: Arc<str>,
    pub args: Vec<(Arc<str>, Value)>,
    /// Whether any argument is labelled. The engine enforces its sinks from it.
    pub secret: bool,
    /// Whether the call is made from a `defer` block (exempt from cancellation and from the
    /// drain fence, chapter 03).
    pub in_defer: bool,
}

impl ToolInvocation {
    /// The argument named `name`.
    #[must_use]
    pub fn arg(&self, name: &str) -> Option<&Value> {
        self.args.iter().find(|(n, _)| &**n == name).map(|(_, v)| v)
    }
}

/// A tool's result: the value the built-in returns (built with [`Value`]'s constructors,
/// labelled by the host's source rules).
#[derive(Clone, Debug)]
pub struct ToolOutcome {
    pub value: Value,
}

/// An engine-owned conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConvId(pub u64);

/// Which conversation [`Host::conversation`] names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvSelector {
    /// The frame's current conversation.
    Current,
    /// The agent's main conversation.
    Main,
}

/// A 32-byte digest (BLAKE3 unless said otherwise).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// Lower-case hex.
    #[must_use]
    pub fn hex(&self) -> String {
        crate::tape::hex(&self.0)
    }
}

/// A chat request on a conversation.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub builtin: Arc<str>,
    pub model: Option<ModelRef>,
    pub max_tokens: Option<u64>,
    pub options: Vec<(Arc<str>, Value)>,
    pub secret: bool,
}

/// Which family of engine operations an [`Operation`] belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpFamily {
    Agents,
    Memory,
    Session,
    Context,
    Eval,
    Time,
    Meta,
    Other,
}

impl OpFamily {
    /// The family of a built-in, by its module.
    #[must_use]
    pub fn of(builtin: &str) -> OpFamily {
        match builtin.split('.').next().unwrap_or("") {
            "agents" => OpFamily::Agents,
            "memory" => OpFamily::Memory,
            "session" => OpFamily::Session,
            "context" => OpFamily::Context,
            "eval" => OpFamily::Eval,
            "time" => OpFamily::Time,
            "meta" => OpFamily::Meta,
            _ => OpFamily::Other,
        }
    }
}

/// An engine operation not yet given a typed request of its own (agents, memory, session,
/// context, eval, time): the built-in, its arguments by parameter name, and the secrecy flag.
/// The stages that implement these built-ins type them (each such change bumps
/// [`HOST_API_VERSION`]).
#[derive(Clone, Debug)]
pub struct Operation {
    pub family: OpFamily,
    pub builtin: Arc<str>,
    pub args: Vec<(Arc<str>, Value)>,
    pub secret: bool,
}

/// What an approval is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApprovalKind {
    /// `declassify(x, because)`: always asks; refused unattended.
    Declassify,
    /// `session.request_approval(summary, detail)`.
    Request,
}

/// An approval the user answers.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub kind: ApprovalKind,
    pub summary: Labelled<String>,
    pub detail: Option<Value>,
}

/// An output-only event (`log.*`, `trace.*`, `session.emit`, `session.stream`,
/// `session.notify`): no result, never on the tape, suppressed in replay.
#[derive(Clone, Debug)]
pub struct RatioEvent {
    pub builtin: Arc<str>,
    pub key: EffectKey,
    pub args: Vec<(Arc<str>, Value)>,
    /// Whether any argument is labelled: the host redacts it (`[secret: <digest>]`).
    pub secret: bool,
}

/// How a frame closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameOutcome {
    Returned,
    Raised,
    Cancelled,
}

/// The host: everything outside the interpreter (chapter 10).
///
/// The asynchronous methods return futures that borrow nothing ([`HostFuture`]); take what
/// you need from `self` (an `Arc`) and the arguments into the future. Every request carries
/// the frame it is made in ([`FrameCtx`]): a host checks its scope against **its own record**
/// of the frame (by [`FrameCtx::path`]), never trusting it to be narrow.
pub trait Host: Send + Sync + 'static {
    // ---- models ----

    /// A model call (`model.generate` and the other single-request built-ins).
    fn generate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ModelRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>>;

    /// Embeddings of each text.
    fn embed(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        texts: Vec<Labelled<String>>,
        model: Option<ModelRef>,
    ) -> HostFuture<Result<Vec<Vec<f32>>, HostError>>;

    /// Counts the input tokens of `what` for `frame`'s model (asynchronously: an exact
    /// tokenizer may be): an input of admission, recorded with it. A failure is the model
    /// call's error.
    fn count_tokens(
        self: Arc<Self>,
        frame: FrameCtx,
        what: Countable,
    ) -> HostFuture<Result<u64, HostError>>;

    /// The card of a model: its location, maximum output, window and prices.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown model.
    fn resolve_model(&self, frame: &FrameCtx, model: &ModelRef) -> Result<ModelCard, HostError>;

    // ---- tools ----

    /// A tool call. The host checks the frame's scope and the secrecy flag before its own
    /// checks, bounds the call by the deadline and honours the token by the tool's class.
    fn call_tool(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        call: ToolInvocation,
    ) -> HostFuture<Result<ToolOutcome, HostError>>;

    /// A tool's cancellation class (an unlisted tool is [`ToolClass::UNLISTED`]; a server's
    /// own read-only claim never makes a tool `abort`).
    fn tool_class(&self, frame: &FrameCtx, tool: &str) -> ToolClass;

    /// Whether `path` matches `ratio.secret_paths` (a write of secret content to such a path
    /// is not a sink, chapter 03).
    fn is_secret_path(&self, frame: &FrameCtx, path: &str) -> bool;

    /// The labels the host's source rules give a call's result (`ratio.secret_paths`, secret
    /// commands, private memory): a replay adds them to the recorded labels, so a tape
    /// recorded under weaker secrecy settings cannot launder a value the current ones label.
    fn source_labels(&self, frame: &FrameCtx, builtin: &str, args: &[(Arc<str>, Value)]) -> Labels;

    // ---- conversations ----

    /// A conversation handle.
    ///
    /// # Errors
    ///
    /// When the frame has no such conversation.
    fn conversation(&self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError>;

    /// The head of a conversation's chained digest, `d(k) = H(d(k−1), H(message k with its
    /// labels))` (chapter 01), in O(1).
    ///
    /// # Errors
    ///
    /// When the conversation does not exist.
    fn conversation_digest(&self, frame: &FrameCtx, conv: ConvId) -> Result<Digest, HostError>;

    /// The labels of a conversation (chapter 03: a conversation holding a labelled message is
    /// labelled, and a call on it is a sink as if its whole content were secret). The guard
    /// refuses a call on a labelled conversation to a hosted model.
    ///
    /// # Errors
    ///
    /// When the conversation does not exist (the guard then treats it as labelled).
    fn conversation_labels(&self, frame: &FrameCtx, conv: ConvId) -> Result<Labels, HostError>;

    /// A model call on a conversation.
    fn chat(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        conv: ConvId,
        req: ChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>>;

    // ---- frames ----

    /// A frame opens below `parent`: the host verifies, against its own record of the parent
    /// (looked up by `parent.path`, never from `parent.scope`), that `child.scope` is within
    /// it in every dimension (`frame::check_within`), and records the child.
    ///
    /// **The kind is checked through `child.origin`**, filled by Ratio from the program's IR
    /// and consistent with `child.kind`: a host verifies it against what it loaded. A
    /// [`FrameOrigin::Harness`](crate::frame::FrameOrigin::Harness) must name a harness the
    /// host loaded (its package, the package's IR hash and the harness's key); a
    /// [`FrameOrigin::Block`](crate::frame::FrameOrigin::Block) a site in a package the host
    /// loaded; an entry's frame carries its parent's path. So a harness call passed off as a
    /// `with` block (kind `Block`, harness `with`, the parent's depth, within its parent in
    /// every dimension) fails on its origin unless its site is one the host loaded. The
    /// contract suite checks each case, the consistent forgery included.
    ///
    /// # Errors
    ///
    /// `Denied { by: attenuation }` for a child not within its parent.
    fn open_frame(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError>;

    /// A frame closed.
    fn close_frame(&self, frame: &FrameCtx, outcome: FrameOutcome);

    // ---- the engine: agents, memory, session, context, eval, time ----

    /// An engine operation (see [`Operation`]).
    fn operate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        op: Operation,
    ) -> HostFuture<Result<Value, HostError>>;

    /// Asks the user. For `declassify` the answer is only whether they agreed: the
    /// interpreter itself removes the labels, from the argument alone.
    fn approve(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ApprovalRequest,
    ) -> HostFuture<Result<bool, HostError>>;

    // ---- recorded, authority-free ----

    /// The monotonic clock deadlines are measured on.
    fn now(&self) -> Instant;

    /// The turn's random seed, drawn once per turn and recorded.
    fn seed(&self) -> [u8; 32];

    /// A future that resolves at `at` (the scheduler's timeouts; there is no runtime here).
    fn timer(&self, at: Instant) -> HostFuture<()>;

    /// An output-only event. A host refuses a labelled `session.emit` from the secrecy flag
    /// (a sink) and redacts labelled logs, traces and streams.
    ///
    /// # Errors
    ///
    /// `Denied { by: secrecy }` for a secret at a sink.
    fn emit(&self, frame: &FrameCtx, event: RatioEvent) -> Result<(), HostError>;
}

/// A host that answers every effect with `Unsupported`: what [`crate::evaluate`] runs a pure
/// entry with.
#[derive(Debug, Default)]
pub struct NoHost;

fn unsupported<T: Send + 'static>(what: &str) -> HostFuture<Result<T, HostError>> {
    let e = HostError::unsupported(format!("`{what}` makes effects, which need a host"));
    Box::pin(async move { Err(e) })
}

impl Host for NoHost {
    fn generate(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        req: ModelRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        unsupported(&req.builtin)
    }

    fn embed(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        _: Vec<Labelled<String>>,
        _: Option<ModelRef>,
    ) -> HostFuture<Result<Vec<Vec<f32>>, HostError>> {
        unsupported("model.embed")
    }

    fn count_tokens(
        self: Arc<Self>,
        _: FrameCtx,
        _: Countable,
    ) -> HostFuture<Result<u64, HostError>> {
        Box::pin(async { Ok(0) })
    }

    fn resolve_model(&self, _: &FrameCtx, m: &ModelRef) -> Result<ModelCard, HostError> {
        Err(HostError::NotFound {
            message: format!("no model `{}` without a host", m.0),
        })
    }

    fn call_tool(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        call: ToolInvocation,
    ) -> HostFuture<Result<ToolOutcome, HostError>> {
        unsupported(&call.builtin)
    }

    fn tool_class(&self, _: &FrameCtx, _: &str) -> ToolClass {
        ToolClass::UNLISTED
    }

    fn is_secret_path(&self, _: &FrameCtx, _: &str) -> bool {
        false
    }

    fn source_labels(&self, _: &FrameCtx, _: &str, _: &[(Arc<str>, Value)]) -> Labels {
        Labels::NONE
    }

    fn conversation_labels(&self, _: &FrameCtx, _: ConvId) -> Result<Labels, HostError> {
        Err(HostError::unsupported("conversations need a host"))
    }

    fn conversation(&self, _: &FrameCtx, _: ConvSelector) -> Result<ConvId, HostError> {
        Err(HostError::unsupported("conversations need a host"))
    }

    fn conversation_digest(&self, _: &FrameCtx, _: ConvId) -> Result<Digest, HostError> {
        Err(HostError::unsupported("conversations need a host"))
    }

    fn chat(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        _: ConvId,
        req: ChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        unsupported(&req.builtin)
    }

    fn open_frame(&self, _: &FrameCtx, _: &FrameCtx) -> Result<(), HostError> {
        Ok(())
    }

    fn close_frame(&self, _: &FrameCtx, _: FrameOutcome) {}

    fn operate(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        op: Operation,
    ) -> HostFuture<Result<Value, HostError>> {
        unsupported(&op.builtin)
    }

    fn approve(
        self: Arc<Self>,
        _: FrameCtx,
        _: EffectCtx,
        req: ApprovalRequest,
    ) -> HostFuture<Result<bool, HostError>> {
        unsupported(match req.kind {
            ApprovalKind::Declassify => "declassify",
            ApprovalKind::Request => "session.request_approval",
        })
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn seed(&self) -> [u8; 32] {
        [0; 32]
    }

    fn timer(&self, at: Instant) -> HostFuture<()> {
        Box::pin(crate::host::sleep::Sleep::until(at))
    }

    fn emit(&self, _: &FrameCtx, _: RatioEvent) -> Result<(), HostError> {
        Ok(())
    }
}

pub mod sleep;

pub use sleep::Sleep;
