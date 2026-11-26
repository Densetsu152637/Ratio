//! The `re` built-ins of chapter 06 (`data`), over the linear-time engine of [`crate::re`].
//!
//! **Compiled once per call site.** The machine compiles a call's pattern before charging it
//! and keeps it in a [`ReCache`] keyed by the call site and the pattern text (at most
//! [`CACHE_ENTRIES`], oldest dropped first), charged to the heap while it is kept. A pattern
//! that does not compile is cached as its refusal, and the built-in raises `Invalid`.
//!
//! **Charges** (`product`): before running, `(n + 1) · W · (1 + k) + c` work, where `n` is the
//! text's characters, `W` the program's weight, `k` the slots the search tracks (0 for
//! `is_match`, 2 for spans, every group's for `captures` and for a replacement that refers to
//! groups) and `c` the characters of the pattern, the text and the replacement; and the VM's
//! working memory on the heap. A single search counts at most four times that work. Iterating
//! (`find_all`, `captures_all`, `replace_all`, `split`) searches again from each match's end,
//! which a pathological pattern can make quadratic, as in Rust's `regex`: work past four times
//! the charge is debited as it is done, and the call stops with `Budget { which: steps }` when
//! the steps left cannot pay, so no call runs unpaid. Building a replacement is work too.
//!
//! Spans (`start`, `end`) count characters, as every `text` index does. Results carry the
//! labels of the pattern, the text and any other argument.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::text::{labels_of, new_record};
use super::{Args, Sizes, arg, int, new_list, new_text, opt, sz, text};
use crate::error::{Kind, RtError, raise};
use crate::heap::{Charge, Heap, HeapExhausted, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::pmap::Map;
use crate::re::{Matches, Regex, UNSET, scratch_bytes, search_work};
use crate::value::{MAX_ELEMS, MAX_STR_BYTES, Value};

/// A call site's pattern: compiled, or why it was refused.
pub(crate) type Compiled = Result<Regex, String>;

/// The compiled patterns an evaluation keeps.
pub(crate) const CACHE_ENTRIES: usize = 32;

/// The heap bytes a compiled program is charged per unit of weight.
const PROGRAM_BYTES_PER_WEIGHT: u64 = 16;

type Key = (u32, u32, u32, Box<str>);

/// The compiled patterns of an evaluation's `re` call sites.
#[derive(Debug, Default)]
pub(crate) struct ReCache {
    entries: HashMap<Key, (Arc<Compiled>, Charge)>,
    order: VecDeque<Key>,
}

impl ReCache {
    /// The pattern compiled for this call site, compiling and caching it on first use.
    pub(crate) fn get(
        &mut self,
        loc: ratio_ir::Loc,
        pattern: &str,
        heap: &Heap,
    ) -> Result<Arc<Compiled>, HeapExhausted> {
        if pattern.len() > crate::re::MAX_PATTERN_BYTES {
            // Refused without compiling, and not kept.
            return Ok(Arc::new(Err("the pattern is longer than 4 KiB".to_owned())));
        }
        let key: Key = (loc.module, loc.line, loc.col, pattern.into());
        if let Some((compiled, _)) = self.entries.get(&key) {
            return Ok(Arc::clone(compiled));
        }
        let compiled = Regex::new(pattern);
        let weight = compiled.as_ref().map_or(0, Regex::weight);
        let charge = heap.alloc(
            cost::STR + pattern.len() as u64 + weight.saturating_mul(PROGRAM_BYTES_PER_WEIGHT),
        )?;
        if self.order.len() == CACHE_ENTRIES
            && let Some(old) = self.order.pop_front()
        {
            self.entries.remove(&old);
        }
        let compiled = Arc::new(compiled);
        self.order.push_back(key.clone());
        self.entries.insert(key, (Arc::clone(&compiled), charge));
        Ok(compiled)
    }
}

// ---- sizing ----

fn sizes(a: &Args, re: &Regex, ns: usize, extra: u64) -> Sizes {
    let n = sz(a, 1);
    Sizes {
        n: n.saturating_add(1),
        n2: re.weight().saturating_mul(ns as u64 + 1),
        c: sz(a, 0).saturating_add(n).saturating_add(extra),
        m: 0,
        scratch: scratch_bytes(re.insts(), ns as u64),
    }
}

pub(super) fn s_is_match(a: &Args, re: &Regex) -> Sizes {
    sizes(a, re, 0, 0)
}

pub(super) fn s_find(a: &Args, re: &Regex) -> Sizes {
    sizes(a, re, 2, 0)
}

pub(super) fn s_captures(a: &Args, re: &Regex) -> Sizes {
    sizes(a, re, re.slots(true), 0)
}

pub(super) fn s_replace(a: &Args, re: &Regex) -> Sizes {
    let with = a.get(2).and_then(Option::as_ref);
    let all = with
        .and_then(Value::as_text)
        .is_some_and(|w| w.contains('$'));
    sizes(a, re, re.slots(all), sz(a, 2))
}

// ---- the call's context ----

/// What every `re` body needs: the program, the text, the labels and the work allowance.
struct Call<'a> {
    re: Arc<Compiled>,
    hay: &'a str,
    labels: Labels,
    /// The work charged before running, times four: work past it is debited as it is done.
    allowance: u64,
    work: u64,
}

impl Call<'_> {
    fn re(&self) -> &Regex {
        match &*self.re {
            Ok(re) => re,
            // `start` refuses a pattern that did not compile.
            Err(_) => unreachable!("checked in `start`"),
        }
    }
}

fn start<'a>(cx: &mut Cx<'_>, args: &'a Args, ns: usize, extra: u64) -> Result<Call<'a>, RtError> {
    let pattern = arg(cx, args, 0)?;
    let s = arg(cx, args, 1)?;
    text(cx, pattern)?;
    let hay = text(cx, s)?;
    let labels = labels_of(args);
    let Some(re) = cx.regex.clone() else {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "expected a pattern",
            labels,
            cx.loc,
        ));
    };
    if let Err(why) = &*re {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            &format!("an invalid regular expression: {why}"),
            pattern.labels,
            cx.loc,
        ));
    }
    let pre = {
        let Ok(r) = &*re else { unreachable!() };
        let n = hay.chars().count() as u64;
        search_work(r.weight(), n, ns as u64)
            .saturating_add(n)
            .saturating_add(text(cx, pattern)?.chars().count() as u64)
            .saturating_add(extra)
    };
    Ok(Call {
        re,
        hay,
        labels,
        allowance: pre.saturating_mul(4),
        work: 0,
    })
}

impl Call<'_> {
    /// Debits the work past the allowance, or stops when the steps left cannot pay it.
    fn pay(&self, cx: &mut Cx<'_>) -> Result<(), RtError> {
        let extra = self.work.saturating_sub(self.allowance).div_ceil(64);
        if extra > cx.steps_left {
            cx.work += self.work;
            return Err(raise(
                cx.p,
                Kind::Budget("steps"),
                "the step limit was reached",
                Labels::NONE,
                cx.loc,
            ));
        }
        Ok(())
    }

    fn done(&self, cx: &mut Cx<'_>) {
        cx.debit = self.work.saturating_sub(self.allowance).div_ceil(64);
        cx.work += self.work;
    }
}

fn limit(cx: &Cx<'_>, args: &Args, i: usize, labels: Labels) -> Result<u64, RtError> {
    let l = match opt(args, i) {
        Some(v) if !v.is_none() => int(cx, v)?,
        _ => 1000,
    };
    u64::try_from(l)
        .ok()
        .filter(|l| *l <= MAX_ELEMS)
        .ok_or_else(|| {
            raise(
                cx.p,
                Kind::Invalid,
                "a match limit below 0 or above 1,000,000",
                labels,
                cx.loc,
            )
        })
}

/// Byte offsets to character indexes, for offsets visited in increasing order.
struct Chars<'a> {
    hay: &'a str,
    byte: usize,
    ch: usize,
}

impl Chars<'_> {
    fn at(&mut self, byte: usize) -> i64 {
        if byte >= self.byte {
            self.ch += self.hay[self.byte..byte].chars().count();
        } else {
            self.ch -= self.hay[byte..self.byte].chars().count();
        }
        self.byte = byte;
        i64::try_from(self.ch).unwrap_or(i64::MAX)
    }
}

fn piece(cx: &Cx<'_>, s: &str, labels: Labels) -> Result<Value, RtError> {
    new_text(cx, s.to_owned(), labels)
}

/// `{ text, start, end }`.
fn match_value(
    cx: &Cx<'_>,
    call: &Call<'_>,
    pos: &mut Chars<'_>,
    slots: &[usize],
) -> Result<Value, RtError> {
    let (s, e) = (slots[0], slots[1]);
    let l = call.labels;
    let fields = vec![
        ("text", piece(cx, &call.hay[s..e], l)?),
        ("start", Value::int(pos.at(s)).with_labels(l)),
        ("end", Value::int(pos.at(e)).with_labels(l)),
    ];
    new_record(cx, fields, l)
}

/// `{ groups: [S?], named: Map<Str, S> }`: every group (0 the whole match), `none` for one
/// that did not take part; the named groups that did.
fn captures_value(cx: &Cx<'_>, call: &Call<'_>, slots: &[usize]) -> Result<Value, RtError> {
    let l = call.labels;
    let re = call.re();
    let group = |i: usize| match (slots.get(2 * i), slots.get(2 * i + 1)) {
        (Some(&s), Some(&e)) if s != UNSET && e != UNSET => Some(&call.hay[s..e]),
        _ => None,
    };
    let mut groups = Vec::with_capacity(re.groups as usize);
    for i in 0..re.groups as usize {
        groups.push(match group(i) {
            Some(t) => piece(cx, t, l)?,
            None => Value::none().with_labels(l),
        });
    }
    let mut named = Map::empty();
    for (name, i) in &re.names {
        if let Some(t) = group(*i as usize) {
            named = named
                .insert(cx.heap, piece(cx, name, l)?, piece(cx, t, l)?)
                .map_err(|f| cx.fault(f, l))?;
        }
    }
    let fields = vec![
        ("groups", new_list(cx, groups, l)?),
        ("named", Value::new_map(named).with_labels(l)),
    ];
    new_record(cx, fields, l)
}

// ---- the built-ins ----

pub(super) fn is_match(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 0, 0)?;
    let mut work = 0;
    let mut vm = call.re().vm(0);
    let found = vm
        .search(&call.re().program, call.hay, 0, &mut work)
        .is_some();
    call.work += work;
    call.done(cx);
    Ok(Value::bool(found).with_labels(call.labels))
}

pub(super) fn find(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 2, 0)?;
    let mut work = 0;
    let mut vm = call.re().vm(2);
    let found = vm.search(&call.re().program, call.hay, 0, &mut work);
    call.work += work;
    call.done(cx);
    let mut pos = Chars {
        hay: call.hay,
        byte: 0,
        ch: 0,
    };
    match found {
        Some(slots) => match_value(cx, &call, &mut pos, &slots),
        None => Ok(Value::none().with_labels(call.labels)),
    }
}

pub(super) fn find_all(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 2, 0)?;
    let max = limit(cx, args, 2, call.labels)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    let mut matches = Matches::new(r, call.hay, 2);
    let mut pos = Chars {
        hay: call.hay,
        byte: 0,
        ch: 0,
    };
    let mut items = Vec::new();
    while (items.len() as u64) < max {
        let Some(slots) = matches.next(&mut call.work) else {
            break;
        };
        call.pay(cx)?;
        items.push(match_value(cx, &call, &mut pos, &slots)?);
    }
    call.done(cx);
    new_list(cx, items, call.labels)
}

pub(super) fn captures(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let ns = match &cx.regex.as_deref() {
        Some(Ok(re)) => re.slots(true),
        _ => 2,
    };
    let mut call = start(cx, args, ns, 0)?;
    let mut work = 0;
    let mut vm = call.re().vm(ns);
    let found = vm.search(&call.re().program, call.hay, 0, &mut work);
    call.work += work;
    call.done(cx);
    match found {
        Some(slots) => captures_value(cx, &call, &slots),
        None => Ok(Value::none().with_labels(call.labels)),
    }
}

pub(super) fn captures_all(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let ns = match &cx.regex.as_deref() {
        Some(Ok(re)) => re.slots(true),
        _ => 2,
    };
    let mut call = start(cx, args, ns, 0)?;
    let max = limit(cx, args, 2, call.labels)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    let mut matches = Matches::new(r, call.hay, ns);
    let mut items = Vec::new();
    while (items.len() as u64) < max {
        let Some(slots) = matches.next(&mut call.work) else {
            break;
        };
        call.pay(cx)?;
        items.push(captures_value(cx, &call, &slots)?);
    }
    call.done(cx);
    new_list(cx, items, call.labels)
}

pub(super) fn replace(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    replacing(cx, args, false)
}

pub(super) fn replace_all(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    replacing(cx, args, true)
}

fn replacing(cx: &mut Cx<'_>, args: &Args, all: bool) -> Result<Value, RtError> {
    let with = text(cx, arg(cx, args, 2)?)?;
    let groups = with.contains('$');
    let ns = match &cx.regex.as_deref() {
        Some(Ok(re)) => re.slots(groups),
        _ => 2,
    };
    let mut call = start(cx, args, ns, with.chars().count() as u64)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    let mut matches = Matches::new(r, call.hay, ns);
    let mut out = String::new();
    let mut last = 0;
    let over = |cx: &mut Cx<'_>, call: &Call<'_>, written: u64| {
        cx.debit = call.work.saturating_add(written).div_ceil(64);
        cx.work += call.work + written;
        raise(
            cx.p,
            Kind::Invalid,
            "the replaced text is longer than 16 MiB",
            call.labels,
            cx.loc,
        )
    };
    while let Some(slots) = matches.next(&mut call.work) {
        call.pay(cx)?;
        let (s, e) = (slots[0], slots[1]);
        if out.len() + (s - last) > MAX_STR_BYTES {
            let written = out.len() as u64;
            return Err(over(cx, &call, written));
        }
        out.push_str(&call.hay[last..s]);
        call.work += (s - last) as u64;
        match crate::re::expand(r, with, call.hay, &slots, &mut out, MAX_STR_BYTES) {
            Ok(w) => call.work += w,
            Err(w) => {
                call.work += w;
                return Err(over(cx, &call, 0));
            }
        }
        last = e;
        if !all {
            break;
        }
    }
    if out.len() + (call.hay.len() - last) > MAX_STR_BYTES {
        let written = out.len() as u64;
        return Err(over(cx, &call, written));
    }
    out.push_str(&call.hay[last..]);
    call.work += (call.hay.len() - last) as u64;
    call.pay(cx)?;
    call.done(cx);
    new_text(cx, out, call.labels)
}

pub(super) fn split(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 2, 0)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    let mut matches = Matches::new(r, call.hay, 2);
    let mut items = Vec::new();
    let mut last = 0;
    while let Some(slots) = matches.next(&mut call.work) {
        call.pay(cx)?;
        items.push(piece(cx, &call.hay[last..slots[0]], call.labels)?);
        last = slots[1];
    }
    items.push(piece(cx, &call.hay[last..], call.labels)?);
    call.work += call.hay.len() as u64;
    call.done(cx);
    new_list(cx, items, call.labels)
}

pub(super) fn escape(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    let escaped = crate::re::escape(t);
    cx.work += (t.len() + escaped.len()) as u64 + 1;
    new_text(cx, escaped, s.labels)
}
