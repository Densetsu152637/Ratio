//! Values and host errors on the tape, with their labels.
//!
//! A value is an array whose first element names its kind: `["u"]` unit, `["n"]` none,
//! `["b",true]`, `["i",5]`, `["f","<bits as hex>"]` (exact), `["d",us]` duration, `["m",µ$]`
//! money, `["s","text"]`, `["l",[…]]` list, `["t",[…]]` tuple, `["r",{…}]` record, and JSON
//! as `["jn"]`, `["jb",b]`, `["ji",i]`, `["jf","<bits>"]`, `["js","…"]`, `["ja",[…]]`,
//! `["jo",[[k,v],…]]`. A labelled value is wrapped: `["L",bits,…]`. Other kinds (functions,
//! maps, variants) never come from a host and are not written.

use std::sync::Arc;

use super::json::J;
use crate::heap::Heap;
use crate::host::{CancelOutcome, DeniedBy, HostError, Labelled};
use crate::labels::Labels;
use crate::plist::List;
use crate::value::{Json, Repr, Str, Value};

fn tag(t: &str, rest: impl IntoIterator<Item = J>) -> J {
    let mut v = vec![J::Str(t.to_owned())];
    v.extend(rest);
    J::Arr(v)
}

fn float_bits(f: f64) -> J {
    J::Str(format!("{:016x}", f.to_bits()))
}

/// Encodes `v`; `labels` keeps the labels (a digest leaves them out, since replay may add
/// labels but must reproduce the value).
pub(crate) fn encode(v: &Value, labels: bool) -> Result<J, String> {
    let inner = match &v.repr {
        Repr::Unit => tag("u", []),
        Repr::None => tag("n", []),
        Repr::Bool(b) => tag("b", [J::Bool(*b)]),
        Repr::Int(i) => tag("i", [J::Int(*i)]),
        Repr::Float(f) => tag("f", [float_bits(*f)]),
        Repr::Duration(d) => tag("d", [J::Int(*d)]),
        Repr::Money(m) => tag("m", [J::Int(*m)]),
        Repr::Str(s) => tag("s", [J::Str(s.as_str().to_owned())]),
        Repr::List(l) => {
            let mut xs = Vec::new();
            let mut err = None;
            l.for_each(&mut |x| match encode(x, labels) {
                Ok(j) => xs.push(j),
                Err(e) => err = Some(e),
            });
            if let Some(e) = err {
                return Err(e);
            }
            tag("l", [J::Arr(xs)])
        }
        Repr::Tuple(a) => tag(
            "t",
            [J::Arr(
                a.items
                    .iter()
                    .map(|x| encode(x, labels))
                    .collect::<Result<_, _>>()?,
            )],
        ),
        Repr::Record(r) => tag(
            "r",
            [J::Obj(
                r.layout
                    .names
                    .iter()
                    .zip(r.items.iter())
                    .map(|(n, x)| Ok((n.to_string(), encode(x, labels)?)))
                    .collect::<Result<_, String>>()?,
            )],
        ),
        Repr::Json(j) => encode_json(&j.node, labels)?,
        _ => return Err(format!("a {} cannot be recorded", v.kind_name())),
    };
    Ok(if labels && !v.labels.is_empty() {
        tag("L", [J::Int(i64::from(v.labels.bits())), inner])
    } else {
        inner
    })
}

fn encode_json(n: &Json, labels: bool) -> Result<J, String> {
    Ok(match n {
        Json::Null => tag("jn", []),
        Json::Bool(b) => tag("jb", [J::Bool(*b)]),
        Json::Int(i) => tag("ji", [J::Int(*i)]),
        Json::Float(f) => tag("jf", [float_bits(*f)]),
        Json::Str(s) => tag("js", [J::Str(s.as_str().to_owned())]),
        Json::Array(l) => {
            let mut xs = Vec::new();
            let mut err = None;
            l.for_each(&mut |x| match encode(x, labels) {
                Ok(j) => xs.push(j),
                Err(e) => err = Some(e),
            });
            if let Some(e) = err {
                return Err(e);
            }
            tag("ja", [J::Arr(xs)])
        }
        Json::Object(entries) => tag(
            "jo",
            [J::Arr(
                entries
                    .iter()
                    .map(|(k, v)| Ok(J::Arr(vec![encode(k, labels)?, encode(v, labels)?])))
                    .collect::<Result<_, String>>()?,
            )],
        ),
    })
}

fn bits(j: &J) -> Result<f64, String> {
    match j {
        J::Str(s) => u64::from_str_radix(s, 16)
            .map(f64::from_bits)
            .map_err(|_| "a malformed float".to_owned()),
        _ => Err("a malformed float".to_owned()),
    }
}

/// Decodes a value (bounded by the parser's own depth bound).
pub(crate) fn decode(j: &J) -> Result<Value, String> {
    let bad = || "a malformed value on the tape".to_owned();
    let J::Arr(xs) = j else { return Err(bad()) };
    let Some(J::Str(t)) = xs.first() else {
        return Err(bad());
    };
    let arg = |i: usize| xs.get(i).ok_or_else(bad);
    let heap = Heap::unmetered();
    let fault = |f: crate::value::Fault| format!("{f:?}");
    Ok(match t.as_str() {
        "L" => {
            let J::Int(b) = arg(1)? else {
                return Err(bad());
            };
            let b = u32::try_from(*b).map_err(|_| bad())?;
            let v = decode(arg(2)?)?;
            let own = v.labels | Labels::from_bits(b);
            v.with_labels(own)
        }
        "u" => Value::unit(),
        "n" => Value::none(),
        "b" => match arg(1)? {
            J::Bool(b) => Value::bool(*b),
            _ => return Err(bad()),
        },
        "i" => match arg(1)? {
            J::Int(i) => Value::int(*i),
            _ => return Err(bad()),
        },
        "f" => Value::float(bits(arg(1)?)?),
        "d" => match arg(1)? {
            J::Int(i) => Value::duration(*i),
            _ => return Err(bad()),
        },
        "m" => match arg(1)? {
            J::Int(i) => Value::money(*i),
            _ => return Err(bad()),
        },
        "s" => match arg(1)? {
            J::Str(s) => Value::text(s),
            _ => return Err(bad()),
        },
        "l" | "t" => {
            let J::Arr(items) = arg(1)? else {
                return Err(bad());
            };
            let items = items.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
            if t == "l" {
                Value::list(items).map_err(fault)?
            } else {
                Value::tuple(items).map_err(fault)?
            }
        }
        "r" => {
            let J::Obj(m) = arg(1)? else {
                return Err(bad());
            };
            let fields = m
                .iter()
                .map(|(k, v)| Ok((k.as_str(), decode(v)?)))
                .collect::<Result<Vec<_>, String>>()?;
            Value::record(fields).map_err(fault)?
        }
        "jn" => Value::new_json(&heap, Json::Null).map_err(fault)?,
        "jb" => match arg(1)? {
            J::Bool(b) => Value::new_json(&heap, Json::Bool(*b)).map_err(fault)?,
            _ => return Err(bad()),
        },
        "ji" => match arg(1)? {
            J::Int(i) => Value::new_json(&heap, Json::Int(*i)).map_err(fault)?,
            _ => return Err(bad()),
        },
        "jf" => Value::new_json(&heap, Json::Float(bits(arg(1)?)?)).map_err(fault)?,
        "js" => match arg(1)? {
            J::Str(s) => Value::new_json(&heap, Json::Str(Str::plain(s))).map_err(fault)?,
            _ => return Err(bad()),
        },
        "ja" => {
            let J::Arr(items) = arg(1)? else {
                return Err(bad());
            };
            let items = items.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
            let l = List::from_vec(&heap, items).map_err(fault)?;
            Value::new_json(&heap, Json::Array(l)).map_err(fault)?
        }
        "jo" => {
            let J::Arr(items) = arg(1)? else {
                return Err(bad());
            };
            let mut entries = Vec::with_capacity(items.len());
            for it in items {
                let J::Arr(kv) = it else { return Err(bad()) };
                let [k, v] = kv.as_slice() else {
                    return Err(bad());
                };
                entries.push((decode(k)?, decode(v)?));
            }
            Value::new_json(&heap, Json::Object(entries.into())).map_err(fault)?
        }
        _ => return Err(bad()),
    })
}

/// A host error on the tape.
pub(crate) fn encode_error(e: &HostError) -> J {
    let text = |k: &str, m: &str| {
        J::Obj(vec![
            ("err".to_owned(), J::Str(k.to_owned())),
            ("msg".to_owned(), J::Str(m.to_owned())),
        ])
    };
    match e {
        HostError::Denied { by, message } => J::Obj(vec![
            ("err".to_owned(), J::Str("denied".to_owned())),
            ("by".to_owned(), J::Str(by.name().to_owned())),
            ("msg".to_owned(), J::Str(message.clone())),
        ]),
        HostError::ToolFailed { message, data } => J::Obj(vec![
            ("err".to_owned(), J::Str("tool_failed".to_owned())),
            ("msg".to_owned(), J::Str(message.value.clone())),
            ("l".to_owned(), J::Int(i64::from(message.labels.bits()))),
            (
                "data".to_owned(),
                encode(data, true).unwrap_or_else(|_| tag("u", [])),
            ),
        ]),
        HostError::ModelFailed { message } => J::Obj(vec![
            ("err".to_owned(), J::Str("model_failed".to_owned())),
            ("msg".to_owned(), J::Str(message.value.clone())),
            ("l".to_owned(), J::Int(i64::from(message.labels.bits()))),
        ]),
        HostError::Timeout { message } => text("timeout", message),
        HostError::NotFound { message } => text("not_found", message),
        HostError::Unsupported { message } => text("unsupported", message),
        HostError::Invalid { message } => text("invalid", message),
        HostError::RateLimited { message } => text("rate_limited", message),
        HostError::Cancelled(o) => text("cancelled", o.name()),
    }
}

/// Decodes a host error.
pub(crate) fn decode_error(j: &J) -> Result<HostError, String> {
    let bad = || "a malformed error on the tape".to_owned();
    let msg = j.str("msg").ok_or_else(bad)?.to_owned();
    let labels = Labels::from_bits(j.int("l").and_then(|l| u32::try_from(l).ok()).unwrap_or(0));
    Ok(match j.str("err").ok_or_else(bad)? {
        "denied" => {
            let by = j.str("by").ok_or_else(bad)?;
            let by = [
                DeniedBy::Rule,
                DeniedBy::Mode,
                DeniedBy::Floor,
                DeniedBy::User,
                DeniedBy::Hook,
                DeniedBy::Profile,
                DeniedBy::Attenuation,
                DeniedBy::Unattended,
                DeniedBy::Secrecy,
                DeniedBy::Sees,
            ]
            .into_iter()
            .find(|d| d.name() == by)
            .ok_or_else(bad)?;
            HostError::Denied { by, message: msg }
        }
        "tool_failed" => HostError::ToolFailed {
            message: Labelled::new(msg, labels),
            data: j.get("data").map(decode).transpose()?.unwrap_or_default(),
        },
        "model_failed" => HostError::ModelFailed {
            message: Labelled::new(msg, labels),
        },
        "timeout" => HostError::Timeout { message: msg },
        "not_found" => HostError::NotFound { message: msg },
        "unsupported" => HostError::Unsupported { message: msg },
        "invalid" => HostError::Invalid { message: msg },
        "rate_limited" => HostError::RateLimited { message: msg },
        "cancelled" => HostError::Cancelled(CancelOutcome::named(&msg).ok_or_else(bad)?),
        _ => return Err(bad()),
    })
}

/// The canonical text of a request: the built-in and its named arguments, without labels
/// (labels a replay adds to earlier answers must not change a request's digest).
pub(crate) fn request_text(builtin: &str, args: &[(Arc<str>, Value)]) -> String {
    let args = args
        .iter()
        .map(|(n, v)| (n.to_string(), encode(v, false).unwrap_or_else(J::Str)))
        .collect();
    J::Obj(vec![
        ("b".to_owned(), J::Str(builtin.to_owned())),
        ("a".to_owned(), J::Obj(args)),
    ])
    .text()
}
