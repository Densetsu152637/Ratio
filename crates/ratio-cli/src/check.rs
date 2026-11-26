//! `ratio check`: finds the package each path names, loads it through the file-system
//! registry, and gathers the diagnostics of every stage that ran (syntax `P`, names `N`, the
//! checker's `C`).

use std::path::{Path, PathBuf};

use ratio::{Loader, MemoryRegistry, PackageName, QualifiedName, RelPath, Span};

use crate::fsreg::{self, FsRegistry};

/// How bad a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    pub fn word(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// One diagnostic, ready to print.
#[derive(Clone, Debug)]
pub struct Finding {
    /// The file as the person named it (the package root they gave, then the module's path).
    pub file: String,
    pub span: Option<Span>,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    /// The file's text, for the caret line.
    pub source: Option<String>,
}

/// A package to check: where it is and how to name it.
#[derive(Clone, Debug)]
pub struct Target {
    /// The path as given.
    pub given: String,
    /// The folder the entry is in, as the person would write it (`""` for the current one).
    pub root_shown: String,
    pub root: PathBuf,
    pub entry: RelPath,
    /// Whether the path named a folder (the package is named after it) or a file (after it).
    pub folder: bool,
}

/// Why a path names no package.
#[derive(Clone, Debug)]
pub struct PathProblem {
    pub path: String,
    pub reason: String,
}

/// The package a path names: a `.ratio` file (its folder is the source root), or a folder whose
/// entry is `main.ratio`, else the only `.ratio` file in it with a `meta` block.
pub fn target(given: &str) -> Result<Target, PathProblem> {
    let problem = |reason: String| PathProblem {
        path: given.to_owned(),
        reason,
    };
    let path = Path::new(given);
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        problem(match e.kind() {
            std::io::ErrorKind::NotFound => "no such file or folder".to_owned(),
            _ => format!("cannot be read ({e})"),
        })
    })?;
    if meta.file_type().is_symlink() {
        return Err(problem("is a link, which a package may not be".to_owned()));
    }
    if meta.is_dir() {
        fsreg::check_root(path).map_err(|e| problem(registry_reason(e)))?;
        let entry = folder_entry(path).map_err(problem)?;
        let shown = given.trim_end_matches(['/', '\\']).to_owned();
        return Ok(Target {
            given: given.to_owned(),
            root_shown: shown,
            root: path.to_path_buf(),
            entry,
            folder: true,
        });
    }
    if !meta.is_file() {
        return Err(problem("is neither a file nor a folder".to_owned()));
    }
    if path.extension().and_then(|e| e.to_str()) != Some("ratio") {
        return Err(problem("is not a `.ratio` file".to_owned()));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| problem("has a file name that is not UTF-8".to_owned()))?;
    let entry =
        RelPath::parse(name).map_err(|e| problem(format!("cannot be a module path: {e}")))?;
    let parent = path.parent().unwrap_or(Path::new(""));
    let (root, shown) = if parent.as_os_str().is_empty() {
        (PathBuf::from("."), String::new())
    } else {
        (
            parent.to_path_buf(),
            given[..given.len() - name.len()]
                .trim_end_matches(['/', '\\'])
                .to_owned(),
        )
    };
    fsreg::check_root(&root).map_err(|e| problem(format!("its folder {}", registry_reason(e))))?;
    Ok(Target {
        given: given.to_owned(),
        root_shown: shown,
        root,
        entry,
        folder: false,
    })
}

fn registry_reason(e: ratio::RegistryError) -> String {
    match e {
        ratio::RegistryError::NotFound => "is not a folder".to_owned(),
        ratio::RegistryError::Link => "is a link, which a package root may not be".to_owned(),
        ratio::RegistryError::TooLarge => "is too large".to_owned(),
        ratio::RegistryError::Other => "cannot be read".to_owned(),
    }
}

/// A folder's entry: `main.ratio`, else the only `.ratio` file directly in it that declares
/// `meta`. Candidates are read through the registry's rules (no links, size first).
fn folder_entry(dir: &Path) -> Result<RelPath, String> {
    let max = ratio::names::Limits::default().max_file_bytes;
    let main = RelPath::parse("main.ratio").map_err(|e| e.to_string())?;
    match fsreg::read_under(dir, &main, max) {
        Ok(_) => return Ok(main),
        Err(ratio::RegistryError::NotFound) => {}
        Err(e) => return Err(format!("main.ratio {}", registry_reason(e))),
    }
    let listing = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|e| e.to_string())?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if Path::new(&name).extension().and_then(|e| e.to_str()) != Some("ratio") {
            continue;
        }
        let Ok(rel) = RelPath::parse(&name) else {
            continue;
        };
        let Ok(bytes) = fsreg::read_under(dir, &rel, max) else {
            continue;
        };
        let parsed = ratio::parse(&String::from_utf8_lossy(&bytes));
        if parsed.module.meta.is_some() {
            found.push(rel);
        }
    }
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("has no main.ratio and no .ratio file with a `meta` block".to_owned()),
        _ => Err(format!(
            "has no main.ratio and several .ratio files with a `meta` block ({}); name one",
            found
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A `user:` name for a package: its folder's or file's name when that is a valid package name,
/// else `main`; a name already taken gets a number.
fn package_name(target: &Target, registry: &FsRegistry) -> PackageName {
    let base = if target.folder {
        target
            .root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .unwrap_or_default()
    } else {
        target.entry.stem().to_owned()
    };
    let base =
        PackageName::parse(&base).map_or_else(|_| "main".to_owned(), |n| n.as_str().to_owned());
    let mut candidate = base.clone();
    let mut n = 2;
    loop {
        if let Ok(name) = PackageName::parse(&candidate)
            && !registry.has(&name)
        {
            return name;
        }
        candidate = format!("{base}_{n}");
        n += 1;
    }
}

/// Checks every target, each as its own `user:` package. Packages named together may import
/// one another as `user:<name>`; `std:` packages are not shipped yet, so a `std:` import is
/// reported as not found, like any missing package.
pub fn check_all(targets: &[Target]) -> (Vec<Finding>, Vec<String>) {
    let mut registry = FsRegistry::new();
    let mut names = Vec::with_capacity(targets.len());
    for t in targets {
        let name = package_name(t, &registry);
        registry.add(name.clone(), t.root.clone(), t.entry.clone());
        names.push(name);
    }
    let builtin = MemoryRegistry::new();
    let packages = Loader::new(&registry, &builtin);
    let mut findings = Vec::new();
    let mut internal = Vec::new();
    for (t, name) in targets.iter().zip(&names) {
        let qualified = match QualifiedName::parse(&format!("user:{}", name.as_str())) {
            Ok(q) => q,
            Err(e) => {
                internal.push(format!("{}: {e}", t.given));
                continue;
            }
        };
        let loaded = packages.load(&qualified);
        let shown = |m: ratio::names::ModuleId| -> (String, Option<String>) {
            match loaded.module(m) {
                Some(module) => {
                    let file = display_file(
                        &targets_root(targets, &names, &module.package),
                        module.path.as_str(),
                    );
                    (file, Some(module.source.clone()))
                }
                None => (t.given.clone(), None),
            }
        };
        for module in &loaded.modules {
            let file = display_file(
                &targets_root(targets, &names, &module.package),
                module.path.as_str(),
            );
            for d in &module.syntax {
                findings.push(Finding {
                    file: file.clone(),
                    span: Some(d.span),
                    severity: Severity::Error,
                    code: d.code.to_string(),
                    message: d.message.clone(),
                    source: Some(module.source.clone()),
                });
            }
        }
        for d in &loaded.diagnostics {
            let (file, source, span) = match d.at {
                Some((m, span)) => {
                    let (file, source) = shown(m);
                    (file, source, Some(span))
                }
                None => (t.given.clone(), None, None),
            };
            findings.push(Finding {
                file,
                span,
                severity: if d.code.is_warning() {
                    Severity::Warning
                } else {
                    Severity::Error
                },
                code: d.code.to_string(),
                message: d.message.clone(),
                source,
            });
        }
        if loaded.entry.is_none() {
            continue;
        }
        let checked = ratio::check(&loaded);
        internal.extend(checked.internal_errors().iter().cloned());
        for d in &checked.diagnostics {
            let (file, source) = shown(d.module);
            findings.push(Finding {
                file,
                span: Some(d.span),
                severity: if d.code.is_warning() {
                    Severity::Warning
                } else {
                    Severity::Error
                },
                code: d.code.to_string(),
                message: d.message.clone(),
                source,
            });
        }
    }
    findings.sort_by(|a, b| {
        let at = |f: &Finding| f.span.map_or((0, 0), |s| (s.line, s.col));
        (&a.file, at(a), &a.code).cmp(&(&b.file, at(b), &b.code))
    });
    // A module imported by two packages named together is reported once.
    findings.dedup_by(|a, b| {
        a.file == b.file && a.span == b.span && a.code == b.code && a.message == b.message
    });
    (findings, internal)
}

/// The root a package of the load was given with, as the person wrote it.
fn targets_root(targets: &[Target], names: &[PackageName], package: &ratio::PackageId) -> String {
    targets
        .iter()
        .zip(names)
        .find(|(_, n)| package.source == ratio::Source::User && **n == package.name)
        .map_or_else(|| package.to_string(), |(t, _)| t.root_shown.clone())
}

fn display_file(root: &str, path: &str) -> String {
    if root.is_empty() {
        path.to_owned()
    } else {
        format!("{root}/{path}")
    }
}
