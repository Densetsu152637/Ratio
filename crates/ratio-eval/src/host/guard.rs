//! The sink guard (chapter 03, "Sinks"; chapter 10, "Security requirements by side").
//!
//! The interpreter wraps every host in a [`Guard`]; no host can opt out of it, because the
//! evaluation builds it around whatever host it is given. The guard refuses a labelled value at
//! every secrecy sink before the host sees it (`Denied { by: secrecy }`), recomputes the
//! per-call secrecy flag from the labels (never trusting a flag it was handed), and checks
//! every frame it forwards against its parent with [`check_within`] as defence in depth. The
//! engine enforces the same sinks again at dispatch from the flag, which catches a bug here
//! (but not a dropped label: propagation is the interpreter's alone).
//!
//! Where the guard cannot know what the engine knows, it refuses: an MCP tool's server may be
//! local, a `media_*` backend may be local, a model whose card cannot be read may be local; a
//! labelled value reaches none of them through the guard.

use std::sync::Arc;
use std::time::Instant;

use super::{
    ApprovalRequest, ChatRequest, ConvId, ConvSelector, Countable, DeniedBy, Digest, EffectCtx,
    FrameOutcome, Host, HostError, HostFuture, Labelled, ModelCard, ModelRef, ModelRequest,
    ModelResponse, Operation, RatioEvent, ToolClass, ToolInvocation, ToolOutcome,
};
use crate::frame::{FrameCtx, Location, check_within};
use crate::labels::Labels;
use crate::value::Value;

/// How a sink treats labelled arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkRule {
    /// Not a sink: labelled arguments pass, with the secrecy flag set.
    Open,
    /// A sink: any labelled argument is refused.
    Refuse,
    /// `write_file` and `edit_file`: labelled content is refused unless the target path
    /// itself matches `ratio.secret_paths` (the path argument must be unlabelled either way).
    UnlessSecretPath,
}

/// The sink rule of a tool call made by `builtin` naming `tool` (chapter 03, "Sinks").
#[must_use]
pub fn sink_rule(builtin: &str, tool: &str) -> SinkRule {
    // They can reach any tool by name, outbound ones included.
    if matches!(
        builtin,
        "tools.call_tool" | "tools.mcp_call" | "tools.dispatch" | "tools.dispatch_all"
    ) {
        return SinkRule::Refuse;
    }
    match tool {
        "web_search" | "web_fetch" | "run_command" | "run_background" | "run_script" => {
            SinkRule::Refuse
        }
        "write_file" | "edit_file" => SinkRule::UnlessSecretPath,
        t if t.starts_with("browser_") || t.starts_with("media_") || t.starts_with("mcp__") => {
            SinkRule::Refuse
        }
        _ => SinkRule::Open,
    }
}

/// The sink rule of an engine operation or output event.
#[must_use]
pub fn operation_rule(builtin: &str) -> SinkRule {
    match builtin {
        "memory.remember" | "memory.edit" | "memory.propose" | "session.emit" | "eval.metric"
        | "eval.score" | "eval.label" => SinkRule::Refuse,
        // Briefs, steering, questions and anything else handed to another agent.
        b if b.starts_with("agents.") => SinkRule::Refuse,
        _ => SinkRule::Open,
    }
}

fn labels_of<'a>(values: impl IntoIterator<Item = &'a Value>) -> Labels {
    values
        .into_iter()
        .fold(Labels::NONE, |acc, v| match crate::sink::check(v) {
            Ok(()) => acc,
            Err(l) => acc | l,
        })
}

fn args_labels(args: &[(Arc<str>, Value)]) -> Labels {
    labels_of(args.iter().map(|(_, v)| v))
}

fn refusal(what: &str) -> HostError {
    HostError::secrecy(format!("a secret cannot reach {what}"))
}

fn refused<T: Send + 'static>(e: HostError) -> HostFuture<Result<T, HostError>> {
    Box::pin(async move { Err(e) })
}

/// The guard every evaluation wraps its host in.
pub struct Guard {
    inner: Arc<dyn Host>,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Guard")
    }
}

impl Guard {
    #[must_use]
    pub fn new(inner: Arc<dyn Host>) -> Guard {
        Guard { inner }
    }

    /// The host it guards.
    #[must_use]
    pub fn inner(&self) -> &Arc<dyn Host> {
        &self.inner
    }

    /// Checks a tool call and sets its secrecy flag.
    ///
    /// # Errors
    ///
    /// `Denied { by: secrecy }` when a labelled argument would reach a sink.
    pub fn check_tool(&self, frame: &FrameCtx, call: &mut ToolInvocation) -> Result<(), HostError> {
        let labels = args_labels(&call.args);
        call.secret = !labels.is_empty();
        if !call.secret {
            return Ok(());
        }
        match sink_rule(&call.builtin, &call.tool) {
            SinkRule::Open => Ok(()),
            SinkRule::Refuse => Err(refusal(&format!("the tool `{}`", call.tool))),
            SinkRule::UnlessSecretPath => {
                let path = call.arg("path");
                let path_labelled = path.is_some_and(|p| crate::sink::check(p).is_err());
                let secret_path = path
                    .and_then(Value::as_text)
                    .is_some_and(|p| self.inner.is_secret_path(frame, p));
                if path_labelled || !secret_path {
                    Err(refusal(&format!(
                        "`{}` outside `ratio.secret_paths`",
                        call.tool
                    )))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Checks an engine operation and sets its secrecy flag.
    ///
    /// # Errors
    ///
    /// `Denied { by: secrecy }` when a labelled argument would reach a sink.
    pub fn check_operation(op: &mut Operation) -> Result<(), HostError> {
        op.secret = !args_labels(&op.args).is_empty();
        if op.secret && operation_rule(&op.builtin) == SinkRule::Refuse {
            return Err(refusal(&format!("`{}`", op.builtin)));
        }
        Ok(())
    }

    /// Checks an output-only event and sets its secrecy flag (`session.emit` is a sink; logs,
    /// traces and streams pass, for the host to redact).
    ///
    /// # Errors
    ///
    /// `Denied { by: secrecy }` for a labelled `session.emit`.
    pub fn check_event(event: &mut RatioEvent) -> Result<(), HostError> {
        event.secret = !args_labels(&event.args).is_empty();
        if event.secret && operation_rule(&event.builtin) == SinkRule::Refuse {
            return Err(refusal(&format!("`{}`", event.builtin)));
        }
        Ok(())
    }

    /// Whether `model` is local (its card says so); a model whose card cannot be read counts
    /// as hosted.
    fn is_local(&self, frame: &FrameCtx, model: Option<&ModelRef>) -> bool {
        model
            .and_then(|m| self.inner.resolve_model(frame, m).ok())
            .is_some_and(|c| c.location == Location::Local)
    }

    /// Checks a model request and sets its secrecy flag: a labelled prompt or option may
    /// reach a local model only.
    ///
    /// # Errors
    ///
    /// `Denied { by: secrecy }` for a secret sent to a hosted model.
    pub fn check_model(&self, frame: &FrameCtx, req: &mut ModelRequest) -> Result<(), HostError> {
        let labels =
            labels_of(std::iter::once(&req.prompt).chain(req.options.iter().map(|(_, v)| v)));
        req.secret = !labels.is_empty();
        if req.secret && !self.is_local(frame, Some(&req.model)) {
            return Err(refusal(&format!("the hosted model `{}`", req.model.0)));
        }
        Ok(())
    }

    /// Emits an event after [`Guard::check_event`].
    ///
    /// # Errors
    ///
    /// As [`Guard::check_event`].
    pub fn emit_checked(&self, frame: &FrameCtx, mut event: RatioEvent) -> Result<(), HostError> {
        Guard::check_event(&mut event)?;
        self.inner.emit(frame, event);
        Ok(())
    }
}

impl Host for Guard {
    fn generate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        mut req: ModelRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        if let Err(e) = self.check_model(&frame, &mut req) {
            return refused(e);
        }
        Arc::clone(&self.inner).generate(frame, cx, req)
    }

    fn embed(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        texts: Vec<Labelled<String>>,
        model: Option<ModelRef>,
    ) -> HostFuture<Result<Vec<Vec<f32>>, HostError>> {
        if texts.iter().any(Labelled::is_labelled) && !self.is_local(&frame, model.as_ref()) {
            return refused(refusal("a hosted embedding model"));
        }
        Arc::clone(&self.inner).embed(frame, cx, texts, model)
    }

    fn count_tokens(&self, frame: &FrameCtx, what: &Countable) -> u64 {
        self.inner.count_tokens(frame, what)
    }

    fn resolve_model(&self, frame: &FrameCtx, model: &ModelRef) -> Result<ModelCard, HostError> {
        self.inner.resolve_model(frame, model)
    }

    fn call_tool(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        mut call: ToolInvocation,
    ) -> HostFuture<Result<ToolOutcome, HostError>> {
        if let Err(e) = self.check_tool(&frame, &mut call) {
            return refused(e);
        }
        Arc::clone(&self.inner).call_tool(frame, cx, call)
    }

    fn tool_class(&self, frame: &FrameCtx, tool: &str) -> ToolClass {
        let c = self.inner.tool_class(frame, tool);
        // A class that breaks the contract is treated as the unlisted one.
        if c.is_bounded() {
            c
        } else {
            ToolClass::UNLISTED
        }
    }

    fn is_secret_path(&self, frame: &FrameCtx, path: &str) -> bool {
        self.inner.is_secret_path(frame, path)
    }

    fn source_labels(&self, frame: &FrameCtx, call: &ToolInvocation) -> Labels {
        self.inner.source_labels(frame, call)
    }

    fn conversation(&self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError> {
        self.inner.conversation(frame, which)
    }

    fn conversation_digest(&self, frame: &FrameCtx, conv: ConvId) -> Result<Digest, HostError> {
        self.inner.conversation_digest(frame, conv)
    }

    fn chat(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        conv: ConvId,
        mut req: ChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        req.secret = !args_labels(&req.options).is_empty();
        if req.secret && !self.is_local(&frame, req.model.as_ref()) {
            return refused(refusal("a hosted model"));
        }
        Arc::clone(&self.inner).chat(frame, cx, conv, req)
    }

    fn open_frame(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError> {
        if let Err(d) = check_within(&parent.scope, &child.scope, child.kind) {
            return Err(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!(
                    "the frame `{}` is wider than its caller in `{}`",
                    child.path,
                    d.name()
                ),
            });
        }
        self.inner.open_frame(parent, child)
    }

    fn close_frame(&self, frame: &FrameCtx, outcome: FrameOutcome) {
        self.inner.close_frame(frame, outcome);
    }

    fn operate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        mut op: Operation,
    ) -> HostFuture<Result<Value, HostError>> {
        if let Err(e) = Guard::check_operation(&mut op) {
            return refused(e);
        }
        Arc::clone(&self.inner).operate(frame, cx, op)
    }

    fn approve(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ApprovalRequest,
    ) -> HostFuture<Result<bool, HostError>> {
        Arc::clone(&self.inner).approve(frame, cx, req)
    }

    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn seed(&self) -> [u8; 32] {
        self.inner.seed()
    }

    fn timer(&self, at: Instant) -> HostFuture<()> {
        self.inner.timer(at)
    }

    fn emit(&self, frame: &FrameCtx, mut event: RatioEvent) {
        // The trait's `emit` cannot refuse: a refused event is dropped here; the scheduler
        // calls [`Guard::emit_checked`] and raises the refusal in the program.
        if Guard::check_event(&mut event).is_ok() {
            self.inner.emit(frame, event);
        }
    }
}
