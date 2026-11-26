//! Ratio's type checker: stages 4 and 5 of the implementation plan (chapter 10).
//!
//! [`check`] takes a loaded package (stage 3's [`ratio_names::Loaded`]) and checks every module
//! that loaded without syntax errors: records, enums, options, tuples, generic functions,
//! subtyping (chapter 03), contextual variants and variant literals (chapter 02, rules 3 and
//! 10), exhaustiveness, turbofish calls and the constant evaluation rules. It reports
//! diagnostics of the `C` series ([`Code`]) and leaves side tables for the later stages: the
//! type of every expression, what each name resolved to, how each call was instantiated, and
//! every local binding ([`ModuleTable`]), keyed by source span.
//!
//! Stage 5 adds **schemas and taint** (chapter 03): the sinks (untrusted text in an
//! instruction position, a `task` section that is not a `Brief`, a secret where an unlabelled
//! value is needed), `Brief` and `TrustedPrompt` inference, monomorphic `trust`, `as_task` and
//! `declassify` recorded as trust sites ([`TrustSite`], listed transitively per item in
//! [`Summary`]), static `Secret<T>` where the label is known, the guarded-effect lint, schema
//! derivation and type digests ([`schema`], [`digest`]), and `harnesses.check` as a library
//! call ([`harnesses_check`], [`report`]).
//!
//! Source text is model-written and hostile, so the work is bounded: types are interned and
//! limited in depth, relations are memoised and spend a fixed budget, exhaustiveness has its
//! own budget per match, the analyses are linear in the program, and the recursive walk runs on
//! a large stack with a depth bound. No input makes it panic.

mod analysis;
mod body;
mod builtins;
mod call;
mod check;
mod diag;
mod effects;
mod exhaust;
mod expr;
mod lint;
mod pattern;
mod relate;
pub mod report;
mod schema;
mod sha256;
mod stack;
mod tables;
mod types;

pub use analysis::{Effects, Load, MAX_SITE_WORK, Summary, TrustSite};
pub use builtins::TrustKind;
pub use diag::{Code, Diagnostic};
pub use lint::MAX_ROUNDS;
pub use report::{
    HarnessCheck, HarnessInfo, PackageCheck, SiteInfo, harnesses_check, package_readings,
};
pub use schema::{
    Digest, MAX_SCHEMA_BYTES, MAX_SCHEMA_DEPTH, SchemaError, canonical_form, digest, regex_problem,
    schema, snake_case,
};
pub use sha256::{hex, sha256};
pub use tables::{
    ArgSource, CallInfo, Coercion, ItemRef, Key, LocalId, LocalInfo, ModuleTable, Origin, PatRes,
    Res,
};
pub use types::{
    Bound, EnumDef, EnumId, Field, FieldMeta, Handle, MAX_TYPE_DEPTH, ParamDef, ParamId, Ty,
    TypeId, Types, VarId, VariantDef,
};

use std::collections::HashMap;

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
    /// Every `trust`, `as_task` and `declassify` call of the checked modules, in module and
    /// source order.
    pub trust_sites: Vec<TrustSite>,
    /// Every `meta.harness` call of the checked modules, in module and source order.
    pub loads: Vec<Load>,
    /// Whether the trust-site lists of [`Checked::summary`] were cut short by
    /// [`MAX_SITE_WORK`] (then every item lists every site: a superset).
    pub approximate: bool,
    /// The declaration of each enum, by [`EnumId`]: what stage 6's lowering names it by.
    pub enum_items: Vec<ItemRef>,
    summaries: HashMap<(ModuleId, usize), Summary>,
    std_names: Vec<&'static str>,
    /// The name each built-in source gives each of its items, by [`Origin::Std`] index and item
    /// index (`None` for items that define no name).
    std_items: Vec<Vec<Option<String>>>,
    internal: Vec<String>,
}

impl Checked {
    /// Whether the check found no error (warnings allowed).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        // An error in the built-in tables themselves (a duplicate name) fails closed.
        self.internal.is_empty() && self.diagnostics.iter().all(|d| d.code.is_warning())
    }

    /// The tables of module `id`.
    #[must_use]
    pub fn table(&self, id: ModuleId) -> Option<&ModuleTable> {
        self.tables.get(id.0)
    }

    /// What item `index` of module `module` may do and which trust sites it reaches, its own
    /// and through everything it names (`None` for imports, types and enums, and for modules
    /// not checked).
    #[must_use]
    pub fn summary(&self, module: ModuleId, index: usize) -> Option<&Summary> {
        self.summaries.get(&(module, index))
    }

    /// The JSON Schema of `t` (chapter 03, "Types are schemas").
    ///
    /// # Errors
    ///
    /// When `t` is not schema-able, or its schema is over the bounds.
    pub fn schema(&mut self, t: TypeId) -> Result<String, SchemaError> {
        schema::schema(&mut self.types, t)
    }

    /// The type digest of `t` (chapter 03, "Type digests").
    ///
    /// # Errors
    ///
    /// As [`Checked::schema`].
    pub fn digest(&mut self, t: TypeId) -> Result<Digest, SchemaError> {
        schema::digest(&mut self.types, t)
    }

    /// The name of each built-in source, by the index [`Origin::Std`] carries: `prelude` (the
    /// shared types), then the standard modules the checker has signatures for.
    #[must_use]
    pub fn std_names(&self) -> &[&'static str] {
        &self.std_names
    }

    /// The name a program writes for the built-in declaration `r`: `list.push`, or a bare
    /// `fail` for the `control` functions and the prelude's items (`None` for a user item).
    #[must_use]
    pub fn builtin_name(&self, r: ItemRef) -> Option<String> {
        let Origin::Std(s) = r.origin else {
            return None;
        };
        let name = self.std_items.get(s)?.get(r.index)?.as_deref()?;
        let module = *self.std_names.get(s)?;
        Some(if module == "control" || module == "prelude" {
            name.to_owned()
        } else {
            format!("{module}.{name}")
        })
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
    let results = checker.analyse();
    checker.lint(&results);
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
        trust_sites: results.trust_sites,
        loads: results.loads,
        approximate: results.approximate,
        enum_items: checker.enum_items,
        summaries: results.summaries,
        std_names: std.iter().map(|m| m.name).collect(),
        std_items: std
            .iter()
            .map(|m| {
                m.module
                    .items
                    .iter()
                    .map(|item| {
                        check::item_name(&item.kind)
                            .map(|id| builtins::builtin_name(&id.name).to_owned())
                    })
                    .collect()
            })
            .collect(),
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
        trust_sites: Vec::new(),
        loads: Vec::new(),
        approximate: false,
        enum_items: Vec::new(),
        summaries: HashMap::new(),
        std_names: Vec::new(),
        std_items: Vec::new(),
        internal: Vec::new(),
    })
}

/// A built-in of the checker's table, for the test that every attribute-driven rule holds
/// for function values too.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct BuiltinRules {
    /// The name as a program writes it (`trust`, `meta.harness`).
    pub name: String,
    /// Its attributes' names, in order.
    pub attrs: Vec<String>,
    /// Whether it has type parameters.
    pub generic: bool,
    /// Why it may not be used as a function value, if it may not.
    pub refused_as_value: Option<&'static str>,
}

/// Every built-in function of the table, with its attributes. For tests.
#[doc(hidden)]
#[must_use]
pub fn builtin_rules() -> Vec<BuiltinRules> {
    let mut internal = Vec::new();
    let mut out = Vec::new();
    for m in check::std_modules(&mut internal).iter().skip(1) {
        for item in &m.module.items {
            let ratio_syntax::ast::ItemKind::Fn(decl) = &item.kind else {
                continue;
            };
            let traits = builtins::Traits::of(&item.attrs, &mut internal);
            let plain = builtins::builtin_name(&decl.name.name);
            let name = if m.name == "control" {
                plain.to_owned()
            } else {
                format!("{}.{plain}", m.name)
            };
            let generic = !decl.type_params.is_empty();
            out.push(BuiltinRules {
                name,
                attrs: item.attrs.iter().map(|a| a.name.name.clone()).collect(),
                generic,
                refused_as_value: traits.value_refusal(generic),
            });
        }
    }
    out
}
