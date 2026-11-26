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

/// As [`run_with`], with the machine's log of pure built-in calls (their charge and counted
/// work; empty in release builds).
pub fn run_logged(
    p: &Arc<Prepared>,
    name: &str,
    args: Vec<Value>,
    limits: Limits,
) -> (Finished, Vec<ratio_eval::BuiltinWork>) {
    let entry = p
        .find("main.ratio", name)
        .unwrap_or_else(|| panic!("no function `{name}`"));
    let mut eval = ratio_eval::evaluate(Arc::clone(p), entry, args, limits, CancelToken::new())
        .expect("starts");
    let finished = ratio_eval::block_on(&mut eval);
    #[cfg(debug_assertions)]
    let log = eval.machine().work_log().to_vec();
    #[cfg(not(debug_assertions))]
    let log = Vec::new();
    (finished, log)
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

// ---- JSON normalisation for comparing values ----

#[derive(Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn string(&mut self) -> String {
        self.i += 1;
        let mut out = Vec::new();
        while self.s[self.i] != b'"' {
            if self.s[self.i] == b'\\' {
                out.push(self.s[self.i]);
                self.i += 1;
            }
            out.push(self.s[self.i]);
            self.i += 1;
        }
        self.i += 1;
        String::from_utf8(out).unwrap()
    }

    fn value(&mut self) -> J {
        self.ws();
        match self.s[self.i] {
            b'n' => {
                self.i += 4;
                J::Null
            }
            b't' => {
                self.i += 4;
                J::Bool(true)
            }
            b'f' => {
                self.i += 5;
                J::Bool(false)
            }
            b'"' => J::Str(self.string()),
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.s[self.i] == b']' {
                        self.i += 1;
                        return J::Arr(items);
                    }
                    items.push(self.value());
                    self.ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.s[self.i] == b'}' {
                        self.i += 1;
                        items.sort_by(|a: &(String, J), b| a.0.cmp(&b.0));
                        return J::Obj(items);
                    }
                    let k = self.string();
                    self.ws();
                    self.i += 1;
                    let v = self.value();
                    items.push((k, v));
                    self.ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && b"+-.0123456789eE".contains(&self.s[self.i]) {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                // Compare numbers by value: `1.0` and `1.00` are the same.
                let n: f64 = text.parse().unwrap();
                J::Num(format!("{n:?}"))
            }
        }
    }
}

pub fn normal(text: &str) -> J {
    P {
        s: text.as_bytes(),
        i: 0,
    }
    .value()
}
