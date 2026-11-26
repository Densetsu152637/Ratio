//! Ratio, the programming language for agent harnesses.
//!
//! This is the public facade: hosts such as Imperium depend on this crate (package `ratio-lang`,
//! library `ratio`) and reach the language through it, not through the crates behind it. It is
//! built in stages (chapter 10 of the specification). Stage 1, the token reader, and stage 2, the
//! parser (the [`ast`] module, [`Parsed`], [`parse`] and [`parse_with`]), both of
//! [`ratio_syntax`], are re-exported here; nothing else exists yet.

pub use ratio_syntax::{
    BraceKind, Code, Contextual, Diagnostic, Keyword, LexOptions, Lexed, MAX_DIAGNOSTICS,
    MAX_INTERPOLATION_DEPTH, MAX_NESTING, MAX_SOURCE_BYTES, Parsed, Span, Token, TokenKind, ast,
    lex, lex_with, parse, parse_with,
};

/// The draft of the specification (`spec/` in this repository) this release implements.
///
/// The specification and the implementation live in the same repository, so they move together:
/// a language change edits `spec/` and the code in one pull request. A test checks that this
/// constant matches the version line of `spec/README.md`.
pub const SPEC_DRAFT: &str = "0.8";

/// The implementation stage reached (chapter 10's table): the parser.
pub const STAGE: u32 = 2;
