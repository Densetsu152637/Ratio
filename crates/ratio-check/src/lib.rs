//! Ratio's type checker: stage 4 of the implementation plan (chapter 10).
//!
//! [`check`] takes a loaded package (stage 3's [`ratio_names::Loaded`]) and checks every module
//! that loaded without syntax errors: records, enums, options, tuples, generic functions,
//! subtyping (chapter 03), contextual variants and variant literals (chapter 02, rules 3 and
//! 10), exhaustiveness, turbofish calls and the constant evaluation rules. It reports
//! diagnostics of the `C` series ([`Code`]) and leaves side tables for the later stages: the
//! type of every expression, what each name resolved to, how each call was instantiated, and
//! every local binding ([`ModuleTable`]), keyed by source span.
//!
//! Source text is model-written and hostile, so the work is bounded: types are interned and
//! limited in depth, relations are memoised and spend a fixed budget, exhaustiveness has its
//! own budget per match, and the recursive walk runs on a large stack with a depth bound. No
//! input makes it panic.
//!
//! Taint (sinks, `Brief` and `TrustedPrompt` positions, `trust`), schemas and `Secret<T>` are
//! stage 5. This stage maps `Untrusted<T>` by structure, because field access and the subtype
//! relation need it, and types prompts as `TrustedPrompt` or `Prompt` by their sections.

mod body;
mod builtins;
mod call;
mod check;
mod diag;
mod effects;
mod exhaust;
mod expr;
mod pattern;
mod relate;
mod stack;
mod tables;
mod types;

pub use diag::{Code, Diagnostic};
pub use tables::{
    ArgSource, CallInfo, Coercion, ItemRef, Key, LocalId, LocalInfo, ModuleTable, Origin, PatRes,
    Res,
};
pub use types::{
    Bound, EnumDef, EnumId, Field, Handle, MAX_TYPE_DEPTH, ParamDef, ParamId, Ty, TypeId, Types,
    VarId, VariantDef,
};

use ratio_names::{Loaded, ModuleId};
use ratio_syntax::Span;

use crate::check::Checker;

/// The result of a check.
#[derive(Clone, Debug)]
pub struct Checked {
    /// Every type the tables refer to.
    pub types: Types,
    /// The tables of each module, by [`ModuleId`] (empty for modules not checked: those with
    /// syntax errors).
    pub tables: Vec<ModuleTable>,
    /// The checker's diagnostics, in module and source order (loading and syntax diagnostics
    /// stay on the [`Loaded`]).
    pub diagnostics: Vec<Diagnostic>,
    std_names: Vec<&'static str>,
    internal: Vec<String>,
}

impl Checked {
    /// Whether the check found no error (warnings allowed).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.iter().all(|d| d.code.is_warning())
    }

    /// The tables of module `id`.
    #[must_use]
    pub fn table(&self, id: ModuleId) -> Option<&ModuleTable> {
        self.tables.get(id.0)
    }

    /// The name of each built-in source, by the index [`Origin::Std`] carries: `prelude` (the
    /// shared types), then the standard modules the checker has signatures for.
    #[must_use]
    pub fn std_names(&self) -> &[&'static str] {
        &self.std_names
    }

    /// Problems in the checker's own built-in declarations: always empty unless the checker
    /// has a bug. For tests.
    #[doc(hidden)]
    #[must_use]
    pub fn internal_errors(&self) -> &[String] {
        &self.internal
    }
}

/// The passes, on the current thread, following expressions, patterns and types at most
/// `max_depth` deep.
fn check_on(loaded: &Loaded, max_depth: usize, fuel: u64) -> Checked {
    let mut internal = Vec::new();
    let std = check::std_modules(&mut internal);
    let mut checker = Checker::new(loaded, &std, max_depth);
    checker.types.fuel = fuel;
    checker.internal = internal;
    checker.declare_enums();
    checker.lower_payloads();
    checker.lower_aliases();
    checker.types.compute_variance();
    checker.signatures();
    checker.bodies();
    checker.effects();
    checker.finish_tables();
    // A budget spent after the last report still refuses the package.
    if checker.types.fuel == 0
        && !checker.fuel_reported
        && let Some(entry) = loaded.entry
    {
        let span = loaded.modules[entry.0].ast.span;
        checker.report(
            tables::Origin::Module(entry),
            Code::TooComplex,
            span,
            "the program's types are too large to check within the checker's work budget"
                .to_owned(),
        );
    }
    let mut diagnostics = checker.diags;
    diagnostics.sort_by_key(|d| (d.module, d.span.start, d.span.end));
    Checked {
        types: checker.types,
        tables: checker.tables,
        diagnostics,
        std_names: std.iter().map(|m| m.name).collect(),
        internal: checker.internal,
    }
}

/// The check forced onto the caller's own stack, as it runs when no thread with a large stack
/// can be started: nesting beyond 12 levels is refused (`C0022`). For tests only.
#[doc(hidden)]
pub mod fallback {
    /// [`crate::check`] in fallback mode.
    #[must_use]
    pub fn check(loaded: &ratio_names::Loaded) -> crate::Checked {
        crate::check_on(loaded, crate::stack::FALLBACK_DEPTH, crate::types::FUEL)
    }
}

/// The check with a given work budget instead of the default, so tests can make it run out at
/// a chosen point. For tests only.
#[doc(hidden)]
pub mod budget {
    /// [`crate::check`] with `fuel` units of work for relations and substitutions.
    #[must_use]
    pub fn check(loaded: &ratio_names::Loaded, fuel: u64) -> crate::Checked {
        crate::stack::run(|max_depth| crate::check_on(loaded, max_depth, fuel))
            .unwrap_or_else(|| crate::check(loaded))
    }
}

/// Checks the modules of a loaded package.
#[must_use]
pub fn check(loaded: &Loaded) -> Checked {
    let done = stack::run(|max_depth| check_on(loaded, max_depth, types::FUEL));
    done.unwrap_or_else(|| Checked {
        types: Types::new(),
        tables: Vec::new(),
        diagnostics: vec![Diagnostic {
            code: Code::TooComplex,
            message: "the checker could not run".to_owned(),
            module: loaded.entry.unwrap_or(ModuleId(0)),
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
                end_line: 1,
                end_col: 1,
            },
        }],
        std_names: Vec::new(),
        internal: Vec::new(),
    })
}
