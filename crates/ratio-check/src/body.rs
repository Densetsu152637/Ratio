//! Bodies: the context an expression is checked in (local scopes, the enclosing function or
//! closure, loops, the effect log), statements, and pass 4 over the items.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ratio_names::{ModuleId, prelude};
use ratio_syntax::Span;
use ratio_syntax::ast::{self, ItemKind, Stmt, StmtKind};

use crate::check::{Checker, FnSig, ItemSig, Lazy, MAX_DEMAND_DEPTH};
use crate::diag::Code;
use crate::effects::EffectLog;
use crate::pattern::{Binder, PatCx};
use crate::tables::{ItemRef, Key, LocalId, LocalInfo, Origin, PatRes};
use crate::types::{ParamId, Ty, TypeId};

/// Which warnings declaring a local may draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Warn {
    /// A `let` or `var`: shadowing an earlier binding or a standard module.
    Let,
    /// Any other binding: shadowing a standard module.
    Plain,
}

/// What `return` does in the innermost function or closure.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ret {
    /// Not in a function: a constant, a configuration or a test.
    Forbidden,
    /// A declared result type.
    Known(TypeId),
    /// A closure's inferred result: the common type of its `return`s so far.
    Infer(Option<TypeId>),
}

/// A loop being checked: whether it takes `break` values (`loop`) and their common type.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LoopCx {
    pub(crate) value: bool,
    pub(crate) brk: Option<TypeId>,
    /// The type expected of a `loop`, which its `break` values are checked with.
    pub(crate) expected: Option<TypeId>,
}

/// A function or closure being checked.
#[derive(Clone, Debug)]
pub(crate) struct FnCx {
    pub(crate) ret: Ret,
    pub(crate) loops: Vec<LoopCx>,
}

/// The context of one body.
pub(crate) struct Body {
    pub(crate) module: ModuleId,
    pub(crate) tparams: Vec<(String, ParamId)>,
    by_name: HashMap<String, Vec<LocalId>>,
    scopes: Vec<Vec<String>>,
    pub(crate) fns: Vec<FnCx>,
    /// The function level (closure nesting) each local was declared at.
    levels: HashMap<LocalId, usize>,
    pub(crate) log: EffectLog,
}

impl Body {
    pub(crate) fn new(module: ModuleId, ret: Ret) -> Self {
        Body {
            module,
            tparams: Vec::new(),
            by_name: HashMap::new(),
            scopes: vec![Vec::new()],
            fns: vec![FnCx {
                ret,
                loops: Vec::new(),
            }],
            levels: HashMap::new(),
            log: EffectLog::default(),
        }
    }

    pub(crate) fn origin(&self) -> Origin {
        Origin::Module(self.module)
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    pub(crate) fn pop_scope(&mut self) {
        for name in self.scopes.pop().unwrap_or_default() {
            if let Some(stack) = self.by_name.get_mut(&name) {
                stack.pop();
                if stack.is_empty() {
                    self.by_name.remove(&name);
                }
            }
        }
    }

    pub(crate) fn local(&self, name: &str) -> Option<LocalId> {
        self.by_name.get(name).and_then(|s| s.last()).copied()
    }

    /// The function level of the local `id` and the current level.
    pub(crate) fn levels(&self, id: LocalId) -> (usize, usize) {
        (
            self.levels.get(&id).copied().unwrap_or(0),
            self.fns.len() - 1,
        )
    }

    pub(crate) fn fn_cx(&mut self) -> &mut FnCx {
        let last = self.fns.len() - 1;
        &mut self.fns[last]
    }
}

impl Checker<'_> {
    /// Declares a local in the innermost scope and records it.
    pub(crate) fn declare(
        &mut self,
        b: &mut Body,
        binder: &Binder,
        mutable: bool,
        warn: Warn,
    ) -> LocalId {
        let origin = b.origin();
        if warn == Warn::Let && b.local(&binder.name).is_some() {
            self.report(
                origin,
                Code::ShadowedBinding,
                binder.span,
                format!("`{}` shadows an earlier binding", binder.name),
            );
        }
        if let Some(prelude::PreludeName::Module(m)) = prelude::lookup(&binder.name) {
            self.report(
                origin,
                Code::ShadowsModule,
                binder.span,
                format!("`{}` shadows the standard module `{m}`", binder.name),
            );
        }
        let table = self.table(b.module);
        let id = LocalId(u32::try_from(table.locals.len()).unwrap_or(u32::MAX));
        table.locals.push(LocalInfo {
            name: binder.name.clone(),
            ty: binder.ty,
            mutable,
            span: binder.span,
        });
        table
            .patterns
            .insert(Key::from(binder.key), PatRes::Bind(id));
        b.by_name.entry(binder.name.clone()).or_default().push(id);
        if let Some(scope) = b.scopes.last_mut() {
            scope.push(binder.name.clone());
        }
        b.levels.insert(id, b.fns.len() - 1);
        id
    }

    /// Declares the names a pattern bound, reporting a name bound twice.
    pub(crate) fn declare_all(&mut self, b: &mut Body, binders: &[Binder], warn: Warn) {
        let mut seen: HashSet<&str> = HashSet::new();
        for binder in binders {
            if !seen.insert(&binder.name) {
                self.report(
                    b.origin(),
                    Code::Duplicate,
                    binder.span,
                    format!("`{}` is bound twice in this pattern", binder.name),
                );
                continue;
            }
            self.declare(b, binder, false, warn);
        }
    }

    pub(crate) fn local_info(&self, b: &Body, id: LocalId) -> &LocalInfo {
        &self.tables[b.module.0].locals[id.0 as usize]
    }

    // ---- statements ----

    /// Checks a block in a new scope; its value is its last statement's when that is an
    /// expression, else `()`.
    pub(crate) fn block(
        &mut self,
        b: &mut Body,
        block: &ast::Block,
        expected: Option<TypeId>,
    ) -> TypeId {
        b.push_scope();
        let mut value = TypeId::UNIT;
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let last = i + 1 == n;
            value = self.stmt(b, stmt, if last { expected } else { None });
            if !last || !matches!(stmt.kind, StmtKind::Expr(_)) {
                value = TypeId::UNIT;
            }
        }
        b.pop_scope();
        value
    }

    fn stmt(&mut self, b: &mut Body, stmt: &Stmt, expected: Option<TypeId>) -> TypeId {
        let origin = b.origin();
        match &stmt.kind {
            StmtKind::Let { pattern, ty, value } => {
                let ty = self.bound_value(b, ty.as_ref(), value);
                let mut binders = Vec::new();
                let shape = self.pattern(b, pattern, ty, PatCx::Irrefutable, &mut binders);
                self.irrefutable(b, shape, ty, pattern.span);
                self.declare_all(b, &binders, Warn::Let);
                TypeId::UNIT
            }
            StmtKind::Var { name, ty, value } => {
                let ty = self.bound_value(b, ty.as_ref(), value);
                let binder = Binder {
                    name: name.name.clone(),
                    span: name.span,
                    key: name.span,
                    ty,
                };
                self.declare(b, &binder, true, Warn::Let);
                TypeId::UNIT
            }
            StmtKind::Assign { target, op, value } => {
                self.assign(b, target, *op, value);
                TypeId::UNIT
            }
            StmtKind::Defer(block) => {
                self.block(b, block, None);
                TypeId::UNIT
            }
            StmtKind::Expr(e) => {
                let _ = origin;
                self.expr(b, e, expected)
            }
        }
    }

    /// The type a `let` or `var` binds: the annotation (the value checked against it), or the
    /// value's own type.
    fn bound_value(&mut self, b: &mut Body, ty: Option<&ast::Type>, value: &ast::Expr) -> TypeId {
        match ty {
            Some(t) => {
                let t = self.lower_type(b.origin(), &b.tparams.clone(), t);
                self.expect(b, value, t);
                t
            }
            None => self.expr(b, value, None),
        }
    }

    fn assign(&mut self, b: &mut Body, target: &ast::Expr, op: ast::AssignOp, value: &ast::Expr) {
        let origin = b.origin();
        let mut root = target;
        while let ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. } =
            &root.kind
        {
            root = object;
        }
        let place = self.expr(b, target, None);
        if let ast::ExprKind::Name(name) = &root.kind {
            match b.local(&name.name) {
                Some(id) if !self.local_info(b, id).mutable => self.report(
                    origin,
                    Code::NotAssignable,
                    name.span,
                    format!("`{}` is not a `var`, so it cannot be assigned", name.name),
                ),
                Some(id) => {
                    let (declared, now) = b.levels(id);
                    if declared < now {
                        self.report(
                            origin,
                            Code::NotAssignable,
                            name.span,
                            format!(
                                "`{}` is captured by this closure, which cannot assign to it",
                                name.name
                            ),
                        );
                    }
                }
                None => self.report(
                    origin,
                    Code::NotAssignable,
                    name.span,
                    format!("`{}` is not a local `var`", name.name),
                ),
            }
        }
        if op == ast::AssignOp::Set {
            self.expect(b, value, place);
            return;
        }
        let right = self.expr(b, value, Some(place));
        let binop = match op {
            ast::AssignOp::Add => ast::BinaryOp::Add,
            ast::AssignOp::Sub => ast::BinaryOp::Sub,
            ast::AssignOp::Mul => ast::BinaryOp::Mul,
            _ => ast::BinaryOp::Div,
        };
        let result = self.arith(b, binop, place, right, value.span);
        self.mismatch(b, value.span, result, place);
    }

    // ---- pass 4: item bodies ----

    pub(crate) fn bodies(&mut self) {
        for m in 0..self.loaded.modules.len() {
            if !self.checked[m] {
                continue;
            }
            let module = ModuleId(m);
            let origin = Origin::Module(module);
            for index in 0..self.items(origin).len() {
                let r = ItemRef { origin, index };
                self.item_body(module, r);
            }
        }
    }

    fn item_body(&mut self, module: ModuleId, r: ItemRef) {
        let item = self.item(r);
        match &item.kind {
            ItemKind::Const(_) => {
                if !self.logs.contains_key(&r) {
                    self.check_const(r, item.span);
                }
            }
            ItemKind::State(decl) => {
                let mut b = Body::new(module, Ret::Forbidden);
                let ty = self.lower_type(r.origin, &[], &decl.ty);
                self.expect(&mut b, &decl.value, ty);
                self.logs.insert(r, b.log);
            }
            ItemKind::Fn(decl) => {
                let Some(sig) = self.fn_sig(r) else {
                    return;
                };
                let mut b = Body::new(module, Ret::Known(sig.ret));
                b.tparams = decl
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .zip(sig.type_params.iter().copied())
                    .collect();
                self.params(&mut b, &decl.params, &sig);
                self.expect_block(&mut b, &decl.body, sig.ret);
                self.logs.insert(r, b.log);
            }
            ItemKind::Harness(decl) => {
                let Some(sig) = self.fn_sig(r) else {
                    return;
                };
                // One body binds the parameters for both: the configuration first, with no
                // `return` and its own effect log, then the defaults and the body.
                let mut b = Body::new(module, Ret::Forbidden);
                self.bind_params(&mut b, &decl.params, &sig);
                if let Some(config) = &decl.config {
                    let want = self.known_type("HarnessConfig");
                    let got = self.record_lit(&mut b, config, Some(want), Code::RecordFields);
                    self.mismatch(&b, config.span, got, want);
                    self.config_logs.insert(r, std::mem::take(&mut b.log));
                }
                b.fns[0].ret = Ret::Known(sig.ret);
                self.check_defaults(&mut b, &decl.params, &sig);
                self.expect_block(&mut b, &decl.body, sig.ret);
                self.logs.insert(r, b.log);
            }
            ItemKind::Prompt(_) => {
                self.prompt_type(r, item.span);
            }
            ItemKind::Test(decl) => {
                let mut b = Body::new(module, Ret::Forbidden);
                self.block(&mut b, &decl.body, None);
            }
            ItemKind::Import(_) | ItemKind::Type(_) | ItemKind::Enum(_) => {}
        }
    }

    pub(crate) fn fn_sig(&self, r: ItemRef) -> Option<Rc<FnSig>> {
        match self.sig(r) {
            ItemSig::Fn(sig) => Some(sig.clone()),
            _ => None,
        }
    }

    /// Binds a declaration's parameters (and checks their defaults).
    fn params(&mut self, b: &mut Body, params: &[ast::Param], sig: &FnSig) {
        self.bind_params(b, params, sig);
        self.check_defaults(b, params, sig);
    }

    fn bind_params(&mut self, b: &mut Body, params: &[ast::Param], sig: &FnSig) {
        for (p, ps) in params.iter().zip(sig.params.iter()) {
            let binder = Binder {
                name: p.name.name.clone(),
                span: p.name.span,
                key: p.name.span,
                ty: ps.ty,
            };
            self.declare(b, &binder, false, Warn::Plain);
        }
    }

    fn check_defaults(&mut self, b: &mut Body, params: &[ast::Param], sig: &FnSig) {
        for (p, ps) in params.iter().zip(sig.params.iter()) {
            if let Some(default) = &p.default {
                self.expect(b, default, ps.ty);
            }
        }
    }

    /// Whether `shapes` cover type `ty`. After one check over the bound (reported), the rest are
    /// skipped: the package is refused already, and each would spend the budget again.
    pub(crate) fn exhaustive(
        &mut self,
        shapes: Vec<crate::exhaust::DPat>,
        ty: TypeId,
    ) -> crate::exhaust::Outcome {
        use crate::exhaust::{Exhaust, Outcome};
        if self.exhaust_refused || self.types.resolve(ty) == TypeId::ERROR {
            return Outcome::Exhaustive;
        }
        let depth = self.max_depth * 4;
        let outcome = Exhaust::new(&mut self.types, depth).check(shapes, ty);
        if outcome == Outcome::TooComplex {
            self.exhaust_refused = true;
        }
        outcome
    }

    /// Checks a body block against its declared result type.
    fn expect_block(&mut self, b: &mut Body, block: &ast::Block, ret: TypeId) {
        let got = self.block(b, block, Some(ret));
        if !self.types.subtype(&mut self.memo, got, ret) {
            let at = block.stmts.last().map_or(block.span, |s| s.span);
            let (got, want) = (self.types.display(got), self.types.display(ret));
            self.report(
                b.origin(),
                Code::Mismatch,
                at,
                format!("expected `{want}` as the result, found `{got}`"),
            );
        }
    }

    /// A constant's type: its annotation, or (checked on first use) its value's.
    pub(crate) fn const_type(&mut self, r: ItemRef, at: Span) -> TypeId {
        match self.sig(r) {
            ItemSig::Const(Lazy::Done(t)) => *t,
            ItemSig::Const(Lazy::Todo) => self.check_const(r, at),
            _ => TypeId::ERROR,
        }
    }

    pub(crate) fn check_const(&mut self, r: ItemRef, at: Span) -> TypeId {
        let ItemKind::Const(decl) = &self.item(r).kind else {
            return TypeId::ERROR;
        };
        let Origin::Module(module) = r.origin else {
            return TypeId::ERROR;
        };
        if self.demand_depth >= MAX_DEMAND_DEPTH {
            self.report(
                r.origin,
                Code::TooComplex,
                at,
                format!(
                    "constants depend on each other more than {MAX_DEMAND_DEPTH} deep; \
                     annotate `{}`'s type",
                    decl.name.name
                ),
            );
            self.set_sig(r, ItemSig::Const(Lazy::Done(TypeId::ERROR)));
            self.logs.insert(r, EffectLog::default());
            return TypeId::ERROR;
        }
        self.set_sig(r, ItemSig::Const(Lazy::Busy));
        self.demand_depth += 1;
        let mut b = Body::new(module, Ret::Forbidden);
        let ty = match &decl.ty {
            Some(t) => {
                let t = self.lower_type(r.origin, &[], t);
                self.set_sig(r, ItemSig::Const(Lazy::Done(t)));
                self.expect(&mut b, &decl.value, t);
                t
            }
            None => self.expr(&mut b, &decl.value, None),
        };
        self.demand_depth -= 1;
        self.set_sig(r, ItemSig::Const(Lazy::Done(ty)));
        self.tables[module.0].items[r.index] = Some(ty);
        self.logs.insert(r, b.log);
        ty
    }

    /// A prompt declaration's type, `TrustedPrompt` or `Prompt`, from its body (checked on
    /// first use).
    pub(crate) fn prompt_type(&mut self, r: ItemRef, at: Span) -> TypeId {
        match self.prompt_types.get(&r).copied() {
            Some(Lazy::Done(t)) => return t,
            Some(Lazy::Busy) | None => return TypeId::PROMPT,
            Some(Lazy::Todo) => {}
        }
        let (ItemKind::Prompt(decl), Origin::Module(module)) = (&self.item(r).kind, r.origin)
        else {
            return TypeId::PROMPT;
        };
        let Some(sig) = self.fn_sig(r) else {
            return TypeId::PROMPT;
        };
        if self.demand_depth >= MAX_DEMAND_DEPTH {
            self.report(
                r.origin,
                Code::TooComplex,
                at,
                format!("prompts include each other more than {MAX_DEMAND_DEPTH} deep"),
            );
            self.prompt_types.insert(r, Lazy::Done(TypeId::PROMPT));
            return TypeId::PROMPT;
        }
        self.prompt_types.insert(r, Lazy::Busy);
        self.demand_depth += 1;
        let mut b = Body::new(module, Ret::Forbidden);
        self.params(&mut b, &decl.params, &sig);
        let ty = self.prompt_body(&mut b, &decl.body);
        self.demand_depth -= 1;
        self.prompt_types.insert(r, Lazy::Done(ty));
        let params = sig.params.iter().map(|p| p.ty).collect();
        self.tables[module.0].items[r.index] = Some(self.types.function(params, ty));
        self.logs.insert(r, b.log);
        ty
    }

    /// A shared type of the prelude by name (the checker's own declarations; always there).
    pub(crate) fn known_type(&mut self, name: &str) -> TypeId {
        let Some(&index) = self.std[0].scope.get(name) else {
            return TypeId::ERROR;
        };
        let r = ItemRef {
            origin: Origin::Std(0),
            index,
        };
        let span = self.item(r).span;
        let args = match self.sig(r) {
            ItemSig::Enum(id) if !self.types.enum_def(*id).params.is_empty() => {
                return TypeId::ERROR;
            }
            _ => Vec::new(),
        };
        self.item_type(Origin::Std(0), r, args, span)
    }

    /// Replaces inference variables in the recorded types by their bindings (or by the error
    /// type, where inference failed and was reported).
    pub(crate) fn finish_tables(&mut self) {
        for m in 0..self.tables.len() {
            let mut table = std::mem::take(&mut self.tables[m]);
            for t in table.types.values_mut() {
                *t = self.finish(*t);
            }
            for local in &mut table.locals {
                local.ty = self.finish(local.ty);
            }
            for call in table.calls.values_mut() {
                for t in &mut call.type_args {
                    *t = self.finish(*t);
                }
            }
            self.tables[m] = table;
        }
    }

    fn finish(&mut self, t: TypeId) -> TypeId {
        let t = self.types.resolve(t);
        if !self.types.has_vars(t) {
            return t;
        }
        let mut leaf = |_: &mut crate::types::Types, ty: &Ty| match ty {
            Ty::Var(_) => Some(TypeId::ERROR),
            _ => None,
        };
        self.types.rebuild(t, &mut leaf, &mut HashMap::new())
    }
}

/// Whether `e` is a closure literal (possibly parenthesised).
pub(crate) fn is_closure(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Closure { .. } | ast::ExprKind::FnExpr { .. } => true,
        ast::ExprKind::Paren(inner) => is_closure(inner),
        _ => false,
    }
}
