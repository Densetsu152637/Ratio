//! From the machine's effects to host calls: which method a built-in reaches, its arguments by
//! name, its tool name and cancellation class, and whether it is input-bearing (chapter 01:
//! output-only effects get no tape entry and are suppressed in replay).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::{
    ApprovalKind, ApprovalRequest, Labelled, ModelRef, ModelRequest, OpFamily, Operation,
    RatioEvent, ToolClass, ToolInvocation,
};
use crate::frame::FrameCtx;
use crate::machine::EffectRequest;
use crate::value::Value;

/// The parameter names of every built-in, from the checker's table.
fn params() -> &'static HashMap<String, Vec<Arc<str>>> {
    static PARAMS: OnceLock<HashMap<String, Vec<Arc<str>>>> = OnceLock::new();
    PARAMS.get_or_init(|| {
        ratio_check::builtin_rules()
            .into_iter()
            .map(|r| {
                let names = r.params.iter().map(|p| Arc::from(p.as_str())).collect();
                (r.name, names)
            })
            .collect()
    })
}

/// The arguments given (defaults left out), by parameter name.
pub(crate) fn named_args(req: &EffectRequest) -> Vec<(Arc<str>, Value)> {
    let names = params().get(&*req.builtin);
    req.args
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let v = a.clone()?;
            let name = names
                .and_then(|n| n.get(i).cloned())
                .unwrap_or_else(|| Arc::from(format!("arg{i}")));
            Some((name, v))
        })
        .collect()
}

fn arg<'a>(args: &'a [(Arc<str>, Value)], name: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| &**n == name).map(|(_, v)| v)
}

fn text_of(args: &[(Arc<str>, Value)], name: &str) -> String {
    arg(args, name)
        .and_then(Value::as_text)
        .unwrap_or_default()
        .to_owned()
}

/// The host call an effect makes.
#[derive(Debug)]
pub(crate) enum Call {
    Tool(ToolInvocation),
    Model(ModelRequest),
    Approve(ApprovalRequest),
    /// Output-only: no result, no tape entry, suppressed in replay.
    Event(RatioEvent),
    Op(Operation),
}

impl Call {
    /// Whether the program reads the effect's response (it gets a tape entry).
    pub(crate) fn input_bearing(&self) -> bool {
        !matches!(self, Call::Event(_))
    }
}

/// The tool a `tools.*` built-in calls, or `None` for an engine read that calls none.
fn tool_name(builtin: &str, args: &[(Arc<str>, Value)]) -> Option<String> {
    let name = builtin.strip_prefix("tools.")?;
    let mcp = |what: &str| format!("mcp__{}__{what}", text_of(args, "server"));
    Some(match name {
        "call_tool" => text_of(args, "name"),
        "mcp_call" => format!(
            "mcp__{}__{}",
            text_of(args, "server"),
            text_of(args, "tool")
        ),
        "mcp_resources" => mcp("resources_list"),
        "mcp_read_resource" => mcp("resources_read"),
        "mcp_prompts" => mcp("prompts_list"),
        "mcp_get_prompt" => mcp("prompts_get"),
        "run_background" => "run_command".to_owned(),
        // Engine reads and conversation dispatch: operations, not tool calls.
        "mcp_servers" | "offered" | "spec" | "has" | "skills" | "plugins" | "refuse"
        | "dispatch" | "dispatch_all" => return None,
        other => other.to_owned(),
    })
}

/// A model's name: text, or a `ModelRef.Named { id }`.
fn model_name(v: &Value) -> Option<String> {
    if let Some(t) = v.as_text() {
        return Some(t.to_owned());
    }
    match &v.repr {
        crate::value::Repr::VariantP(vd) => vd
            .payload
            .field("id")
            .and_then(|id| id.as_text().map(str::to_owned)),
        _ => None,
    }
}

/// The model a request names: `opts.model`, else the frame's setting, else `default`.
fn model_of(frame: &FrameCtx, args: &[(Arc<str>, Value)]) -> ModelRef {
    let from_opts = arg(args, "opts")
        .and_then(|o| o.field("model"))
        .and_then(|m| model_name(&m));
    let from_frame = frame.settings.model.as_ref().and_then(model_name);
    ModelRef(Arc::from(
        from_opts
            .or(from_frame)
            .unwrap_or_else(|| "default".to_owned()),
    ))
}

/// The engine operations that are `drain`-class when cancelled (they write); every other
/// operation is aborted.
fn operation_class(builtin: &str) -> ToolClass {
    match builtin {
        "memory.remember"
        | "memory.edit"
        | "memory.forget"
        | "memory.propose"
        | "agents.merge"
        | "agents.worktree"
        | "session.update_plan"
        | "session.update_state"
        | "session.checkpoint"
        | "session.rewind_files"
        | "session.branch"
        | "session.start"
        | "context.compact" => ToolClass::drain(Duration::from_secs(600)),
        _ => ToolClass::abort(false),
    }
}

/// The class an effect is cancelled by: its tool's for a tool call (from the host's table),
/// `abort` for model calls and approvals (an approval pending is withdrawn), and the
/// operation's own otherwise.
pub(crate) fn class_of(host: &dyn super::Host, frame: &FrameCtx, call: &Call) -> ToolClass {
    match call {
        Call::Tool(t) => host.tool_class(frame, &t.tool),
        Call::Op(o) => operation_class(&o.builtin),
        Call::Model(_) | Call::Approve(_) | Call::Event(_) => ToolClass::abort(false),
    }
}

/// The host call `req` makes.
pub(crate) fn classify(req: &EffectRequest) -> Call {
    let builtin = Arc::clone(&req.builtin);
    let args = named_args(req);
    let b = &*builtin;
    if b == "declassify" || b == "session.request_approval" {
        let (kind, summary, detail) = if b == "declassify" {
            let because = arg(&args, "because");
            (
                ApprovalKind::Declassify,
                Labelled::new(
                    because
                        .and_then(Value::as_text)
                        .unwrap_or_default()
                        .to_owned(),
                    because.map_or(crate::labels::Labels::NONE, Value::labels),
                ),
                None,
            )
        } else {
            let summary = arg(&args, "summary");
            (
                ApprovalKind::Request,
                Labelled::new(
                    summary
                        .and_then(Value::as_text)
                        .unwrap_or_default()
                        .to_owned(),
                    summary.map_or(crate::labels::Labels::NONE, Value::labels),
                ),
                arg(&args, "detail").cloned(),
            )
        };
        return Call::Approve(ApprovalRequest {
            kind,
            summary,
            detail,
        });
    }
    if b.starts_with("log.")
        || b == "trace.event"
        || b == "session.emit"
        || b == "session.stream"
        || b == "session.notify"
    {
        return Call::Event(RatioEvent {
            builtin,
            key: req.key(),
            args,
            secret: false,
        });
    }
    if matches!(b, "model.generate" | "model.complete") {
        let prompt = arg(&args, "p")
            .or_else(|| arg(&args, "text"))
            .cloned()
            .unwrap_or_default();
        let max_tokens = arg(&args, "opts")
            .and_then(|o| o.field("max_tokens"))
            .and_then(|m| m.as_int())
            .and_then(|m| u64::try_from(m).ok());
        let model = model_of(&req.frame, &args);
        let options = args
            .into_iter()
            .filter(|(n, _)| &**n != "p" && &**n != "text")
            .collect();
        return Call::Model(ModelRequest {
            builtin,
            model,
            prompt,
            max_tokens,
            options,
            secret: false,
        });
    }
    if let Some(tool) = tool_name(b, &args) {
        return Call::Tool(ToolInvocation {
            tool: Arc::from(tool),
            builtin,
            args,
            secret: false,
            in_defer: req.in_defer,
        });
    }
    Call::Op(Operation {
        family: OpFamily::of(b),
        builtin,
        args,
        secret: false,
    })
}
