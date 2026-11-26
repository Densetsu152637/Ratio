//! The guarded-effect lint (chapter 03, "What the guarantee covers"): a warning (`C0039`)
//! where a condition derived from an untrusted value (a model-derived scalar) is the only
//! guard on an effect that writes, runs a command, delegates, reaches the network or runs a
//! tool a model chooses. Taint tracks explicit flows only; this shows a reviewer each place
//! where a model's output alone decides such an effect. `@allow(model_guard)` on the item
//! silences it there (statements take no attributes, chapter 02).
//!
//! **Derived** values, over-approximated (the safe direction for a warning):
//!
//! - a value whose type may hold untrusted text (`Untrusted<…>`, `Brief`, `Json`, a type
//!   parameter), and anything computed from a derived value, including the value of an `if`
//!   or `match` whose condition is derived (implicit flow);
//! - the result of a built-in that makes effects (model and tool output), unless its result
//!   is the person's answer (`session.request_approval`, `session.ask`); of a harness; of a
//!   function that makes effects; and of any call of a function value (`declassify`'s result
//!   is as derived as its argument: the person approves removing the label, not the value's
//!   origin);
//! - a local bound from a derived value, a `var` assigned one or assigned under a derived
//!   condition, a pattern variable of a derived scrutinee, a closure's parameters (their
//!   values come from wherever the closure is called), and a caught error.
//!
//! A function's or harness's own parameters count by their type only: a `Bool` parameter is
//! not derived, even when a caller passes a model's answer (the call site, if itself under a
//! model-derived guard, is where that is reported).
//!
//! **Guards** are the conditions of `if`, `if let` and `while`, `match` and `catch` arms (a
//! refutable pattern of a derived scrutinee, and arm guards), and the left side of `and`, `or`
//! and `??` for their right side. A condition is *model-only* when it is derived and no
//! conjunct of it is trusted: `if v.pass and approved` has a trusted guard as well. An effect is
//! reported when it has at least one guard in its item and every one is model-only. A
//! closure's body counts as running where it is written.
//!
//! Locals can become derived after their first use (a `var` in a loop), so each item is walked
//! until nothing changes, at most [`MAX_ROUNDS`] times; past that every local of the item counts
//! as derived. Each round is linear in the item, so the lint is linear in the program.

use std::collections::HashSet;

use ratio_names::ModuleId;
use ratio_syntax::Span;
use ratio_syntax::ast::{
    self, BinaryOp, Block, Catch, Condition, Else, Expr, ExprKind, FieldInit, ItemKind,
    PatternKind, StmtKind, UnaryOp,
};

use crate::analysis::{Effects, Results};
use crate::check::{Checker, FnKind, ItemSig};
use crate::diag::Code;
use crate::tables::{ArgSource, Key, LocalId, Origin, PatRes, Res};
use crate::types::Ty;

/// Why a generic declaration instantiated at a secret is refused.
const SECRET_INSTANCE: &str = "a generic function that may reach a secrecy sink cannot be \
     instantiated at a type holding a secret: its body cannot see the label (`declassify` asks \
     the user)";

/// How many times an item is walked for its derived locals to settle.
pub const MAX_ROUNDS: usize = 8;

/// The lint's state over one item.
struct St<'r> {
    module: ModuleId,
    results: &'r Results,
    /// Every binding of the module, by its pattern's span start.
    binds: &'r [(usize, usize, LocalId)],
    derived: HashSet<LocalId>,
    /// The enclosing guards: whether each is derived, and whether it is model-only.
    guards: Vec<(bool, bool)>,
    changed: bool,
    /// Whether this walk reports (the last one).
    collect: bool,
    warnings: Vec<(Span, String)>,
    /// Secrets passed where the callee cannot see their label (errors, not silenced).
    errors: Vec<(Span, String)>,
    depth: usize,
}

impl Checker<'_> {
    /// Runs the lint over every checked module.
    pub(crate) fn lint(&mut self, results: &Results) {
        let mut found: Vec<(ModuleId, Span, String)> = Vec::new();
        let mut errors: Vec<(ModuleId, Span, String)> = Vec::new();
        for m in 0..self.loaded.modules.len() {
            if !self.checked[m] {
                continue;
            }
            let module = ModuleId(m);
            let mut binds: Vec<(usize, usize, LocalId)> = self.tables[m]
                .patterns
                .iter()
                .filter_map(|(k, p)| match p {
                    PatRes::Bind(l) => Some((k.start, k.end, *l)),
                    PatRes::Variant(..) => None,
                })
                .collect();
            binds.sort_unstable();
            for item in &self.loaded.modules[m].ast.items {
                let allowed = item.attrs.iter().any(|a| {
                    a.name.name == "allow"
                        && a.args.iter().flatten().any(|x| {
                            matches!(&x.value.kind, ExprKind::Name(id) if id.name == "model_guard")
                        })
                });
                let mut st = St {
                    module,
                    results,
                    binds: &binds,
                    derived: HashSet::new(),
                    guards: Vec::new(),
                    changed: false,
                    collect: false,
                    warnings: Vec::new(),
                    errors: Vec::new(),
                    depth: 0,
                };
                let mut settled = false;
                for _ in 0..MAX_ROUNDS {
                    st.changed = false;
                    self.lint_item(&mut st, &item.kind);
                    if !st.changed {
                        settled = true;
                        break;
                    }
                }
                if !settled {
                    for (start, end, l) in &binds {
                        if *start >= item.span.start && *end <= item.span.end {
                            st.derived.insert(*l);
                        }
                    }
                }
                st.collect = true;
                self.lint_item(&mut st, &item.kind);
                errors.extend(st.errors.into_iter().map(|(s, w)| (module, s, w)));
                if !allowed {
                    found.extend(st.warnings.into_iter().map(|(s, w)| (module, s, w)));
                }
            }
        }
        for (module, span, message) in found {
            self.report(Origin::Module(module), Code::ModelGuard, span, message);
        }
        // Generic declarations named as values at a type holding a secret.
        for (module, span, r, targs) in &results.value_uses {
            let may_sink = match r.origin {
                Origin::Module(m) => results
                    .summaries
                    .get(&(m, r.index))
                    .is_none_or(|s| s.effects.intersects(Effects::SINK)),
                Origin::Std(_) => true,
            };
            if may_sink && targs.iter().any(|t| self.types.secret_inside(*t)) {
                errors.push((*module, *span, SECRET_INSTANCE.to_owned()));
            }
        }
        errors.sort_by_key(|(m, s, _)| (*m, s.start, s.end));
        errors.dedup_by_key(|(m, s, _)| (*m, s.start, s.end));
        for (module, span, message) in errors {
            self.report(Origin::Module(module), Code::Secret, span, message);
        }
    }

    fn lint_item(&self, st: &mut St<'_>, kind: &ItemKind) {
        match kind {
            ItemKind::Fn(decl) => {
                for p in &decl.params {
                    if let Some(d) = &p.default {
                        self.ex(st, d);
                    }
                }
                self.lint_block(st, &decl.body);
            }
            ItemKind::Harness(decl) => {
                if let Some(config) = &decl.config {
                    self.fields(st, &config.fields);
                }
                for p in &decl.params {
                    if let Some(d) = &p.default {
                        self.ex(st, d);
                    }
                }
                self.lint_block(st, &decl.body);
            }
            ItemKind::Const(decl) => {
                self.ex(st, &decl.value);
            }
            ItemKind::State(decl) => {
                self.ex(st, &decl.value);
            }
            ItemKind::Prompt(decl) => {
                self.prompt(st, &decl.body);
            }
            ItemKind::Test(decl) => {
                self.lint_block(st, &decl.body);
            }
            ItemKind::Import(_) | ItemKind::Type(_) | ItemKind::Enum(_) => {}
        }
    }

    /// Marks every binding inside `span` derived.
    fn bind(st: &mut St<'_>, span: Span) {
        let from = st.binds.partition_point(|(s, _, _)| *s < span.start);
        for (start, end, l) in &st.binds[from..] {
            if *start > span.end {
                break;
            }
            if *end <= span.end && st.derived.insert(*l) {
                st.changed = true;
            }
        }
    }

    /// Whether the expression at `span` has a type that may hold untrusted text (function
    /// types aside: a function is not data).
    fn tainted_at(&self, st: &St<'_>, span: Span) -> bool {
        self.tables[st.module.0]
            .types
            .get(&Key::from(span))
            .is_some_and(|t| {
                !matches!(self.types.get(*t), Ty::Fn(..) | Ty::Harness(..))
                    && self.types.tainted(*t)
            })
    }

    fn lint_block(&self, st: &mut St<'_>, block: &Block) -> bool {
        let mut last = false;
        for (i, stmt) in block.stmts.iter().enumerate() {
            let value = match &stmt.kind {
                StmtKind::Let { pattern, value, .. } => {
                    if self.ex(st, value) {
                        Self::bind(st, pattern.span);
                    }
                    false
                }
                StmtKind::Var { name, value, .. } => {
                    if self.ex(st, value) {
                        Self::bind(st, name.span);
                    }
                    false
                }
                StmtKind::Assign { target, value, .. } => {
                    let derived = self.ex(st, value) || st.guards.iter().any(|(d, _)| *d);
                    self.ex(st, target);
                    if derived {
                        let mut root = target;
                        while let ExprKind::Member { object, .. } | ExprKind::Index { object, .. } =
                            &root.kind
                        {
                            root = object;
                        }
                        let local = match self.tables[st.module.0].res.get(&Key::from(root.span)) {
                            Some(Res::Local(l)) => Some(*l),
                            _ => None,
                        };
                        if local.is_some_and(|l| st.derived.insert(l)) {
                            st.changed = true;
                        }
                    }
                    false
                }
                StmtKind::Defer(b) => {
                    self.lint_block(st, b);
                    false
                }
                StmtKind::Expr(e) => self.ex(st, e),
            };
            if i + 1 == block.stmts.len() {
                last = value;
            }
        }
        last
    }

    /// Whether `e` is derived.
    fn ex(&self, st: &mut St<'_>, e: &Expr) -> bool {
        if st.depth >= self.max_depth {
            return true;
        }
        st.depth += 1;
        let derived = self.ex_inner(st, e);
        st.depth -= 1;
        derived || self.tainted_at(st, e.span)
    }

    /// A condition: whether it is derived, and whether it is model-only.
    fn cond(&self, st: &mut St<'_>, e: &Expr) -> (bool, bool) {
        if st.depth >= self.max_depth {
            return (true, true);
        }
        st.depth += 1;
        let out = self.cond_inner(st, e);
        st.depth -= 1;
        out
    }

    fn cond_inner(&self, st: &mut St<'_>, e: &Expr) -> (bool, bool) {
        match &e.kind {
            ExprKind::Paren(inner) => self.cond(st, inner),
            ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } => self.cond(st, operand),
            ExprKind::Binary {
                op: op @ (BinaryOp::And | BinaryOp::Or),
                lhs,
                rhs,
            } => {
                let (d1, m1) = self.cond(st, lhs);
                st.guards.push((d1, m1));
                let (d2, m2) = self.cond(st, rhs);
                st.guards.pop();
                let model = if *op == BinaryOp::And {
                    m1 && m2
                } else {
                    m1 || m2
                };
                (d1 || d2, model)
            }
            _ => {
                let d = self.ex(st, e);
                (d, d)
            }
        }
    }

    fn guarded<T>(st: &mut St<'_>, guard: (bool, bool), f: impl FnOnce(&mut St<'_>) -> T) -> T {
        st.guards.push(guard);
        let out = f(st);
        st.guards.pop();
        out
    }

    fn fields(&self, st: &mut St<'_>, fields: &[FieldInit]) -> bool {
        let mut d = false;
        for f in fields {
            d |= match f {
                FieldInit::Field { value, .. } | FieldInit::Spread(value) => self.ex(st, value),
                FieldInit::Shorthand(id) => {
                    matches!(
                        self.tables[st.module.0].res.get(&Key::from(id.span)),
                        Some(Res::Local(l)) if st.derived.contains(l)
                    ) || self.tainted_at(st, id.span)
                }
            };
        }
        d
    }

    fn prompt(&self, st: &mut St<'_>, body: &ast::PromptBody) -> bool {
        if st.depth >= self.max_depth {
            return true;
        }
        st.depth += 1;
        let d = self.prompt_inner(st, body);
        st.depth -= 1;
        d
    }

    fn prompt_inner(&self, st: &mut St<'_>, body: &ast::PromptBody) -> bool {
        use ast::SectionKind as S;
        let mut d = false;
        for section in &body.sections {
            d |= match &section.kind {
                S::System(e)
                | S::Instruct(e)
                | S::Task(e)
                | S::Note(e)
                | S::Steps(e)
                | S::Context(e)
                | S::Include(e) => self.ex(st, e),
                S::Data { value, label } | S::Image { value, label } => {
                    let v = self.ex(st, value);
                    label.as_ref().is_some_and(|l| self.ex(st, l)) || v
                }
                S::Example { input, output } => {
                    let a = self.ex(st, input);
                    self.ex(st, output) || a
                }
                S::Format(_) => false,
                S::If { cond, then, else_ } => {
                    let g = self.cond(st, cond);
                    Self::guarded(st, g, |st| {
                        let a = self.prompt(st, then);
                        else_.as_ref().is_some_and(|x| self.prompt(st, x)) || a
                    }) || g.0
                }
                S::For {
                    pattern,
                    iter,
                    body,
                } => {
                    if self.ex(st, iter) {
                        Self::bind(st, pattern.span);
                    }
                    self.prompt(st, body)
                }
            };
        }
        d
    }

    /// Whether a pattern always matches (a binding or `_`), so it is no guard.
    fn catch_all(&self, st: &St<'_>, p: &ast::Pattern) -> bool {
        match &p.kind {
            PatternKind::Wildcard => true,
            PatternKind::Name(_) => matches!(
                self.tables[st.module.0].patterns.get(&Key::from(p.span)),
                Some(PatRes::Bind(_))
            ),
            _ => false,
        }
    }

    /// The arms of a `match` or `catch` over a scrutinee that is derived or not.
    fn arms(&self, st: &mut St<'_>, scrutinee: bool, arms: &[ast::Arm]) -> bool {
        let mut d = scrutinee;
        for arm in arms {
            if scrutinee {
                Self::bind(st, arm.pattern.span);
            }
            let catch_all = self.catch_all(st, &arm.pattern);
            let guard = arm.guard.as_ref().map(|g| self.cond(st, g));
            let pushed = match (catch_all, guard) {
                (true, None) => None,
                (true, Some(g)) => Some(g),
                (false, None) => Some((scrutinee, scrutinee)),
                (false, Some((gd, gm))) => Some((scrutinee || gd, scrutinee && gm)),
            };
            d |= match pushed {
                Some(g) => Self::guarded(st, g, |st| self.ex(st, &arm.body)),
                None => self.ex(st, &arm.body),
            };
        }
        d
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of expression")]
    fn ex_inner(&self, st: &mut St<'_>, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Lit(ast::Lit::Str(s)) => {
                let mut d = false;
                for part in &s.parts {
                    if let ast::StrPart::Interp { expr, .. } = part {
                        d |= self.ex(st, expr);
                    }
                }
                d
            }
            ExprKind::Lit(_) | ExprKind::Error | ExprKind::Continue => false,
            ExprKind::Name(_) => matches!(
                self.tables[st.module.0].res.get(&Key::from(e.span)),
                Some(Res::Local(l)) if st.derived.contains(l)
            ),
            ExprKind::Paren(x) | ExprKind::Raise(x) => self.ex(st, x),
            ExprKind::Return(x) | ExprKind::Break(x) => x.as_ref().is_some_and(|x| self.ex(st, x)),
            // Every part is walked (for its effects), so no short circuit.
            ExprKind::Tuple(xs) | ExprKind::Par(xs) | ExprKind::Race(xs) => {
                let mut d = false;
                for x in xs {
                    d |= self.ex(st, x);
                }
                d
            }
            ExprKind::List(items) => {
                let mut d = false;
                for x in items {
                    d |= self.ex(st, &x.expr);
                }
                d
            }
            ExprKind::Record(lit) | ExprKind::Variant { fields: lit, .. } => {
                self.fields(st, &lit.fields)
            }
            ExprKind::Block(b) | ExprKind::Do(b) | ExprKind::Loop(b) => self.lint_block(st, b),
            ExprKind::Unary { operand, .. } => self.ex(st, operand),
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinaryOp::Pipe => {
                    let left = self.ex(st, lhs);
                    match &rhs.kind {
                        ExprKind::Call { callee, args, .. } => {
                            self.lint_call(st, e.span, Some(callee), args, (left, Some(lhs.span)))
                        }
                        _ => self.lint_call(st, e.span, Some(rhs), &[], (left, Some(lhs.span))),
                    }
                }
                BinaryOp::And | BinaryOp::Or | BinaryOp::Coalesce => {
                    let g = self.cond(st, lhs);
                    Self::guarded(st, g, |st| self.ex(st, rhs)) || g.0
                }
                _ => {
                    let a = self.ex(st, lhs);
                    self.ex(st, rhs) || a
                }
            },
            ExprKind::Call { callee, args, .. } => {
                self.lint_call(st, e.span, Some(callee), args, (false, None))
            }
            ExprKind::Member { object, .. } => {
                let module = matches!(
                    self.tables[st.module.0].res.get(&Key::from(object.span)),
                    Some(Res::Module(_))
                );
                !module && self.ex(st, object)
            }
            ExprKind::Index { object, index } => {
                let a = self.ex(st, object);
                self.ex(st, index) || a
            }
            ExprKind::WithOverride { call, config } => {
                let a = self.ex(st, call);
                self.fields(st, &config.fields) || a
            }
            ExprKind::If { cond, then, else_ } => {
                let g = match &**cond {
                    Condition::Expr(c) => self.cond(st, c),
                    Condition::Let { pattern, value } => {
                        let d = self.ex(st, value);
                        if d {
                            Self::bind(st, pattern.span);
                        }
                        (d, d)
                    }
                };
                Self::guarded(st, g, |st| {
                    let a = self.lint_block(st, then);
                    let b = match else_.as_deref() {
                        Some(Else::If(x)) => self.ex(st, x),
                        Some(Else::Block(b)) => self.lint_block(st, b),
                        None => false,
                    };
                    a || b
                }) || g.0
            }
            ExprKind::Match { scrutinee, arms } => {
                let s = self.ex(st, scrutinee);
                self.arms(st, s, arms)
            }
            ExprKind::For {
                pattern,
                iter,
                body,
            } => {
                if self.ex(st, iter) {
                    Self::bind(st, pattern.span);
                }
                self.lint_block(st, body);
                false
            }
            ExprKind::While { cond, body } => {
                let g = match &**cond {
                    Condition::Expr(c) => self.cond(st, c),
                    Condition::Let { pattern, value } => {
                        let d = self.ex(st, value);
                        if d {
                            Self::bind(st, pattern.span);
                        }
                        (d, d)
                    }
                };
                Self::guarded(st, g, |st| self.lint_block(st, body));
                false
            }
            ExprKind::Repeat { count, body } => {
                self.ex(st, count);
                self.lint_block(st, body);
                false
            }
            ExprKind::Try { body, catch } => {
                let d = self.lint_block(st, body);
                // A caught error comes from an effect: what it says is derived.
                let c = match catch {
                    Catch::Arms(arms) => self.arms(st, true, arms),
                    Catch::Bind { name, body } => {
                        Self::bind(st, name.span);
                        Self::guarded(st, (true, true), |st| self.lint_block(st, body));
                        true
                    }
                };
                d || c
            }
            ExprKind::WithTimeout {
                duration,
                body,
                else_,
            } => {
                self.ex(st, duration);
                let a = self.lint_block(st, body);
                else_.as_ref().is_some_and(|b| self.lint_block(st, b)) || a
            }
            ExprKind::With { config, body } => {
                self.fields(st, &config.fields);
                self.lint_block(st, body)
            }
            ExprKind::Closure { params, body } => {
                for p in params {
                    Self::bind(st, p.pattern.span);
                }
                self.ex(st, body);
                false
            }
            ExprKind::FnExpr { params, body, .. } => {
                for p in params {
                    if let Some(d) = &p.default {
                        self.ex(st, d);
                    }
                }
                self.lint_block(st, body);
                false
            }
            ExprKind::Prompt(body) => self.prompt(st, body),
        }
    }

    /// A call: walks its parts, reports it if it is a watched effect under model-only guards,
    /// and says whether its result is derived.
    fn lint_call(
        &self,
        st: &mut St<'_>,
        span: Span,
        callee: Option<&Expr>,
        args: &[ast::Arg],
        (receiver, lhs): (bool, Option<Span>),
    ) -> bool {
        let mut d = receiver;
        if let Some(c) = callee {
            d |= match &c.kind {
                ExprKind::Member { object, .. } => {
                    let module = matches!(
                        self.tables[st.module.0].res.get(&Key::from(object.span)),
                        Some(Res::Module(_))
                    );
                    !module && self.ex(st, object)
                }
                ExprKind::Name(_) => self.ex_inner(st, c),
                _ => self.ex(st, c),
            };
        }
        for a in args {
            d |= self.ex(st, &a.value);
        }
        let info = self.tables[st.module.0].calls.get(&Key::from(span));
        let (effects, derived) = if let Some(r) = info.and_then(|i| i.callee) {
            match (r.origin, self.sig(r)) {
                (Origin::Std(_), ItemSig::Fn(sig)) => {
                    let pure = matches!(sig.kind, FnKind::Builtin { pure: true });
                    (sig.traits.effects, d || (!pure && !sig.traits.user))
                }
                (Origin::Module(m), ItemSig::Fn(sig)) => {
                    let effects = st
                        .results
                        .summaries
                        .get(&(m, r.index))
                        .map_or(Effects::ALL, |s| s.effects);
                    let harness = sig.kind == FnKind::Harness;
                    (effects, d || harness || !effects.is_empty())
                }
                _ => (Effects::NONE, true),
            }
        } else {
            let harness = callee
                .and_then(|c| self.tables[st.module.0].types.get(&Key::from(c.span)))
                .is_some_and(|t| matches!(self.types.get(*t), Ty::Harness(..)));
            (if harness { Effects::ALL } else { Effects::NONE }, true)
        };
        if st.collect
            && effects.intersects(Effects::GUARDED)
            && !st.guards.is_empty()
            && st.guards.iter().all(|(_, model)| *model)
        {
            let kinds: Vec<&str> = effects
                .names()
                .into_iter()
                .filter(|n| !matches!(*n, "model" | "other"))
                .collect();
            st.warnings.push((
                span,
                format!(
                    "this effect ({}) is guarded only by a condition derived from model or tool \
                     output; check the decision is meant to be the model's \
                     (`@allow(model_guard)` on the item silences this)",
                    kinds.join(", ")
                ),
            ));
        }
        if st.collect && effects.intersects(Effects::SINK) {
            self.secret_into_generic(st, span, callee, args, lhs, info);
        }
        derived
    }

    /// A statically secret value passed for a generic parameter of a declaration that may
    /// reach a secrecy sink ([`Effects::SINK`]) is refused (`C0037`): the callee's body is
    /// checked at its type parameter, which carries no label, so it cannot refuse the sink
    /// itself. A call whose type arguments hold a secret some other way (from the expected
    /// type, a turbofish) is refused at the call. (A parameter declared `Secret<…>` is checked
    /// in the callee; the built-in secrecy sinks refuse their own arguments.)
    fn secret_into_generic(
        &self,
        st: &mut St<'_>,
        span: Span,
        callee: Option<&Expr>,
        args: &[ast::Arg],
        lhs: Option<Span>,
        info: Option<&crate::tables::CallInfo>,
    ) {
        let Some(info) = info else {
            return;
        };
        let Some(r) = info.callee else {
            return;
        };
        let (Origin::Module(_), ItemSig::Fn(sig)) = (r.origin, self.sig(r)) else {
            return;
        };
        let table = &self.tables[st.module.0];
        let mut found = false;
        for (param, source) in sig.params.iter().zip(&info.args) {
            if !self.types.has_params(param.ty) {
                continue;
            }
            let at = match source {
                ArgSource::Arg(k) => args.get(*k).map(|a| a.value.span),
                ArgSource::Receiver => lhs.or(match callee.map(|c| &c.kind) {
                    Some(ExprKind::Member { object, .. }) => Some(object.span),
                    _ => None,
                }),
                ArgSource::Default => None,
            };
            let Some(at) = at else {
                continue;
            };
            if table
                .types
                .get(&Key::from(at))
                .is_some_and(|t| self.types.secret_inside(*t))
            {
                found = true;
                st.errors.push((
                    at,
                    format!(
                        "a secret cannot be passed for the generic parameter `{}` of a function \
                         that may reach a secrecy sink (a model, a tool, the network, a \
                         command, a write, delegation): it could send the secret on \
                         (`declassify` asks the user)",
                        param.name
                    ),
                ));
            }
        }
        if !found && info.type_args.iter().any(|t| self.types.secret_inside(*t)) {
            st.errors.push((span, SECRET_INSTANCE.to_owned()));
        }
    }
}
