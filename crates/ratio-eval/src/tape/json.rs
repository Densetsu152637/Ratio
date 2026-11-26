//! The small JSON the tape is written in: integers only (floats travel as bit patterns in
//! text), and a parser with fixed bounds on size, nesting and counts, so a crafted tape can
//! neither crash nor stall the replayer.

use std::fmt::Write as _;

/// A JSON value of the tape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

/// The parser's bounds.
pub(crate) const MAX_DEPTH: usize = 64;
/// Elements of one array or members of one object.
pub(crate) const MAX_ITEMS: usize = 1 << 20;

impl J {
    pub(crate) fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn int(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            J::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub(crate) fn uint(&self, key: &str) -> Option<u64> {
        self.int(key).and_then(|i| u64::try_from(i).ok())
    }

    pub(crate) fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => {
                let _ = write!(out, "{i}");
            }
            J::Str(s) => quote(s, out),
            J::Arr(xs) => {
                out.push('[');
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    x.write(out);
                }
                out.push(']');
            }
            J::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    quote(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub(crate) fn text(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parses one line. `Err` with a description for anything malformed or over a bound.
pub(crate) fn parse(text: &str) -> Result<J, String> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing characters at byte {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn lit(&mut self, word: &[u8], v: J) -> Result<J, String> {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(format!("unexpected character at byte {}", self.i))
        }
    }

    fn value(&mut self, depth: usize) -> Result<J, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".to_owned());
        }
        self.ws();
        match self.b.get(self.i) {
            None => Err("unexpected end".to_owned()),
            Some(b'n') => self.lit(b"null", J::Null),
            Some(b't') => self.lit(b"true", J::Bool(true)),
            Some(b'f') => self.lit(b"false", J::Bool(false)),
            Some(b'"') => self.string().map(J::Str),
            Some(b'[') => {
                self.i += 1;
                let mut xs = Vec::new();
                if self.eat(b']') {
                    return Ok(J::Arr(xs));
                }
                loop {
                    if xs.len() >= MAX_ITEMS {
                        return Err("an array with too many elements".to_owned());
                    }
                    xs.push(self.value(depth + 1)?);
                    if self.eat(b']') {
                        return Ok(J::Arr(xs));
                    }
                    if !self.eat(b',') {
                        return Err(format!("expected `,` at byte {}", self.i));
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                if self.eat(b'}') {
                    return Ok(J::Obj(m));
                }
                loop {
                    if m.len() >= MAX_ITEMS {
                        return Err("an object with too many members".to_owned());
                    }
                    self.ws();
                    let k = self.string()?;
                    if !self.eat(b':') {
                        return Err(format!("expected `:` at byte {}", self.i));
                    }
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    if self.eat(b'}') {
                        return Ok(J::Obj(m));
                    }
                    if !self.eat(b',') {
                        return Err(format!("expected `,` at byte {}", self.i));
                    }
                }
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.int(),
            Some(_) => Err(format!("unexpected character at byte {}", self.i)),
        }
    }

    fn int(&mut self) -> Result<J, String> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let digits = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
            if self.i - digits > 20 {
                return Err("a number too long".to_owned());
            }
        }
        if self.i == digits {
            return Err(format!("a malformed number at byte {start}"));
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .map(J::Int)
            .ok_or_else(|| format!("a number out of range at byte {start}"))
    }

    fn string(&mut self) -> Result<String, String> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(format!("expected a string at byte {}", self.i));
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return Err("an unterminated string".to_owned());
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    let e = *self.b.get(self.i + 1).ok_or("an unterminated escape")?;
                    self.i += 2;
                    out.push(match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let h = self
                                .b
                                .get(self.i..self.i + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .ok_or("a malformed \\u escape")?;
                            self.i += 4;
                            char::from_u32(h).ok_or("a surrogate in a \\u escape")?
                        }
                        _ => return Err("an unknown escape".to_owned()),
                    });
                }
                c if c < 0x20 => return Err("a control character in a string".to_owned()),
                _ => {
                    // Copy one UTF-8 character.
                    let len = match c {
                        0x00..=0x7F => 1,
                        0xC0..=0xDF => 2,
                        0xE0..=0xEF => 3,
                        _ => 4,
                    };
                    let s = self
                        .b
                        .get(self.i..self.i + len)
                        .and_then(|s| std::str::from_utf8(s).ok())
                        .ok_or("invalid UTF-8")?;
                    out.push_str(s);
                    self.i += len;
                }
            }
        }
    }
}
