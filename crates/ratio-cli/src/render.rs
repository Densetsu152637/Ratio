//! How diagnostics are printed: for a person, `file:line:col: severity CODE: message` and the
//! source line with carets under the span (as `ratio_syntax::Diagnostic::render` draws them);
//! for tools, one JSON object per line.

use std::fmt::Write as _;

use crate::check::Finding;

/// The diagnostic for a person.
pub fn human(f: &Finding) -> String {
    let mut out = String::new();
    match f.span {
        Some(s) => {
            let _ = write!(out, "{}:{}:{}: ", f.file, s.line, s.col);
        }
        None => {
            let _ = write!(out, "{}: ", f.file);
        }
    }
    let _ = writeln!(out, "{} {}: {}", f.severity.word(), f.code, f.message);
    if let (Some(span), Some(source)) = (f.span, &f.source) {
        out.push_str(&caret(span, source));
    }
    out
}

/// The source line of `span` and a caret line under it, with a line-number gutter.
fn caret(span: ratio::Span, source: &str) -> String {
    let mut start = span.start.min(source.len());
    while !source.is_char_boundary(start) {
        start -= 1;
    }
    let before = source.get(..start).unwrap_or("");
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let after = source.get(start..).unwrap_or("");
    let line_end = after.find('\n').map_or(source.len(), |i| start + i);
    let text = source.get(line_start..line_end).unwrap_or("");
    let text = text.strip_suffix('\r').unwrap_or(text);
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let col = usize::try_from(span.col.max(1)).unwrap_or(1);
    let pad: String = text
        .chars()
        .take(col - 1)
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let width = if span.end_line == span.line {
        usize::try_from(span.end_col.saturating_sub(span.col)).unwrap_or(1)
    } else {
        text.chars().count().saturating_sub(col - 1)
    };
    let line_no = span.line.max(1).to_string();
    let gutter = " ".repeat(line_no.len());
    format!(
        "{line_no} | {text}\n{gutter} | {pad}{}\n",
        "^".repeat(width.max(1))
    )
}

/// The diagnostic as one line of JSON: `file`, `line`, `column`, `end_line`, `end_column`
/// (absent without a position), `severity`, `code` and `message`.
pub fn json(f: &Finding) -> String {
    let mut out = String::from("{");
    let _ = write!(out, "\"file\":{}", string(&f.file));
    if let Some(s) = f.span {
        let _ = write!(
            out,
            ",\"line\":{},\"column\":{},\"end_line\":{},\"end_column\":{}",
            s.line, s.col, s.end_line, s.end_col
        );
    }
    let _ = write!(
        out,
        ",\"severity\":{},\"code\":{},\"message\":{}}}",
        string(f.severity.word()),
        string(&f.code),
        string(&f.message)
    );
    out
}

/// A JSON string literal.
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Severity;

    fn finding(span: Option<ratio::Span>) -> Finding {
        Finding {
            file: "a/b.ratio".to_owned(),
            span,
            severity: Severity::Error,
            code: "C0004".to_owned(),
            message: "expected `Str`, found \"Int\"\n".to_owned(),
            source: Some("fn f() -> Str {\n  1\n}\n".to_owned()),
        }
    }

    fn span() -> ratio::Span {
        ratio::Span {
            start: 18,
            end: 19,
            line: 2,
            col: 3,
            end_line: 2,
            end_col: 4,
        }
    }

    #[test]
    fn a_person_sees_the_position_the_line_and_a_caret() {
        assert_eq!(
            human(&finding(Some(span()))),
            "a/b.ratio:2:3: error C0004: expected `Str`, found \"Int\"\n\n2 |   1\n  |   ^\n"
        );
        assert_eq!(
            human(&finding(None)),
            "a/b.ratio: error C0004: expected `Str`, found \"Int\"\n\n"
        );
    }

    #[test]
    fn json_is_one_escaped_object() {
        assert_eq!(
            json(&finding(Some(span()))),
            "{\"file\":\"a/b.ratio\",\"line\":2,\"column\":3,\"end_line\":2,\"end_column\":4,\
             \"severity\":\"error\",\"code\":\"C0004\",\"message\":\"expected `Str`, found \
             \\\"Int\\\"\\n\"}"
        );
        assert_eq!(string("\u{1}\\"), "\"\\u0001\\\\\"");
    }
}
