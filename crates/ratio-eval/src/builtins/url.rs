//! `url` (chapter 06, `data`): URLs.
//!
//! `url.parse` follows the **WHATWG URL Standard** (the parser `web_fetch` uses) for the URLs
//! it accepts, and is **strict**: it answers `none` for everything it is not certain to read the
//! way that parser does, so that a decision made on the parse (an allow list on the host, say)
//! is never made on something the fetching parser would read differently. A URL is refused
//! when it is longer than 8,192 bytes, holds a space, a control character or a backslash, has
//! no scheme, has a host with a character outside `[A-Za-z0-9._~-]` (this includes non-ASCII
//! hosts: IDNA processing is not implemented, so an international name must be given in its
//! `xn--` form), a port above 65,535, a second `@` in its user information, a special scheme
//! (`http`, `https`, `ws`, `wss`, `ftp`, `file`) without `//` after the colon, a `file` URL
//! with a port, an `http`-like URL with no host, or an IPv4 or IPv6 host that does not parse.
//!
//! What it accepts it normalizes as the standard does: the scheme and a registered host in
//! lower case; a host that ends in a number is an IPv4 address in any of its spellings
//! (`2130706433`, `0x7f.1`) and is written as four decimal parts; an IPv6 host is written in
//! its compressed lower-case form inside brackets; a port equal to the scheme's default is
//! left out; the path has its `.` and `..` segments resolved (also in their `%2e` forms), an
//! empty path of a special scheme becomes `/`, and characters outside the path, query or
//! fragment sets are percent-encoded as UTF-8 (an existing `%` is never touched). The user
//! information is validated and then dropped: a `Url` has no field for it.
//!
//! The `query` map is the decoded `application/x-www-form-urlencoded` pairs (`+` is a space,
//! invalid UTF-8 becomes U+FFFD, a pair with no `=` has the empty value, empty pairs are
//! skipped); when a key repeats **the last pair wins**. `fragment` is present (possibly empty)
//! if the URL has a `#`, `port` if it names a non-default port.
//!
//! `with_query` edits the original text and keeps everything but the replaced and added pairs
//! as written; it raises `Invalid` for a string `parse` would refuse. `encode` and `decode` are
//! component-level percent-coding (`encode` keeps only `A-Z a-z 0-9 - . _ ~`; `decode` leaves a
//! `%` that is not followed by two hex digits, and a `+`, as they are). All are `linear`; a
//! decoded or encoded text over 16 MiB is refused before it is allocated.
//!
//! Every field of a parse, the `host` and the edited text carry the labels of the URL text
//! (and, for `with_query`, of every key and value of the map: the output holds them all).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    reason = "port and address arithmetic on small ranges"
)]

use super::data3::{TextBuf, charged, layout, raise_invalid, record, text_value};
use super::{Args, Sizes, arg, map, new_text, sz, text, weight};
use crate::error::RtError;
use crate::heap::cost;
use crate::labels::Labels;
use crate::machine::Cx;
use crate::pmap::Map;
use crate::value::{MAX_STR_BYTES, Value};
use crate::work;
use std::collections::HashMap;
use std::fmt::Write as _;

/// The longest URL `parse` reads.
const MAX_URL: usize = 8192;

pub(super) fn s_with_query(a: &Args) -> Sizes {
    Sizes {
        n: sz(a, 0).saturating_add(weight(a, 1)),
        ..Sizes::default()
    }
}

// ---- percent coding ----

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes to bytes; `plus` reads `+` as a space (query strings).
fn pct_decode(s: &str, plus: bool) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len()
                && hex_val(b[i + 1]).is_some()
                && hex_val(b[i + 2]).is_some() =>
            {
                out.push(hex_val(b[i + 1]).unwrap_or(0) << 4 | hex_val(b[i + 2]).unwrap_or(0));
                i += 3;
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// A component's percent-encoding: everything but `A-Z a-z 0-9 - . _ ~`.
fn encode_component(s: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
}

/// Percent-encodes the bytes of `s` that `set` selects (and every non-ASCII byte), leaving an
/// existing `%` alone.
fn encode_with(s: &str, set: fn(u8) -> bool, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in s.as_bytes() {
        if b >= 0x80 || set(b) {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        } else {
            out.push(char::from(b));
        }
    }
}

fn path_set(b: u8) -> bool {
    matches!(b, b'"' | b'<' | b'>' | b'^' | b'`' | b'{' | b'}')
}

fn query_set(b: u8) -> bool {
    matches!(b, b'"' | b'<' | b'>')
}

fn special_query_set(b: u8) -> bool {
    matches!(b, b'"' | b'<' | b'>' | b'\'')
}

fn fragment_set(b: u8) -> bool {
    matches!(b, b'"' | b'<' | b'>' | b'`')
}

// ---- hosts ----

/// A WHATWG IPv4 number: decimal, `0x` hexadecimal or leading-zero octal.
fn ipv4_number(part: &str) -> Option<u64> {
    if part.is_empty() {
        return None;
    }
    let (digits, radix) =
        if let Some(h) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            (h, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (part, 10)
        };
    if digits.is_empty() {
        return Some(0);
    }
    let mut n: u64 = 0;
    for c in digits.chars() {
        let d = u64::from(c.to_digit(radix)?);
        // Saturate far above any valid value rather than overflow.
        n = n
            .saturating_mul(u64::from(radix))
            .saturating_add(d)
            .min(1 << 40);
    }
    Some(n)
}

/// What a host is as an IPv4 address.
enum Ipv4 {
    /// It does not end in a number: a domain.
    Domain,
    /// It ends in a number but is not a valid address.
    Invalid,
    /// The address in dotted decimal.
    Address(String),
}

fn ipv4_host(host: &str) -> Ipv4 {
    let mut parts: Vec<&str> = host.split('.').collect();
    if parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    let numeric = !last.is_empty()
        && (last.bytes().all(|b| b.is_ascii_digit())
            || last
                .strip_prefix("0x")
                .or_else(|| last.strip_prefix("0X"))
                .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit())));
    if !numeric {
        return Ipv4::Domain;
    }
    if parts.len() > 4 {
        return Ipv4::Invalid;
    }
    let mut numbers = Vec::with_capacity(4);
    for p in &parts {
        let Some(n) = ipv4_number(p) else {
            return Ipv4::Invalid;
        };
        numbers.push(n);
    }
    let Some((&tail, head)) = numbers.split_last() else {
        return Ipv4::Invalid;
    };
    if head.iter().any(|n| *n > 255) {
        return Ipv4::Invalid;
    }
    if tail >= 256u64.pow(5 - numbers.len() as u32) {
        return Ipv4::Invalid;
    }
    let mut addr = tail;
    for (i, n) in head.iter().enumerate() {
        addr += n * 256u64.pow(3 - i as u32);
    }
    Ipv4::Address(format!(
        "{}.{}.{}.{}",
        (addr >> 24) & 255,
        (addr >> 16) & 255,
        (addr >> 8) & 255,
        addr & 255
    ))
}

/// The WHATWG IPv6 parser; `s` is what is between the brackets.
fn ipv6_parse(s: &str) -> Option<[u16; 8]> {
    let b = s.as_bytes();
    let mut pieces = [0u16; 8];
    let mut index = 0usize;
    let mut compress: Option<usize> = None;
    let mut p = 0usize;
    if b.first() == Some(&b':') {
        if b.get(1) != Some(&b':') {
            return None;
        }
        p += 2;
        index += 1;
        compress = Some(index);
    }
    while p < b.len() {
        if index == 8 {
            return None;
        }
        if b[p] == b':' {
            if compress.is_some() {
                return None;
            }
            p += 1;
            index += 1;
            compress = Some(index);
            continue;
        }
        let (mut value, mut length) = (0u32, 0usize);
        while length < 4 && p < b.len() && hex_val(b[p]).is_some() {
            value = value * 16 + u32::from(hex_val(b[p])?);
            p += 1;
            length += 1;
        }
        if p < b.len() && b[p] == b'.' {
            if length == 0 {
                return None;
            }
            p -= length;
            if index > 6 {
                return None;
            }
            let mut seen = 0;
            while p < b.len() {
                let mut piece: Option<u32> = None;
                if seen > 0 {
                    if b[p] == b'.' && seen < 4 {
                        p += 1;
                    } else {
                        return None;
                    }
                }
                if p >= b.len() || !b[p].is_ascii_digit() {
                    return None;
                }
                while p < b.len() && b[p].is_ascii_digit() {
                    let d = u32::from(b[p] - b'0');
                    piece = match piece {
                        None => Some(d),
                        Some(0) => return None,
                        Some(v) => Some(v * 10 + d),
                    };
                    if piece > Some(255) {
                        return None;
                    }
                    p += 1;
                }
                pieces[index] = pieces[index] * 0x100 + piece? as u16;
                seen += 1;
                if seen == 2 || seen == 4 {
                    index += 1;
                }
            }
            if seen != 4 {
                return None;
            }
            break;
        } else if p < b.len() && b[p] == b':' {
            p += 1;
            if p >= b.len() {
                return None;
            }
        } else if p < b.len() {
            return None;
        }
        pieces[index] = value as u16;
        index += 1;
    }
    if let Some(c) = compress {
        let mut swaps = index - c;
        index = 7;
        while index != 0 && swaps > 0 {
            pieces.swap(index, c + swaps - 1);
            index -= 1;
            swaps -= 1;
        }
    } else if index != 8 {
        return None;
    }
    Some(pieces)
}

fn ipv6_text(pieces: &[u16; 8]) -> String {
    // The first longest run of zero pieces, if longer than one.
    let (mut best, mut best_len) = (None, 1);
    let mut i = 0;
    while i < 8 {
        if pieces[i] == 0 {
            let start = i;
            while i < 8 && pieces[i] == 0 {
                i += 1;
            }
            if i - start > best_len {
                best = Some(start);
                best_len = i - start;
            }
        } else {
            i += 1;
        }
    }
    let mut out = String::from("[");
    let mut ignore0 = false;
    for (i, piece) in pieces.iter().enumerate() {
        if ignore0 && *piece == 0 {
            continue;
        }
        ignore0 = false;
        if best == Some(i) {
            out.push_str(if i == 0 { "::" } else { ":" });
            ignore0 = true;
            continue;
        }
        let _ = write!(out, "{piece:x}");
        if i != 7 {
            out.push(':');
        }
    }
    out.push(']');
    out
}

/// A registered host in its ASCII lower-case form, or an IPv4 or IPv6 address; `None` if it is
/// not acceptable. `special` schemes parse IPv4; others keep the host as written.
fn parse_host(host: &str, special: bool) -> Option<String> {
    if let Some(inner) = host.strip_prefix('[') {
        return ipv6_parse(inner.strip_suffix(']')?).map(|p| ipv6_text(&p));
    }
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
    {
        return None;
    }
    if !special {
        return Some(host.to_owned());
    }
    let lower = host.to_ascii_lowercase();
    match ipv4_host(&lower) {
        Ipv4::Address(addr) => Some(addr),
        Ipv4::Invalid => None,
        Ipv4::Domain => Some(lower),
    }
}

// ---- parse ----

struct Parsed {
    scheme: String,
    host: String,
    port: Option<u16>,
    path: String,
    /// The query as written (percent-encoded), without the `?`.
    query: Option<String>,
    fragment: Option<String>,
}

fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn parse_url(s: &str) -> Option<Parsed> {
    if s.len() > MAX_URL || s.bytes().any(|b| b <= b' ' || b == 0x7f || b == b'\\') {
        return None;
    }
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    let mut sc = scheme.chars();
    if !sc.next()?.is_ascii_alphabetic()
        || !sc.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let special = is_special(&scheme);
    let rest = &s[colon + 1..];
    // Split off the fragment and the query (in that order: a `?` after a `#` is the fragment's).
    let (before_frag, fragment) = match rest.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (rest, None),
    };
    let (before_query, query) = match before_frag.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (before_frag, None),
    };
    let fragment = fragment.map(|f| {
        let mut o = String::new();
        encode_with(f, fragment_set, &mut o);
        o
    });
    let query = query.map(|q| {
        let mut o = String::new();
        encode_with(
            q,
            if special {
                special_query_set
            } else {
                query_set
            },
            &mut o,
        );
        o
    });
    if let Some(after) = before_query.strip_prefix("//") {
        let end = after.find('/').unwrap_or(after.len());
        let (authority, path) = after.split_at(end);
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        if let Some(u) = userinfo
            && !u.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'-' | b'.'
                            | b'_'
                            | b'~'
                            | b'!'
                            | b'$'
                            | b'&'
                            | b'\''
                            | b'('
                            | b')'
                            | b'*'
                            | b'+'
                            | b','
                            | b';'
                            | b'='
                            | b':'
                            | b'%'
                    )
            })
        {
            return None;
        }
        let (host_text, port_text) = if hostport.starts_with('[') {
            let close = hostport.find(']')?;
            let after_close = &hostport[close + 1..];
            match after_close.strip_prefix(':') {
                Some(p) => (&hostport[..=close], Some(p)),
                None if after_close.is_empty() => (hostport, None),
                None => return None,
            }
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (hostport, None),
            }
        };
        let host = if host_text.is_empty() {
            // Only `file` and the non-special schemes have an empty host.
            if special && scheme != "file" {
                return None;
            }
            String::new()
        } else {
            parse_host(host_text, special)?
        };
        let port = match port_text {
            None | Some("") => None,
            Some(p) => {
                if !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 10 {
                    return None;
                }
                let n: u64 = p.parse().ok()?;
                let n = u16::try_from(n).ok()?;
                (default_port(&scheme) != Some(n)).then_some(n)
            }
        };
        if scheme == "file" && port_text.is_some_and(|p| !p.is_empty()) {
            return None;
        }
        let path = if special || !path.is_empty() {
            normalize_path(path, special)
        } else {
            String::new()
        };
        return Some(Parsed {
            scheme,
            host,
            port,
            path,
            query,
            fragment,
        });
    }
    if special {
        return None;
    }
    // A non-special scheme with no authority: an opaque path (`mailto:a@b.c`) or a rooted one.
    let path = if before_query.starts_with('/') {
        normalize_path(before_query, false)
    } else {
        let mut o = String::new();
        encode_with(before_query, |_| false, &mut o);
        o
    };
    Some(Parsed {
        scheme,
        host: String::new(),
        port: None,
        path,
        query,
        fragment,
    })
}

/// The path of a hierarchical URL: segments with `.` and `..` resolved, each percent-encoded.
fn normalize_path(path: &str, special: bool) -> String {
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() && !special {
        return "/".to_owned();
    }
    let mut segments: Vec<String> = Vec::new();
    let all: Vec<&str> = path.split('/').collect();
    for (i, seg) in all.iter().enumerate() {
        let last = i + 1 == all.len();
        let lower = seg.to_ascii_lowercase();
        let dot = matches!(lower.as_str(), "." | "%2e");
        let dotdot = matches!(lower.as_str(), ".." | ".%2e" | "%2e." | "%2e%2e");
        if dotdot {
            segments.pop();
            if last {
                segments.push(String::new());
            }
        } else if dot {
            if last {
                segments.push(String::new());
            }
        } else {
            let mut o = String::new();
            encode_with(seg, path_set, &mut o);
            segments.push(o);
        }
    }
    format!("/{}", segments.join("/"))
}

/// The pairs of a query string: decoded, in order, empty pairs skipped.
fn query_pairs(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (
                String::from_utf8_lossy(&pct_decode(k, true)).into_owned(),
                String::from_utf8_lossy(&pct_decode(v, true)).into_owned(),
            )
        })
        .collect()
}

pub(super) fn parse(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    let labels = s.labels;
    work::add(t.len().min(MAX_URL + 1) as u64);
    let Some(u) = parse_url(t) else {
        return Ok(Value::none().with_labels(labels));
    };
    let txt = |cx: &Cx<'_>, x: String| new_text(cx, x, labels);
    // The query map: later pairs win.
    let mut q = Map::empty();
    if let Some(raw) = &u.query {
        for (k, v) in query_pairs(raw) {
            q = q
                .insert(cx.heap, txt(cx, k)?, txt(cx, v)?)
                .map_err(|f| cx.fault(f, labels))?;
        }
    }
    let query = Value::new_map(q).with_labels(labels);
    // Fields in name order; the optional ones only when present.
    let mut names: Vec<&str> = Vec::new();
    let mut items: Vec<Value> = Vec::new();
    if let Some(f) = u.fragment {
        names.push("fragment");
        items.push(txt(cx, f)?);
    }
    names.push("host");
    items.push(txt(cx, u.host)?);
    names.push("path");
    items.push(txt(cx, u.path)?);
    if let Some(p) = u.port {
        names.push("port");
        items.push(Value::int(i64::from(p)).with_labels(labels));
    }
    names.push("query");
    items.push(query);
    names.push("scheme");
    items.push(txt(cx, u.scheme)?);
    record(cx, &layout(&names), items, labels)
}

pub(super) fn host(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    work::add(t.len().min(MAX_URL + 1) as u64);
    match parse_url(t) {
        Some(u) if !u.host.is_empty() => new_text(cx, u.host, s.labels),
        _ => Ok(Value::none().with_labels(s.labels)),
    }
}

pub(super) fn encode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    // The size is known before anything is built: three bytes for each one that is encoded.
    let encoded = t
        .bytes()
        .filter(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')))
        .count();
    let out_len = encoded
        .checked_mul(2)
        .and_then(|e| e.checked_add(t.len()))
        .filter(|n| *n <= MAX_STR_BYTES)
        .ok_or_else(|| {
            raise_invalid(cx, "the encoded text would be longer than 16 MiB", s.labels)
        })?;
    let _charge = charged(cx, out_len as u64)?;
    let mut out = String::with_capacity(out_len);
    encode_component(t, &mut out);
    work::add(t.len() as u64);
    text_value(cx, out, s.labels)
}

pub(super) fn decode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    let _charge = charged(cx, t.len() as u64)?;
    work::add(t.len() as u64);
    match String::from_utf8(pct_decode(t, false)) {
        Ok(d) => text_value(cx, d, s.labels),
        Err(_) => Ok(Value::none().with_labels(s.labels)),
    }
}

pub(super) fn with_query(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (s, params) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels: Labels = s.labels | params.quick();
    let t = text(cx, s)?;
    work::add(t.len().min(MAX_URL + 1) as u64);
    if parse_url(t).is_none() {
        return Err(raise_invalid(cx, "not a URL", s.labels));
    }
    let pm = map(cx, params)?;
    // The text before the query, the query as written, and the fragment with its `#`.
    let (head, fragment) = match t.find('#') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t, ""),
    };
    let (base, old) = match head.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (head, None),
    };
    // The existing pairs as written, with their decoded keys; at most one per `&` in the URL.
    let existing: Vec<(String, &str)> = old
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter(|p| !p.is_empty())
        .map(|p| {
            let key = p.split_once('=').map_or(p, |(k, _)| k);
            (
                String::from_utf8_lossy(&pct_decode(key, true)).into_owned(),
                p,
            )
        })
        .collect();
    let _charge = charged(
        cx,
        (existing.len() as u64 + 1) * (cost::SLOT + cost::STR) * 2,
    )?;
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, (k, _)) in existing.iter().enumerate() {
        index.entry(k.as_str()).or_insert(i);
    }
    // Pass 1: which existing pairs a parameter replaces.
    let mut replacement: Vec<Option<String>> = vec![None; existing.len()];
    let mut cur = pm.cursor();
    while let Some((k, v)) = cur.next() {
        let (Some(k), Some(v)) = (k.as_text(), v.as_text()) else {
            return Err(raise_invalid(cx, "expected a map of text", labels));
        };
        work::add(1 + k.len() as u64);
        if let Some(&i) = index.get(k) {
            let mut pair = String::new();
            encode_component(k, &mut pair);
            pair.push('=');
            encode_component(v, &mut pair);
            replacement[i] = Some(pair);
        }
    }
    // Pass 2: write the text.
    let mut out = TextBuf::new(labels);
    out.push_str(cx, base)?;
    let mut sep = '?';
    for (i, (k, raw)) in existing.iter().enumerate() {
        // The first pair of a key stands for it; a later pair of a replaced key is dropped.
        let first = index.get(k.as_str()).copied().unwrap_or(i);
        let piece: &str = match &replacement[first] {
            Some(r) if i == first => r,
            Some(_) => continue,
            None => raw,
        };
        out.push(cx, sep)?;
        sep = '&';
        out.push_str(cx, piece)?;
    }
    let mut cur = pm.cursor();
    while let Some((k, v)) = cur.next() {
        let (Some(k), Some(v)) = (k.as_text(), v.as_text()) else {
            return Err(raise_invalid(cx, "expected a map of text", labels));
        };
        if index.contains_key(k) {
            continue;
        }
        let mut pair = String::new();
        encode_component(k, &mut pair);
        pair.push('=');
        encode_component(v, &mut pair);
        out.push(cx, sep)?;
        sep = '&';
        out.push_str(cx, &pair)?;
    }
    out.push_str(cx, fragment)?;
    out.finish(cx)
}
