//! `diff` (chapter 06, `data`): line and word diffs, unified diffs and patches.
//!
//! **Lines** are what `text.lines` gives (split at `\n` and `\r\n`, no trailing empty line), so
//! a file with and without its final newline, and with `\n` or `\r\n` endings, has the same
//! lines; **words** are what `text.words` gives (runs of non-whitespace), so a word diff does
//! not say anything about spacing. `lines` and `words` return one entry `{ op, text }` per line
//! or word: `equal` (the text of `a`), `delete` (of `a`) and `insert` (of `b`).
//!
//! # The algorithm and its bound
//!
//! The diff is a **shortest edit script**: the common prefix and suffix are removed (a diff of
//! two almost equal files is cheap), the tokens that remain are interned to integers and a
//! longest common subsequence is found with **Hirschberg's linear-space refinement** of the
//! quadratic dynamic program: two rows of `m + 1` counters, a split of the first sequence in
//! half, a forward and a backward pass to find where the optimal path crosses the middle, and
//! the two halves done in turn from an explicit stack. Its time is at most `2 · n · m` cell
//! updates, whatever the input, and its memory `O(n + m)`; there is no recursion. Among shortest
//! scripts it prefers deleting before inserting in a changed region, so the result reads as a
//! unified diff does.
//!
//! The chapter says "(Myers)": Myers' greedy algorithm is `O((n + m) · d)` for an edit script
//! of length `d`, which on two very unequal inputs (a line against a million) is far above the
//! `n · m` that the charge covers, so it cannot be the bound; this one is exactly the product
//! class's `n₁ · n₂ + c₁ + c₂` (times two), and the script it returns has the same length as
//! Myers'.
//!
//! # Charge
//!
//! `lines`, `words` and `unified` are `product`: the sizes are the line (or word) counts
//! `n₁`, `n₂` and the character counts `c₁`, `c₂`, all debited **before running**, so on large
//! inputs they raise `Budget { which: steps }` without having read a line more than the counting
//! needed. The counters (two integers and two buffers of tokens, the interning table, the
//! rows, the script) are charged to the heap before they are allocated, 64 bytes a token and 8
//! bytes a column. A diff of more than 1,000,000 entries is refused (`Invalid`) before the
//! algorithm runs. `apply` and `stats` are `linear`.
//!
//! # Patches
//!
//! `unified` writes `--- from`, `+++ to` and the hunks, with `context` lines of context (a
//! negative count is `Invalid`; so is a header with a line break in it, which would forge
//! lines of the patch) and nothing at all for equal inputs. `apply` reads such a patch and
//! applies it **exactly**: every context and deleted line must equal the line of the text at
//! the place the hunk's header gives (no fuzz, no offset, hunks in order without overlap),
//! otherwise `Invalid` names the hunk. Text before the first hunk is ignored; after it only
//! hunks, `\ No newline at end of file` markers and blank lines are accepted, so a patch for
//! more than one file is refused. The result ends with a newline when the text did (or was
//! empty).
//!
//! Every entry and the list carry the labels of both inputs: which lines are equal depends on
//! all of both.

use std::collections::HashMap;
use std::sync::Arc;

use super::data3::{TextBuf, charged, layout, raise_invalid, record, text_of, text_value};
use super::{Args, Sizes, arg, int, list, new_list, opt, text};
use crate::error::RtError;
use crate::heap::cost;
use crate::labels::Labels;
use crate::machine::Cx;
use crate::value::{Layout, MAX_ELEMS, Repr, Value};
use crate::work;

/// Equal, insert, delete: the order of the `DiffOp` variants.
const EQUAL: u8 = 0;
const INSERT: u8 = 1;
const DELETE: u8 = 2;

/// Bytes charged to the heap for each token of either input (the token list, its id, the
/// interning table and the script).
const TOKEN_BYTES: u64 = 64;

/// The cells between looks at the cancellation flag.
const CHECK_CELLS: u64 = 64 * crate::machine::CHECKPOINT;

fn word_count(t: &str) -> u64 {
    t.split_whitespace().count() as u64
}

fn line_count(t: &str) -> u64 {
    t.lines().count() as u64
}

fn product_of(a: &Args, count: fn(&str) -> u64) -> Sizes {
    let text_of = |i: usize| a.get(i).and_then(Option::as_ref).and_then(Value::as_text);
    let (ta, tb) = (text_of(0).unwrap_or(""), text_of(1).unwrap_or(""));
    let (n1, n2) = (count(ta), count(tb));
    let chars = |i: usize| super::sz(a, i);
    Sizes {
        n: n1,
        n2,
        c: chars(0).saturating_add(chars(1)),
        m: 0,
        scratch: 0,
    }
}

pub(super) fn s_lines(a: &Args) -> Sizes {
    product_of(a, line_count)
}

pub(super) fn s_words(a: &Args) -> Sizes {
    product_of(a, word_count)
}

/// `unified`: the sizes of its two texts, and the characters of its two header names, which
/// are copied into the output.
pub(super) fn s_unified(a: &Args) -> Sizes {
    let mut s = product_of(a, line_count);
    s.c =
        s.c.saturating_add(super::sz(a, 3))
            .saturating_add(super::sz(a, 4));
    s
}

/// The shortest edit script from `a` to `b`, as one of [`EQUAL`], [`INSERT`], [`DELETE`] per
/// step, in order.
fn script(cx: &Cx<'_>, a: &[&str], b: &[&str]) -> Result<Vec<u8>, RtError> {
    let (n1, n2) = (a.len(), b.len());
    let mut prefix = 0;
    while prefix < n1.min(n2) && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < n1.min(n2) - prefix && a[n1 - 1 - suffix] == b[n2 - 1 - suffix] {
        suffix += 1;
    }
    work::add((prefix + suffix) as u64 + 1);
    let mut ops: Vec<u8> = Vec::with_capacity(n1 + n2 - prefix - suffix);
    ops.extend(std::iter::repeat_n(EQUAL, prefix));
    core(
        cx,
        &a[prefix..n1 - suffix],
        &b[prefix..n2 - suffix],
        &mut ops,
    )?;
    ops.extend(std::iter::repeat_n(EQUAL, suffix));
    Ok(ops)
}

/// The script for the tokens left after trimming, appended to `ops`.
fn core<'a>(cx: &Cx<'_>, a: &[&'a str], b: &[&'a str], ops: &mut Vec<u8>) -> Result<(), RtError> {
    if a.is_empty() || b.is_empty() {
        ops.extend(std::iter::repeat_n(DELETE, a.len()));
        ops.extend(std::iter::repeat_n(INSERT, b.len()));
        return Ok(());
    }
    // Intern the tokens: equal text, equal integer.
    let _intern_charge = charged(cx, TOKEN_BYTES.saturating_mul((a.len() + b.len()) as u64))?;
    let mut ids: HashMap<&'a str, u32> = HashMap::with_capacity(a.len() + b.len());
    let mut id_of = |t: &'a str| -> u32 {
        let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        *ids.entry(t).or_insert(next)
    };
    let ai: Vec<u32> = a.iter().map(|t| id_of(t)).collect();
    let bi: Vec<u32> = b.iter().map(|t| id_of(t)).collect();
    work::add((a.len() + b.len()) as u64);
    hirschberg(cx, &ai, &bi, ops)
}

fn cancelled(cx: &Cx<'_>) -> RtError {
    crate::error::raise(
        cx.p,
        crate::error::Kind::Cancelled,
        "the evaluation was cancelled",
        Labels::NONE,
        cx.loc,
    )
}

/// A shortest edit script by Hirschberg's linear-space longest common subsequence, with an
/// explicit stack (see the module documentation).
fn hirschberg(cx: &Cx<'_>, a: &[u32], b: &[u32], ops: &mut Vec<u8>) -> Result<(), RtError> {
    let cols = b.len() + 1;
    let _rows = charged(cx, 8 * cols as u64 + 256 * cost::SLOT)?;
    let mut f = vec![0u32; cols];
    let mut g = vec![0u32; cols];
    let mut stack: Vec<(usize, usize, usize, usize)> = vec![(0, a.len(), 0, b.len())];
    let mut cells = 0u64;
    while let Some((alo, ahi, blo, bhi)) = stack.pop() {
        let (al, bl) = (ahi - alo, bhi - blo);
        if al == 0 {
            ops.extend(std::iter::repeat_n(INSERT, bl));
            continue;
        }
        if bl == 0 {
            ops.extend(std::iter::repeat_n(DELETE, al));
            continue;
        }
        if al == 1 {
            let x = a[alo];
            work::add(bl as u64);
            if let Some(j) = b[blo..bhi].iter().position(|y| *y == x) {
                ops.extend(std::iter::repeat_n(INSERT, j));
                ops.push(EQUAL);
                ops.extend(std::iter::repeat_n(INSERT, bl - j - 1));
            } else {
                ops.push(DELETE);
                ops.extend(std::iter::repeat_n(INSERT, bl));
            }
            continue;
        }
        let mid = alo + al / 2;
        // f[j]: the longest common subsequence of a[alo..mid] and b[blo..blo + j].
        f[..=bl].fill(0);
        for &x in &a[alo..mid] {
            let mut diag = f[0];
            for j in 1..=bl {
                let old = f[j];
                f[j] = if x == b[blo + j - 1] {
                    diag + 1
                } else {
                    old.max(f[j - 1])
                };
                diag = old;
            }
        }
        // g[j]: the longest common subsequence of a[mid..ahi] and b[blo + j..bhi].
        g[..=bl].fill(0);
        for &x in a[mid..ahi].iter().rev() {
            let mut diag = g[bl];
            for j in (0..bl).rev() {
                let old = g[j];
                g[j] = if x == b[blo + j] {
                    diag + 1
                } else {
                    old.max(g[j + 1])
                };
                diag = old;
            }
        }
        let done = (al as u64).saturating_mul(bl as u64);
        work::add(done);
        cells += done;
        if cells >= CHECK_CELLS {
            cells = 0;
            if cx.cancel.is_cancelled() {
                return Err(cancelled(cx));
            }
        }
        // Where the best path crosses the middle row: the first best column, so that in a
        // changed region the deletions come before the insertions.
        let (mut best, mut at) = (f[0] + g[0], 0);
        for k in 1..=bl {
            if f[k] + g[k] > best {
                best = f[k] + g[k];
                at = k;
            }
        }
        stack.push((mid, ahi, blo + at, bhi));
        stack.push((alo, mid, blo, blo + at));
    }
    Ok(())
}

// ---- the entries ----

/// The variant index of a `DiffOp` by name, in the program's enum.
fn op_index(cx: &Cx<'_>, name: &str) -> u32 {
    cx.p.enums
        .get(cx.p.rt.diff_op as usize)
        .and_then(|e| e.variant(name))
        .unwrap_or(0)
}

type Tokens<'a> = (Vec<&'a str>, Vec<&'a str>, crate::heap::Charge);

/// Both token lists of a diff, charged before they are built; `Invalid` when either side
/// would make more entries than a list may hold.
fn tokens<'a>(
    cx: &Cx<'_>,
    ta: &'a str,
    tb: &'a str,
    count: fn(&str) -> u64,
    fill: for<'x> fn(&'x str) -> Vec<&'x str>,
    labels: Labels,
) -> Result<Tokens<'a>, RtError> {
    let (n1, n2) = (count(ta), count(tb));
    if n1.max(n2) > MAX_ELEMS {
        return Err(raise_invalid(
            cx,
            "a diff of more than 1,000,000 entries",
            labels,
        ));
    }
    let charge = charged(cx, TOKEN_BYTES.saturating_mul(n1.saturating_add(n2)))?;
    work::add(ta.len() as u64 + tb.len() as u64);
    Ok((fill(ta), fill(tb), charge))
}

fn lines_of(t: &str) -> Vec<&str> {
    t.lines().collect()
}

fn words_of(t: &str) -> Vec<&str> {
    t.split_whitespace().collect()
}

fn diff_entries(
    cx: &mut Cx<'_>,
    args: &Args,
    count: fn(&str) -> u64,
    fill: for<'x> fn(&'x str) -> Vec<&'x str>,
) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = a.labels | b.labels;
    let (ta, tb) = (text(cx, a)?, text(cx, b)?);
    let (ia, ib, _charge) = tokens(cx, ta, tb, count, fill, labels)?;
    let ops = script(cx, &ia, &ib)?;
    let _items_charge = charged(cx, cost::SLOT.saturating_mul(ops.len() as u64))?;
    let l: Arc<Layout> = layout(&["op", "text"]);
    let indexes = [
        op_index(cx, "equal"),
        op_index(cx, "insert"),
        op_index(cx, "delete"),
    ];
    let enum_ = cx.p.rt.diff_op;
    let mut items = Vec::with_capacity(ops.len());
    let (mut x, mut y) = (0usize, 0usize);
    for op in &ops {
        let (token, index) = match *op {
            EQUAL => {
                x += 1;
                y += 1;
                (ia[x - 1], indexes[0])
            }
            INSERT => {
                y += 1;
                (ib[y - 1], indexes[1])
            }
            _ => {
                x += 1;
                (ia[x - 1], indexes[2])
            }
        };
        let kind = Value::new_variant(
            cx.heap,
            enum_,
            index,
            cx.p.variant_weight(enum_, index),
            None,
        )
        .map_err(|f| cx.fault(f, labels))?
        .with_labels(labels);
        let entry = record(cx, &l, vec![kind, text_of(cx, token, labels)?], labels)?;
        items.push(entry);
    }
    new_list(cx, items, labels)
}

pub(super) fn lines(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    diff_entries(cx, args, line_count, lines_of)
}

pub(super) fn words(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    diff_entries(cx, args, word_count, words_of)
}

pub(super) fn stats(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let d = arg(cx, args, 0)?;
    let l = list(cx, d)?;
    let (equal, insert, delete) = (
        op_index(cx, "equal"),
        op_index(cx, "insert"),
        op_index(cx, "delete"),
    );
    let enum_ = cx.p.rt.diff_op;
    // The counts depend on the list's shape and on every `op`.
    let mut labels = d.labels;
    let (mut added, mut removed, mut unchanged) = (0i64, 0i64, 0i64);
    let mut bad = false;
    l.for_each(&mut |entry| {
        let op = entry.field("op");
        match op.as_ref().map(|o| (&o.repr, o.labels)) {
            Some((Repr::Variant(e, i, _), l)) if *e == enum_ => {
                labels |= l;
                if *i == insert {
                    added += 1;
                } else if *i == delete {
                    removed += 1;
                } else if *i == equal {
                    unchanged += 1;
                } else {
                    bad = true;
                }
            }
            _ => bad = true,
        }
    });
    work::add(l.len());
    if bad {
        return Err(raise_invalid(
            cx,
            "expected a list of diff entries",
            d.quick(),
        ));
    }
    let stat = |n: i64| Value::int(n).with_labels(labels);
    record(
        cx,
        &layout(&["added", "removed", "unchanged"]),
        vec![stat(added), stat(removed), stat(unchanged)],
        labels,
    )
}

// ---- unified ----

/// `start,len` as a hunk header writes it: the line number of the first line (the line before
/// when there is none), the length left out when it is 1.
fn range(first: usize, len: usize) -> String {
    match len {
        0 => format!("{first},0"),
        1 => format!("{}", first + 1),
        _ => format!("{},{len}", first + 1),
    }
}

fn name_arg<'a>(
    cx: &Cx<'_>,
    v: Option<&'a Value>,
    default: &'static str,
) -> Result<&'a str, RtError> {
    match v {
        Some(v) => text(cx, v),
        None => Ok(default),
    }
}

pub(super) fn unified(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (ctx, from, to) = (opt(args, 2), opt(args, 3), opt(args, 4));
    let labels = a.labels
        | b.labels
        | ctx.map_or(Labels::NONE, |c| c.labels)
        | from.map_or(Labels::NONE, |c| c.labels)
        | to.map_or(Labels::NONE, |c| c.labels);
    let context = match ctx {
        Some(c) => int(cx, c)?,
        None => 3,
    };
    let Ok(context) = usize::try_from(context) else {
        return Err(raise_invalid(
            cx,
            "`diff.unified` needs `context` of 0 or more",
            labels,
        ));
    };
    let (from, to) = (name_arg(cx, from, "a")?, name_arg(cx, to, "b")?);
    if from.contains(['\n', '\r']) || to.contains(['\n', '\r']) {
        return Err(raise_invalid(
            cx,
            "a patch header cannot contain a line break",
            labels,
        ));
    }
    let (ta, tb) = (text(cx, a)?, text(cx, b)?);
    let (ia, ib, _charge) = tokens(cx, ta, tb, line_count, lines_of, labels)?;
    let ops = script(cx, &ia, &ib)?;
    let mut out = TextBuf::new(labels);
    // Runs of changes, grouped into hunks when no more than twice the context separates them.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    let mut last_change_end: Option<usize> = None;
    while i < ops.len() {
        if ops[i] == EQUAL {
            i += 1;
            continue;
        }
        let start = i;
        while i < ops.len() && ops[i] != EQUAL {
            i += 1;
        }
        match (last_change_end, hunks.last_mut()) {
            (Some(prev), Some(h)) if start - prev <= context.saturating_mul(2) => h.1 = i,
            _ => hunks.push((start, i)),
        }
        last_change_end = Some(i);
    }
    work::add(ops.len() as u64);
    if hunks.is_empty() {
        return out.finish(cx);
    }
    out.push_str(cx, &format!("--- {from}\n+++ {to}\n"))?;
    // Walk the script once, tracking the line of each side, writing the hunks in order.
    let (mut x, mut y, mut at) = (0usize, 0usize, 0usize);
    for (index, &(first, last)) in hunks.iter().enumerate() {
        let before = if index == 0 {
            first
        } else {
            first - hunks[index - 1].1
        };
        let lo = first - context.min(before);
        let after = if index + 1 == hunks.len() {
            ops.len() - last
        } else {
            hunks[index + 1].0 - last
        };
        let hi = last + context.min(after);
        // Advance to the hunk's first step.
        while at < lo {
            if ops[at] != INSERT {
                x += 1;
            }
            if ops[at] != DELETE {
                y += 1;
            }
            at += 1;
        }
        let (mut old_len, mut new_len) = (0, 0);
        for &op in &ops[lo..hi] {
            if op != INSERT {
                old_len += 1;
            }
            if op != DELETE {
                new_len += 1;
            }
        }
        out.push_str(
            cx,
            &format!("@@ -{} +{} @@\n", range(x, old_len), range(y, new_len)),
        )?;
        while at < hi {
            let (sign, token) = match ops[at] {
                EQUAL => {
                    x += 1;
                    y += 1;
                    (' ', ia[x - 1])
                }
                INSERT => {
                    y += 1;
                    ('+', ib[y - 1])
                }
                _ => {
                    x += 1;
                    ('-', ia[x - 1])
                }
            };
            out.push(cx, sign)?;
            out.push_str(cx, token)?;
            out.push(cx, '\n')?;
            at += 1;
        }
    }
    // The text written is charged too, as the other built-ins' output is: a product has no
    // output part of its own.
    cx.debit = cx.debit.saturating_add((out.len() as u64).div_ceil(64));
    out.finish(cx)
}

// ---- apply ----

/// `@@ -a[,b] +c[,d] @@`: the old start and length, the new length.
fn hunk_header(line: &str) -> Option<(usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let number = |s: &str| s.parse::<usize>().ok().filter(|_| s.len() <= 9);
    let pair = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((number(a)?, number(b)?)),
            None => Some((number(s)?, 1)),
        }
    };
    let (os, ol) = pair(old)?;
    let (_, nl) = pair(new)?;
    Some((os, ol, nl))
}

fn clip(s: &str) -> String {
    let t: String = s.chars().take(80).collect();
    if t.len() < s.len() {
        format!("{t}...")
    } else {
        t
    }
}

pub(super) fn apply(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (t, p) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = t.labels | p.labels;
    let (body, patch) = (text(cx, t)?, text(cx, p)?);
    let n = line_count(body);
    let _charge = charged(cx, TOKEN_BYTES.saturating_mul(n))?;
    let lines: Vec<&str> = body.lines().collect();
    work::add(body.len() as u64 + patch.len() as u64);
    let fail = |cx: &Cx<'_>, why: String| raise_invalid(cx, &why, labels);
    let mut out = TextBuf::new(labels);
    let mut cursor = 0usize;
    let mut plines = patch.lines().peekable();
    let mut hunk_no = 0usize;
    // Anything before the first hunk is a header or noise.
    while plines.peek().is_some_and(|l| !l.starts_with("@@ ")) {
        plines.next();
    }
    while let Some(header) = plines.next() {
        if !header.starts_with("@@ ") {
            if header.is_empty() || header.starts_with('\\') {
                continue;
            }
            return Err(fail(
                cx,
                format!("diff.apply: a line outside any hunk: `{}`", clip(header)),
            ));
        }
        hunk_no += 1;
        let Some((start, old_len, new_len)) = hunk_header(header) else {
            return Err(fail(
                cx,
                format!(
                    "diff.apply: hunk {hunk_no} has a malformed header `{}`",
                    clip(header)
                ),
            ));
        };
        let at = if old_len == 0 {
            start
        } else {
            start.wrapping_sub(1)
        };
        if at < cursor || at.checked_add(old_len).is_none_or(|e| e > lines.len()) {
            return Err(fail(
                cx,
                format!(
                    "diff.apply: hunk {hunk_no} (`{}`) does not apply: it is outside the text or overlaps the one before",
                    clip(header)
                ),
            ));
        }
        for l in &lines[cursor..at] {
            out.push_str(cx, l)?;
            out.push(cx, '\n')?;
        }
        let mut idx = at;
        let (mut seen_old, mut seen_new) = (0usize, 0usize);
        while seen_old < old_len || seen_new < new_len {
            let Some(line) = plines.next() else {
                return Err(fail(
                    cx,
                    format!(
                        "diff.apply: hunk {hunk_no} ends before its counts (`{}`)",
                        clip(header)
                    ),
                ));
            };
            let (sign, rest) = match line.chars().next() {
                None => (' ', ""),
                Some(c @ (' ' | '-' | '+')) => (c, &line[1..]),
                Some('\\') => continue,
                Some(_) => {
                    return Err(fail(
                        cx,
                        format!(
                            "diff.apply: hunk {hunk_no} holds a line that is not part of it: `{}`",
                            clip(line)
                        ),
                    ));
                }
            };
            if sign != '+' {
                // A context or deleted line must be the text's, exactly.
                if idx >= lines.len() || lines[idx] != rest {
                    return Err(fail(
                        cx,
                        format!(
                            "diff.apply: hunk {hunk_no} (`{}`) does not apply at line {}: expected `{}`, found `{}`",
                            clip(header),
                            idx + 1,
                            clip(rest),
                            clip(lines.get(idx).copied().unwrap_or("<end of text>"))
                        ),
                    ));
                }
                idx += 1;
                seen_old += 1;
            }
            if sign != '-' {
                out.push_str(cx, rest)?;
                out.push(cx, '\n')?;
                seen_new += 1;
            }
        }
        cursor = idx;
    }
    for l in &lines[cursor..] {
        out.push_str(cx, l)?;
        out.push(cx, '\n')?;
    }
    let v = out.finish(cx)?;
    // The result ends with a newline when the text did (or was empty).
    if !body.is_empty() && !body.ends_with('\n') {
        let s = v.as_text().unwrap_or("");
        if let Some(trimmed) = s.strip_suffix('\n') {
            let _copy = charged(cx, trimmed.len() as u64)?;
            return text_value(cx, trimmed.to_owned(), labels);
        }
    }
    Ok(v)
}
