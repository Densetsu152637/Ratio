//! `harnesses.check` as a library call (chapter 08's daemon method, chapter 03's transitive
//! trust sites, chapter 07's "not runnable by models").
//!
//! [`Checked::harness_check`] summarises one checked package: what it may do (writes,
//! commands, network, delegation, any tool of the frame), the tools it names, every trust site
//! with its position, and each harness's signature schemas and whether a model may run it
//! through `run_harness`. [`harnesses_check`] loads and checks a package through a registry
//! and follows its `meta.harness` loads to the packages they name, so the trust-site list is
//! transitive through imports (one load holds every imported module) and through everything
//! the package can load. A load whose name is computed, or names a package the registry does
//! not have, makes the list incomplete ([`PackageCheck::complete`]), never silently shorter.
//! Running `test` blocks against a scripted host (`test: true`) needs the interpreter
//! (stage 6 and later).

use std::collections::BTreeSet;

use ratio_names::{Loaded, Loader, ModuleId, QualifiedName, Registry};
use ratio_syntax::Span;
use ratio_syntax::ast::ItemKind;

use crate::analysis::{Effects, Summary};
use crate::builtins::TrustKind;
use crate::schema::SchemaError;
use crate::types::{Ty, TypeId};
use crate::{Checked, check};

/// At most this many packages are loaded through `meta.harness` names, transitively.
pub const MAX_LOADED_PACKAGES: usize = 32;

/// A trust site, described without reference to the tables.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteInfo {
    /// The package (`user:app`).
    pub package: String,
    /// The file within it.
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub kind: TrustKind,
    /// The type it converts to, as written in messages.
    pub ty: String,
    /// The `because` text, when it is a literal.
    pub reason: Option<String>,
}

/// An entry attribute on a harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Turn,
    Task,
}

/// One harness of the package.
#[derive(Clone, Debug)]
pub struct HarnessInfo {
    pub module: ModuleId,
    pub name: String,
    pub span: Span,
    pub public: bool,
    pub entry: Option<Entry>,
    /// The parameters' names, types and schemas.
    pub params: Vec<(String, TypeId, Result<String, SchemaError>)>,
    /// The result type and its schema.
    pub returns: (TypeId, Result<String, SchemaError>),
    /// `Ok` when a model may run it with `run_harness`; else why not ("not runnable by
    /// models"): it is not `pub`, it is an entry, or a parameter is not schema-able or holds
    /// trusted text (`Str`, `UserText`, `Brief`, `TrustedPrompt`), chapter 07.
    pub runnable_by_models: Result<(), String>,
    /// What it may do and the trust sites it reaches.
    pub summary: Summary,
}

/// The summary of one checked package.
#[derive(Clone, Debug, Default)]
pub struct HarnessCheck {
    /// Whether the package loaded and checked without an error.
    pub clean: bool,
    /// The union of what every item of the package may do.
    pub effects: Effects,
    /// The tools the package names, sorted.
    pub tools: Vec<String>,
    /// Every trust site of the package (every module it imports included), in module and
    /// source order.
    pub trust_sites: Vec<SiteInfo>,
    /// The names its `meta.harness` calls load; `None` for a computed name.
    pub loads: Vec<Option<String>>,
    /// Every harness of the entry module and of the modules it imports.
    pub harnesses: Vec<HarnessInfo>,
}

impl HarnessCheck {
    /// The summary's line for writes, commands, network and delegation, as chapter 07's
    /// approval prompt words it: "any tool of the frame" when the package runs tools a model
    /// chooses, else the tools named.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (kind, word) in [
            (Effects::WRITE, "writes"),
            (Effects::COMMAND, "runs commands"),
            (Effects::NETWORK, "reaches the network"),
            (Effects::DELEGATE, "delegates"),
        ] {
            if self.effects.contains(kind) {
                parts.push(word.to_owned());
            }
        }
        if self.effects.contains(Effects::ANY_TOOL) {
            parts.push("any tool of the frame".to_owned());
        } else if !self.tools.is_empty() {
            parts.push(format!("tools: {}", self.tools.join(", ")));
        }
        if parts.is_empty() {
            "no writes, commands, network or delegation".to_owned()
        } else {
            parts.join("; ")
        }
    }
}

impl Checked {
    /// The summary of the package `loaded` (which this checked).
    #[must_use]
    pub fn harness_check(&mut self, loaded: &Loaded) -> HarnessCheck {
        let mut out = HarnessCheck {
            clean: loaded.is_clean() && self.is_clean(),
            ..HarnessCheck::default()
        };
        let mut tools: BTreeSet<String> = BTreeSet::new();
        for ((module, index), summary) in &self.summaries {
            // `test` blocks run only against a scripted host, never in a session.
            let test = loaded.modules.get(module.0).is_some_and(|m| {
                matches!(
                    m.ast.items.get(*index).map(|i| &i.kind),
                    Some(ItemKind::Test(_))
                )
            });
            if !test {
                out.effects = out.effects.union(summary.effects);
                tools.extend(summary.tools.iter().cloned());
            }
        }
        out.tools = tools.into_iter().collect();
        out.trust_sites = self
            .trust_sites
            .iter()
            .map(|s| site_info(loaded, &self.types, s))
            .collect();
        out.loads = self.loads.iter().map(|l| l.name.clone()).collect();
        for (m, module) in loaded.modules.iter().enumerate() {
            for (index, item) in module.ast.items.iter().enumerate() {
                let ItemKind::Harness(decl) = &item.kind else {
                    continue;
                };
                let Some(Some(ty)) = self.tables.get(m).map(|t| t.items[index]) else {
                    continue;
                };
                let Ty::Harness(ps, ret) = self.types.get(ty).clone() else {
                    continue;
                };
                let entry = item.attrs.iter().find_map(|a| match a.name.name.as_str() {
                    "turn" => Some(Entry::Turn),
                    "task" => Some(Entry::Task),
                    _ => None,
                });
                let mut params = Vec::with_capacity(ps.len());
                let mut runnable: Result<(), String> = if !item.public {
                    Err("it is not `pub`".to_owned())
                } else if entry.is_some() {
                    Err("it is an entry".to_owned())
                } else {
                    Ok(())
                };
                for (p, t) in decl.params.iter().zip(ps.iter()) {
                    let schema = self.schema(*t);
                    if runnable.is_ok() {
                        if schema.is_err() {
                            runnable = Err(format!(
                                "the parameter `{}` is not schema-able",
                                p.name.name
                            ));
                        } else if self.types.holds_trusted_text(*t) {
                            runnable = Err(format!(
                                "the parameter `{}` holds trusted text (`{}`): take it \
                                 untrusted and convert it inside with an audited `trust` or \
                                 `as_task`",
                                p.name.name,
                                self.types.display(*t)
                            ));
                        }
                    }
                    params.push((p.name.name.clone(), *t, schema));
                }
                let returns = (ret, self.schema(ret));
                out.harnesses.push(HarnessInfo {
                    module: ModuleId(m),
                    name: decl.name.name.clone(),
                    span: decl.name.span,
                    public: item.public,
                    entry,
                    params,
                    returns,
                    runnable_by_models: runnable,
                    summary: self
                        .summary(ModuleId(m), index)
                        .cloned()
                        .unwrap_or_default(),
                });
            }
        }
        out
    }
}

fn site_info(loaded: &Loaded, types: &crate::Types, s: &crate::TrustSite) -> SiteInfo {
    let module = &loaded.modules[s.module.0];
    SiteInfo {
        package: module.package.to_string(),
        file: module.path.to_string(),
        line: s.span.line,
        col: s.span.col,
        kind: s.kind,
        ty: types.display(s.ty),
        reason: s.reason.clone(),
    }
}

/// The result of [`harnesses_check`].
#[derive(Clone, Debug)]
pub struct PackageCheck {
    /// The package as loaded (loading and syntax diagnostics).
    pub loaded: Loaded,
    /// The package as checked (the checker's diagnostics and tables).
    pub checked: Checked,
    /// The package's own summary.
    pub summary: HarnessCheck,
    /// Every trust site of the package and of every package it can load, transitively, sorted.
    pub trust_sites: Vec<SiteInfo>,
    /// The packages loaded through `meta.harness` names, in the order first met.
    pub loaded_packages: Vec<String>,
    /// `false` when some load could not be followed (a computed name, a package the registry
    /// does not have, or more than [`MAX_LOADED_PACKAGES`]): the list may then be missing
    /// sites, and a caller deciding `ratio.trust` should treat the package as having
    /// unknown sites.
    pub complete: bool,
}

/// The package a `meta.harness` name loads: the name without its last dotted part (the
/// harness), or the whole name when it has none.
fn package_of(name: &str) -> Option<QualifiedName> {
    let (body, range) = match name.split_once('@') {
        Some((b, r)) => (b, Some(r)),
        None => (name, None),
    };
    let package = match body.rsplit_once('.') {
        Some((p, _)) if !p.ends_with(':') && !p.is_empty() => p,
        _ => body,
    };
    let text = match range {
        Some(r) => format!("{package}@{r}"),
        None => package.to_owned(),
    };
    QualifiedName::parse(&text).ok()
}

/// Loads, checks and summarises the package `name`, and follows its `meta.harness` loads
/// (transitively, at most [`MAX_LOADED_PACKAGES`] packages) to list every trust site it can
/// reach.
#[must_use]
pub fn harnesses_check(
    registry: &dyn Registry,
    builtin: &dyn Registry,
    name: &QualifiedName,
) -> PackageCheck {
    let loader = Loader::new(registry, builtin);
    let package = loader.load(name);
    let mut checked = check(&package);
    let summary = checked.harness_check(&package);
    let mut sites: BTreeSet<SiteInfo> = summary.trust_sites.iter().cloned().collect();
    let mut complete = summary.clean;
    let mut seen: Vec<String> = vec![name.to_string()];
    let mut loaded_packages = Vec::new();
    let mut queue: Vec<Option<String>> = summary.loads.clone();
    let mut next = 0;
    while next < queue.len() {
        let load = queue[next].clone();
        next += 1;
        let Some(target) = load.as_deref().and_then(package_of) else {
            complete = false;
            continue;
        };
        let key = target.to_string();
        if seen.contains(&key) {
            continue;
        }
        if seen.len() > MAX_LOADED_PACKAGES {
            complete = false;
            break;
        }
        seen.push(key.clone());
        let other = loader.load(&target);
        if other.entry.is_none() {
            complete = false;
            continue;
        }
        let mut other_checked = check(&other);
        let other_summary = other_checked.harness_check(&other);
        complete &= other_summary.clean;
        sites.extend(other_summary.trust_sites.iter().cloned());
        queue.extend(other_summary.loads.iter().cloned());
        loaded_packages.push(key);
    }
    PackageCheck {
        loaded: package,
        checked,
        summary,
        trust_sites: sites.into_iter().collect(),
        loaded_packages,
        complete,
    }
}
