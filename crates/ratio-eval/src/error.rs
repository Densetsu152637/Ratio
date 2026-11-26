//! Run-time errors (chapter 03, "Errors"): values of the prelude's `Error` record, with the
//! class that says who may catch them.

use std::sync::Arc;

use ratio_ir::Loc;

use crate::heap::Heap;
use crate::labels::Labels;
use crate::prepare::Prepared;
use crate::value::{Json, Value};

/// Who may catch an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Any `try` may catch it.
    Catchable,
    /// A budget of the turn ran out: no code inside the turn may catch it (chapter 03,
    /// "Budgets cannot be swallowed where they run out"). Scoped budgets (`with { budget }`,
    /// stage 10) will carry the scope that may catch them.
    Budget,
    /// Cancellation passes through every `catch`; `defer` blocks observe it.
    Cancelled,
    /// A replay diverged (stage 11): never catchable.
    Replay,
}

/// A raised error: the `Error` value a `catch` binds, and its class.
#[derive(Clone, Debug)]
pub struct RtError {
    pub(crate) value: Value,
    pub(crate) class: Class,
}

impl RtError {
    /// The `Error` record.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    #[must_use]
    pub fn class(&self) -> Class {
        self.class
    }

    /// The `ErrorKind` variant's name (`Invalid`, `Budget`), given the program it was raised in.
    #[must_use]
    pub fn kind(&self, p: &Prepared) -> String {
        match self.value.field("kind").map(|k| k.repr) {
            Some(crate::value::Repr::Variant(e, i)) => variant_name(p, e, i),
            Some(crate::value::Repr::VariantP(v)) => variant_name(p, v.enum_, v.index),
            _ => "?".to_owned(),
        }
    }

    /// The message, if it is text.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        self.value
            .field("message")
            .and_then(|m| m.as_text().map(str::to_owned))
    }

    /// For a `Budget` error, which budget (`steps`, `heap`, `call_depth`).
    #[must_use]
    pub fn budget(&self, p: &Prepared) -> Option<String> {
        let kind = self.value.field("kind")?;
        let crate::value::Repr::VariantP(v) = &kind.repr else {
            return None;
        };
        match v.payload.field("which")?.repr {
            crate::value::Repr::Variant(e, i) => Some(variant_name(p, e, i)),
            _ => None,
        }
    }
}

fn variant_name(p: &Prepared, e: u32, i: u32) -> String {
    p.enums
        .get(e as usize)
        .and_then(|en| en.variants.get(i as usize))
        .map_or_else(|| "?".to_owned(), |v| v.name.to_string())
}

/// The kinds the interpreter raises itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind<'a> {
    Invalid,
    Unsupported,
    Assertion,
    NotFound,
    SchemaMismatch,
    /// `Budget { which }`.
    Budget(&'static str),
    Cancelled,
    /// `Denied { by }`.
    Denied(&'static str),
    /// `User { tag }`, from `fail`.
    User(&'a str),
}

impl Kind<'_> {
    fn name(self) -> &'static str {
        match self {
            Kind::Invalid => "Invalid",
            Kind::Unsupported => "Unsupported",
            Kind::Assertion => "Assertion",
            Kind::NotFound => "NotFound",
            Kind::SchemaMismatch => "SchemaMismatch",
            Kind::Budget(_) => "Budget",
            Kind::Cancelled => "Cancelled",
            Kind::Denied(_) => "Denied",
            Kind::User(_) => "User",
        }
    }

    fn class(self) -> Class {
        match self {
            Kind::Budget(_) => Class::Budget,
            Kind::Cancelled => Class::Cancelled,
            _ => Class::Catchable,
        }
    }
}

/// The `Site` record for a position.
pub(crate) fn site(p: &Prepared, loc: Loc) -> Value {
    let file = p
        .modules
        .get(loc.module as usize)
        .map_or_else(|| Arc::from("?"), |(_, path)| Arc::clone(path));
    let items = vec![
        Value::int(i64::from(loc.col)),
        Value::from_str(crate::value::Str::plain(&file)),
        Value::new_list(crate::plist::List::empty()),
        Value::int(i64::from(loc.line)),
    ];
    Value::new_record(&Heap::unmetered(), Arc::clone(&p.rt.site), items)
        .unwrap_or_else(|_| Value::unit())
}

/// The `ErrorKind` value of `kind`.
fn kind_value(p: &Prepared, kind: Kind<'_>) -> Value {
    let heap = Heap::unmetered();
    let ek = p.rt.error_kind;
    let Some(index) = p.enums[ek as usize].variant(kind.name()) else {
        return Value::unit();
    };
    let payload = match kind {
        Kind::Budget(which) => {
            let bk = p.rt.budget_kind;
            let w = p.enums[bk as usize].variant(which).unwrap_or(0);
            p.layout_of(ek, index).and_then(|layout| {
                Value::new_record(
                    &heap,
                    Arc::clone(layout),
                    vec![Value::new_variant(&heap, bk, w, None).ok()?],
                )
                .ok()
            })
        }
        Kind::Denied(by) => {
            let db = p.rt.denied_by;
            let b = p.enums[db as usize].variant(by).unwrap_or(0);
            p.layout_of(ek, index).and_then(|layout| {
                Value::new_record(
                    &heap,
                    Arc::clone(layout),
                    vec![Value::new_variant(&heap, db, b, None).ok()?],
                )
                .ok()
            })
        }
        Kind::User(tag) => p.layout_of(ek, index).and_then(|layout| {
            Value::new_record(&heap, Arc::clone(layout), vec![Value::text(tag)]).ok()
        }),
        _ => None,
    };
    Value::new_variant(&heap, ek, index, payload).unwrap_or_else(|_| Value::unit())
}

/// A JSON `null`.
pub(crate) fn null() -> Value {
    Value::new_json(&Heap::unmetered(), Json::Null).unwrap_or_else(|_| Value::unit())
}

/// An error of `kind` raised at `loc`. `labels` are those of the data the failed operation
/// read: they label the message, which may describe it.
pub(crate) fn raise(
    p: &Prepared,
    kind: Kind<'_>,
    message: &str,
    labels: Labels,
    loc: Loc,
) -> RtError {
    raise_with(p, kind, Value::text(message).with_labels(labels), None, loc)
}

/// An error with a given message value and `data`.
pub(crate) fn raise_with(
    p: &Prepared,
    kind: Kind<'_>,
    message: Value,
    data: Option<Value>,
    loc: Loc,
) -> RtError {
    RtError {
        value: error_value(p, kind, message, data, loc),
        class: kind.class(),
    }
}

/// The `Error` record for `kind` (what `fail` returns, without raising it).
pub(crate) fn error_value(
    p: &Prepared,
    kind: Kind<'_>,
    message: Value,
    data: Option<Value>,
    loc: Loc,
) -> Value {
    // An error record must always be built: data too deep to nest in it is left out.
    let data = data.filter(|d| d.depth() + 2 < crate::value::MAX_DEPTH);
    let items = vec![
        data.unwrap_or_else(null),
        kind_value(p, kind),
        message,
        site(p, loc),
    ];
    Value::new_record(&Heap::unmetered(), Arc::clone(&p.rt.error), items)
        .unwrap_or_else(|_| Value::unit())
}
