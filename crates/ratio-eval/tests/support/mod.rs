//! Shared helpers: load, check, lower and prepare a one-file program, then run a function.

#![allow(dead_code, reason = "each test file uses some of the helpers")]

use std::sync::Arc;

use ratio_eval::{CancelToken, Finished, Labels, Limits, Outcome, Prepared, Value};
use ratio_names::{
    Loaded, Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source,
};

/// Loads a package of one file, `main.ratio`, or of several `(path, source)` files.
pub fn load_files(files: &[(&str, &str)]) -> Loaded {
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let mut registry = MemoryRegistry::new();
    for (path, source) in files {
        let rel = RelPath::parse(path).unwrap();
        if *path == "main.ratio" {
            registry.set_entry(&package, rel.clone());
        }
        registry.add_file(&package, rel, source.as_bytes().to_vec());
    }
    let builtin = MemoryRegistry::new();
    Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap())
}

/// The lowered program of a package of one file, panicking with the diagnostics if it does
/// not check.
pub fn program(source: &str) -> ratio_ir::Program {
    program_of(&[("main.ratio", source)])
}

pub fn program_of(files: &[(&str, &str)]) -> ratio_ir::Program {
    let loaded = load_files(files);
    assert!(
        loaded.is_clean(),
        "the source must load cleanly: {:?} {:?}",
        loaded.diagnostics,
        loaded.modules.iter().map(|m| &m.syntax).collect::<Vec<_>>()
    );
    let checked = ratio_check::check(&loaded);
    assert!(
        checked.is_clean(),
        "the source must check cleanly: {:#?}",
        checked
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    ratio_ir::lower(&loaded, &checked).expect("lowers")
}

pub fn prepared(source: &str) -> Arc<Prepared> {
    ratio_eval::prepare(&program(source)).expect("prepares")
}

/// Runs `name` in `main.ratio` with `args` and `limits`.
pub fn run_with(p: &Arc<Prepared>, name: &str, args: Vec<Value>, limits: Limits) -> Finished {
    let entry = p
        .find("main.ratio", name)
        .unwrap_or_else(|| panic!("no function `{name}`"));
    let eval = ratio_eval::evaluate(Arc::clone(p), entry, args, limits, CancelToken::new())
        .expect("starts");
    ratio_eval::block_on(eval)
}

pub fn run(p: &Arc<Prepared>, name: &str, args: Vec<Value>) -> Finished {
    run_with(p, name, args, Limits::default())
}

/// The value `name` returns, panicking if it raises.
pub fn value(p: &Arc<Prepared>, name: &str, args: Vec<Value>) -> Value {
    match run(p, name, args).outcome {
        Outcome::Returned(v) => v,
        Outcome::Raised(e) => panic!("`{name}` raised {} ({:?})", e.kind(p), e.message()),
    }
}

/// The kind of error `name` raises, panicking if it returns.
pub fn raised(p: &Arc<Prepared>, name: &str, args: Vec<Value>, limits: Limits) -> String {
    match run_with(p, name, args, limits).outcome {
        Outcome::Returned(v) => panic!("`{name}` returned {}", ratio_eval::to_json_text(p, &v)),
        Outcome::Raised(e) => e.kind(p),
    }
}

/// A secret label (source 0).
pub const SECRET: Labels = Labels::source(0);

/// Unmetered text with the secret label.
pub fn secret(text: &str) -> Value {
    Value::text(text).with_labels(SECRET)
}

pub fn secret_int(i: i64) -> Value {
    Value::int(i).with_labels(SECRET)
}
