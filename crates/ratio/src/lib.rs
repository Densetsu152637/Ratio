//! Ratio, the programming language for agent harnesses.
//!
//! This is the public facade: hosts such as Imperium depend on this crate (package `ratio-lang`,
//! library `ratio`) and reach the language through it, not through the crates behind it. It is
//! built in stages (chapter 10 of the specification). Stage 1, the token reader, and stage 2, the
//! parser (the [`ast`] module, [`Parsed`], [`parse`] and [`parse_with`]), both of
//! [`ratio_syntax`], are re-exported here, and so is stage 3, modules and names ([`names`]): a
//! host implements [`Registry`] over its package sources and loads a package with a [`Loader`].
//! Stage 4, the type checker ([`checker`]), checks what was loaded: [`check`] gives the
//! [`Checked`] result, its diagnostics and the side tables later stages read.

pub use ratio_syntax::{
    BraceKind, Code, Contextual, Diagnostic, Keyword, LexOptions, Lexed, MAX_DIAGNOSTICS,
    MAX_INTERPOLATION_DEPTH, MAX_NESTING, MAX_SOURCE_BYTES, Parsed, Span, Token, TokenKind, ast,
    lex, lex_with, parse, parse_with,
};

/// Stage 3: sources, imports, source-qualified names, the registry trait and name resolution.
/// Its diagnostic codes are [`names::Code`] (`N`), apart from the syntax crate's [`Code`].
pub use ratio_names as names;
pub use ratio_names::{
    Loaded, Loader, MemoryRegistry, PackageId, PackageName, PluginName, QualifiedName, Registry,
    RegistryError, RelPath, Source,
};

/// Stage 4: the type checker. Its diagnostic codes are [`checker::Code`] (`C`).
pub use ratio_check as checker;
pub use ratio_check::{Checked, check};

/// The draft of the specification (`spec/` in this repository) this release implements.
///
/// The specification and the implementation live in the same repository, so they move together:
/// a language change edits `spec/` and the code in one pull request. A test checks that this
/// constant matches the version line of `spec/README.md`.
pub const SPEC_DRAFT: &str = "0.11";

/// The implementation stage reached (chapter 10's table): the type checker.
pub const STAGE: u32 = 4;
