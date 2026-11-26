//! Character sets: what a class (`[a-z]`, `\d`, `\p{Alphabetic}`, `.`) matches, as a small
//! tree evaluated per character. Every set has a **weight** (its nodes and ranges), which the
//! program size and the charges count, so testing a character against a set costs at most its
//! weight.

/// A set of characters.
#[derive(Clone, Debug)]
pub(crate) enum Set {
    /// Inclusive ranges.
    Ranges(Vec<(char, char)>),
    /// A built-in class.
    Perl(Perl),
    /// The set, case-insensitively: it holds `c` when it holds `c` or one of its simple case
    /// variants.
    Fold(Box<Set>),
    Not(Box<Set>),
    Union(Vec<Set>),
    And(Box<Set>, Box<Set>),
    Minus(Box<Set>, Box<Set>),
    Xor(Box<Set>, Box<Set>),
}

/// The built-in classes, Unicode-aware as in Rust's `regex`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Perl {
    /// `\d`: decimal digits (general category `Nd`).
    Digit,
    /// `\w`: see [`is_word`].
    Word,
    /// `\s`: the `White_Space` property.
    Space,
    /// `\p{Alphabetic}`.
    Alphabetic,
    /// `\p{Lowercase}`.
    Lower,
    /// `\p{Uppercase}`.
    Upper,
    /// `\p{N}`: `Nd`, `Nl` and `No`.
    Numeric,
    /// `\p{Cc}`.
    Control,
    /// `\p{Any}`.
    Any,
}

impl Perl {
    fn contains(self, c: char) -> bool {
        match self {
            Perl::Digit => is_digit(c),
            Perl::Word => is_word(c),
            Perl::Space => c.is_whitespace(),
            Perl::Alphabetic => c.is_alphabetic(),
            Perl::Lower => c.is_lowercase(),
            Perl::Upper => c.is_uppercase(),
            Perl::Numeric => c.is_numeric(),
            Perl::Control => c.is_control(),
            Perl::Any => true,
        }
    }
}

impl Set {
    /// Whether the set holds `c`. `work` counts the nodes and ranges visited.
    pub(crate) fn contains(&self, c: char, work: &mut u64) -> bool {
        *work += 1;
        match self {
            Set::Ranges(r) => {
                *work += r.len() as u64;
                r.iter().any(|&(a, b)| a <= c && c <= b)
            }
            Set::Perl(p) => p.contains(c),
            Set::Fold(s) => {
                s.contains(c, work)
                    || case_variants(c)
                        .into_iter()
                        .flatten()
                        .any(|v| s.contains(v, work))
            }
            Set::Not(s) => !s.contains(c, work),
            Set::Union(v) => v.iter().any(|s| s.contains(c, work)),
            Set::And(a, b) => a.contains(c, work) && b.contains(c, work),
            Set::Minus(a, b) => a.contains(c, work) && !b.contains(c, work),
            Set::Xor(a, b) => a.contains(c, work) != b.contains(c, work),
        }
    }

    /// The most work [`Set::contains`] can count.
    pub(crate) fn weight(&self) -> u64 {
        1 + match self {
            Set::Ranges(r) => r.len() as u64,
            Set::Perl(_) => 0,
            // The character and up to four variants.
            Set::Fold(s) => 5 * s.weight(),
            Set::Not(s) => s.weight(),
            Set::Union(v) => v.iter().map(Set::weight).sum(),
            Set::And(a, b) | Set::Minus(a, b) | Set::Xor(a, b) => a.weight() + b.weight(),
        }
    }

    /// The set made case-insensitive: case folding applies to the leaves, under any negation
    /// or set operation, as in Rust's `regex` (`(?i)[^a]` excludes both `a` and `A`).
    pub(crate) fn folded(self) -> Set {
        match self {
            Set::Not(s) => Set::Not(Box::new(s.folded())),
            Set::Union(v) => Set::Union(v.into_iter().map(Set::folded).collect()),
            Set::And(a, b) => Set::And(Box::new(a.folded()), Box::new(b.folded())),
            Set::Minus(a, b) => Set::Minus(Box::new(a.folded()), Box::new(b.folded())),
            Set::Xor(a, b) => Set::Xor(Box::new(a.folded()), Box::new(b.folded())),
            Set::Fold(s) => Set::Fold(s),
            leaf => Set::Fold(Box::new(leaf)),
        }
    }
}

/// A character's simple case variants: its lower and upper case where each is one character,
/// and their own counterparts (so `ſ` reaches `S` and `s`). An approximation of Unicode's simple
/// case folding orbits from the standard library's case mappings.
pub(crate) fn case_variants(c: char) -> [Option<char>; 4] {
    let lower = single(c.to_lowercase());
    let upper = single(c.to_uppercase());
    let lower_of_upper = upper.and_then(|u| single(u.to_lowercase()));
    let upper_of_lower = lower.and_then(|l| single(l.to_uppercase()));
    let mut out = [None; 4];
    for (slot, v) in out
        .iter_mut()
        .zip([lower, upper, lower_of_upper, upper_of_lower])
    {
        *slot = v.filter(|v| *v != c);
    }
    out
}

fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    let a = it.next()?;
    it.next().is_none().then_some(a)
}

/// The characters of a literal under `(?i)`: itself and its distinct case variants.
pub(crate) fn literal_variants(c: char) -> Vec<char> {
    let mut out = vec![c];
    for v in case_variants(c).into_iter().flatten() {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// The decimal digits (general category `Nd`, Unicode 16), as ranges.
pub(crate) const DIGITS: &[(char, char)] = &[
    ('\u{30}', '\u{39}'),
    ('\u{660}', '\u{669}'),
    ('\u{6F0}', '\u{6F9}'),
    ('\u{7C0}', '\u{7C9}'),
    ('\u{966}', '\u{96F}'),
    ('\u{9E6}', '\u{9EF}'),
    ('\u{A66}', '\u{A6F}'),
    ('\u{AE6}', '\u{AEF}'),
    ('\u{B66}', '\u{B6F}'),
    ('\u{BE6}', '\u{BEF}'),
    ('\u{C66}', '\u{C6F}'),
    ('\u{CE6}', '\u{CEF}'),
    ('\u{D66}', '\u{D6F}'),
    ('\u{DE6}', '\u{DEF}'),
    ('\u{E50}', '\u{E59}'),
    ('\u{ED0}', '\u{ED9}'),
    ('\u{F20}', '\u{F29}'),
    ('\u{1040}', '\u{1049}'),
    ('\u{1090}', '\u{1099}'),
    ('\u{17E0}', '\u{17E9}'),
    ('\u{1810}', '\u{1819}'),
    ('\u{1946}', '\u{194F}'),
    ('\u{19D0}', '\u{19D9}'),
    ('\u{1A80}', '\u{1A89}'),
    ('\u{1A90}', '\u{1A99}'),
    ('\u{1B50}', '\u{1B59}'),
    ('\u{1BB0}', '\u{1BB9}'),
    ('\u{1C40}', '\u{1C49}'),
    ('\u{1C50}', '\u{1C59}'),
    ('\u{A620}', '\u{A629}'),
    ('\u{A8D0}', '\u{A8D9}'),
    ('\u{A900}', '\u{A909}'),
    ('\u{A9D0}', '\u{A9D9}'),
    ('\u{A9F0}', '\u{A9F9}'),
    ('\u{AA50}', '\u{AA59}'),
    ('\u{ABF0}', '\u{ABF9}'),
    ('\u{FF10}', '\u{FF19}'),
    ('\u{104A0}', '\u{104A9}'),
    ('\u{10D30}', '\u{10D39}'),
    ('\u{10D40}', '\u{10D49}'),
    ('\u{11066}', '\u{1106F}'),
    ('\u{110F0}', '\u{110F9}'),
    ('\u{11136}', '\u{1113F}'),
    ('\u{111D0}', '\u{111D9}'),
    ('\u{112F0}', '\u{112F9}'),
    ('\u{11450}', '\u{11459}'),
    ('\u{114D0}', '\u{114D9}'),
    ('\u{11650}', '\u{11659}'),
    ('\u{116C0}', '\u{116C9}'),
    ('\u{116D0}', '\u{116E3}'),
    ('\u{11730}', '\u{11739}'),
    ('\u{118E0}', '\u{118E9}'),
    ('\u{11950}', '\u{11959}'),
    ('\u{11BF0}', '\u{11BF9}'),
    ('\u{11C50}', '\u{11C59}'),
    ('\u{11D50}', '\u{11D59}'),
    ('\u{11DA0}', '\u{11DA9}'),
    ('\u{11F50}', '\u{11F59}'),
    ('\u{16130}', '\u{16139}'),
    ('\u{16A60}', '\u{16A69}'),
    ('\u{16AC0}', '\u{16AC9}'),
    ('\u{16B50}', '\u{16B59}'),
    ('\u{16D70}', '\u{16D79}'),
    ('\u{1CCF0}', '\u{1CCF9}'),
    ('\u{1D7CE}', '\u{1D7FF}'),
    ('\u{1E140}', '\u{1E149}'),
    ('\u{1E2F0}', '\u{1E2F9}'),
    ('\u{1E4F0}', '\u{1E4F9}'),
    ('\u{1E5F1}', '\u{1E5FA}'),
    ('\u{1E950}', '\u{1E959}'),
    ('\u{1FBF0}', '\u{1FBF9}'),
];

/// Combining marks `\w` includes beyond those the `Alphabetic` property already covers: the
/// combining diacritical blocks and the Hebrew and Arabic points. An approximation of the
/// general category `M` (Rust's `\w` takes all of it), documented in the crate's README.
const MARKS: &[(char, char)] = &[
    ('\u{300}', '\u{36F}'),
    ('\u{483}', '\u{489}'),
    ('\u{591}', '\u{5BD}'),
    ('\u{5BF}', '\u{5BF}'),
    ('\u{5C1}', '\u{5C2}'),
    ('\u{5C4}', '\u{5C5}'),
    ('\u{5C7}', '\u{5C7}'),
    ('\u{610}', '\u{61A}'),
    ('\u{64B}', '\u{65F}'),
    ('\u{670}', '\u{670}'),
    ('\u{6D6}', '\u{6DC}'),
    ('\u{6DF}', '\u{6E4}'),
    ('\u{6E7}', '\u{6E8}'),
    ('\u{6EA}', '\u{6ED}'),
    ('\u{1AB0}', '\u{1AFF}'),
    ('\u{1DC0}', '\u{1DFF}'),
    ('\u{20D0}', '\u{20FF}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FE20}', '\u{FE2F}'),
    ('\u{E0100}', '\u{E01EF}'),
];

/// Connector punctuation (`Pc`) and the join controls, which `\w` includes.
const CONNECTORS: &[(char, char)] = &[
    ('_', '_'),
    ('\u{200C}', '\u{200D}'),
    ('\u{203F}', '\u{2040}'),
    ('\u{2054}', '\u{2054}'),
    ('\u{FE33}', '\u{FE34}'),
    ('\u{FE4D}', '\u{FE4F}'),
    ('\u{FF3F}', '\u{FF3F}'),
];

fn in_table(table: &[(char, char)], c: char) -> bool {
    table
        .binary_search_by(|&(a, b)| {
            if c < a {
                std::cmp::Ordering::Greater
            } else if c > b {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

pub(crate) fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && in_table(DIGITS, c))
}

/// `\w`: `Alphabetic`, decimal digits, connector punctuation, the join controls and the
/// combining marks of [`MARKS`].
pub(crate) fn is_word(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    c.is_alphabetic() || in_table(DIGITS, c) || in_table(CONNECTORS, c) || in_table(MARKS, c)
}

/// The ASCII class `[:name:]`, as ranges.
pub(crate) fn ascii_class(name: &str) -> Option<Vec<(char, char)>> {
    let r: &[(char, char)] = match name {
        "alnum" => &[('0', '9'), ('A', 'Z'), ('a', 'z')],
        "alpha" => &[('A', 'Z'), ('a', 'z')],
        "ascii" => &[('\0', '\x7F')],
        "blank" => &[('\t', '\t'), (' ', ' ')],
        "cntrl" => &[('\0', '\x1F'), ('\x7F', '\x7F')],
        "digit" => &[('0', '9')],
        "graph" => &[('!', '~')],
        "lower" => &[('a', 'z')],
        "print" => &[(' ', '~')],
        "punct" => &[('!', '/'), (':', '@'), ('[', '`'), ('{', '~')],
        "space" => &[('\t', '\r'), (' ', ' ')],
        "upper" => &[('A', 'Z')],
        "word" => &[('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')],
        "xdigit" => &[('0', '9'), ('A', 'F'), ('a', 'f')],
        _ => return None,
    };
    Some(r.to_vec())
}

/// The Unicode class `\p{name}` names, loosely matched (case, spaces, `_` and `-` ignored, as
/// Unicode's UAX44-LM3 rule), or `None` for a class this engine does not carry.
pub(crate) fn unicode_class(name: &str) -> Option<Set> {
    let key: String = name
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect();
    let perl = match key.as_str() {
        "alphabetic" | "alpha" => Perl::Alphabetic,
        "lowercase" | "lower" => Perl::Lower,
        "uppercase" | "upper" => Perl::Upper,
        "whitespace" | "wspace" | "space" => Perl::Space,
        "nd" | "decimalnumber" | "digit" => Perl::Digit,
        "n" | "number" => Perl::Numeric,
        "cc" | "control" | "cntrl" => Perl::Control,
        "any" => Perl::Any,
        "ascii" => return Some(Set::Ranges(vec![('\0', '\x7F')])),
        _ => return None,
    };
    Some(Set::Perl(perl))
}

/// The `\p` classes [`unicode_class`] carries, for error messages.
pub(crate) const UNICODE_CLASSES: &str = "Alphabetic, Lowercase, Uppercase, White_Space, Nd (Decimal_Number), N (Number), Cc (Control), Any and ASCII";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_digit_range_is_numeric_and_sorted() {
        for w in DIGITS.windows(2) {
            assert!(w[0].1 < w[1].0, "{w:?} out of order");
        }
        for &(a, b) in DIGITS {
            for c in a..=b {
                assert!(c.is_numeric(), "{c:?} is not numeric");
            }
        }
        for t in [MARKS, CONNECTORS] {
            for w in t.windows(2) {
                assert!(w[0].1 < w[1].0, "{w:?} out of order");
            }
        }
    }

    #[test]
    fn perl_classes_are_unicode_aware() {
        assert!(is_digit('٣') && is_digit('7') && !is_digit('²') && !is_digit('a'));
        assert!(is_word('é') && is_word('_') && is_word('\u{301}') && !is_word('-'));
        let mut work = 0;
        let s = Set::Ranges(vec![('a', 'c')]).folded();
        assert!(s.contains('B', &mut work) && !s.contains('d', &mut work));
        let not = Set::Not(Box::new(Set::Ranges(vec![('a', 'a')]))).folded();
        assert!(!not.contains('A', &mut work) && not.contains('b', &mut work));
        assert!(work <= 64 * (s.weight() + not.weight()));
    }
}
