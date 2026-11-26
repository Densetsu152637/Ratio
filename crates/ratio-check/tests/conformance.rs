//! The type-checking corpus: `conformance/types/` (chapter 10, "The conformance suite").
//!
//! Each `.ratio` file directly in `conformance/types/` is a package of one file. Each folder
//! is a package of several files: its `main.ratio` is the entry and the others are imported by
//! path. The entry's header says what the checker must report:
//!
//! ```text
//! // expect: check-ok                              (no checker diagnostic at all)
//! // expect: check-error C0004 at 7:13             (an error, in this file)
//! // expect: check-error C0029 at 2:9 in lib.ratio (an error in another file of the package)
//! // expect: check-warning C0031 at 4:7            (a warning)
//! ```
//!
//! The diagnostics listed are all of the checker's, in any order. The package must load and
//! resolve without an error (stage 3), and the checker's own built-in declarations must check
//! cleanly. Every code of [`ratio_check::Code`] has at least one file that expects it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ratio_check::Code;
use ratio_names::{Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source};

type Expected = (String, String, u32, u32);

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/types")
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// The `check-*` lines of a header, or `None` when it has none.
fn expectations(text: &str, file: &str) -> Result<Option<Vec<Expected>>, String> {
    let mut found = None;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        let Some(expect) = rest.strip_prefix("expect: ") else {
            continue;
        };
        let words: Vec<&str> = expect.split_whitespace().collect();
        let (code, at, in_file) = match words.as_slice() {
            ["check-ok"] => {
                found.get_or_insert_with(Vec::new);
                continue;
            }
            ["check-error" | "check-warning", code, "at", at] => (*code, *at, file),
            ["check-error" | "check-warning", code, "at", at, "in", other] => (*code, *at, *other),
            [kind, ..] if kind.starts_with("check") => {
                return Err(format!("{file}: bad expectation `{expect}`"));
            }
            _ => continue,
        };
        let Some(parsed) = Code::from_code(code) else {
            return Err(format!("{file}: no code `{code}`"));
        };
        if parsed.is_warning() != words[0].ends_with("warning") {
            return Err(format!("{file}: `{code}` is listed with the wrong level"));
        }
        let (l, c) = at
            .split_once(':')
            .ok_or(format!("{file}: bad position {at}"))?;
        let number = |n: &str| {
            n.parse::<u32>()
                .map_err(|_| format!("{file}: bad position {at}"))
        };
        found.get_or_insert_with(Vec::new).push((
            in_file.to_owned(),
            code.to_owned(),
            number(l)?,
            number(c)?,
        ));
    }
    Ok(found)
}

/// Checks one package (a file, or a folder of files); returns the codes it expects.
fn run(path: &Path, failures: &mut Vec<String>) -> Vec<String> {
    let label = path.file_name().unwrap().to_string_lossy().into_owned();
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let mut registry = MemoryRegistry::new();
    let mut expected = None;
    let files: Vec<(String, PathBuf)> = if path.is_dir() {
        entries(path)
            .into_iter()
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
            .collect()
    } else {
        vec![("main.ratio".to_owned(), path.to_path_buf())]
    };
    for (file, source) in &files {
        let bytes = fs::read(source).unwrap();
        let rel = RelPath::parse(file).unwrap_or_else(|e| panic!("{label}/{file}: {e}"));
        if file == "main.ratio" {
            registry.set_entry(&package, rel.clone());
            match expectations(&String::from_utf8_lossy(&bytes), file) {
                Ok(found) => expected = found,
                Err(problem) => failures.push(format!("{label}: {problem}")),
            }
        }
        registry.add_file(&package, rel, bytes);
    }
    let Some(mut expected) = expected else {
        failures.push(format!("{label}: no check expectation in main.ratio"));
        return Vec::new();
    };
    let builtin = MemoryRegistry::new();
    let name = QualifiedName::parse("user:app").unwrap();
    let loaded = Loader::new(&registry, &builtin).load(&name);
    if !loaded.is_clean() {
        let syntax: Vec<String> = loaded
            .modules
            .iter()
            .flat_map(|m| m.syntax.iter().map(|d| format!("{}: {d:?}", m.path)))
            .collect();
        let names: Vec<String> = loaded.diagnostics.iter().map(ToString::to_string).collect();
        failures.push(format!(
            "{label}: the package does not load cleanly: {names:?} {syntax:?}"
        ));
        return Vec::new();
    }
    let checked = ratio_check::check(&loaded);
    if !checked.internal_errors().is_empty() {
        failures.push(format!(
            "{label}: the built-in declarations do not check: {:?}",
            checked.internal_errors()
        ));
    }
    let mut got: Vec<Expected> = checked
        .diagnostics
        .iter()
        .map(|d| {
            let file = loaded.module(d.module).unwrap().path.to_string();
            (file, d.code.to_string(), d.span.line, d.span.col)
        })
        .collect();
    got.sort();
    expected.sort();
    if got != expected {
        let messages: Vec<String> = checked
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect();
        failures.push(format!(
            "{label}: diagnostics differ\n  expected {expected:?}\n  got      {got:?}\n  messages {messages:#?}"
        ));
    }
    expected.into_iter().map(|(_, code, _, _)| code).collect()
}

#[test]
fn the_type_corpus_checks_as_expected() {
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for path in entries(&corpus()) {
        let is_ratio = path.extension().is_some_and(|e| e == "ratio");
        if !(path.is_dir() || is_ratio) {
            continue;
        }
        count += 1;
        seen.extend(run(&path, &mut failures));
    }
    for code in Code::ALL {
        if !seen.contains(code.as_str()) {
            failures.push(format!("no file expects {code}"));
        }
    }
    assert!(count > 0, "no files in conformance/types");
    assert!(
        failures.is_empty(),
        "{} problem(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
