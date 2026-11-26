//! The `json` built-ins of chapter 06 (`data`) that stage 6 did not carry: `stringify`,
//! `schema`, `validate`, `set`, `keys`, `type_of`, `merge`, `patch`, `diff` and `equal`.
//!
//! A `Json` node's own labels are its node's; a rebuilt container gains the labels of what
//! decided the edit (the pointer, the patch, the keys searched), a miss or a failure is
//! labelled with everything compared, and a result computed from whole values carries their
//! `quick` labels. Edits copy the path they change (`crate::jsonedit`), so every one is
//! charged from the sizes of its arguments before it runs.

use crate::error::{Kind, RtError, raise};
use crate::jsonedit::{
    Edit, apply_op, array, diff, edit, merge as merge_nodes, node, object, render, text_node,
    tokens,
};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::ops::eq;
use crate::value::{Fault, Json, Repr, Value};

use super::util::{Gather, Pace, heap_error};
use super::validate::validate;
use super::{Args, Sizes, arg, new_text, opt, text, weight};

/// `json.patch`: each operation may rebuild a path through the document, so the charge is the
/// product of the document's size (grown by what the patch adds) and the patch's.
pub(super) fn s_patch(a: &Args) -> Sizes {
    let (doc, ops) = (weight(a, 0), weight(a, 1));
    let n = doc.saturating_add(ops);
    Sizes {
        n,
        n2: ops,
        c: n.saturating_add(ops),
        ..Sizes::default()
    }
}

fn invalid(cx: &Cx<'_>, message: &str, labels: Labels) -> RtError {
    raise(cx.p, Kind::Invalid, message, labels, cx.loc)
}

fn json_of<'a>(cx: &Cx<'_>, v: &'a Value) -> Result<&'a Value, RtError> {
    match &v.repr {
        Repr::Json(_) => Ok(v),
        _ => Err(invalid(cx, "expected Json", v.labels)),
    }
}

pub(super) fn stringify(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let v = arg(cx, args, 0)?;
    let pretty = opt(args, 1).and_then(Value::as_bool).unwrap_or(false);
    let labels = v.quick() | opt(args, 1).map_or(Labels::NONE, |p| p.labels);
    let Some(&t) = cx.type_args.first() else {
        return Err(invalid(cx, "json.stringify without a type", Labels::NONE));
    };
    let encoded =
        crate::json::encode(cx.p, cx.heap, v, t, &[], 0).map_err(|f| cx.fault(f, v.quick()))?;
    let mut out = String::new();
    if !render(&encoded, pretty, &mut out) {
        // What was written before the refusal is paid for, as for `text.show`.
        cx.debit = (out.len() as u64).div_ceil(64);
        return Err(invalid(cx, "the JSON text is longer than 16 MiB", labels));
    }
    new_text(cx, out, labels)
}

pub(super) fn schema(cx: &mut Cx<'_>, _args: &Args) -> Result<Value, RtError> {
    let Some(&t) = cx.type_args.first() else {
        return Err(invalid(cx, "json.schema without a type", Labels::NONE));
    };
    let (text, used) = crate::schema::schema_text(cx.p, t);
    // The derivation's work is paid for whether it finishes or not.
    cx.debit = used;
    let text = text.map_err(|f| cx.fault(f, Labels::NONE))?;
    crate::json::parse(cx.heap, &text, Labels::NONE).map_err(|f| cx.fault(f, Labels::NONE))
}

pub(super) fn validate_(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (j, schema) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    json_of(cx, j)?;
    json_of(cx, schema)?;
    // Whether there are violations depends on everything in both.
    let labels = j.quick() | schema.quick();
    let budget = super::validate::visit_budget(j.weight(), schema.weight());
    let layout = std::sync::Arc::clone(&cx.p.rt.violation);
    let mut out = Gather::new();
    let cx_ref: &Cx<'_> = cx;
    let paid = std::sync::atomic::AtomicU64::new(0);
    let walked = validate(cx_ref, j, schema, budget, &paid, &mut |pointer, message| {
        let message = Value::new_text(cx_ref.heap, message).map_err(|f| cx_ref.fault(f, labels))?;
        let pointer = Value::new_text(cx_ref.heap, pointer.to_owned())
            .map_err(|f| cx_ref.fault(f, labels))?;
        let record = Value::new_record(
            cx_ref.heap,
            std::sync::Arc::clone(&layout),
            vec![message, pointer],
        )
        .map_err(|f| cx_ref.fault(f, labels))?;
        out.push(cx_ref, record)
    });
    // The pattern work is paid for, whether the walk finished or not.
    cx.debit = cx
        .debit
        .saturating_add(paid.load(std::sync::atomic::Ordering::Relaxed));
    walked?;
    out.finish(cx, labels)
}

pub(super) fn set(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (j, ptr, v) = (arg(cx, args, 0)?, arg(cx, args, 1)?, arg(cx, args, 2)?);
    json_of(cx, j)?;
    let labels = j.quick() | ptr.labels;
    let toks = tokens(text(cx, ptr)?).map_err(|f| cx.fault(f, labels))?;
    // The pointer decided where the value went; the keys searched decide whether it replaced
    // or added (`edit` adds their labels to each container it rebuilds).
    let out = edit(cx.heap, j, &toks, Edit::Upsert(v.clone()), ptr.labels)
        .map_err(|f| cx.fault(f, labels))?;
    Ok(out.with_labels(if toks.is_empty() {
        ptr.labels
    } else {
        Labels::NONE
    }))
}

pub(super) fn keys(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let j = arg(cx, args, 0)?;
    json_of(cx, j)?;
    let entries = object(j).unwrap_or(&[]);
    let mut out = Gather::with_capacity(cx, entries.len() as u64, j.labels)?;
    let mut pace = Pace::new();
    // Their order and number depend on the labels of the keys themselves.
    let mut own = j.labels;
    for (k, _) in entries {
        pace.tick(cx, 1)?;
        own |= k.labels;
        out.push(cx, k.clone().with_labels(j.labels))?;
    }
    out.finish(cx, own)
}

pub(super) fn type_of(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let j = arg(cx, args, 0)?;
    let name = match node(j) {
        Some(Json::Null) => "null",
        Some(Json::Bool(_)) => "bool",
        Some(Json::Int(_) | Json::Float(_)) => "number",
        Some(Json::Str(_)) => "string",
        Some(Json::Array(_)) => "array",
        Some(Json::Object(_)) => "object",
        None => return Err(invalid(cx, "expected Json", j.labels)),
    };
    let e = cx.p.rt.json_type;
    let index = cx.p.enums[e as usize].variant(name).unwrap_or(0);
    // The node's own labels picked the variant (the kind of node is theirs to decide).
    Value::new_variant(cx.heap, e, index, cx.p.variant_weight(e, index), None)
        .map(|v| v.with_labels(j.labels))
        .map_err(|f| cx.fault(f, j.labels))
}

pub(super) fn merge(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    json_of(cx, a)?;
    json_of(cx, b)?;
    merge_nodes(cx.heap, Some(a), b).map_err(|f| cx.fault(f, a.quick() | b.quick()))
}

pub(super) fn patch(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (j, ops) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    json_of(cx, j)?;
    let labels = j.quick() | ops.quick();
    let Some(list) = array(ops) else {
        return Err(invalid(cx, "a patch is an array of operations", labels));
    };
    let mut doc = j.clone();
    let mut pace = Pace::new();
    let mut c = list.cursor();
    let mut i = 0usize;
    while let Some(op) = c.next() {
        // An operation may rebuild a path through the document.
        pace.tick(cx, 1 + doc.weight().min(1 << 16))?;
        doc = apply_op(cx.heap, &doc, &op, i, ops.quick()).map_err(|f| cx.fault(f, labels))?;
        i += 1;
    }
    Ok(doc)
}

pub(super) fn diff_(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    json_of(cx, a)?;
    json_of(cx, b)?;
    // The patch depends on every part of both documents.
    let labels = a.quick() | b.quick();
    let mut out = Gather::new();
    let mut pace = Pace::new();
    let cx_ref: &Cx<'_> = cx;
    let fault = |f: Fault| cx_ref.fault(f, labels);
    diff(
        cx_ref.heap,
        a,
        b,
        &mut |op: &str, path: &str, value: Option<&Value>| {
            let mut entries = vec![
                (
                    Value::new_text(cx_ref.heap, "op".to_owned()).map_err(fault)?,
                    text_node(cx_ref.heap, op).map_err(fault)?,
                ),
                (
                    Value::new_text(cx_ref.heap, "path".to_owned()).map_err(fault)?,
                    text_node(cx_ref.heap, path).map_err(fault)?,
                ),
            ];
            if let Some(v) = value {
                entries.push((
                    Value::new_text(cx_ref.heap, "value".to_owned()).map_err(fault)?,
                    v.clone(),
                ));
            }
            let object = Value::new_json(cx_ref.heap, Json::Object(entries.into_boxed_slice()))
                .map_err(fault)?;
            out.push(cx_ref, object)
        },
        &mut |units| pace.tick(cx_ref, units),
        &|| heap_error(cx_ref),
    )?;
    out.finish_json(cx, labels)
}

pub(super) fn equal(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    Ok(Value::bool(eq(a, b)).with_labels(a.quick() | b.quick()))
}
