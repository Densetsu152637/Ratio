//! The data corpus (chapter 10, stage 8): `conformance/data/`, one file per `json`, `list` and
//! `map` built-in of chapter 06 (`data`), each with its rows' results, failures and secrecy
//! labels.
//!
//! A file is a package of one file whose header says what it covers and what must happen:
//!
//! ```text
//! // builtin: list.insert                    (the built-ins the file tests; at least one)
//! // case NAME: value [1, 9, 2]              (the function `NAME()` returns this, as JSON)
//! // case NAME: error Invalid                (it raises an error of this kind)
//! // case NAME: steps 21                     (it is charged exactly this many steps)
//! // labels NAME(xs, i, x): [1, 2] | 1 | 9  (label propagation; see below)
//! // labels NAME: [1, 2] | 1 ; inert 1       (…with arguments that must not reach the result)
//! ```
//!
//! A `labels` line runs `NAME` with the arguments written after the colon (JSON, separated by
//! `|`: integers, floats, text, booleans, `null` for `none`, arrays for lists) several times:
//! with none labelled (the result must carry no label: nothing is invented), then once per
//! argument with that argument labelled secret (its own label), and the result, or the error
//! raised, must carry the label. `; inert 0 2` names arguments whose label must **not** reach
//! it (it did not decide the result); `; inner 0` labels the first element of a list argument
//! instead of the list and the label must reach the result; `; inner-inert 0` is its
//! negative. An error counts as reached when its value (message, data) carries the label.
//!
//! Every `json`, `list` and `map` row of `spec/06-stdlib/data.md` must be named by exactly one
//! file (the test reads the tables), and every name must be a built-in the interpreter has.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "test programs and models name values briefly, and a corpus runner reads best whole"
)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ratio_eval::{Labels, Outcome, Value, to_json_text};
use support::{SECRET, normal};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/data")
}

#[derive(Default)]
struct Case {
    value: Option<String>,
    error: Option<String>,
    steps: Option<u64>,
}

struct LabelCase {
    args: Vec<String>,
    inert: Vec<usize>,
    inner: Vec<usize>,
    inner_inert: Vec<usize>,
}

#[derive(Default)]
struct Spec {
    builtins: Vec<String>,
    cases: BTreeMap<String, Case>,
    labels: BTreeMap<String, LabelCase>,
}

fn indexes(words: &str) -> Vec<usize> {
    words
        .split_whitespace()
        .map(|w| w.parse().expect("an argument index"))
        .collect()
}

fn header(text: &str, file: &str) -> Result<Spec, String> {
    let mut spec = Spec::default();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(names) = rest.strip_prefix("builtin: ") {
            spec.builtins
                .extend(names.split(',').map(|n| n.trim().to_owned()));
        } else if let Some(rest) = rest.strip_prefix("case ") {
            let (name, what) = rest
                .split_once(": ")
                .ok_or_else(|| format!("{file}: a `case` needs `NAME: what`"))?;
            let case = spec.cases.entry(name.to_owned()).or_default();
            let (kind, arg) = what.split_once(' ').unwrap_or((what, ""));
            match kind {
                "value" => case.value = Some(arg.trim().to_owned()),
                "error" => case.error = Some(arg.trim().to_owned()),
                "steps" => {
                    case.steps = Some(
                        arg.trim()
                            .parse()
                            .map_err(|_| format!("{file}: bad steps"))?,
                    );
                }
                _ => return Err(format!("{file}: unknown case expectation `{what}`")),
            }
        } else if let Some(rest) = rest.strip_prefix("labels ") {
            let (name, tail) = rest
                .split_once(": ")
                .ok_or_else(|| format!("{file}: `labels` needs `NAME: args`"))?;
            // `NAME(xs, i)`: the parameters are written for the reader.
            let name = name.split('(').next().unwrap_or(name);
            let mut parts = tail.split(';');
            let args = parts
                .next()
                .unwrap_or("")
                .split(" | ")
                .map(|a| a.trim().to_owned())
                .collect();
            let mut lc = LabelCase {
                args,
                inert: Vec::new(),
                inner: Vec::new(),
                inner_inert: Vec::new(),
            };
            for p in parts {
                let p = p.trim();
                if let Some(w) = p.strip_prefix("inner-inert") {
                    lc.inner_inert = indexes(w);
                } else if let Some(w) = p.strip_prefix("inner") {
                    lc.inner = indexes(w);
                } else if let Some(w) = p.strip_prefix("inert") {
                    lc.inert = indexes(w);
                } else {
                    return Err(format!("{file}: unknown `labels` option `{p}`"));
                }
            }
            spec.labels.insert(name.to_owned(), lc);
        } else {
            return Err(format!("{file}: unknown header line `{line}`"));
        }
    }
    if spec.builtins.is_empty() {
        return Err(format!("{file}: no `// builtin:` line"));
    }
    if spec.cases.is_empty() && spec.labels.is_empty() {
        return Err(format!("{file}: no case"));
    }
    Ok(spec)
}

// ---- arguments ----

struct Args<'a> {
    s: &'a [u8],
    i: usize,
}

impl Args<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Value {
        self.ws();
        match self.s[self.i] {
            b'n' => {
                self.i += 4;
                Value::none()
            }
            b't' => {
                self.i += 4;
                Value::bool(true)
            }
            b'f' => {
                self.i += 5;
                Value::bool(false)
            }
            b'"' => {
                self.i += 1;
                let start = self.i;
                while self.s[self.i] != b'"' {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                self.i += 1;
                Value::text(text)
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.s[self.i] == b']' {
                        self.i += 1;
                        return Value::list(items).unwrap();
                    }
                    items.push(self.value());
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
                text.parse::<i64>().map_or_else(
                    |_| Value::float(text.parse().expect("a number")),
                    Value::int,
                )
            }
        }
    }
}

fn argument(text: &str) -> Value {
    Args {
        s: text.as_bytes(),
        i: 0,
    }
    .value()
}

/// The list with its first element labelled.
fn inner_labelled(v: &Value) -> Value {
    let mut items = v.elements().expect("`inner` needs a list argument");
    assert!(!items.is_empty(), "`inner` needs a non-empty list");
    items[0] = items[0].clone().with_labels(SECRET);
    Value::list(items).unwrap()
}

// ---- running ----

fn load(
    path: &Path,
    name: &str,
    failures: &mut Vec<String>,
) -> Option<(Spec, std::sync::Arc<ratio_eval::Prepared>)> {
    let text = fs::read_to_string(path).unwrap();
    let spec = match header(&text, name) {
        Ok(s) => s,
        Err(e) => {
            failures.push(e);
            return None;
        }
    };
    let loaded = support::load_files(&[("main.ratio", &text)]);
    if !loaded.is_clean() {
        failures.push(format!("{name}: does not load: {:?}", loaded.diagnostics));
        return None;
    }
    let checked = ratio_check::check(&loaded);
    if !checked.is_clean() {
        let d: Vec<String> = checked
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect();
        failures.push(format!("{name}: does not check: {d:?}"));
        return None;
    }
    let program = match ratio_ir::lower(&loaded, &checked) {
        Ok(p) => p,
        Err(e) => {
            failures.push(format!("{name}: does not lower: {e}"));
            return None;
        }
    };
    Some((spec, ratio_eval::prepare(&program).unwrap()))
}

fn run_cases(
    name: &str,
    spec: &Spec,
    p: &std::sync::Arc<ratio_eval::Prepared>,
    failures: &mut Vec<String>,
) {
    for (fname, case) in &spec.cases {
        if p.find("main.ratio", fname).is_none() {
            failures.push(format!("{name}: no function `{fname}`"));
            continue;
        }
        let f = support::run(p, fname, Vec::new());
        match (&f.outcome, &case.error) {
            (Outcome::Raised(e), Some(kind)) => {
                if e.kind(p) != *kind {
                    failures.push(format!(
                        "{name}/{fname}: raised {} ({:?}), expected {kind}",
                        e.kind(p),
                        e.message()
                    ));
                }
            }
            (Outcome::Raised(e), None) => {
                failures.push(format!(
                    "{name}/{fname}: raised {} ({:?})",
                    e.kind(p),
                    e.message()
                ));
            }
            (Outcome::Returned(v), Some(kind)) => {
                failures.push(format!(
                    "{name}/{fname}: returned {}, expected {kind}",
                    to_json_text(p, v)
                ));
            }
            (Outcome::Returned(v), None) => {
                if let Some(want) = &case.value {
                    let got = to_json_text(p, v);
                    if normal(&got) != normal(want) {
                        failures.push(format!(
                            "{name}/{fname}: value differs\n  expected {want}\n  got      {got}"
                        ));
                    }
                }
            }
        }
        if let Some(want) = case.steps
            && f.steps != want
        {
            failures.push(format!(
                "{name}/{fname}: charged {} steps, expected {want}",
                f.steps
            ));
        }
    }
}

/// The labels in what a run returned or raised.
fn reached(p: &std::sync::Arc<ratio_eval::Prepared>, outcome: &Outcome) -> Labels {
    match outcome {
        Outcome::Returned(v) => {
            assert!(v.summaries_hold(), "a summary misses a label");
            v.scan()
        }
        Outcome::Raised(e) => {
            let _ = p;
            e.value().scan()
        }
    }
}

fn run_labels(
    name: &str,
    spec: &Spec,
    p: &std::sync::Arc<ratio_eval::Prepared>,
    failures: &mut Vec<String>,
) {
    for (fname, lc) in &spec.labels {
        let base: Vec<Value> = lc.args.iter().map(|a| argument(a)).collect();
        let clean = reached(p, &support::run(p, fname, base.clone()).outcome);
        if !clean.is_empty() {
            failures.push(format!(
                "{name}/{fname}: invented a label from unlabelled arguments"
            ));
        }
        for i in 0..base.len() {
            for (inner, expect) in [
                (false, !lc.inert.contains(&i)),
                (true, lc.inner.contains(&i)),
            ] {
                let relevant = if inner {
                    lc.inner.contains(&i) || lc.inner_inert.contains(&i)
                } else {
                    true
                };
                if !relevant {
                    continue;
                }
                let mut args = base.clone();
                args[i] = if inner {
                    inner_labelled(&base[i])
                } else {
                    base[i].clone().with_labels(SECRET)
                };
                let got = reached(p, &support::run(p, fname, args).outcome);
                let has = got.contains(SECRET);
                if has != expect {
                    failures.push(format!(
                        "{name}/{fname}: argument {i}{} {} the label",
                        if inner { " (an element)" } else { "" },
                        if has { "reached" } else { "did not reach" },
                    ));
                }
            }
        }
    }
}

#[test]
fn the_data_corpus_runs_as_expected() {
    let mut paths: Vec<PathBuf> = fs::read_dir(corpus())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ratio"))
        .collect();
    paths.sort();
    let mut failures = Vec::new();
    let mut covered: BTreeMap<String, String> = BTreeMap::new();
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some((spec, p)) = load(path, &name, &mut failures) else {
            continue;
        };
        for b in &spec.builtins {
            if let Some(other) = covered.insert(b.clone(), name.clone()) {
                failures.push(format!("{b} is covered by both {other} and {name}"));
            }
        }
        run_cases(&name, &spec, &p, &mut failures);
        run_labels(&name, &spec, &p, &mut failures);
    }
    // Every row of the chapter's `json`, `list` and `map` tables has its file.
    let chapter = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/06-stdlib/data.md"),
    )
    .unwrap();
    let rows: Vec<String> = chapter
        .lines()
        .filter_map(|l| {
            let name = l.strip_prefix("| `")?.split('`').next()?;
            ["json.", "list.", "map."]
                .iter()
                .any(|m| name.starts_with(m))
                .then(|| name.to_owned())
        })
        .collect();
    assert_eq!(rows.len(), 14 + 47 + 15, "the chapter's tables");
    let implemented: Vec<&str> = ratio_eval::builtins::implemented()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for row in &rows {
        if !covered.contains_key(row) {
            failures.push(format!("{row} has no file in conformance/data"));
        }
        if !implemented.contains(&row.as_str()) {
            failures.push(format!("{row} is not implemented"));
        }
    }
    for name in covered.keys() {
        if !rows.contains(name) {
            failures.push(format!("{name} is not a row of the chapter"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} problem(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
