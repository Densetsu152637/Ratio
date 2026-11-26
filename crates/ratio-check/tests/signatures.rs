//! The checker's built-in table against chapter 06 (`conformance/builtins.txt`, generated from
//! the chapter's tables by `crates/ratio/tests/spec.rs`).
//!
//! Every built-in of a module the checker owns has a signature, so a harness that uses it
//! checks without `C0034` ("no signature"), and its parameters are chapter 06's: the same names
//! in the same order, the same ones optional. The modules still pending are listed by the
//! stage that adds them; when a stage lands, its modules move out of [`PENDING`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// The modules whose signatures later stages add, with the stage.
const PENDING: &[(&str, &str)] = &[
    ("json", "stage 8"),
    ("list", "stage 8"),
    ("map", "stage 8"),
    ("math", "stage 9"),
    ("hash", "stage 9"),
    ("diff", "stage 9"),
    ("template", "stage 9"),
    ("vec", "stage 9"),
    ("path", "stage 9"),
    ("url", "stage 9"),
    ("codec", "stage 9"),
];

/// A built-in's module: the part before the dot, or `control` for the unprefixed functions.
fn module(name: &str) -> &str {
    name.split_once('.').map_or("control", |(m, _)| m)
}

/// The parameters of a chapter 06 signature, `<T>(a: A, b: B = d) -> R`: each name, and
/// whether it has a default. Commas inside brackets, braces, parentheses and strings belong to
/// a type or a default, not to the list.
fn params(signature: &str) -> Vec<(String, bool)> {
    let open = signature.find('(').expect("a parameter list");
    let mut depth = 0i32;
    let mut quoted = false;
    let mut parts = Vec::new();
    let mut current = String::new();
    for c in signature[open + 1..].chars() {
        if quoted {
            quoted = c != '"';
            current.push(c);
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' | '[' | '{' | '<' => depth += 1,
            ')' if depth == 0 => break,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        // `->` inside a function type is not a closing bracket.
        if c == '>' && current.ends_with('-') {
            depth += 1;
        }
        current.push(c);
    }
    parts.push(current);
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let name = p.split(':').next().unwrap_or("").trim().to_owned();
            let after_type = p.split_once(':').map_or("", |(_, rest)| rest);
            (name, after_type.contains(" = "))
        })
        .collect()
}

#[test]
fn every_owned_chapter_06_built_in_has_its_signature() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/builtins.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let spec: BTreeMap<&str, &str> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let mut f = l.split('\t');
            (f.next().unwrap(), f.next().expect("a signature column"))
        })
        .collect();
    assert_eq!(spec.len(), 479, "chapter 06 lists 479 built-ins");
    let table: BTreeMap<String, Vec<(String, bool)>> = ratio_check::builtin_rules()
        .into_iter()
        .map(|b| (b.name, b.params))
        .collect();
    let pending = |name: &str| PENDING.iter().find(|(m, _)| *m == module(name));

    let mut problems = Vec::new();
    let mut waiting: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, signature) in &spec {
        if let Some((_, stage)) = pending(name) {
            if !table.contains_key(*name) {
                *waiting.entry(stage).or_default() += 1;
            }
            continue;
        }
        let Some(ours) = table.get(*name) else {
            problems.push(format!(
                "{name}: no checker signature (`C0034` in a harness)"
            ));
            continue;
        };
        let want = params(signature);
        if *ours != want {
            problems.push(format!(
                "{name}: parameters {ours:?}, chapter 06 has {want:?}"
            ));
        }
    }
    for name in table.keys() {
        if !spec.contains_key(name.as_str()) {
            problems.push(format!(
                "{name}: in the checker's table but not in chapter 06"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    // What is still missing belongs to the pending stages only; say how much, so a stage that
    // lands sees its own count reach zero.
    let pending_modules: BTreeSet<&str> = PENDING.iter().map(|(m, _)| *m).collect();
    let missing: Vec<&str> = spec
        .keys()
        .filter(|n| !table.contains_key(**n))
        .copied()
        .collect();
    assert!(
        missing.iter().all(|n| pending_modules.contains(module(n))),
        "{missing:?}"
    );
    println!("built-ins without a signature, by the stage that adds them: {waiting:?}");
}

#[test]
fn the_parameter_reader_splits_at_the_top_level_only() {
    assert_eq!(
        params(
            "<T>(f: fn(A, B) -> T, on: [ErrorKind] = [ModelFailed, Timeout], d: Str = \"a, b\") -> T"
        ),
        vec![
            ("f".to_owned(), false),
            ("on".to_owned(), true),
            ("d".to_owned(), true)
        ]
    );
    assert_eq!(params("() -> Bool"), Vec::<(String, bool)>::new());
    assert_eq!(
        params("(m: Map<Str, Str> = {}, until: (fn(X) -> Bool)? = none) -> ()"),
        vec![("m".to_owned(), true), ("until".to_owned(), true)]
    );
}
