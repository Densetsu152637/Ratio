//! The sink guard (chapter 03, "Sinks"; chapter 10, "Security requirements by side").
//!
//! The interpreter wraps every host in a [`Guard`]; no host can opt out of it, because the
//! evaluation builds it around whatever host it is given, and calls the host through it for
//! every effect. The guard refuses a labelled value at every secrecy sink before the host sees
//! it (`Denied { by: secrecy }`), recomputes the per-call secrecy flag from the labels (never
//! trusting a flag it was handed), redacts labelled parts of an approval's text, and checks
//! every frame it forwards against its parent with [`check_within`] as defence in depth. The
//! engine enforces the same sinks again at dispatch from the flag, which catches a bug here
//! (but not a dropped label: propagation is the interpreter's alone).
//!
//! The rules fail closed. They are read from the checker's table of built-ins: a tool or
//! operation is a sink unless the table marks it `@local` (its effects keep its arguments on the
//! machine) or `@pure`; `write_file` and `edit_file` may write a secret only to a path in
//! `ratio.secret_paths`; a built-in the table does not know is refused outright. Where the
//! guard cannot know what the engine knows, it refuses: an MCP server may be local, a model
//! without a card may be local; a labelled value reaches none of them through the guard.

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

/// Whether the checker's table marks `builtin` as keeping its arguments on the machine.
fn local(builtin: &str) -> bool {
    super::bridge::traits(builtin).is_some_and(|t| (t.local || t.pure) && !t.secrecy_sink)
}

/// The sink rule of a tool call made by `builtin` naming `tool` (chapter 03, "Sinks"): a sink
/// unless the table marks the built-in `@local`, whatever the tool it names.
#[must_use]
pub fn sink_rule(builtin: &str, tool: &str) -> SinkRule {
    // They can reach any tool by name, outbound ones included.
    if matches!(
        builtin,
        "tools.call_tool" | "tools.mcp_call" | "tools.dispatch" | "tools.dispatch_all"
    ) {
        return SinkRule::Refuse;
    }
    if matches!(tool, "write_file" | "edit_file") {
        return SinkRule::UnlessSecretPath;
    }
    if local(builtin) {
        SinkRule::Open
    } else {
        SinkRule::Refuse
    }
}

/// The sink rule of an engine operation or output event: a sink unless the table marks it
/// `@local` (logs, traces, streams and notices pass, for the host to redact).
#[must_use]
pub fn operation_rule(builtin: &str) -> SinkRule {
    if builtin.starts_with("log.")
        || matches!(builtin, "trace.event" | "session.stream" | "session.notify")
    {
        return SinkRule::Open;
    }
    if local(builtin) {
        SinkRule::Open
    } else {
        SinkRule::Refuse
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

/// The placeholder a labelled part of an approval's text is shown as.
fn placeholder(labels: Labels) -> String {
    let sources: Vec<String> = (0..32)
        .filter(|i| labels.contains(Labels::source(*i)))
        .map(|i| i.to_string())
        .collect();
    format!("[secret: source {}]", sources.join(", "))
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

    /// An approval's summary and detail with every labelled part replaced by a placeholder
    /// naming its label sources: the user is asked, but no secret is shown to the host.
    #[must_use]
    pub fn redact_approval(mut req: ApprovalRequest) -> ApprovalRequest {
        if req.summary.is_labelled() {
            req.summary = Labelled::plain(placeholder(req.summary.labels));
        }
        if let Some(d) = &req.detail
            && let Err(l) = crate::sink::check(d)
        {
            req.detail = Some(Value::text(&placeholder(l)));
        }
        req
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

    fn source_labels(&self, frame: &FrameCtx, builtin: &str, args: &[(Arc<str>, Value)]) -> Labels {
        self.inner.source_labels(frame, builtin, args)
    }

    fn conversation(&self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError> {
        self.inner.conversation(frame, which)
    }

    fn conversation_digest(&self, frame: &FrameCtx, conv: ConvId) -> Result<Digest, HostError> {
        self.inner.conversation_digest(frame, conv)
    }

    fn conversation_labels(&self, frame: &FrameCtx, conv: ConvId) -> Result<Labels, HostError> {
        self.inner.conversation_labels(frame, conv)
    }

    fn chat(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        conv: ConvId,
        mut req: ChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        // A call on a labelled conversation is a sink as if its whole content were secret; a
        // conversation whose labels cannot be read counts as labelled.
        let conv_labelled = self
            .inner
            .conversation_labels(&frame, conv)
            .map_or(true, |l| !l.is_empty());
        req.secret = conv_labelled || !args_labels(&req.options).is_empty();
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
        Arc::clone(&self.inner).approve(frame, cx, Guard::redact_approval(req))
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

    fn emit(&self, frame: &FrameCtx, mut event: RatioEvent) -> Result<(), HostError> {
        Guard::check_event(&mut event)?;
        self.inner.emit(frame, event)
    }
}
