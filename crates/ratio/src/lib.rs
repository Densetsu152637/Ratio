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
//! Stage 11a, the host, the tape and the scheduler: the [`Host`] trait every effect crosses
//! ([`host`], [`HOST_API_VERSION`]) with the sink [`Guard`](host::Guard) the interpreter wraps
//! around every host; [`evaluate_with`], whose concurrent branches run with real concurrency
//! within the width; [`ScriptedHost`] for tests; the [`tape`] ([`TapeHost`], [`TAPE_VERSION`])
//! that records a turn and replays it verified against the tape alone; per-branch random
//! generators ([`rng`]); and, behind the `host-tests` feature, the host contract suite
//! (`host_tests`) a host in another crate runs against itself.

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

/// Stage 11a: the host boundary. [`Host`] is the trait a host implements;
/// [`host::Guard`] is the sink guard every evaluation wraps it in; [`host::HOST_API_VERSION`]
/// changes whenever the trait or a type crossing it changes (assert it at compile time:
/// `const _: () = assert!(ratio::HOST_API_VERSION == 1);`).
pub use ratio_eval::host;
pub use ratio_eval::host::{HOST_API_VERSION, Host};
/// Stage 11a: the host contract suite, for a host in another crate (feature `host-tests`).
#[cfg(feature = "host-tests")]
pub use ratio_eval::host_tests;
/// Stage 11a: the scripted test host, per-branch random generators, and evaluation with a
/// host (concurrent branches, the width, cancellation, the drain fence, deadlines).
pub use ratio_eval::scripted::ScriptedHost;
/// Stage 11a: the effect tape: record a turn, replay it verified against the tape alone.
pub use ratio_eval::tape;
pub use ratio_eval::tape::{TAPE_VERSION, TapeHost};
pub use ratio_eval::{evaluate_with, fork, rng, scripted};

/// The draft of the specification (`spec/` in this repository) this release implements.
///
/// The specification and the implementation live in the same repository, so they move together:
/// a language change edits `spec/` and the code in one pull request. A test checks that this
/// constant matches the version line of `spec/README.md`.
pub const SPEC_DRAFT: &str = "0.16";

/// The implementation stage reached (chapter 10's table): the host, the tape and the scheduler
/// (stage 11a), with stages 1 to 7 and 10 built too (8 and 9 are in progress; 11b follows).
pub const STAGE: u32 = 11;
