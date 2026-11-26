//! The front end of `json.validate`'s `pattern`: chapter 03's `@pattern` subset (the common
//! subset of ECMAScript and the `re` syntax) read as ECMAScript reads it, then handed to the
//! `re` engine ([`crate::re`]).
//!
//! `ratio_check::regex_problem` refuses everything outside the subset. Inside it, the two
//! syntaxes still differ in a few places, which are either rewritten to what ECMAScript means
//! or refused, never left to differ silently:
//!
//! - `.` is any character but `\n`, `\r`, U+2028 and U+2029 (the `re` engine's `.` excludes
//!   only `\n`);
//! - `\d`, `\w` are ASCII (the `re` engine's are Unicode), and `\s` is ECMAScript's white space
//!   (including U+FEFF, excluding U+0085);
//! - `\D`, `\W`, `\S` are rewritten outside classes and refused inside them;
//! - `\b` and `\B` are refused (the `re` engine's word characters are Unicode);
//! - a class that starts with `]` or `^]` is refused (`[]x]` and `[^]` mean different things);
//! - a character outside the basic plane counts as one character, as ECMAScript's `u` flag
//!   reads it (without the flag it would be two).

/// ECMAScript's `\s`, as the members of a class.
const SPACE: &str = "\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

/// The pattern as the `re` engine reads it, with the meaning ECMAScript gives it.
///
/// # Errors
///
/// Why the pattern is outside the subset or differs in meaning and is refused.
pub(super) fn to_re(pattern: &str) -> Result<String, String> {
    if let Some(why) = ratio_check::regex_problem(pattern) {
        return Err(why);
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut i = 0;
    let mut in_class = false;
    // Inside a class: the last item was a `\d`, `\w` or `\s`, and nothing came before the
    // first item.
    let mut after_perl = false;
    let mut first = false;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == '\\' {
            let Some(&e) = chars.get(i) else {
                return Err("a trailing `\\`".to_owned());
            };
            i += 1;
            let (outside, inside): (Option<String>, Option<&str>) = match e {
                'd' => (Some("[0-9]".to_owned()), Some("0-9")),
                'D' => (Some("[^0-9]".to_owned()), None),
                'w' => (Some("[A-Za-z0-9_]".to_owned()), Some("A-Za-z0-9_")),
                'W' => (Some("[^A-Za-z0-9_]".to_owned()), None),
                's' => (Some(format!("[{SPACE}]")), Some(SPACE)),
                'S' => (Some(format!("[^{SPACE}]")), None),
                'b' | 'B' => return Err("`\\b` and `\\B` are not supported".to_owned()),
                _ => {
                    first = false;
                    after_perl = false;
                    out.push('\\');
                    out.push(e);
                    continue;
                }
            };
            if in_class {
                first = false;
                after_perl = matches!(e, 'd' | 'w' | 's');
                match inside {
                    Some(s) => out.push_str(s),
                    None => return Err(format!("`\\{e}` inside a class is not supported")),
                }
            } else if let Some(s) = outside {
                out.push_str(&s);
            }
            continue;
        }
        if in_class {
            let was_first = std::mem::replace(&mut first, false);
            let was_perl = std::mem::replace(&mut after_perl, false);
            match c {
                ']' => {
                    in_class = false;
                    out.push(c);
                }
                // Literal in ECMAScript, set operations and nested classes in the `re` syntax.
                '&' | '~' | '[' => {
                    out.push('\\');
                    out.push(c);
                }
                '-' => {
                    let next = chars.get(i).copied();
                    let perl_next = next == Some('\\')
                        && chars
                            .get(i + 1)
                            .is_some_and(|n| matches!(n, 'd' | 'D' | 'w' | 'W' | 's' | 'S'));
                    if next == Some('-') {
                        return Err("`--` in a class is not supported".to_owned());
                    }
                    if (was_perl && next != Some(']')) || (perl_next && !was_first) {
                        return Err(
                            "a `-` next to `\\d`, `\\w` or `\\s` is not supported".to_owned()
                        );
                    }
                    out.push('-');
                }
                other => out.push(other),
            }
            continue;
        }
        match c {
            '[' => {
                in_class = true;
                first = true;
                out.push('[');
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    return Err("a class that starts with `]` is not supported".to_owned());
                }
            }
            '.' => out.push_str("[^\n\r\u{2028}\u{2029}]"),
            other => out.push(other),
        }
    }
    Ok(out)
}
