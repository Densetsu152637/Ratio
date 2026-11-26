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
//! groups) and `c` the compile work (four units per pattern character, and `W`), the text's characters
//! and the replacement's; and the VM's working memory on the heap. The compile work is charged
//! whether or not the pattern was already compiled, and a refused pattern is charged the work
//! it took to refuse. A single search counts at most four times its charge. Iterating
//! (`find_all`, `captures_all`, `replace_all`, `split`) searches again from each match's end,
//! which a pathological pattern can make quadratic, as in Rust's `regex`: work past four times
//! the charge is debited as it is done, and the call stops with `Budget { which: steps }` when
//! the steps left cannot pay, so no call runs unpaid. The **output** is charged too, one unit
//! per element and per character built (a match record, a group, a named entry, a split part,
//! a replacement's text), debited as it is built. Searches stop with `Cancelled` within a
//! checkpoint's worth of work of a cancellation.
//!
//! Spans (`start`, `end`) count characters, as every `text` index does. Results carry the
//! labels of the pattern, the text and any other argument.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::text::{Items, labels_of, new_record};
use super::{Args, Sizes, arg, int, new_text, opt, sz, text};
use crate::error::{Kind, RtError, raise};
use crate::heap::{Charge, Heap, HeapExhausted, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::pmap::Map;
use crate::re::{Matches, Refusal, Regex, Stopped, Template, UNSET, scratch_bytes, search_work};
use crate::value::{MAX_ELEMS, MAX_STR_BYTES, Value};

/// A call site's pattern: compiled, or why it was refused.
pub(crate) type Compiled = Result<Regex, Refusal>;

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
            // Refused without compiling (or hashing), and not kept.
            return Ok(Arc::new(Regex::new(pattern)));
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

/// The work charged before running (see the module's documentation).
fn pre_work(re: &Regex, n: u64, ns: usize, extra: u64) -> u64 {
    search_work(re.weight(), n, ns as u64)
        .saturating_add(re.compile_work)
        .saturating_add(n)
        .saturating_add(extra)
}

fn sizes(a: &Args, re: &Regex, ns: usize, extra: u64) -> Sizes {
    let n = sz(a, 1);
    Sizes {
        n: n.saturating_add(1),
        n2: re.weight().saturating_mul(ns as u64 + 1),
        c: re.compile_work.saturating_add(n).saturating_add(extra),
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

/// Whether a replacement refers to groups (so its searches track every group's slots).
fn replacement_groups(re: &Regex, with: Option<&str>) -> bool {
    with.is_some_and(|w| w.contains('$') && Template::new(re, w).0.uses_groups())
}

pub(super) fn s_replace(a: &Args, re: &Regex) -> Sizes {
    let with = a.get(2).and_then(Option::as_ref).and_then(Value::as_text);
    sizes(a, re, re.slots(replacement_groups(re, with)), sz(a, 2))
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
    /// Output built (elements and characters), debited as it is built.
    out: u64,
    /// When to check cancellation next between searches (work and output so far).
    next_check: u64,
}

/// The work between cancellation checks of an iteration (one checkpoint's worth).
const CHECK_WORK: u64 = 64 * crate::machine::CHECKPOINT;

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
    let pre = match &*re {
        Err(why) => {
            return Err(raise(
                cx.p,
                Kind::Invalid,
                &format!("an invalid regular expression: {}", why.message),
                pattern.labels,
                cx.loc,
            ));
        }
        Ok(r) => pre_work(r, hay.chars().count() as u64, ns, extra),
    };
    Ok(Call {
        re,
        hay,
        labels,
        allowance: pre.saturating_mul(4),
        work: 0,
        out: 0,
        next_check: CHECK_WORK,
    })
}

fn cancelled(cx: &Cx<'_>) -> RtError {
    raise(
        cx.p,
        Kind::Cancelled,
        "the evaluation was cancelled",
        Labels::NONE,
        cx.loc,
    )
}

impl Call<'_> {
    fn re(&self) -> &Regex {
        match &*self.re {
            Ok(re) => re,
            // `start` refuses a pattern that did not compile.
            Err(_) => unreachable!("checked in `start`"),
        }
    }

    /// The steps owed beyond the charge debited before running.
    fn owed(&self) -> u64 {
        self.work
            .saturating_sub(self.allowance)
            .saturating_add(self.out)
            .div_ceil(64)
    }

    /// Stops when the steps left cannot pay what is owed.
    fn pay(&self, cx: &mut Cx<'_>) -> Result<(), RtError> {
        if self.owed() > cx.steps_left {
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

    /// The next match, paying as it goes.
    fn next(
        &mut self,
        cx: &mut Cx<'_>,
        m: &mut Matches<'_>,
    ) -> Result<Option<Vec<usize>>, RtError> {
        if self.work + self.out >= self.next_check {
            if cx.cancel.is_cancelled() {
                return Err(cancelled(cx));
            }
            self.next_check = self.work + self.out + CHECK_WORK;
        }
        let cancel = cx.cancel.clone();
        match m.next(&mut self.work, &|| cancel.is_cancelled()) {
            Ok(found) => {
                self.pay(cx)?;
                Ok(found)
            }
            Err(Stopped) => Err(cancelled(cx)),
        }
    }

    /// One search from the start of the text.
    fn search(&mut self, cx: &Cx<'_>, ns: usize) -> Result<Option<Vec<usize>>, RtError> {
        let re = Arc::clone(&self.re);
        let Ok(r) = &*re else {
            unreachable!("checked in `start`")
        };
        let mut vm = r.vm(ns);
        let cancel = cx.cancel.clone();
        vm.search(&r.program, self.hay, 0, &mut self.work, &|| {
            cancel.is_cancelled()
        })
        .map_err(|Stopped| cancelled(cx))
    }

    /// New text for the result, counted as output.
    fn piece(&mut self, cx: &Cx<'_>, s: &str) -> Result<Value, RtError> {
        self.out += s.len() as u64 + 1;
        new_text(cx, s.to_owned(), self.labels)
    }
}

/// Debits what the call owes, whether it returned or raised an error a program can see
/// (`Budget` and `Cancelled` end the turn; nothing more is charged).
fn settle(
    cx: &mut Cx<'_>,
    call: &Call<'_>,
    result: Result<Value, RtError>,
) -> Result<Value, RtError> {
    if result
        .as_ref()
        .err()
        .is_none_or(|e| e.class() == crate::ErrorClass::Catchable)
    {
        cx.debit = call.owed();
    }
    cx.work += call.work + call.out;
    result
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

/// `{ text, start, end }`.
fn match_value(
    cx: &Cx<'_>,
    call: &mut Call<'_>,
    pos: &mut Chars<'_>,
    slots: &[usize],
) -> Result<Value, RtError> {
    let (s, e) = (slots[0], slots[1]);
    let l = call.labels;
    call.out += 3;
    let hay = call.hay;
    let fields = vec![
        ("text", call.piece(cx, &hay[s..e])?),
        ("start", Value::int(pos.at(s)).with_labels(l)),
        ("end", Value::int(pos.at(e)).with_labels(l)),
    ];
    new_record(cx, fields, l)
}

/// The named groups' names as values, built once per call and shared by every match.
fn group_names(cx: &Cx<'_>, call: &mut Call<'_>) -> Result<Vec<(Value, u32)>, RtError> {
    let names: Vec<(String, u32)> = call.re().names.clone();
    names
        .iter()
        .map(|(n, i)| Ok((call.piece(cx, n)?, *i)))
        .collect()
}

/// `{ groups: [S?], named: Map<Str, S> }`: every group (0 the whole match), `none` for one
/// that did not take part; the named groups that did.
fn captures_value(
    cx: &Cx<'_>,
    call: &mut Call<'_>,
    names: &[(Value, u32)],
    slots: &[usize],
) -> Result<Value, RtError> {
    let l = call.labels;
    let hay = call.hay;
    let group = |i: usize| match (slots.get(2 * i), slots.get(2 * i + 1)) {
        (Some(&s), Some(&e)) if s != UNSET && e != UNSET => Some(&hay[s..e]),
        _ => None,
    };
    let count = call.re().groups as usize;
    let mut groups = Items::new();
    for i in 0..count {
        let v = if let Some(t) = group(i) {
            call.piece(cx, t)?
        } else {
            call.out += 1;
            Value::none().with_labels(l)
        };
        groups.push(cx, v, l)?;
    }
    let mut named = Map::empty();
    for (name, i) in names {
        if let Some(t) = group(*i as usize) {
            call.out += 1;
            let v = call.piece(cx, t)?;
            named = named
                .insert(cx.heap, name.clone(), v)
                .map_err(|f| cx.fault(f, l))?;
        }
    }
    call.out += 2;
    let fields = vec![
        ("groups", groups.finish(cx, l)?),
        ("named", Value::new_map(named).with_labels(l)),
    ];
    new_record(cx, fields, l)
}

// ---- the built-ins ----

pub(super) fn is_match(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 0, 0)?;
    let labels = call.labels;
    let result = call
        .search(cx, 0)
        .map(|m| Value::bool(m.is_some()).with_labels(labels));
    settle(cx, &call, result)
}

pub(super) fn find(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 2, 0)?;
    let result = (|| {
        let found = call.search(cx, 2)?;
        let mut pos = Chars {
            hay: call.hay,
            byte: 0,
            ch: 0,
        };
        match found {
            Some(slots) => match_value(cx, &mut call, &mut pos, &slots),
            None => Ok(Value::none().with_labels(call.labels)),
        }
    })();
    settle(cx, &call, result)
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
    let mut items = Items::new();
    let result = (|| {
        while (items.len() as u64) < max {
            let Some(slots) = call.next(cx, &mut matches)? else {
                break;
            };
            let v = match_value(cx, &mut call, &mut pos, &slots)?;
            items.push(cx, v, call.labels)?;
            call.pay(cx)?;
        }
        items.finish(cx, call.labels)
    })();
    settle(cx, &call, result)
}

pub(super) fn captures(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let ns = match cx.regex.as_deref() {
        Some(Ok(re)) => re.slots(true),
        _ => 2,
    };
    let mut call = start(cx, args, ns, 0)?;
    let result = (|| match call.search(cx, ns)? {
        Some(slots) => {
            let names = group_names(cx, &mut call)?;
            captures_value(cx, &mut call, &names, &slots)
        }
        None => Ok(Value::none().with_labels(call.labels)),
    })();
    settle(cx, &call, result)
}

pub(super) fn captures_all(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let ns = match cx.regex.as_deref() {
        Some(Ok(re)) => re.slots(true),
        _ => 2,
    };
    let mut call = start(cx, args, ns, 0)?;
    let max = limit(cx, args, 2, call.labels)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    let mut matches = Matches::new(r, call.hay, ns);
    let mut items = Items::new();
    let result = (|| {
        let names = group_names(cx, &mut call)?;
        while (items.len() as u64) < max {
            let Some(slots) = call.next(cx, &mut matches)? else {
                break;
            };
            let v = captures_value(cx, &mut call, &names, &slots)?;
            items.push(cx, v, call.labels)?;
            call.pay(cx)?;
        }
        items.finish(cx, call.labels)
    })();
    settle(cx, &call, result)
}

pub(super) fn replace(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    replacing(cx, args, false)
}

pub(super) fn replace_all(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    replacing(cx, args, true)
}

fn too_long(cx: &Cx<'_>, call: &Call<'_>) -> RtError {
    raise(
        cx.p,
        Kind::Invalid,
        "the replaced text is longer than 16 MiB",
        call.labels,
        cx.loc,
    )
}

fn replacing(cx: &mut Cx<'_>, args: &Args, all: bool) -> Result<Value, RtError> {
    let with = text(cx, arg(cx, args, 2)?)?;
    let ns = match cx.regex.as_deref() {
        Some(Ok(re)) => re.slots(replacement_groups(re, Some(with))),
        _ => 2,
    };
    let mut call = start(cx, args, ns, with.chars().count() as u64)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    // The template is resolved against the group names once per call.
    let (template, resolved) = Template::new(r, with);
    call.work += resolved;
    let mut matches = Matches::new(r, call.hay, ns);
    let mut out = String::new();
    let mut last = 0;
    let hay = call.hay;
    let result = (|| {
        while let Some(slots) = call.next(cx, &mut matches)? {
            let (s, e) = (slots[0], slots[1]);
            if out.len() + (s - last) > MAX_STR_BYTES {
                return Err(too_long(cx, &call));
            }
            out.push_str(&hay[last..s]);
            call.out += (s - last) as u64;
            match template.expand(hay, &slots, &mut out, MAX_STR_BYTES) {
                Ok(w) => call.out += w,
                Err(w) => {
                    call.out += w;
                    return Err(too_long(cx, &call));
                }
            }
            call.pay(cx)?;
            last = e;
            if !all {
                break;
            }
        }
        if out.len() + (hay.len() - last) > MAX_STR_BYTES {
            return Err(too_long(cx, &call));
        }
        out.push_str(&hay[last..]);
        call.out += (hay.len() - last) as u64 + 1;
        call.pay(cx)?;
        new_text(cx, std::mem::take(&mut out), call.labels)
    })();
    settle(cx, &call, result)
}

/// `re.split`: the parts between matches, built as they are found, each charged; more than
/// 1,000,000 parts is `Invalid`.
pub(super) fn split(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let mut call = start(cx, args, 2, 0)?;
    let re = Arc::clone(&call.re);
    let Ok(r) = &*re else { unreachable!() };
    let mut matches = Matches::new(r, call.hay, 2);
    let mut items = Items::new();
    let hay = call.hay;
    let result = (|| {
        let mut last = 0;
        while let Some(slots) = call.next(cx, &mut matches)? {
            let v = call.piece(cx, &hay[last..slots[0]])?;
            items.push(cx, v, call.labels)?;
            call.pay(cx)?;
            last = slots[1];
        }
        let v = call.piece(cx, &hay[last..])?;
        items.push(cx, v, call.labels)?;
        call.pay(cx)?;
        items.finish(cx, call.labels)
    })();
    settle(cx, &call, result)
}

pub(super) fn escape(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    let escaped = crate::re::escape(t);
    cx.work += (t.len() + escaped.len()) as u64 + 1;
    new_text(cx, escaped, s.labels)
}
