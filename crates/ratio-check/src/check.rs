//! The checker's state, its passes over the items, type expressions and name lookup.
//!
//! The passes, over every checked module and the built-in sources:
//!
//! 1. **Declare**: each enum gets an [`EnumId`] and its parameters, so declarations may refer
//!    to each other in any order (enums are nominal, so recursion through an enum is fine).
//! 2. **Payloads**: each enum's variant payloads are lowered; type aliases are lowered on first
//!    use, with a cycle check (`C0018`).
//! 3. **Signatures**: each function, harness and prompt declaration gets its signature.
//! 4. **Bodies**: constants (also on first use, when unannotated), `state`, harness
//!    configurations and bodies, functions, prompts and tests (see `expr`).
//! 5. **Effects**: which functions are pure, and whether constant expressions call anything
//!    that is not (`C0016`), and constant cycles (`C0017`) (see `effects`).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ratio_names::{Binding, Loaded, ModuleId, prelude};
use ratio_syntax::Span;
use ratio_syntax::ast::{self, Ident, ItemKind, TypeKind};

use crate::builtins::{self, Primitive, Traits};
use crate::diag::{Code, Diagnostic};
use crate::effects::EffectLog;
use crate::relate::Memo;
use crate::tables::{ItemRef, Key, ModuleTable, Origin};
use crate::types::{Bound, EnumDef, EnumId, Field, ParamId, TypeId, Types, VariantDef};

/// A parsed built-in source and its names.
pub(crate) struct StdModule {
    pub(crate) name: &'static str,
    pub(crate) module: ast::Module,
    pub(crate) scope: HashMap<String, usize>,
}

/// The built-in sources, parsed once per check.
pub(crate) fn std_modules(internal: &mut Vec<String>) -> Vec<StdModule> {
    let mut all = Vec::with_capacity(builtins::MODULES.len() + 1);
    let sources =
        std::iter::once(("prelude", builtins::PRELUDE)).chain(builtins::MODULES.iter().copied());
    for (name, source) in sources {
        let parsed = ratio_syntax::parse(source);
        for d in &parsed.diagnostics {
            internal.push(format!("built-in `{name}`: {d:?}"));
        }
        let mut scope = HashMap::new();
        for (index, item) in parsed.module.items.iter().enumerate() {
            if let Some(ident) = item_name(&item.kind) {
                let name = builtins::builtin_name(&ident.name).to_owned();
                scope.entry(name).or_insert(index);
            }
        }
        all.push(StdModule {
            name,
            module: parsed.module,
            scope,
        });
    }
    all
}

/// The name a declaration defines.
pub(crate) fn item_name(kind: &ItemKind) -> Option<&Ident> {
    match kind {
        ItemKind::Type(d) => Some(&d.name),
        ItemKind::Enum(d) => Some(&d.name),
        ItemKind::Const(d) => Some(&d.name),
        ItemKind::Fn(d) => Some(&d.name),
        ItemKind::Harness(d) => Some(&d.name),
        ItemKind::Prompt(d) => Some(&d.name),
        ItemKind::Import(_) | ItemKind::State(_) | ItemKind::Test(_) => None,
    }
}

/// The name a declaration's field or variant has: as written, except that the built-in
/// sources write a name that is a keyword (`type`, `harness`) with a trailing `_`, which is
/// dropped, as for built-in functions and parameters ([`builtins::builtin_name`]).
fn declared_name(origin: Origin, name: &str) -> &str {
    if matches!(origin, Origin::Std(_)) {
        builtins::builtin_name(name)
    } else {
        name
    }
}

/// A lazily computed type, with a marker for "being computed" to find cycles.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Lazy {
    Todo,
    Busy,
    Done(TypeId),
}

/// What kind of callable a signature belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FnKind {
    Fn,
    Harness,
    Prompt,
    /// A built-in, pure or not.
    Builtin {
        pure: bool,
    },
}

/// A parameter of a declared callable.
#[derive(Clone, Debug)]
pub(crate) struct ParamSig {
    pub(crate) name: String,
    pub(crate) ty: TypeId,
    pub(crate) has_default: bool,
}

/// The signature of a declared function, harness, prompt or built-in.
#[derive(Clone, Debug)]
pub(crate) struct FnSig {
    pub(crate) type_params: Vec<ParamId>,
    pub(crate) params: Vec<ParamSig>,
    pub(crate) ret: TypeId,
    pub(crate) kind: FnKind,
    /// Parameter indexes by name (the first of a repeated name).
    pub(crate) by_name: HashMap<String, usize>,
    /// What a built-in's attributes say about it (the default for user declarations).
    pub(crate) traits: Traits,
}

impl FnSig {
    /// The index of the parameter called `name`.
    pub(crate) fn param_index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
}

/// What the checker knows about an item.
#[derive(Clone, Debug)]
pub(crate) enum ItemSig {
    None,
    Alias { params: Vec<ParamId>, body: Lazy },
    Enum(EnumId),
    Const(Lazy),
    Fn(Rc<FnSig>),
}

/// The checker.
pub(crate) struct Checker<'a> {
    pub(crate) loaded: &'a Loaded,
    pub(crate) std: &'a [StdModule],
    pub(crate) types: Types,
    pub(crate) memo: Memo,
    pub(crate) user_sigs: Vec<Vec<ItemSig>>,
    pub(crate) std_sigs: Vec<Vec<ItemSig>>,
    pub(crate) tables: Vec<ModuleTable>,
    pub(crate) diags: Vec<Diagnostic>,
    pub(crate) internal: Vec<String>,
    /// Which modules are checked (reached and parsed without errors).
    pub(crate) checked: Vec<bool>,
    /// The current nesting of the checker's recursion, and its bound.
    pub(crate) depth: usize,
    pub(crate) max_depth: usize,
    pub(crate) deep_reported: bool,
    /// An exhaustiveness check went over its bound; later ones are skipped.
    pub(crate) exhaust_refused: bool,
    pub(crate) fuel_reported: bool,
    /// The effect logs of user items, by item; harness configurations under their harness's
    /// reference in `configs`.
    pub(crate) logs: HashMap<ItemRef, EffectLog>,
    pub(crate) config_logs: HashMap<ItemRef, EffectLog>,
    /// The prompt declarations' inferred result types.
    pub(crate) prompt_types: HashMap<ItemRef, Lazy>,
    /// The `state` declarations' types, lowered once (by the declaration's body check or the
    /// first `state.*` call that needs it).
    pub(crate) state_types: HashMap<ItemRef, TypeId>,
    /// The nesting of on-demand constant and prompt checks.
    pub(crate) demand_depth: usize,
    /// For each origin, the enums visible at its top level by variant name (built on use).
    pub(crate) variant_index: HashMap<Origin, HashMap<String, Vec<EnumId>>>,
    /// Trust sites, `meta.harness` loads and harness-value calls, for the analysis.
    pub(crate) records: crate::analysis::Records,
    /// The declaration of each enum, by [`EnumId`] (for stage 6's lowering).
    pub(crate) enum_items: Vec<ItemRef>,
}

/// How deep on-demand checks of constants and prompts (and alias lowering) may nest.
pub(crate) const MAX_DEMAND_DEPTH: usize = 64;

impl<'a> Checker<'a> {
    pub(crate) fn new(loaded: &'a Loaded, std: &'a [StdModule], max_depth: usize) -> Self {
        let checked: Vec<bool> = loaded.modules.iter().map(|m| m.syntax.is_empty()).collect();
        Checker {
            loaded,
            std,
            types: Types::new(),
            memo: Memo::default(),
            user_sigs: loaded
                .modules
                .iter()
                .map(|m| vec![ItemSig::None; m.ast.items.len()])
                .collect(),
            std_sigs: std
                .iter()
                .map(|m| vec![ItemSig::None; m.module.items.len()])
                .collect(),
            tables: loaded
                .modules
                .iter()
                .map(|m| ModuleTable {
                    items: vec![None; m.ast.items.len()],
                    ..ModuleTable::default()
                })
                .collect(),
            diags: Vec::new(),
            internal: Vec::new(),
            checked,
            depth: 0,
            max_depth,
            deep_reported: false,
            exhaust_refused: false,
            fuel_reported: false,
            logs: HashMap::new(),
            config_logs: HashMap::new(),
            prompt_types: HashMap::new(),
            state_types: HashMap::new(),
            demand_depth: 0,
            variant_index: HashMap::new(),
            records: crate::analysis::Records::default(),
            enum_items: Vec::new(),
        }
    }

    /// Every origin the checker declares items for.
    pub(crate) fn origins(&self) -> Vec<Origin> {
        let std = (0..self.std.len()).map(Origin::Std);
        let user = (0..self.loaded.modules.len())
            .filter(|i| self.checked[*i])
            .map(|i| Origin::Module(ModuleId(i)));
        std.chain(user).collect()
    }

    pub(crate) fn items(&self, origin: Origin) -> &'a [ast::Item] {
        match origin {
            Origin::Module(m) => &self.loaded.modules[m.0].ast.items,
            Origin::Std(s) => &self.std[s].module.items,
        }
    }

    pub(crate) fn item(&self, r: ItemRef) -> &'a ast::Item {
        &self.items(r.origin)[r.index]
    }

    pub(crate) fn sig(&self, r: ItemRef) -> &ItemSig {
        match r.origin {
            Origin::Module(m) => &self.user_sigs[m.0][r.index],
            Origin::Std(s) => &self.std_sigs[s][r.index],
        }
    }

    pub(crate) fn set_sig(&mut self, r: ItemRef, sig: ItemSig) {
        match r.origin {
            Origin::Module(m) => self.user_sigs[m.0][r.index] = sig,
            Origin::Std(s) => self.std_sigs[s][r.index] = sig,
        }
    }

    /// Reports a diagnostic in a user module; one in a built-in source is a bug, kept for the
    /// tests.
    pub(crate) fn report(&mut self, origin: Origin, code: Code, span: Span, message: String) {
        match origin {
            Origin::Module(module) => self.diags.push(Diagnostic {
                code,
                message,
                module,
                span,
            }),
            Origin::Std(s) => self.internal.push(format!(
                "built-in `{}` {}:{}: {code} {message}",
                self.std[s].name, span.line, span.col
            )),
        }
    }

    /// Enters one level of recursion; `false` (after reporting once) when too deep.
    pub(crate) fn enter(&mut self, origin: Origin, span: Span) -> bool {
        if self.depth >= self.max_depth {
            if !self.deep_reported {
                self.deep_reported = true;
                self.report(
                    origin,
                    Code::TooComplex,
                    span,
                    "this is nested too deeply for the checker".to_owned(),
                );
            }
            return false;
        }
        self.depth += 1;
        true
    }

    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Reports a type that grew past the depth limit, or the work budget running out, at
    /// `span`, if either happened since the last call.
    pub(crate) fn check_limits(&mut self, origin: Origin, span: Span) {
        if std::mem::take(&mut self.types.too_deep) {
            self.report(
                origin,
                Code::TooComplex,
                span,
                format!(
                    "this type is nested deeper than {} levels",
                    crate::types::MAX_TYPE_DEPTH
                ),
            );
        }
        if self.types.fuel == 0 && !self.fuel_reported {
            self.fuel_reported = true;
            self.report(
                origin,
                Code::TooComplex,
                span,
                "the program's types are too large to check within the checker's work budget"
                    .to_owned(),
            );
        }
    }

    pub(crate) fn table(&mut self, module: ModuleId) -> &mut ModuleTable {
        &mut self.tables[module.0]
    }

    pub(crate) fn record_type(&mut self, module: ModuleId, span: Span, ty: TypeId) {
        self.tables[module.0].types.insert(Key::from(span), ty);
    }

    // ---- pass 1 and 2: enums ----

    pub(crate) fn declare_enums(&mut self) {
        for origin in self.origins() {
            for (index, item) in self.items(origin).iter().enumerate() {
                let ItemKind::Enum(decl) = &item.kind else {
                    continue;
                };
                if let (Origin::Module(_), Some(first)) = (origin, decl.params.first()) {
                    self.report(
                        origin,
                        Code::GenericEnum,
                        first.span,
                        format!(
                            "the enum `{}` cannot take type parameters: only functions and type \
                             aliases are generic",
                            decl.name.name
                        ),
                    );
                }
                let params = self.declare_params(origin, &decl.params, false);
                let mut variants: Vec<VariantDef> = Vec::new();
                let mut seen: HashSet<&str> = HashSet::new();
                for v in &decl.variants {
                    if !seen.insert(&v.name.name) {
                        self.report(
                            origin,
                            Code::Duplicate,
                            v.name.span,
                            format!("the variant `{}` is declared twice", v.name.name),
                        );
                        continue;
                    }
                    variants.push(VariantDef {
                        name: declared_name(origin, &v.name.name).to_owned(),
                        payload: None,
                        has_payload: v.fields.is_some(),
                    });
                }
                let id = self.types.add_enum(EnumDef::new(
                    decl.name.name.clone(),
                    params.iter().map(|(_, p)| *p).collect(),
                    variants,
                ));
                let r = ItemRef { origin, index };
                debug_assert_eq!(self.enum_items.len(), id.0 as usize);
                self.enum_items.push(r);
                self.set_sig(r, ItemSig::Enum(id));
                // `ContextMode`'s `none` (chapter 04) is a keyword, so its source cannot
                // declare it; the prelude adds it here.
                if origin == Origin::Std(0) && decl.name.name == "ContextMode" {
                    self.types.enum_def_mut(id).add_unit_variant("none");
                }
                let ty = {
                    let args = params.iter().map(|(_, p)| self.types.param(*p)).collect();
                    self.types.enum_type(id, args)
                };
                if let Origin::Module(m) = origin {
                    self.tables[m.0].items[index] = Some(ty);
                }
            }
        }
        for origin in self.origins() {
            for (index, item) in self.items(origin).iter().enumerate() {
                match &item.kind {
                    ItemKind::Type(decl) => {
                        let params = self.declare_params(origin, &decl.params, false);
                        let params = params.into_iter().map(|(_, p)| p).collect();
                        self.set_sig(
                            ItemRef { origin, index },
                            ItemSig::Alias {
                                params,
                                body: Lazy::Todo,
                            },
                        );
                    }
                    ItemKind::Const(_) => {
                        self.set_sig(ItemRef { origin, index }, ItemSig::Const(Lazy::Todo));
                    }
                    _ => {}
                }
            }
        }
    }

    /// Type parameters, each a fresh [`ParamId`]; a repeated name is reported.
    pub(crate) fn declare_params(
        &mut self,
        origin: Origin,
        params: &[Ident],
        builtin: bool,
    ) -> Vec<(String, ParamId)> {
        let mut out: Vec<(String, ParamId)> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for p in params {
            if !seen.insert(&p.name) {
                self.report(
                    origin,
                    Code::Duplicate,
                    p.span,
                    format!("the type parameter `{}` is declared twice", p.name),
                );
                continue;
            }
            let bound = if builtin {
                builtins::bound(&p.name)
            } else {
                Bound::None
            };
            out.push((p.name.clone(), self.types.add_param(&p.name, bound)));
        }
        out
    }

    pub(crate) fn lower_payloads(&mut self) {
        for origin in self.origins() {
            for (index, item) in self.items(origin).iter().enumerate() {
                let ItemKind::Enum(decl) = &item.kind else {
                    continue;
                };
                let ItemSig::Enum(id) = *self.sig(ItemRef { origin, index }) else {
                    continue;
                };
                let params: Vec<(String, ParamId)> = decl
                    .params
                    .iter()
                    .map(|p| p.name.clone())
                    .zip(self.types.enum_def(id).params.iter().copied())
                    .collect();
                for v in &decl.variants {
                    let Some(fields) = &v.fields else {
                        continue;
                    };
                    let payload = self.lower_record(origin, &params, fields, v.span);
                    let def = self.types.enum_def_mut(id);
                    let name = declared_name(origin, &v.name.name);
                    if let Some(slot) = def.variants.iter_mut().find(|w| w.name == name) {
                        slot.payload.get_or_insert(payload);
                    }
                }
            }
        }
    }

    // ---- pass 3: signatures ----

    pub(crate) fn signatures(&mut self) {
        for origin in self.origins() {
            let builtin = matches!(origin, Origin::Std(_));
            for (index, item) in self.items(origin).iter().enumerate() {
                let r = ItemRef { origin, index };
                let mut sig = match &item.kind {
                    ItemKind::Fn(decl) => {
                        let tparams = self.declare_params(origin, &decl.type_params, builtin);
                        let params = self.param_sigs(origin, &tparams, &decl.params);
                        let ret = match &decl.ret {
                            Some(t) => self.lower_type(origin, &tparams, t),
                            None => TypeId::UNIT,
                        };
                        let traits = if builtin {
                            Traits::of(&item.attrs, &mut self.internal)
                        } else {
                            Traits::default()
                        };
                        let kind = if builtin {
                            FnKind::Builtin {
                                pure: item.attrs.iter().any(|a| a.name.name == "pure"),
                            }
                        } else {
                            FnKind::Fn
                        };
                        FnSig {
                            type_params: tparams.into_iter().map(|(_, p)| p).collect(),
                            params,
                            ret,
                            kind,
                            by_name: HashMap::new(),
                            traits,
                        }
                    }
                    ItemKind::Harness(decl) => {
                        let params = self.param_sigs(origin, &[], &decl.params);
                        let ret = if let Some(t) = &decl.ret {
                            self.lower_type(origin, &[], t)
                        } else {
                            self.report(
                                origin,
                                Code::HarnessReturn,
                                decl.name.span,
                                format!(
                                    "the harness `{}` needs a return type: a harness \
                                     signature is a contract",
                                    decl.name.name
                                ),
                            );
                            TypeId::ERROR
                        };
                        FnSig {
                            type_params: Vec::new(),
                            params,
                            ret,
                            kind: FnKind::Harness,
                            by_name: HashMap::new(),
                            traits: Traits::default(),
                        }
                    }
                    ItemKind::Prompt(decl) => {
                        let params = self.param_sigs(origin, &[], &decl.params);
                        self.prompt_types.insert(r, Lazy::Todo);
                        FnSig {
                            type_params: Vec::new(),
                            params,
                            ret: TypeId::PROMPT,
                            kind: FnKind::Prompt,
                            by_name: HashMap::new(),
                            traits: Traits::default(),
                        }
                    }
                    _ => continue,
                };
                if let Origin::Module(m) = origin {
                    let params = sig.params.iter().map(|p| p.ty).collect();
                    let ty = if sig.kind == FnKind::Harness {
                        self.types.harness(params, sig.ret)
                    } else {
                        self.types.function(params, sig.ret)
                    };
                    self.tables[m.0].items[index] = Some(ty);
                }
                if builtin {
                    for p in &mut sig.params {
                        p.name = builtins::builtin_name(&p.name).to_owned();
                    }
                }
                sig.by_name = sig
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.name.clone(), i))
                    .collect();
                self.set_sig(r, ItemSig::Fn(Rc::new(sig)));
            }
        }
    }

    fn param_sigs(
        &mut self,
        origin: Origin,
        tparams: &[(String, ParamId)],
        params: &[ast::Param],
    ) -> Vec<ParamSig> {
        let mut out: Vec<ParamSig> = Vec::with_capacity(params.len());
        let mut seen: HashSet<&str> = HashSet::new();
        for p in params {
            let ty = self.lower_type(origin, tparams, &p.ty);
            if !seen.insert(&p.name.name) {
                self.report(
                    origin,
                    Code::Duplicate,
                    p.name.span,
                    format!("the parameter `{}` is declared twice", p.name.name),
                );
            }
            out.push(ParamSig {
                name: p.name.name.clone(),
                ty,
                has_default: p.default.is_some(),
            });
        }
        out
    }

    // ---- types ----

    /// Lowers a written type in `origin`'s scope with the type parameters `params`.
    pub(crate) fn lower_type(
        &mut self,
        origin: Origin,
        params: &[(String, ParamId)],
        t: &ast::Type,
    ) -> TypeId {
        if !self.enter(origin, t.span) {
            return TypeId::ERROR;
        }
        let ty = self.lower_type_inner(origin, params, t);
        self.leave();
        self.check_limits(origin, t.span);
        ty
    }

    fn lower_type_inner(
        &mut self,
        origin: Origin,
        params: &[(String, ParamId)],
        t: &ast::Type,
    ) -> TypeId {
        match &t.kind {
            TypeKind::Error => TypeId::ERROR,
            TypeKind::Named { path, args } => {
                let args: Vec<TypeId> = args
                    .iter()
                    .map(|a| self.lower_type(origin, params, a))
                    .collect();
                self.named_type(origin, params, path, args, t.span)
            }
            TypeKind::List(inner) => {
                let inner = self.lower_type(origin, params, inner);
                self.types.list(inner)
            }
            TypeKind::Record(fields) => self.lower_record(origin, params, fields, t.span),
            TypeKind::Tuple(items) => {
                if items.len() > 8 {
                    self.report(
                        origin,
                        Code::TupleArity,
                        t.span,
                        format!("a tuple has at most 8 elements, this one {}", items.len()),
                    );
                    return TypeId::ERROR;
                }
                let items = items
                    .iter()
                    .map(|a| self.lower_type(origin, params, a))
                    .collect();
                self.types.tuple(items)
            }
            TypeKind::Fn { params: ps, ret } | TypeKind::Harness { params: ps, ret } => {
                let ps = ps
                    .iter()
                    .map(|a| self.lower_type(origin, params, a))
                    .collect();
                let ret = self.lower_type(origin, params, ret);
                if matches!(t.kind, TypeKind::Fn { .. }) {
                    self.types.function(ps, ret)
                } else {
                    self.types.harness(ps, ret)
                }
            }
            TypeKind::Option(inner) => {
                let inner = self.lower_type(origin, params, inner);
                self.types.option(inner)
            }
        }
    }

    pub(crate) fn lower_record(
        &mut self,
        origin: Origin,
        params: &[(String, ParamId)],
        fields: &[ast::FieldType],
        span: Span,
    ) -> TypeId {
        let mut out: Vec<Field> = Vec::with_capacity(fields.len());
        let mut metas: Vec<(Box<str>, crate::types::FieldMeta)> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for f in fields {
            let ty = self.lower_type(origin, params, &f.ty);
            if !seen.insert(&f.name.name) {
                self.report(
                    origin,
                    Code::Duplicate,
                    f.name.span,
                    format!("the field `{}` is declared twice", f.name.name),
                );
                continue;
            }
            let meta = self.field_meta(origin, f, ty);
            let name = declared_name(origin, &f.name.name);
            metas.push((name.into(), meta));
            out.push(Field {
                name: name.into(),
                ty,
                optional: f.optional,
            });
        }
        let ty = self.types.record(out);
        if metas.iter().any(|(_, m)| !m.is_empty()) {
            metas.sort_by(|a, b| a.0.cmp(&b.0));
            self.types
                .set_field_meta(ty, metas.into_iter().map(|(_, m)| m).collect());
        }
        self.check_limits(origin, span);
        ty
    }

    fn named_type(
        &mut self,
        origin: Origin,
        params: &[(String, ParamId)],
        path: &[Ident],
        args: Vec<TypeId>,
        span: Span,
    ) -> TypeId {
        let Some(first) = path.first() else {
            return TypeId::ERROR;
        };
        if path.len() > 1 {
            let found = self
                .module_of(origin, &first.name)
                .and_then(|m| self.member_item(m, &path[1].name));
            return match (found, path.len()) {
                (Some(Ok(r)), 2) if self.is_type_item(r) => self.item_type(origin, r, args, span),
                (Some(Err(r)), 2) if self.is_type_item(r) => {
                    self.private(origin, &path[1], r);
                    TypeId::ERROR
                }
                _ => self.unknown_type(origin, path, span),
            };
        }
        let name = first.name.as_str();
        if let Some((_, p)) = params.iter().rev().find(|(n, _)| n == name) {
            if !args.is_empty() {
                self.report(
                    origin,
                    Code::TypeArity,
                    span,
                    format!("the type parameter `{name}` takes no type arguments"),
                );
            }
            return self.types.param(*p);
        }
        match self.scope_item(origin, name) {
            Some(Ok(r)) if self.is_type_item(r) => return self.item_type(origin, r, args, span),
            Some(Ok(_)) => {
                self.report(
                    origin,
                    Code::UnknownType,
                    span,
                    format!("`{name}` is not a type"),
                );
                return TypeId::ERROR;
            }
            Some(Err(())) => return TypeId::ERROR,
            None => {}
        }
        if let Some(prim) = builtins::primitive(name) {
            return self.primitive_type(origin, prim, name, &args, span);
        }
        if let Some(&index) = self.std[0].scope.get(name) {
            let r = ItemRef {
                origin: Origin::Std(0),
                index,
            };
            return self.item_type(origin, r, args, span);
        }
        self.unknown_type(origin, path, span)
    }

    fn unknown_type(&mut self, origin: Origin, path: &[Ident], span: Span) -> TypeId {
        let name: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
        self.report(
            origin,
            Code::UnknownType,
            span,
            format!("no type named `{}`", name.join(".")),
        );
        TypeId::ERROR
    }

    pub(crate) fn private(&mut self, origin: Origin, name: &Ident, _r: ItemRef) {
        self.report(
            origin,
            Code::Private,
            name.span,
            format!("`{}` is not `pub` in its module", name.name),
        );
    }

    fn primitive_type(
        &mut self,
        origin: Origin,
        prim: Primitive,
        name: &str,
        args: &[TypeId],
        span: Span,
    ) -> TypeId {
        if args.len() != prim.arity() {
            self.report(
                origin,
                Code::TypeArity,
                span,
                format!(
                    "`{name}` takes {} type argument{}, not {}",
                    prim.arity(),
                    if prim.arity() == 1 { "" } else { "s" },
                    args.len()
                ),
            );
            return TypeId::ERROR;
        }
        match prim {
            Primitive::Plain(t) => t,
            Primitive::Handle(h) => self.types.handle(h),
            Primitive::Untrusted => self.types.untrusted(args[0]),
            Primitive::Secret => self.types.secret(args[0]),
            Primitive::Task => self.types.task(args[0]),
            Primitive::Map => {
                if !self.types.is_map_key(args[0]) {
                    let shown = self.types.display(args[0]);
                    self.report(
                        origin,
                        Code::MapKey,
                        span,
                        format!(
                            "`{shown}` cannot be a map key (keys are `Str`, `Untrusted<Str>`, \
                             `Int`, `Bool` or a unit-only enum)"
                        ),
                    );
                    return TypeId::ERROR;
                }
                self.types.map(args[0], args[1])
            }
        }
    }

    pub(crate) fn is_type_item(&self, r: ItemRef) -> bool {
        matches!(self.item(r).kind, ItemKind::Type(_) | ItemKind::Enum(_))
    }

    /// The type an alias or enum item names, applied to `args`.
    pub(crate) fn item_type(
        &mut self,
        origin: Origin,
        r: ItemRef,
        args: Vec<TypeId>,
        span: Span,
    ) -> TypeId {
        let (params, body) = match self.sig(r).clone() {
            ItemSig::Enum(id) => {
                let n = self.types.enum_def(id).params.len();
                if n != args.len() {
                    self.arity(origin, r, n, args.len(), span);
                    return TypeId::ERROR;
                }
                return self.types.enum_type(id, args);
            }
            ItemSig::Alias { params, body } => (params, body),
            _ => return TypeId::ERROR,
        };
        if params.len() != args.len() {
            self.arity(origin, r, params.len(), args.len(), span);
            return TypeId::ERROR;
        }
        let body = match body {
            Lazy::Done(t) => t,
            Lazy::Busy => {
                let name = item_name(&self.item(r).kind).map_or("", |i| i.name.as_str());
                self.report(
                    origin,
                    Code::RecursiveAlias,
                    span,
                    format!("the type alias `{name}` refers to itself"),
                );
                self.set_sig(
                    r,
                    ItemSig::Alias {
                        params: params.clone(),
                        body: Lazy::Done(TypeId::ERROR),
                    },
                );
                return TypeId::ERROR;
            }
            Lazy::Todo => self.lower_alias(r, &params, span, origin),
        };
        let pairs: Vec<(ParamId, TypeId)> = params.into_iter().zip(args).collect();
        let ty = self.types.substitute(body, &pairs);
        self.check_limits(origin, span);
        ty
    }

    fn lower_alias(&mut self, r: ItemRef, params: &[ParamId], span: Span, at: Origin) -> TypeId {
        let ItemKind::Type(decl) = &self.item(r).kind else {
            return TypeId::ERROR;
        };
        if self.demand_depth >= MAX_DEMAND_DEPTH {
            self.report(
                at,
                Code::TooComplex,
                span,
                format!("type aliases are nested deeper than {MAX_DEMAND_DEPTH}"),
            );
            return TypeId::ERROR;
        }
        self.set_sig(
            r,
            ItemSig::Alias {
                params: params.to_vec(),
                body: Lazy::Busy,
            },
        );
        let named: Vec<(String, ParamId)> = decl
            .params
            .iter()
            .map(|p| p.name.clone())
            .zip(params.iter().copied())
            .collect();
        self.demand_depth += 1;
        let body = self.lower_type(r.origin, &named, &decl.ty);
        self.demand_depth -= 1;
        // A cycle found inside sets the alias to an error already.
        if let ItemSig::Alias {
            body: Lazy::Done(done),
            ..
        } = self.sig(r)
        {
            return *done;
        }
        self.set_sig(
            r,
            ItemSig::Alias {
                params: params.to_vec(),
                body: Lazy::Done(body),
            },
        );
        if let Origin::Module(m) = r.origin {
            self.tables[m.0].items[r.index] = Some(body);
        }
        body
    }

    fn arity(&mut self, origin: Origin, r: ItemRef, want: usize, got: usize, span: Span) {
        let name = item_name(&self.item(r).kind).map_or("", |i| i.name.as_str());
        self.report(
            origin,
            Code::TypeArity,
            span,
            format!("`{name}` takes {want} type argument(s), not {got}"),
        );
    }

    /// Lowers every alias, so each is checked even when unused.
    pub(crate) fn lower_aliases(&mut self) {
        for origin in self.origins() {
            for index in 0..self.items(origin).len() {
                let r = ItemRef { origin, index };
                if let ItemSig::Alias {
                    params,
                    body: Lazy::Todo,
                } = self.sig(r).clone()
                {
                    let span = self.item(r).span;
                    self.lower_alias(r, &params, span, origin);
                }
            }
        }
    }

    // ---- names ----

    /// What `name` is bound to at the top level of `origin`: an item reference, `Err(())` for
    /// a binding whose import failed (already reported), or `None` (not bound there; the
    /// prelude may still have it). Modules are not items: see [`Checker::module_of`].
    pub(crate) fn scope_item(&self, origin: Origin, name: &str) -> Option<Result<ItemRef, ()>> {
        match origin {
            Origin::Std(s) => self.std[s].scope.get(name).map(|&index| {
                Ok(ItemRef {
                    origin: Origin::Std(s),
                    index,
                })
            }),
            Origin::Module(m) => match self.loaded.modules[m.0].scope.get(name)? {
                Binding::Item(index) => Some(Ok(ItemRef { origin, index })),
                Binding::Imported {
                    module: Some(other),
                    item: Some(index),
                    ..
                } if self.checked[other.0] => Some(Ok(ItemRef {
                    origin: Origin::Module(other),
                    index,
                })),
                Binding::Imported { .. } => Some(Err(())),
                Binding::Module { .. } => None,
            },
        }
    }

    /// The module `name` names in `origin`: an import's module, or a standard module.
    pub(crate) fn module_of(&self, origin: Origin, name: &str) -> Option<ModRef> {
        if let Origin::Module(m) = origin {
            match self.loaded.modules[m.0].scope.get(name) {
                Some(Binding::Module { module, .. }) => {
                    return Some(match module {
                        Some(other) if self.checked[other.0] => ModRef::User(other),
                        _ => ModRef::Broken,
                    });
                }
                Some(_) => return None,
                None => {}
            }
        }
        match prelude::lookup(name)? {
            prelude::PreludeName::Module(module) => Some(self.std_module(module)),
            prelude::PreludeName::Function(_) => None,
        }
    }

    /// The type of the `state` declaration `r`, lowered on first use.
    pub(crate) fn state_type(&mut self, r: ItemRef) -> TypeId {
        if let Some(t) = self.state_types.get(&r) {
            return *t;
        }
        let ItemKind::State(decl) = &self.item(r).kind else {
            return TypeId::ERROR;
        };
        let ty = self.lower_type(r.origin, &[], &decl.ty);
        self.state_types.insert(r, ty);
        ty
    }

    /// The declared `state` type of the package `module` belongs to (chapter 03, "Persistent
    /// state": a package declares at most one), if it declares one in a checked module.
    pub(crate) fn package_state(&mut self, module: ModuleId) -> Option<TypeId> {
        let package = self.loaded.modules[module.0].package.clone();
        let found = self.loaded.modules.iter().enumerate().find_map(|(m, lm)| {
            if lm.package != package || !self.checked[m] {
                return None;
            }
            let index = lm
                .ast
                .items
                .iter()
                .position(|i| matches!(i.kind, ItemKind::State(_)))?;
            Some(ItemRef {
                origin: Origin::Module(ModuleId(m)),
                index,
            })
        })?;
        Some(self.state_type(found))
    }

    /// A standard module by name: its built-in source, or `Missing` without one.
    pub(crate) fn std_module(&self, name: &'static str) -> ModRef {
        match self.std.iter().position(|m| m.name == name) {
            Some(s) => ModRef::Std(s),
            None => ModRef::Missing(name),
        }
    }

    /// The member `name` of a module: `Ok` for a visible item, `Err` for a private one.
    pub(crate) fn member_item(
        &self,
        module: ModRef,
        name: &str,
    ) -> Option<Result<ItemRef, ItemRef>> {
        match module {
            ModRef::User(m) => match self.loaded.modules[m.0].scope.get(name)? {
                Binding::Item(index) => {
                    let r = ItemRef {
                        origin: Origin::Module(m),
                        index,
                    };
                    Some(if self.item(r).public { Ok(r) } else { Err(r) })
                }
                _ => None,
            },
            ModRef::Std(s) => self.std[s].scope.get(name).map(|&index| {
                Ok(ItemRef {
                    origin: Origin::Std(s),
                    index,
                })
            }),
            ModRef::Missing(_) | ModRef::Broken => None,
        }
    }
}

/// A module as the object of a member access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModRef {
    User(ModuleId),
    Std(usize),
    /// A standard module the checker has no signatures for yet.
    Missing(&'static str),
    /// An import that failed to load (already reported).
    Broken,
}

impl ModRef {
    pub(crate) fn origin(self) -> Option<Origin> {
        match self {
            ModRef::User(m) => Some(Origin::Module(m)),
            ModRef::Std(s) => Some(Origin::Std(s)),
            ModRef::Missing(_) | ModRef::Broken => None,
        }
    }
}
