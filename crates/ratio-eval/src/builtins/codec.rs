//! `codec` (chapter 06, `data`): Base64, hex and CSV.
//!
//! All are `linear`. An encoder knows its output size from its argument (`4 · ⌈n / 3⌉` bytes
//! for Base64, `2n` for hex) and refuses one over 16 MiB (`Invalid`) before allocating
//! anything; a decoder's output is at most its input. Text that is built goes through the
//! charged [`TextBuf`] or is charged to the heap before it is made.
//!
//! * **Base64** is the standard alphabet with `=` padding. Decoding accepts padded or unpadded
//!   input but nothing else: no whitespace, no URL-safe alphabet, no stray `=`, and no non-zero
//!   bits after the last full byte (a text that does not encode back to itself). Bytes that are
//!   not UTF-8 give `none`, as does any invalid input.
//! * **Hex** encodes lower-case; decoding accepts either case and an even number of digits.
//! * **CSV** is RFC 4180: records end at `\r\n` or `\n`, a field is quoted with `"` (and a quote
//!   inside it doubled) when it holds the delimiter, a quote or a line break, and a quoted
//!   field may span lines. The delimiter is one character other than `"`, `\r` and `\n`
//!   (`Invalid` otherwise). `csv_parse` is strict: a record with a different number of fields
//!   than the header (or than the first record, without a header), a repeated header name, an
//!   unterminated or misplaced quote and a lone `\r` raise `Invalid` naming the line the record
//!   starts on. An empty line is a record of one empty field. Without a header the keys are
//!   `c1`, `c2`, …. `csv_write` takes the header from the first row's keys (in key order),
//!   writes `\r\n` after each record, and raises `Invalid` naming the row when a later row lacks
//!   a header key; keys a later row has beyond the header are not written.
//!
//! Every result carries the labels of the text (and of the delimiter and the header flag, which
//! decide its shape); a CSV result's keys, from the header, carry the text's labels too.

#![allow(
    clippy::chunks_exact_to_as_chunks,
    reason = "chunks of a decoded slice, whose last chunk may be short"
)]

use super::data3::{TextBuf, cancelled, charged, raise_invalid, text_of, text_value};
use super::{Args, arg, list, new_list, new_text, opt, text};
use crate::error::RtError;
use crate::heap::{Charge, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::ops::cmp_natural;
use crate::pmap::Map;
use crate::value::{Fault, MAX_STR_BYTES, Value};
use crate::work;

/// The output size of an encoder, or the refusal when it passes 16 MiB.
fn encoded_len(
    cx: &Cx<'_>,
    len: usize,
    bytes_per_group: usize,
    group: usize,
    labels: Labels,
) -> Result<usize, RtError> {
    len.div_ceil(group)
        .checked_mul(bytes_per_group)
        .filter(|n| *n <= MAX_STR_BYTES)
        .ok_or_else(|| raise_invalid(cx, "the encoded text would be longer than 16 MiB", labels))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(super) fn base64_encode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let bytes = text(cx, s)?.as_bytes();
    let out_len = encoded_len(cx, bytes.len(), 4, 3, s.labels)?;
    let _charge = charged(cx, out_len as u64)?;
    let mut out = String::with_capacity(out_len);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for shift in [18, 12, 6, 0].iter().take(chunk.len() + 1) {
            out.push(char::from(B64[((n >> shift) & 63) as usize]));
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    work::add(bytes.len() as u64);
    text_value(cx, out, s.labels)
}

fn b64_value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => u32::from(c - b'A'),
        b'a'..=b'z' => u32::from(c - b'a') + 26,
        b'0'..=b'9' => u32::from(c - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// Decodes Base64 to bytes, or `None` when it is not valid (see the module documentation).
fn b64_decode(input: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let body = match input {
        [rest @ .., b'=', b'='] => {
            if !input.len().is_multiple_of(4) {
                return None;
            }
            rest
        }
        [rest @ .., b'='] => {
            if !input.len().is_multiple_of(4) {
                return None;
            }
            rest
        }
        _ => input,
    };
    if body.len() % 4 == 1 {
        return None;
    }
    for group in body.chunks(4) {
        let mut n = 0u32;
        for (i, c) in group.iter().enumerate() {
            n |= b64_value(*c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        let produced = group.len() * 6 / 8;
        // The bits past the last whole byte must be zero.
        let extra_bits = group.len() * 6 - produced * 8;
        if extra_bits > 0 && (n >> (24 - group.len() * 6)) & ((1 << extra_bits) - 1) != 0 {
            return None;
        }
        out.extend_from_slice(&bytes[1..=produced]);
    }
    Some(())
}

pub(super) fn base64_decode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let input = text(cx, s)?.as_bytes();
    // At most three bytes for every four characters.
    let cap = input.len() / 4 * 3 + 3;
    let _charge = charged(cx, cap as u64)?;
    let mut bytes = Vec::with_capacity(cap);
    work::add(input.len() as u64);
    let decoded = b64_decode(input, &mut bytes).and_then(|()| String::from_utf8(bytes).ok());
    match decoded {
        Some(t) => text_value(cx, t, s.labels),
        None => Ok(Value::none().with_labels(s.labels)),
    }
}

pub(super) fn hex_encode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let bytes = text(cx, s)?.as_bytes();
    let out_len = encoded_len(cx, bytes.len(), 2, 1, s.labels)?;
    let _charge = charged(cx, out_len as u64)?;
    work::add(bytes.len() as u64);
    text_value(cx, ratio_check::hex(bytes), s.labels)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub(super) fn hex_decode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let input = text(cx, s)?.as_bytes();
    let _charge = charged(cx, (input.len() / 2) as u64)?;
    work::add(input.len() as u64);
    let decoded = (input.len() % 2 == 0)
        .then(|| {
            input
                .chunks_exact(2)
                .map(|p| Some(hex_digit(p[0])? << 4 | hex_digit(p[1])?))
                .collect::<Option<Vec<u8>>>()
        })
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    match decoded {
        Some(t) => text_value(cx, t, s.labels),
        None => Ok(Value::none().with_labels(s.labels)),
    }
}

// ---- CSV ----

/// The delimiter: exactly one character, not a quote or a line break.
fn delimiter(cx: &Cx<'_>, v: Option<&Value>, labels: Labels) -> Result<char, RtError> {
    let Some(v) = v else {
        return Ok(',');
    };
    let t = text(cx, v)?;
    let mut chars = t.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c != '"' && c != '\r' && c != '\n' => Ok(c),
        _ => Err(raise_invalid(
            cx,
            "a CSV delimiter is one character, not a quote or a line break",
            labels | v.labels,
        )),
    }
}

/// A reader over the CSV text that counts lines.
struct Reader<'a> {
    s: &'a str,
    pos: usize,
    line: u64,
}

/// One field: a slice of the text, or built (a quoted field with doubled quotes).
enum Field<'a> {
    Slice(&'a str),
    Built(TextBuf),
}

/// What ended a field.
#[derive(PartialEq)]
enum End {
    Delimiter,
    Record,
    Eof,
}

impl<'a> Reader<'a> {
    fn peek(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.s.len()
    }

    /// Reads one field and the character that ended it; `Err(message)` for malformed text.
    fn field(
        &mut self,
        cx: &mut Cx<'_>,
        delim: char,
        labels: Labels,
    ) -> Result<(Field<'a>, End), RtError> {
        let fail = |cx: &Cx<'_>, line: u64, why: &str| {
            raise_invalid(cx, &format!("CSV line {line}: {why}"), labels)
        };
        if self.peek() == Some('"') {
            let start_line = self.line;
            self.pos += 1;
            let mut out = TextBuf::new(labels);
            loop {
                let rest = &self.s[self.pos..];
                let Some(q) = rest.find('"') else {
                    return Err(fail(cx, start_line, "a quoted field is not closed"));
                };
                out.push_str(cx, &rest[..q])?;
                self.line += rest[..q].bytes().filter(|b| *b == b'\n').count() as u64;
                self.pos += q + 1;
                if self.peek() == Some('"') {
                    out.push(cx, '"')?;
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let end = self
                .end(delim)
                .ok_or_else(|| fail(cx, self.line, "text after a closing quote"))?;
            return Ok((Field::Built(out), end));
        }
        // Unquoted: up to the delimiter or the end of the record.
        let rest = &self.s[self.pos..];
        let stop = rest
            .char_indices()
            .find(|(_, c)| *c == delim || *c == '\n' || *c == '\r' || *c == '"')
            .map_or(rest.len(), |(i, _)| i);
        let slice = &rest[..stop];
        work::add(stop as u64 + 1);
        self.pos += stop;
        match self.peek() {
            Some('"') => return Err(fail(cx, self.line, "a quote inside an unquoted field")),
            Some('\r') if !self.s[self.pos..].starts_with("\r\n") => {
                return Err(fail(cx, self.line, "a carriage return without a line feed"));
            }
            _ => {}
        }
        let end = self.end(delim).unwrap_or(End::Eof);
        Ok((Field::Slice(slice), end))
    }

    /// Consumes what ends a field, or `None` when something else follows.
    fn end(&mut self, delim: char) -> Option<End> {
        match self.peek() {
            None => Some(End::Eof),
            Some(c) if c == delim => {
                self.pos += c.len_utf8();
                Some(End::Delimiter)
            }
            Some('\n') => {
                self.pos += 1;
                self.line += 1;
                Some(End::Record)
            }
            Some('\r') if self.s[self.pos..].starts_with("\r\n") => {
                self.pos += 2;
                self.line += 1;
                Some(End::Record)
            }
            _ => None,
        }
    }
}

fn field_value(cx: &Cx<'_>, f: Field<'_>, labels: Labels) -> Result<Value, RtError> {
    match f {
        Field::Slice(t) => text_of(cx, t, labels),
        Field::Built(b) => b.finish(cx),
    }
}

pub(super) fn csv_parse(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let header_arg = opt(args, 1);
    let delim_arg = opt(args, 2);
    let labels = s.labels
        | header_arg.map_or(Labels::NONE, |h| h.labels)
        | delim_arg.map_or(Labels::NONE, |d| d.labels);
    let header = match header_arg {
        Some(h) => super::data3::bool_arg(cx, h)?,
        None => true,
    };
    let delim = delimiter(cx, delim_arg, labels)?;
    let mut r = Reader {
        s: text(cx, s)?,
        pos: 0,
        line: 1,
    };
    let mut keys: Option<Vec<Value>> = None;
    let mut rows: Vec<Value> = Vec::new();
    let mut rows_charge = Charge::none();
    let mut keys_charge = Charge::none();
    let mut order: Vec<usize> = Vec::new();
    let mut held: Vec<Charge> = Vec::new();
    while !r.at_end() {
        let start_line = r.line;
        let mut fields: Vec<Value> = Vec::new();
        let mut fields_charge = Charge::none();
        loop {
            let (f, end) = r.field(cx, delim, labels)?;
            fields_charge
                .grow(cx.heap, cost::SLOT)
                .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
            fields.push(field_value(cx, f, labels)?);
            if end != End::Delimiter {
                break;
            }
        }
        match &keys {
            None if header => {
                let _seen_charge = charged(cx, 16 * fields.len() as u64)?;
                let mut seen: Vec<&str> = fields.iter().filter_map(Value::as_text).collect();
                seen.sort_unstable();
                if seen.windows(2).any(|w| w[0] == w[1]) {
                    return Err(raise_invalid(
                        cx,
                        &format!("CSV line {start_line}: a header name is repeated"),
                        labels,
                    ));
                }
                keys_charge
                    .grow(cx.heap, cost::SLOT * fields.len() as u64)
                    .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
                keys = Some(fields);
                continue;
            }
            None => {
                let named: Result<Vec<Value>, RtError> = (1..=fields.len())
                    .map(|i| new_text(cx, format!("c{i}"), labels))
                    .collect();
                keys_charge
                    .grow(cx.heap, cost::SLOT * fields.len() as u64)
                    .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
                keys = Some(named?);
            }
            Some(_) => {}
        }
        let Some(ks) = &keys else { continue };
        if ks.len() != fields.len() {
            return Err(raise_invalid(
                cx,
                &format!(
                    "CSV line {start_line}: {} fields where {} were expected",
                    fields.len(),
                    ks.len()
                ),
                labels,
            ));
        }
        if cx.cancel.is_cancelled() {
            return Err(cancelled(cx));
        }
        if order.len() != ks.len() {
            // The header sorted once: every row is built from this order with no comparing.
            held.push(charged(
                cx,
                16 * ks.len() as u64 + 2 * cost::SLOT * ks.len() as u64,
            )?);
            order = (0..ks.len()).collect();
            let name = |i: usize| ks[i].as_text().unwrap_or("");
            order.sort_by(|a, b| {
                work::add(name(*a).len().min(name(*b).len()) as u64 + 1);
                name(*a).cmp(name(*b))
            });
        }
        work::add(ks.len() as u64);
        let entries: Vec<(Value, Value)> = order
            .iter()
            .map(|i| (ks[*i].clone(), fields[*i].clone()))
            .collect();
        let m = Map::from_sorted(cx.heap, entries.len(), &mut entries.into_iter())
            .map_err(|f| cx.fault(f, labels))?;
        rows_charge
            .grow(cx.heap, cost::SLOT)
            .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
        rows.push(Value::new_map(m).with_labels(labels));
    }
    new_list(cx, rows, labels)
}

/// Writes one CSV field, quoted when it needs to be.
fn write_field(cx: &mut Cx<'_>, out: &mut TextBuf, f: &str, delim: char) -> Result<(), RtError> {
    if f.contains([delim, '"', '\r', '\n']) {
        out.push(cx, '"')?;
        for part in f.split('"').enumerate() {
            if part.0 > 0 {
                out.push_str(cx, "\"\"")?;
            }
            out.push_str(cx, part.1)?;
        }
        out.push(cx, '"')
    } else {
        out.push_str(cx, f)
    }
}

pub(super) fn csv_write(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let rows = arg(cx, args, 0)?;
    let delim_arg = opt(args, 1);
    let labels = rows.quick() | delim_arg.map_or(Labels::NONE, |d| d.labels);
    let delim = delimiter(cx, delim_arg, labels)?;
    let l = list(cx, rows)?;
    let mut out = TextBuf::new(labels);
    let Some(first) = l.get(0) else {
        return out.finish(cx);
    };
    let crate::value::Repr::Map(first_map) = &first.repr else {
        return Err(raise_invalid(cx, "expected a list of maps", labels));
    };
    let _header_charge = charged(cx, cost::SLOT.saturating_mul(first_map.len()))?;
    let mut header: Vec<Value> = Vec::with_capacity(usize::try_from(first_map.len()).unwrap_or(0));
    let mut hc = first_map.cursor();
    while let Some((k, _)) = hc.next() {
        header.push(k);
    }
    // The header line.
    for (i, k) in header.iter().enumerate() {
        if i > 0 {
            out.push(cx, delim)?;
        }
        let Some(t) = k.as_text() else {
            return Err(raise_invalid(cx, "a CSV header is text", labels));
        };
        write_field(cx, &mut out, t, delim)?;
    }
    out.push_str(cx, "\r\n")?;
    let mut cur = l.cursor();
    let mut row_no = 0u64;
    while let Some(row) = cur.next() {
        let crate::value::Repr::Map(m) = &row.repr else {
            return Err(raise_invalid(cx, "expected a list of maps", labels));
        };
        // Both the header and the row are in key order: one merged walk finds every cell.
        let mut cells = m.cursor();
        let mut pending = cells.next();
        for (i, k) in header.iter().enumerate() {
            let mut found: Option<Value> = None;
            while let Some((rk, rv)) = pending.take() {
                work::add(rk.weight().min(k.weight()));
                match cmp_natural(&rk, k) {
                    std::cmp::Ordering::Less => pending = cells.next(),
                    std::cmp::Ordering::Equal => {
                        found = Some(rv);
                        pending = cells.next();
                        break;
                    }
                    std::cmp::Ordering::Greater => {
                        pending = Some((rk, rv));
                        break;
                    }
                }
            }
            let Some(v) = found else {
                return Err(raise_invalid(
                    cx,
                    &format!(
                        "CSV row {row_no} lacks the key `{}`",
                        k.as_text().unwrap_or("")
                    ),
                    labels,
                ));
            };
            let Some(t) = v.as_text() else {
                return Err(raise_invalid(cx, "a CSV cell is text", labels));
            };
            if i > 0 {
                out.push(cx, delim)?;
            }
            write_field(cx, &mut out, t, delim)?;
        }
        out.push_str(cx, "\r\n")?;
        row_no += 1;
    }
    out.finish(cx)
}
