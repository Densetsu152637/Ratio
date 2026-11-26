//! Editing `Json` values (chapter 06, `json`): RFC 6901 pointers, `json.set`, `json.merge`
//! (RFC 7396), `json.patch` (RFC 6902), `json.diff`, and rendering to text.
//!
//! Every edit copies the path to the changed node and shares the rest. A rebuilt container
//! keeps its own labels and gains the caller's `extra` (what decided the edit: the pointer,
//! the patch) and the labels of the keys it was searched through, so the edit's result carries
//! everything that decided it. Walks over the document are iterative (an explicit stack) except
//! `merge`, which recurses over the patch's depth (at most [`MAX_DEPTH`]) with a small frame.
//! Buffers proportional to an object's width are charged to the heap before they are made.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::heap::{Charge, Heap, cost};
use crate::labels::Labels;
use crate::ops::eq;
use crate::plist::{Cursor, List};
use crate::value::{Fault, Json, MAX_DEPTH, MAX_ELEMS, MAX_STR_BYTES, Repr, Str, Value};

fn invalid(what: impl Into<String>) -> Fault {
    Fault::Invalid(what.into())
}

// ---- pointers ----

/// The reference tokens of an RFC 6901 pointer (`""` is the whole document).
///
/// # Errors
///
/// `Invalid`, for a pointer that does not start with `/` or has a `~` not followed by `0` or `1`.
pub(crate) fn tokens(pointer: &str) -> Result<Vec<String>, Fault> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(invalid("a JSON pointer starts with `/`"));
    };
    // No value nests this deep: a longer pointer is refused before it is split.
    if pointer.bytes().filter(|b| *b == b'/').count() > MAX_TOKENS {
        return Err(invalid("a JSON pointer of more than 257 tokens"));
    }
    rest.split('/').map(unescape).collect()
}

/// The most tokens a pointer to a value of the greatest depth can have.
pub(crate) const MAX_TOKENS: usize = MAX_DEPTH as usize + 1;

/// Whether `pointer` is well formed, without splitting it: for a pointer too long to name
/// anything, which `json.get` answers with a miss.
pub(crate) fn tokens_checked(pointer: &str) -> Result<(), Fault> {
    if !pointer.starts_with('/') {
        return Err(invalid("a JSON pointer starts with `/`"));
    }
    let bytes = pointer.as_bytes();
    let mut i = 0;
    while let Some(at) = pointer[i..].find('~') {
        match bytes.get(i + at + 1) {
            Some(b'0' | b'1') => i += at + 2,
            _ => return Err(invalid("a `~` in a JSON pointer is followed by 0 or 1")),
        }
    }
    Ok(())
}

fn unescape(raw: &str) -> Result<String, Fault> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return Err(invalid("a `~` in a JSON pointer is followed by 0 or 1")),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// A pointer token as an array index: digits, no leading zero.
pub(crate) fn index_token(token: &str) -> Option<u64> {
    let digits = !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit());
    (digits && (token == "0" || !token.starts_with('0')))
        .then(|| token.parse().ok())
        .flatten()
}

pub(crate) fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

// ---- nodes ----

pub(crate) fn node(v: &Value) -> Option<&Json> {
    match &v.repr {
        Repr::Json(n) => Some(&n.node),
        _ => None,
    }
}

/// An object's entries; `None` for anything else.
pub(crate) fn object(v: &Value) -> Option<&[(Value, Value)]> {
    match node(v)? {
        Json::Object(e) => Some(e),
        _ => None,
    }
}

pub(crate) fn array(v: &Value) -> Option<&List> {
    match node(v)? {
        Json::Array(l) => Some(l),
        _ => None,
    }
}

/// An object's field called `name`.
pub(crate) fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    object(v)?
        .iter()
        .find(|(k, _)| k.as_text() == Some(name))
        .map(|(_, x)| x)
}

fn is_null(v: &Value) -> bool {
    matches!(node(v), Some(Json::Null))
}

pub(crate) fn text_node(heap: &Heap, s: &str) -> Result<Value, Fault> {
    Value::new_json(heap, Json::Str(Str::new(heap, s.to_owned())?))
}

/// A heap charge for `slots` slots, held while a buffer of that many is built.
fn pre_charge(heap: &Heap, slots: u64) -> Result<Charge, Fault> {
    Ok(heap.alloc(cost::SLOT.saturating_mul(slots))?)
}

/// An object node over `entries` with the labels `own`.
fn object_node(heap: &Heap, own: Labels, entries: Vec<(Value, Value)>) -> Result<Value, Fault> {
    if entries.len() as u64 > MAX_ELEMS {
        return Err(Fault::TooLarge("an object of more than 1,000,000 keys"));
    }
    Ok(Value::new_json(heap, Json::Object(entries.into_boxed_slice()))?.with_labels(own))
}

fn array_node(heap: &Heap, own: Labels, list: List) -> Result<Value, Fault> {
    Ok(Value::new_json(heap, Json::Array(list))?.with_labels(own))
}

fn key_labels(entries: &[(Value, Value)]) -> Labels {
    entries.iter().fold(Labels::NONE, |l, (k, _)| l | k.labels)
}

// ---- lookup and edit ----

/// Where a token leads in a container.
#[derive(Clone, Copy, Debug)]
enum Pos {
    Key(usize),
    Index(u64),
}

/// The child of `cur` that `token` names, if there is one.
fn find(cur: &Value, token: &str) -> Option<Pos> {
    match node(cur)? {
        Json::Object(e) => {
            crate::work::add(e.len() as u64);
            e.iter()
                .position(|(k, _)| k.as_text() == Some(token))
                .map(Pos::Key)
        }
        Json::Array(l) => index_token(token).filter(|i| *i < l.len()).map(Pos::Index),
        _ => None,
    }
}

fn child_at(cur: &Value, pos: Pos) -> Option<Value> {
    match (node(cur)?, pos) {
        (Json::Object(e), Pos::Key(i)) => e.get(i).map(|(_, v)| v.clone()),
        (Json::Array(l), Pos::Index(i)) => l.get(i).cloned(),
        _ => None,
    }
}

/// The node at `toks`, without the labels of the path (an iterative walk).
pub(crate) fn lookup(doc: &Value, toks: &[String]) -> Option<Value> {
    let mut cur = doc.clone();
    for t in toks {
        let pos = find(&cur, t)?;
        cur = child_at(&cur, pos)?;
    }
    Some(cur)
}

/// What an edit does at the last token of its pointer.
#[derive(Debug)]
pub(crate) enum Edit {
    /// `json.set`: replace an existing value, or add (a new key; an array's end).
    Upsert(Value),
    /// RFC 6902 `add`: a new key or a replaced one in an object; an insertion in an array.
    Add(Value),
    /// RFC 6902 `replace`: the target must exist.
    Replace(Value),
    /// RFC 6902 `remove`: the target must exist.
    Remove,
}

/// A copy of `doc` with `op` applied at `toks`; every container on the path is rebuilt with
/// `extra` added to its own labels.
///
/// # Errors
///
/// `Invalid` when a parent does not exist or is not a container, or the last token does not
/// fit (a missing target, an index past the end); heap, depth and size limits.
pub(crate) fn edit(
    heap: &Heap,
    doc: &Value,
    toks: &[String],
    op: Edit,
    extra: Labels,
) -> Result<Value, Fault> {
    let Some((last, parents)) = toks.split_last() else {
        return match op {
            Edit::Remove => Err(invalid("the root cannot be removed")),
            Edit::Upsert(v) | Edit::Add(v) | Edit::Replace(v) => Ok(v),
        };
    };
    let mut path: Vec<(Value, Pos)> = Vec::with_capacity(parents.len());
    let mut cur = doc.clone();
    for t in parents {
        let pos =
            find(&cur, t).ok_or_else(|| invalid(format!("no value at the pointer token `{t}`")))?;
        let child = child_at(&cur, pos).ok_or_else(|| invalid("a pointer through a scalar"))?;
        path.push((cur, pos));
        cur = child;
    }
    let mut rebuilt = edit_leaf(heap, &cur, last, op, extra)?;
    while let Some((parent, pos)) = path.pop() {
        rebuilt = replace_child(heap, &parent, pos, rebuilt, extra)?;
    }
    Ok(rebuilt)
}

fn edit_leaf(
    heap: &Heap,
    cur: &Value,
    token: &str,
    op: Edit,
    extra: Labels,
) -> Result<Value, Fault> {
    match node(cur) {
        Some(Json::Object(entries)) => {
            let own = cur.labels | extra | key_labels(entries);
            crate::work::add(2 * entries.len() as u64 + 1);
            let at = entries.iter().position(|(k, _)| k.as_text() == Some(token));
            let _charge = pre_charge(heap, 2 * (entries.len() as u64 + 1))?;
            let mut out: Vec<(Value, Value)> = Vec::with_capacity(entries.len() + 1);
            out.extend(entries.iter().cloned());
            match (op, at) {
                (Edit::Upsert(v) | Edit::Add(v) | Edit::Replace(v), Some(i)) => out[i].1 = v,
                (Edit::Upsert(v) | Edit::Add(v), None) => {
                    out.push((Value::new_text(heap, token.to_owned())?, v));
                }
                (Edit::Remove, Some(i)) => {
                    out.remove(i);
                }
                (Edit::Replace(_) | Edit::Remove, None) => {
                    return Err(invalid(format!("no member `{token}` to replace or remove")));
                }
            }
            object_node(heap, own, out)
        }
        Some(Json::Array(l)) => {
            let own = cur.labels | extra;
            let len = l.len();
            let end = token == "-";
            let at = if end { Some(len) } else { index_token(token) };
            let Some(at) = at else {
                return Err(invalid(format!("`{token}` is not an array index")));
            };
            let list = match op {
                Edit::Upsert(v) if at < len && !end => l.set(heap, at, v)?,
                Edit::Upsert(v) if at == len => l.push(heap, v)?,
                Edit::Add(v) if at <= len => {
                    let head = l.slice(heap, 0, at)?;
                    let one = List::from_vec(heap, vec![v])?;
                    let tail = l.slice(heap, at, len)?;
                    head.concat(heap, &one)?.concat(heap, &tail)?
                }
                Edit::Replace(v) if at < len && !end => l.set(heap, at, v)?,
                Edit::Remove if at < len && !end => {
                    let head = l.slice(heap, 0, at)?;
                    let tail = l.slice(heap, at + 1, len)?;
                    head.concat(heap, &tail)?
                }
                _ => return Err(invalid(format!("the index `{token}` is out of range"))),
            };
            array_node(heap, own, list)
        }
        _ => Err(invalid("a pointer through a scalar")),
    }
}

fn replace_child(
    heap: &Heap,
    parent: &Value,
    pos: Pos,
    child: Value,
    extra: Labels,
) -> Result<Value, Fault> {
    match (node(parent), pos) {
        (Some(Json::Object(entries)), Pos::Key(i)) => {
            let own = parent.labels | extra | key_labels(entries);
            crate::work::add(2 * entries.len() as u64 + 1);
            let _charge = pre_charge(heap, 2 * entries.len() as u64)?;
            let mut out: Vec<(Value, Value)> = entries.to_vec();
            out[i].1 = child;
            object_node(heap, own, out)
        }
        (Some(Json::Array(l)), Pos::Index(i)) => {
            array_node(heap, parent.labels | extra, l.set(heap, i, child)?)
        }
        _ => Err(invalid("a pointer through a scalar")),
    }
}

// ---- equality and order ----

/// Semantic equality of two JSON nodes (object key order ignored, numbers compared by value),
/// with an explicit stack: it never recurses over the nesting.
pub(crate) fn json_equal(a: &Value, b: &Value) -> bool {
    let mut work = vec![(a.clone(), b.clone())];
    while let Some((x, y)) = work.pop() {
        crate::work::add(1);
        match (node(&x), node(&y)) {
            (Some(Json::Null), Some(Json::Null)) => {}
            (Some(Json::Bool(p)), Some(Json::Bool(q))) if p == q => {}
            (Some(Json::Int(p)), Some(Json::Int(q))) if p == q => {}
            #[allow(clippy::float_cmp, reason = "numbers compare by value")]
            (Some(Json::Float(p)), Some(Json::Float(q))) if p == q => {}
            #[allow(
                clippy::cast_precision_loss,
                clippy::float_cmp,
                reason = "numbers compare by value"
            )]
            (Some(Json::Int(p)), Some(Json::Float(q)))
            | (Some(Json::Float(q)), Some(Json::Int(p)))
                if (*p as f64) == *q => {}
            (Some(Json::Str(p)), Some(Json::Str(q))) => {
                crate::work::add(p.as_str().len().min(q.as_str().len()) as u64);
                if p.as_str() != q.as_str() {
                    return false;
                }
            }
            (Some(Json::Array(p)), Some(Json::Array(q))) => {
                if p.len() != q.len() {
                    return false;
                }
                let (mut cp, mut cq) = (p.cursor(), q.cursor());
                while let (Some(u), Some(v)) = (cp.next(), cq.next()) {
                    work.push((u, v));
                }
            }
            (Some(Json::Object(p)), Some(Json::Object(q))) => {
                if p.len() != q.len() {
                    return false;
                }
                let index: HashMap<&str, &Value> = q
                    .iter()
                    .map(|(k, v)| (k.as_text().unwrap_or(""), v))
                    .collect();
                for (k, u) in p.iter() {
                    match index.get(k.as_text().unwrap_or("")) {
                        Some(v) => work.push((u.clone(), (*v).clone())),
                        None => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

fn json_rank(j: &Json) -> u8 {
    match j {
        Json::Null => 0,
        Json::Bool(_) => 1,
        Json::Int(_) | Json::Float(_) => 2,
        Json::Str(_) => 3,
        Json::Array(_) => 4,
        Json::Object(_) => 5,
    }
}

/// What the order walk still has to do.
enum Step {
    Pair(Value, Value),
    /// Object keys, compared before the values under them.
    Keys(Value, Value),
    /// The lengths of two arrays or objects, compared after their common prefix.
    Lengths(usize, usize),
}

/// The natural order of JSON nodes: `null`, booleans, numbers by value, text by code point,
/// arrays lexicographically, objects as their entries sorted by key. Total, so a sort or a map
/// of JSON keys is well defined, and iterative.
#[allow(clippy::cast_precision_loss, reason = "Int and Float compare by value")]
pub(crate) fn json_order(a: &Value, b: &Value) -> Ordering {
    let float = |x: f64, y: f64| match (x.is_nan(), y.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
    };
    let mut work = vec![Step::Pair(a.clone(), b.clone())];
    while let Some(step) = work.pop() {
        crate::work::add(1);
        let o = match step {
            Step::Lengths(p, q) => p.cmp(&q),
            Step::Keys(p, q) => p.as_text().unwrap_or("").cmp(q.as_text().unwrap_or("")),
            Step::Pair(x, y) => match (node(&x), node(&y)) {
                (Some(Json::Bool(p)), Some(Json::Bool(q))) => p.cmp(q),
                (Some(Json::Int(p)), Some(Json::Int(q))) => p.cmp(q),
                (Some(Json::Float(p)), Some(Json::Float(q))) => float(*p, *q),
                (Some(Json::Int(p)), Some(Json::Float(q))) => float(*p as f64, *q),
                (Some(Json::Float(p)), Some(Json::Int(q))) => float(*p, *q as f64),
                (Some(Json::Str(p)), Some(Json::Str(q))) => {
                    crate::work::add(p.as_str().len().min(q.as_str().len()) as u64);
                    p.as_str().cmp(q.as_str())
                }
                (Some(Json::Array(p)), Some(Json::Array(q))) => {
                    work.push(Step::Lengths(p.len() as usize, q.len() as usize));
                    let pairs: Vec<(Value, Value)> =
                        p.cursor_values().zip(q.cursor_values()).collect();
                    for (u, v) in pairs.into_iter().rev() {
                        work.push(Step::Pair(u, v));
                    }
                    Ordering::Equal
                }
                (Some(Json::Object(p)), Some(Json::Object(q))) => {
                    let sorted = |e: &[(Value, Value)]| {
                        let mut v: Vec<(Value, Value)> = e.to_vec();
                        v.sort_by(|m, n| {
                            m.0.as_text().unwrap_or("").cmp(n.0.as_text().unwrap_or(""))
                        });
                        v
                    };
                    let (sp, sq) = (sorted(p), sorted(q));
                    work.push(Step::Lengths(sp.len(), sq.len()));
                    for ((k1, v1), (k2, v2)) in sp.into_iter().zip(sq).rev() {
                        work.push(Step::Pair(v1, v2));
                        work.push(Step::Keys(k1, k2));
                    }
                    Ordering::Equal
                }
                (Some(p), Some(q)) => json_rank(p).cmp(&json_rank(q)),
                _ => Ordering::Equal,
            },
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

// ---- merge ----

/// RFC 7396: `patch` merged onto `target` (absent when there is nothing there). A patch that
/// is not an object replaces the target; an object patch merges key by key, a `null` removes.
/// The result of an object patch carries the labels of both, of every key of both, and of every
/// patch value (whether it is `null` decided what was kept).
pub(crate) fn merge(
    heap: &Heap,
    target: Option<&Value>,
    patch: &Value,
    depth: u32,
) -> Result<Value, Fault> {
    if depth > MAX_DEPTH {
        return Err(Fault::Depth);
    }
    let Some(patch_entries) = object(patch) else {
        return Ok(patch.clone());
    };
    let target_entries: &[(Value, Value)] = target.and_then(object).unwrap_or(&[]);
    crate::work::add(2 * (target_entries.len() + patch_entries.len()) as u64 + 1);
    let mut own = patch.labels | key_labels(patch_entries) | key_labels(target_entries);
    if let Some(t) = target {
        own |= t.labels;
    }
    // An index of the target's keys, and the entries being built, charged first.
    let _charge = pre_charge(
        heap,
        4 * target_entries.len() as u64 + 2 * patch_entries.len() as u64,
    )?;
    let index: HashMap<&str, usize> = target_entries
        .iter()
        .enumerate()
        .map(|(i, (k, _))| (k.as_text().unwrap_or(""), i))
        .collect();
    let mut out: Vec<Option<(Value, Value)>> = target_entries.iter().cloned().map(Some).collect();
    for (pk, pv) in patch_entries {
        crate::work::add(1);
        own |= pv.labels;
        let name = pk.as_text().unwrap_or("");
        let at = index.get(name).copied();
        if is_null(pv) {
            if let Some(i) = at {
                out[i] = None;
            }
            continue;
        }
        let current = at.and_then(|i| out[i].as_ref().map(|(_, v)| v.clone()));
        let merged = merge(heap, current.as_ref(), pv, depth + 1)?;
        match at {
            Some(i) => {
                let key = out[i]
                    .as_ref()
                    .map_or_else(|| pk.clone(), |(k, _)| k.clone());
                out[i] = Some((key, merged));
            }
            None => out.push(Some((pk.clone(), merged))),
        }
    }
    object_node(heap, own, out.into_iter().flatten().collect())
}

// ---- patch ----

/// The first violation of RFC 6902's operation shape, or the operation applied to `doc`.
///
/// # Errors
///
/// `Invalid`: a malformed operation, a path that does not fit, a `test` that fails.
pub(crate) fn apply_op(
    heap: &Heap,
    doc: &Value,
    op: &Value,
    at: usize,
    extra: Labels,
) -> Result<Value, Fault> {
    let name = field(op, "op")
        .and_then(|n| string_of(n))
        .ok_or_else(|| invalid(format!("operation {at} has no `op`")))?;
    let path = field(op, "path")
        .and_then(|n| string_of(n))
        .ok_or_else(|| invalid(format!("operation {at} has no `path`")))?;
    let toks = tokens(path)?;
    let value = || {
        field(op, "value")
            .cloned()
            .ok_or_else(|| invalid(format!("operation {at} has no `value`")))
    };
    let from = || -> Result<(Vec<String>, Value), Fault> {
        let f = field(op, "from")
            .and_then(|n| string_of(n))
            .ok_or_else(|| invalid(format!("operation {at} has no `from`")))?;
        let ftoks = tokens(f)?;
        let v = lookup(doc, &ftoks)
            .ok_or_else(|| invalid(format!("operation {at}: nothing at `from`")))?;
        Ok((ftoks, v))
    };
    match name {
        "add" => edit(heap, doc, &toks, Edit::Add(value()?), extra),
        "replace" => edit(heap, doc, &toks, Edit::Replace(value()?), extra),
        "remove" => edit(heap, doc, &toks, Edit::Remove, extra),
        "test" => {
            let want = value()?;
            let got = lookup(doc, &toks).ok_or_else(|| {
                invalid(format!("operation {at}: `test` found nothing at the path"))
            })?;
            if eq(&got, &want) {
                Ok(doc.clone())
            } else {
                Err(invalid(format!("operation {at}: `test` failed")))
            }
        }
        "copy" => {
            let (_, v) = from()?;
            edit(heap, doc, &toks, Edit::Add(v), extra)
        }
        "move" => {
            let (ftoks, v) = from()?;
            if toks.len() > ftoks.len() && toks[..ftoks.len()] == ftoks[..] {
                return Err(invalid(format!(
                    "operation {at}: a value cannot move into itself"
                )));
            }
            let removed = edit(heap, doc, &ftoks, Edit::Remove, extra)?;
            edit(heap, &removed, &toks, Edit::Add(v), extra)
        }
        other => Err(invalid(format!("operation {at}: unknown `op` `{other}`"))),
    }
}

pub(crate) fn string_of(v: &Value) -> Option<&str> {
    match node(v)? {
        Json::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

// ---- diff ----

/// Receives one RFC 6902 operation of a diff: its name, its path and its value.
type Emit<'a, E> = &'a mut dyn FnMut(&str, &str, Option<&Value>) -> Result<(), E>;

/// What the diff walk still has to do.
enum Work {
    /// Compare two nodes at the path built so far, after adding `segment` to it.
    Visit(Value, Value, Option<String>),
    /// Cut the path back to this length.
    Pop(usize),
    /// An array's trailing removals and insertions, at `path`.
    Tail {
        path: String,
        at: u64,
        remove: u64,
        add: Vec<Value>,
    },
}

/// Walks the difference of `a` and `b`, calling `emit(op, path, value)` for each RFC 6902
/// operation, in an order that makes the operations apply one after another. Objects are
/// compared key by key; arrays after trimming their common prefix and suffix, element by
/// element over the rest, then the surplus removed or added; anything else is replaced.
/// `tick(units)` is called for the work done, so a caller can check for cancellation.
///
/// # Errors
///
/// What `emit` or `tick` return.
pub(crate) fn diff<E>(
    heap: &Heap,
    a: &Value,
    b: &Value,
    emit: Emit<'_, E>,
    tick: &mut dyn FnMut(u64) -> Result<(), E>,
    oom: &dyn Fn() -> E,
) -> Result<(), E> {
    let mut path = String::new();
    // Everything the walk gathers (its stack, the element copies it compares) is charged to
    // the heap as it is gathered, and given back at the end.
    let mut held = Charge::none();
    let mut grow = |slots: u64| -> Result<(), E> {
        held.grow(heap, cost::SLOT.saturating_mul(slots))
            .map_err(|_| oom())
    };
    let mut stack = vec![Work::Visit(a.clone(), b.clone(), None)];
    grow(3)?;
    while let Some(w) = stack.pop() {
        match w {
            Work::Pop(len) => path.truncate(len),
            Work::Tail {
                path,
                at,
                remove,
                add,
            } => {
                tick(remove + add.len() as u64 + 1)?;
                for _ in 0..remove {
                    emit("remove", &format!("{path}/{at}"), None)?;
                }
                for (j, v) in add.iter().enumerate() {
                    emit("add", &format!("{path}/{}", at + j as u64), Some(v))?;
                }
            }
            Work::Visit(x, y, segment) => {
                tick(1)?;
                let restore = path.len();
                if let Some(s) = segment {
                    path.push('/');
                    path.push_str(&s);
                }
                visit(
                    &x, &y, &mut path, restore, &mut stack, emit, tick, &mut grow,
                )?;
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the walk's state, passed down once"
)]
fn visit<E>(
    x: &Value,
    y: &Value,
    path: &mut String,
    restore: usize,
    stack: &mut Vec<Work>,
    emit: Emit<'_, E>,
    tick: &mut dyn FnMut(u64) -> Result<(), E>,
    grow: &mut dyn FnMut(u64) -> Result<(), E>,
) -> Result<(), E> {
    match (node(x), node(y)) {
        (Some(Json::Object(ex)), Some(Json::Object(ey))) => {
            let in_y: HashMap<&str, &Value> = ey
                .iter()
                .map(|(k, v)| (k.as_text().unwrap_or(""), v))
                .collect();
            let in_x: HashSet<&str> = ex.iter().map(|(k, _)| k.as_text().unwrap_or("")).collect();
            tick(ex.len() as u64 + ey.len() as u64)?;
            grow(4 * (ex.len() as u64 + ey.len() as u64) + 2)?;
            stack.push(Work::Pop(restore));
            let mut children = Vec::new();
            for (k, vx) in ex {
                let name = k.as_text().unwrap_or("");
                match in_y.get(name) {
                    Some(vy) => children.push((vx.clone(), (*vy).clone(), name.to_owned())),
                    None => emit("remove", &format!("{path}/{}", escape(name)), None)?,
                }
            }
            for (k, vy) in ey {
                let name = k.as_text().unwrap_or("");
                if !in_x.contains(name) {
                    emit("add", &format!("{path}/{}", escape(name)), Some(vy))?;
                }
            }
            for (vx, vy, name) in children.into_iter().rev() {
                stack.push(Work::Visit(vx, vy, Some(escape(&name))));
            }
        }
        (Some(Json::Array(lx)), Some(Json::Array(ly))) => {
            grow(lx.len().saturating_add(ly.len()).saturating_add(8))?;
            let (vx, vy) = (lx.to_vec(), ly.to_vec());
            tick(vx.len() as u64 + vy.len() as u64)?;
            let mut prefix = 0;
            while prefix < vx.len() && prefix < vy.len() && eq(&vx[prefix], &vy[prefix]) {
                prefix += 1;
            }
            let mut suffix = 0;
            while suffix < vx.len() - prefix
                && suffix < vy.len() - prefix
                && eq(&vx[vx.len() - 1 - suffix], &vy[vy.len() - 1 - suffix])
            {
                suffix += 1;
            }
            let (mid_x, mid_y) = (vx.len() - prefix - suffix, vy.len() - prefix - suffix);
            let common = mid_x.min(mid_y);
            stack.push(Work::Pop(restore));
            if mid_x > common || mid_y > common {
                stack.push(Work::Tail {
                    path: path.clone(),
                    at: (prefix + common) as u64,
                    remove: (mid_x - common) as u64,
                    add: vy[prefix + common..prefix + mid_y].to_vec(),
                });
            }
            for i in (0..common).rev() {
                stack.push(Work::Visit(
                    vx[prefix + i].clone(),
                    vy[prefix + i].clone(),
                    Some((prefix + i).to_string()),
                ));
            }
        }
        _ => {
            if !eq(x, y) {
                emit("replace", path, Some(y))?;
            }
            path.truncate(restore);
        }
    }
    Ok(())
}

// ---- rendering ----

enum Frame {
    Array(Cursor, bool),
    Object(Value, usize),
}

fn indent(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Renders a JSON node as text, compact or indented by two spaces (`"key": value`, one element
/// per line, `[]` and `{}` for the empty), with an explicit stack. Stops writing once the text
/// is longer than 16 MiB and returns `false`; the caller refuses the result.
pub(crate) fn render(j: &Value, pretty: bool, out: &mut String) -> bool {
    let done = render_nodes(j, pretty, out);
    crate::work::add(out.len() as u64);
    done
}

fn render_nodes(j: &Value, pretty: bool, out: &mut String) -> bool {
    let mut stack: Vec<Frame> = Vec::new();
    let mut next = Some(j.clone());
    loop {
        if out.len() > MAX_STR_BYTES {
            return false;
        }
        if let Some(v) = next.take() {
            match node(&v) {
                Some(Json::Array(l)) if !l.is_empty() => {
                    out.push('[');
                    stack.push(Frame::Array(l.cursor(), true));
                }
                Some(Json::Array(_)) => out.push_str("[]"),
                Some(Json::Object(e)) if !e.is_empty() => {
                    out.push('{');
                    stack.push(Frame::Object(v.clone(), 0));
                }
                Some(Json::Object(_)) => out.push_str("{}"),
                Some(Json::Null) | None => out.push_str("null"),
                Some(Json::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
                Some(Json::Int(i)) => out.push_str(&i.to_string()),
                Some(Json::Float(f)) => crate::json::number(*f, out),
                Some(Json::Str(s)) => crate::json::quote(s.as_str(), out),
            }
        }
        let depth = stack.len();
        let Some(top) = stack.last_mut() else {
            return true;
        };
        match top {
            Frame::Array(cursor, first) => {
                if let Some(item) = cursor.next() {
                    if !*first {
                        out.push(',');
                    }
                    *first = false;
                    if pretty {
                        indent(out, depth);
                    }
                    next = Some(item);
                } else {
                    stack.pop();
                    if pretty {
                        indent(out, depth - 1);
                    }
                    out.push(']');
                }
            }
            Frame::Object(obj, at) => {
                let entry = object(obj).and_then(|e| e.get(*at));
                if let Some((k, item)) = entry {
                    if *at > 0 {
                        out.push(',');
                    }
                    *at += 1;
                    if pretty {
                        indent(out, depth);
                    }
                    crate::json::quote(k.as_text().unwrap_or(""), out);
                    out.push(':');
                    if pretty {
                        out.push(' ');
                    }
                    next = Some(item.clone());
                } else {
                    stack.pop();
                    if pretty {
                        indent(out, depth - 1);
                    }
                    out.push('}');
                }
            }
        }
    }
}
