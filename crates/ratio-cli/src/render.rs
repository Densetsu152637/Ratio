//! How diagnostics are printed: for a person, `file:line:col: severity CODE: message` and the
//! source line with carets under the span (as `ratio_syntax::Diagnostic::render` draws them);
//! for tools, one JSON object per line.
//!
//! Everything a person reads comes from the package (file names, messages quoting source,
//! source lines), so it passes through [`clean`] first: a terminal control sequence (OSC 52
//! writing the clipboard, a screen clear, a hyperlink) or a bidirectional override (Trojan
//! Source) is shown as an escape, never sent to the terminal. JSON escapes its own.

use std::fmt::Write as _;

use crate::check::Finding;

/// The diagnostic for a person.
pub fn human(f: &Finding) -> String {
    let mut out = String::new();
    let file = clean(&f.file);
    match f.span {
        Some(s) => {
            let _ = write!(out, "{file}:{}:{}: ", s.line, s.col);
        }
        None => {
            let _ = write!(out, "{file}: ");
        }
    }
    let _ = writeln!(
        out,
        "{} {}: {}",
        f.severity.word(),
        clean(&f.code),
        clean(&f.message)
    );
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
    // The pad and the carets follow the cleaned line: an escaped character is as wide as its
    // escape.
    let mut pad = String::new();
    for c in text.chars().take(col - 1) {
        if c == '\t' {
            pad.push('\t');
        } else {
            pad.push_str(&" ".repeat(shown_width(c)));
        }
    }
    let count = if span.end_line == span.line {
        usize::try_from(span.end_col.saturating_sub(span.col)).unwrap_or(1)
    } else {
        text.chars().count().saturating_sub(col - 1)
    };
    let width: usize = text
        .chars()
        .skip(col - 1)
        .take(count)
        .map(shown_width)
        .sum::<usize>()
        + count.saturating_sub(text.chars().count().saturating_sub(col - 1));
    let line_no = span.line.max(1).to_string();
    let gutter = " ".repeat(line_no.len());
    format!(
        "{line_no} | {}\n{gutter} | {pad}{}\n",
        clean(text),
        "^".repeat(width.max(1))
    )
}

/// Whether `c` would control the terminal or reorder the text around it: a C0 control other
/// than tab, DEL, a C1 control, or a bidirectional embedding, override or isolate.
fn hostile(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{a}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// How many columns `c` takes once cleaned.
fn shown_width(c: char) -> usize {
    if hostile(c) {
        escape(c).chars().count()
    } else {
        1
    }
}

fn escape(c: char) -> String {
    format!("\\u{{{:x}}}", u32::from(c))
}

/// `s` for a terminal: each [`hostile`] character written as `\u{..}`.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if hostile(c) {
            out.push_str(&escape(c));
        } else {
            out.push(c);
        }
    }
    out
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
            "a/b.ratio:2:3: error C0004: expected `Str`, found \"Int\"\\u{a}\n2 |   1\n  |   ^\n"
        );
        assert_eq!(
            human(&finding(None)),
            "a/b.ratio: error C0004: expected `Str`, found \"Int\"\\u{a}\n"
        );
    }

    #[test]
    fn control_characters_are_shown_escaped_and_the_caret_follows() {
        let mut f = finding(Some(ratio::Span {
            start: 8,
            end: 9,
            line: 1,
            col: 9,
            end_line: 1,
            end_col: 10,
        }));
        f.file = "a\u{1b}]52;c;eA==\u{7}.ratio".to_owned();
        f.message = "bad \u{202e}name\u{9b}".to_owned();
        f.source = Some("// \u{1b}[2J x y\n".to_owned());
        let shown = human(&f);
        assert!(!shown.chars().any(|c| c != '\n' && hostile(c)), "{shown:?}");
        // The caret sits under `x`: 3 columns, the 6 of `\u{1b}`, then `[2J ` (4).
        assert_eq!(
            shown,
            format!(
                "a\\u{{1b}}]52;c;eA==\\u{{7}}.ratio:1:9: error C0004: bad \\u{{202e}}name\\u{{9b}}\n\
                 1 | // \\u{{1b}}[2J x y\n  | {}^\n",
                " ".repeat(13)
            )
        );
        assert_eq!(clean("tab\tok"), "tab\tok");
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
