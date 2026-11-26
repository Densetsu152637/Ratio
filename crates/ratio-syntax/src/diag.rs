//! Diagnostics of the token reader and the parser.
//!
//! The checker's codes are the `E` series (chapter 10). The token reader uses the `L` series and
//! the parser the `P` series; chapter 10 lists both, and each code has at least one conformance
//! file (`lex-error` or `parse-error`).

use std::fmt;

use crate::token::Span;

/// The diagnostic codes of the token reader (`L`) and the parser (`P`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// A character that starts no token.
    UnexpectedCharacter,
    /// A string that ends at a line break (short strings) or at the end of the source.
    UnterminatedString,
    /// An unknown or malformed escape.
    InvalidEscape,
    /// A `\r` that is not part of `\r\n`.
    BareCarriageReturn,
    /// A lone `}` in string text (write `\}`).
    LoneCloseBrace,
    /// A line break inside an interpolation of a short string.
    LineBreakInInterpolation,
    /// An interpolation that is not closed before the end of the source.
    UnterminatedInterpolation,
    /// Letters directly after a number that are not a unit, or a bad digit separator.
    InvalidNumericSuffix,
    /// A number outside the range of its type.
    NumberOutOfRange,
    /// A quantity, duration or amount that cannot be exact.
    NumberNotExact,
    /// Interpolations nested deeper than the limit.
    InterpolationTooDeep,
    /// Brackets, blocks and interpolations nested deeper than the limit.
    NestingTooDeep,
    /// A source larger than the package-source limit.
    SourceTooLarge,
    /// A closing delimiter that matches nothing, or an opening one never closed.
    UnbalancedDelimiter,
    /// More diagnostics than the reader reports; it stops.
    TooManyErrors,
    /// A `{`, `}` or line break in a format spec.
    InvalidFormatSpec,
    /// A token the grammar does not allow where it is.
    UnexpectedToken,
    /// Expressions, types and patterns nested deeper than the limit; parsing stops.
    ParseNestingTooDeep,
    /// A `{` that the token reader read as one kind (rule 1) where the grammar needs another.
    WrongBrace,
    /// The left side of an assignment is not a place.
    NotAPlace,
    /// A second comparison or range operator in a row (`a < b < c`).
    NonAssociative,
    /// Attributes, documentation or `pub` where they do not apply.
    MisplacedModifier,
    /// More diagnostics than the limit; parsing stops.
    ParseTooManyErrors,
    /// A head (`if x`, `while x`, `for p in xs`, …) whose `{` is on the next line.
    HeadSplitAcrossLines,
    /// A positional argument after a named one.
    PositionalAfterNamed,
}

impl Code {
    /// Every code, in numeric order (the `L` codes, then the `P` codes).
    pub const ALL: [Code; 25] = [
        Code::UnexpectedCharacter,
        Code::UnterminatedString,
        Code::InvalidEscape,
        Code::BareCarriageReturn,
        Code::LoneCloseBrace,
        Code::LineBreakInInterpolation,
        Code::UnterminatedInterpolation,
        Code::InvalidNumericSuffix,
        Code::NumberOutOfRange,
        Code::NumberNotExact,
        Code::InterpolationTooDeep,
        Code::NestingTooDeep,
        Code::SourceTooLarge,
        Code::UnbalancedDelimiter,
        Code::TooManyErrors,
        Code::InvalidFormatSpec,
        Code::UnexpectedToken,
        Code::ParseNestingTooDeep,
        Code::WrongBrace,
        Code::NotAPlace,
        Code::NonAssociative,
        Code::MisplacedModifier,
        Code::ParseTooManyErrors,
        Code::HeadSplitAcrossLines,
        Code::PositionalAfterNamed,
    ];

    /// The code as written in diagnostics and conformance files (`L0001`, `P0001`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => "L0001",
            Code::UnterminatedString => "L0002",
            Code::InvalidEscape => "L0003",
            Code::BareCarriageReturn => "L0004",
            Code::LoneCloseBrace => "L0005",
            Code::LineBreakInInterpolation => "L0006",
            Code::UnterminatedInterpolation => "L0007",
            Code::InvalidNumericSuffix => "L0008",
            Code::NumberOutOfRange => "L0009",
            Code::NumberNotExact => "L0010",
            Code::InterpolationTooDeep => "L0011",
            Code::NestingTooDeep => "L0012",
            Code::SourceTooLarge => "L0013",
            Code::UnbalancedDelimiter => "L0014",
            Code::TooManyErrors => "L0015",
            Code::InvalidFormatSpec => "L0016",
            Code::UnexpectedToken => "P0001",
            Code::ParseNestingTooDeep => "P0002",
            Code::WrongBrace => "P0003",
            Code::NotAPlace => "P0004",
            Code::NonAssociative => "P0005",
            Code::MisplacedModifier => "P0006",
            Code::ParseTooManyErrors => "P0007",
            Code::HeadSplitAcrossLines => "P0008",
            Code::PositionalAfterNamed => "P0009",
        }
    }

    /// Whether the token reader reports this code (`L`); the parser reports the others (`P`).
    #[must_use]
    pub const fn is_lexical(self) -> bool {
        (self as u8) <= (Code::InvalidFormatSpec as u8)
    }

    /// The code spelled `text`, if there is one.
    #[must_use]
    pub fn from_code(text: &str) -> Option<Code> {
        Code::ALL.into_iter().find(|code| code.as_str() == text)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A problem found while reading tokens or parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub message: String,
    pub span: Span,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: error[{}]: {}",
            self.span.line, self.span.col, self.code, self.message
        )
    }
}

impl Diagnostic {
    /// The diagnostic as a person reads it: the code and message, the file, line and column, and
    /// the source line with a caret under the span.
    ///
    /// ```text
    /// error[P0001]: expected `,` or `)`, found `}`
    ///  --> harness.ratio:3:9
    ///   |
    /// 3 |   f(a, b}
    ///   |         ^
    /// ```
    ///
    /// `source` is the text the span indexes. The carets cover the span on its first line (at
    /// least one column); a tab before the span is copied, so the caret lines up in a terminal.
    #[must_use]
    pub fn render(&self, file: &str, source: &str) -> String {
        let span = self.span;
        let mut start = span.start.min(source.len());
        while !source.is_char_boundary(start) {
            start -= 1;
        }
        let before = source.get(..start).unwrap_or("");
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let after = source.get(start..).unwrap_or("");
        let line_end = after.find('\n').map_or(source.len(), |i| start + i);
        let text = source.get(line_start..line_end).unwrap_or("");
        let text = text.strip_suffix('\r').unwrap_or(text);
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let col = usize::try_from(span.col.max(1)).unwrap_or(1);
        let pad: String = text
            .chars()
            .take(col - 1)
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        let width = if span.end_line == span.line {
            usize::try_from(span.end_col.saturating_sub(span.col)).unwrap_or(1)
        } else {
            text.chars().count().saturating_sub(col - 1)
        };
        let carets = "^".repeat(width.max(1));
        let line_no = span.line.max(1).to_string();
        let gutter = " ".repeat(line_no.len());
        format!(
            "error[{code}]: {message}\n{gutter}--> {file}:{line}:{col}\n{gutter} |\n{line_no} | {text}\n{gutter} | {pad}{carets}\n",
            code = self.code,
            message = self.message,
            line = span.line,
            col = span.col,
        )
    }
}
