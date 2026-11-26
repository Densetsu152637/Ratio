//! The resolution corpus: `conformance/names/` (chapter 10, "The conformance suite").
//!
//! Each folder under `conformance/names/` is a **world**: a set of source roots laid out as a host
//! keeps them, one folder per source, then one folder per package with its entry file
//! `main.ratio`:
//!
//! ```text
//! <world>/std/<package>/…            <world>/plugin/<plugin>/<package>/…
//! <world>/user/<package>/…           <world>/authored/session/<package>/…
//! <world>/project/<package>/…        <world>/authored/env/<package>/…
//! ```
//!
//! Exactly one file of a world, the entry file of the package the world loads, carries the
//! resolution expectations:
//!
//! ```text
//! // expect: resolve-ok                                   (loads with no diagnostic at all)
//! // expect: resolve-error N0007 at 3:8                   (an error in this file)
//! // expect: resolve-error N0009 at 2:8 in user/app/b.ratio   (an error in another file)
//! // expect: resolve-warning N0020 at 4:4                 (a warning)
//! ```
//!
//! The diagnostics listed are all of the load's, in any order. The other files carry
//! `parse-ok` or `parse-error` lines, which the syntax runner checks.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ratio_names::{
    Area, Code, Loader, Lookup, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath,
    Source,
};

type Expected = (String, String, u32, u32);

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/names")
}

fn dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for path in fs::read_dir(dir)
        .expect("a folder")
        .flatten()
        .map(|e| e.path())
    {
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// The source roots of a world, with their folder relative to the world.
fn roots(world: &Path) -> Vec<(Source, String)> {
    let mut roots = Vec::new();
    for root in dirs(world) {
        match name(&root).as_str() {
            "std" => roots.push((Source::Std, "std".to_owned())),
            "user" => roots.push((Source::User, "user".to_owned())),
            "project" => roots.push((Source::Project, "project".to_owned())),
            "plugin" => {
                for plugin in dirs(&root) {
                    let plugin = name(&plugin);
                    roots.push((Source::Plugin(plugin.clone()), format!("plugin/{plugin}")));
                }
            }
            "authored" => {
                for area in dirs(&root) {
                    let area = match name(&area).as_str() {
                        "session" => Area::Session,
                        "env" => Area::Env,
                        other => panic!("{}: no authored area `{other}`", world.display()),
                    };
                    roots.push((
                        Source::Authored(area),
                        format!("authored/{}", area.as_str()),
                    ));
                }
            }
            other => panic!("{}: no source `{other}`", world.display()),
        }
    }
    roots
}

/// The `resolve-*` lines of a header: whether there are any, and the diagnostics they expect.
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
            ["resolve-ok"] => {
                found.get_or_insert_with(Vec::new);
                continue;
            }
            ["resolve-error" | "resolve-warning", code, "at", at] => (*code, *at, file),
            [
                "resolve-error" | "resolve-warning",
                code,
                "at",
                at,
                "in",
                other,
            ] => (*code, *at, *other),
            [kind, ..] if kind.starts_with("resolve") => {
                return Err(format!("{file}: bad expectation `{expect}`"));
            }
            _ => continue,
        };
        let warning = Code::from_code(code).is_some_and(Code::is_warning);
        if warning != words[0].ends_with("warning") {
            return Err(format!("{file}: `{code}` is listed with the wrong level"));
        }
        let (l, c) = at
            .split_once(':')
            .ok_or(format!("{file}: bad position {at}"))?;
        let parse = |n: &str| {
            n.parse::<u32>()
                .map_err(|_| format!("{file}: bad position {at}"))
        };
        found.get_or_insert_with(Vec::new).push((
            in_file.to_owned(),
            code.to_owned(),
            parse(l)?,
            parse(c)?,
        ));
    }
    Ok(found)
}

/// Runs one world; returns the codes its entry expects.
fn run(world: &Path, failures: &mut Vec<String>) -> Vec<String> {
    let world_name = name(world);
    let mut host = MemoryRegistry::new();
    let mut builtin = MemoryRegistry::new();
    let mut entry: Option<(PackageId, Vec<Expected>)> = None;
    let mut root_of = Vec::new();
    for (source, root) in roots(world) {
        root_of.push((source.clone(), root.clone()));
        for package_dir in dirs(&world.join(&root)) {
            let package_name = PackageName::parse(&name(&package_dir)).unwrap();
            let package = PackageId::new(source.clone(), package_name);
            let registry = if source == Source::Std {
                &mut builtin
            } else {
                &mut host
            };
            let mut paths = Vec::new();
            files(&package_dir, &mut paths);
            for path in paths {
                let relative = path.strip_prefix(&package_dir).unwrap();
                let relative = relative.to_string_lossy().replace('\\', "/");
                let file = format!("{root}/{}/{relative}", package.name);
                let rel = RelPath::parse(&relative)
                    .unwrap_or_else(|e| panic!("{world_name}/{file}: {e}"));
                let bytes = fs::read(&path).unwrap();
                if relative == "main.ratio" {
                    registry.set_entry(&package, rel.clone());
                }
                match expectations(&String::from_utf8_lossy(&bytes), &file) {
                    Err(problem) => failures.push(format!("{world_name}: {problem}")),
                    Ok(None) => {}
                    Ok(Some(_)) if relative != "main.ratio" => failures.push(format!(
                        "{world_name}/{file}: only a package's main.ratio can be an entry"
                    )),
                    Ok(Some(expected)) => {
                        if entry.is_some() {
                            failures.push(format!("{world_name}: more than one entry"));
                        }
                        entry = Some((package.clone(), expected));
                    }
                }
                registry.add_file(&package, rel, bytes);
            }
        }
    }
    let Some((package, mut expected)) = entry else {
        failures.push(format!("{world_name}: no file has a resolve expectation"));
        return Vec::new();
    };
    let lookup = QualifiedName {
        lookup: Lookup::In(package.source.clone()),
        name: package.name.clone(),
        range: None,
    };
    let loaded = Loader::new(&host, &builtin).load(&lookup);
    let mut got: Vec<Expected> = loaded
        .diagnostics
        .iter()
        .map(|d| match d.at {
            Some((module, span)) => {
                let module = loaded.module(module).unwrap();
                let root = &root_of
                    .iter()
                    .find(|(s, _)| *s == module.package.source)
                    .unwrap()
                    .1;
                let file = format!("{root}/{}/{}", module.package.name, module.path);
                (file, d.code.to_string(), span.line, span.col)
            }
            None => ("(entry)".to_owned(), d.code.to_string(), 0, 0),
        })
        .collect();
    got.sort();
    expected.sort();
    if got != expected {
        let messages: Vec<String> = loaded.diagnostics.iter().map(ToString::to_string).collect();
        failures.push(format!(
            "{world_name}: diagnostics differ\n  expected {expected:?}\n  got      {got:?}\n  messages {messages:#?}"
        ));
    }
    if expected.is_empty() && !loaded.is_clean() {
        failures.push(format!(
            "{world_name}: resolve-ok, but the load is not clean"
        ));
    }
    expected.into_iter().map(|(_, code, _, _)| code).collect()
}

#[test]
fn the_resolution_corpus_passes() {
    let worlds = dirs(&corpus());
    assert!(
        worlds.len() >= 20,
        "expected the stage 3 corpus, found {} worlds",
        worlds.len()
    );
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for world in &worlds {
        seen.extend(run(world, &mut failures));
    }
    assert!(
        failures.is_empty(),
        "{} failure(s)\n{}",
        failures.len(),
        failures.join("\n")
    );

    // Every code has a world, except those only a host or a limit can trigger, which the
    // integration tests cover with a registry or limits made for them.
    let host_only = [
        Code::Link,
        Code::Unreadable,
        Code::TooLarge,
        Code::LimitExceeded,
    ];
    for code in Code::ALL {
        if !host_only.contains(code) {
            assert!(seen.contains(code.as_str()), "no world expects {code}");
        }
    }
}
