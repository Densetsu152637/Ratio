//! A linear-time regular expression engine for the `re` built-ins (chapter 06, `data`): Rust
//! `regex` syntax ([`parse`]), compiled to a Thompson NFA ([`compile`]) and run by a Pike VM
//! ([`vm`]). Nothing backtracks, so no pattern can make a search take more than
//! O(|text| · program weight · (1 + slots)) time, and the program's weight is bounded
//! ([`compile::MAX_PROGRAM`]); patterns are at most [`MAX_PATTERN_BYTES`].

mod class;
mod compile;
mod parse;
mod vm;

pub(crate) use vm::{UNSET, Vm, scratch_bytes};

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
}

impl Regex {
    /// Compiles a pattern, or says why it is refused.
    pub(crate) fn new(pattern: &str) -> Result<Regex, String> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err("the pattern is longer than 4 KiB".to_owned());
        }
        let parsed = parse::parse(pattern)?;
        let program = compile::compile(&parsed)?;
        Ok(Regex {
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

    /// The group named `name`.
    pub(crate) fn group(&self, name: &str) -> Option<u32> {
        self.names.iter().find(|(n, _)| n == name).map(|(_, i)| *i)
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

    /// The next match's slots (byte offsets), counting the work in `work`.
    pub(crate) fn next(&mut self, work: &mut u64) -> Option<Vec<usize>> {
        loop {
            if self.start > self.hay.len() {
                return None;
            }
            let slots = self
                .vm
                .search(&self.re.program, self.hay, self.start, work)?;
            let (s, e) = (slots[0], slots[1]);
            if s == e && self.last_end == Some(e) {
                // An empty match touching the last one: search again one character on.
                match self.hay[e..].chars().next() {
                    Some(c) => self.start = e + c.len_utf8(),
                    None => return None,
                }
                continue;
            }
            self.start = e;
            self.last_end = Some(e);
            return Some(slots);
        }
    }
}

/// Appends the replacement `with` for one match to `out`, as Rust's `regex` expands it: `$n`
/// and `${n}` are group `n`, `$name` and `${name}` a named group (`$name` takes the longest run
/// of letters, digits and `_`), `$$` is `$`, a group that did not take part or does not exist
/// is empty, and a `$` that starts none of these is itself. Returns the work (bytes read and
/// written), or `Err` with the work done when `out` would pass `limit` bytes (it stops there).
pub(crate) fn expand(
    re: &Regex,
    with: &str,
    hay: &str,
    slots: &[usize],
    out: &mut String,
    limit: usize,
) -> Result<u64, u64> {
    let group = |i: u32| -> &str {
        let (a, b) = (2 * i as usize, 2 * i as usize + 1);
        match (slots.get(a), slots.get(b)) {
            (Some(&s), Some(&e)) if s != UNSET && e != UNSET => &hay[s..e],
            _ => "",
        }
    };
    let lookup = |name: &str| -> &str {
        match name.parse::<u32>() {
            Ok(i) => group(i),
            Err(_) => re.group(name).map_or("", group),
        }
    };
    let start = out.len();
    let work = |out: &String| (with.len() + out.len() - start) as u64;
    let push = |out: &mut String, s: &str| -> Result<(), ()> {
        if out.len() + s.len() > limit {
            return Err(());
        }
        out.push_str(s);
        Ok(())
    };
    let mut rest = with;
    while let Some(at) = rest.find('$') {
        let mut ok = push(out, &rest[..at]);
        rest = &rest[at + 1..];
        if let Some(r) = rest.strip_prefix('$') {
            ok = ok.and_then(|()| push(out, "$"));
            rest = r;
        } else if let Some(r) = rest.strip_prefix('{')
            && let Some(close) = r.find('}')
        {
            ok = ok.and_then(|()| push(out, lookup(&r[..close])));
            rest = &r[close + 1..];
        } else {
            let len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if len == 0 {
                ok = ok.and_then(|()| push(out, "$"));
            } else {
                ok = ok.and_then(|()| push(out, lookup(&rest[..len])));
                rest = &rest[len..];
            }
        }
        if ok.is_err() {
            return Err(work(out));
        }
    }
    match push(out, rest) {
        Ok(()) => Ok(work(out)),
        Err(()) => Err(work(out)),
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
        let re = Regex::new(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        let mut vm = re.vm(2);
        let mut work = 0;
        vm.search(&re.program, h, 0, &mut work)
            .map(|s| (s[0], s[1]))
    }

    fn all(p: &str, h: &str) -> Vec<(usize, usize)> {
        let re = Regex::new(p).unwrap();
        let mut m = Matches::new(&re, h, 2);
        let mut out = Vec::new();
        let mut work = 0;
        while let Some(s) = m.next(&mut work) {
            out.push((s[0], s[1]));
        }
        out
    }

    fn groups(p: &str, h: &str) -> Vec<Option<String>> {
        let re = Regex::new(p).unwrap();
        let mut vm = re.vm(re.slots(true));
        let mut work = 0;
        let s = vm.search(&re.program, h, 0, &mut work).unwrap();
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
            .unwrap_or_else(|| panic!("`{p}` was accepted"))
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
        let s = vm.search(&re.program, hay, 0, &mut work).unwrap();
        let mut out = String::new();
        expand(
            &re,
            "$2/${y} $$ $y1 $ ${9} $0",
            hay,
            &s,
            &mut out,
            usize::MAX,
        )
        .unwrap();
        let mut short = String::new();
        assert!(expand(&re, "$0$0$0", hay, &s, &mut short, 10).is_err());
        assert!(short.len() <= 10);
        assert_eq!(out, "05/2024 $  $  2024-05");
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
                vm.search(&re.program, &hay, 0, &mut work);
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
            vm.search(&re.program, text, 0, &mut work)
                .map(|s| (s[0], s[1])),
            Some((0, text.len()))
        );
    }
}
