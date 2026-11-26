//! The prelude: the names in scope in every file without an import (chapter 03, "Modules and
//! imports"; chapter 06).
//!
//! These are value names: the standard modules, the `data` submodules by their short names,
//! `control`'s submodules `log` and `trace`, and `control`'s functions, which are in scope
//! without their prefix. Type names (`Str`, `Untrusted`, the shared types of chapter 06) are the
//! checker's (stage 4).

/// The standard modules and their submodules that are in scope by name.
pub const MODULES: &[&str] = &[
    "model", "think", "tools", "agents", "memory", "context", "session", "state", "data",
    "control", "eval", "meta", // the modules
    "text", "re", "json", "list", "map", "math", "rand", "time", "hash", "diff", "template", "vec",
    "path", "url", "codec", // `data`
    "log", "trace", // `control`
];

/// `control`'s functions, in scope without `control.`.
pub const FUNCTIONS: &[&str] = &[
    "retry",
    "timeout",
    "race",
    "fallback",
    "par",
    "par_map",
    "par_map_settled",
    "par_fold",
    "rate_limit",
    "cached",
    "memo",
    "once",
    "assert",
    "assert_eq",
    "ensure",
    "fail",
    "question",
    "result",
    "trust",
    "as_task",
    "declassify",
    "cancelled",
    "budget_left",
    "spent",
];

/// A name the prelude provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreludeName {
    /// A standard module (`model`, `text`, `log`).
    Module(&'static str),
    /// A function of `control` (`retry`, `trust`).
    Function(&'static str),
}

/// What the prelude binds to `name`, if anything.
#[must_use]
pub fn lookup(name: &str) -> Option<PreludeName> {
    if let Some(module) = MODULES.iter().find(|m| **m == name) {
        return Some(PreludeName::Module(module));
    }
    FUNCTIONS
        .iter()
        .find(|f| **f == name)
        .map(|f| PreludeName::Function(f))
}
