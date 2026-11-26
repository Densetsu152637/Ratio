//! Operators (chapter 02, "Operators and precedence"; chapter 03, "Values"): equality,
//! ordering, arithmetic, membership, and the text a value shows as.
//!
//! Every operator's result carries the union of its operands' labels; the machine adds them
//! (`crate::machine`), so the functions here compute values only. Integer arithmetic is checked
//! (overflow and division by zero raise `Invalid`); `Float` follows IEEE 754 binary64, except
//! that division by zero raises `Invalid` as the operator table says for `/`.

use std::cmp::Ordering;

use ratio_ir::BinOp;

use crate::prepare::Prepared;
use crate::value::{Json, Repr, Value};

/// The steps a cost-class charge of work `w` costs: `1 + ⌈w / 64⌉` (chapter 06, `data`).
pub(crate) fn steps_for(w: u64) -> u64 {
    1 + w.div_ceil(64)
}

/// `⌈log2(n + 1)⌉`.
pub(crate) fn log_work(n: u64) -> u64 {
    u64::from(64 - n.leading_zeros())
}

/// The size of an input for the cost classes: characters of text, elements of lists, entries
/// of maps, nodes of JSON, and 1 for anything else.
pub(crate) fn size(v: &Value) -> u64 {
    match &v.repr {
        Repr::Str(s) => s.chars(),
        Repr::List(l) => l.len(),
        Repr::Map(m) => m.len(),
        Repr::Json(_) => v.weight(),
        _ => 1,
    }
}

/// Whether a value is a scalar (compared in O(1)).
pub(crate) fn is_scalar(v: &Value) -> bool {
    matches!(
        v.repr,
        Repr::Unit
            | Repr::None
            | Repr::Bool(_)
            | Repr::Int(_)
            | Repr::Float(_)
            | Repr::Duration(_)
            | Repr::Money(_)
            | Repr::Variant(..)
            | Repr::Item(_)
            | Repr::Builtin(_)
    )
}

/// Structural equality (chapter 02: "Equality is structural on values"). `Float` compares by
/// IEEE 754 (`NaN != NaN`); functions compare by identity; a record's absent optional field
/// equals `none`; JSON objects compare regardless of key order.
pub(crate) fn eq(a: &Value, b: &Value) -> bool {
    match (&a.repr, &b.repr) {
        (Repr::Unit, Repr::Unit) | (Repr::None, Repr::None) => true,
        (Repr::Bool(x), Repr::Bool(y)) => x == y,
        (Repr::Int(x), Repr::Int(y))
        | (Repr::Duration(x), Repr::Duration(y))
        | (Repr::Money(x), Repr::Money(y)) => x == y,
        #[allow(
            clippy::float_cmp,
            reason = "IEEE 754 equality is the specified semantics"
        )]
        (Repr::Float(x), Repr::Float(y)) => x == y,
        (Repr::Str(x), Repr::Str(y)) | (Repr::Tool(x), Repr::Tool(y)) => x.as_str() == y.as_str(),
        (Repr::Range(x), Repr::Range(y)) => {
            (x.start, x.end, x.inclusive) == (y.start, y.end, y.inclusive)
        }
        (Repr::Tuple(x), Repr::Tuple(y)) => {
            x.items.len() == y.items.len()
                && x.items.iter().zip(y.items.iter()).all(|(p, q)| eq(p, q))
        }
        (Repr::Record(x), Repr::Record(y)) => {
            let none = Value::none();
            let get = |r: &crate::value::RecordData, n: &str| {
                r.layout.index(n).and_then(|i| r.items.get(i)).cloned()
            };
            x.layout.names.iter().chain(y.layout.names.iter()).all(|n| {
                eq(
                    &get(x, n).unwrap_or_else(|| none.clone()),
                    &get(y, n).unwrap_or_else(|| none.clone()),
                )
            })
        }
        (Repr::List(x), Repr::List(y)) => {
            if x.len() != y.len() {
                return false;
            }
            let (mut cx, mut cy) = (x.cursor(), y.cursor());
            while let (Some(p), Some(q)) = (cx.next(), cy.next()) {
                if !eq(&p, &q) {
                    return false;
                }
            }
            true
        }
        (Repr::Map(x), Repr::Map(y)) => {
            if x.len() != y.len() {
                return false;
            }
            let (mut cx, mut cy) = (x.cursor(), y.cursor());
            while let (Some((k1, v1)), Some((k2, v2))) = (cx.next(), cy.next()) {
                if !eq(&k1, &k2) || !eq(&v1, &v2) {
                    return false;
                }
            }
            true
        }
        (Repr::Variant(e1, i1), Repr::Variant(e2, i2)) => (e1, i1) == (e2, i2),
        (Repr::VariantP(x), Repr::VariantP(y)) => {
            (x.enum_, x.index) == (y.enum_, y.index) && eq(&x.payload, &y.payload)
        }
        (Repr::Item(x), Repr::Item(y)) => x == y,
        (Repr::Builtin(x), Repr::Builtin(y)) => x == y,
        (Repr::Closure(x), Repr::Closure(y)) => std::sync::Arc::ptr_eq(x, y),
        (Repr::Json(x), Repr::Json(y)) => json_eq(&x.node, &y.node),
        (Repr::Prompt(x), Repr::Prompt(y)) => {
            x.sections.len() == y.sections.len()
                && x.sections.iter().zip(y.sections.iter()).all(|(s, t)| {
                    s.kind == t.kind
                        && s.format == t.format
                        && s.values.len() == t.values.len()
                        && s.values.iter().zip(t.values.iter()).all(|(p, q)| eq(p, q))
                })
        }
        _ => false,
    }
}

fn json_eq(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Int(x), Json::Int(y)) => x == y,
        #[allow(clippy::float_cmp, reason = "numbers compare by value")]
        (Json::Float(x), Json::Float(y)) => x == y,
        #[allow(clippy::cast_precision_loss, reason = "numbers compare by value")]
        (Json::Int(x), Json::Float(y)) | (Json::Float(y), Json::Int(x)) => (*x as f64) == *y,
        (Json::Str(x), Json::Str(y)) => x.as_str() == y.as_str(),
        (Json::Array(x), Json::Array(y)) => {
            let (vx, vy) = (x.to_vec(), y.to_vec());
            vx.len() == vy.len() && vx.iter().zip(vy.iter()).all(|(p, q)| eq(p, q))
        }
        (Json::Object(x), Json::Object(y)) => {
            let index: std::collections::HashMap<&str, &Value> = y
                .iter()
                .map(|(k, v)| (k.as_text().unwrap_or(""), v))
                .collect();
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    index
                        .get(k.as_text().unwrap_or(""))
                        .is_some_and(|v2| eq(v, v2))
                })
        }
        _ => false,
    }
}

fn kind_rank(v: &Value) -> u8 {
    match v.repr {
        Repr::None => 0,
        Repr::Unit => 1,
        Repr::Bool(_) => 2,
        Repr::Int(_) | Repr::Float(_) => 3,
        Repr::Duration(_) => 4,
        Repr::Money(_) => 5,
        Repr::Str(_) => 6,
        Repr::Variant(..) | Repr::VariantP(_) => 7,
        Repr::Tuple(_) => 8,
        Repr::List(_) => 9,
        Repr::Record(_) => 10,
        Repr::Map(_) => 11,
        _ => 12,
    }
}

fn float_cmp(x: f64, y: f64) -> Ordering {
    // NaN sorts last (chapter 06, `list.sort`).
    match (x.is_nan(), y.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
    }
}

/// The natural order, for sorting and `min`/`max`: `none` first, then numbers by value (`NaN`
/// last), text by code point, `false` before `true`, variants by declaration order then
/// payload, tuples and lists lexicographically, records field by field in name order.
#[allow(clippy::cast_precision_loss, reason = "Int and Float compare by value")]
pub(crate) fn cmp_natural(a: &Value, b: &Value) -> Ordering {
    match (&a.repr, &b.repr) {
        (Repr::Int(x), Repr::Int(y))
        | (Repr::Duration(x), Repr::Duration(y))
        | (Repr::Money(x), Repr::Money(y)) => x.cmp(y),
        (Repr::Float(x), Repr::Float(y)) => float_cmp(*x, *y),
        (Repr::Int(x), Repr::Float(y)) => float_cmp(*x as f64, *y),
        (Repr::Float(x), Repr::Int(y)) => float_cmp(*x, *y as f64),
        (Repr::Bool(x), Repr::Bool(y)) => x.cmp(y),
        (Repr::Str(x), Repr::Str(y)) => x.as_str().cmp(y.as_str()),
        (Repr::Variant(e1, i1), Repr::Variant(e2, i2)) => (e1, i1).cmp(&(e2, i2)),
        (Repr::VariantP(x), Repr::VariantP(y)) => (x.enum_, x.index)
            .cmp(&(y.enum_, y.index))
            .then_with(|| cmp_natural(&x.payload, &y.payload)),
        (Repr::Variant(e1, i1), Repr::VariantP(y)) => {
            (e1, i1).cmp(&(&y.enum_, &y.index)).then(Ordering::Less)
        }
        (Repr::VariantP(x), Repr::Variant(e2, i2)) => {
            (&x.enum_, &x.index).cmp(&(e2, i2)).then(Ordering::Greater)
        }
        (Repr::Tuple(x), Repr::Tuple(y)) => lex(x.items.iter().cloned(), y.items.iter().cloned()),
        (Repr::List(x), Repr::List(y)) => lex(x.to_vec().into_iter(), y.to_vec().into_iter()),
        (Repr::Record(x), Repr::Record(y)) => lex(x.items.iter().cloned(), y.items.iter().cloned()),
        _ => kind_rank(a).cmp(&kind_rank(b)),
    }
}

fn lex(mut a: impl Iterator<Item = Value>, mut b: impl Iterator<Item = Value>) -> Ordering {
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match cmp_natural(&x, &y) {
                Ordering::Equal => {}
                o => return o,
            },
        }
    }
}

/// Why an arithmetic operation failed.
pub(crate) enum ArithError {
    Overflow,
    DivByZero,
    /// Operands the operator does not apply to (the checker prevents this).
    Mismatch,
}

impl ArithError {
    pub(crate) fn message(&self, op: BinOp) -> String {
        match self {
            ArithError::Overflow => format!("`{}` overflowed", op.name()),
            ArithError::DivByZero => "division by zero".to_owned(),
            ArithError::Mismatch => format!("`{}` does not apply to these values", op.name()),
        }
    }
}

/// `a op b` for the numeric operators (not text or list `+`, handled by the machine).
pub(crate) fn arith(op: BinOp, a: &Value, b: &Value) -> Result<Value, ArithError> {
    use ArithError::{DivByZero, Mismatch, Overflow};
    let int = |r: Option<i64>| r.map(Value::int).ok_or(Overflow);
    match (&a.repr, &b.repr) {
        (Repr::Int(x), Repr::Int(y)) => match op {
            BinOp::Add => int(x.checked_add(*y)),
            BinOp::Sub => int(x.checked_sub(*y)),
            BinOp::Mul => int(x.checked_mul(*y)),
            BinOp::Div if *y == 0 => Err(DivByZero),
            BinOp::Div => int(x.checked_div(*y)),
            BinOp::Rem if *y == 0 => Err(DivByZero),
            BinOp::Rem => int(x.checked_rem(*y)),
            _ => Err(Mismatch),
        },
        (Repr::Float(x), Repr::Float(y)) => match op {
            BinOp::Add => Ok(Value::float(x + y)),
            BinOp::Sub => Ok(Value::float(x - y)),
            BinOp::Mul => Ok(Value::float(x * y)),
            BinOp::Div if *y == 0.0 => Err(DivByZero),
            BinOp::Div => Ok(Value::float(x / y)),
            _ => Err(Mismatch),
        },
        (Repr::Duration(x), Repr::Duration(y)) | (Repr::Money(x), Repr::Money(y)) => {
            let r = match op {
                BinOp::Add => x.checked_add(*y),
                BinOp::Sub => x.checked_sub(*y),
                _ => return Err(Mismatch),
            };
            let r = r.ok_or(Overflow)?;
            Ok(match a.repr {
                Repr::Duration(_) => Value::duration(r),
                _ => Value::money(r),
            })
        }
        (Repr::Duration(x) | Repr::Money(x), Repr::Int(y)) => {
            let r = match op {
                BinOp::Mul => x.checked_mul(*y).ok_or(Overflow)?,
                BinOp::Div if *y == 0 => return Err(DivByZero),
                BinOp::Div => x.checked_div(*y).ok_or(Overflow)?,
                _ => return Err(Mismatch),
            };
            Ok(match a.repr {
                Repr::Duration(_) => Value::duration(r),
                _ => Value::money(r),
            })
        }
        (Repr::Int(x), Repr::Duration(y) | Repr::Money(y)) if op == BinOp::Mul => {
            let r = x.checked_mul(*y).ok_or(Overflow)?;
            Ok(match b.repr {
                Repr::Duration(_) => Value::duration(r),
                _ => Value::money(r),
            })
        }
        _ => Err(Mismatch),
    }
}

/// `a < b` and the other orderings: numbers of one type, or texts.
pub(crate) fn ordered(op: BinOp, a: &Value, b: &Value) -> Option<bool> {
    let o = match (&a.repr, &b.repr) {
        (Repr::Int(x), Repr::Int(y))
        | (Repr::Duration(x), Repr::Duration(y))
        | (Repr::Money(x), Repr::Money(y)) => Some(x.cmp(y)),
        (Repr::Float(x), Repr::Float(y)) => x.partial_cmp(y),
        (Repr::Str(x), Repr::Str(y)) => Some(x.as_str().cmp(y.as_str())),
        _ => return None,
    };
    // IEEE 754: every comparison with NaN is false.
    Some(match (op, o) {
        (_, None) => false,
        (BinOp::Lt, Some(o)) => o == Ordering::Less,
        (BinOp::Le, Some(o)) => o != Ordering::Greater,
        (BinOp::Gt, Some(o)) => o == Ordering::Greater,
        (BinOp::Ge, Some(o)) => o != Ordering::Less,
        _ => return None,
    })
}

/// A duration in its literal form: the largest exact unit.
pub(crate) fn duration_text(us: i64) -> String {
    const UNITS: [(i64, &str); 4] = [
        (3_600_000_000, "h"),
        (60_000_000, "m"),
        (1_000_000, "s"),
        (1_000, "ms"),
    ];
    for (n, unit) in UNITS {
        if us % n == 0 {
            return format!("{}{unit}", us / n);
        }
    }
    format!("{us}us")
}

/// An amount in its literal form: `$1.5`, `$2`, `-$0.000001`.
pub(crate) fn money_text(micro: i64) -> String {
    let sign = if micro < 0 { "-" } else { "" };
    let abs = micro.unsigned_abs();
    let (whole, frac) = (abs / 1_000_000, abs % 1_000_000);
    if frac == 0 {
        format!("{sign}${whole}")
    } else {
        let digits = format!("{frac:06}");
        format!("{sign}${whole}.{}", digits.trim_end_matches('0'))
    }
}

/// A float in its literal form (Rust's shortest round-trip form, which keeps `.0`).
pub(crate) fn float_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        }
    } else {
        format!("{f:?}")
    }
}

/// What `text.show(v)` gives: text as is, scalars in literal form, everything else as compact
/// JSON (chapter 06, `text.show`).
pub(crate) fn show(p: &Prepared, v: &Value) -> String {
    match &v.repr {
        Repr::Str(s) => s.as_str().to_owned(),
        Repr::Unit => "()".to_owned(),
        Repr::None => "none".to_owned(),
        Repr::Bool(b) => b.to_string(),
        Repr::Int(i) => i.to_string(),
        Repr::Float(f) => float_text(*f),
        Repr::Duration(d) => duration_text(*d),
        Repr::Money(m) => money_text(*m),
        Repr::Variant(e, i) => p
            .enums
            .get(*e as usize)
            .and_then(|en| en.variants.get(*i as usize))
            .map_or_else(String::new, |v| v.name.to_string()),
        Repr::Range(r) => format!(
            "{}{}{}",
            r.start,
            if r.inclusive { "..=" } else { ".." },
            r.end
        ),
        Repr::Tool(s) => s.as_str().to_owned(),
        Repr::Item(_) | Repr::Builtin(_) | Repr::Closure(_) => "<fn>".to_owned(),
        Repr::Prompt(_) => "<prompt>".to_owned(),
        _ => {
            let mut out = String::new();
            crate::json::write(p, v, &mut out);
            out
        }
    }
}

/// An interpolation's format spec applied to `v` (chapter 02): `.2` (decimals), `>6` and `<6`
/// (alignment in a width, which may precede the decimals: `>8.2`), `json` (compact JSON).
pub(crate) fn format(p: &Prepared, v: &Value, spec: &str) -> Result<String, String> {
    if spec == "json" {
        let mut out = String::new();
        crate::json::write(p, v, &mut out);
        return Ok(out);
    }
    let bad = || format!("`{spec}` is not a format spec (`.2`, `>6`, `<6` or `json`)");
    let mut rest = spec;
    let align = match rest.chars().next() {
        Some(c @ ('<' | '>')) => {
            rest = &rest[1..];
            Some(c)
        }
        _ => None,
    };
    let (width, prec) = match rest.split_once('.') {
        Some((w, d)) => (w, Some(d)),
        None => (rest, None),
    };
    let width: Option<usize> = if width.is_empty() {
        None
    } else {
        Some(width.parse().map_err(|_| bad())?)
    };
    if align.is_some() != width.is_some() || (align.is_none() && prec.is_none()) {
        return Err(bad());
    }
    let mut text = match prec {
        Some(d) => {
            let d: usize = d.parse().map_err(|_| bad())?;
            if d > 64 {
                return Err(bad());
            }
            #[allow(clippy::cast_precision_loss, reason = "an Int shown with decimals")]
            match v.repr {
                Repr::Float(f) => format!("{f:.d$}"),
                Repr::Int(i) => format!("{:.d$}", i as f64),
                _ => return Err(format!("`{spec}` applies to numbers")),
            }
        }
        None => show(p, v),
    };
    if let (Some(a), Some(w)) = (align, width) {
        if w > 4096 {
            return Err(bad());
        }
        let len = text.chars().count();
        if len < w {
            let pad = " ".repeat(w - len);
            text = if a == '>' { pad + &text } else { text + &pad };
        }
    }
    Ok(text)
}

/// `x in r` for a range.
pub(crate) fn in_range(x: i64, r: &crate::value::RangeData) -> bool {
    x >= r.start && (if r.inclusive { x <= r.end } else { x < r.end })
}
