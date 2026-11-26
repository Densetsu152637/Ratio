//! `Json` values and the conversions that keep secrecy labels on their leaves (chapter 03,
//! "Secrecy": `json.encode` and `json.stringify` of a labelled value give labelled JSON, and
//! `json.decode` keeps them).
//!
//! A JSON node is a [`Value`] whose own labels are the node's; reading a child out of an
//! array or object adds the container's own labels, as for every container. `parse` labels
//! every node with the text's labels; `encode` gives each node the labels of the value it came
//! from; `decode` gives each value the labels of the node it came from; `write` renders text
//! whose labels the caller sets from the value's.

use std::fmt::Write as _;

use ratio_ir::Type;

use crate::heap::Heap;
use crate::labels::Labels;
use crate::plist::List;
use crate::prepare::Prepared;
use crate::value::{Fault, Json, MAX_DEPTH, Repr, Str, Value};

/// The deepest JSON `parse` and `decode` accept (chapter 07: 128).
pub const MAX_JSON_DEPTH: usize = 128;

pub(crate) fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub(crate) fn number(f: f64, out: &mut String) {
    if f.is_finite() {
        let _ = write!(out, "{f:?}");
    } else {
        out.push_str("null");
    }
}

fn key_text(p: &Prepared, k: &Value) -> String {
    match &k.repr {
        Repr::Str(s) => s.as_str().to_owned(),
        Repr::Variant(e, i, _) => p
            .enums
            .get(*e as usize)
            .and_then(|en| en.variants.get(*i as usize))
            .map_or_else(String::new, |v| v.json.to_string()),
        _ => crate::ops::show(p, k),
    }
}

/// Compact JSON for any value, without its type (for `text.show`, the `json` format spec and
/// printing results): records as objects with every field (`none` as `null`), tuples and
/// lists as arrays, maps as objects, variants as their `snake_case` names or `{"kind": …}`
/// objects, durations and money as their literal text.
/// Whether the text being written is already longer than any string may be: the writers
/// stop there, so rendering a large or widely shared value never builds more than that (the
/// caller then refuses the result as too long).
pub(crate) fn full(out: &str) -> bool {
    out.len() > crate::value::MAX_STR_BYTES
}

pub(crate) fn write(p: &Prepared, v: &Value, out: &mut String) {
    if full(out) {
        return;
    }
    match &v.repr {
        Repr::Unit | Repr::None => out.push_str("null"),
        Repr::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Repr::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Repr::Float(f) => number(*f, out),
        Repr::Duration(d) => quote(&crate::ops::duration_text(*d), out),
        Repr::Money(m) => quote(&crate::ops::money_text(*m), out),
        Repr::Str(s) | Repr::Tool(s) => quote(s.as_str(), out),
        Repr::Range(r) => {
            let _ = write!(out, "[{},{}]", r.start, r.end);
        }
        Repr::Tuple(a) => array(p, a.items.iter(), out),
        Repr::List(l) => list_array(p, l, out),
        Repr::Record(r) => {
            out.push('{');
            for (i, (n, x)) in r.layout.names.iter().zip(r.items.iter()).enumerate() {
                if full(out) {
                    return;
                }
                if i > 0 {
                    out.push(',');
                }
                quote(n, out);
                out.push(':');
                write(p, x, out);
            }
            out.push('}');
        }
        Repr::Map(m) => {
            out.push('{');
            let mut cursor = m.cursor();
            let mut first = true;
            while let Some((k, x)) = cursor.next() {
                if full(out) {
                    return;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                quote(&key_text(p, &k), out);
                out.push(':');
                write(p, &x, out);
            }
            out.push('}');
        }
        Repr::Variant(e, i, _) => {
            let en = p.enums.get(*e as usize);
            let name = en
                .and_then(|en| en.variants.get(*i as usize))
                .map_or_else(String::new, |v| v.json.to_string());
            if en.is_some_and(|en| en.unit_only) {
                quote(&name, out);
            } else {
                out.push_str("{\"kind\":");
                quote(&name, out);
                out.push('}');
            }
        }
        Repr::VariantP(vd) => {
            let name = p
                .enums
                .get(vd.enum_ as usize)
                .and_then(|en| en.variants.get(vd.index as usize))
                .map_or_else(String::new, |v| v.json.to_string());
            out.push_str("{\"kind\":");
            quote(&name, out);
            if let Repr::Record(r) = &vd.payload.repr {
                for (n, x) in r.layout.names.iter().zip(r.items.iter()) {
                    out.push(',');
                    quote(n, out);
                    out.push(':');
                    write(p, x, out);
                }
            }
            out.push('}');
        }
        Repr::Json(j) => write_json(p, &j.node, out),
        Repr::Item(_) | Repr::Builtin(_) | Repr::Closure(_) => quote("<fn>", out),
        Repr::Prompt(_) => quote("<prompt>", out),
    }
}

fn array<'a>(p: &Prepared, items: impl Iterator<Item = &'a Value>, out: &mut String) {
    out.push('[');
    for (i, x) in items.enumerate() {
        if full(out) {
            return;
        }
        if i > 0 {
            out.push(',');
        }
        write(p, x, out);
    }
    out.push(']');
}

fn list_array(p: &Prepared, l: &List, out: &mut String) {
    out.push('[');
    let mut cursor = l.cursor();
    let mut first = true;
    while let Some(x) = cursor.next() {
        if full(out) {
            return;
        }
        if !first {
            out.push(',');
        }
        first = false;
        write(p, &x, out);
    }
    out.push(']');
}

fn write_json(p: &Prepared, j: &Json, out: &mut String) {
    match j {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Json::Float(f) => number(*f, out),
        Json::Str(s) => quote(s.as_str(), out),
        Json::Array(l) => list_array(p, l, out),
        Json::Object(entries) => {
            out.push('{');
            for (i, (k, x)) in entries.iter().enumerate() {
                if full(out) {
                    return;
                }
                if i > 0 {
                    out.push(',');
                }
                quote(k.as_text().unwrap_or(""), out);
                out.push(':');
                write(p, x, out);
            }
            out.push('}');
        }
    }
}

// ---- parse ----

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    heap: &'a Heap,
    labels: Labels,
}

/// Parses JSON text; every node carries `labels`. The error names the line and column.
pub(crate) fn parse(heap: &Heap, text: &str, labels: Labels) -> Result<Value, Fault> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        heap,
        labels,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.fail("text after the JSON value"));
    }
    Ok(v)
}

impl Parser<'_> {
    fn fail(&self, what: &str) -> Fault {
        let before = &self.s[..self.i.min(self.s.len())];
        let line = before.split(|b| *b == b'\n').count();
        let col = before.iter().rev().take_while(|b| **b != b'\n').count() + 1;
        Fault::Invalid(format!("invalid JSON at {line}:{col}: {what}"))
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.s.get(self.i) {
            self.i += 1;
        }
    }

    fn node(&self, node: Json) -> Result<Value, Fault> {
        Ok(Value::new_json(self.heap, node)?.with_labels(self.labels))
    }

    fn lit(&mut self, word: &str, node: Json) -> Result<Value, Fault> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            self.node(node)
        } else {
            Err(self.fail("an unknown word"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Fault> {
        if depth >= MAX_JSON_DEPTH {
            return Err(self.fail("nested deeper than 128"));
        }
        match self.s.get(self.i) {
            Some(b'n') => self.lit("null", Json::Null),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'"') => {
                let s = self.string()?;
                let s = Str::new(self.heap, s)?;
                self.node(Json::Str(s))
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        items.push(self.value(depth + 1)?);
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.fail("expected `,` or `]`")),
                        }
                    }
                }
                let list = List::from_vec(self.heap, items)?;
                self.node(Json::Array(list))
            }
            Some(b'{') => {
                self.i += 1;
                let mut entries: Vec<(Value, Value)> = Vec::new();
                let mut seen: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.s.get(self.i) != Some(&b'"') {
                            return Err(self.fail("expected a key"));
                        }
                        let k = self.string()?;
                        let k = Value::new_text(self.heap, k)?.with_labels(self.labels);
                        self.ws();
                        if self.s.get(self.i) != Some(&b':') {
                            return Err(self.fail("expected `:`"));
                        }
                        self.i += 1;
                        self.ws();
                        let v = self.value(depth + 1)?;
                        // A repeated key: the later value wins, in the first one's place.
                        let key = k.as_text().unwrap_or("").to_owned();
                        if let Some(&at) = seen.get(&key) {
                            entries[at].1 = v;
                        } else {
                            if entries.len() as u64 >= crate::value::MAX_ELEMS {
                                return Err(Fault::TooLarge(
                                    "an object of more than 1,000,000 keys",
                                ));
                            }
                            seen.insert(key, entries.len());
                            entries.push((k, v));
                        }
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.fail("expected `,` or `}`")),
                        }
                    }
                }
                self.node(Json::Object(entries.into_boxed_slice()))
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.fail("expected a value")),
        }
    }

    fn number(&mut self) -> Result<Value, Fault> {
        let start = self.i;
        if self.s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let from = p.i;
            while let Some(b'0'..=b'9') = p.s.get(p.i) {
                p.i += 1;
            }
            p.i - from
        };
        let int_digits = digits(self);
        if int_digits == 0 || (int_digits > 1 && self.s[self.i - int_digits] == b'0') {
            return Err(self.fail("a malformed number"));
        }
        let mut float = false;
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            float = true;
            if digits(self) == 0 {
                return Err(self.fail("a malformed number"));
            }
        }
        if let Some(b'e' | b'E') = self.s.get(self.i) {
            self.i += 1;
            float = true;
            if let Some(b'+' | b'-') = self.s.get(self.i) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return Err(self.fail("a malformed number"));
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i])
            .map_err(|_| self.fail("a malformed number"))?;
        if !float && let Ok(i) = text.parse::<i64>() {
            return self.node(Json::Int(i));
        }
        match text.parse::<f64>() {
            Ok(f) if f.is_finite() => self.node(Json::Float(f)),
            _ => Err(self.fail("a number out of range")),
        }
    }

    fn hex4(&mut self) -> Result<u32, Fault> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.fail("a short \\u escape"))?;
        // Exactly four hexadecimal digits (`from_str_radix` would also take a sign).
        if !h.iter().all(u8::is_ascii_hexdigit) {
            return Err(self.fail("a bad \\u escape"));
        }
        let h = std::str::from_utf8(h).map_err(|_| self.fail("a bad \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.fail("a bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, Fault> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(&b) = self.s.get(self.i) {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(
                std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.fail("bad UTF-8"))?,
            );
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = match self.s.get(self.i) {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.i += 1;
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                if self.s.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err(self.fail("a lone surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.fail("a lone surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            let c = char::from_u32(code)
                                .ok_or_else(|| self.fail("a lone surrogate"))?;
                            out.push(c);
                            continue;
                        }
                        _ => return Err(self.fail("an unknown escape")),
                    };
                    self.i += 1;
                    out.push(c);
                }
                _ => return Err(self.fail("an unterminated string")),
            }
        }
    }
}

// ---- encode and decode ----

/// Type parameters bound while walking a generic enum's payload.
type Env<'a> = &'a [(String, u32)];

fn resolve<'a>(p: &'a Prepared, mut ty: u32, env: Env<'_>) -> Option<&'a Type> {
    for _ in 0..64 {
        match p.types.get(ty as usize)? {
            Type::Param(name) => ty = env.iter().find(|(n, _)| n == name)?.1,
            t => return Some(t),
        }
    }
    None
}

fn enum_env(p: &Prepared, e: u32, args: &[u32]) -> Vec<(String, u32)> {
    p.enums
        .get(e as usize)
        .map(|en| {
            en.params
                .iter()
                .cloned()
                .zip(args.iter().copied())
                .collect()
        })
        .unwrap_or_default()
}

/// `json.encode::<T>(v)`: a schema-able value as `Json`, each node labelled as the value it
/// came from (chapter 03).
pub(crate) fn encode(
    p: &Prepared,
    heap: &Heap,
    v: &Value,
    ty: u32,
    env: Env<'_>,
    depth: u32,
) -> Result<Value, Fault> {
    if depth > MAX_DEPTH {
        return Err(Fault::Depth);
    }
    let labels = v.labels;
    let t = resolve(p, ty, env).cloned().unwrap_or(Type::Error);
    let node = match (&t, &v.repr) {
        (Type::Option(_), Repr::None) => Json::Null,
        (Type::Option(inner) | Type::Untrusted(inner) | Type::Secret(inner), _) => {
            return encode(p, heap, v, *inner, env, depth);
        }
        (Type::Json, Repr::Json(_)) => return Ok(v.clone()),
        (_, Repr::Str(s)) => Json::Str(s.clone()),
        (_, Repr::Int(i)) => Json::Int(*i),
        (_, Repr::Float(f)) if f.is_finite() => Json::Float(*f),
        (_, Repr::Float(_)) => {
            return Err(Fault::Invalid(
                "a non-finite Float has no JSON form".to_owned(),
            ));
        }
        (_, Repr::Bool(b)) => Json::Bool(*b),
        (_, Repr::Unit | Repr::None) => Json::Null,
        (_, Repr::Duration(d)) => Json::Str(Str::new(heap, crate::ops::duration_text(*d))?),
        (_, Repr::Money(m)) => Json::Str(Str::new(heap, crate::ops::money_text(*m))?),
        (Type::List(elem), Repr::List(l)) => {
            let mut items = Vec::new();
            for x in l.to_vec() {
                items.push(encode(p, heap, &x, *elem, env, depth + 1)?);
            }
            Json::Array(List::from_vec(heap, items)?)
        }
        (Type::Tuple(ts), Repr::Tuple(a)) => {
            let mut items = Vec::new();
            for (x, t) in a.items.iter().zip(ts.iter()) {
                items.push(encode(p, heap, x, *t, env, depth + 1)?);
            }
            Json::Array(List::from_vec(heap, items)?)
        }
        (Type::Record(fields), Repr::Record(r)) => {
            let mut entries = Vec::new();
            for f in fields {
                let x = r
                    .layout
                    .index(&f.name)
                    .and_then(|i| r.items.get(i))
                    .cloned()
                    .unwrap_or_else(Value::none);
                if f.optional && x.is_none() {
                    continue;
                }
                let k = Value::new_text(heap, f.name.clone())?;
                entries.push((k, encode(p, heap, &x, f.ty, env, depth + 1)?));
            }
            Json::Object(entries.into_boxed_slice())
        }
        (Type::Map(_, vt), Repr::Map(m)) => {
            let mut entries = Vec::new();
            let mut pairs = Vec::new();
            m.for_each(&mut |k, x| pairs.push((k.clone(), x.clone())));
            for (k, x) in pairs {
                let key = Value::new_text(heap, key_text(p, &k))?.with_labels(k.labels);
                entries.push((key, encode(p, heap, &x, *vt, env, depth + 1)?));
            }
            Json::Object(entries.into_boxed_slice())
        }
        (Type::Enum(e, args), Repr::Variant(_, i, _)) => {
            let name = variant_json(p, *e, *i);
            let unit_only = p.enums.get(*e as usize).is_some_and(|en| en.unit_only);
            let _ = args;
            if unit_only {
                Json::Str(Str::new(heap, name)?)
            } else {
                let k = Value::new_text(heap, "kind".to_owned())?;
                let n = Value::new_json(heap, Json::Str(Str::new(heap, name)?))?;
                Json::Object(vec![(k, n)].into_boxed_slice())
            }
        }
        (Type::Enum(e, args), Repr::VariantP(vd)) => {
            let env2 = enum_env(p, *e, args);
            let name = variant_json(p, vd.enum_, vd.index);
            let k = Value::new_text(heap, "kind".to_owned())?;
            let n = Value::new_json(heap, Json::Str(Str::new(heap, name)?))?;
            let mut entries = vec![(k, n)];
            let payload_ty = payload_type(p, *e, vd.index);
            if let (Some(pt), Repr::Record(r)) = (payload_ty, &vd.payload.repr)
                && let Some(Type::Record(fields)) = p.types.get(pt as usize)
            {
                for f in fields {
                    let x = r
                        .layout
                        .index(&f.name)
                        .and_then(|i| r.items.get(i))
                        .cloned()
                        .unwrap_or_else(Value::none)
                        .with_labels(vd.payload.labels);
                    if f.optional && x.is_none() {
                        continue;
                    }
                    let key = Value::new_text(heap, f.name.clone())?;
                    entries.push((key, encode(p, heap, &x, f.ty, &env2, depth + 1)?));
                }
            }
            Json::Object(entries.into_boxed_slice())
        }
        (_, Repr::Json(_)) => return Ok(v.clone()),
        _ => {
            return Err(Fault::Invalid(format!(
                "a {} has no JSON form",
                v.kind_name()
            )));
        }
    };
    Ok(Value::new_json(heap, node)?.with_labels(labels))
}

fn variant_json(p: &Prepared, e: u32, i: u32) -> String {
    p.enums
        .get(e as usize)
        .and_then(|en| en.variants.get(i as usize))
        .map_or_else(String::new, |v| v.json.to_string())
}

/// The payload record type of a variant, from the program's enum table.
fn payload_type(p: &Prepared, e: u32, i: u32) -> Option<u32> {
    p.enums
        .get(e as usize)?
        .variants
        .get(i as usize)?
        .payload_ty
}

/// The labels of a JSON value and its children reached so far.
struct Decoder<'a> {
    p: &'a Prepared,
    heap: &'a Heap,
    violations: Vec<String>,
    /// Where the walk is, as segments: rendered into a JSON pointer only for a violation, so
    /// decoding costs nothing per element for its paths (a key may be megabytes long).
    path: Vec<Seg>,
}

/// One step of a JSON pointer.
enum Seg {
    Index(usize),
    /// An object key, as the JSON holds it.
    Key(Value),
}

/// The longest a segment is shown in a violation.
const SEGMENT_SHOWN: usize = 64;

/// `json.decode::<T>(j)`: the value of type `T` the JSON holds, each part labelled as the node
/// it came from; or every violation, with its JSON pointer.
pub(crate) fn decode(
    p: &Prepared,
    heap: &Heap,
    j: &Value,
    ty: u32,
) -> Result<Result<Value, Vec<String>>, Fault> {
    let mut d = Decoder {
        p,
        heap,
        violations: Vec::new(),
        path: Vec::new(),
    };
    let v = d.value(j, ty, &[], 0)?;
    Ok(match v {
        Some(v) if d.violations.is_empty() => Ok(v),
        _ => Err(d.violations),
    })
}

impl Decoder<'_> {
    /// Records a violation at the current path (plus `extra`, a missing or unknown field).
    fn bad_at(&mut self, extra: Option<&str>, what: &str) -> Option<Value> {
        if self.violations.len() < 100 {
            let mut at = String::new();
            let shown = |s: &str, at: &mut String| {
                at.push('/');
                // Only the head of a long key is shown, and only it is escaped.
                let head: String = s.chars().take(SEGMENT_SHOWN).collect();
                at.push_str(&pointer_escape(&head));
                if s.len() > head.len() {
                    at.push('…');
                }
            };
            for seg in &self.path {
                match seg {
                    Seg::Index(i) => {
                        let _ = write!(at, "/{i}");
                    }
                    Seg::Key(k) => shown(k.as_text().unwrap_or(""), &mut at),
                }
            }
            if let Some(e) = extra {
                shown(e, &mut at);
            }
            if at.is_empty() {
                at.push('/');
            }
            self.violations.push(format!("{at}: {what}"));
        }
        None
    }

    fn bad(&mut self, what: &str) -> Option<Value> {
        self.bad_at(None, what)
    }

    /// Decodes `j` as the child at `seg`.
    fn child(
        &mut self,
        seg: Seg,
        j: &Value,
        ty: u32,
        env: Env<'_>,
        depth: usize,
    ) -> Result<Option<Value>, Fault> {
        self.path.push(seg);
        let out = self.value(j, ty, env, depth);
        self.path.pop();
        out
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of type")]
    fn value(
        &mut self,
        j: &Value,
        ty: u32,
        env: Env<'_>,
        depth: usize,
    ) -> Result<Option<Value>, Fault> {
        if depth > MAX_JSON_DEPTH {
            return Ok(self.bad("nested deeper than 128"));
        }
        let Repr::Json(node) = &j.repr else {
            return Ok(self.bad("not JSON"));
        };
        let labels = j.labels;
        let t = resolve(self.p, ty, env).cloned().unwrap_or(Type::Error);
        let heap = self.heap;
        let out = match (&t, &node.node) {
            (Type::Json, _) => Some(j.clone()),
            (Type::Untrusted(inner) | Type::Secret(inner), _) => {
                return self.value(j, *inner, env, depth);
            }
            (Type::Option(_), Json::Null) => Some(Value::none().with_labels(labels)),
            (Type::Option(inner), _) => return self.value(j, *inner, env, depth),
            (Type::Unit, Json::Null) => Some(Value::unit().with_labels(labels)),
            (Type::Bool, Json::Bool(b)) => Some(Value::bool(*b).with_labels(labels)),
            (Type::Int, Json::Int(i)) => Some(Value::int(*i).with_labels(labels)),
            #[allow(
                clippy::cast_possible_truncation,
                reason = "checked to be integral and in range"
            )]
            (Type::Int, Json::Float(f)) if f.fract() == 0.0 && f.abs() < 9.0e18 => {
                Some(Value::int(*f as i64).with_labels(labels))
            }
            #[allow(clippy::cast_precision_loss, reason = "a JSON integer read as a Float")]
            (Type::Float, Json::Int(i)) => Some(Value::float(*i as f64).with_labels(labels)),
            (Type::Float, Json::Float(f)) => Some(Value::float(*f).with_labels(labels)),
            (Type::Str | Type::UserText | Type::Brief | Type::UntrustedStr, Json::Str(s)) => {
                Some(Value::from_str(s.clone()).with_labels(labels))
            }
            (Type::Duration, Json::Str(s)) => match parse_duration(s.as_str()) {
                Some(d) => Some(Value::duration(d).with_labels(labels)),
                None => self.bad("not a duration"),
            },
            (Type::Money, Json::Str(s)) => match parse_money(s.as_str()) {
                Some(m) => Some(Value::money(m).with_labels(labels)),
                None => self.bad("not an amount"),
            },
            (Type::List(elem), Json::Array(l)) => {
                let mut items = Vec::new();
                let mut ok = true;
                let mut cursor = l.cursor();
                let mut i = 0;
                while let Some(x) = cursor.next() {
                    let got =
                        self.child(Seg::Index(i), &x.with_labels(labels), *elem, env, depth + 1)?;
                    i += 1;
                    match got {
                        Some(v) => items.push(v),
                        None => ok = false,
                    }
                }
                if ok {
                    Some(Value::new_list(List::from_vec(heap, items)?).with_labels(labels))
                } else {
                    None
                }
            }
            (Type::Tuple(ts), Json::Array(l)) => {
                let xs = l.to_vec();
                if xs.len() == ts.len() {
                    let mut items = Vec::new();
                    let mut ok = true;
                    for (i, (x, t)) in xs.iter().zip(ts.iter()).enumerate() {
                        match self.child(
                            Seg::Index(i),
                            &x.clone().with_labels(labels),
                            *t,
                            env,
                            depth + 1,
                        )? {
                            Some(v) => items.push(v),
                            None => ok = false,
                        }
                    }
                    if ok {
                        Some(Value::new_tuple(heap, items)?.with_labels(labels))
                    } else {
                        None
                    }
                } else {
                    self.bad(&format!("expected {} elements", ts.len()))
                }
            }
            (Type::Record(fields), Json::Object(entries)) => {
                self.record(entries, fields, env, depth, labels)?
            }
            (Type::Map(kt, vt), Json::Object(entries)) => {
                let mut m = crate::pmap::Map::empty();
                let mut ok = true;
                for (k, x) in entries {
                    let ktext = k.as_text().unwrap_or("");
                    let key = match resolve(self.p, *kt, env) {
                        Some(Type::Int) => ktext.parse::<i64>().ok().map(Value::int),
                        Some(Type::Bool) => match ktext {
                            "true" => Some(Value::bool(true)),
                            "false" => Some(Value::bool(false)),
                            _ => None,
                        },
                        Some(Type::Enum(e, _)) => self.unit_variant(*e, ktext),
                        _ => Some(k.clone()),
                    };
                    let Some(key) = key else {
                        self.bad_at(Some(ktext), "not a key of the map's key type");
                        ok = false;
                        continue;
                    };
                    let key = key.with_labels(k.labels | labels);
                    match self.child(
                        Seg::Key(k.clone()),
                        &x.clone().with_labels(labels),
                        *vt,
                        env,
                        depth + 1,
                    )? {
                        Some(v) => m = m.insert(heap, key, v)?,
                        None => ok = false,
                    }
                }
                if ok {
                    Some(Value::new_map(m).with_labels(labels))
                } else {
                    None
                }
            }
            (Type::Enum(e, _), Json::Str(s))
                if self.p.enums.get(*e as usize).is_some_and(|en| en.unit_only) =>
            {
                match self.unit_variant(*e, s.as_str()) {
                    Some(v) => Some(v.with_labels(labels)),
                    None => self.bad("not one of the enum's names"),
                }
            }
            (Type::Enum(e, args), Json::Object(entries)) => {
                let kind = entries.iter().find(|(k, _)| k.as_text() == Some("kind"));
                // The variant is chosen by the `kind` node: its labels (and its key's) reach
                // the result.
                let chosen = kind.map_or(Labels::NONE, |(k, v)| k.labels | v.quick());
                let labels = labels | chosen;
                let name = kind.and_then(|(_, v)| match &v.repr {
                    Repr::Json(n) => match &n.node {
                        Json::Str(s) => Some(s.as_str().to_owned()),
                        _ => None,
                    },
                    _ => None,
                });
                let en = self.p.enums.get(*e as usize);
                let index = name
                    .as_deref()
                    .and_then(|n| en.and_then(|en| en.variants.iter().position(|v| &*v.json == n)))
                    .and_then(|i| u32::try_from(i).ok());
                match index {
                    None => self.bad("no variant of that `kind`"),
                    Some(i) => match payload_type(self.p, *e, i) {
                        None => Some(
                            Value::new_variant(heap, *e, i, self.p.variant_weight(*e, i), None)?
                                .with_labels(labels),
                        ),
                        Some(pt) => {
                            let env2 = enum_env(self.p, *e, args);
                            let rest: Vec<(Value, Value)> = entries
                                .iter()
                                .filter(|(k, _)| k.as_text() != Some("kind"))
                                .cloned()
                                .collect();
                            match resolve(self.p, pt, &env2).cloned() {
                                Some(Type::Record(fields)) => {
                                    match self.record(&rest, &fields, &env2, depth, Labels::NONE)? {
                                        Some(payload) => Some(
                                            Value::new_variant(
                                                heap,
                                                *e,
                                                i,
                                                self.p.variant_weight(*e, i),
                                                Some(payload),
                                            )?
                                            .with_labels(labels),
                                        ),
                                        None => None,
                                    }
                                }
                                _ => self.bad("a variant without a record payload"),
                            }
                        }
                    },
                }
            }
            _ => self.bad("the wrong kind of value"),
        };
        Ok(out)
    }

    fn unit_variant(&self, e: u32, name: &str) -> Option<Value> {
        let en = self.p.enums.get(e as usize)?;
        let i = en.variants.iter().position(|v| &*v.json == name)?;
        let i = u32::try_from(i).ok()?;
        Value::new_variant(&Heap::unmetered(), e, i, self.p.variant_weight(e, i), None).ok()
    }

    fn record(
        &mut self,
        entries: &[(Value, Value)],
        fields: &[ratio_ir::FieldType],
        env: Env<'_>,
        depth: usize,
        labels: Labels,
    ) -> Result<Option<Value>, Fault> {
        let mut items = Vec::with_capacity(fields.len());
        let mut ok = true;
        for f in fields {
            match entries
                .iter()
                .find(|(k, _)| k.as_text() == Some(f.name.as_str()))
            {
                Some((key, x)) => {
                    // The field was found by its key: the key's labels reach its value.
                    let x = x.clone().with_labels(labels | key.labels);
                    match self.child(Seg::Key(key.clone()), &x, f.ty, env, depth + 1)? {
                        Some(v) => items.push(v),
                        None => ok = false,
                    }
                }
                None if f.optional => items.push(Value::none()),
                None => {
                    self.bad_at(Some(&f.name), "a required field is missing");
                    ok = false;
                }
            }
        }
        for (k, _) in entries {
            let name = k.as_text().unwrap_or("");
            if !fields.iter().any(|f| f.name == name) {
                self.bad_at(Some(name), "a field the type does not have");
                ok = false;
            }
        }
        if !ok {
            return Ok(None);
        }
        let layout = std::sync::Arc::new(crate::value::Layout {
            names: fields
                .iter()
                .map(|f| std::sync::Arc::from(f.name.as_str()))
                .collect(),
        });
        Ok(Some(
            Value::new_record(self.heap, layout, items)?.with_labels(labels),
        ))
    }
}

fn pointer_escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

fn parse_duration(s: &str) -> Option<i64> {
    let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = digits.parse().ok()?;
    let per = match unit {
        "ms" => 1_000,
        "s" => 1_000_000,
        "m" => 60_000_000,
        "h" => 3_600_000_000,
        _ => return None,
    };
    n.checked_mul(per)
}

fn parse_money(s: &str) -> Option<i64> {
    let rest = s.strip_prefix('$')?;
    let (whole, frac) = rest.split_once('.').unwrap_or((rest, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || frac.len() > 6
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let w: i64 = whole.parse().ok()?;
    let f: i64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<6}").parse().ok()?
    };
    w.checked_mul(1_000_000)?.checked_add(f)
}

/// A lookup in a JSON node: the child found (with the container's own labels and, for an
/// object, its key's), or `none` labelled with everything the miss depended on (every key the
/// key was compared with).
fn child(cur: &Value, key: Option<&str>, at: Option<u64>) -> Result<Value, Value> {
    let miss = || Value::none().with_labels(cur.quick());
    let Repr::Json(n) = &cur.repr else {
        return Err(miss());
    };
    let found = match (&n.node, key, at) {
        (Json::Object(entries), Some(k), _) => entries
            .iter()
            .find(|(k2, _)| k2.as_text() == Some(k))
            .map(|(k2, v)| v.clone().with_labels(k2.labels)),
        (Json::Array(l), _, Some(i)) => l.get(i).cloned(),
        _ => None,
    };
    found.map(|v| v.with_labels(cur.labels)).ok_or_else(miss)
}

/// `json.get(j, pointer)`: the node at an RFC 6901 pointer, with the labels of every container
/// and key on the way; `none` (labelled) when there is none.
pub(crate) fn get(j: &Value, pointer: &str) -> Result<Value, Fault> {
    if pointer.is_empty() {
        return Ok(j.clone());
    }
    // A pointer with more tokens than values nest deep names nothing: a miss, without
    // splitting it (a pointer of 16 MiB of slashes would make millions of tokens).
    if pointer.bytes().filter(|b| *b == b'/').count() > crate::jsonedit::MAX_TOKENS {
        return crate::jsonedit::tokens_checked(pointer)
            .map(|()| Value::none().with_labels(j.quick()));
    }
    let mut cur = j.clone();
    for token in crate::jsonedit::tokens(pointer)? {
        let at = crate::jsonedit::index_token(&token);
        match child(&cur, Some(token.as_str()), at) {
            Ok(v) => cur = v,
            Err(miss) => return Ok(miss),
        }
    }
    Ok(cur)
}

/// `j[i]` on `Json`: an object's field by text, an array's element by index (a labelled `none`
/// when absent).
pub(crate) fn index(j: &Value, i: &Value) -> Value {
    let (key, at) = match &i.repr {
        Repr::Str(k) => (Some(k.as_str()), None),
        Repr::Int(x) => (None, u64::try_from(*x).ok()),
        _ => (None, None),
    };
    child(j, key, at).unwrap_or_else(|miss| miss)
}
