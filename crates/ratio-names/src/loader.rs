//! Loading a package: finding it by name, reading and parsing its files, following imports under
//! the trust rules, and resolving each module's top-level names (chapter 03, "Modules and
//! imports").
//!
//! Source text is hostile. Every file is read and parsed once, imports are followed with an
//! explicit stack (no recursion), and the loader stops at fixed limits on modules, packages,
//! import depth and bytes, so the work is linear in what was read and nothing panics.

use std::collections::{BTreeMap, HashMap};

use ratio_syntax::ast::{ExprKind, FieldInit, ImportPath, ItemKind, Lit, Module};
use ratio_syntax::{Diagnostic as SyntaxDiagnostic, LexOptions, Span, parse_with};

use crate::diag::{Code, Diagnostic};
use crate::path::{PathError, RelPath};
use crate::registry::{Registry, RegistryError};
use crate::scope::{self, Scope};
use crate::source::{Lookup, PackageId, QualifiedName, Source};
use crate::version::Version;

/// A loaded module: an index into [`Loaded::modules`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub usize);

/// The loader's bounds. The byte limits are chapter 07's; the others bound hostile import graphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes per file (256 KiB).
    pub max_file_bytes: usize,
    /// Bytes per package, all its files together (2 MiB).
    pub max_package_bytes: usize,
    /// Bytes per authored package (64 KiB).
    pub max_authored_package_bytes: usize,
    /// Files in one load (1024).
    pub max_modules: usize,
    /// Packages in one load (64).
    pub max_packages: usize,
    /// The longest chain of imports from the entry (64).
    pub max_import_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_file_bytes: 256 * 1024,
            max_package_bytes: 2 * 1024 * 1024,
            max_authored_package_bytes: 64 * 1024,
            max_modules: 1024,
            max_packages: 64,
            max_import_depth: 64,
        }
    }
}

/// One file of a load.
#[derive(Clone, Debug)]
pub struct LoadedModule {
    pub package: PackageId,
    pub path: RelPath,
    pub source: String,
    pub ast: Module,
    /// The token reader's and parser's diagnostics. A module with any is not followed further:
    /// its imports are not loaded and its names are not resolved.
    pub syntax: Vec<SyntaxDiagnostic>,
    /// The module each import item loaded, by the item's index in `ast.items`.
    pub imports: BTreeMap<usize, ModuleId>,
    /// The module's top-level names.
    pub scope: Scope,
}

impl LoadedModule {
    /// `user:app main.ratio`, for messages.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} {}", self.package, self.path)
    }
}

/// The result of a load: every module read, the entry's id, and the diagnostics.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub modules: Vec<LoadedModule>,
    /// The entry package's entry module, if it was found and read.
    pub entry: Option<ModuleId>,
    /// Loading and name diagnostics (syntax diagnostics are on each module).
    pub diagnostics: Vec<Diagnostic>,
}

impl Loaded {
    /// Whether the package loaded with no error: no error diagnostic (warnings allowed) and no
    /// syntax diagnostic in any module.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.entry.is_some()
            && self.diagnostics.iter().all(|d| d.code.is_warning())
            && self.modules.iter().all(|m| m.syntax.is_empty())
    }

    #[must_use]
    pub fn module(&self, id: ModuleId) -> &LoadedModule {
        &self.modules[id.0]
    }
}

/// Loads packages from a host's registry and the built-in packages.
pub struct Loader<'a> {
    registry: &'a dyn Registry,
    builtin: &'a dyn Registry,
    limits: Limits,
}

impl<'a> Loader<'a> {
    /// A loader over the host's `registry`; `std:` names are looked up in `builtin` only, never
    /// in the host's registry.
    #[must_use]
    pub fn new(registry: &'a dyn Registry, builtin: &'a dyn Registry) -> Self {
        Loader {
            registry,
            builtin,
            limits: Limits::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Loads the package `name` and everything it imports. The entry itself is the host's
    /// choice (selecting a package is the user's act), so only its imports go through the
    /// trust rules.
    #[must_use]
    pub fn load(&self, name: &QualifiedName) -> Loaded {
        let mut run = Run {
            loader: self,
            modules: Vec::new(),
            by_path: HashMap::new(),
            packages: BTreeMap::new(),
            diagnostics: Vec::new(),
        };
        let entry = match run
            .find(name)
            .and_then(|(id, path)| run.open_package(&id, path, name))
        {
            Ok(entry) => Some(entry),
            Err((code, message)) => {
                run.diagnostics.push(Diagnostic {
                    code,
                    message,
                    at: None,
                });
                None
            }
        };
        if let Some(entry) = entry {
            run.walk(entry);
        }
        // Only modules the walk reached have names to resolve: one opened for an import that
        // then failed (a version outside the range) is not part of the load.
        let mut reached = vec![false; run.modules.len()];
        for id in entry
            .into_iter()
            .chain(run.modules.iter().flat_map(|m| m.imports.values().copied()))
        {
            reached[id.0] = true;
        }
        let scopes = scope::build(&run.modules, &reached, &mut run.diagnostics);
        for (module, scope) in run.modules.iter_mut().zip(scopes) {
            module.scope = scope;
        }
        Loaded {
            modules: run.modules,
            entry,
            diagnostics: run.diagnostics,
        }
    }
}

type Failure = (Code, String);

#[derive(Default)]
struct PackageState {
    bytes: usize,
    /// Lower-cased path to the path read, to refuse paths that differ only in case.
    folded: HashMap<String, RelPath>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    New,
    Active,
    Done,
}

struct Run<'l, 'a> {
    loader: &'l Loader<'a>,
    modules: Vec<LoadedModule>,
    by_path: HashMap<(PackageId, RelPath), ModuleId>,
    packages: BTreeMap<PackageId, PackageState>,
    diagnostics: Vec<Diagnostic>,
}

impl Run<'_, '_> {
    fn registry(&self, source: &Source) -> &dyn Registry {
        if *source == Source::Std {
            self.loader.builtin
        } else {
            self.loader.registry
        }
    }

    /// The first source, in trust order, that has the package.
    fn find(&self, name: &QualifiedName) -> Result<(PackageId, RelPath), Failure> {
        for source in name.candidates() {
            match self.registry(&source).find(&source, &name.name) {
                Ok(Some(entry)) => return Ok((PackageId::new(source, name.name.clone()), entry)),
                Ok(None) => {}
                Err(error) => {
                    return Err((
                        Code::Unreadable,
                        format!("the registry could not look up `{name}`: {}", why(&error)),
                    ));
                }
            }
        }
        let message = match name.lookup {
            Lookup::Bare => format!(
                "no `std` or `user` package is named `{}`; a plugin, project or authored \
                 package is reached only by its qualified name",
                name.name
            ),
            Lookup::Authored => format!("no authored package is named `{}`", name.name),
            Lookup::In(ref source) => format!(
                "no package `{}`",
                PackageId::new(source.clone(), name.name.clone())
            ),
        };
        Err((Code::PackageNotFound, message))
    }

    /// Opens a package's entry module and checks its version against the name's range.
    fn open_package(
        &mut self,
        package: &PackageId,
        entry: RelPath,
        name: &QualifiedName,
    ) -> Result<ModuleId, Failure> {
        let id = self.open(package, entry)?;
        let module = &self.modules[id.0];
        if let (Some(range), true) = (&name.range, module.syntax.is_empty()) {
            match version_of(&module.ast) {
                None => {
                    return Err((
                        Code::VersionMismatch,
                        format!(
                            "`{package}` has no `version` in its `meta`, so it cannot match `@{range}`"
                        ),
                    ));
                }
                Some(version) if !range.matches(&version) => {
                    return Err((
                        Code::VersionMismatch,
                        format!("`{package}` is version {version}, outside `@{range}`"),
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(id)
    }

    /// Reads and parses a file once; later opens of the same path return the same module.
    fn open(&mut self, package: &PackageId, path: RelPath) -> Result<ModuleId, Failure> {
        let key = (package.clone(), path);
        if let Some(id) = self.by_path.get(&key) {
            return Ok(*id);
        }
        let (package, path) = key;
        let limits = self.loader.limits;
        if self.modules.len() >= limits.max_modules {
            return Err((
                Code::LimitExceeded,
                format!("more than {} files in one load", limits.max_modules),
            ));
        }
        if !self.packages.contains_key(&package) && self.packages.len() >= limits.max_packages {
            return Err((
                Code::LimitExceeded,
                format!("more than {} packages in one load", limits.max_packages),
            ));
        }
        let folded = path.as_str().to_lowercase();
        let state = self.packages.get(&package);
        if let Some(other) = state.and_then(|s| s.folded.get(&folded)) {
            return Err((
                Code::CaseConflict,
                format!("`{path}` and `{other}` of {package} differ only in letter case"),
            ));
        }
        let package_limit = if matches!(package.source, Source::Authored(_)) {
            limits.max_authored_package_bytes
        } else {
            limits.max_package_bytes
        };
        let remaining = package_limit.saturating_sub(state.map_or(0, |s| s.bytes));
        let max = limits.max_file_bytes.min(remaining);
        let too_large = || {
            let message = if max < limits.max_file_bytes {
                format!(
                    "{package} is over its limit of {} KiB",
                    package_limit / 1024
                )
            } else {
                format!(
                    "`{path}` of {package} is over the limit of {} KiB",
                    max / 1024
                )
            };
            (Code::TooLarge, message)
        };
        let bytes = match self.registry(&package.source).read(&package, &path, max) {
            Ok(bytes) if bytes.len() > max => return Err(too_large()),
            Ok(bytes) => bytes,
            Err(RegistryError::TooLarge) => return Err(too_large()),
            Err(RegistryError::NotFound) => {
                return Err((Code::FileNotFound, format!("no file `{path}` in {package}")));
            }
            Err(RegistryError::Link) => {
                return Err((
                    Code::Link,
                    format!("`{path}` of {package} is a link, and links are not followed"),
                ));
            }
            Err(error @ RegistryError::Other(_)) => {
                return Err((
                    Code::Unreadable,
                    format!("`{path}` of {package} could not be read: {}", why(&error)),
                ));
            }
        };
        let Ok(source) = String::from_utf8(bytes) else {
            return Err((
                Code::Unreadable,
                format!("`{path}` of {package} is not UTF-8"),
            ));
        };
        let state = self.packages.entry(package.clone()).or_default();
        state.bytes += source.len();
        state.folded.insert(folded, path.clone());
        let parsed = parse_with(&source, &LexOptions::default());
        let id = ModuleId(self.modules.len());
        self.by_path.insert((package.clone(), path.clone()), id);
        self.modules.push(LoadedModule {
            package,
            path,
            source,
            ast: parsed.module,
            syntax: parsed.diagnostics,
            imports: BTreeMap::new(),
            scope: Scope::default(),
        });
        Ok(id)
    }

    /// Follows imports depth first from `entry`, with an explicit stack, refusing cycles.
    fn walk(&mut self, entry: ModuleId) {
        let mut visits = vec![Visit::New; self.modules.len()];
        let mut stack: Vec<(ModuleId, Vec<usize>, usize)> = Vec::new();
        visits[entry.0] = Visit::Active;
        stack.push((entry, import_items(&self.modules[entry.0]), 0));
        while let Some(top) = stack.last_mut() {
            let module = top.0;
            let Some(&item) = top.1.get(top.2) else {
                visits[module.0] = Visit::Done;
                stack.pop();
                continue;
            };
            top.2 += 1;
            let span = import_span(&self.modules[module.0].ast, item);
            let Some(target) = self.import(module, item, span) else {
                continue;
            };
            self.modules[module.0].imports.insert(item, target);
            visits.resize(self.modules.len(), Visit::New);
            match visits[target.0] {
                Visit::Done => {}
                Visit::Active => {
                    let start = stack.iter().position(|f| f.0 == target).unwrap_or(0);
                    let chain: Vec<String> = stack[start..]
                        .iter()
                        .map(|f| f.0)
                        .chain([target])
                        .map(|id| self.modules[id.0].describe())
                        .collect();
                    self.report(
                        Code::ImportCycle,
                        format!("import cycle: {}", chain.join(" -> ")),
                        module,
                        span,
                    );
                }
                Visit::New if stack.len() >= self.loader.limits.max_import_depth => {
                    let max = self.loader.limits.max_import_depth;
                    self.report(
                        Code::LimitExceeded,
                        format!("imports nested more than {max} deep"),
                        module,
                        span,
                    );
                }
                Visit::New => {
                    visits[target.0] = Visit::Active;
                    let items = import_items(&self.modules[target.0]);
                    stack.push((target, items, 0));
                }
            }
        }
    }

    /// Resolves one import item of `module`, reporting what goes wrong at `span`.
    fn import(&mut self, module: ModuleId, item: usize, span: Span) -> Option<ModuleId> {
        let importer = &self.modules[module.0];
        let ItemKind::Import(import) = &importer.ast.items[item].kind else {
            return None;
        };
        let result = match &import.path {
            ImportPath::File(lit) => {
                let path = match crate::plain_text(lit) {
                    None => Err((
                        Code::InvalidPath,
                        "an import path must be a plain string, without interpolation".to_owned(),
                    )),
                    Some(text) => importer.path.join(&text).map_err(|error| {
                        let code = match error {
                            PathError::Escapes => Code::PathEscapes,
                            PathError::Invalid(_) => Code::InvalidPath,
                        };
                        (code, format!("`{text}`: {error}"))
                    }),
                };
                let package = importer.package.clone();
                path.and_then(|path| self.open(&package, path))
            }
            ImportPath::Source(name) => {
                let name = QualifiedName::from_ast(name);
                let source = importer.package.source.clone();
                name.map_err(|message| (Code::InvalidName, message))
                    .and_then(|name| self.package_import(&source, &name))
            }
        };
        match result {
            Ok(target) => {
                if !self.modules[target.0].syntax.is_empty() {
                    let what = self.modules[target.0].describe();
                    self.report(
                        Code::SyntaxErrors,
                        format!("{what} has syntax errors and cannot be loaded"),
                        module,
                        span,
                    );
                }
                Some(target)
            }
            Err((code, message)) => {
                self.report(code, message, module, span);
                None
            }
        }
    }

    fn package_import(
        &mut self,
        importer: &Source,
        name: &QualifiedName,
    ) -> Result<ModuleId, Failure> {
        let (package, entry) = self.find(name)?;
        if !importer.may_import(&package.source) {
            return Err((
                Code::ImportRefused,
                format!("`{package}` may not be imported here: {}", rule(importer)),
            ));
        }
        self.open_package(&package, entry, name)
    }

    fn report(&mut self, code: Code, message: String, module: ModuleId, span: Span) {
        self.diagnostics.push(Diagnostic {
            code,
            message,
            at: Some((module, span)),
        });
    }
}

/// What a source may import, for the message refusing an import.
fn rule(importer: &Source) -> String {
    match importer {
        Source::Std => "a `std` package imports only `std` packages".to_owned(),
        Source::User => {
            "a user package imports `std`, user, plugin and project packages".to_owned()
        }
        Source::Plugin(plugin) => {
            format!(
                "a package of plugin `{plugin}` imports only `std` packages and its own plugin's"
            )
        }
        Source::Project => {
            "a project package imports only `std` packages and its project's".to_owned()
        }
        Source::Authored(area) => format!(
            "an authored package imports only `std` packages and authored packages of its own area (`authored:{}`)",
            area.as_str()
        ),
    }
}

fn why(error: &RegistryError) -> String {
    match error {
        RegistryError::NotFound => "not found".to_owned(),
        RegistryError::Link => "a link".to_owned(),
        RegistryError::TooLarge => "too large".to_owned(),
        RegistryError::Other(why) => why.clone(),
    }
}

/// The indexes of a module's import items; none for a module with syntax errors.
fn import_items(module: &LoadedModule) -> Vec<usize> {
    if !module.syntax.is_empty() {
        return Vec::new();
    }
    let items = module.ast.items.iter().enumerate();
    items
        .filter(|(_, item)| matches!(item.kind, ItemKind::Import(_)))
        .map(|(index, _)| index)
        .collect()
}

/// Where an import's path or name was written.
fn import_span(module: &Module, item: usize) -> Span {
    let item = &module.items[item];
    match &item.kind {
        ItemKind::Import(import) => match &import.path {
            ImportPath::File(lit) => lit.span,
            ImportPath::Source(name) => name.span,
        },
        _ => item.span,
    }
}

/// The `version` of a module's `meta`, when it is a plain string holding a version.
fn version_of(module: &Module) -> Option<Version> {
    module
        .meta
        .as_ref()?
        .fields
        .iter()
        .find_map(|field| match field {
            FieldInit::Field { name, value } if name.name == "version" => match &value.kind {
                ExprKind::Lit(Lit::Str(lit)) => Version::parse(&crate::plain_text(lit)?),
                _ => None,
            },
            _ => None,
        })
}
