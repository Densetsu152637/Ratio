//! A small regular-expression matcher for `json.validate`'s `pattern` and `patternProperties`.
//!
//! It reads the common subset of ECMAScript and RE2 syntax that chapter 03 allows in a schema's
//! `@pattern` (alternation, groups including `(?:…)`, classes with ranges, the quantifiers `*`,
//! `+`, `?`, `{m}`, `{m,}` and `{m,n}`, anchors, and the escapes `\d \w \s \D \W \S \b \B` and
//! escaped punctuation) and runs a Thompson simulation: time O(text · program), no
//! backtracking, so no pattern can hang the interpreter. Everything is bounded: a pattern of
//! at most 4 KiB, a program of at most 4,096 instructions, a nesting of at most 64. The `re`
//! module of stage 7 is the full engine; this one only has to decide whether a pattern matches
//! anywhere in a text.

const MAX_PATTERN_BYTES: usize = 4096;
const MAX_PROGRAM: usize = 4096;
const MAX_NESTING: usize = 64;
const MAX_REPEAT: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Assertion {
    Start,
    End,
    WordBoundary,
    NotWordBoundary,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Assert(Assertion),
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, Option<u32>),
}

#[derive(Clone, Debug)]
enum Inst {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Assert(Assertion),
    Split(usize, usize),
    Jump(usize),
    Match,
}

/// A compiled pattern.
#[derive(Debug)]
pub(super) struct Regex {
    prog: Vec<Inst>,
}

struct Parser {
    s: Vec<char>,
    i: usize,
}

type Parsed<T> = Result<T, String>;

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

const DIGITS: [(char, char); 1] = [('0', '9')];
const WORD: [(char, char); 4] = [('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
const SPACE: [(char, char); 4] = [
    (' ', ' '),
    ('\t', '\r'),
    ('\u{a0}', '\u{a0}'),
    ('\u{feff}', '\u{feff}'),
];

fn negate(ranges: &[(char, char)]) -> Vec<(char, char)> {
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable();
    let mut out = Vec::new();
    let mut next = 0u32;
    for (lo, hi) in sorted {
        if u32::from(lo) > next
            && let (Some(a), Some(b)) = (char::from_u32(next), char::from_u32(u32::from(lo) - 1))
        {
            out.push((a, b));
        }
        next = next.max(u32::from(hi) + 1);
    }
    if next <= 0x10_FFFF
        && let Some(a) = char::from_u32(next)
    {
        out.push((a, '\u{10FFFF}'));
    }
    out
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self, depth: usize) -> Parsed<Node> {
        if depth > MAX_NESTING {
            return Err("nested too deeply".to_owned());
        }
        let mut branches = vec![self.cat(depth)?];
        while self.eat('|') {
            branches.push(self.cat(depth)?);
        }
        Ok(if branches.len() == 1 {
            branches.remove(0)
        } else {
            Node::Alt(branches)
        })
    }

    fn cat(&mut self, depth: usize) -> Parsed<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom(depth)?;
            items.push(self.quantified(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.remove(0),
            _ => Node::Cat(items),
        })
    }

    fn number(&mut self) -> Parsed<Option<u32>> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return Ok(None);
        }
        let text: String = self.s[start..self.i].iter().collect();
        text.parse::<u32>()
            .map(Some)
            .map_err(|_| "a repeat count that is too large".to_owned())
    }

    fn quantified(&mut self, atom: Node) -> Parsed<Node> {
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                self.i += 1;
                let Some(min) = self.number()? else {
                    return Err("a `{` with no count".to_owned());
                };
                let max = if self.eat(',') {
                    self.number()?
                } else {
                    Some(min)
                };
                if self.peek() != Some('}') {
                    return Err("an unterminated `{`".to_owned());
                }
                if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT || m < min) {
                    return Err("a repeat count out of range".to_owned());
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        // A lazy marker changes which match is preferred, not whether there is one.
        self.eat('?');
        if matches!(atom, Node::Assert(_)) {
            return Err("a quantifier on an anchor".to_owned());
        }
        Ok(Node::Repeat(Box::new(atom), min, max))
    }

    fn escape_class(c: char) -> Option<(Vec<(char, char)>, bool)> {
        match c {
            'd' => Some((DIGITS.to_vec(), false)),
            'D' => Some((DIGITS.to_vec(), true)),
            'w' => Some((WORD.to_vec(), false)),
            'W' => Some((WORD.to_vec(), true)),
            's' => Some((SPACE.to_vec(), false)),
            'S' => Some((SPACE.to_vec(), true)),
            _ => None,
        }
    }

    fn escaped_char(c: char) -> Parsed<char> {
        match c {
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            c if c.is_ascii_punctuation() => Ok(c),
            _ => Err(format!("the escape `\\{c}` is not in the supported subset")),
        }
    }

    fn atom(&mut self, depth: usize) -> Parsed<Node> {
        let Some(c) = self.peek() else {
            return Err("unexpected end".to_owned());
        };
        self.i += 1;
        match c {
            '(' => {
                if self.eat('?') && !self.eat(':') {
                    return Err("only `(?:` groups are supported".to_owned());
                }
                let inner = self.alt(depth + 1)?;
                if !self.eat(')') {
                    return Err("an unclosed group".to_owned());
                }
                Ok(inner)
            }
            '[' => self.class(),
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Assert(Assertion::Start)),
            '$' => Ok(Node::Assert(Assertion::End)),
            '*' | '+' | '?' => Err("a quantifier with nothing to repeat".to_owned()),
            '{' | '}' | ']' => Ok(Node::Char(c)),
            '\\' => {
                let Some(e) = self.peek() else {
                    return Err("a trailing `\\`".to_owned());
                };
                self.i += 1;
                if let Some((ranges, neg)) = Self::escape_class(e) {
                    return Ok(Node::Class(ranges, neg));
                }
                match e {
                    'b' => Ok(Node::Assert(Assertion::WordBoundary)),
                    'B' => Ok(Node::Assert(Assertion::NotWordBoundary)),
                    other => Ok(Node::Char(Self::escaped_char(other)?)),
                }
            }
            other => Ok(Node::Char(other)),
        }
    }

    fn class(&mut self) -> Parsed<Node> {
        let negated = self.eat('^');
        let mut ranges: Vec<(char, char)> = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err("an unclosed `[`".to_owned());
            };
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let Some(e) = self.peek() else {
                    return Err("a trailing `\\`".to_owned());
                };
                self.i += 1;
                if let Some((set, neg)) = Self::escape_class(e) {
                    ranges.extend(if neg { negate(&set) } else { set });
                    continue;
                }
                Self::escaped_char(e)?
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|n| *n != ']') {
                self.i += 1;
                let Some(h) = self.peek() else {
                    return Err("an unclosed `[`".to_owned());
                };
                self.i += 1;
                let hi = if h == '\\' {
                    let Some(e) = self.peek() else {
                        return Err("a trailing `\\`".to_owned());
                    };
                    self.i += 1;
                    Self::escaped_char(e)?
                } else {
                    h
                };
                if hi < lo {
                    return Err("a class range out of order".to_owned());
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Node::Class(ranges, negated))
    }
}

struct Compiler {
    prog: Vec<Inst>,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> Parsed<usize> {
        if self.prog.len() >= MAX_PROGRAM {
            return Err("the pattern is too large".to_owned());
        }
        self.prog.push(i);
        Ok(self.prog.len() - 1)
    }

    fn compile(&mut self, n: &Node) -> Parsed<()> {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                self.emit(Inst::Char(*c))?;
            }
            Node::Any => {
                self.emit(Inst::Any)?;
            }
            Node::Class(r, neg) => {
                self.emit(Inst::Class(r.clone(), *neg))?;
            }
            Node::Assert(a) => {
                self.emit(Inst::Assert(*a))?;
            }
            Node::Cat(items) => {
                for x in items {
                    self.compile(x)?;
                }
            }
            Node::Alt(branches) => {
                let mut jumps = Vec::new();
                for (i, b) in branches.iter().enumerate() {
                    if i + 1 < branches.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.compile(b)?;
                        jumps.push(self.emit(Inst::Jump(0))?);
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(b)?;
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jump(end);
                }
            }
            Node::Repeat(inner, min, max) => {
                for _ in 0..*min {
                    self.compile(inner)?;
                }
                match max {
                    None => {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.compile(inner)?;
                        self.emit(Inst::Jump(split))?;
                        let end = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, end);
                    }
                    Some(max) => {
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            splits.push(self.emit(Inst::Split(0, 0))?);
                            self.compile(inner)?;
                        }
                        let end = self.prog.len();
                        for s in splits {
                            self.prog[s] = Inst::Split(s + 1, end);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Regex {
    /// Compiles `pattern`.
    ///
    /// # Errors
    ///
    /// What is wrong with the pattern, as text.
    pub(super) fn new(pattern: &str) -> Parsed<Regex> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err("the pattern is longer than 4 KiB".to_owned());
        }
        let mut p = Parser {
            s: pattern.chars().collect(),
            i: 0,
        };
        let ast = p.alt(0)?;
        if p.i < p.s.len() {
            return Err("an unbalanced `)`".to_owned());
        }
        let mut c = Compiler { prog: Vec::new() };
        c.compile(&ast)?;
        c.emit(Inst::Match)?;
        Ok(Regex { prog: c.prog })
    }

    /// The program's size, for the heap charge.
    pub(super) fn size(&self) -> usize {
        self.prog.len()
    }

    fn holds(a: Assertion, prev: Option<char>, next: Option<char>) -> bool {
        let before = prev.is_some_and(word);
        let after = next.is_some_and(word);
        match a {
            Assertion::Start => prev.is_none(),
            Assertion::End => next.is_none(),
            Assertion::WordBoundary => before != after,
            Assertion::NotWordBoundary => before == after,
        }
    }

    /// Adds the threads reachable from `pc` without consuming a character.
    #[allow(
        clippy::too_many_arguments,
        reason = "the simulation's state, passed down"
    )]
    fn follow(
        &self,
        pc: usize,
        list: &mut Vec<usize>,
        mark: &mut [u64],
        generation: u64,
        prev: Option<char>,
        next: Option<char>,
        stack: &mut Vec<usize>,
    ) {
        stack.push(pc);
        while let Some(pc) = stack.pop() {
            if mark[pc] == generation {
                continue;
            }
            mark[pc] = generation;
            match &self.prog[pc] {
                Inst::Jump(t) => stack.push(*t),
                Inst::Split(a, b) => {
                    stack.push(*b);
                    stack.push(*a);
                }
                Inst::Assert(a) => {
                    if Self::holds(*a, prev, next) {
                        stack.push(pc + 1);
                    }
                }
                _ => list.push(pc),
            }
        }
    }

    /// Whether the pattern matches anywhere in `text`. Returns the work done too (character
    /// steps times threads), for the counters.
    pub(super) fn is_match(&self, text: &str) -> (bool, u64) {
        let n = self.prog.len();
        let mut mark = vec![0u64; n];
        let mut generation = 0u64;
        let mut clist: Vec<usize> = Vec::with_capacity(n);
        let mut nlist: Vec<usize> = Vec::with_capacity(n);
        let mut stack = Vec::new();
        let mut work = 0u64;
        let mut chars = text.chars().peekable();
        let mut prev: Option<char> = None;
        loop {
            let cur = chars.peek().copied();
            // A thread may start at every position (an unanchored search).
            generation += 1;
            for &pc in &clist {
                mark[pc] = generation;
            }
            self.follow(0, &mut clist, &mut mark, generation, prev, cur, &mut stack);
            work += clist.len() as u64 + 1;
            if clist.iter().any(|pc| matches!(self.prog[*pc], Inst::Match)) {
                return (true, work);
            }
            let Some(c) = cur else {
                return (false, work);
            };
            chars.next();
            let after = chars.peek().copied();
            generation += 1;
            nlist.clear();
            for &pc in &clist {
                let ok = match &self.prog[pc] {
                    Inst::Char(x) => *x == c,
                    Inst::Any => c != '\n',
                    Inst::Class(ranges, neg) => {
                        ranges.iter().any(|(lo, hi)| *lo <= c && c <= *hi) != *neg
                    }
                    _ => false,
                };
                if ok {
                    self.follow(
                        pc + 1,
                        &mut nlist,
                        &mut mark,
                        generation,
                        Some(c),
                        after,
                        &mut stack,
                    );
                }
            }
            std::mem::swap(&mut clist, &mut nlist);
            prev = Some(c);
        }
    }
}
