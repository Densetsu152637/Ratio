//! Names, members, variants and calls.
//!
//! Name resolution is lexical (chapter 03): locals, then the module's items and imports, then
//! the prelude. Where the expected type is an enum, a bare name that is not a local and names
//! one of its unit variants is that variant; where it is a `ToolName` or `ToolSet`, a bare name
//! is a tool (chapter 02, rule 10). `x.f(a)` calls `f` from the module of `x`'s type, else a
//! function `f` in scope whose first parameter accepts `x`.
//!
//! A call of a declared function binds its type parameters locally: from the turbofish, or
//! from inference variables solved by the arguments that are not closures, then by the type
//! expected of the call, then by the closures (whose parameter types come from what is known
//! by then). A type parameter still open is reported (`C0014`) rather than guessed.

use std::collections::HashMap;
use std::rc::Rc;

use ratio_names::prelude::{self, PreludeName};
use ratio_syntax::Span;
use ratio_syntax::ast::{self, Arg, Expr, ExprKind, Ident, ItemKind, RecordLit};

use crate::body::{Body, is_closure};
use crate::builtins::{self, TrustKind};
use crate::check::{Checker, FnKind, FnSig, ItemSig, ModRef, item_name};
use crate::diag::Code;
use crate::tables::{ArgSource, CallInfo, ItemRef, Key, Origin, Res};
use crate::types::{Bound, EnumId, Erase, ParamId, Ty, TypeId};

/// What a call's callee is.
enum Callable {
    /// A declaration with a signature.
    Sig(Option<ItemRef>, Rc<FnSig>),
    /// A value of some type.
    Value(TypeId),
}

impl Checker<'_> {
    // ---- names ----

    pub(crate) fn name_expr(
        &mut self,
        b: &mut Body,
        id: &Ident,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let origin = b.origin();
        if let Some(local) = b.local(&id.name) {
            self.res(b, span, Res::Local(local));
            return self.local_info(b, local).ty;
        }
        if let Some(target) = expected.map(|t| self.strip_option(t)) {
            if let Some(enum_id) = self.types.as_enum(target)
                && let Some(index) = self.types.enum_def(enum_id).variant(&id.name)
                && !self.types.enum_def(enum_id).variants[index].has_payload
            {
                self.res(b, span, Res::Variant(enum_id, index));
                return match self.types.get(target) {
                    Ty::Untrusted(inner) => *inner,
                    _ => target,
                };
            }
            if target == TypeId::TOOL_NAME {
                self.res(b, span, Res::Tool(id.name.clone()));
                return TypeId::TOOL_NAME;
            }
        }
        match self.scope_item(origin, &id.name) {
            Some(Ok(r)) => return self.item_value(b, r, expected, span),
            Some(Err(())) => return TypeId::ERROR,
            None => {}
        }
        if self.module_of(origin, &id.name).is_some() {
            self.report(
                origin,
                Code::UnknownName,
                span,
                format!(
                    "`{}` is a module, not a value; use one of its members",
                    id.name
                ),
            );
            return TypeId::ERROR;
        }
        if let Some(PreludeName::Function(f)) = prelude::lookup(&id.name) {
            return match self.control_function(f) {
                Some(r) => self.item_value(b, r, expected, span),
                None => self.no_signature(origin, span, f),
            };
        }
        let message = if builtins::primitive(&id.name).is_some()
            || self.std[0].scope.contains_key(&id.name)
        {
            format!("`{}` is a type, not a value", id.name)
        } else {
            format!("no value named `{}` is in scope", id.name)
        };
        self.report(origin, Code::UnknownName, span, message);
        TypeId::ERROR
    }

    fn no_signature(&mut self, origin: Origin, span: Span, name: &str) -> TypeId {
        self.report(
            origin,
            Code::NoSignature,
            span,
            format!("the checker has no signature for the built-in `{name}` yet"),
        );
        TypeId::ERROR
    }

    /// The built-in `control` function `name`, if the table has it.
    fn control_function(&self, name: &str) -> Option<ItemRef> {
        let ModRef::Std(s) = self.std_module("control") else {
            return None;
        };
        self.std[s].scope.get(name).map(|&index| ItemRef {
            origin: Origin::Std(s),
            index,
        })
    }

    pub(crate) fn res(&mut self, b: &Body, span: Span, res: Res) {
        self.table(b.module).res.insert(Key::from(span), res);
    }

    /// The value an item denotes when named.
    fn item_value(
        &mut self,
        b: &mut Body,
        r: ItemRef,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let origin = b.origin();
        let item = self.item(r);
        let name = item_name(&item.kind).map_or_else(String::new, |i| i.name.clone());
        match &item.kind {
            ItemKind::Const(_) => {
                self.res(b, span, Res::Item(r));
                if matches!(r.origin, Origin::Module(_)) {
                    b.log.consts.push((r, span));
                }
                self.const_type(r, span)
            }
            ItemKind::Fn(_) | ItemKind::Harness(_) | ItemKind::Prompt(_) => {
                let Some(sig) = self.fn_sig(r) else {
                    return TypeId::ERROR;
                };
                if let Some(why) = sig.traits.value_refusal(!sig.type_params.is_empty()) {
                    // Every rule a built-in's attributes impose must hold for a value too;
                    // these are enforced only where it is called, so it may only be called.
                    self.report(
                        origin,
                        Code::NotMonomorphic,
                        span,
                        format!(
                            "`{}` must be called directly, not used as a value: {why}",
                            builtins::builtin_name(&name)
                        ),
                    );
                    return TypeId::ERROR;
                }
                self.res(b, span, Res::Item(r));
                self.log_use(b, r, &sig, span, "referring to");
                let ty = self.sig_type(r, &sig, span);
                if sig.type_params.is_empty() {
                    return ty;
                }
                let pairs: Vec<(ParamId, TypeId)> = sig
                    .type_params
                    .iter()
                    .map(|p| (*p, self.types.new_var(*p)))
                    .collect();
                let ty = self.types.substitute(ty, &pairs);
                // Instantiated by the expected type. When it does not fit, the mismatch is
                // reported here, at the instantiation with its open parameters: closing them
                // to the error type first would make the value's type an error, which the
                // caller would let through silently (`let d: fn(Json) -> Str = json.decode`
                // would launder `Json` into `Str`).
                let fits = expected.is_none_or(|want| self.types.subtype(&mut self.memo, ty, want));
                if let (false, Some(want)) = (fits, expected) {
                    let shown = self.types.resolve(ty);
                    self.mismatch(b, span, shown, want);
                    self.close_vars(b, &pairs, &name, span, true);
                    return TypeId::ERROR;
                }
                self.close_vars(b, &pairs, &name, span, false);
                // The value-form rules: `@schema(T)` holds at the type it is named with, and a
                // generic declaration named at a type holding a secret is checked against what
                // it may do once its summary is known.
                for (p, t) in &pairs {
                    let pname = self.types.param_def(*p).name.clone();
                    if sig.traits.schema.contains(&pname) {
                        self.require_schema(
                            b,
                            *t,
                            span,
                            &format!("`{name}`'s type argument `{pname}`"),
                        );
                    }
                }
                if matches!(r.origin, Origin::Module(_)) {
                    let targs = pairs.iter().map(|(_, t)| *t).collect();
                    self.records.value_uses.push((b.module, span, r, targs));
                }
                self.types.resolve(ty)
            }
            ItemKind::Type(_) | ItemKind::Enum(_) => {
                self.report(
                    origin,
                    Code::UnknownName,
                    span,
                    format!("`{name}` is a type, not a value"),
                );
                TypeId::ERROR
            }
            ItemKind::Import(_) | ItemKind::State(_) | ItemKind::Test(_) => TypeId::ERROR,
        }
    }

    /// The function (or harness) type of a declaration.
    fn sig_type(&mut self, r: ItemRef, sig: &FnSig, span: Span) -> TypeId {
        let params = sig.params.iter().map(|p| p.ty).collect();
        let ret = if sig.kind == FnKind::Prompt {
            self.prompt_type(r, span)
        } else {
            sig.ret
        };
        if sig.kind == FnKind::Harness {
            self.types.harness(params, ret)
        } else {
            self.types.function(params, ret)
        }
    }

    /// Notes what using (calling or naming) a declaration means for purity.
    fn log_use(&mut self, b: &mut Body, r: ItemRef, sig: &FnSig, span: Span, how: &str) {
        let name = item_name(&self.item(r).kind).map_or_else(String::new, |i| i.name.clone());
        match (sig.kind, r.origin) {
            (FnKind::Builtin { pure: false }, Origin::Std(s)) => {
                let module = self.std[s].name;
                let shown = if module == "control" {
                    name
                } else {
                    format!("{module}.{name}")
                };
                b.log
                    .direct
                    .push((span, format!("{how} `{shown}` makes effects")));
            }
            (FnKind::Harness, _) => {
                b.log
                    .direct
                    .push((span, format!("{how} the harness `{name}` makes effects")));
            }
            (FnKind::Fn | FnKind::Prompt, Origin::Module(_)) => b.log.calls.push((r, span)),
            _ => {}
        }
    }

    /// Reports the type parameters inference left open (binding them to the error type), and
    /// applies the bounds `S` and `Num`.
    /// `quiet` leaves open parameters unreported, where an error already explains them (an
    /// argument of the error type, a mismatch the caller reports).
    fn close_vars(
        &mut self,
        b: &Body,
        pairs: &[(ParamId, TypeId)],
        callee: &str,
        span: Span,
        quiet: bool,
    ) {
        for (param, t) in pairs {
            let Ty::Var(v) = *self.types.get(*t) else {
                continue;
            };
            let resolved = self.types.resolve(*t);
            let pname = self.types.param_def(*param).name.clone();
            if matches!(self.types.get(resolved), Ty::Var(_)) && quiet {
                self.types.bind_var(v, TypeId::ERROR);
                continue;
            }
            if matches!(self.types.get(resolved), Ty::Var(_)) {
                self.report(
                    b.origin(),
                    Code::CannotInfer,
                    span,
                    format!(
                        "the type parameter `{pname}` of `{callee}` cannot be inferred; give it \
                         with a turbofish (`{callee}::<…>`)"
                    ),
                );
                self.types.bind_var(v, TypeId::ERROR);
                continue;
            }
            match self.types.param_def(*param).bound {
                Bound::None => {}
                Bound::Text => {
                    let text = if self.types.subtype(&mut self.memo, resolved, TypeId::STR) {
                        TypeId::STR
                    } else if self
                        .types
                        .subtype(&mut self.memo, resolved, TypeId::UNTRUSTED_STR)
                    {
                        TypeId::UNTRUSTED_STR
                    } else {
                        let shown = self.types.display(resolved);
                        self.report(
                            b.origin(),
                            Code::Mismatch,
                            span,
                            format!("`{callee}` takes text for `{pname}`, not `{shown}`"),
                        );
                        TypeId::ERROR
                    };
                    self.types.bind_var(v, text);
                }
                Bound::Countable => {
                    if !self.countable(resolved) {
                        let shown = self.types.display(resolved);
                        self.report(
                            b.origin(),
                            Code::Mismatch,
                            span,
                            format!(
                                "`{callee}` counts text, a `Prompt` or a `Conversation`, not \
                                 `{shown}`"
                            ),
                        );
                        self.types.bind_var(v, TypeId::ERROR);
                    }
                }
                Bound::Num => {
                    if !matches!(
                        self.types.get(resolved),
                        Ty::Int | Ty::Float | Ty::Duration | Ty::Money | Ty::Error | Ty::Never
                    ) {
                        let shown = self.types.display(resolved);
                        self.report(
                            b.origin(),
                            Code::Mismatch,
                            span,
                            format!("`{callee}` takes a number for `{pname}`, not `{shown}`"),
                        );
                        self.types.bind_var(v, TypeId::ERROR);
                    }
                }
            }
        }
    }

    // ---- paths ----

    /// The module `e` names, when it is a module path (`list`, `critics`, `data.text`) and not
    /// shadowed by a local.
    pub(crate) fn module_path(&self, b: &Body, e: &Expr) -> Option<ModRef> {
        match &e.kind {
            ExprKind::Name(id) if b.local(&id.name).is_none() => {
                self.module_of(b.origin(), &id.name)
            }
            ExprKind::Member {
                object,
                name,
                optional: false,
            } => match &object.kind {
                ExprKind::Name(data)
                    if data.name == "data"
                        && b.local("data").is_none()
                        && self.module_of(b.origin(), "data").is_some() =>
                {
                    let sub = prelude::MODULES.iter().find(|m| **m == name.name)?;
                    Some(self.std_module(sub))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The enum a type path in expression position names (`Effort`, `critics.Verdict`).
    pub(crate) fn type_path(&mut self, b: &Body, e: &Expr) -> Option<EnumId> {
        let mut idents: Vec<&Ident> = Vec::new();
        let mut at = e;
        loop {
            match &at.kind {
                ExprKind::Name(id) => {
                    idents.push(id);
                    break;
                }
                ExprKind::Member {
                    object,
                    name,
                    optional: false,
                } => {
                    idents.push(name);
                    at = object;
                }
                _ => return None,
            }
        }
        idents.reverse();
        if b.local(&idents[0].name).is_some() {
            return None;
        }
        let owned: Vec<Ident> = idents.into_iter().cloned().collect();
        self.type_path_idents(b, &owned)
    }

    /// The enum a written type path names, if it names one.
    pub(crate) fn type_path_idents(&mut self, b: &Body, idents: &[Ident]) -> Option<EnumId> {
        let origin = b.origin();
        let r = match idents {
            [one] => match self.scope_item(origin, &one.name) {
                Some(Ok(r)) => r,
                Some(Err(())) => return None,
                None => ItemRef {
                    origin: Origin::Std(0),
                    index: *self.std[0].scope.get(&one.name)?,
                },
            },
            [module, item] => {
                let m = self.module_of(origin, &module.name)?;
                self.member_item(m, &item.name)?.ok()?
            }
            _ => return None,
        };
        match self.sig(r).clone() {
            ItemSig::Enum(id) => Some(id),
            ItemSig::Alias { params, .. } if params.is_empty() => {
                let span = self.item(r).span;
                let t = self.item_type(origin, r, Vec::new(), span);
                match self.types.get(t) {
                    Ty::Enum(id, _) => Some(*id),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The variant a variant literal's path names: qualified, or bare (the expected enum's,
    /// else the one enum in scope that has it).
    pub(crate) fn resolve_variant(
        &mut self,
        b: &Body,
        path: &[Ident],
        target: Option<TypeId>,
        span: Span,
    ) -> Option<(EnumId, usize)> {
        let origin = b.origin();
        let last = path.last()?;
        if path.len() > 1 {
            let Some(id) = self.type_path_idents(b, &path[..path.len() - 1]) else {
                let names: Vec<&str> = path[..path.len() - 1]
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect();
                self.report(
                    origin,
                    Code::UnknownVariant,
                    span,
                    format!("`{}` is not an enum", names.join(".")),
                );
                return None;
            };
            if let Some(index) = self.types.enum_def(id).variant(&last.name) {
                return Some((id, index));
            }
            let name = self.types.enum_def(id).name.clone();
            self.report(
                origin,
                Code::UnknownVariant,
                last.span,
                format!("`{name}` has no variant `{}`", last.name),
            );
            return None;
        }
        if let Some(t) = target
            && let Some(id) = self.types.as_enum(t)
            && let Some(index) = self.types.enum_def(id).variant(&last.name)
        {
            return Some((id, index));
        }
        for scope in [origin, Origin::Std(0)] {
            let found = self.variants_named(scope, &last.name);
            match found.as_slice() {
                [] => {}
                [one] => {
                    let index = self.types.enum_def(*one).variant(&last.name)?;
                    return Some((*one, index));
                }
                many => {
                    let names: Vec<String> = many
                        .iter()
                        .map(|id| self.types.enum_def(*id).name.clone())
                        .collect();
                    let names: Vec<&str> = names.iter().map(String::as_str).collect();
                    self.report(
                        origin,
                        Code::AmbiguousVariant,
                        last.span,
                        format!(
                            "the variant `{}` is in several enums ({}); qualify it",
                            last.name,
                            crate::pattern::quote(&names)
                        ),
                    );
                    return None;
                }
            }
        }
        self.report(
            origin,
            Code::UnknownVariant,
            last.span,
            format!("no enum in scope has a variant `{}`", last.name),
        );
        None
    }

    /// The enums visible at the top level of `origin` that have a variant `name`.
    fn variants_named(&mut self, origin: Origin, name: &str) -> Vec<EnumId> {
        if !self.variant_index.contains_key(&origin) {
            let mut index: HashMap<String, Vec<EnumId>> = HashMap::new();
            let refs: Vec<ItemRef> = match origin {
                Origin::Module(m) => self.loaded.modules[m.0]
                    .scope
                    .iter()
                    .filter_map(|(n, _, _)| self.scope_item(origin, n).and_then(Result::ok))
                    .collect(),
                Origin::Std(s) => self.std[s]
                    .scope
                    .values()
                    .map(|&index| ItemRef { origin, index })
                    .collect(),
            };
            for r in refs {
                if let ItemSig::Enum(id) = self.sig(r) {
                    for v in &self.types.enum_def(*id).variants {
                        let ids = index.entry(v.name.clone()).or_default();
                        if !ids.contains(id) {
                            ids.push(*id);
                        }
                    }
                }
            }
            for ids in index.values_mut() {
                ids.sort_unstable();
            }
            self.variant_index.insert(origin, index);
        }
        self.variant_index[&origin]
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    // ---- members ----

    pub(crate) fn member(
        &mut self,
        b: &mut Body,
        e: &Expr,
        object: &Expr,
        name: &Ident,
        optional: bool,
        expected: Option<TypeId>,
    ) -> TypeId {
        let origin = b.origin();
        if !optional && let Some(m) = self.module_path(b, object) {
            if let Some(o) = m.origin() {
                self.res(b, object.span, Res::Module(o));
            }
            return self.module_member(b, m, name, expected, e.span);
        }
        if !optional && let Some(enum_id) = self.type_path(b, object) {
            return self.unit_variant(b, enum_id, name, expected, e.span);
        }
        let t = self.expr(b, object, None);
        let t = self.types.resolve(t);
        let base = if optional {
            self.optional_base(b, t, object.span)
        } else {
            t
        };
        let field = self.field_type(base, &name.name);
        let ty = match field {
            Some(f) => {
                self.res(b, e.span, Res::Field(name.name.clone()));
                f
            }
            None if base == TypeId::ERROR => TypeId::ERROR,
            None => {
                let shown = self.types.display(base);
                let hint = if self.method_exists(base, &name.name) {
                    format!("; `{}` is a method, call it", name.name)
                } else {
                    String::new()
                };
                self.report(
                    origin,
                    Code::NoMember,
                    name.span,
                    format!("`{shown}` has no field `{}`{hint}", name.name),
                );
                TypeId::ERROR
            }
        };
        if optional { self.types.option(ty) } else { ty }
    }

    /// The value inside an option for `?.`.
    fn optional_base(&mut self, b: &Body, t: TypeId, span: Span) -> TypeId {
        match self.types.get(t) {
            Ty::Option(inner) => *inner,
            Ty::Error => TypeId::ERROR,
            _ => {
                let shown = self.types.display(t);
                self.report(
                    b.origin(),
                    Code::Operator,
                    span,
                    format!("`?.` needs an option on its left, not `{shown}`"),
                );
                TypeId::ERROR
            }
        }
    }

    /// The type a read of field `name` of a value of type `t` gives.
    fn field_type(&mut self, t: TypeId, name: &str) -> Option<TypeId> {
        let Ty::Record(fields) = self.types.get(t).clone() else {
            return None;
        };
        let f = &fields[crate::types::field_index(&fields, name)?];
        Some(if f.optional {
            self.types.option(f.ty)
        } else {
            f.ty
        })
    }

    fn method_exists(&self, t: TypeId, name: &str) -> bool {
        builtins::method_module(&self.types, t).is_some_and(|m| match self.std_module(m) {
            ModRef::Std(s) => self.std[s].scope.contains_key(name),
            _ => false,
        })
    }

    fn module_member(
        &mut self,
        b: &mut Body,
        m: ModRef,
        name: &Ident,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let origin = b.origin();
        match m {
            ModRef::Broken => TypeId::ERROR,
            ModRef::Missing(module) => {
                let full = format!("{module}.{}", name.name);
                self.no_signature(origin, span, &full)
            }
            ModRef::User(_) | ModRef::Std(_) => match self.member_item(m, &name.name) {
                Some(Ok(r)) => self.item_value(b, r, expected, span),
                Some(Err(r)) => {
                    self.private(origin, name, r);
                    TypeId::ERROR
                }
                None => {
                    if let ModRef::Std(s) = m {
                        let full = format!("{}.{}", self.std[s].name, name.name);
                        self.no_signature(origin, span, &full)
                    } else {
                        self.report(
                            origin,
                            Code::NoMember,
                            name.span,
                            format!("the module defines no `{}`", name.name),
                        );
                        TypeId::ERROR
                    }
                }
            },
        }
    }

    fn unit_variant(
        &mut self,
        b: &Body,
        enum_id: EnumId,
        name: &Ident,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let origin = b.origin();
        let def = self.types.enum_def(enum_id);
        let Some(index) = def.variant(&name.name) else {
            let shown = def.name.clone();
            self.report(
                origin,
                Code::UnknownVariant,
                name.span,
                format!("`{shown}` has no variant `{}`", name.name),
            );
            return TypeId::ERROR;
        };
        if def.variants[index].has_payload {
            self.report(
                origin,
                Code::UnknownVariant,
                span,
                format!(
                    "the variant `{}` has a payload; write `{} {{ … }}`",
                    name.name, name.name
                ),
            );
            return TypeId::ERROR;
        }
        self.res(b, span, Res::Variant(enum_id, index));
        let target = expected.map(|t| self.strip_option(t));
        let (ty, fresh) = self.enum_instance(enum_id, target);
        if let Some(want) = target {
            self.types.subtype_quiet(&mut self.memo, ty, want);
        }
        let ty = self.types.resolve(ty);
        if fresh {
            self.unbound_args(b, ty, span);
        }
        self.types.resolve(ty)
    }

    // ---- calls ----

    #[allow(clippy::too_many_arguments, reason = "a call's parts")]
    pub(crate) fn call(
        &mut self,
        b: &mut Body,
        e: &Expr,
        callee: &Expr,
        type_args: &[ast::Type],
        args: &[Arg],
        expected: Option<TypeId>,
    ) -> TypeId {
        if let ExprKind::Member {
            object,
            name,
            optional,
        } = &callee.kind
            && self.module_path(b, object).is_none()
            && self.type_path(b, object).is_none()
        {
            return self.method_call(
                b, e, callee, object, name, *optional, type_args, args, expected,
            );
        }
        let target = self.callable(b, callee);
        self.apply(b, e.span, target, None, type_args, args, expected)
    }

    pub(crate) fn pipe(
        &mut self,
        b: &mut Body,
        e: &Expr,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<TypeId>,
    ) -> TypeId {
        let left = self.expr(b, lhs, None);
        let (callee, type_args, args): (&Expr, &[ast::Type], &[Arg]) = match &rhs.kind {
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => (callee, type_args, args),
            _ => (rhs, &[], &[]),
        };
        let target = self.callable(b, callee);
        // The call's result is recorded at the pipe expression only (by `expr`).
        self.apply(
            b,
            e.span,
            target,
            Some((left, lhs.span)),
            type_args,
            args,
            expected,
        )
    }

    /// What a call's callee is: a declaration, or a value.
    fn callable(&mut self, b: &mut Body, callee: &Expr) -> Callable {
        let origin = b.origin();
        let found = match &callee.kind {
            ExprKind::Name(id) if b.local(&id.name).is_none() => {
                match self.scope_item(origin, &id.name) {
                    Some(Ok(r)) => Some(r),
                    Some(Err(())) => return Callable::Value(TypeId::ERROR),
                    None => match prelude::lookup(&id.name) {
                        Some(PreludeName::Function(f)) => self.control_function(f),
                        _ => None,
                    },
                }
            }
            ExprKind::Member {
                object,
                name,
                optional: false,
            } => match self.module_path(b, object) {
                Some(m) => {
                    if let Some(o) = m.origin() {
                        self.res(b, object.span, Res::Module(o));
                    }
                    match self.member_item(m, &name.name) {
                        Some(Ok(r)) => Some(r),
                        _ => None,
                    }
                }
                None => None,
            },
            _ => None,
        };
        if let Some(r) = found
            && let Some(sig) = self.fn_sig(r)
        {
            self.res(b, callee.span, Res::Item(r));
            let ty = self.sig_type(r, &sig, callee.span);
            self.record_type(b.module, callee.span, ty);
            return Callable::Sig(Some(r), sig);
        }
        let t = self.expr(b, callee, None);
        Callable::Value(t)
    }

    #[allow(clippy::too_many_arguments, reason = "a call's parts")]
    fn apply(
        &mut self,
        b: &mut Body,
        span: Span,
        target: Callable,
        receiver: Option<(TypeId, Span)>,
        type_args: &[ast::Type],
        args: &[Arg],
        expected: Option<TypeId>,
    ) -> TypeId {
        match target {
            Callable::Sig(r, sig) => {
                self.call_sig(b, span, r, &sig, receiver, type_args, args, expected)
            }
            Callable::Value(t) => self.call_value(b, span, t, receiver, type_args, args),
        }
    }

    #[allow(clippy::too_many_arguments, reason = "a method call's parts")]
    fn method_call(
        &mut self,
        b: &mut Body,
        e: &Expr,
        callee: &Expr,
        object: &Expr,
        name: &Ident,
        optional: bool,
        type_args: &[ast::Type],
        args: &[Arg],
        expected: Option<TypeId>,
    ) -> TypeId {
        let origin = b.origin();
        let t = self.expr(b, object, None);
        let t = self.types.resolve(t);
        let base = if optional {
            self.optional_base(b, t, object.span)
        } else {
            t
        };
        let wrap = |this: &mut Self, ty: TypeId| if optional { this.types.option(ty) } else { ty };
        if base == TypeId::ERROR {
            self.unchecked_args(b, args);
            return TypeId::ERROR;
        }
        if let Some(field) = self.field_type(base, &name.name) {
            self.res(b, callee.span, Res::Field(name.name.clone()));
            self.record_type(b.module, callee.span, field);
            let ty = self.call_value(b, e.span, field, None, type_args, args);
            return wrap(self, ty);
        }
        let in_module =
            builtins::method_module(&self.types, base).and_then(|m| match self.std_module(m) {
                ModRef::Std(s) => self.std[s].scope.get(&name.name).map(|&index| ItemRef {
                    origin: Origin::Std(s),
                    index,
                }),
                _ => None,
            });
        let candidate = in_module.or_else(|| {
            let r = match self.scope_item(origin, &name.name) {
                Some(Ok(r)) => Some(r),
                Some(Err(())) => None,
                None => match prelude::lookup(&name.name) {
                    Some(PreludeName::Function(f)) => self.control_function(f),
                    _ => None,
                },
            }?;
            let sig = self.fn_sig(r)?;
            self.accepts(&sig, base).then_some(r)
        });
        let Some(r) = candidate else {
            let shown = self.types.display(base);
            self.report(
                origin,
                Code::NoMember,
                name.span,
                format!("`{shown}` has no method `{}`", name.name),
            );
            self.unchecked_args(b, args);
            return TypeId::ERROR;
        };
        let Some(sig) = self.fn_sig(r) else {
            return TypeId::ERROR;
        };
        self.res(b, callee.span, Res::Item(r));
        let ty = self.sig_type(r, &sig, callee.span);
        self.record_type(b.module, callee.span, ty);
        let ty = self.call_sig(
            b,
            e.span,
            Some(r),
            &sig,
            Some((base, object.span)),
            type_args,
            args,
            expected,
        );
        wrap(self, ty)
    }

    /// Whether a value of type `t` can be the first argument of `sig`.
    fn accepts(&mut self, sig: &FnSig, t: TypeId) -> bool {
        let Some(first) = sig.params.first() else {
            return false;
        };
        let pairs: Vec<(ParamId, TypeId)> = sig
            .type_params
            .iter()
            .map(|p| (*p, self.types.new_var(*p)))
            .collect();
        let param = self.types.substitute(first.ty, &pairs);
        self.types.subtype_quiet(&mut self.memo, t, param)
    }

    fn unchecked_args(&mut self, b: &mut Body, args: &[Arg]) {
        for arg in args {
            self.expr(b, &arg.value, None);
        }
    }

    /// Notes a call of a function value: a harness value makes effects; any other is noted
    /// for the rule on harness defaults and configuration.
    fn log_value_call(&mut self, b: &mut Body, span: Span, harness: bool) {
        if harness {
            b.log
                .direct
                .push((span, "calling a harness makes effects".to_owned()));
            self.records.harness_calls.push((b.module, span));
        } else {
            b.log.values.push(span);
        }
    }

    /// A call of a function value. It adds nothing to the body's effects: every function value
    /// was noted where it was made (a closure's body is checked in place) or named (a
    /// declaration, a built-in), so its effects are already counted there.
    fn call_value(
        &mut self,
        b: &mut Body,
        span: Span,
        t: TypeId,
        receiver: Option<(TypeId, Span)>,
        type_args: &[ast::Type],
        args: &[Arg],
    ) -> TypeId {
        let origin = b.origin();
        let t = self.types.resolve(t);
        let (params, ret, harness) = match self.types.get(t).clone() {
            Ty::Fn(ps, r) => (ps.to_vec(), r, false),
            Ty::Harness(ps, r) => (ps.to_vec(), r, true),
            Ty::Error => {
                self.unchecked_args(b, args);
                return TypeId::ERROR;
            }
            _ => {
                let shown = self.types.display(t);
                self.report(
                    origin,
                    Code::NotCallable,
                    span,
                    format!("a value of type `{shown}` cannot be called"),
                );
                self.unchecked_args(b, args);
                return TypeId::ERROR;
            }
        };
        if !type_args.is_empty() {
            self.report(
                origin,
                Code::TypeArguments,
                span,
                "a function value takes no type arguments".to_owned(),
            );
        }
        let given = args.len() + usize::from(receiver.is_some());
        if given != params.len() {
            self.report(
                origin,
                Code::Arguments,
                span,
                format!(
                    "this function takes {} argument(s), not {given}",
                    params.len()
                ),
            );
        }
        let mut slots = Vec::new();
        let mut want = params.iter().copied();
        if let Some((rt, at)) = receiver {
            slots.push(ArgSource::Receiver);
            if let Some(p) = want.next() {
                self.mismatch(b, at, rt, p);
            }
        }
        for (k, arg) in args.iter().enumerate() {
            if let Some(n) = &arg.name {
                self.report(
                    origin,
                    Code::Arguments,
                    n.span,
                    "a function value takes positional arguments only".to_owned(),
                );
            }
            slots.push(ArgSource::Arg(k));
            match want.next() {
                Some(p) => {
                    self.expect(b, &arg.value, p);
                }
                None => {
                    self.expr(b, &arg.value, None);
                }
            }
        }
        self.log_value_call(b, span, harness);
        let given: Vec<TypeId> = receiver
            .map(|(rt, _)| rt)
            .into_iter()
            .chain(
                args.iter()
                    .filter_map(|a| self.tables[b.module.0].type_at(a.value.span)),
            )
            .collect();
        let secret_arg = given.into_iter().any(|t| {
            let t = self.types.resolve(t);
            self.types.secret_inside(t)
        });
        let ret = if secret_arg && carries_data(ret) && !self.types.secret_inside(ret) {
            self.types.secret(ret)
        } else {
            ret
        };
        self.table(b.module).calls.insert(
            Key::from(span),
            CallInfo {
                callee: None,
                type_args: Vec::new(),
                args: slots,
            },
        );
        ret
    }

    /// The type a `state.*` call works at: the declared `state` type of the calling module's
    /// package. Without a declaration, or with a turbofish (the type is never the caller's to
    /// choose: `state.get::<Str>()` would read untrusted state as trusted text), the call is
    /// refused and typed as an error.
    fn state_type_at(&mut self, b: &Body, span: Span, name: &str, turbofish: bool) -> TypeId {
        let origin = b.origin();
        if turbofish {
            self.report(
                origin,
                Code::TypeArguments,
                span,
                format!(
                    "`state.{name}` takes no type arguments: its type is the package's declared \
                     `state` type"
                ),
            );
            return TypeId::ERROR;
        }
        if let Some(t) = self.package_state(b.module) {
            return t;
        }
        self.report(
            origin,
            Code::UnknownName,
            span,
            format!("`state.{name}` needs a `state` declaration in the package, and it has none"),
        );
        TypeId::ERROR
    }

    /// Closes to `()` each still-open type parameter of a built-in that only parameters left to
    /// their defaults mention, and not its result (`log.info("done")`'s `data: T?`): no value of
    /// it comes from the call and none goes back, so nothing is guessed (`()` rather than
    /// `Never`, so a `@schema(T)` parameter stays satisfied).
    fn close_unused_params(
        &mut self,
        sig: &FnSig,
        pairs: &[(ParamId, TypeId)],
        slots: &[Option<ArgSource>],
    ) {
        for (p, t) in pairs {
            let resolved = self.types.resolve(*t);
            let Ty::Var(v) = *self.types.get(resolved) else {
                continue;
            };
            let probe = [(*p, TypeId::ERROR)];
            let mentions =
                |types: &mut crate::types::Types, ty: TypeId| types.substitute(ty, &probe) != ty;
            if mentions(&mut self.types, sig.ret) {
                continue;
            }
            let given = sig
                .params
                .iter()
                .zip(slots)
                .any(|(param, slot)| slot.is_some() && mentions(&mut self.types, param.ty));
            if !given {
                self.types.bind_var(v, TypeId::UNIT);
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "a call's parts and steps"
    )]
    fn call_sig(
        &mut self,
        b: &mut Body,
        span: Span,
        r: Option<ItemRef>,
        sig: &FnSig,
        receiver: Option<(TypeId, Span)>,
        type_args: &[ast::Type],
        args: &[Arg],
        expected: Option<TypeId>,
    ) -> TypeId {
        let origin = b.origin();
        let name = r.and_then(|r| item_name(&self.item(r).kind)).map_or_else(
            || "this function".to_owned(),
            |i| builtins::builtin_name(&i.name).to_owned(),
        );
        // Type arguments: the package's state type for `state.*`, the turbofish's, or inference
        // variables.
        let mut pairs: Vec<(ParamId, TypeId)> = Vec::with_capacity(sig.type_params.len());
        if sig.traits.state {
            let state = self.state_type_at(b, span, &name, !type_args.is_empty());
            for p in &sig.type_params {
                pairs.push((*p, state));
            }
        } else if type_args.is_empty() {
            for p in &sig.type_params {
                pairs.push((*p, self.types.new_var(*p)));
            }
        } else {
            let tparams = b.tparams.clone();
            let given: Vec<TypeId> = type_args
                .iter()
                .map(|t| self.lower_type(origin, &tparams, t))
                .collect();
            if given.len() != sig.type_params.len() {
                self.report(
                    origin,
                    Code::TypeArguments,
                    span,
                    format!(
                        "`{name}` takes {} type argument(s), not {}",
                        sig.type_params.len(),
                        given.len()
                    ),
                );
            }
            for (i, p) in sig.type_params.iter().enumerate() {
                let t = match given.get(i) {
                    Some(t) => self.explicit_bound(b, *p, *t, &name, span),
                    None => self.types.new_var(*p),
                };
                pairs.push((*p, t));
            }
        }
        let params: Vec<TypeId> = sig
            .params
            .iter()
            .map(|p| self.types.substitute(p.ty, &pairs))
            .collect();
        let ret = match (sig.kind, r) {
            (FnKind::Prompt, Some(r)) => self.prompt_type(r, span),
            _ => self.types.substitute(sig.ret, &pairs),
        };
        // Which argument fills which parameter.
        let mut slots: Vec<Option<ArgSource>> = vec![None; params.len()];
        let mut unplaced: Vec<usize> = Vec::new();
        let mut next = 0usize;
        if receiver.is_some() {
            if params.is_empty() {
                self.report(
                    origin,
                    Code::Arguments,
                    span,
                    format!("`{name}` takes no arguments"),
                );
            } else {
                slots[0] = Some(ArgSource::Receiver);
                next = 1;
            }
        }
        for (k, arg) in args.iter().enumerate() {
            match &arg.name {
                None if next < params.len() => {
                    slots[next] = Some(ArgSource::Arg(k));
                    next += 1;
                }
                None => {
                    self.report(
                        origin,
                        Code::Arguments,
                        arg.value.span,
                        format!("too many arguments: `{name}` takes {}", params.len()),
                    );
                    unplaced.push(k);
                }
                Some(n) => match sig.param_index(&n.name) {
                    Some(i) if slots[i].is_none() => slots[i] = Some(ArgSource::Arg(k)),
                    Some(_) => {
                        self.report(
                            origin,
                            Code::Arguments,
                            n.span,
                            format!("the argument `{}` is given twice", n.name),
                        );
                        unplaced.push(k);
                    }
                    None => {
                        self.report(
                            origin,
                            Code::Arguments,
                            n.span,
                            format!("`{name}` has no parameter `{}`", n.name),
                        );
                        unplaced.push(k);
                    }
                },
            }
        }
        // The receiver and the arguments that are not closures, then what the call is expected
        // to give, then the closures.
        // Each argument that fit, with its type, where it is and its parameter: re-checked
        // once the type arguments are final.
        let mut placed: Vec<(TypeId, Span, usize)> = Vec::new();
        let mut saw_error = false;
        if let (Some((rt, at)), Some(p)) = (receiver, params.first()) {
            saw_error |= self.types.resolve(rt) == TypeId::ERROR;
            if self.mismatch(b, at, rt, *p) {
                placed.push((rt, at, 0));
            }
        }
        for closures in [false, true] {
            // An audited conversion's type comes from its argument (or turbofish), never
            // from what is expected of it: relating its result to an expected inference
            // variable now would bind that variable to the argument's untrusted type.
            if closures
                && sig.traits.audited.is_none()
                && let Some(want) = expected
                && self.types.has_vars(ret)
            {
                self.types.subtype_quiet(&mut self.memo, ret, want);
            }
            for (i, slot) in slots.iter().enumerate() {
                let Some(ArgSource::Arg(k)) = slot else {
                    continue;
                };
                let value = &args[*k].value;
                if is_closure(value) != closures {
                    continue;
                }
                let got = self.expr(b, value, Some(params[i]));
                saw_error |= self.types.resolve(got) == TypeId::ERROR;
                if self.mismatch(b, value.span, got, params[i]) {
                    placed.push((got, value.span, i));
                }
            }
        }
        for k in unplaced {
            self.expr(b, &args[k].value, None);
        }
        let missing: Vec<&str> = sig
            .params
            .iter()
            .zip(&slots)
            .filter(|(p, s)| s.is_none() && !p.has_default)
            .map(|(p, _)| p.name.as_str())
            .collect();
        if !missing.is_empty() {
            self.report(
                origin,
                Code::Arguments,
                span,
                format!(
                    "`{name}` is missing the argument(s) {}",
                    crate::pattern::quote(&missing)
                ),
            );
        }
        if r.is_some_and(|r| matches!(r.origin, Origin::Std(_))) {
            self.close_unused_params(sig, &pairs, &slots);
        }
        self.close_vars(b, &pairs, &name, span, saw_error);
        // A later argument may have widened a type argument past what an earlier one allowed
        // (`apply(id, web)` binds `T` to `Str` from `id`, then widens it to `Untrusted<Str>`
        // for `web`): every argument is checked again against its final parameter type.
        let mut secret_arg = false;
        let mut fitted: Vec<(TypeId, Span)> = Vec::new();
        for (got, at, i) in placed {
            let want = self.types.resolve(params[i]);
            let got = self.types.resolve(got);
            secret_arg |= self.types.secret_inside(got);
            if self.mismatch(b, at, got, want) {
                fitted.push((got, at));
            }
        }
        let mut result = self.types.resolve(ret);
        let mut targs: Vec<TypeId> = pairs.iter().map(|(_, t)| self.types.resolve(*t)).collect();
        let traits = &sig.traits;
        if let Some(kind) = traits.audited {
            let explicit = !type_args.is_empty();
            result = self.audited(b, span, kind, &mut targs, explicit, args, &slots);
        }
        if traits.secrecy_sink {
            for (got, at) in &fitted {
                if self.types.secret_inside(*got) {
                    let shown = self.types.display(*got);
                    self.report(
                        origin,
                        Code::Secret,
                        *at,
                        format!(
                            "`{shown}` holds a secret, which cannot reach `{name}` (a secrecy \
                             sink: it can send or keep its arguments outside the machine)"
                        ),
                    );
                }
            }
        }
        for (p, t) in sig.type_params.iter().zip(targs.clone()) {
            let pname = self.types.param_def(*p).name.clone();
            if traits.schema.contains(&pname) {
                self.require_schema(b, t, span, &format!("`{name}`'s type argument `{pname}`"));
            }
        }
        if traits.secret_answers && !plain_questions(args, &slots) {
            // Answers are secret unless every question is written out without `secret: true`.
            if let Ty::List(elem) = *self.types.get(result) {
                let secret = self.types.secret(elem);
                result = self.types.list(secret);
            }
        }
        if traits.loads {
            let loaded = first_literal(args, &slots);
            self.records.loads.push((b.module, span, loaded));
            let not_harness = targs
                .first()
                .filter(|h| !matches!(self.types.get(**h), Ty::Harness(..) | Ty::Error));
            if let Some(h) = not_harness {
                let shown = self.types.display(*h);
                self.report(
                    origin,
                    Code::TypeArguments,
                    span,
                    format!("`{name}` loads a harness, so its type argument is a harness type, not `{shown}`"),
                );
            }
        }
        // A result computed from a statically secret argument is secret too (chapter 03,
        // "Propagation"), except where `declassify` removes the label, for prompts, whose
        // sections keep labels at run time, and for `()`, which holds nothing.
        if secret_arg
            && traits.audited != Some(TrustKind::Declassify)
            && sig.kind != FnKind::Prompt
            && carries_data(result)
            && !self.types.secret_inside(result)
        {
            result = self.types.secret(result);
        }
        self.table(b.module).calls.insert(
            Key::from(span),
            CallInfo {
                callee: r,
                type_args: targs,
                args: slots
                    .into_iter()
                    .map(|s| s.unwrap_or(ArgSource::Default))
                    .collect(),
            },
        );
        if let Some(r) = r {
            self.log_use(b, r, sig, span, "calling");
        }
        result
    }

    /// A call of `trust`, `as_task` or `declassify`: the type it converts to, which must be
    /// monomorphic (`C0036`), recorded as a trust site. `trust`'s `T`, unless given, is the
    /// argument's type with its trust labels removed (`Untrusted<Verdict>` gives `Verdict`),
    /// never what generic inference would bind (the argument's own, untrusted, type).
    #[allow(clippy::too_many_arguments, reason = "a call's parts")]
    fn audited(
        &mut self,
        b: &Body,
        span: Span,
        kind: TrustKind,
        targs: &mut [TypeId],
        explicit: bool,
        args: &[Arg],
        slots: &[Option<ArgSource>],
    ) -> TypeId {
        let to = match kind {
            TrustKind::Trust => {
                let t = targs.first().copied().unwrap_or(TypeId::ERROR);
                let t = if explicit {
                    t
                } else {
                    self.types.erase(t, Erase::Trust)
                };
                if let Some(slot) = targs.first_mut() {
                    *slot = t;
                }
                t
            }
            TrustKind::Declassify => targs.first().copied().unwrap_or(TypeId::ERROR),
            TrustKind::AsTask => TypeId::BRIEF,
        };
        let reason = match slots.get(1) {
            Some(Some(ArgSource::Arg(k))) => literal_text(&args[*k].value),
            _ => None,
        };
        self.records
            .trust_sites
            .push((b.module, span, kind, to, reason));
        if self.types.has_params(to) || self.types.has_vars(to) {
            let shown = self.types.display(to);
            self.report(
                b.origin(),
                Code::NotMonomorphic,
                span,
                format!(
                    "`{}` converts only at a monomorphic type, and `{shown}` is or holds a type \
                     parameter: a generic helper cannot become a general-purpose launderer",
                    kind.name()
                ),
            );
            return TypeId::ERROR;
        }
        to
    }

    /// Checks an explicit type argument against its parameter's bound.
    /// Whether `t` is chapter 06's `Countable`: text (trusted or not), a prompt, or a
    /// conversation. A secret is none of them.
    fn countable(&mut self, t: TypeId) -> bool {
        matches!(
            self.types.get(t),
            Ty::Error | Ty::Never | Ty::Handle(crate::types::Handle::Conversation)
        ) || self.types.subtype(&mut self.memo, t, TypeId::UNTRUSTED_STR)
            || self.types.subtype(&mut self.memo, t, TypeId::PROMPT)
    }

    fn explicit_bound(
        &mut self,
        b: &Body,
        p: ParamId,
        t: TypeId,
        callee: &str,
        span: Span,
    ) -> TypeId {
        let ok = match self.types.param_def(p).bound {
            Bound::None => true,
            Bound::Text => matches!(self.types.get(t), Ty::Str | Ty::UntrustedStr | Ty::Error),
            Bound::Num => matches!(
                self.types.get(t),
                Ty::Int | Ty::Float | Ty::Duration | Ty::Money | Ty::Error
            ),
            Bound::Countable => self.countable(t),
        };
        if ok {
            return t;
        }
        let pname = self.types.param_def(p).name.clone();
        let shown = self.types.display(t);
        self.report(
            b.origin(),
            Code::TypeArguments,
            span,
            format!("`{shown}` does not fit `{callee}`'s type parameter `{pname}`"),
        );
        TypeId::ERROR
    }

    pub(crate) fn with_override(
        &mut self,
        b: &mut Body,
        call: &Expr,
        config: &RecordLit,
        expected: Option<TypeId>,
    ) -> TypeId {
        let t = self.expr(b, call, expected);
        let harness = match &call.kind {
            ExprKind::Call { callee, .. } => self.tables[b.module.0]
                .type_at(callee.span)
                .map(|c| self.types.resolve(c))
                .is_some_and(|c| matches!(self.types.get(c), Ty::Harness(..) | Ty::Error)),
            _ => false,
        };
        if !harness {
            self.report(
                b.origin(),
                Code::OverrideNotHarness,
                config.span,
                "`with { … }` overrides apply only to a call of a harness".to_owned(),
            );
        }
        let want = self.known_type("HarnessConfig");
        let got = self.record_lit(b, config, Some(want), Code::RecordFields);
        self.mismatch(b, config.span, got, want);
        t
    }

    /// `x is Variant`.
    pub(crate) fn is_test(&mut self, b: &mut Body, left: TypeId, rhs: &Expr) {
        let origin = b.origin();
        let left = self.types.resolve(left);
        if left == TypeId::ERROR {
            return;
        }
        let Some(enum_id) = self.types.as_enum(left) else {
            let shown = self.types.display(left);
            self.report(
                origin,
                Code::Operator,
                rhs.span,
                format!("`is` tests the variant of an enum value, not of `{shown}`"),
            );
            return;
        };
        let (named_enum, name) = match &rhs.kind {
            ExprKind::Name(id) => (Some(enum_id), id),
            ExprKind::Member {
                object,
                name,
                optional: false,
            } => (self.type_path(b, object), name),
            _ => {
                self.report(
                    origin,
                    Code::UnknownVariant,
                    rhs.span,
                    "`is` takes a variant name".to_owned(),
                );
                return;
            }
        };
        let def = self.types.enum_def(enum_id);
        match def.variant(&name.name) {
            Some(index) if named_enum == Some(enum_id) => {
                self.res(b, rhs.span, Res::Variant(enum_id, index));
            }
            _ => {
                let shown = def.name.clone();
                self.report(
                    origin,
                    Code::UnknownVariant,
                    rhs.span,
                    format!("`{shown}` has no variant `{}` here", name.name),
                );
            }
        }
    }
}

/// The text of a string literal without interpolations (possibly parenthesised).
pub(crate) fn literal_text(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Paren(inner) => literal_text(inner),
        ExprKind::Lit(ast::Lit::Str(s)) => {
            let mut out = String::new();
            for part in &s.parts {
                match part {
                    ast::StrPart::Text(t) => out.push_str(t),
                    ast::StrPart::Interp { .. } => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// The literal text of the argument that fills a call's first parameter, if it is one.
fn first_literal(args: &[Arg], slots: &[Option<ArgSource>]) -> Option<String> {
    match slots.first() {
        Some(Some(ArgSource::Arg(k))) => literal_text(&args[*k].value),
        _ => None,
    }
}

/// Whether `session.ask`'s questions are a list written out whose every question is a record
/// written out without a `secret` field, or with `secret: false`: only then are the answers
/// known not to be secret.
fn plain_questions(args: &[Arg], slots: &[Option<ArgSource>]) -> bool {
    let Some(Some(ArgSource::Arg(k))) = slots.first() else {
        return false;
    };
    let mut e = &args[*k].value;
    while let ExprKind::Paren(inner) = &e.kind {
        e = inner;
    }
    let ExprKind::List(items) = &e.kind else {
        return false;
    };
    items.iter().all(|item| {
        let ExprKind::Record(lit) = &item.expr.kind else {
            return false;
        };
        !item.spread
            && lit.fields.iter().all(|f| match f {
                ast::FieldInit::Field { name, value } if name.name == "secret" => {
                    matches!(value.kind, ExprKind::Lit(ast::Lit::Bool(false)))
                }
                ast::FieldInit::Shorthand(name) => name.name != "secret",
                ast::FieldInit::Spread(_) => false,
                ast::FieldInit::Field { .. } => true,
            })
    })
}

/// Whether a result of type `t` can hold anything a label would protect (`()` and `Never`
/// cannot).
fn carries_data(t: TypeId) -> bool {
    !matches!(t, TypeId::UNIT | TypeId::NEVER | TypeId::ERROR)
}
