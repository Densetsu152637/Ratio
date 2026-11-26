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
mod stack;
mod token;

pub use diag::{Code, Diagnostic};
pub use dump::dump;
pub use lexer::{
    LexOptions, Lexed, MAX_DIAGNOSTICS, MAX_INTERPOLATION_DEPTH, MAX_NESTING, MAX_SOURCE_BYTES,
    lex, lex_with,
};
pub use parser::{Parsed, parse, parse_with};
pub use printer::print;
/// The passes forced onto the caller's own stack, as they run when no thread with a large stack
/// can be started (see `stack`): depth is clamped to 12. For tests only.
#[doc(hidden)]
pub mod fallback {
    use crate::ast::Module;
    use crate::{LexOptions, Parsed};

    /// [`crate::parse_with`] in fallback mode.
    #[must_use]
    pub fn parse_with(source: &str, options: &LexOptions) -> Parsed {
        crate::parser::parse_on(source, options, false)
    }

    /// [`crate::print`] in fallback mode.
    #[must_use]
    pub fn print(module: &Module) -> String {
        crate::printer::print_on(module, false)
    }

    /// [`crate::dump`] in fallback mode.
    #[must_use]
    pub fn dump(module: &Module) -> String {
        crate::dump::dump_on(module, false)
    }
}
pub use token::{BraceKind, Contextual, Keyword, Span, Token, TokenKind};
