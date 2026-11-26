//! The `map` built-ins of chapter 06 (`data`) that stage 6 did not carry.
//!
//! A lookup or update compares the key with the keys on its path, so its result carries their
//! labels too (a hit or a miss is labelled with everything the probe was compared against,
//! `Map::probe`). Built-ins that rebuild a map from several keys carry the labels of every key
//! they compared. Maps charge `1 + ⌈log2(n + 1) · |k| / 64⌉` for a keyed update (stage 6's
//! `s_map_key`), and the bulk ones the class their rows give.

use std::cmp::Ordering;

use crate::error::{Kind, RtError, raise};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::pmap::{Map, MapCursor, key_cmp};
use crate::value::{Repr, Value};

use super::list::{Keyed, keyed};
use super::util::{Gather, Pace};
use super::{Args, Resume, Task, TaskStep, arg, map as map_of};

pub(super) fn len(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let m = arg(cx, args, 0)?;
    let n = i64::try_from(map_of(cx, m)?.len()).unwrap_or(i64::MAX);
    Ok(Value::int(n).with_labels(m.labels))
}

pub(super) fn is_empty(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let m = arg(cx, args, 0)?;
    Ok(Value::bool(map_of(cx, m)?.is_empty()).with_labels(m.labels))
}

pub(super) fn has(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (m, k) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (found, path) = map_of(cx, m)?.probe(k);
    Ok(Value::bool(found.is_some()).with_labels(m.labels | k.labels | path))
}

pub(super) fn remove(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (m, k) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let map = map_of(cx, m)?;
    let (_, path) = map.probe(k);
    let labels = m.labels | k.labels | path;
    let out = map.remove(cx.heap, k).map_err(|f| cx.fault(f, labels))?;
    Ok(Value::new_map(out).with_labels(labels))
}

/// The entries of `m` in key order, each with `f`, gathered; and the union of the own labels
/// of every key (the order and the number of entries depend on them).
fn entries_with(
    cx: &Cx<'_>,
    m: &Value,
    mut each: impl FnMut(&Cx<'_>, Value, Value) -> Result<(), RtError>,
) -> Result<Labels, RtError> {
    let mut keys = Labels::NONE;
    let mut pace = Pace::new();
    let mut c = map_of(cx, m)?.cursor();
    while let Some((k, v)) = c.next() {
        pace.tick(cx, 1)?;
        keys |= k.quick();
        each(cx, k, v)?;
    }
    Ok(keys)
}

pub(super) fn values(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let m = arg(cx, args, 0)?;
    let mut out = Gather::with_capacity(cx, map_of(cx, m)?.len(), m.labels)?;
    let keys = entries_with(cx, m, |cx, _, v| out.push(cx, v))?;
    out.finish(cx, m.labels | keys)
}

pub(super) fn entries(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let m = arg(cx, args, 0)?;
    let mut out = Gather::with_capacity(cx, map_of(cx, m)?.len(), m.labels)?;
    let keys = entries_with(cx, m, |cx, k, v| {
        let pair = Value::new_tuple(cx.heap, vec![k, v]).map_err(|f| cx.fault(f, m.labels))?;
        out.push(cx, pair)
    })?;
    out.finish(cx, m.labels | keys)
}

/// `map.merge(a, b)`: a linear merge of the two key orders; `b` wins on a shared key. The
/// result carries the labels of both maps and of every key compared.
pub(super) fn merge(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (ma, mb) = (map_of(cx, a)?, map_of(cx, b)?);
    let mut keys = Labels::NONE;
    let mut out = Gather::pairs();
    let mut pace = Pace::new();
    let (mut ca, mut cb): (MapCursor, MapCursor) = (ma.cursor(), mb.cursor());
    let (mut ea, mut eb) = (ca.next(), cb.next());
    loop {
        pace.tick(cx, 1)?;
        match (ea.take(), eb.take()) {
            (Some((ka, va)), Some((kb, vb))) => match key_cmp(&ka, &kb) {
                Ordering::Less => {
                    keys |= ka.quick();
                    out.push_pair(cx, ka, va)?;
                    ea = ca.next();
                    eb = Some((kb, vb));
                }
                Ordering::Greater => {
                    keys |= kb.quick();
                    out.push_pair(cx, kb, vb)?;
                    eb = cb.next();
                    ea = Some((ka, va));
                }
                Ordering::Equal => {
                    keys |= ka.quick() | kb.quick();
                    let key = kb.with_labels(ka.quick());
                    out.push_pair(cx, key, vb)?;
                    ea = ca.next();
                    eb = cb.next();
                }
            },
            (Some((ka, va)), None) => {
                keys |= ka.quick();
                out.push_pair(cx, ka, va)?;
                ea = ca.next();
            }
            (None, Some((kb, vb))) => {
                keys |= kb.quick();
                out.push_pair(cx, kb, vb)?;
                eb = cb.next();
            }
            (None, None) => break,
        }
    }
    out.into_map(cx, a.labels | b.labels | keys)
}

/// `map.from(pairs)`: inserts the pairs in order, so later pairs win.
pub(super) fn from(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let pairs = arg(cx, args, 0)?;
    let l = super::list(cx, pairs)?;
    let mut map = Map::empty();
    let mut decided = pairs.labels;
    let mut pace = Pace::new();
    let mut c = l.cursor();
    while let Some(pair) = c.next() {
        pace.tick(
            cx,
            pair.weight()
                .saturating_mul(crate::ops::log_work(map.len())),
        )?;
        let Repr::Tuple(t) = &pair.repr else {
            return Err(raise(
                cx.p,
                Kind::Invalid,
                "`map.from` expects pairs",
                pairs.labels,
                cx.loc,
            ));
        };
        let (Some(k), Some(v)) = (t.items.first(), t.items.get(1)) else {
            return Err(raise(
                cx.p,
                Kind::Invalid,
                "`map.from` expects pairs",
                pairs.labels,
                cx.loc,
            ));
        };
        // A pair's own labels reach its key and its value.
        let (k, v) = (
            k.clone().with_labels(pair.labels),
            v.clone().with_labels(pair.labels),
        );
        decided |= k.quick();
        map = map
            .insert(cx.heap, k, v)
            .map_err(|f| cx.fault(f, decided))?;
    }
    Ok(Value::new_map(map).with_labels(decided))
}

pub(super) fn count_by(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    keyed(cx, args, Keyed::CountBy)
}

// ---- callbacks ----

#[derive(Debug)]
struct UpdateTask {
    m: Value,
    k: Value,
    f: Value,
    default: Value,
    decided: Labels,
}

impl Task for UpdateTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        match input {
            Resume::Start => {
                let (hit, path) = map_of(cx, &self.m)?.probe(&self.k);
                self.decided = self.m.labels | self.k.labels | path;
                let current = match hit {
                    Some((_, v)) => v.clone().with_labels(self.m.labels),
                    None => self.default.clone(),
                };
                Ok(TaskStep::Call(self.f.clone(), vec![current]))
            }
            Resume::Value(next) => {
                let out = map_of(cx, &self.m)?
                    .insert(cx.heap, self.k.clone(), next)
                    .map_err(|f| cx.fault(f, self.decided))?;
                Ok(TaskStep::Done(
                    Value::new_map(out).with_labels(self.decided),
                ))
            }
            _ => Err(raise(
                cx.p,
                Kind::Invalid,
                "a task resumed out of order",
                Labels::NONE,
                cx.loc,
            )),
        }
    }
}

pub(super) fn update(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    let m = arg(cx, args, 0)?.clone();
    map_of(cx, &m)?;
    Ok(Box::new(UpdateTask {
        k: arg(cx, args, 1)?.clone(),
        f: arg(cx, args, 2)?.clone(),
        default: arg(cx, args, 3)?.clone(),
        m,
        decided: Labels::NONE,
    }))
}

/// `map_values` and `filter`: walk the entries in key order, calling `f`.
#[derive(Debug)]
struct EntriesTask {
    cursor: MapCursor,
    own: Labels,
    f: Value,
    filter: bool,
    out: Gather,
    current: Option<(Value, Value)>,
    decided: Labels,
    keys: Labels,
}

impl Task for EntriesTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(r) = input
            && let Some((k, v)) = self.current.take()
        {
            if self.filter {
                self.decided |= r.quick();
                if r.as_bool() == Some(true) {
                    self.out.push_pair(cx, k, v)?;
                }
            } else {
                self.out.push_pair(cx, k, r)?;
            }
        }
        if let Some((k, v)) = self.cursor.next() {
            crate::work::add(1);
            self.keys |= k.quick();
            let args = if self.filter {
                vec![
                    k.clone().with_labels(self.own),
                    v.clone().with_labels(self.own),
                ]
            } else {
                vec![v.clone().with_labels(self.own)]
            };
            self.current = Some((k, v));
            Ok(TaskStep::Call(self.f.clone(), args))
        } else {
            let out = std::mem::replace(&mut self.out, Gather::pairs());
            // Which entries survive, and their order, depend on every key and result.
            let own = self.own | self.decided | self.keys;
            Ok(TaskStep::Done(out.into_map(cx, own)?))
        }
    }
}

fn entries_task(cx: &Cx<'_>, args: &Args, filter: bool) -> Result<Box<dyn Task>, RtError> {
    let m = arg(cx, args, 0)?;
    Ok(Box::new(EntriesTask {
        cursor: map_of(cx, m)?.cursor(),
        own: m.labels,
        f: arg(cx, args, 1)?.clone(),
        filter,
        out: Gather::pairs(),
        current: None,
        decided: Labels::NONE,
        keys: Labels::NONE,
    }))
}

pub(super) fn map_values(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    entries_task(cx, args, false)
}

pub(super) fn filter(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    entries_task(cx, args, true)
}
