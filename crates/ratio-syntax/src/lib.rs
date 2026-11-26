//! Ratio's syntax.
//!
//! Stage 1 of the implementation plan (chapter 10 of the specification), the token reader, is
//! here: [`lex`] turns source text into tokens with spans, inserts semicolons at line ends, reads
//! string interpolation with a mode stack, and reports lexical errors without panicking. The
//! parser joins it in stage 2.

pub mod ast;
mod diag;
mod dump;
mod lexer;
mod number;
mod parser;
mod printer;
mod token;

pub use diag::{Code, Diagnostic};
pub use dump::dump;
pub use lexer::{
    LexOptions, Lexed, MAX_DIAGNOSTICS, MAX_INTERPOLATION_DEPTH, MAX_NESTING, MAX_SOURCE_BYTES,
    lex, lex_with,
};
pub use parser::{Parsed, parse, parse_with};
pub use printer::print;
pub use token::{BraceKind, Contextual, Keyword, Span, Token, TokenKind};
