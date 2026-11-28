//! From the machine's effects to host calls, driven by the checker's table of built-ins: which
//! method a built-in reaches (every `@effect(model)` built-in is a model call, admitted by
//! reservation; every `tools.*` built-in a tool call, admitted one `tool_calls` per call it
//! makes), its arguments by name, its tool name and cancellation class, its sink rule, and
//! whether it is input-bearing (chapter 01: output-only effects get no tape entry and are
//! suppressed in replay). A built-in the table does not know reaches no host at all.

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

/// What the checker's table says about a built-in.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "independent attributes")]
pub(crate) struct Traits {
    pub params: Vec<Arc<str>>,
    pub effects: Vec<String>,
    pub pure: bool,
    pub local: bool,
    pub secrecy_sink: bool,
    /// `@user`: its answer is the person's (trusted `UserText`, a decision), so it is never
    /// answered from a tape.
    pub user: bool,
}

/// The checker's table, by built-in name.
fn table() -> &'static HashMap<String, Traits> {
    static TABLE: OnceLock<HashMap<String, Traits>> = OnceLock::new();
    TABLE.get_or_init(|| {
        ratio_check::builtin_rules()
            .into_iter()
            .map(|r| {
                let t = Traits {
                    params: r.params.iter().map(|p| Arc::from(p.as_str())).collect(),
                    effects: r.effects,
                    pure: r.pure,
                    local: r.local,
                    secrecy_sink: r.secrecy_sink,
                    user: r.user,
                };
                (r.name, t)
            })
            .collect()
    })
}

/// The table's traits of `builtin`, if it has an entry.
pub(crate) fn traits(builtin: &str) -> Option<&'static Traits> {
    table().get(builtin)
}

/// Whether the table marks `builtin` `@user`: the person answers it.
pub(crate) fn from_user(builtin: &str) -> bool {
    traits(builtin).is_some_and(|t| t.user)
}

/// The arguments given (defaults left out), by parameter name.
pub(crate) fn named_args(req: &EffectRequest) -> Vec<(Arc<str>, Value)> {
    let names = traits(&req.builtin).map(|t| &t.params);
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
    /// A tool call, admitted as this many tool calls.
    Tool(ToolInvocation, u64),
    Model(ModelRequest),
    Approve(ApprovalRequest),
    /// Output-only: no result, no tape entry, suppressed in replay.
    Event(RatioEvent),
    Op(Operation),
    /// A built-in the table does not know: it reaches no host.
    Unknown(Arc<str>),
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
        // A model's calls dispatched on a conversation: tool calls, of whatever they name.
        "dispatch" | "dispatch_all" => "dispatch".to_owned(),
        // Engine reads: operations, not tool calls.
        "mcp_servers" | "offered" | "spec" | "has" | "skills" | "plugins" | "refuse" => {
            return None;
        }
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

/// The bound a `drain` operation is failed at.
pub(crate) const OPERATION_DRAIN: Duration = Duration::from_secs(600);

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
        | "context.compact" => ToolClass::drain(OPERATION_DRAIN),
        _ => ToolClass::abort(false),
    }
}

/// The class an effect is cancelled by: its tool's for a tool call (from the host's table),
/// `abort` for model calls and approvals (an approval pending is withdrawn), and the
/// operation's own otherwise.
pub(crate) fn class_of(host: &dyn super::Host, frame: &FrameCtx, call: &Call) -> ToolClass {
    match call {
        Call::Tool(t, _) => host.tool_class(frame, &t.tool),
        Call::Op(o) => operation_class(&o.builtin),
        Call::Model(_) | Call::Approve(_) | Call::Event(_) | Call::Unknown(_) => {
            ToolClass::abort(false)
        }
    }
}

/// The output-only built-ins.
fn output_only(b: &str) -> bool {
    b.starts_with("log.")
        || matches!(
            b,
            "trace.event" | "session.emit" | "session.stream" | "session.notify"
        )
}

fn approval(b: &str, args: &[(Arc<str>, Value)]) -> ApprovalRequest {
    let labelled = |name: &str| {
        let v = arg(args, name);
        Labelled::new(
            v.and_then(Value::as_text).unwrap_or_default().to_owned(),
            v.map_or(crate::labels::Labels::NONE, Value::labels),
        )
    };
    if b == "declassify" {
        ApprovalRequest {
            kind: ApprovalKind::Declassify,
            summary: labelled("because"),
            detail: None,
        }
    } else {
        ApprovalRequest {
            kind: ApprovalKind::Request,
            summary: labelled("summary"),
            detail: arg(args, "detail").cloned(),
        }
    }
}

/// The host call `req` makes.
pub(crate) fn classify(req: &EffectRequest) -> Call {
    let builtin = Arc::clone(&req.builtin);
    let b = &*builtin;
    let Some(t) = traits(b) else {
        if output_only(b) {
            let args = named_args(req);
            return Call::Event(RatioEvent {
                builtin,
                key: req.key(),
                args,
                secret: false,
            });
        }
        return Call::Unknown(builtin);
    };
    let args = named_args(req);
    if b == "declassify" || b == "session.request_approval" {
        return Call::Approve(approval(b, &args));
    }
    if output_only(b) {
        return Call::Event(RatioEvent {
            builtin,
            key: req.key(),
            args,
            secret: false,
        });
    }
    if t.effects.iter().any(|e| e == "model") {
        let prompt_name = ["p", "text", "input"]
            .into_iter()
            .find(|n| arg(&args, n).is_some());
        let prompt = prompt_name
            .and_then(|n| arg(&args, n))
            .cloned()
            .unwrap_or_default();
        let max_tokens = arg(&args, "opts")
            .and_then(|o| o.field("max_tokens"))
            .and_then(|m| m.as_int())
            .and_then(|m| u64::try_from(m).ok());
        let model = model_of(&req.frame, &args);
        let options = args
            .into_iter()
            .filter(|(n, _)| Some(&**n) != prompt_name)
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
        // `dispatch_all` runs as many tool calls as it is given.
        let count = if b == "tools.dispatch_all" {
            arg(&args, "calls")
                .and_then(Value::elements)
                .map_or(1, |c| c.len() as u64)
                .max(1)
        } else {
            1
        };
        return Call::Tool(
            ToolInvocation {
                tool: Arc::from(tool),
                builtin,
                args,
                secret: false,
                in_defer: req.in_defer,
            },
            count,
        );
    }
    Call::Op(Operation {
        family: OpFamily::of(b),
        builtin,
        args,
        secret: false,
    })
}
