//! A linear-time regular expression engine for the `re` built-ins (chapter 06, `data`): Rust
//! `regex` syntax ([`parse`]), compiled to a Thompson NFA ([`compile`]) and run by a Pike VM
//! ([`vm`]). Nothing backtracks, so no pattern can make a search take more than
//! O(|text| · program weight · (1 + slots)) time, and the program's weight is bounded
//! ([`compile::MAX_PROGRAM`], every repeated copy weighing at least 1, so compiling is bounded
//! too); patterns are at most [`MAX_PATTERN_BYTES`]. A search checks its evaluation's
//! cancellation every checkpoint's worth of work.

mod class;
mod compile;
mod parse;
mod vm;

use std::collections::HashMap;

pub(crate) use vm::{Stopped, UNSET, Vm, scratch_bytes};

/// The longest pattern accepted, in bytes.
pub(crate) const MAX_PATTERN_BYTES: usize = 4096;

/// A compiled pattern.
#[derive(Debug)]
pub(crate) struct Regex {
    pub program: compile::Program,
    /// The named groups and their indexes, in pattern order.
    pub names: Vec<(String, u32)>,
    /// The capturing groups, the whole match (group 0) included.
    pub groups: u32,
    /// The work compiling took: sixteen units per pattern character and the compile steps.
    pub compile_work: u64,
}

/// A refused pattern: why, and the work it took to find out (its characters and the compile
/// steps taken before the refusal).
#[derive(Clone, Debug)]
pub(crate) struct Refusal {
    pub message: String,
    pub work: u64,
}

impl Regex {
    /// Compiles a pattern, or says why it is refused.
    pub(crate) fn new(pattern: &str) -> Result<Regex, Refusal> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(Refusal {
                message: "the pattern is longer than 4 KiB".to_owned(),
                work: 0,
            });
        }
        // Parsing costs a few units per pattern character; compiling, its counted steps (one
        // per node compiled, every repeated copy included, and the weight emitted).
        let chars = 16 * pattern.chars().count() as u64;
        let parsed = parse::parse(pattern).map_err(|message| Refusal {
            message,
            work: chars,
        })?;
        let program = compile::compile(&parsed).map_err(|(message, steps)| Refusal {
            message,
            work: chars + steps,
        })?;
        Ok(Regex {
            compile_work: chars + program.steps,
            program,
            names: parsed.names,
            groups: parsed.groups + 1,
        })
    }

    /// The program's weight, the per-character factor of every charge.
    pub(crate) fn weight(&self) -> u64 {
        self.program.weight
    }

    /// The slots a search tracks: every group's, or just the whole match's.
    pub(crate) fn slots(&self, all: bool) -> usize {
        if all { self.program.slots as usize } else { 2 }
    }

    /// The instructions of the program.
    pub(crate) fn insts(&self) -> u64 {
        self.program.insts.len() as u64
    }

    /// A VM for this program tracking `ns` slots.
    pub(crate) fn vm(&self, ns: usize) -> Vm {
        Vm::new(&self.program, ns)
    }
}

/// The work a search over `n` characters is charged before running: one program's weight per
/// position, times one plus the slots it tracks (each thread carries them).
pub(crate) fn search_work(weight: u64, n: u64, ns: u64) -> u64 {
    n.saturating_add(1)
        .saturating_mul(weight)
        .saturating_mul(ns.saturating_add(1))
}

/// The successive non-overlapping matches of a pattern, as Rust's `regex` finds them: each
/// search starts where the last match ended, and an empty match that ends where the last
/// match ended is skipped.
pub(crate) struct Matches<'a> {
    re: &'a Regex,
    vm: Vm,
    hay: &'a str,
    start: usize,
    last_end: Option<usize>,
}

impl<'a> Matches<'a> {
    pub(crate) fn new(re: &'a Regex, hay: &'a str, ns: usize) -> Matches<'a> {
        Matches {
            re,
            vm: re.vm(ns.max(2)),
            hay,
            start: 0,
            last_end: None,
        }
    }

    /// The next match's slots (byte offsets), counting the work in `work`; `Err` when
    /// `cancelled` says to stop.
    pub(crate) fn next(
        &mut self,
        work: &mut u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Vec<usize>>, Stopped> {
        loop {
            if self.start > self.hay.len() {
                return Ok(None);
            }
            let Some(slots) =
                self.vm
                    .search(&self.re.program, self.hay, self.start, work, cancelled)?
            else {
                return Ok(None);
            };
            let (s, e) = (slots[0], slots[1]);
            if s == e && self.last_end == Some(e) {
                // An empty match touching the last one: search again one character on.
                let Some(c) = self.hay[e..].chars().next() else {
                    return Ok(None);
                };
                self.start = e + c.len_utf8();
                continue;
            }
            self.start = e;
            self.last_end = Some(e);
            return Ok(Some(slots));
        }
    }
}

/// One piece of a replacement template.
#[derive(Debug)]
enum Piece {
    /// Bytes of the template, by range.
    Text(usize, usize),
    Group(u32),
}

/// A replacement (`$1`, `${name}`, `$$`) resolved once per call against the pattern's groups,
/// as Rust's `regex` expands it: `$n` and `${n}` are group `n`, `$name` and `${name}` a named
/// group (`$name` takes the longest run of letters, digits and `_`), `$$` is `$`, a group that
/// does not exist is empty, and a `$` that starts none of these is itself.
#[derive(Debug)]
pub(crate) struct Template<'w> {
    with: &'w str,
    pieces: Vec<Piece>,
}

impl<'w> Template<'w> {
    /// The template, and the work resolving it took (its bytes and the group names).
    pub(crate) fn new(re: &Regex, with: &'w str) -> (Template<'w>, u64) {
        let names: HashMap<&str, u32> = re.names.iter().map(|(n, i)| (n.as_str(), *i)).collect();
        let lookup = |name: &str| -> Option<u32> {
            match name.parse::<u32>() {
                Ok(i) => (i < re.groups).then_some(i),
                Err(_) => names.get(name).copied(),
            }
        };
        let mut pieces = Vec::new();
        let mut at = 0;
        let b = with.as_bytes();
        let mut text_from = 0;
        while let Some(off) = with[at..].find('$') {
            let dollar = at + off;
            let rest = &with[dollar + 1..];
            let (group, skip): (Option<Option<u32>>, usize) = if rest.starts_with('$') {
                // `$$`: the text up to and including the first `$`.
                pieces.push(Piece::Text(text_from, dollar + 1));
                text_from = dollar + 2;
                at = dollar + 2;
                continue;
            } else if let Some(r) = rest.strip_prefix('{')
                && let Some(close) = r.find('}')
            {
                (Some(lookup(&r[..close])), close + 2)
            } else {
                let len = rest
                    .bytes()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                    .count();
                if len == 0 {
                    (None, 0)
                } else {
                    (Some(lookup(&rest[..len])), len)
                }
            };
            match group {
                None => at = dollar + 1,
                Some(g) => {
                    pieces.push(Piece::Text(text_from, dollar));
                    if let Some(g) = g {
                        pieces.push(Piece::Group(g));
                    }
                    at = dollar + 1 + skip;
                    text_from = at;
                }
            }
        }
        pieces.push(Piece::Text(text_from, b.len()));
        pieces.retain(|p| !matches!(p, Piece::Text(a, z) if a == z));
        let work = (with.len() + re.names.len()) as u64 + 1;
        (Template { with, pieces }, work)
    }

    /// Whether any piece refers to a group (so a search must track every group's slots).
    pub(crate) fn uses_groups(&self) -> bool {
        self.pieces
            .iter()
            .any(|p| matches!(p, Piece::Group(g) if *g > 0))
    }

    /// Appends the replacement for one match to `out`. Returns the bytes written, or `Err`
    /// with them when `out` would pass `limit` bytes (it stops there).
    pub(crate) fn expand(
        &self,
        hay: &str,
        slots: &[usize],
        out: &mut String,
        limit: usize,
    ) -> Result<u64, u64> {
        let start = out.len();
        for p in &self.pieces {
            let s = match p {
                Piece::Text(a, z) => &self.with[*a..*z],
                Piece::Group(g) => {
                    let (a, z) = (2 * *g as usize, 2 * *g as usize + 1);
                    match (slots.get(a), slots.get(z)) {
                        (Some(&s), Some(&e)) if s != UNSET && e != UNSET => &hay[s..e],
                        _ => "",
                    }
                }
            };
            if out.len() + s.len() > limit {
                return Err((out.len() - start) as u64);
            }
            out.push_str(s);
        }
        Ok((out.len() - start) as u64)
    }
}

/// The text as a literal pattern: every character with a meaning escaped.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, h: &str) -> Option<(usize, usize)> {
        let re = Regex::new(p).unwrap_or_else(|e| panic!("{p}: {}", e.message));
        let mut vm = re.vm(2);
        let mut work = 0;
        vm.search(&re.program, h, 0, &mut work, &|| false)
            .unwrap()
            .map(|s| (s[0], s[1]))
    }

    fn all(p: &str, h: &str) -> Vec<(usize, usize)> {
        let re = Regex::new(p).unwrap();
        let mut m = Matches::new(&re, h, 2);
        let mut out = Vec::new();
        let mut work = 0;
        while let Some(s) = m.next(&mut work, &|| false).unwrap() {
            out.push((s[0], s[1]));
        }
        out
    }

    fn groups(p: &str, h: &str) -> Vec<Option<String>> {
        let re = Regex::new(p).unwrap();
        let mut vm = re.vm(re.slots(true));
        let mut work = 0;
        let s = vm
            .search(&re.program, h, 0, &mut work, &|| false)
            .unwrap()
            .unwrap();
        (0..re.groups as usize)
            .map(|i| {
                (s[2 * i] != UNSET && s[2 * i + 1] != UNSET)
                    .then(|| h[s[2 * i]..s[2 * i + 1]].to_owned())
            })
            .collect()
    }

    fn refused(p: &str) -> String {
        Regex::new(p)
            .err()
            .map_or_else(|| panic!("`{p}` was accepted"), |e| e.message)
    }

    #[test]
    fn literals_classes_and_repetitions() {
        assert_eq!(find("abc", "xxabcxx"), Some((2, 5)));
        assert_eq!(find("a+", "baaab"), Some((1, 4)));
        assert_eq!(find("a+?", "baaab"), Some((1, 2)));
        assert_eq!(find("a{2,3}", "aaaa"), Some((0, 3)));
        assert_eq!(find("a{2,3}?", "aaaa"), Some((0, 2)));
        assert_eq!(find("a{2}", "a"), None);
        assert_eq!(find("a{2,}", "aaaaa"), Some((0, 5)));
        assert_eq!(find("[a-c]+", "xxcabz"), Some((2, 5)));
        assert_eq!(find("[^a-c]+", "abxyc"), Some((2, 4)));
        assert_eq!(find(r"\d+", "ab ٣٤5 c"), Some((3, 8)));
        assert_eq!(find(r"\w+", "  héllo_1 "), Some((2, 10)));
        assert_eq!(find(r"\s+", "a \t\nb"), Some((1, 4)));
        assert_eq!(find("[[:alpha:]]+", "12ab3"), Some((2, 4)));
        assert_eq!(find("[a-z&&[^aeiou]]+", "aebcd"), Some((2, 5)));
        assert_eq!(find("[a-z--b]+", "bacb"), Some((1, 3)));
        assert_eq!(find(r"\p{Lowercase}+", "ABcdE"), Some((2, 4)));
        assert_eq!(find(r"\P{Alphabetic}", "ab1"), Some((2, 3)));
        assert_eq!(find("a.c", "a\nc abc"), Some((4, 7)));
        assert_eq!(find("(?s)a.c", "a\nc"), Some((0, 3)));
        assert_eq!(find(r"\x41\u{42}\n", "AB\n"), Some((0, 3)));
        assert_eq!(find("[]a]+", "x]a]"), Some((1, 4)));
        assert_eq!(find("é", "café"), Some((3, 5)));
    }

    #[test]
    fn alternation_is_leftmost_first() {
        assert_eq!(find("a|ab", "ab"), Some((0, 1)));
        assert_eq!(find("ab|a", "ab"), Some((0, 2)));
        assert_eq!(find("b|a", "ab"), Some((0, 1)));
        assert_eq!(find("(a|ab)(c|bcd)", "abcd"), Some((0, 4)));
        assert_eq!(find("x*", "abc"), Some((0, 0)));
        assert_eq!(find("", "abc"), Some((0, 0)));
    }

    #[test]
    fn anchors_and_flags() {
        assert_eq!(find("^b", "ab"), None);
        assert_eq!(find("(?m)^b", "a\nb"), Some((2, 3)));
        assert_eq!(find("a$", "a\n"), None);
        assert_eq!(find("(?m)a$", "a\n"), Some((0, 1)));
        assert_eq!(find(r"\Aa", "ba"), None);
        assert_eq!(find(r"a\z", "ab a"), Some((3, 4)));
        assert_eq!(find(r"\bcat\b", "concat cat"), Some((7, 10)));
        assert_eq!(find(r"\Bcat", "cat concat"), Some((7, 10)));
        assert_eq!(find("(?i)ÉTÉ", "un été"), Some((3, 8)));
        assert_eq!(find("(?i)[a-c]+", "xBCa"), Some((1, 4)));
        assert_eq!(find("(?i:a)b", "Ab AB"), Some((0, 2)));
        assert_eq!(find("(?x) a b # comment\n c", "abc"), Some((0, 3)));
        assert_eq!(find("(?U)a+", "aaa"), Some((0, 1)));
        assert_eq!(find("(?U)a+?", "aaa"), Some((0, 3)));
        assert_eq!(find("(?i)a(?-i)b", "AB Ab"), Some((3, 5)));
    }

    #[test]
    fn groups_capture_their_last_iteration() {
        assert_eq!(
            groups(r"(\w+)@(?P<host>\w+)", "me@here"),
            [
                Some("me@here".into()),
                Some("me".into()),
                Some("here".into())
            ]
        );
        assert_eq!(
            groups("(a)|(b)", "b"),
            [Some("b".into()), None, Some("b".into())]
        );
        assert_eq!(
            groups("(?:(a)|b)+", "ab"),
            [Some("ab".into()), Some("a".into())]
        );
        assert_eq!(groups("(?<n>x)?y", "y"), [Some("y".into()), None]);
    }

    #[test]
    fn iteration_skips_empty_matches_touching_the_last() {
        assert_eq!(all("a*", "baaa"), [(0, 0), (1, 4)]);
        assert_eq!(all("x*", "ab"), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(all(",", "a,b,,c"), [(1, 2), (3, 4), (4, 5)]);
        assert_eq!(all("é*", "aé"), [(0, 0), (1, 3)]);
    }

    #[test]
    fn replacement_expansion() {
        let re = Regex::new(r"(?P<y>\d{4})-(\d{2})").unwrap();
        let hay = "on 2024-05";
        let mut vm = re.vm(re.slots(true));
        let mut work = 0;
        let s = vm
            .search(&re.program, hay, 0, &mut work, &|| false)
            .unwrap()
            .unwrap();
        let mut out = String::new();
        let (t, _) = Template::new(&re, "$2/${y} $$ $y1 $ ${9} $0 ${1 $1.5$10$01 $");
        assert!(t.uses_groups());
        t.expand(hay, &s, &mut out, usize::MAX).unwrap();
        let mut short = String::new();
        let (t0, _) = Template::new(&re, "$0$0$0");
        assert!(!t0.uses_groups());
        assert!(t0.expand(hay, &s, &mut short, 10).is_err());
        assert!(short.len() <= 10);
        assert_eq!(out, "05/2024 $  $  2024-05 ${1 2024.52024 $");
    }

    #[test]
    fn unsupported_syntax_is_refused_with_a_reason() {
        for (p, why) in [
            (r"(a)\1", "backreference"),
            ("(?=a)", "lookahead"),
            ("(?<=a)b", "lookbehind"),
            ("(?<!a)b", "lookbehind"),
            (r"\p{Greek}", "not supported"),
            (r"\pL", "not supported"),
            ("(?-u)a", "byte mode"),
            (r"\Z", r"\z"),
            ("(?R)a", "CRLF"),
            ("a**", "repetition of a repetition"),
            ("*a", "without an expression"),
            ("(a", "unclosed group"),
            ("a)", "unopened"),
            ("[a", "unclosed class"),
            ("a{2,1}", "maximum is below"),
            ("a{1001}", "above 1000"),
            ("a{x}", "counted repetition"),
            (r"\q", "unknown escape"),
            ("(?P<1a>x)", "start with a digit"),
            ("(?P<n>a)(?P<n>b)", "used twice"),
        ] {
            let e = refused(p);
            assert!(e.contains(why), "`{p}`: {e}");
        }
        let deep = format!("{}a{}", "(".repeat(65), ")".repeat(65));
        assert!(refused(&deep).contains("64 deep"));
        let deep_class = format!("{}a{}", "[".repeat(70), "]".repeat(70));
        assert!(refused(&deep_class).contains("64 deep"));
        assert!(refused(&"a".repeat(4097)).contains("4 KiB"));
    }

    #[test]
    fn the_program_size_is_bounded() {
        let e = refused("(a{1000}){1000}");
        assert!(e.contains("larger than the limit"), "{e}");
        assert!(refused("(?:[a-z]{1000}){100}").contains("larger than the limit"));
        assert!(refused(&r"\w{1000}".repeat(6)).contains("larger than the limit"));
        let ok = Regex::new("a{1000}").unwrap();
        assert!(ok.weight() <= compile::MAX_PROGRAM);
    }

    #[test]
    fn work_stays_within_the_search_charge() {
        let hay = "ab".repeat(500) + "c";
        for p in ["(a|b)*c", r"(\w+)\s", "x", "(?i)[a-z]+c$", "(.*a){5}"] {
            let re = Regex::new(p).unwrap();
            for ns in [0, 2, re.slots(true)] {
                let mut vm = re.vm(ns);
                let mut work = 0;
                vm.search(&re.program, &hay, 0, &mut work, &|| false)
                    .unwrap();
                let bound = search_work(re.weight(), hay.chars().count() as u64, ns as u64);
                assert!(work <= 4 * bound, "{p} ({ns} slots): {work} > 4 × {bound}");
            }
        }
    }

    #[test]
    fn escape_round_trips() {
        let text = r"a.b*c(d)[e]{f}^$|\#&-~ g";
        let re = Regex::new(&escape(text)).unwrap();
        let mut vm = re.vm(2);
        let mut work = 0;
        assert_eq!(
            vm.search(&re.program, text, 0, &mut work, &|| false)
                .unwrap()
                .map(|s| (s[0], s[1])),
            Some((0, text.len()))
        );
    }

    /// Rust's `regex` 1.12 on the same inputs (the review's differential cases): bodies that
    /// can match empty have regex-automata's priorities.
    #[test]
    fn repetitions_of_bodies_that_can_match_empty_agree_with_rust() {
        let empties = [(0, 0), (1, 1), (2, 2)];
        assert_eq!(all("(|a)*", "aa"), empties);
        assert_eq!(all("(|a)+", "aa"), empties);
        assert_eq!(all("(|a){2,}", "aa"), empties);
        assert_eq!(all("(?:|a)*", "aa"), empties);
        assert_eq!(all(r"(?:\b|a)*", "ab"), empties);
        assert_eq!(all("(?:a*|b)*", "ab"), [(0, 2)]);
        assert_eq!(all("(?:a*|b)*", "aab"), [(0, 3)]);
        assert_eq!(all("(a|)*", "aa"), [(0, 2)]);
        assert_eq!(all("(?:a?)+?", "aa"), [(0, 1), (1, 2)]);
        assert_eq!(all("(|ab)*c", "ababc"), [(0, 5)]);
        assert_eq!(
            groups("(?:(a)|())*", "b"),
            [Some(String::new()), None, Some(String::new())]
        );
        assert_eq!(
            groups("(a?)*", "b"),
            [Some(String::new()), Some(String::new())]
        );
        assert_eq!(
            groups("(|a)*", "aa"),
            [Some(String::new()), Some(String::new())]
        );
        assert_eq!(
            groups("(a*)+$", "aab"),
            [Some(String::new()), Some(String::new())]
        );
        let replaced = |p: &str, h: &str| {
            let re = Regex::new(p).unwrap();
            let (t, _) = Template::new(&re, "-");
            let mut m = Matches::new(&re, h, 2);
            let (mut out, mut last, mut work) = (String::new(), 0, 0);
            while let Some(s) = m.next(&mut work, &|| false).unwrap() {
                out.push_str(&h[last..s[0]]);
                t.expand(h, &s, &mut out, usize::MAX).unwrap();
                last = s[1];
            }
            out.push_str(&h[last..]);
            out
        };
        assert_eq!(replaced("(?:|a)*", "aab"), "-a-a-b-");
        assert_eq!(replaced("a*", "baaac"), "-b-c-");
        assert_eq!(replaced("", "ab"), "-a-b-");
        let split = |p: &str, h: &str| {
            let mut parts = Vec::new();
            let mut last = 0;
            for (s, e) in all(p, h) {
                parts.push(h[last..s].to_owned());
                last = e;
            }
            parts.push(h[last..].to_owned());
            parts
        };
        assert_eq!(split("(?:|a)*", "aab"), ["", "a", "a", "b", ""]);
        assert_eq!(split("b*", "abba"), ["", "a", "a", ""]);
        assert_eq!(split("x*", "ab"), ["", "a", "b", ""]);
    }

    #[test]
    fn empty_and_zero_repetitions_compile_at_once() {
        for p in [
            "(?:(?:(?:(?:){1000}){1000}){1000}){1000}",
            "(?:(?:(?:a{0}){1000}){1000}){1000}",
            "(?:(?:(?:(a){0}){1000}){1000}){1000}",
            "(?:(?:(?:(?:)*){1000}){1000}){1000}",
        ] {
            let started = std::time::Instant::now();
            let re = Regex::new(p).unwrap_or_else(|e| panic!("{p}: {}", e.message));
            assert!(
                started.elapsed() < std::time::Duration::from_millis(100),
                "{p}"
            );
            assert!(re.weight() < 10, "{p}: weight {}", re.weight());
        }
        // Zero-width bodies are not collapsed: every copy weighs something, so nesting them
        // reaches the size limit at once.
        for p in [
            r"(?:(?:(?:\b){1000}){1000}){1000}",
            "(?:(?:(?:()){1000}){1000}){1000}",
        ] {
            let started = std::time::Instant::now();
            assert!(refused(p).contains("larger than the limit"), "{p}");
            assert!(
                started.elapsed() < std::time::Duration::from_millis(100),
                "{p}"
            );
        }
    }

    #[test]
    fn a_class_range_cannot_start_with_a_class() {
        assert!(refused(r"[\d-z]").contains("starting with a class"));
        assert_eq!(find(r"[\d-]+", "-1-"), Some((0, 3)));
        assert_eq!(find(r"[\w--\d]+", "1ab2"), Some((1, 3)));
    }

    #[test]
    fn a_search_stops_when_cancelled() {
        let re = Regex::new(r"(?:\w{100}){4}z").unwrap();
        let hay = "a".repeat(100_000);
        let mut vm = re.vm(0);
        let mut work = 0;
        assert!(
            vm.search(&re.program, &hay, 0, &mut work, &|| true)
                .is_err()
        );
        assert!(work < 2 * 64 * crate::machine::CHECKPOINT + 10 * re.weight());
    }
}
