//! `hash` (chapter 06, `data`): digests.
//!
//! `sha256` and `blake3` hash the UTF-8 bytes of their text and return lower-case hex; both are
//! `linear` in the characters hashed, so a 16 MiB text costs about a quarter of a million steps
//! and its hashing is paid for before it runs. `stable` and `short` hash a value's **canonical
//! JSON**: object keys sorted by text (records and maps and `Json` objects alike), no spaces,
//! strings escaped as JSON does, `Int` as decimal, `Float` in its shortest round-trip form
//! (`1.0`, `0.5`, `1e21`, `1.5e-7`), `none` as `null` and a field holding `none` left out (so
//! an absent optional field and one set to `none`, which are equal values, hash alike),
//! `Duration` and `Money` as their literal text, a unit variant as its name and a variant with
//! a payload as an object with a `kind` key. Equal values hash equal on every machine and in
//! every run. A function, a prompt, a range and a non-finite `Float` have no JSON form and are
//! refused (`Invalid`).
//!
//! The canonical writer is iterative: a stack of frames as deep as the value, never the Rust
//! stack. Each object's sorted entries are charged to the heap when the object is opened, and
//! the text goes through a [`TextBuf`], which stops at 16 MiB. A digest carries the labels of
//! everything in the value.

#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the exponent of a float is small, and the sign is checked"
)]

use std::fmt::Write as _;

use super::blake3;
use super::data3::{TextBuf, charged, raise_invalid, text_value};
use super::{Args, Resume, Task, TaskStep, arg, int, opt, text, weight};
use crate::error::{Kind, RtError, raise};
use crate::heap::Charge;
use crate::heap::cost;
use crate::labels::Labels;
use crate::machine::Cx;
use crate::plist::Cursor;
use crate::prepare::Prepared;
use crate::value::{Json, MAX_DEPTH, Repr, Value};
use crate::work;

pub(super) fn sha256(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    work::add(t.len() as u64);
    text_value(
        cx,
        ratio_check::hex(&ratio_check::sha256(t.as_bytes())),
        s.labels,
    )
}

pub(super) fn blake3(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    work::add(t.len() as u64);
    text_value(cx, ratio_check::hex(&blake3::hash(t.as_bytes())), s.labels)
}

// ---- the canonical JSON ----

/// What is being written: an open container and how far it has got.
#[derive(Debug)]
enum Frame {
    /// A tuple's parts.
    Items {
        it: std::vec::IntoIter<Value>,
        first: bool,
    },
    /// A list or a `Json` array.
    List { cur: Cursor, first: bool },
    /// An object's entries, sorted by key.
    Entries {
        it: std::vec::IntoIter<(String, Value)>,
        first: bool,
        _charge: Charge,
    },
}

fn quote(cx: &mut Cx<'_>, out: &mut TextBuf, s: &str) -> Result<(), RtError> {
    out.push(cx, '"')?;
    for c in s.chars() {
        match c {
            '"' => out.push_str(cx, "\\\"")?,
            '\\' => out.push_str(cx, "\\\\")?,
            '\n' => out.push_str(cx, "\\n")?,
            '\r' => out.push_str(cx, "\\r")?,
            '\t' => out.push_str(cx, "\\t")?,
            '\u{8}' => out.push_str(cx, "\\b")?,
            '\u{c}' => out.push_str(cx, "\\f")?,
            c if u32::from(c) < 0x20 => {
                let mut esc = String::new();
                let _ = write!(esc, "\\u{:04x}", u32::from(c));
                out.push_str(cx, &esc)?;
            }
            c => out.push(cx, c)?,
        }
    }
    out.push(cx, '"')
}

/// The shortest round-trip text of a finite float, written from its shortest digits so that it
/// does not depend on how the standard library formats a `Debug` float: `1.0`, `0.001`,
/// `1e21`, `1.5e-7`. Positional notation for exponents `-5..16`, scientific otherwise.
pub(super) fn float_text(f: f64) -> String {
    // `-0.0` is `0.0`: equal values hash alike.
    if f == 0.0 {
        return "0.0".to_owned();
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::from(sign);
    if (-5..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                out.extend(std::iter::repeat_n('0', int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-exp - 1) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{exp}");
    }
    out
}

/// The text a map key is written as.
fn key_text(p: &Prepared, k: &Value) -> Option<String> {
    match &k.repr {
        Repr::Str(s) => Some(s.as_str().to_owned()),
        Repr::Int(i) => Some(i.to_string()),
        Repr::Bool(b) => Some(b.to_string()),
        Repr::Variant(e, i, _) => p
            .enums
            .get(*e as usize)
            .and_then(|en| en.variants.get(*i as usize))
            .map(|v| v.json.to_string()),
        _ => None,
    }
}

/// The memory of an object's sorted entries, charged **before** they are built: a slot and a
/// string for each of `count` entries, and the characters of the keys (`key_weight`, a bound).
fn entries_charge(cx: &Cx<'_>, count: u64, key_weight: u64) -> Result<Charge, RtError> {
    charged(
        cx,
        count
            .saturating_mul(cost::SLOT + cost::STR)
            .saturating_add(key_weight),
    )
}

/// An object's entries sorted by key (stable, so duplicate keys of a `Json` object keep their
/// order), holding the memory charge made for them.
fn entries(mut list: Vec<(String, Value)>, charge: Charge) -> Frame {
    work::add((list.len() as u64).saturating_mul(crate::ops::log_work(list.len() as u64).max(1)));
    list.sort_by(|a, b| {
        work::add(a.0.len().min(b.0.len()) as u64);
        a.0.cmp(&b.0)
    });
    Frame::Entries {
        it: list.into_iter(),
        first: true,
        _charge: charge,
    }
}

/// Writes `v`: a scalar whole, or the opening of a container (pushing its frame).
fn open(
    cx: &mut Cx<'_>,
    out: &mut TextBuf,
    stack: &mut Vec<Frame>,
    v: &Value,
    labels: Labels,
) -> Result<(), RtError> {
    work::add(1);
    let refuse = |cx: &Cx<'_>, what: &str| raise_invalid(cx, what, labels);
    match &v.repr {
        Repr::Unit | Repr::None => out.push_str(cx, "null"),
        Repr::Bool(b) => out.push_str(cx, if *b { "true" } else { "false" }),
        Repr::Int(i) => out.push_str(cx, &i.to_string()),
        Repr::Float(f) if f.is_finite() => out.push_str(cx, &float_text(*f)),
        Repr::Float(_) => Err(refuse(
            cx,
            "a Float that is not finite has no canonical JSON",
        )),
        Repr::Duration(d) => {
            let t = crate::ops::duration_text(*d);
            quote(cx, out, &t)
        }
        Repr::Money(m) => {
            let t = crate::ops::money_text(*m);
            quote(cx, out, &t)
        }
        Repr::Str(s) | Repr::Tool(s) => quote(cx, out, s.as_str()),
        Repr::Variant(e, i, _) => {
            let name =
                cx.p.enums
                    .get(*e as usize)
                    .and_then(|en| en.variants.get(*i as usize))
                    .map(|x| x.json.to_string());
            match name {
                Some(n) if cx.p.enums[*e as usize].unit_only => quote(cx, out, &n),
                Some(n) => {
                    out.push_str(cx, "{\"kind\":")?;
                    quote(cx, out, &n)?;
                    out.push(cx, '}')
                }
                None => Err(refuse(cx, "a variant of an unknown enum")),
            }
        }
        Repr::Tuple(a) => {
            let items = a.items.to_vec();
            stack.push(Frame::Items {
                it: items.into_iter(),
                first: true,
            });
            out.push(cx, '[')
        }
        Repr::List(l) => {
            stack.push(Frame::List {
                cur: l.cursor(),
                first: true,
            });
            out.push(cx, '[')
        }
        Repr::Record(r) => {
            let list: Vec<(String, Value)> = r
                .layout
                .names
                .iter()
                .zip(r.items.iter())
                .filter(|(_, x)| !matches!(x.repr, Repr::None))
                .map(|(n, x)| (n.to_string(), x.clone()))
                .collect();
            let charge = entries_charge(cx, list.len() as u64, 0)?;
            stack.push(entries(list, charge));
            out.push(cx, '{')
        }
        Repr::Map(m) => {
            // The map's weight bounds its keys' characters.
            let charge = entries_charge(cx, m.len(), v.weight())?;
            let mut list = Vec::new();
            let mut cur = m.cursor();
            while let Some((k, x)) = cur.next() {
                let Some(key) = key_text(cx.p, &k) else {
                    return Err(refuse(cx, "a map key with no JSON form"));
                };
                list.push((key, x));
            }
            stack.push(entries(list, charge));
            out.push(cx, '{')
        }
        Repr::VariantP(vd) => {
            let name =
                cx.p.enums
                    .get(vd.enum_ as usize)
                    .and_then(|en| en.variants.get(vd.index as usize))
                    .map(|x| x.json.to_string())
                    .ok_or_else(|| refuse(cx, "a variant of an unknown enum"))?;
            let mut list = vec![("kind".to_owned(), Value::text(&name))];
            if let Repr::Record(r) = &vd.payload.repr {
                for (n, x) in r.layout.names.iter().zip(r.items.iter()) {
                    if !matches!(x.repr, Repr::None) {
                        list.push((n.to_string(), x.clone()));
                    }
                }
            }
            let charge = entries_charge(cx, list.len() as u64, 0)?;
            stack.push(entries(list, charge));
            out.push(cx, '{')
        }
        Repr::Json(j) => match &j.node {
            Json::Null => out.push_str(cx, "null"),
            Json::Bool(b) => out.push_str(cx, if *b { "true" } else { "false" }),
            Json::Int(i) => out.push_str(cx, &i.to_string()),
            Json::Float(f) if f.is_finite() => out.push_str(cx, &float_text(*f)),
            Json::Float(_) => Err(refuse(
                cx,
                "a Float that is not finite has no canonical JSON",
            )),
            Json::Str(s) => quote(cx, out, s.as_str()),
            Json::Array(l) => {
                stack.push(Frame::List {
                    cur: l.cursor(),
                    first: true,
                });
                out.push(cx, '[')
            }
            Json::Object(es) => {
                let charge = entries_charge(cx, es.len() as u64, v.weight())?;
                let list = es
                    .iter()
                    .map(|(k, x)| (k.as_text().unwrap_or("").to_owned(), x.clone()))
                    .collect();
                stack.push(entries(list, charge));
                out.push(cx, '{')
            }
        },
        Repr::Range(_) | Repr::Item(_) | Repr::Builtin(_) | Repr::Closure(_) | Repr::Prompt(_) => {
            Err(refuse(
                cx,
                "a function, range or prompt has no canonical JSON",
            ))
        }
    }
}

/// The canonical JSON of a value, written a step at a time: each [`Canon::step`] writes one
/// scalar or one container's opening or closing, so a caller can pay for the work as it goes.
#[derive(Debug)]
struct Canon {
    root: Option<Value>,
    stack: Vec<Frame>,
    out: TextBuf,
    labels: Labels,
}

impl Canon {
    fn new(v: &Value) -> Canon {
        let labels = v.quick();
        Canon {
            root: Some(v.clone()),
            stack: Vec::new(),
            out: TextBuf::new(labels),
            labels,
        }
    }

    /// Writes the next piece; `true` when the whole value is written.
    fn step(&mut self, cx: &mut Cx<'_>) -> Result<bool, RtError> {
        let labels = self.labels;
        if let Some(root) = self.root.take() {
            open(cx, &mut self.out, &mut self.stack, &root, labels)?;
            return Ok(self.stack.is_empty());
        }
        if self.stack.len() > MAX_DEPTH as usize + 2 {
            return Err(raise_invalid(
                cx,
                "values nested more than 256 deep",
                labels,
            ));
        }
        let Some(top) = self.stack.last_mut() else {
            return Ok(true);
        };
        // The next thing to write inside the top frame, with the separator it needs.
        let step = match top {
            Frame::Items { it, first } => it.next().map(|x| (std::mem::take(first), None, x)),
            Frame::List { cur, first } => cur.next().map(|x| (std::mem::take(first), None, x)),
            Frame::Entries { it, first, .. } => {
                it.next().map(|(k, x)| (std::mem::take(first), Some(k), x))
            }
        };
        if let Some((was_first, key, x)) = step {
            if !was_first {
                self.out.push(cx, ',')?;
            }
            if let Some(k) = key {
                quote(cx, &mut self.out, &k)?;
                self.out.push(cx, ':')?;
            }
            // Reading an element out of a container adds the container's own labels, which
            // the digest carries already.
            open(cx, &mut self.out, &mut self.stack, &x, labels)?;
        } else {
            let closer = if matches!(self.stack.pop(), Some(Frame::Entries { .. })) {
                '}'
            } else {
                ']'
            };
            self.out.push(cx, closer)?;
        }
        Ok(self.stack.is_empty())
    }
}

/// Work between two looks at the cancellation flag.
const CHECK_UNITS: u64 = 64 * 1000;

/// The units of work the charge covers beyond `n`.
const SLACK: u64 = 4096;

/// `stable` and `short`: the digest of the canonical JSON.
///
/// The class says `linear` in the value's weight, but a number weighs 1 and writes up to 24
/// bytes, and the bytes are the work. So the walk pays for what it writes, as `text.show`
/// does: when the work done (a unit for each value visited and for each byte written) passes
/// what the charge covers (`n` plus 4,096), it asks the evaluator to debit as many units again
/// as are covered so far (a step per 64), in doubling instalments, before going on. A value
/// of text and small structure never pays more than the class says.
#[derive(Debug)]
struct Stable {
    canon: Canon,
    n_pre: u64,
    work: u64,
    extra: u64,
    since_check: u64,
    /// `short`: the number of hex digits wanted, and the labels of the count.
    short: Option<(usize, Labels)>,
}

impl Task for Stable {
    fn resume(&mut self, cx: &mut Cx<'_>, _input: Resume) -> Result<TaskStep, RtError> {
        match self.run(cx) {
            // A walk that fails pays for what it did, past the class's charge.
            Err(e) => {
                cx.debit = self
                    .work
                    .saturating_sub(self.n_pre.saturating_add(self.extra))
                    .div_ceil(64);
                Err(e)
            }
            ok => ok,
        }
    }
}

impl Stable {
    fn run(&mut self, cx: &mut Cx<'_>) -> Result<TaskStep, RtError> {
        loop {
            let covered = self.n_pre.saturating_add(SLACK).saturating_add(self.extra);
            if self.work > covered {
                let steps = covered.max(64 * 1024).div_ceil(64);
                self.extra = self.extra.saturating_add(steps.saturating_mul(64));
                return Ok(TaskStep::Charge(steps));
            }
            if self.since_check >= CHECK_UNITS {
                self.since_check = 0;
                if cx.cancel.is_cancelled() {
                    return Err(raise(
                        cx.p,
                        Kind::Cancelled,
                        "the evaluation was cancelled",
                        Labels::NONE,
                        cx.loc,
                    ));
                }
            }
            let before = self.canon.out.len();
            let done = self.canon.step(cx)?;
            let units = 1 + (self.canon.out.len() - before) as u64;
            self.work += units;
            self.since_check += units;
            if done {
                break;
            }
        }
        let labels = self.canon.labels;
        let out = std::mem::replace(&mut self.canon.out, TextBuf::new(Labels::NONE));
        let text = out.finish(cx)?;
        let bytes = text.as_text().unwrap_or("");
        work::add(bytes.len() as u64);
        let hex = ratio_check::hex(&ratio_check::sha256(bytes.as_bytes()));
        let result = match self.short {
            None => format!("sha256:{hex}"),
            Some((take, _)) => hex[..take].to_owned(),
        };
        let labels = labels | self.short.map_or(Labels::NONE, |(_, l)| l);
        Ok(TaskStep::Done(text_value(cx, result, labels)?))
    }
}

pub(super) fn stable(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    let v = arg(cx, args, 0)?;
    Ok(Box::new(Stable {
        canon: Canon::new(v),
        n_pre: weight(args, 0),
        work: 0,
        extra: 0,
        since_check: 0,
        short: None,
    }))
}

pub(super) fn short(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    let v = arg(cx, args, 0)?;
    let n = opt(args, 1);
    let want = match n {
        Some(n) => int(cx, n)?,
        None => 8,
    };
    // 0 to 64 hex digits: a count outside that is clamped.
    let take = usize::try_from(want.clamp(0, 64)).unwrap_or(0);
    Ok(Box::new(Stable {
        canon: Canon::new(v),
        n_pre: weight(args, 0),
        work: 0,
        extra: 0,
        since_check: 0,
        short: Some((take, n.map_or(Labels::NONE, |n| n.labels))),
    }))
}

#[cfg(test)]
mod tests {
    use super::float_text;

    #[test]
    fn floats_are_written_from_their_shortest_digits() {
        for (f, want) in [
            (1.0, "1.0"),
            (-1.0, "-1.0"),
            (0.5, "0.5"),
            (123.456, "123.456"),
            (100.0, "100.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e16"),
            (1.5e16, "1.5e16"),
            (1e21, "1e21"),
            (0.001, "0.001"),
            (0.00001, "0.00001"),
            (0.000_001, "1e-6"),
            (1.5e-7, "1.5e-7"),
            (-2.5e-10, "-2.5e-10"),
            (0.1 + 0.2, "0.30000000000000004"),
            (f64::MAX, "1.7976931348623157e308"),
            (5e-324, "5e-324"),
            (0.0, "0.0"),
        ] {
            assert_eq!(float_text(f), want);
        }
    }
}
