//! A module's top-level names: its items, what its imports bind, and the prelude behind them.
//!
//! Name resolution is lexical (chapter 03): a local binding shadows an import, which shadows the
//! prelude. This is the module level of it; the checker (stage 4) puts local scopes in front,
//! and resolves the names that need types to resolve (contextual variants and tool names,
//! chapter 02 rule 10; method calls).
//!
//! An import binds its alias, or without an alias and without a name list the last word of its
//! name (`std:debate` binds `debate`, `user:critics.strict` binds `strict`) or its file name
//! (`"./lib/critics.ratio"` binds `critics`). A name list (`{ a, b }`) binds each public item it
//! names, and the module only if it also has an alias.

use std::collections::{BTreeMap, HashMap};

use ratio_syntax::Keyword;
use ratio_syntax::Span;
use ratio_syntax::ast::{Ident, ImportPath, ItemKind};

use crate::diag::{Code, Diagnostic};
use crate::loader::{LoadedModule, ModuleId};
use crate::prelude::{self, PreludeName};

/// What a top-level name of a module refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// An item of this module, by its index in the module's items.
    Item(usize),
    /// A module an import brought in (`None` if the import failed; its diagnostic says why).
    Module {
        import: usize,
        module: Option<ModuleId>,
    },
    /// A public item of another module, named in an import's list (`None` parts if the import
    /// or the name failed).
    Imported {
        import: usize,
        module: Option<ModuleId>,
        item: Option<usize>,
    },
}

/// What a name resolves to at module level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Binding(Binding),
    Prelude(PreludeName),
}

/// A module's top-level names.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    names: BTreeMap<String, (Binding, Span)>,
}

impl Scope {
    /// What the module itself binds to `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Binding> {
        self.names.get(name).map(|(binding, _)| *binding)
    }

    /// `name` at module level: the module's own bindings, then the prelude.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Resolution> {
        match self.get(name) {
            Some(binding) => Some(Resolution::Binding(binding)),
            None => prelude::lookup(name).map(Resolution::Prelude),
        }
    }

    /// Every name the module binds, in order, with where it was bound.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Binding, Span)> {
        self.names
            .iter()
            .map(|(name, (binding, span))| (name.as_str(), *binding, *span))
    }
}

/// The scopes of every module (empty for one the load did not reach), reporting duplicate,
/// unknown and private names.
pub(crate) fn build(
    modules: &[LoadedModule],
    reached: &[bool],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Scope> {
    let exports: Vec<HashMap<&str, (usize, bool)>> = modules.iter().map(exports).collect();
    let mut scopes = Vec::with_capacity(modules.len());
    for (index, module) in modules.iter().enumerate() {
        let mut builder = Builder {
            id: ModuleId(index),
            scope: Scope::default(),
            diagnostics: &mut *diagnostics,
        };
        if reached[index] && module.syntax.is_empty() {
            builder.module(module, modules, &exports);
        }
        scopes.push(builder.scope);
    }
    scopes
}

/// A module's items by name (the first of each name): index and whether it is `pub`.
fn exports(module: &LoadedModule) -> HashMap<&str, (usize, bool)> {
    let mut found = HashMap::new();
    for (index, item) in module.ast.items.iter().enumerate() {
        if let Some(name) = item_name(&item.kind) {
            found
                .entry(name.name.as_str())
                .or_insert((index, item.public));
        }
    }
    found
}

/// The name a declaration defines (imports, `state` and tests define none).
fn item_name(kind: &ItemKind) -> Option<&Ident> {
    match kind {
        ItemKind::Type(decl) => Some(&decl.name),
        ItemKind::Enum(decl) => Some(&decl.name),
        ItemKind::Const(decl) => Some(&decl.name),
        ItemKind::Fn(decl) => Some(&decl.name),
        ItemKind::Harness(decl) => Some(&decl.name),
        ItemKind::Prompt(decl) => Some(&decl.name),
        ItemKind::Import(_) | ItemKind::State(_) | ItemKind::Test(_) => None,
    }
}

fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
        && Keyword::from_word(word).is_none()
}

struct Builder<'d> {
    id: ModuleId,
    scope: Scope,
    diagnostics: &'d mut Vec<Diagnostic>,
}

impl Builder<'_> {
    fn module(
        &mut self,
        module: &LoadedModule,
        modules: &[LoadedModule],
        exports: &[HashMap<&str, (usize, bool)>],
    ) {
        for (index, item) in module.ast.items.iter().enumerate() {
            let ItemKind::Import(import) = &item.kind else {
                if let Some(name) = item_name(&item.kind) {
                    self.bind(&name.name, name.span, Binding::Item(index));
                }
                continue;
            };
            if item.public {
                self.report(
                    Code::PubImport,
                    "`pub` does not apply to an import: imports are not re-exported".to_owned(),
                    item.span,
                );
            }
            let target = module.imports.get(&index).copied();
            let clean = target.filter(|t| modules[t.0].syntax.is_empty());
            let binding = Binding::Module {
                import: index,
                module: target,
            };
            if let Some(alias) = &import.alias {
                self.bind(&alias.name, alias.span, binding);
            } else if import.names.is_none() {
                let (word, span) = match &import.path {
                    ImportPath::File(lit) => {
                        let Some(text) = crate::plain_text(lit) else {
                            continue; // reported by the loader
                        };
                        let file = text.rsplit('/').next().unwrap_or(&text).to_owned();
                        (
                            file.strip_suffix(".ratio").unwrap_or(&file).to_owned(),
                            lit.span,
                        )
                    }
                    ImportPath::Source(name) => {
                        let last = name.parts.last().and_then(|part| part.last());
                        (last.map_or_else(String::new, |i| i.name.clone()), name.span)
                    }
                };
                if is_identifier(&word) {
                    self.bind(&word, span, binding);
                } else {
                    self.report(
                        Code::NoBindingName,
                        format!("`{word}` is not a name this import can bind; add `as <name>`"),
                        span,
                    );
                }
            }
            for name in import.names.iter().flatten() {
                let item = clean.and_then(|t| match exports[t.0].get(name.name.as_str()) {
                    Some(&(found, true)) => Some(found),
                    Some(&(_, false)) => {
                        let what = modules[t.0].describe();
                        self.report(
                            Code::PrivateImportedName,
                            format!("`{}` is not `pub` in {what}", name.name),
                            name.span,
                        );
                        None
                    }
                    None => {
                        let what = modules[t.0].describe();
                        self.report(
                            Code::UnknownImportedName,
                            format!("{what} defines no `{}`", name.name),
                            name.span,
                        );
                        None
                    }
                });
                let binding = Binding::Imported {
                    import: index,
                    module: target,
                    item,
                };
                self.bind(&name.name, name.span, binding);
            }
        }
    }

    fn bind(&mut self, name: &str, span: Span, binding: Binding) {
        if let Some((_, first)) = self.scope.names.get(name) {
            let message = format!(
                "`{name}` is already defined at {}:{}",
                first.line, first.col
            );
            self.report(Code::DuplicateName, message, span);
            return;
        }
        self.scope.names.insert(name.to_owned(), (binding, span));
        if let Some(PreludeName::Module(module)) = prelude::lookup(name) {
            self.report(
                Code::ShadowsStandardModule,
                format!("`{name}` shadows the standard module `{module}`"),
                span,
            );
        }
    }

    fn report(&mut self, code: Code, message: String, span: Span) {
        self.diagnostics.push(Diagnostic {
            code,
            message,
            at: Some((self.id, span)),
        });
    }
}
