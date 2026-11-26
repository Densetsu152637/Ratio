//! The pattern parser: Rust `regex` syntax, the part of it that is linear by construction.
//!
//! Supported: literals and escapes (`\n`, `\t`, `\x7F`, `\x{10FFFF}`, `\u{…}`, `\U{…}`,
//! escaped punctuation), `.`, classes (`[a-z]`, negation, nesting, `[:alpha:]`, `&&`, `--`,
//! `~~`), the Perl classes `\d \w \s` and their negations, the Unicode classes of
//! [`super::class::unicode_class`], the anchors `^ $ \A \z \b \B`, capturing, non-capturing
//! and named groups (`(?P<n>…)`, `(?<n>…)`), alternation, the repetitions `* + ? {n} {n,}
//! {n,m}` and their lazy forms, and the flags `i m s x U` (set with `(?flags)` or scoped with
//! `(?flags:…)`, cleared with `-`). Everything else is refused with a message, never matched
//! differently: backreferences, lookaround, other Unicode classes, byte mode (`(?-u)`), CRLF
//! mode (`R`), `\Z`, `\<`, `\>`, `\b{…}` and octal escapes.
//!
//! Groups are parsed with an explicit stack, so the parser's own recursion is bounded by the
//! nesting of classes, which shares the [`MAX_NEST`] limit with groups.

use super::class::{self, Perl, Set};

/// Groups and classes nested deeper than this are refused.
pub(crate) const MAX_NEST: usize = 64;

/// A repetition count above this is refused (the program size limit refuses most sooner).
pub(crate) const MAX_REPEAT: u32 = 1000;

/// A parsed pattern.
#[derive(Debug)]
pub(crate) enum Node {
    Empty,
    /// One character, or the case variants of one (under `i`).
    Chars(Vec<char>),
    Set(Set),
    /// `.`: any character, or any but `\n`.
    Dot {
        newline: bool,
    },
    Look(Look),
    Group {
        index: Option<u32>,
        node: Box<Node>,
    },
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
}

/// The zero-width assertions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// `\A`, and `^` without `m`.
    StartText,
    /// `\z`, and `$` without `m`.
    EndText,
    /// `^` with `m`.
    StartLine,
    /// `$` with `m`.
    EndLine,
    /// `\b`.
    Word,
    /// `\B`.
    NotWord,
}

/// A parsed pattern with its groups: `groups` capturing groups besides the whole match, and
/// the names of the named ones with their indexes.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub node: Node,
    pub groups: u32,
    pub names: Vec<(String, u32)>,
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "the flags i, m, s, x and U")]
struct Flags {
    i: bool,
    m: bool,
    s: bool,
    x: bool,
    swap: bool,
}

/// A group being parsed: its finished alternatives and the current one.
struct Frame {
    alts: Vec<Node>,
    concat: Vec<Node>,
    index: Option<u32>,
    /// The flags outside the group, restored when it closes.
    outer: Flags,
}

impl Frame {
    fn new(index: Option<u32>, outer: Flags) -> Frame {
        Frame {
            alts: Vec::new(),
            concat: Vec::new(),
            index,
            outer,
        }
    }

    fn finish(mut self) -> Node {
        let cur = concat(std::mem::take(&mut self.concat));
        if self.alts.is_empty() {
            cur
        } else {
            self.alts.push(cur);
            Node::Alt(self.alts)
        }
    }
}

/// A concatenation, without its parts that match nothing (`(?:)`), so no repetition copy
/// walks them again.
fn concat(mut v: Vec<Node>) -> Node {
    v.retain(|n| !is_nothing(n));
    match v.len() {
        0 => Node::Empty,
        1 => v.pop().unwrap_or(Node::Empty),
        _ => Node::Concat(v),
    }
}

/// One escape: a character, a set, or an assertion.
enum Esc {
    Char(char),
    Set(Set),
    Look(Look),
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    flags: Flags,
    groups: u32,
    names: Vec<(String, u32)>,
}

type R<T> = Result<T, String>;

/// Parses a pattern.
pub(crate) fn parse(pattern: &str) -> R<Parsed> {
    let mut p = Parser {
        chars: pattern.chars().collect(),
        pos: 0,
        flags: Flags::default(),
        groups: 0,
        names: Vec::new(),
    };
    let node = p.parse()?;
    Ok(Parsed {
        node,
        groups: p.groups,
        names: p.names,
    })
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.chars.get(self.pos + k).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn err<T>(&self, message: &str) -> R<T> {
        Err(format!("{message} (at character {})", self.pos))
    }

    /// Skips whitespace and `#` comments under `x`.
    fn skip_verbose(&mut self) {
        if !self.flags.x {
            return;
        }
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.pos += 1,
                Some('#') => {
                    while let Some(c) = self.next() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    fn parse(&mut self) -> R<Node> {
        let mut stack: Vec<Frame> = Vec::new();
        let mut cur = Frame::new(None, self.flags);
        loop {
            self.skip_verbose();
            let Some(c) = self.next() else { break };
            match c {
                '(' => {
                    if stack.len() + 1 > MAX_NEST {
                        return self.err("groups nested more than 64 deep");
                    }
                    if let Some(frame) = self.group_open()? {
                        stack.push(std::mem::replace(&mut cur, frame));
                    }
                }
                ')' => {
                    let Some(parent) = stack.pop() else {
                        return self.err("an unopened group `)`");
                    };
                    let done = std::mem::replace(&mut cur, parent);
                    let (index, outer) = (done.index, done.outer);
                    let node = done.finish();
                    self.flags = outer;
                    cur.concat.push(Node::Group {
                        index,
                        node: Box::new(node),
                    });
                }
                '|' => {
                    let alt = concat(std::mem::take(&mut cur.concat));
                    cur.alts.push(alt);
                }
                '*' => self.repeat(&mut cur, 0, None)?,
                '+' => self.repeat(&mut cur, 1, None)?,
                '?' => self.repeat(&mut cur, 0, Some(1))?,
                '{' => {
                    let (min, max) = self.counted()?;
                    self.repeat(&mut cur, min, max)?;
                }
                '[' => {
                    let set = self.class(stack.len() + 1)?;
                    let set = if self.flags.i { set.folded() } else { set };
                    cur.concat.push(Node::Set(set));
                }
                '.' => cur.concat.push(Node::Dot {
                    newline: self.flags.s,
                }),
                '^' => cur.concat.push(Node::Look(if self.flags.m {
                    Look::StartLine
                } else {
                    Look::StartText
                })),
                '$' => cur.concat.push(Node::Look(if self.flags.m {
                    Look::EndLine
                } else {
                    Look::EndText
                })),
                '\\' => match self.escape(false)? {
                    Esc::Char(c) => cur.concat.push(self.literal(c)),
                    Esc::Set(s) => {
                        let s = if self.flags.i { s.folded() } else { s };
                        cur.concat.push(Node::Set(s));
                    }
                    Esc::Look(l) => cur.concat.push(Node::Look(l)),
                },
                c => cur.concat.push(self.literal(c)),
            }
        }
        if !stack.is_empty() {
            return self.err("an unclosed group `(`");
        }
        Ok(cur.finish())
    }

    fn literal(&self, c: char) -> Node {
        if self.flags.i {
            Node::Chars(class::literal_variants(c))
        } else {
            Node::Chars(vec![c])
        }
    }

    /// After `(`: a new group frame, or `None` for a flag setting `(?flags)`.
    fn group_open(&mut self) -> R<Option<Frame>> {
        let outer = self.flags;
        if !self.eat('?') {
            self.groups += 1;
            return Ok(Some(Frame::new(Some(self.groups), outer)));
        }
        match self.peek() {
            Some('P') => {
                self.pos += 1;
                match self.next() {
                    Some('<') => self.named(outer).map(Some),
                    Some('=' | '>') => self.err("backreferences are not supported"),
                    _ => self.err("`(?P` must be followed by `<name>`"),
                }
            }
            Some('<') => {
                self.pos += 1;
                match self.peek() {
                    Some('=' | '!') => self.err("lookbehind assertions are not supported"),
                    _ => self.named(outer).map(Some),
                }
            }
            Some('=' | '!') => self.err("lookahead assertions are not supported"),
            Some('#') => self.err("comment groups `(?#…)` are not supported; use the `x` flag"),
            Some('>') => self.err("atomic groups are not supported"),
            _ => {
                let mut flags = self.flags;
                let mut negate = false;
                let mut any = false;
                loop {
                    let Some(c) = self.next() else {
                        return self.err("an unclosed flag group");
                    };
                    match c {
                        ':' => {
                            self.flags = flags;
                            return Ok(Some(Frame::new(None, outer)));
                        }
                        ')' => {
                            if !any {
                                return self.err("an empty flag group `(?)`");
                            }
                            self.flags = flags;
                            return Ok(None);
                        }
                        '-' if !negate => {
                            negate = true;
                            any = false;
                        }
                        'i' => (flags.i, any) = (!negate, true),
                        'm' => (flags.m, any) = (!negate, true),
                        's' => (flags.s, any) = (!negate, true),
                        'x' => (flags.x, any) = (!negate, true),
                        'U' => (flags.swap, any) = (!negate, true),
                        'u' if !negate => any = true,
                        'u' => return self.err("byte mode `(?-u)` is not supported"),
                        'R' => return self.err("CRLF mode `R` is not supported"),
                        _ => return self.err(&format!("an unknown flag `{c}`")),
                    }
                }
            }
        }
    }

    /// After `(?P<` or `(?<`: the name, `>`, and a capturing frame.
    fn named(&mut self, outer: Flags) -> R<Frame> {
        let start = self.pos;
        let mut name = String::new();
        loop {
            match self.next() {
                Some('>') => break,
                Some(c) if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']') => {
                    name.push(c);
                }
                _ => {
                    self.pos = start;
                    return self.err("a group name of letters, digits, `_`, `.`, `[` or `]`");
                }
            }
        }
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            return self.err("a group name must not be empty or start with a digit");
        }
        if self.names.iter().any(|(n, _)| *n == name) {
            return self.err(&format!("the group name `{name}` is used twice"));
        }
        self.groups += 1;
        self.names.push((name, self.groups));
        Ok(Frame::new(Some(self.groups), outer))
    }

    /// Applies a repetition to the last item of the current alternative.
    fn repeat(&mut self, cur: &mut Frame, min: u32, max: Option<u32>) -> R<()> {
        let mut greedy = !self.eat('?');
        if self.flags.swap {
            greedy = !greedy;
        }
        let Some(last) = cur.concat.pop() else {
            return self.err("a repetition operator without an expression");
        };
        if matches!(last, Node::Repeat { .. }) {
            return self.err("a repetition of a repetition; use a group");
        }
        // `e{0}` matches only the empty string, and a repetition of nothing is nothing: both
        // collapse here, so they cost no program and no compile time.
        if max == Some(0) {
            cur.concat.push(Node::Empty);
            return Ok(());
        }
        if is_nothing(&last) {
            cur.concat.push(last);
            return Ok(());
        }
        cur.concat.push(Node::Repeat {
            node: Box::new(last),
            min,
            max,
            greedy,
        });
        Ok(())
    }

    /// After `{`: `n}`, `n,}` or `n,m}`.
    fn counted(&mut self) -> R<(u32, Option<u32>)> {
        let min = self.number();
        let Some(min) = min else {
            return self.err("a counted repetition such as `{2,5}`");
        };
        let max = if self.eat(',') {
            self.number()
        } else {
            Some(min)
        };
        self.skip_verbose();
        if !self.eat('}') {
            return self.err("an unclosed counted repetition");
        }
        if let Some(m) = max
            && m < min
        {
            return self.err("a counted repetition whose maximum is below its minimum");
        }
        if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
            return self.err("a repetition count above 1000");
        }
        Ok((min, max))
    }

    fn number(&mut self) -> Option<u32> {
        self.skip_verbose();
        let mut n: Option<u32> = None;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            self.pos += 1;
            n = Some(
                n.unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(d)
                    .min(u32::MAX - 1),
            );
        }
        self.skip_verbose();
        n
    }

    /// After `\`.
    fn escape(&mut self, in_class: bool) -> R<Esc> {
        let Some(c) = self.next() else {
            return self.err("a pattern ending in `\\`");
        };
        let esc = match c {
            'a' => Esc::Char('\x07'),
            'f' => Esc::Char('\x0C'),
            't' => Esc::Char('\t'),
            'n' => Esc::Char('\n'),
            'r' => Esc::Char('\r'),
            'v' => Esc::Char('\x0B'),
            'x' => Esc::Char(self.hex(2)?),
            'u' => Esc::Char(self.hex(4)?),
            'U' => Esc::Char(self.hex(8)?),
            'd' => Esc::Set(Set::Perl(Perl::Digit)),
            'w' => Esc::Set(Set::Perl(Perl::Word)),
            's' => Esc::Set(Set::Perl(Perl::Space)),
            'D' => Esc::Set(Set::Not(Box::new(Set::Perl(Perl::Digit)))),
            'W' => Esc::Set(Set::Not(Box::new(Set::Perl(Perl::Word)))),
            'S' => Esc::Set(Set::Not(Box::new(Set::Perl(Perl::Space)))),
            'p' | 'P' => Esc::Set(self.unicode(c == 'P')?),
            'A' if !in_class => Esc::Look(Look::StartText),
            'z' if !in_class => Esc::Look(Look::EndText),
            'b' if !in_class => {
                if self.peek() == Some('{') {
                    return self.err("word-boundary forms `\\b{…}` are not supported");
                }
                Esc::Look(Look::Word)
            }
            'B' if !in_class => Esc::Look(Look::NotWord),
            '0'..='9' => {
                return self.err("backreferences and octal escapes are not supported");
            }
            'k' => return self.err("backreferences are not supported"),
            'Z' => return self.err("`\\Z` is not supported; use `\\z`"),
            '<' | '>' => {
                return self.err("the word-start and word-end assertions are not supported");
            }
            c if c.is_ascii_punctuation() || c == ' ' => Esc::Char(c),
            c => return self.err(&format!("an unknown escape `\\{c}`")),
        };
        Ok(esc)
    }

    /// `\xHH`, `\x{…}` and the `\u`/`\U` forms: `fixed` digits, or any number in braces.
    fn hex(&mut self, fixed: usize) -> R<char> {
        let mut digits = String::new();
        if self.eat('{') {
            loop {
                match self.next() {
                    Some('}') => break,
                    Some(c) if c.is_ascii_hexdigit() && digits.len() < 8 => digits.push(c),
                    _ => return self.err("a bad hexadecimal escape"),
                }
            }
        } else {
            for _ in 0..fixed {
                match self.next() {
                    Some(c) if c.is_ascii_hexdigit() => digits.push(c),
                    _ => return self.err("a bad hexadecimal escape"),
                }
            }
        }
        u32::from_str_radix(&digits, 16)
            .ok()
            .and_then(char::from_u32)
            .map_or_else(
                || self.err("a hexadecimal escape that is not a character"),
                Ok,
            )
    }

    /// After `\p` or `\P`.
    fn unicode(&mut self, negated: bool) -> R<Set> {
        let mut negated = negated;
        let name = if self.eat('{') {
            if self.eat('^') {
                negated = !negated;
            }
            let mut name = String::new();
            loop {
                match self.next() {
                    Some('}') => break,
                    Some(c) if name.len() < 64 => name.push(c),
                    _ => return self.err("an unclosed Unicode class"),
                }
            }
            name
        } else {
            match self.next() {
                Some(c) => c.to_string(),
                None => return self.err("a Unicode class name after `\\p`"),
            }
        };
        let Some(set) = class::unicode_class(&name) else {
            return self.err(&format!(
                "the Unicode class `{name}` is not supported (supported: {})",
                class::UNICODE_CLASSES
            ));
        };
        Ok(if negated {
            Set::Not(Box::new(set))
        } else {
            set
        })
    }

    /// After `[`: a class, nested `depth` deep (groups and classes together).
    fn class(&mut self, depth: usize) -> R<Set> {
        if depth > MAX_NEST {
            return self.err("classes nested more than 64 deep");
        }
        let negated = self.eat('^');
        let mut left: Option<Set> = None;
        let mut op: Option<char> = None;
        let mut items: Vec<Set> = Vec::new();
        let mut ranges: Vec<(char, char)> = Vec::new();
        let mut first = true;
        loop {
            if self.flags.x {
                while self.peek().is_some_and(char::is_whitespace) {
                    self.pos += 1;
                }
            }
            let Some(c) = self.next() else {
                return self.err("an unclosed class `[`");
            };
            if c == ']' && !first {
                break;
            }
            first = false;
            let pair = (c, self.peek());
            match pair {
                ('[', Some(':')) => {
                    let set = self.ascii_class()?;
                    items.push(set);
                }
                ('[', _) => {
                    let inner = self.class(depth + 1)?;
                    items.push(inner);
                }
                ('&', Some('&')) | ('-', Some('-')) | ('~', Some('~')) => {
                    self.pos += 1;
                    let operand =
                        class_union(std::mem::take(&mut items), std::mem::take(&mut ranges));
                    let Some(operand) = operand else {
                        return self.err("a class operation without a left operand");
                    };
                    left = Some(match (left.take(), op) {
                        (Some(l), Some(o)) => combine(l, o, operand),
                        _ => operand,
                    });
                    op = Some(c);
                }
                ('\\', _) => match self.escape(true)? {
                    Esc::Char(lo) => self.class_range(lo, &mut ranges)?,
                    Esc::Set(s) => {
                        if self.peek() == Some('-')
                            && !matches!(self.peek_at(1), Some(']' | '-') | None)
                        {
                            return self.err("a class range starting with a class");
                        }
                        items.push(s);
                    }
                    Esc::Look(_) => return self.err("an assertion inside a class"),
                },
                (lo, _) => self.class_range(lo, &mut ranges)?,
            }
        }
        let last = class_union(items, ranges);
        let set = match (left, op) {
            (Some(l), Some(o)) => {
                let Some(r) = last else {
                    return self.err("a class operation without a right operand");
                };
                combine(l, o, r)
            }
            _ => match last {
                Some(s) => s,
                None => return self.err("an empty class"),
            },
        };
        Ok(if negated {
            Set::Not(Box::new(set))
        } else {
            set
        })
    }

    /// A literal class member `lo`, or the range `lo-hi` when a `-` and an end follow.
    fn class_range(&mut self, lo: char, ranges: &mut Vec<(char, char)>) -> R<()> {
        if self.peek() == Some('-') && !matches!(self.peek_at(1), Some(']' | '-') | None) {
            self.pos += 1;
            let hi = match self.next() {
                Some('\\') => match self.escape(true)? {
                    Esc::Char(c) => c,
                    _ => return self.err("a class range ending in a class"),
                },
                Some('[') => return self.err("a class range ending in a class"),
                Some(c) => c,
                None => return self.err("an unclosed class `[`"),
            };
            if hi < lo {
                return self.err("a class range whose end is before its start");
            }
            ranges.push((lo, hi));
        } else {
            ranges.push((lo, lo));
        }
        Ok(())
    }

    /// After `[:`: `name:]` or `^name:]`.
    fn ascii_class(&mut self) -> R<Set> {
        self.pos += 1;
        let negated = self.eat('^');
        let mut name = String::new();
        loop {
            match self.next() {
                Some(':') if self.eat(']') => break,
                Some(c) if c.is_ascii_alphabetic() && name.len() < 16 => name.push(c),
                _ => return self.err("an unclosed ASCII class `[:name:]`"),
            }
        }
        let Some(ranges) = class::ascii_class(&name) else {
            return self.err(&format!("an unknown ASCII class `[:{name}:]`"));
        };
        let set = Set::Ranges(ranges);
        Ok(if negated {
            Set::Not(Box::new(set))
        } else {
            set
        })
    }
}

/// Whether a node matches only the empty string and records nothing.
fn is_nothing(node: &Node) -> bool {
    match node {
        Node::Empty => true,
        Node::Group { index: None, node } => is_nothing(node),
        Node::Concat(v) => v.iter().all(is_nothing),
        _ => false,
    }
}

fn class_union(mut items: Vec<Set>, ranges: Vec<(char, char)>) -> Option<Set> {
    if !ranges.is_empty() {
        items.push(Set::Ranges(ranges));
    }
    match items.len() {
        0 => None,
        1 => items.pop(),
        _ => Some(Set::Union(items)),
    }
}

fn combine(l: Set, op: char, r: Set) -> Set {
    let (l, r) = (Box::new(l), Box::new(r));
    match op {
        '&' => Set::And(l, r),
        '-' => Set::Minus(l, r),
        _ => Set::Xor(l, r),
    }
}
