//! Tests of the reader's public surface: the limits table, the word lists and the codes. Token
//! level behaviour is covered by the conformance files and the properties.

mod support;

use std::collections::BTreeSet;

use ratio_syntax::{
    Code, Contextual, Keyword, LexOptions, MAX_DIAGNOSTICS, MAX_INTERPOLATION_DEPTH, MAX_NESTING,
    MAX_SOURCE_BYTES, TokenKind, lex, lex_with,
};

#[test]
fn a_configuration_cannot_raise_the_source_or_diagnostic_limits() {
    let raised = LexOptions {
        max_source_bytes: usize::MAX,
        max_diagnostics: usize::MAX,
        ..LexOptions::default()
    };
    assert_eq!(MAX_SOURCE_BYTES, 256 * 1024);
    let over = "a".repeat(MAX_SOURCE_BYTES + 1);
    assert_eq!(codes(&over, &raised), vec![Code::SourceTooLarge]);
    assert_eq!(MAX_DIAGNOSTICS, 100);
    let many = "~".repeat(500);
    let got = codes(&many, &raised);
    assert_eq!(got.len(), MAX_DIAGNOSTICS + 1);
    assert_eq!(got.last(), Some(&Code::TooManyErrors));
}

#[test]
fn the_diagnostic_cap_is_exact() {
    // Exactly the cap: every error is reported and the reader does not claim there were more.
    let at_cap = codes(&"~".repeat(MAX_DIAGNOSTICS), &LexOptions::default());
    assert_eq!(at_cap, vec![Code::UnexpectedCharacter; MAX_DIAGNOSTICS]);
    // One more: the cap's worth, then `L0015` where the next one was found.
    let lexed = lex(&"~".repeat(MAX_DIAGNOSTICS + 1));
    assert_eq!(lexed.diagnostics.len(), MAX_DIAGNOSTICS + 1);
    let last = lexed.diagnostics.last().expect("a diagnostic");
    assert_eq!(last.code, Code::TooManyErrors);
    assert_eq!(last.span.col, u32::try_from(MAX_DIAGNOSTICS + 1).unwrap());
}

#[test]
fn reading_stops_at_the_first_nesting_error() {
    let src = format!("{}~~~", "(".repeat(129));
    let lexed = lex(&src);
    assert_eq!(
        lexed.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
        vec![Code::NestingTooDeep]
    );
    assert_eq!(lexed.tokens.last().map(|t| &t.kind), Some(&TokenKind::Eof));
    assert!(support::check_invariants(&src, &lexed).is_ok());
}

#[test]
fn leading_zeros_are_decimal_and_four_slashes_are_a_doc_comment() {
    assert_eq!(support::render(&lex("007")), "int:7 ;!");
    assert_eq!(support::render(&lex("//// x")), "doc:\"/ x\"");
}

fn nested_interpolations(depth: usize) -> String {
    let mut s = "x".to_owned();
    for _ in 0..depth {
        s = format!("\"{{{s}}}\"");
    }
    s
}

fn codes(src: &str, options: &LexOptions) -> Vec<Code> {
    lex_with(src, options)
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect()
}

#[test]
fn the_reserved_words_are_the_specified_thirty_six() {
    assert_eq!(Keyword::ALL.len(), 36);
    let spelled: BTreeSet<&str> = Keyword::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(spelled.len(), 36);
    for word in [
        "and", "as", "break", "catch", "const", "continue", "defer", "do", "else", "enum", "false",
        "fn", "for", "harness", "if", "import", "in", "is", "let", "loop", "match", "none", "not",
        "or", "prompt", "pub", "raise", "repeat", "return", "test", "true", "try", "type", "var",
        "while", "with",
    ] {
        let keyword = Keyword::from_word(word).unwrap_or_else(|| panic!("{word} is not reserved"));
        assert_eq!(keyword.as_str(), word);
    }
    assert_eq!(Contextual::ALL.len(), 16);
}

#[test]
fn diagnostic_codes_are_distinct_and_round_trip() {
    let spelled: BTreeSet<&str> = Code::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(spelled.len(), Code::ALL.len());
    for code in Code::ALL {
        assert_eq!(Code::from_code(code.as_str()), Some(code));
    }
    assert_eq!(Code::from_code("E0412"), None);
}

#[test]
fn interpolation_nesting_is_limited_to_four_by_default() {
    let options = LexOptions::default();
    assert_eq!(
        codes(&nested_interpolations(4), &options),
        Vec::<Code>::new()
    );
    assert_eq!(
        codes(&nested_interpolations(5), &options),
        vec![Code::InterpolationTooDeep]
    );
}

#[test]
fn a_configuration_cannot_raise_interpolation_nesting_past_eight() {
    let options = LexOptions {
        max_interpolation_depth: 100,
        ..LexOptions::default()
    };
    assert_eq!(MAX_INTERPOLATION_DEPTH, 8);
    assert_eq!(
        codes(&nested_interpolations(8), &options),
        Vec::<Code>::new()
    );
    assert_eq!(
        codes(&nested_interpolations(9), &options),
        vec![Code::InterpolationTooDeep]
    );
}

#[test]
fn bracket_nesting_is_limited_to_128_by_default_and_256_at_most() {
    let balanced = |n: usize| format!("{}{}", "(".repeat(n), ")".repeat(n));
    let options = LexOptions::default();
    assert_eq!(codes(&balanced(128), &options), Vec::<Code>::new());
    assert_eq!(codes(&balanced(129), &options), vec![Code::NestingTooDeep]);
    let raised = LexOptions {
        max_nesting: 10_000,
        ..LexOptions::default()
    };
    assert_eq!(MAX_NESTING, 256);
    assert_eq!(codes(&balanced(256), &raised), Vec::<Code>::new());
    assert_eq!(codes(&balanced(257), &raised), vec![Code::NestingTooDeep]);
}

#[test]
fn source_size_is_limited_per_file() {
    let source = "a ".repeat(40_000);
    assert_eq!(source.len(), 80_000);
    assert!(lex(&source).is_clean());
    let lexed = lex_with(&source, &LexOptions::authored());
    assert_eq!(
        lexed.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
        vec![Code::SourceTooLarge]
    );
    assert_eq!(lexed.tokens.len(), 1);
    assert_eq!(lexed.tokens[0].kind, TokenKind::Eof);
    let default = LexOptions::default();
    assert_eq!(default.max_source_bytes, 256 * 1024);
    assert_eq!(LexOptions::authored().max_source_bytes, 64 * 1024);
    let over = "a".repeat(default.max_source_bytes + 1);
    assert_eq!(codes(&over, &default), vec![Code::SourceTooLarge]);
}

#[test]
fn a_byte_order_mark_is_ignored_and_not_counted_in_columns() {
    let lexed = lex("\u{feff}x");
    assert_eq!(support::render(&lexed), "ident:x ;!");
    let x = &lexed.tokens[0];
    assert_eq!((x.span.start, x.span.end), (3, 4));
    assert_eq!((x.span.line, x.span.col), (1, 1));
    assert!(support::check_invariants("\u{feff}x", &lexed).is_ok());
}

#[test]
fn crlf_line_ends_count_as_one_line() {
    let src = "let a = 1\r\nlet b = 2\r\n";
    let lexed = lex(src);
    assert!(lexed.is_clean());
    let b = lexed
        .tokens
        .iter()
        .rev()
        .find(|t| t.kind == TokenKind::Ident("b".to_owned()))
        .expect("the token b");
    assert_eq!((b.span.line, b.span.col), (2, 5));
    assert_eq!(
        support::render(&lexed),
        "kw:let ident:a = int:1 ;! kw:let ident:b = int:2 ;!"
    );
}

#[test]
fn the_stream_always_ends_in_one_eof() {
    for src in [
        "", " ", "\n", "//", "\"", "{", "}", "\u{feff}", "r\"\"\"", "$",
    ] {
        let lexed = lex(src);
        assert_eq!(lexed.tokens.last().map(|t| &t.kind), Some(&TokenKind::Eof));
        assert!(support::check_invariants(src, &lexed).is_ok(), "{src:?}");
    }
}
