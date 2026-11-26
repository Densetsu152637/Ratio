//! `path` (chapter 06, `data`): lexical operations on path strings.
//!
//! Pure text, on the program's own path values: nothing here reads or resolves anything on a
//! file system, follows a link or knows the working directory. Both `/` and `\` separate on
//! input; output uses `/`.
//!
//! A path is a **root** and **components**. The root is nothing (`a/b`), `/`, a drive (`C:`,
//! drive-relative, or `C:/`) or a UNC prefix (`//server/share/`, from `\\server\share`); the
//! components are the non-empty runs between separators (so repeated and trailing separators
//! vanish). Three or more leading separators are one, as in POSIX.
//!
//! * `normalize` drops `.` components and resolves `..` against the component before it.
//!   A `..` with nothing to resolve against **stays** in a relative path (`../a`) and is
//!   **dropped** in a rooted one (`/..` is `/`, `C:/../a` is `C:/a`): lexical resolution never
//!   climbs above a root, and never pretends a relative path stays inside its start. An empty
//!   relative result is `.`.
//! * `relative_to` normalizes both and gives the rest of `p` after `base`, `.` for the same
//!   path, and `none` unless the roots are equal (drive letters compare without case) and `base`
//!   is a prefix of `p`'s components. A rest that starts with `..` (`p` is `../x`, `base` is
//!   `.`) is not under `base`: `none`. It never yields a path that leaves `base`.
//! * `is_absolute` is true for a leading separator or a drive letter followed by a colon, the
//!   conservative reading: `C:foo`, which depends on the current directory of drive C, counts.
//! * An extension is what follows the last `.` of the name when something precedes the dot and
//!   something follows it: `a.tar.gz` has the extension `gz` and the stem `a.tar`; `.bashrc`,
//!   `a.` and `..` have none.
//!
//! All are `linear` in the characters read. The component list is charged to the heap before
//! it is allocated; the result is at most as long as the input (one character more for `.`),
//! so no refusal can happen after allocating.

use super::data3::{TextBuf, charged, text_value};
use super::{Args, arg, list, new_text, text};
use crate::error::RtError;
use crate::labels::Labels;
use crate::machine::Cx;
use crate::value::Value;
use crate::work;

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// A path split into its root and components.
struct Parts<'a> {
    /// `""`, `"/"`, `"C:"`, `"C:/"` or `"//server/share/"`.
    root: String,
    rooted: bool,
    comps: Vec<&'a str>,
}

/// Splits `p`; the component list is charged first (an upper bound: one per separator).
fn split<'a>(cx: &Cx<'_>, p: &'a str) -> Result<(Parts<'a>, crate::heap::Charge), RtError> {
    let seps = p.bytes().filter(|b| *b == b'/' || *b == b'\\').count() as u64;
    // The component list twice (split and normalized) and the text built from it.
    let charge = charged(
        cx,
        (p.len() as u64).saturating_add(48u64.saturating_mul(seps.saturating_add(1))),
    )?;
    work::add(p.len() as u64);
    let mut root = String::new();
    let mut rooted = false;
    let mut rest = p;
    let bytes = p.as_bytes();
    let starts_sep = |s: &str| s.chars().next().is_some_and(is_sep);
    if starts_sep(p) {
        rooted = true;
        let after = p.trim_start_matches(is_sep);
        let lead = p.len() - after.len();
        if lead == 2 && !after.is_empty() {
            // `\\server\share\...`
            let (server, tail) = after.split_once(is_sep).unwrap_or((after, ""));
            let tail = tail.trim_start_matches(is_sep);
            let (share, tail) = tail.split_once(is_sep).unwrap_or((tail, ""));
            root = format!("//{server}/{share}/");
            rest = tail;
        } else {
            root.push('/');
            rest = after;
        }
    } else if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        root.push(char::from(bytes[0]));
        root.push(':');
        rest = &p[2..];
        if starts_sep(rest) {
            root.push('/');
            rooted = true;
            rest = rest.trim_start_matches(is_sep);
        }
    }
    let comps: Vec<&str> = rest.split(is_sep).filter(|c| !c.is_empty()).collect();
    Ok((
        Parts {
            root,
            rooted,
            comps,
        },
        charge,
    ))
}

fn render(root: &str, comps: &[&str]) -> String {
    let mut out =
        String::with_capacity(root.len() + comps.iter().map(|c| c.len() + 1).sum::<usize>());
    out.push_str(root);
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(c);
    }
    out
}

/// Resolves `.` and `..` lexically; see the module documentation.
fn normalized<'a>(parts: &Parts<'a>) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::with_capacity(parts.comps.len());
    for c in &parts.comps {
        match *c {
            "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !parts.rooted {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Whether a component reads as a drive: a letter and a colon.
fn drive_like(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn stem_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() && !name.bytes().all(|b| b == b'.') => {
            (&name[..i], Some(&name[i + 1..]))
        }
        _ => (name, None),
    }
}

pub(super) fn join(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let parts = arg(cx, args, 0)?;
    let labels = parts.quick();
    let l = list(cx, parts)?;
    let mut out = TextBuf::new(labels);
    // Non-empty parts are joined by one separator, and every run of separators becomes one;
    // a leading run of exactly two before a name (a UNC prefix) is kept.
    let (mut started, mut pending, mut at_sep) = (false, false, false);
    let mut cur = l.cursor();
    while let Some(v) = cur.next() {
        let t = text(cx, &v)?;
        work::add(t.len() as u64 + 1);
        if t.is_empty() {
            continue;
        }
        let mut piece = t;
        if started {
            pending = true;
        } else {
            started = true;
            let rest = t.trim_start_matches(is_sep);
            let lead = t.len() - rest.len();
            if lead > 0 {
                let unc = lead == 2 && !rest.is_empty();
                out.push_str(cx, if unc { "//" } else { "/" })?;
                at_sep = true;
                piece = rest;
            }
        }
        for c in piece.chars() {
            if is_sep(c) {
                pending = true;
            } else {
                if pending && !at_sep {
                    out.push(cx, '/')?;
                }
                pending = false;
                at_sep = false;
                out.push(cx, c)?;
            }
        }
    }
    if pending && !at_sep {
        out.push(cx, '/')?;
    }
    out.finish(cx)
}

pub(super) fn parent(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let p = arg(cx, args, 0)?;
    let (parts, _charge) = split(cx, text(cx, p)?)?;
    Ok(match parts.comps.split_last() {
        None => Value::none().with_labels(p.labels),
        Some((_, rest)) => new_text(cx, render(&parts.root, rest), p.labels)?,
    })
}

pub(super) fn name(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let p = arg(cx, args, 0)?;
    let (parts, _charge) = split(cx, text(cx, p)?)?;
    new_text(
        cx,
        parts.comps.last().copied().unwrap_or("").to_owned(),
        p.labels,
    )
}

pub(super) fn stem(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let p = arg(cx, args, 0)?;
    let (parts, _charge) = split(cx, text(cx, p)?)?;
    let name = parts.comps.last().copied().unwrap_or("");
    new_text(cx, stem_ext(name).0.to_owned(), p.labels)
}

pub(super) fn ext(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let p = arg(cx, args, 0)?;
    let (parts, _charge) = split(cx, text(cx, p)?)?;
    let name = parts.comps.last().copied().unwrap_or("");
    Ok(match stem_ext(name).1 {
        Some(e) => new_text(cx, e.to_owned(), p.labels)?,
        None => Value::none().with_labels(p.labels),
    })
}

pub(super) fn normalize(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let p = arg(cx, args, 0)?;
    let (parts, _charge) = split(cx, text(cx, p)?)?;
    let comps = normalized(&parts);
    // A relative path whose first component reads as a drive (`C:`) would become a drive path
    // once the parts before it were resolved away: it keeps a `./` in front.
    let drive_first = parts.root.is_empty() && comps.first().is_some_and(|c| drive_like(c));
    let out = if drive_first {
        format!("./{}", render("", &comps))
    } else if comps.is_empty() && !parts.rooted {
        // Nothing left of a relative path: the current directory (of the drive, if any).
        format!("{}.", parts.root)
    } else {
        render(&parts.root, &comps)
    };
    new_text(cx, out, p.labels)
}

pub(super) fn is_absolute(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let p = arg(cx, args, 0)?;
    let t = text(cx, p)?;
    let b = t.as_bytes();
    let abs = t.chars().next().is_some_and(is_sep)
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':');
    work::add(1);
    Ok(Value::bool(abs).with_labels(p.labels))
}

fn same_root(a: &str, b: &str) -> bool {
    // A drive letter compares without case; a UNC server and share are compared as written.
    let drive = |r: &str| r.len() >= 2 && r.as_bytes()[1] == b':';
    if drive(a) && drive(b) {
        a[..1].eq_ignore_ascii_case(&b[..1]) && a[1..] == b[1..]
    } else {
        a == b
    }
}

pub(super) fn relative_to(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (p, base) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels: Labels = p.labels | base.labels;
    let (pp, _c1) = split(cx, text(cx, p)?)?;
    let (bp, _c2) = split(cx, text(cx, base)?)?;
    let none = || Value::none().with_labels(labels);
    if !same_root(&pp.root, &bp.root) {
        return Ok(none());
    }
    let (pc, bc) = (normalized(&pp), normalized(&bp));
    if pc.len() < bc.len() || pc[..bc.len()] != bc[..] {
        return Ok(none());
    }
    let rest = &pc[bc.len()..];
    if rest.first() == Some(&"..")
        || (pp.root.is_empty() && rest.first().is_some_and(|c| drive_like(c)))
    {
        // Only a relative `p` that climbs out of its start has `..` left: not under `base`.
        return Ok(none());
    }
    let out = if rest.is_empty() {
        ".".to_owned()
    } else {
        rest.join("/")
    };
    text_value(cx, out, labels)
}
