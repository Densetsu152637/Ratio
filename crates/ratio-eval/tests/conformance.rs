//! The evaluation corpus (chapter 10, "The conformance suite"): `conformance/eval/`.
//!
//! Each `.ratio` file is a package of one file whose `fn main()` is run (with no arguments,
//! the default limits and no host). Its header says what must happen:
//!
//! ```text
//! // expect: ok                          (checks and runs without raising)
//! // expect: value {"a": [1, 2]}         (returns this value, printed as JSON; object keys in
//! //                                      any order)
//! // expect: runtime-error Invalid       (raises an error of this kind)
//! // expect: steps 1042                  (is charged exactly this many steps)
//! ```
//!
//! A file must check cleanly (stages 3 to 5). Every header line must be one of these
//! directives, so a misspelt one fails the file.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use ratio_eval::{Outcome, to_json_text};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/eval")
}

#[derive(Default)]
struct Case {
    ok: bool,
    value: Option<String>,
    error: Option<String>,
    steps: Option<u64>,
}

fn header(text: &str, file: &str) -> Result<Case, String> {
    let mut case = Case::default();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        let Some(expect) = rest.strip_prefix("expect: ") else {
            return Err(format!("{file}: unknown header directive `{line}`"));
        };
        let (kind, arg) = expect.split_once(' ').unwrap_or((expect, ""));
        match kind {
            "ok" if arg.is_empty() => case.ok = true,
            "value" => case.value = Some(arg.trim().to_owned()),
            "runtime-error" => case.error = Some(arg.trim().to_owned()),
            "steps" => {
                case.steps = Some(
                    arg.trim()
                        .parse()
                        .map_err(|_| format!("{file}: bad step count `{arg}`"))?,
                );
            }
            _ => return Err(format!("{file}: unknown expectation `{expect}`")),
        }
    }
    if !case.ok && case.value.is_none() && case.error.is_none() && case.steps.is_none() {
        return Err(format!("{file}: no expectation"));
    }
    Ok(case)
}

// ---- JSON normalisation for comparing values ----

#[derive(Debug, PartialEq)]
enum J {
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

fn normal(text: &str) -> J {
    P {
        s: text.as_bytes(),
        i: 0,
    }
    .value()
}

fn run_file(path: &Path, failures: &mut Vec<String>) -> Case {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = fs::read_to_string(path).unwrap();
    let case = match header(&text, &name) {
        Ok(c) => c,
        Err(e) => {
            failures.push(e);
            return Case::default();
        }
    };
    let loaded = support::load_files(&[("main.ratio", &text)]);
    if !loaded.is_clean() {
        let syntax: Vec<String> = loaded
            .modules
            .iter()
            .flat_map(|m| m.syntax.iter().map(|d| format!("{d:?}")))
            .collect();
        failures.push(format!(
            "{name}: does not load: {:?} {syntax:?}",
            loaded.diagnostics
        ));
        return case;
    }
    let checked = ratio_check::check(&loaded);
    if !checked.is_clean() {
        let d: Vec<String> = checked
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect();
        failures.push(format!("{name}: does not check: {d:?}"));
        return case;
    }
    let program = match ratio_ir::lower(&loaded, &checked) {
        Ok(p) => p,
        Err(e) => {
            failures.push(format!("{name}: does not lower: {e}"));
            return case;
        }
    };
    let p = ratio_eval::prepare(&program).unwrap();
    let finished = support::run(&p, "main", Vec::new());
    match (&finished.outcome, &case.error) {
        (Outcome::Raised(e), Some(kind)) => {
            if e.kind(&p) != *kind {
                failures.push(format!(
                    "{name}: raised {} ({:?}), expected {kind}",
                    e.kind(&p),
                    e.message()
                ));
            }
        }
        (Outcome::Raised(e), None) => {
            failures.push(format!("{name}: raised {} ({:?})", e.kind(&p), e.message()));
        }
        (Outcome::Returned(v), Some(kind)) => {
            failures.push(format!(
                "{name}: returned {}, expected {kind}",
                to_json_text(&p, v)
            ));
        }
        (Outcome::Returned(v), None) => {
            if let Some(want) = &case.value {
                let got = to_json_text(&p, v);
                if normal(&got) != normal(want) {
                    failures.push(format!(
                        "{name}: value differs\n  expected {want}\n  got      {got}"
                    ));
                }
            }
        }
    }
    if let Some(want) = case.steps
        && finished.steps != want
    {
        failures.push(format!(
            "{name}: charged {} steps, expected {want}",
            finished.steps
        ));
    }
    case
}

#[test]
fn the_eval_corpus_runs_as_expected() {
    let mut paths: Vec<PathBuf> = fs::read_dir(corpus())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ratio"))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 20,
        "expected the stage 6 corpus, found {} files",
        paths.len()
    );
    let mut failures = Vec::new();
    let mut kinds = std::collections::BTreeSet::new();
    let mut steps = 0;
    for path in &paths {
        let case = run_file(path, &mut failures);
        if let Some(k) = case.error {
            kinds.insert(k);
        }
        steps += usize::from(case.steps.is_some());
    }
    for kind in [
        "Invalid",
        "NotFound",
        "Budget",
        "Assertion",
        "SchemaMismatch",
        "Unsupported",
        "User",
    ] {
        if !kinds.contains(kind) {
            failures.push(format!("no file expects runtime-error {kind}"));
        }
    }
    assert!(steps >= 6, "one `steps` file per cost class");
    assert!(
        failures.is_empty(),
        "{} problem(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---- frames (stage 10) ----

/// The frames corpus (`conformance/frames/`): harness calls, configuration and overrides,
/// attenuation in every dimension a program can observe, scoped budgets, context modes,
/// questions crossing frames, `meta` and frame paths. Same directives as `conformance/eval/`.
#[test]
fn the_frames_corpus_runs_as_expected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/frames");
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ratio"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 15, "expected the stage 10 corpus");
    let mut failures = Vec::new();
    let mut kinds = std::collections::BTreeSet::new();
    for path in &paths {
        if let Some(k) = run_file(path, &mut failures).error {
            kinds.insert(k);
        }
    }
    for kind in ["Denied", "Budget", "Question"] {
        if !kinds.contains(kind) {
            failures.push(format!("no frames file expects runtime-error {kind}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} problem(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
