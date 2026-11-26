//! `template` (chapter 06, `data`): logic-less templates, a Mustache subset.
//!
//! The tags are `{{name}}`, `{{a.b}}`, `{{.}}` (the current value, which makes lists of scalars
//! renderable), `{{#name}}…{{/name}}` and `{{^name}}…{{/name}}`; spaces inside the braces are
//! ignored. There are no partials, lambdas, comments, `{{{raw}}}`, `{{&raw}}`, delimiter
//! changes or HTML escaping, and no "standalone line" trimming: the text between tags is copied
//! exactly. Anything else between `{{` and `}}`, an unclosed or mismatched tag, nesting deeper
//! than 128 sections and a name that is not `identifier(.identifier)*` or `.` is a syntax error
//! (`Invalid`, with the line). A template can reach nothing but its two arguments: it is parsed
//! into a flat list of nodes and walked with an explicit stack, never evaluated.
//!
//! **Names** are looked up in the innermost scope first and then outward (a record's fields, a
//! map's `Str` keys); a dotted name finds its first part so and descends from there. A section
//! over a **list** renders its body once per element with the element as the scope (an empty
//! list renders nothing); over any other truthy value once with that value as the scope;
//! `{{^name}}` renders its body when the value is falsy. Falsy: `none`, `false`, the empty
//! text, the empty list and the empty map, and a name that is not there (a record's absent
//! optional field cannot be told from a misspelt one). A plain `{{name}}` of a name that is
//! not there, or is `none`, is `Invalid` (a missing name), and so is one that is a list, map,
//! record or tuple: only text, numbers, booleans, durations, money, unit variants and their
//! `Json` forms fill a tag, in the literal form `text.show` gives them.
//!
//! The result is `Untrusted<Str>` in the checker's table (the rule "untrusted if any filled
//! value is" cannot be written there), and carries the labels of the template, of every
//! container consulted on the way to a name, of every value that filled a tag or decided a
//! section, and of a name that was not found.
//!
//! **Charge.** `render` is `linear`: `n` is the characters of the template plus the weight of
//! the variables, `m` the characters written (at most 16 MiB; a longer expansion is refused
//! and pays for what was written). The walk can do more than `n + m` work in two ways, a
//! section whose body writes nothing over a large list, and a name looked up through many
//! scopes; it pays for that too, *before* doing it, in doubling instalments: when the work it
//! has done passes what the charge covers (`n` plus the characters written so far plus 4,096),
//! it asks the evaluator to debit as many more units again as are covered so far (a step per
//! 64) and goes on only if the steps are there (`Budget { which: steps }` otherwise). A template
//! whose tags all write something never pays more than the class says.

#![allow(
    clippy::cast_possible_truncation,
    reason = "offsets into a template, which is at most 16 MiB"
)]

use std::collections::HashSet;

use super::data3::{TextBuf, charged, raise_invalid};
use super::{Args, Resume, Sizes, Task, TaskStep, arg, new_list, new_text, sz, text, weight};
use crate::error::{Kind, RtError, raise};
use crate::heap::{Charge, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::plist::Cursor;
use crate::value::{Json, Repr, Value};
use crate::work;

/// The deepest nesting of sections.
const MAX_DEPTH: usize = 128;

/// Work between two looks at the cancellation flag.
const CHECK_UNITS: u64 = 64 * 1000;

/// The units of work the charge covers beyond `n` and the output.
const SLACK: u64 = 4096;

pub(super) fn s_template(a: &Args) -> Sizes {
    Sizes {
        n: sz(a, 0).saturating_add(weight(a, 1)),
        ..Sizes::default()
    }
}

#[derive(Clone, Copy, Debug)]
struct Name {
    start: u32,
    end: u32,
}

#[derive(Clone, Copy, Debug)]
enum Node {
    Text(u32, u32),
    Var(Name),
    /// A section; `close` is the index of its `Close`.
    Open {
        name: Name,
        inverted: bool,
        close: u32,
    },
    Close,
}

/// The line of `offset` in `tpl`, from 1.
fn line_of(tpl: &str, offset: usize) -> usize {
    tpl.as_bytes()[..offset.min(tpl.len())]
        .split(|b| *b == b'\n')
        .count()
}

fn valid_name(n: &str) -> bool {
    n == "."
        || n.split('.').all(|seg| {
            let mut c = seg.chars();
            c.next()
                .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
                && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        })
}

/// Parses a template into nodes (charged to the heap first, from an upper bound).
fn parse(cx: &Cx<'_>, tpl: &str, labels: Labels) -> Result<(Vec<Node>, Charge), RtError> {
    let tags = tpl.matches("{{").count() as u64;
    let charge = charged(
        cx,
        tags.saturating_mul(2)
            .saturating_add(1)
            .saturating_mul(cost::SLOT),
    )?;
    work::add(tpl.len() as u64);
    let syntax = |cx: &Cx<'_>, at: usize, why: &str| {
        raise_invalid(
            cx,
            &format!("template line {}: {why}", line_of(tpl, at)),
            labels,
        )
    };
    let mut nodes: Vec<Node> = Vec::with_capacity(usize::try_from(tags * 2 + 1).unwrap_or(0));
    let mut open: Vec<(usize, Name)> = Vec::new();
    let mut pos = 0usize;
    while pos < tpl.len() {
        let Some(i) = tpl[pos..].find("{{") else {
            nodes.push(Node::Text(pos as u32, tpl.len() as u32));
            break;
        };
        if i > 0 {
            nodes.push(Node::Text(pos as u32, (pos + i) as u32));
        }
        let tag = pos + i;
        let inner_start = tag + 2;
        let Some(j) = tpl[inner_start..].find("}}") else {
            return Err(syntax(cx, tag, "a tag is not closed"));
        };
        let inner = &tpl[inner_start..inner_start + j];
        pos = inner_start + j + 2;
        let trimmed = inner.trim();
        let lead = inner.len() - inner.trim_start().len();
        // The name written after the sigil, trimmed, with its range in the template.
        let name_of = |rest: &str, rest_off: usize| -> Option<Name> {
            let t = rest.trim();
            let skip = rest.len() - rest.trim_start().len();
            let start = inner_start + lead + rest_off + skip;
            valid_name(t).then_some(Name {
                start: start as u32,
                end: (start + t.len()) as u32,
            })
        };
        match trimmed.chars().next() {
            Some(c @ ('#' | '^')) => {
                let Some(name) = name_of(&trimmed[1..], 1) else {
                    return Err(syntax(cx, tag, "a section needs a name"));
                };
                if open.len() >= MAX_DEPTH {
                    return Err(syntax(cx, tag, "sections nested more than 128 deep"));
                }
                open.push((nodes.len(), name));
                nodes.push(Node::Open {
                    name,
                    inverted: c == '^',
                    close: 0,
                });
            }
            Some('/') => {
                let Some(name) = name_of(&trimmed[1..], 1) else {
                    return Err(syntax(cx, tag, "a closing tag needs a name"));
                };
                let Some((at, opened)) = open.pop() else {
                    return Err(syntax(cx, tag, "a section is closed that was not opened"));
                };
                let same = tpl[opened.start as usize..opened.end as usize]
                    == tpl[name.start as usize..name.end as usize];
                if !same {
                    return Err(syntax(cx, tag, "a closing tag does not match its section"));
                }
                let close = nodes.len() as u32;
                if let Node::Open { close: c, .. } = &mut nodes[at] {
                    *c = close;
                }
                nodes.push(Node::Close);
            }
            Some(c) if c == '.' || c == '_' || c.is_ascii_alphabetic() => {
                let Some(name) = name_of(trimmed, 0) else {
                    return Err(syntax(cx, tag, "not a name (`identifier`, `a.b` or `.`)"));
                };
                nodes.push(Node::Var(name));
            }
            _ => {
                return Err(syntax(
                    cx,
                    tag,
                    "an unsupported tag (only names and `#`, `^` and `/` sections)",
                ));
            }
        }
    }
    if let Some((_, name)) = open.last() {
        return Err(syntax(cx, name.start as usize, "a section is never closed"));
    }
    Ok((nodes, charge))
}

pub(super) fn names(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let t = arg(cx, args, 0)?;
    let tpl = text(cx, t)?;
    let (nodes, _charge) = parse(cx, tpl, t.labels)?;
    let _seen_charge = charged(
        cx,
        (nodes.len() as u64).saturating_mul(cost::SLOT + cost::STR),
    )?;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for node in &nodes {
        let name = match node {
            Node::Var(n) | Node::Open { name: n, .. } => &tpl[n.start as usize..n.end as usize],
            _ => continue,
        };
        if name != "." && seen.insert(name) {
            out.push(new_text(cx, name.to_owned(), t.labels)?);
        }
    }
    new_list(cx, out, t.labels)
}

// ---- rendering ----

/// An open section: where it is, and what it is iterating.
#[derive(Debug)]
struct Frame {
    open: usize,
    /// The elements still to render after the current one.
    items: Option<Cursor>,
    /// The own labels of the list being walked, added to every element.
    list_labels: Labels,
    /// Whether the section pushed a scope (an inverted one does not).
    pushed: bool,
}

#[derive(Debug)]
struct Render {
    tpl: Value,
    n_pre: u64,
    parsed: Option<(Vec<Node>, Charge)>,
    scopes: Vec<Value>,
    frames: Vec<Frame>,
    ip: usize,
    out: TextBuf,
    acc: Labels,
    work: u64,
    since_check: u64,
    /// Units the walk has paid for beyond the class's charge.
    extra: u64,
}

/// The value one step into `v` under `key`: a record's field or a map's `Str` key, with the
/// labels of what was consulted added to `acc` on a miss and to the value on a hit.
fn child(v: &Value, key: &str, acc: &mut Labels) -> Option<Value> {
    match &v.repr {
        Repr::Record(_) => {
            let found = v.field(key);
            if found.is_none() {
                *acc |= v.labels;
            }
            found
        }
        Repr::Map(m) => {
            if let Some((stored, x)) = m.get_entry(&Value::text(key)) {
                Some(x.clone().with_labels(v.labels | stored.labels))
            } else {
                // A miss depends on every key the probe was compared with.
                *acc |= v.quick();
                None
            }
        }
        _ => None,
    }
}

fn truthy(v: &Value) -> bool {
    match &v.repr {
        Repr::None | Repr::Unit => false,
        Repr::Bool(b) => *b,
        Repr::Str(s) => !s.as_str().is_empty(),
        Repr::List(l) => !l.is_empty(),
        Repr::Map(m) => !m.is_empty(),
        Repr::Json(j) => match &j.node {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Str(s) => !s.as_str().is_empty(),
            Json::Array(l) => !l.is_empty(),
            Json::Object(e) => !e.is_empty(),
            _ => true,
        },
        _ => true,
    }
}

impl Render {
    /// The value `name` stands for, or `None`; adds the labels of what was consulted.
    fn lookup(&mut self, name: &str) -> Option<Value> {
        if name == "." {
            let v = self.scopes.last().cloned();
            self.work += 1;
            return v;
        }
        let mut segs = name.split('.');
        let first = segs.next()?;
        let mut found = None;
        self.work += name.len() as u64 + 1;
        for scope in self.scopes.iter().rev() {
            self.work += 1;
            if let Some(v) = child(scope, first, &mut self.acc) {
                found = Some(v);
                break;
            }
        }
        let mut cur = found?;
        for seg in segs {
            cur = child(&cur, seg, &mut self.acc)?;
        }
        Some(cur)
    }

    fn name_text(&self, n: Name) -> String {
        self.tpl.as_text().map_or_else(String::new, |t| {
            t[n.start as usize..n.end as usize].to_owned()
        })
    }

    fn fail(&self, cx: &Cx<'_>, why: &str) -> RtError {
        raise(cx.p, Kind::Invalid, why, self.acc | self.tpl.labels, cx.loc)
    }
}

impl Task for Render {
    fn resume(&mut self, cx: &mut Cx<'_>, _input: Resume) -> Result<TaskStep, RtError> {
        if self.parsed.is_none() {
            let tpl = self.tpl.clone();
            self.parsed = Some(parse(cx, tpl.as_text().unwrap_or(""), tpl.labels)?);
        }
        loop {
            // Pay in advance for work past what the charge covers, in doubling instalments.
            let covered = self
                .n_pre
                .saturating_add(self.out.len() as u64)
                .saturating_add(SLACK)
                .saturating_add(self.extra);
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
            let before = self.work;
            let node = match self.parsed.as_ref().and_then(|(n, _)| n.get(self.ip)) {
                Some(n) => *n,
                None => break,
            };
            self.work += 1;
            match node {
                Node::Text(a, b) => {
                    let tpl = self.tpl.clone();
                    let t = tpl.as_text().unwrap_or("");
                    self.out.push_str(cx, &t[a as usize..b as usize])?;
                    self.ip += 1;
                }
                Node::Var(n) => {
                    let name = self.name_text(n);
                    let Some(v) = self.lookup(&name) else {
                        return Err(
                            self.fail(cx, &format!("template: the name `{name}` is missing"))
                        );
                    };
                    self.acc |= v.labels;
                    let shown = match &v.repr {
                        Repr::Str(s) => Some(s.as_str().to_owned()),
                        Repr::Int(_)
                        | Repr::Float(_)
                        | Repr::Bool(_)
                        | Repr::Duration(_)
                        | Repr::Money(_)
                        | Repr::Variant(..) => Some(crate::ops::show(cx.p, &v)),
                        Repr::Json(j) => match &j.node {
                            Json::Str(s) => Some(s.as_str().to_owned()),
                            Json::Int(_) | Json::Float(_) | Json::Bool(_) => {
                                Some(crate::ops::show(cx.p, &v))
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    let Some(shown) = shown else {
                        return Err(self.fail(
                            cx,
                            &format!(
                                "template: `{name}` is not text or a scalar and cannot fill a tag"
                            ),
                        ));
                    };
                    self.out.push_str(cx, &shown)?;
                    self.ip += 1;
                }
                Node::Open {
                    name,
                    inverted,
                    close,
                } => {
                    let text_name = self.name_text(name);
                    let v = self.lookup(&text_name);
                    if let Some(v) = &v {
                        // Whether the section runs depends on the value.
                        self.acc |= v.labels;
                    }
                    let falsy = v.as_ref().is_none_or(|v| !truthy(v));
                    let close = close as usize;
                    if inverted {
                        if falsy {
                            self.frames.push(Frame {
                                open: self.ip,
                                items: None,
                                list_labels: Labels::NONE,
                                pushed: false,
                            });
                            self.ip += 1;
                        } else {
                            self.ip = close + 1;
                        }
                    } else if falsy {
                        self.ip = close + 1;
                    } else if let Some(v) = v {
                        if let Repr::List(l) = &v.repr {
                            let mut items = l.cursor();
                            let list_labels = v.labels;
                            // Not empty (the list is truthy): the first element.
                            let first = items
                                .next()
                                .map_or_else(Value::unit, |x| x.with_labels(list_labels));
                            self.scopes.push(first);
                            self.frames.push(Frame {
                                open: self.ip,
                                items: Some(items),
                                list_labels,
                                pushed: true,
                            });
                        } else {
                            self.scopes.push(v);
                            self.frames.push(Frame {
                                open: self.ip,
                                items: None,
                                list_labels: Labels::NONE,
                                pushed: true,
                            });
                        }
                        self.ip += 1;
                    } else {
                        self.ip = close + 1;
                    }
                }
                Node::Close => {
                    let Some(frame) = self.frames.last_mut() else {
                        return Err(self.fail(cx, "template: a closing tag with no section"));
                    };
                    let next = frame.items.as_mut().and_then(Cursor::next);
                    if let Some(x) = next {
                        let x = x.with_labels(frame.list_labels);
                        let body = frame.open + 1;
                        if let Some(top) = self.scopes.last_mut() {
                            *top = x;
                        }
                        self.ip = body;
                    } else {
                        let pushed = frame.pushed;
                        self.frames.pop();
                        if pushed {
                            self.scopes.pop();
                        }
                        self.ip += 1;
                    }
                }
            }
            let done = self.work - before;
            work::add(done);
            self.since_check += done;
        }
        let out = std::mem::replace(&mut self.out, TextBuf::new(Labels::NONE));
        let v = out.finish(cx)?;
        Ok(TaskStep::Done(v.with_labels(self.acc | self.tpl.labels)))
    }
}

pub(super) fn render(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    let (tpl, vars) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    text(cx, tpl)?;
    Ok(Box::new(Render {
        tpl: tpl.clone(),
        n_pre: sz(args, 0).saturating_add(weight(args, 1)),
        parsed: None,
        scopes: vec![vars.clone()],
        frames: Vec::new(),
        ip: 0,
        out: TextBuf::new(tpl.labels),
        acc: Labels::NONE,
        work: 0,
        since_check: 0,
        extra: 0,
    }))
}
