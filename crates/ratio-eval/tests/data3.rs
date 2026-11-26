//! Stage 9 (`math`, `hash`, `diff`, `template`, `vec`, `path`, `url`, `codec`): the data III
//! corpus, `conformance/data/<module>-<built-in>.ratio`, one file per built-in.
//!
//! The files use the directives of the stage 7 corpus (`conformance/data/`, run for `text` and
//! `re` by `conformance.rs`):
//!
//! ```text
//! // arg: "text"                         (an argument for `main`, a JSON scalar, given with
//! //                                      a secrecy label; one line per parameter)
//! // expect: value ["a", "b"]            (returns this value, printed as JSON)
//! // expect: labels secret none          (the labels found in each element of the returned
//! //                                      list: `secret` or `none`)
//! // expect: runtime-error Invalid       (raises an error of this kind)
//! // expect: steps 12                    (is charged exactly this many steps)
//! ```
//!
//! Every file must check labels, and the files must cover every stage 9 built-in the evaluator
//! implements. `RATIO_RECORD=1` prints what each file returns instead of failing, to write a new
//! file's `value` line (and to be read against the chapter before it is trusted).

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use ratio_eval::{
    CancelToken, Labels, Limits, Outcome, PoolSession, Prepared, Value, block_on, to_json_text,
};

/// The modules of stage 9.
const MODULES: [&str; 8] = [
    "math", "hash", "diff", "template", "vec", "path", "url", "codec",
];

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/data")
}

#[derive(Default)]
struct Case {
    value: Option<String>,
    error: Option<String>,
    steps: Option<u64>,
    args: Vec<Value>,
    labels: Option<Vec<bool>>,
}

/// A JSON scalar argument, with the secret label.
fn argument(text: &str) -> Option<Value> {
    argument_with(text, true)
}

/// A JSON scalar argument, labelled secret or not.
fn argument_with(text: &str, secret: bool) -> Option<Value> {
    let t = text.trim();
    let v = if t.starts_with('"') {
        match normal(t) {
            J::Str(s) => Value::text(&unescape(&s)),
            _ => return None,
        }
    } else if t == "true" || t == "false" {
        Value::bool(t == "true")
    } else if let Ok(i) = t.parse::<i64>() {
        Value::int(i)
    } else {
        Value::float(t.parse().ok()?)
    };
    Some(if secret {
        v.with_labels(support::SECRET)
    } else {
        v
    })
}

/// The JSON escapes a header string may use.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

fn header(text: &str, file: &str) -> Result<Case, String> {
    let mut case = Case::default();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(a) = rest.strip_prefix("arg: ") {
            case.args
                .push(argument(a).ok_or_else(|| format!("{file}: bad argument `{a}`"))?);
            continue;
        }
        let Some(expect) = rest.strip_prefix("expect: ") else {
            return Err(format!("{file}: unknown header directive `{line}`"));
        };
        let (kind, arg) = expect.split_once(' ').unwrap_or((expect, ""));
        match kind {
            "value" => case.value = Some(arg.trim().to_owned()),
            "labels" => {
                let mut v = Vec::new();
                for w in arg.split_whitespace() {
                    v.push(match w {
                        "secret" => true,
                        "none" => false,
                        _ => return Err(format!("{file}: bad label word `{w}`")),
                    });
                }
                case.labels = Some(v);
            }
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

/// Each element's labels (or the whole value's, for one word) against the expectation, and
/// every summary against a full scan.
fn check_labels(name: &str, v: &Value, want: &[bool], failures: &mut Vec<String>) {
    if !v.summaries_hold() {
        failures.push(format!("{name}: a label summary misses a label"));
    }
    let parts = match (want.len(), v.elements().or_else(|| v.parts())) {
        (1, _) => vec![v.clone()],
        (_, Some(parts)) => parts,
        (_, None) => {
            failures.push(format!("{name}: `labels` needs a list or tuple result"));
            return;
        }
    };
    if parts.len() != want.len() {
        failures.push(format!(
            "{name}: {} label words for {} elements",
            want.len(),
            parts.len()
        ));
        return;
    }
    for (i, (part, secret)) in parts.iter().zip(want).enumerate() {
        let got = part.scan().contains(support::SECRET) && !part.scan().is_empty();
        if got != *secret {
            failures.push(format!(
                "{name}: element {i} is {}, expected {}",
                if got { "secret" } else { "unlabelled" },
                if *secret { "secret" } else { "unlabelled" }
            ));
        }
    }
}

fn run_file(path: &Path, failures: &mut Vec<String>) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = fs::read_to_string(path).unwrap();
    let record = std::env::var_os("RATIO_RECORD").is_some();
    let case = match header(&text, &name) {
        Ok(c) => c,
        Err(e) => {
            failures.push(e);
            return;
        }
    };
    let Ok(p) = std::panic::catch_unwind(|| support::prepared(&text)) else {
        failures.push(format!("{name}: does not load, check or lower"));
        return;
    };
    let finished = support::run(&p, "main", case.args.clone());
    match (&finished.outcome, &case.error) {
        (Outcome::Raised(e), Some(kind)) => {
            if e.kind(&p) != *kind {
                failures.push(format!("{name}: raised {}, expected {kind}", e.kind(&p)));
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
            let got = to_json_text(&p, v);
            if record {
                println!("RECORD {name} {got}");
                println!(
                    "LABELS {name} {}",
                    v.elements()
                        .unwrap_or_default()
                        .iter()
                        .map(|e| if e.scan().is_empty() {
                            "none"
                        } else {
                            "secret"
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            if let Some(want) = &case.value
                && !record
                && normal(&got) != normal(want)
            {
                failures.push(format!(
                    "{name}: value differs\n  expected {want}\n  got      {got}"
                ));
            }
            match &case.labels {
                Some(want) => {
                    if !record {
                        check_labels(&name, v, want, failures);
                    }
                }
                None => failures.push(format!("{name}: every data file checks label propagation")),
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
}

/// Every stage 9 file of the corpus, sorted.
fn files() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(corpus())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ratio"))
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            MODULES.iter().any(|m| n.starts_with(&format!("{m}-")))
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn the_data_iii_corpus_runs_as_expected() {
    let mut failures = Vec::new();
    for path in files() {
        run_file(&path, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} problem(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_stage_9_built_in_has_its_file() {
    let names: std::collections::BTreeSet<String> = files()
        .iter()
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    let mut missing = Vec::new();
    let mut count = 0;
    for (builtin, _) in ratio_eval::builtins::implemented() {
        let Some((module, _)) = builtin.split_once('.') else {
            continue;
        };
        if MODULES.contains(&module) {
            count += 1;
            let file = builtin.replacen('.', "-", 1).replace('_', "-");
            if !names.contains(&file) {
                missing.push(format!("no conformance/data/{file}.ratio for `{builtin}`"));
            }
        }
    }
    assert!(count >= 64, "the stage 9 built-ins: {count}");
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

/// Runs `main`, on `session` if given: what it observably does, and how many calls ran on the
/// pool. Value, per-element labels, steps, heap peak and the work log (name, steps, counted
/// work of every pure built-in call) must not depend on where a call ran.
type Seen = (String, Vec<Labels>, u64, u64, Vec<(&'static str, u64, u64)>);

fn run_on(p: &Arc<Prepared>, args: Vec<Value>, session: Option<&PoolSession>) -> (Seen, usize) {
    let entry = p.find("main.ratio", "main").expect("a main");
    let mut eval = ratio_eval::evaluate(
        Arc::clone(p),
        entry,
        args,
        Limits::default(),
        CancelToken::new(),
    )
    .unwrap();
    if let Some(s) = session {
        eval = eval.offload_to(s.clone());
    }
    let f = block_on(&mut eval);
    let log: Vec<ratio_eval::BuiltinWork> = if cfg!(debug_assertions) {
        eval.machine().work_log().to_vec()
    } else {
        Vec::new()
    };
    let offloaded = log.iter().filter(|w| w.offloaded).count();
    let work = log.iter().map(|w| (w.name, w.steps, w.work)).collect();
    let (shown, labels) = match &f.outcome {
        Outcome::Returned(v) => (
            to_json_text(p, v),
            v.elements()
                .map_or_else(|| vec![v.scan()], |es| es.iter().map(Value::scan).collect()),
        ),
        Outcome::Raised(e) => (e.kind(p), Vec::new()),
    };
    ((shown, labels, f.steps, f.heap_peak, work), offloaded)
}

/// Every stage 9 built-in that can go to the pool does so at a threshold of 0 and gives the
/// same value, labels, steps, heap peak and counted work as inline, for every file of the
/// corpus, with its arguments labelled and not.
#[test]
fn pooled_runs_of_the_data_iii_corpus_match_inline_runs() {
    let pool = ratio_eval::Pool::with_thresholds(
        2,
        ratio_eval::Thresholds {
            text_bytes: 0,
            elements: 0,
            steps: 0,
        },
    );
    let session = pool.session();
    let mut failures = Vec::new();
    let mut offloaded = 0;
    for path in files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&path).unwrap();
        let p = support::prepared(&text);
        for secret in [true, false] {
            let args: Vec<Value> = text
                .lines()
                .filter_map(|l| l.strip_prefix("// arg: "))
                .map(|a| argument_with(a, secret).unwrap())
                .collect();
            let (inline, _) = run_on(&p, args.clone(), None);
            let (pooled, n) = run_on(&p, args, Some(&session));
            offloaded += n;
            if inline != pooled {
                failures.push(format!(
                    "{name} (secret {secret}): inline and pooled runs differ
  inline {inline:?}
  pooled {pooled:?}"
                ));
            }
        }
    }
    if cfg!(debug_assertions) {
        assert!(offloaded > 300, "only {offloaded} calls ran on the pool");
    }
    assert_eq!(session.in_flight(), 0);
    assert!(
        failures.is_empty(),
        "{}",
        failures.join(
            "
"
        )
    );
}
