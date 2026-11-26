//! Ratio, the programming language for agent harnesses.
//!
//! This is the public facade: hosts such as Imperium depend on this crate (package `ratio-lang`,
//! library `ratio`) and reach the language through it, not through the crates behind it. It is
//! built in stages (chapter 10 of the specification). Stage 1, the token reader, and stage 2, the
//! parser (the [`ast`] module, [`Parsed`], [`parse`] and [`parse_with`]), both of
//! [`ratio_syntax`], are re-exported here, and so is stage 3, modules and names ([`names`]): a
//! host implements [`Registry`] over its package sources and loads a package with a [`Loader`].
//! Stage 4, the type checker ([`checker`]), checks what was loaded: [`check`] gives the
//! [`Checked`] result, its diagnostics and the side tables later stages read. Stage 5, schemas
//! and taint, is part of the same check: the sinks, trust sites ([`Checked::trust_sites`] and
//! the transitive [`Checked::summary`]), static secrets, the guarded-effect lint, schemas and
//! type digests ([`Checked::schema`], [`Checked::digest`]), and `harnesses.check` as a library
//! call ([`harnesses_check`]). Stage 6, the IR and the core interpreter: [`lower`] turns a clean
//! check into the Core IR ([`ir`]), whose canonical form gives each package its IR hash
//! ([`ir::Package::hash`]) and each expression its site id under [`IR_VERSION`]; [`prepare`]
//! builds the executable prepared form (never hashed) and [`evaluate`] runs a pure entry as a
//! future ([`interp`]), with secrecy labels on every value ([`Value`], [`Labels`]), steps, heap
//! and call-depth limits ([`Limits`]), and effects suspended for the host (stage 11). Stage 10,
//! frames ([`frame`]): harness calls, entries and `with` blocks open frames attenuated from
//! their caller's in every dimension, with budget ledgers that admit effects by reservation.

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

/// Stages 4 and 5: the type checker, with schemas and taint. Its diagnostic codes are
/// [`checker::Code`] (`C`).
pub use ratio_check as checker;
pub use ratio_check::{Checked, PackageCheck, check, harnesses_check};

/// Stage 6: the Ratio Core IR, its lowering, canonical serialisation and IR hash.
pub use ratio_ir as ir;
pub use ratio_ir::{IR_VERSION, LowerError, lower};

/// Stage 6: the core interpreter (the prepared form, values with secrecy labels, the
/// evaluator of the pure language).
pub use ratio_eval as interp;
pub use ratio_eval::{
    CancelToken, Evaluation, Finished, Labels, Limits, Outcome, Prepared, RtError, Value, block_on,
    evaluate, prepare,
};

/// Stage 10: frames. A frame's scope and the pure attenuation functions a host mirrors
/// ([`frame::attenuate`], [`frame::check_within`]), the budget ledgers with admission by
/// reservation ([`frame::Ledgers`]), the frame context every effect carries
/// ([`frame::FrameCtx`]), and evaluation in a host's session ([`evaluate_in`],
/// [`frame::Session`]).
pub use ratio_eval::{evaluate_in, frame};

/// The draft of the specification (`spec/` in this repository) this release implements.
///
/// The specification and the implementation live in the same repository, so they move together:
/// a language change edits `spec/` and the code in one pull request. A test checks that this
/// constant matches the version line of `spec/README.md`.
pub const SPEC_DRAFT: &str = "0.16";

/// The implementation stage reached (chapter 10's table): frames (stage 10), with stages 1 to 7
/// built too (8 and 9 are in progress).
pub const STAGE: u32 = 10;
