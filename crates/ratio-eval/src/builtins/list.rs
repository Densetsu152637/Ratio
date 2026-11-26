//! The `list` built-ins of chapter 06 (`data`) that stage 6 did not carry.
//!
//! Every result carries the labels of everything that decided it: the list's own labels (its
//! length and order), the arguments that picked positions or counts, the labels of every
//! callback result that decided which elements were kept or where a scan stopped, and, for the
//! order-dependent ones, the labels anywhere inside the list (`quick`). Elements that move
//! into a new list keep their own labels; the new list's own labels cover what decided where
//! they went. Charges are debited before the work, from the arguments' sizes with checked
//! arithmetic (the `s_*` functions here); a refusal is `Invalid` before anything is allocated.

use std::cmp::Ordering;

use crate::error::{Kind, RtError, raise};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::ops::{cmp_natural, eq, log_work};
use crate::plist::List;
use crate::pmap::Map;
use crate::value::{MAX_ELEMS, Repr, Value};

use super::util::{Gather, Pace, clamp_index, merge_sort};
use super::{Args, Resume, Sizes, Task, TaskStep, Walk, arg, int, list as xs_of, opt};

// ---- sizing ----

/// The number of elements `list.range(start, end, step)` makes, if it is a legal call.
fn range_count(a: &Args) -> Option<u64> {
    let int_at = |i: usize| a.get(i).and_then(Option::as_ref).and_then(Value::as_int);
    let (start, end) = (int_at(0)?, int_at(1)?);
    let step = int_at(2).unwrap_or(1);
    if step == 0 {
        return None;
    }
    let (s, e, st) = (i128::from(start), i128::from(end), i128::from(step));
    let n = if st > 0 {
        if s < e { (e - s + st - 1) / st } else { 0 }
    } else if s > e {
        (s - e + (-st) - 1) / (-st)
    } else {
        0
    };
    u64::try_from(n).ok().filter(|n| *n <= MAX_ELEMS)
}

/// `list.range`: the output is the count of elements the arguments fix; an illegal call is
/// charged nothing here and refused before anything is built.
pub(super) fn s_range(a: &Args) -> Sizes {
    Sizes {
        m: range_count(a).unwrap_or(0),
        ..Sizes::default()
    }
}

/// `list.repeat`: `n` elements, when `n` is in `0..=1,000,000`.
pub(super) fn s_repeat(a: &Args) -> Sizes {
    let n = a.get(1).and_then(Option::as_ref).and_then(Value::as_int);
    Sizes {
        m: n.and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n <= MAX_ELEMS)
            .unwrap_or(0),
        ..Sizes::default()
    }
}

/// Order-dependent built-ins compare whole elements: `n` is the length and `n2` the weight, so
/// the charge is `weight · ⌈log2(len + 1)⌉` (chapter 06: values are walked at their weight).
pub(super) fn s_compare(a: &Args) -> Sizes {
    let len = a
        .first()
        .and_then(Option::as_ref)
        .map_or(0, crate::ops::size);
    let w = a.first().and_then(Option::as_ref).map_or(0, Value::weight);
    Sizes {
        n: len,
        n2: w,
        ..Sizes::default()
    }
}

/// `list.window`: building `len − size + 1` windows costs O(log len) nodes and two leaf copies
/// each (a slice shares everything else), so they are charged as
/// `len + windows · (⌈log2(len + 1)⌉ + 64)`.
pub(super) fn s_window(a: &Args) -> Sizes {
    let len = a
        .first()
        .and_then(Option::as_ref)
        .map_or(0, crate::ops::size);
    let size = a
        .get(1)
        .and_then(Option::as_ref)
        .and_then(Value::as_int)
        .and_then(|s| u64::try_from(s).ok())
        .filter(|s| *s >= 1);
    let windows = size.map_or(0, |s| if s <= len { len - s + 1 } else { 0 });
    Sizes::n(len.saturating_add(windows.saturating_mul(log_work(len) + 64)))
}

// ---- helpers ----

fn invalid(cx: &Cx<'_>, message: &str, labels: Labels) -> RtError {
    raise(cx.p, Kind::Invalid, message, labels, cx.loc)
}

/// The elements of `l`, gathered under the heap charge (`n` slots up front).
fn collect(cx: &Cx<'_>, l: &List, labels: Labels) -> Result<Gather, RtError> {
    let mut g = Gather::with_capacity(cx, l.len(), labels)?;
    let mut pace = Pace::new();
    let mut c = l.cursor();
    while let Some(v) = c.next() {
        pace.tick(cx, 1)?;
        g.push(cx, v)?;
    }
    Ok(g)
}

// ---- positions and shape ----

pub(super) fn is_empty(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    Ok(Value::bool(xs_of(cx, xs)?.is_empty()).with_labels(xs.labels))
}

pub(super) fn last(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let l = xs_of(cx, xs)?;
    Ok(match l.len().checked_sub(1).and_then(|i| l.get(i)) {
        Some(v) => v.clone().with_labels(xs.labels),
        None => Value::none().with_labels(xs.labels),
    })
}

pub(super) fn insert(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, i, x) = (arg(cx, args, 0)?, arg(cx, args, 1)?, arg(cx, args, 2)?);
    let labels = xs.labels | i.labels;
    let l = xs_of(cx, xs)?;
    let len = l.len();
    let Some(at) = u64::try_from(int(cx, i)?).ok().filter(|a| *a <= len) else {
        return Err(invalid(
            cx,
            "`list.insert`: the index is out of range",
            labels,
        ));
    };
    let fault = |f| cx.fault(f, labels);
    let head = l.slice(cx.heap, 0, at).map_err(fault)?;
    let tail = l.slice(cx.heap, at, len).map_err(fault)?;
    let one = List::from_vec(cx.heap, vec![x.clone()]).map_err(fault)?;
    let out = head
        .concat(cx.heap, &one)
        .and_then(|h| h.concat(cx.heap, &tail))
        .map_err(fault)?;
    Ok(Value::new_list(out).with_labels(labels))
}

pub(super) fn remove_at(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, i) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = xs.labels | i.labels;
    let l = xs_of(cx, xs)?;
    let len = l.len();
    let Some(at) = u64::try_from(int(cx, i)?).ok().filter(|a| *a < len) else {
        return Err(invalid(
            cx,
            "`list.remove_at`: the index is out of range",
            labels,
        ));
    };
    let fault = |f| cx.fault(f, labels);
    let head = l.slice(cx.heap, 0, at).map_err(fault)?;
    let tail = l.slice(cx.heap, at + 1, len).map_err(fault)?;
    let out = head.concat(cx.heap, &tail).map_err(fault)?;
    Ok(Value::new_list(out).with_labels(labels))
}

pub(super) fn concat(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, ys) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = xs.labels | ys.labels;
    let out = xs_of(cx, xs)?
        .concat(cx.heap, xs_of(cx, ys)?)
        .map_err(|f| cx.fault(f, labels))?;
    Ok(Value::new_list(out).with_labels(labels))
}

pub(super) fn take_rest(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, n) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = xs.labels | n.labels;
    let l = xs_of(cx, xs)?;
    let skip = u64::try_from(int(cx, n)?).unwrap_or(0);
    let out = l
        .slice(cx.heap, skip, l.len())
        .map_err(|f| cx.fault(f, labels))?;
    Ok(Value::new_list(out).with_labels(labels))
}

pub(super) fn slice(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, start) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let end = opt(args, 2).filter(|e| !e.is_none());
    let labels = xs.labels | start.labels | opt(args, 2).map_or(Labels::NONE, |e| e.labels);
    let l = xs_of(cx, xs)?;
    let len = l.len();
    let from = clamp_index(int(cx, start)?, len);
    let to = match end {
        Some(e) => clamp_index(int(cx, e)?, len),
        None => len,
    };
    let out = l
        .slice(cx.heap, from, to)
        .map_err(|f| cx.fault(f, labels))?;
    Ok(Value::new_list(out).with_labels(labels))
}

pub(super) fn flatten(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xss = arg(cx, args, 0)?;
    let outer = xs_of(cx, xss)?;
    // The result's length depends on every inner list's own labels.
    let mut labels = xss.labels;
    let mut out = Gather::new();
    let mut pace = Pace::new();
    let mut cursor = outer.cursor();
    while let Some(inner) = cursor.next() {
        pace.tick(cx, 1)?;
        labels |= inner.labels;
        let mut c = xs_of(cx, &inner)?.cursor();
        while let Some(v) = c.next() {
            pace.tick(cx, 1)?;
            out.push(cx, v)?;
        }
    }
    out.finish(cx, labels)
}

pub(super) fn range(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (start, end) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let step = opt(args, 2).cloned().unwrap_or_else(|| Value::int(1));
    let labels = start.labels | end.labels | step.labels;
    let (s, e, st) = (int(cx, start)?, int(cx, end)?, int(cx, &step)?);
    if st == 0 {
        return Err(invalid(cx, "`list.range`: the step is 0", labels));
    }
    let Some(count) = range_count(args) else {
        return Err(invalid(
            cx,
            "`list.range`: more than 1,000,000 elements",
            labels,
        ));
    };
    let _ = e;
    let mut out = Gather::with_capacity(cx, count, labels)?;
    let mut at = i128::from(s);
    for _ in 0..count {
        let v = i64::try_from(at).map_err(|_| invalid(cx, "`list.range` overflowed", labels))?;
        out.push(cx, Value::int(v))?;
        at += i128::from(st);
    }
    crate::work::add(count);
    out.finish(cx, labels)
}

pub(super) fn repeat(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (x, n) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = n.labels;
    let count = int(cx, n)?;
    let Some(count) = u64::try_from(count).ok().filter(|c| *c <= MAX_ELEMS) else {
        return Err(invalid(
            cx,
            "`list.repeat`: the count is negative or over 1,000,000",
            labels,
        ));
    };
    let mut out = Gather::with_capacity(cx, count, labels)?;
    for _ in 0..count {
        out.push(cx, x.clone())?;
    }
    crate::work::add(count);
    out.finish(cx, labels)
}

pub(super) fn reverse(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let mut g = collect(cx, xs_of(cx, xs)?, xs.labels)?;
    g.reverse();
    g.finish(cx, xs.labels)
}

pub(super) fn chunk(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, size) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = xs.labels | size.labels;
    let size = int(cx, size)?;
    let Some(size) = u64::try_from(size).ok().filter(|s| *s >= 1) else {
        return Err(invalid(cx, "`list.chunk`: the size is less than 1", labels));
    };
    let mut outer = Gather::new();
    let mut cur = Gather::new();
    let mut pace = Pace::new();
    let mut c = xs_of(cx, xs)?.cursor();
    while let Some(v) = c.next() {
        pace.tick(cx, 1)?;
        cur.push(cx, v)?;
        if cur.len() as u64 == size {
            let done = std::mem::replace(&mut cur, Gather::new());
            outer.push(cx, done.finish(cx, labels)?)?;
        }
    }
    if cur.len() > 0 {
        outer.push(cx, cur.finish(cx, labels)?)?;
    }
    outer.finish(cx, labels)
}

pub(super) fn window(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, size) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = xs.labels | size.labels;
    let size = int(cx, size)?;
    let Some(size) = u64::try_from(size).ok().filter(|s| *s >= 1) else {
        return Err(invalid(
            cx,
            "`list.window`: the size is less than 1",
            labels,
        ));
    };
    let l = xs_of(cx, xs)?;
    let len = l.len();
    let mut out = Gather::new();
    let mut pace = Pace::new();
    if size <= len {
        for i in 0..=(len - size) {
            pace.tick(cx, log_work(len))?;
            let w = l
                .slice(cx.heap, i, i + size)
                .map_err(|f| cx.fault(f, labels))?;
            out.push(cx, Value::new_list(w).with_labels(labels))?;
        }
    }
    out.finish(cx, labels)
}

pub(super) fn zip(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, ys) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    // The length is the shorter of the two: both lists' own labels decided it.
    let labels = xs.labels | ys.labels;
    let (lx, ly) = (xs_of(cx, xs)?, xs_of(cx, ys)?);
    let mut out = Gather::with_capacity(cx, lx.len().min(ly.len()), labels)?;
    let (mut a, mut b) = (lx.cursor(), ly.cursor());
    let mut pace = Pace::new();
    while let (Some(x), Some(y)) = (a.next(), b.next()) {
        pace.tick(cx, 1)?;
        let pair = Value::new_tuple(cx.heap, vec![x, y]).map_err(|f| cx.fault(f, labels))?;
        out.push(cx, pair)?;
    }
    out.finish(cx, labels)
}

pub(super) fn unzip(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let ps = arg(cx, args, 0)?;
    let l = xs_of(cx, ps)?;
    let mut firsts = Gather::with_capacity(cx, l.len(), ps.labels)?;
    let mut seconds = Gather::with_capacity(cx, l.len(), ps.labels)?;
    let mut pace = Pace::new();
    let mut c = l.cursor();
    while let Some(pair) = c.next() {
        pace.tick(cx, 1)?;
        let Repr::Tuple(t) = &pair.repr else {
            return Err(invalid(cx, "`list.unzip` expects pairs", ps.labels));
        };
        let (Some(x), Some(y)) = (t.items.first(), t.items.get(1)) else {
            return Err(invalid(cx, "`list.unzip` expects pairs", ps.labels));
        };
        // A pair's own labels reach both of its parts.
        firsts.push(cx, x.clone().with_labels(pair.labels))?;
        seconds.push(cx, y.clone().with_labels(pair.labels))?;
    }
    let (a, b) = (
        firsts.finish(cx, ps.labels)?,
        seconds.finish(cx, ps.labels)?,
    );
    Value::new_tuple(cx.heap, vec![a, b]).map_err(|f| cx.fault(f, ps.labels))
}

pub(super) fn enumerate(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let l = xs_of(cx, xs)?;
    let mut out = Gather::with_capacity(cx, l.len(), xs.labels)?;
    let mut pace = Pace::new();
    let mut c = l.cursor();
    let mut i = 0i64;
    while let Some(v) = c.next() {
        pace.tick(cx, 1)?;
        let pair = Value::new_tuple(cx.heap, vec![Value::int(i), v])
            .map_err(|f| cx.fault(f, xs.labels))?;
        out.push(cx, pair)?;
        i += 1;
    }
    out.finish(cx, xs.labels)
}

// ---- searching and order ----

pub(super) fn index_of(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, x) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    // Which index (or none) depends on every element compared.
    let labels = xs.quick() | x.quick();
    let mut pace = Pace::new();
    let mut c = xs_of(cx, xs)?.cursor();
    let mut i = 0i64;
    while let Some(v) = c.next() {
        pace.tick(cx, v.weight().min(x.weight()))?;
        if eq(&v, x) {
            return Ok(Value::int(i).with_labels(labels));
        }
        i += 1;
    }
    Ok(Value::none().with_labels(labels))
}

fn extreme(cx: &Cx<'_>, args: &Args, max: bool) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let mut best: Option<Value> = None;
    let mut pace = Pace::new();
    let mut c = xs_of(cx, xs)?.cursor();
    while let Some(v) = c.next() {
        pace.tick(cx, v.weight())?;
        let better = match &best {
            None => true,
            Some(b) => {
                let o = cmp_natural(&v, b);
                if max {
                    o == Ordering::Greater
                } else {
                    o == Ordering::Less
                }
            }
        };
        if better {
            best = Some(v);
        }
    }
    // The choice depends on every element.
    let labels = xs.quick();
    Ok(best.unwrap_or_else(Value::none).with_labels(labels))
}

pub(super) fn min(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    extreme(cx, args, false)
}

pub(super) fn max(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    extreme(cx, args, true)
}

pub(super) fn sort(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let items = collect(cx, xs_of(cx, xs)?, xs.labels)?;
    let order = merge_sort(
        cx,
        items.len(),
        &|i| items.items()[i].weight(),
        &mut |a, b| cmp_natural(&items.items()[a], &items.items()[b]),
    )?;
    let mut out = Gather::with_capacity(cx, items.len() as u64, xs.labels)?;
    for &i in &order.indexes {
        out.push(cx, items.items()[i].clone())?;
    }
    // The order depends on every element.
    out.finish(cx, xs.quick())
}

pub(super) fn unique(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let mut seen = Map::empty();
    let mut out = Gather::new();
    let mut pace = Pace::new();
    let mut c = xs_of(cx, xs)?.cursor();
    while let Some(v) = c.next() {
        pace.tick(
            cx,
            v.weight().saturating_mul(crate::ops::log_work(seen.len())),
        )?;
        if seen.get_entry(&v).is_none() {
            seen = seen
                .insert(cx.heap, v.clone(), Value::unit())
                .map_err(|f| cx.fault(f, xs.labels))?;
            out.push(cx, v)?;
        }
    }
    // Which elements survive depends on every element.
    out.finish(cx, xs.quick())
}

// ---- callbacks that scan ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    MapIndexed,
    FlatMap,
    Reduce,
    Find,
    FindIndex,
    Any,
    All,
    Count,
    TakeWhile,
    Partition,
    MinBy,
    MaxBy,
}

/// A walk of the list calling a function on each element, gathering what the mode needs.
#[derive(Debug)]
struct Each {
    walk: Walk,
    mode: Mode,
    /// Elements handed to the callback so far.
    called: i64,
    /// The element in flight, and its position.
    current: Option<(Value, i64)>,
    /// The labels of every callback result examined: what decided where the scan stopped and
    /// which elements it kept.
    decided: Labels,
    out: Gather,
    rest: Gather,
    acc: Option<Value>,
    count: i64,
    best: Option<(Value, Value)>,
}

impl Each {
    fn start(cx: &Cx<'_>, args: &Args, mode: Mode) -> Result<Box<dyn Task>, RtError> {
        let mut walk = Walk::new(cx, args)?;
        let acc = if mode == Mode::Reduce {
            walk.next()
        } else {
            None
        };
        Ok(Box::new(Each {
            walk,
            mode,
            called: 0,
            current: None,
            decided: Labels::NONE,
            out: Gather::new(),
            rest: Gather::new(),
            acc,
            count: 0,
            best: None,
        }))
    }

    fn own(&self) -> Labels {
        self.walk.own
    }

    /// The labels of a result that depended on what the scan saw.
    fn decided(&self) -> Labels {
        self.walk.own | self.decided
    }

    fn call_args(&mut self, x: Value) -> Vec<Value> {
        let at = self.called;
        self.called += 1;
        match self.mode {
            Mode::MapIndexed => vec![Value::int(at).with_labels(self.walk.own), x],
            Mode::Reduce => vec![self.acc.take().unwrap_or_else(Value::unit), x],
            _ => {
                self.current = Some((x.clone(), at));
                vec![x]
            }
        }
    }

    /// Handles a callback's result; `Some` ends the scan with that value.
    #[allow(clippy::too_many_lines, reason = "one arm per mode")]
    fn result(&mut self, cx: &Cx<'_>, v: Value) -> Result<Option<Value>, RtError> {
        crate::work::add(1);
        match self.mode {
            Mode::MapIndexed => self.out.push(cx, v)?,
            Mode::FlatMap => {
                self.decided |= v.labels;
                let Repr::List(l) = &v.repr else {
                    return Err(invalid(cx, "`list.flat_map`: expected lists", v.labels));
                };
                let mut c = l.cursor();
                while let Some(e) = c.next() {
                    crate::work::add(1);
                    self.out.push(cx, e)?;
                }
            }
            Mode::Reduce => self.acc = Some(v),
            Mode::Find | Mode::FindIndex | Mode::Any | Mode::All | Mode::TakeWhile => {
                self.decided |= v.quick();
                let hit = v.as_bool() == Some(true);
                let cur = self.current.take();
                match (self.mode, hit) {
                    (Mode::Find, true) => {
                        let x = cur.map_or_else(Value::none, |(x, _)| x);
                        return Ok(Some(x.with_labels(self.decided())));
                    }
                    (Mode::FindIndex, true) => {
                        let at = cur.map_or(0, |(_, at)| at);
                        return Ok(Some(Value::int(at).with_labels(self.decided())));
                    }
                    (Mode::Any, true) => {
                        return Ok(Some(Value::bool(true).with_labels(self.decided())));
                    }
                    (Mode::All, false) => {
                        return Ok(Some(Value::bool(false).with_labels(self.decided())));
                    }
                    (Mode::TakeWhile, true) => {
                        if let Some((x, _)) = cur {
                            self.out.push(cx, x)?;
                        }
                    }
                    (Mode::TakeWhile, false) => {
                        let out = std::mem::replace(&mut self.out, Gather::new());
                        return Ok(Some(out.finish(cx, self.decided())?));
                    }
                    _ => {}
                }
            }
            Mode::Count => {
                self.decided |= v.quick();
                if v.as_bool() == Some(true) {
                    self.count += 1;
                }
            }
            Mode::Partition => {
                self.decided |= v.quick();
                if let Some((x, _)) = self.current.take() {
                    if v.as_bool() == Some(true) {
                        self.out.push(cx, x)?;
                    } else {
                        self.rest.push(cx, x)?;
                    }
                }
            }
            Mode::MinBy | Mode::MaxBy => {
                self.decided |= v.quick();
                if let Some((x, _)) = self.current.take() {
                    let better = match &self.best {
                        None => true,
                        Some((bk, _)) => {
                            let o = cmp_natural(&v, bk);
                            if self.mode == Mode::MaxBy {
                                o == Ordering::Greater
                            } else {
                                o == Ordering::Less
                            }
                        }
                    };
                    if better {
                        self.best = Some((v, x));
                    }
                }
            }
        }
        Ok(None)
    }

    fn finish(&mut self, cx: &Cx<'_>) -> Result<Value, RtError> {
        let own = self.own();
        let decided = self.decided();
        match self.mode {
            Mode::MapIndexed => std::mem::replace(&mut self.out, Gather::new()).finish(cx, own),
            Mode::FlatMap | Mode::TakeWhile => {
                std::mem::replace(&mut self.out, Gather::new()).finish(cx, decided)
            }
            Mode::Reduce => Ok(self.acc.take().unwrap_or_else(Value::none).with_labels(own)),
            Mode::Find => Ok(Value::none().with_labels(decided)),
            Mode::FindIndex => Ok(Value::none().with_labels(decided)),
            Mode::Any => Ok(Value::bool(false).with_labels(decided)),
            Mode::All => Ok(Value::bool(true).with_labels(decided)),
            Mode::Count => Ok(Value::int(self.count).with_labels(decided)),
            Mode::Partition => {
                let a = std::mem::replace(&mut self.out, Gather::new()).finish(cx, decided)?;
                let b = std::mem::replace(&mut self.rest, Gather::new()).finish(cx, decided)?;
                Value::new_tuple(cx.heap, vec![a, b]).map_err(|f| cx.fault(f, decided))
            }
            Mode::MinBy | Mode::MaxBy => Ok(self
                .best
                .take()
                .map_or_else(Value::none, |(_, x)| x)
                .with_labels(decided)),
        }
    }
}

impl Task for Each {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(v) = input
            && let Some(done) = self.result(cx, v)?
        {
            return Ok(TaskStep::Done(done));
        }
        match self.walk.next() {
            Some(x) => {
                let args = self.call_args(x);
                Ok(TaskStep::Call(self.walk.f.clone(), args))
            }
            None => Ok(TaskStep::Done(self.finish(cx)?)),
        }
    }
}

macro_rules! each_task {
    ($($name:ident => $mode:ident;)*) => {
        $(
            pub(super) fn $name(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
                Each::start(cx, args, Mode::$mode)
            }
        )*
    };
}

each_task! {
    map_indexed => MapIndexed;
    flat_map => FlatMap;
    reduce => Reduce;
    find => Find;
    find_index => FindIndex;
    any => Any;
    all => All;
    count => Count;
    take_while => TakeWhile;
    partition => Partition;
    min_by => MinBy;
    max_by => MaxBy;
}

// ---- callbacks that key ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names, reason = "named for the built-ins")]
pub(super) enum Keyed {
    UniqueBy,
    GroupBy,
    CountBy,
}

/// `unique_by`, `group_by` and `count_by`: call the key function on every element, charge
/// for the comparisons the keys will take part in (their total weight times
/// `⌈log2(n + 1)⌉`), then build the result in a persistent map keyed by the keys.
#[derive(Debug)]
struct KeyedTask {
    walk: Walk,
    mode: Keyed,
    keys: Gather,
    items: Gather,
    charged: bool,
}

impl Task for KeyedTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(k) = input {
            self.keys.push(cx, k)?;
        }
        if let Some(x) = self.walk.next() {
            self.items.push(cx, x.clone())?;
            return Ok(TaskStep::Call(self.walk.f.clone(), vec![x]));
        }
        if !self.charged {
            self.charged = true;
            let n = self.keys.len() as u64;
            let weight = self
                .keys
                .items()
                .iter()
                .fold(0u64, |w, k| w.saturating_add(k.weight()));
            return Ok(TaskStep::Charge(
                weight.saturating_mul(log_work(n)).div_ceil(64),
            ));
        }
        self.build(cx).map(TaskStep::Done)
    }
}

impl KeyedTask {
    fn build(&mut self, cx: &Cx<'_>) -> Result<Value, RtError> {
        // Which elements land where depends on every key.
        let decided = self
            .keys
            .items()
            .iter()
            .fold(self.walk.own, |l, k| l | k.quick());
        let fault = |f| cx.fault(f, decided);
        let mut pace = Pace::new();
        let mut seen = Map::empty();
        let mut out = Gather::new();
        for (k, x) in self.keys.items().iter().zip(self.items.items().iter()) {
            pace.tick(cx, k.weight().saturating_mul(log_work(seen.len())))?;
            match self.mode {
                Keyed::UniqueBy => {
                    if seen.get_entry(k).is_none() {
                        seen = seen
                            .insert(cx.heap, k.clone(), Value::unit())
                            .map_err(fault)?;
                        out.push(cx, x.clone())?;
                    }
                }
                Keyed::GroupBy => {
                    let list = match seen.get_entry(k) {
                        Some((
                            _,
                            Value {
                                repr: Repr::List(l),
                                ..
                            },
                        )) => l.push(cx.heap, x.clone()).map_err(fault)?,
                        _ => List::from_vec(cx.heap, vec![x.clone()]).map_err(fault)?,
                    };
                    seen = seen
                        .insert(cx.heap, k.clone(), Value::new_list(list))
                        .map_err(fault)?;
                }
                Keyed::CountBy => {
                    let n = seen.get_entry(k).and_then(|(_, v)| v.as_int()).unwrap_or(0);
                    seen = seen
                        .insert(cx.heap, k.clone(), Value::int(n.saturating_add(1)))
                        .map_err(fault)?;
                }
            }
        }
        match self.mode {
            Keyed::UniqueBy => out.finish(cx, decided),
            Keyed::GroupBy | Keyed::CountBy => Ok(Value::new_map(seen).with_labels(decided)),
        }
    }
}

/// The task for a `Keyed` built-in over `[T]` and a key function.
pub(super) fn keyed(cx: &Cx<'_>, args: &Args, mode: Keyed) -> Result<Box<dyn Task>, RtError> {
    Ok(Box::new(KeyedTask {
        walk: Walk::new(cx, args)?,
        mode,
        keys: Gather::new(),
        items: Gather::new(),
        charged: false,
    }))
}

pub(super) fn unique_by(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    keyed(cx, args, Keyed::UniqueBy)
}

pub(super) fn group_by(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    keyed(cx, args, Keyed::GroupBy)
}
