//! What a frame reports about itself (`meta.frame`, `meta.caller`, `meta.depth`,
//! `meta.limits`, `meta.permissions`; chapter 06, `meta`), `question` (chapter 06, `control`),
//! and the ambient reads `sees` gates (chapter 04, "What a callee sees").

use std::sync::Arc;

use super::scope::{BudgetKind, Paths, SeesItem};
use super::stack::FrameStack;
use crate::error::{Kind, RtError, raise};
use crate::heap::Heap;
use crate::labels::Labels;
use crate::machine::Cx;
use crate::prepare::Prepared;
use crate::value::{Fault, Layout, Str, Value};

/// The `sees` item that gates an ambient read, for the built-ins that read session state
/// without a tool call. A read outside the frame's `sees` raises `Denied { by: sees }` before
/// the host is asked.
#[must_use]
pub fn gate(builtin: &str) -> Option<SeesItem> {
    Some(match builtin {
        "session.turn" => SeesItem::Turn,
        "session.plan" | "session.state" => SeesItem::Plan,
        "memory.environment" => SeesItem::MemoryEnvironment,
        "memory.index" | "memory.used" => SeesItem::MemoryIndex,
        "agents.list" | "agents.status" | "agents.knowledge" => SeesItem::Agents,
        "context.system_default" => SeesItem::SystemDefault,
        "state.get" => SeesItem::State,
        _ => return None,
    })
}

/// Whether the interpreter answers this built-in itself, from the frame.
pub(crate) fn answers(builtin: &str) -> bool {
    matches!(
        builtin,
        "meta.frame" | "meta.caller" | "meta.depth" | "meta.limits" | "meta.permissions"
    )
}

/// A record of these fields, charged to `heap`.
fn record(heap: &Heap, mut fields: Vec<(&str, Value)>) -> Result<Value, Fault> {
    fields.sort_by(|a, b| a.0.cmp(b.0));
    let layout = Arc::new(Layout {
        names: fields.iter().map(|(n, _)| Arc::from(*n)).collect(),
    });
    Value::new_record(heap, layout, fields.into_iter().map(|(_, v)| v).collect())
}

fn unit_variant(p: &Prepared, enum_name: &str, name: &str) -> Result<Value, Fault> {
    let e =
        p.rt.prelude_enums
            .get(enum_name)
            .copied()
            .ok_or_else(|| Fault::Invalid(format!("the prelude enum `{enum_name}` is missing")))?;
    let i = p.enums[e as usize]
        .variant(name)
        .ok_or_else(|| Fault::Invalid(format!("`{enum_name}` has no variant `{name}`")))?;
    Value::new_variant(&Heap::unmetered(), e, i, p.variant_weight(e, i), None)
}

fn int(n: u64) -> Value {
    Value::int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn text(heap: &Heap, s: &str) -> Result<Value, Fault> {
    Ok(Value::from_str(Str::new(heap, s.to_owned())?))
}

fn list(heap: &Heap, items: Vec<Value>) -> Result<Value, Fault> {
    Ok(Value::new_list(crate::plist::List::from_vec(heap, items)?))
}

/// The value `builtin` reads from the top frame.
pub(crate) fn answer(
    p: &Prepared,
    heap: &Heap,
    frames: &FrameStack,
    heap_limit: u64,
    builtin: &str,
) -> Result<Value, Fault> {
    let top = frames.top();
    let ctx = &top.ctx;
    let s = &ctx.scope;
    match builtin {
        "meta.depth" => Ok(int(u64::from(s.depth))),
        "meta.frame" => {
            let model = match &ctx.settings.model {
                Some(m) => m.clone(),
                None => unit_variant(p, "ModelRef", "session")?,
            };
            let title = match &ctx.settings.title {
                Some(t) => t.clone(),
                None => text(heap, &ctx.harness)?,
            };
            record(
                heap,
                vec![
                    ("attended", Value::bool(s.attended)),
                    ("context", unit_variant(p, "ContextMode", s.context.name())?),
                    ("depth", int(u64::from(s.depth))),
                    (
                        "effort",
                        ctx.settings.effort.clone().unwrap_or_else(Value::none),
                    ),
                    ("name", text(heap, &ctx.harness)?),
                    ("location", unit_variant(p, "Location", s.location.name())?),
                    ("mode", unit_variant(p, "PermissionMode", s.mode.name())?),
                    ("model", model),
                    ("path", text(heap, &ctx.path)?),
                    ("title", title),
                    ("tools", int(s.tools.len() as u64)),
                ],
            )
        }
        "meta.caller" => match &top.caller {
            None => Ok(Value::none()),
            Some(c) => record(
                heap,
                vec![
                    ("name", text(heap, &c.harness)?),
                    ("site", crate::error::site(p, c.loc)),
                ],
            ),
        },
        "meta.limits" => {
            let mut budget = Vec::new();
            for k in BudgetKind::ALL {
                let v = match (k, s.budget.get(k)) {
                    (_, None) => Value::none(),
                    (BudgetKind::Cost, Some(c)) => {
                        Value::money(i64::try_from(c).unwrap_or(i64::MAX))
                    }
                    (BudgetKind::Wall, Some(w)) => {
                        Value::duration(i64::try_from(w).unwrap_or(i64::MAX))
                    }
                    (_, Some(n)) => int(n),
                };
                budget.push((k.name(), v));
            }
            record(
                heap,
                vec![
                    ("budget", record(heap, budget)?),
                    ("depth", int(u64::from(s.max_depth))),
                    ("heap", int(s.heap.unwrap_or(heap_limit).min(heap_limit))),
                    ("width", int(u64::from(s.width))),
                ],
            )
        }
        "meta.permissions" => {
            let session = &frames.session_scope().rules;
            let mut rules = Vec::new();
            for (action, set, base) in [
                ("deny", &s.rules.deny, &session.deny),
                ("ask", &s.rules.ask, &session.ask),
            ] {
                for r in set.difference(base) {
                    rules.push(record(
                        heap,
                        vec![
                            ("action", unit_variant(p, "RuleAction", action)?),
                            ("rule", text(heap, r)?),
                        ],
                    )?);
                }
            }
            let paths = match &s.paths {
                Paths::Any => Value::none(),
                Paths::Within(fs) => list(
                    heap,
                    fs.iter()
                        .map(|f| text(heap, &f.to_string()))
                        .collect::<Result<_, _>>()?,
                )?,
            };
            let tools = s
                .tools
                .iter()
                .map(|t| text(heap, &t.to_string()))
                .collect::<Result<_, _>>()?;
            record(
                heap,
                vec![
                    ("extra_rules", list(heap, rules)?),
                    ("location", unit_variant(p, "Location", s.location.name())?),
                    ("mode", unit_variant(p, "PermissionMode", s.mode.name())?),
                    ("paths", paths),
                    ("tools", list(heap, tools)?),
                ],
            )
        }
        _ => Err(Fault::Invalid(format!(
            "`{builtin}` is not a frame reading"
        ))),
    }
}

/// `question(text, options)`: a `Question` error to raise (chapter 06, `control`). Both
/// fields keep their labels, and so does the message.
pub(crate) fn question(cx: &mut Cx<'_>, args: &[Option<Value>]) -> Result<Value, RtError> {
    let p = cx.p;
    let missing = || {
        raise(
            p,
            Kind::Invalid,
            "`question` was called without its text",
            Labels::NONE,
            cx.loc,
        )
    };
    let text = args.first().cloned().flatten().ok_or_else(missing)?;
    let options = args
        .get(1)
        .cloned()
        .flatten()
        .map_or_else(|| Value::list(Vec::new()), Ok)
        .map_err(|f| cx.fault(f, Labels::NONE))?;
    let ek = p.rt.error_kind;
    let index = p.enums[ek as usize]
        .variant("Question")
        .ok_or_else(missing)?;
    let layout = p.layout_of(ek, index).cloned().ok_or_else(missing)?;
    let mut items = vec![Value::none(); layout.names.len()];
    for (slot, name) in layout.names.iter().enumerate() {
        items[slot] = match &**name {
            "question" => text.clone(),
            "options" => options.clone(),
            _ => Value::none(),
        };
    }
    let payload =
        Value::new_record(cx.heap, layout, items).map_err(|f| cx.fault(f, Labels::NONE))?;
    let kind = Value::new_variant(
        cx.heap,
        ek,
        index,
        p.variant_weight(ek, index),
        Some(payload),
    )
    .map_err(|f| cx.fault(f, Labels::NONE))?;
    let items = vec![
        crate::error::null(),
        kind,
        text,
        crate::error::site(p, cx.loc),
    ];
    Value::new_record(cx.heap, Arc::clone(&p.rt.error), items)
        .map_err(|f| cx.fault(f, Labels::NONE))
}
