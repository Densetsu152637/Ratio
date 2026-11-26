//! Expressions, checked bidirectionally: each is checked with the type expected where it
//! stands, when there is one, which literals, closures, records, lists, `none`, contextual
//! variants and generic calls use; the result is the expression's own type, which the caller
//! relates to what it expected.

use std::collections::HashSet;

use ratio_syntax::Span;
use ratio_syntax::ast::{
    self, BinaryOp, Catch, Condition, Else, Expr, ExprKind, FieldInit, Lit, RecordLit, StrPart,
    UnaryOp,
};

use crate::body::{Body, FnCx, LoopCx, Ret, Warn};
use crate::check::Checker;
use crate::diag::Code;
use crate::exhaust::Outcome;
use crate::pattern::{Binder, PatCx, quote};
use crate::tables::{Coercion, Key};
use crate::types::{Erase, Field, Ty, TypeId};

impl Checker<'_> {
    /// Checks `e`, with the type expected where it stands if there is one, and records its
    /// type.
    pub(crate) fn expr(&mut self, b: &mut Body, e: &Expr, expected: Option<TypeId>) -> TypeId {
        let origin = b.origin();
        if !self.enter(origin, e.span) {
            return TypeId::ERROR;
        }
        let expected = expected.map(|t| self.types.resolve(t));
        let ty = self.expr_inner(b, e, expected);
        self.leave();
        self.check_limits(origin, e.span);
        self.record_type(b.module, e.span, ty);
        ty
    }

    /// Checks `e` against `want`, reporting a mismatch; returns `e`'s own type.
    pub(crate) fn expect(&mut self, b: &mut Body, e: &Expr, want: TypeId) -> TypeId {
        let got = self.expr(b, e, Some(want));
        self.mismatch(b, e.span, got, want);
        got
    }

    /// Reports `got` where `want` was expected, unless `got` is a subtype of it.
    pub(crate) fn mismatch(&mut self, b: &Body, span: Span, got: TypeId, want: TypeId) -> bool {
        self.mismatch_at(b, span, got, want, None)
    }

    /// [`Checker::mismatch`] for a position the message names ("the `system` section").
    /// A value that would fit but for its trust labels is a taint error (`C0035`), and one
    /// that would fit but for a secrecy label a secrecy error (`C0037`): the sinks of chapter
    /// 03 are trusted (or unlabelled) positions, so this is where they are refused.
    pub(crate) fn mismatch_at(
        &mut self,
        b: &Body,
        span: Span,
        got: TypeId,
        want: TypeId,
        position: Option<&str>,
    ) -> bool {
        if self.types.subtype(&mut self.memo, got, want) {
            return true;
        }
        let want = self.types.resolve(want);
        let got = self.types.resolve(got);
        let code = self.classify(got, want);
        let (shown_got, shown_want) = (self.types.display(got), self.types.display(want));
        let at = position.map_or_else(String::new, |p| format!(" in {p}"));
        let message = match code {
            Code::Untrusted => format!(
                "expected `{shown_want}`{at}, found `{shown_got}`: untrusted text cannot reach a \
                 trusted position (convert it with an audited `trust` or `as_task`, or frame it \
                 in a `data` section)"
            ),
            Code::Secret => format!(
                "expected `{shown_want}`{at}, found `{shown_got}`: a secret cannot reach a \
                 position that is not labelled secret (`declassify` asks the user)"
            ),
            _ => format!("expected `{shown_want}`{at}, found `{shown_got}`"),
        };
        self.report(b.origin(), code, span, message);
        false
    }

    /// Why `got` does not fit `want`: only its trust labels (`C0035`), only its secrecy
    /// labels (`C0037`), or its shape (`C0004`).
    fn classify(&mut self, got: TypeId, want: TypeId) -> Code {
        if self.types.has_vars(got) || self.types.has_vars(want) {
            return Code::Mismatch;
        }
        let (g, w) = (
            self.types.erase(got, Erase::Trust),
            self.types.erase(want, Erase::Trust),
        );
        let untrusted = (g != got || w != want) && self.types.subtype_quiet(&mut self.memo, g, w);
        if untrusted {
            return Code::Untrusted;
        }
        if self.types.secret_inside(got) {
            let (g, w) = (
                self.types.erase(g, Erase::Secret),
                self.types.erase(w, Erase::Secret),
            );
            if self.types.subtype_quiet(&mut self.memo, g, w) {
                return Code::Secret;
            }
        }
        Code::Mismatch
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of expression")]
    fn expr_inner(&mut self, b: &mut Body, e: &Expr, expected: Option<TypeId>) -> TypeId {
        match &e.kind {
            ExprKind::Error => TypeId::ERROR,
            ExprKind::Lit(lit) => self.literal(b, lit, expected, e.span),
            ExprKind::Name(id) => self.name_expr(b, id, expected, e.span),
            ExprKind::Paren(inner) => self.expr(b, inner, expected),
            ExprKind::Tuple(items) => self.tuple(b, items, expected, e.span),
            ExprKind::List(items) => self.list(b, items, expected, e.span),
            ExprKind::Record(lit) => self.record_lit(b, lit, expected, Code::RecordFields),
            ExprKind::Variant { path, fields } => {
                self.variant_lit(b, path, fields, expected, e.span)
            }
            ExprKind::Block(block) | ExprKind::Do(block) => self.block(b, block, expected),
            ExprKind::Unary { op, operand } => self.unary(b, *op, operand),
            ExprKind::Binary { op, lhs, rhs } => self.binary(b, e, *op, lhs, rhs, expected),
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => self.call(b, e, callee, type_args, args, expected),
            ExprKind::Member {
                object,
                name,
                optional,
            } => self.member(b, e, object, name, *optional, expected),
            ExprKind::Index { object, index } => self.index(b, object, index, e.span),
            ExprKind::WithOverride { call, config } => {
                self.with_override(b, call, config, expected)
            }
            ExprKind::If { cond, then, else_ } => {
                self.if_expr(b, cond, then, else_.as_deref(), expected, e.span)
            }
            ExprKind::Match { scrutinee, arms } => {
                self.match_expr(b, scrutinee, arms, expected, e.span)
            }
            ExprKind::For {
                pattern,
                iter,
                body,
            } => {
                b.push_scope();
                let elem = self.iterated(b, iter);
                self.irrefutable_binding(b, pattern, elem);
                self.loop_body(b, body, false, None);
                b.pop_scope();
                TypeId::UNIT
            }
            ExprKind::While { cond, body } => {
                b.push_scope();
                self.condition(b, cond);
                self.loop_body(b, body, false, None);
                b.pop_scope();
                TypeId::UNIT
            }
            ExprKind::Loop(body) => self
                .loop_body(b, body, true, expected)
                .unwrap_or(TypeId::NEVER),
            ExprKind::Repeat { count, body } => {
                self.expect(b, count, TypeId::INT);
                self.loop_body(b, body, false, None);
                TypeId::UNIT
            }
            ExprKind::Try { body, catch } => self.try_expr(b, body, catch, expected, e.span),
            ExprKind::Par(items) => {
                if items.len() > 8 {
                    self.report(
                        b.origin(),
                        Code::TupleArity,
                        e.span,
                        format!(
                            "`par` gives a tuple, of at most 8 values, not {}",
                            items.len()
                        ),
                    );
                }
                let wants = self.tuple_expectation(expected, items.len());
                let ts = items
                    .iter()
                    .zip(wants)
                    .map(|(x, want)| self.expr(b, x, want))
                    .collect();
                self.types.tuple(ts)
            }
            ExprKind::Race(items) => {
                let found: Vec<(TypeId, Span)> = items
                    .iter()
                    .map(|x| (self.expr(b, x, expected), x.span))
                    .collect();
                self.join_all(b, &found, expected, e.span)
            }
            ExprKind::WithTimeout {
                duration,
                body,
                else_,
            } => {
                self.expect(b, duration, TypeId::DURATION);
                let first = self.block(b, body, expected);
                match else_ {
                    Some(other) => {
                        let second = self.block(b, other, expected);
                        self.join_all(
                            b,
                            &[(first, body.span), (second, other.span)],
                            expected,
                            e.span,
                        )
                    }
                    None => first,
                }
            }
            ExprKind::With { config, body } => {
                let want = self.known_type("HarnessConfig");
                let got = self.record_lit(b, config, Some(want), Code::RecordFields);
                self.mismatch(b, config.span, got, want);
                self.block(b, body, expected)
            }
            ExprKind::Closure { params, body } => self.closure(b, params, body, expected),
            ExprKind::FnExpr { params, ret, body } => self.fn_expr(b, params, ret.as_ref(), body),
            ExprKind::Prompt(body) => self.prompt_body(b, body),
            ExprKind::Return(value) => {
                self.return_expr(b, value.as_deref(), e.span);
                TypeId::NEVER
            }
            ExprKind::Break(value) => {
                self.break_expr(b, value.as_deref(), e.span);
                TypeId::NEVER
            }
            ExprKind::Continue => {
                if b.fns.last().is_none_or(|f| f.loops.is_empty()) {
                    self.report(
                        b.origin(),
                        Code::MisplacedJump,
                        e.span,
                        "`continue` outside a loop".to_owned(),
                    );
                }
                TypeId::NEVER
            }
            ExprKind::Raise(value) => {
                let error = self.known_type("Error");
                self.expect(b, value, error);
                TypeId::NEVER
            }
        }
    }

    // ---- literals ----

    fn literal(&mut self, b: &mut Body, lit: &Lit, expected: Option<TypeId>, span: Span) -> TypeId {
        match lit {
            Lit::Int(_) | Lit::Quantity(_) => TypeId::INT,
            Lit::Float(_) => TypeId::FLOAT,
            Lit::Duration(_) => TypeId::DURATION,
            Lit::Money(_) => TypeId::MONEY,
            Lit::Bool(_) => TypeId::BOOL,
            Lit::None => {
                // Where an enum with a `none` variant (`ContextMode`) is expected, `none` is
                // that variant, as a contextual variant would be.
                if let Some(target) = expected
                    && let Some(id) = self.types.as_enum(target)
                    && let Some(index) = self.types.enum_def(id).variant("none")
                {
                    self.res(b, span, crate::tables::Res::Variant(id, index));
                    return target;
                }
                self.types.option(TypeId::NEVER)
            }
            Lit::Str(s) => {
                // The literal is as trusted as the least trusted value it interpolates: any
                // value that may hold untrusted text (a type parameter included) makes it
                // `Untrusted<Str>`; a `Brief`, with only trusted text otherwise, a `Brief`
                // (a task the harness words around a task it was given); a statically secret
                // value makes it secret.
                let mut tainted = false;
                let mut brief = false;
                let mut secret = false;
                let mut plain = true;
                for part in &s.parts {
                    if let StrPart::Interp { expr, .. } = part {
                        plain = false;
                        let t = self.expr(b, expr, None);
                        let t = self.types.resolve(t);
                        secret |= self.types.secret_inside(t);
                        let t = self.types.erase(t, Erase::Secret);
                        if t == TypeId::BRIEF {
                            brief = true;
                        } else {
                            tainted |= self.types.tainted(t);
                        }
                    }
                }
                let text = if tainted {
                    TypeId::UNTRUSTED_STR
                } else if brief {
                    TypeId::BRIEF
                } else {
                    TypeId::STR
                };
                if secret {
                    return self.types.secret(text);
                }
                let target = expected.map(|t| self.strip_option(t));
                if plain && let Some(target) = target {
                    if self.is_named(target, "ModelRef") {
                        self.table(b.module)
                            .coercions
                            .insert(Key::from(span), Coercion::ModelName);
                        return target;
                    }
                    if target == TypeId::TOOL_NAME {
                        self.table(b.module)
                            .coercions
                            .insert(Key::from(span), Coercion::ToolPattern);
                        return TypeId::TOOL_NAME;
                    }
                }
                text
            }
        }
    }

    /// `t` without one option layer.
    pub(crate) fn strip_option(&self, t: TypeId) -> TypeId {
        match self.types.get(t) {
            Ty::Option(inner) => *inner,
            _ => t,
        }
    }

    /// Whether `t` is the prelude's enum `name`.
    pub(crate) fn is_named(&self, t: TypeId, name: &str) -> bool {
        self.types.as_enum(t).is_some_and(|id| {
            self.types.enum_def(id).name == name && self.prelude_enum(name) == Some(id)
        })
    }

    fn prelude_enum(&self, name: &str) -> Option<crate::types::EnumId> {
        let index = *self.std[0].scope.get(name)?;
        match self.std_sigs[0][index] {
            crate::check::ItemSig::Enum(id) => Some(id),
            _ => None,
        }
    }

    // ---- collections ----

    fn tuple_expectation(&self, expected: Option<TypeId>, n: usize) -> Vec<Option<TypeId>> {
        match expected.map(|t| self.types.get(t)) {
            Some(Ty::Tuple(ts)) if ts.len() == n => ts.iter().map(|t| Some(*t)).collect(),
            _ => vec![None; n],
        }
    }

    fn tuple(
        &mut self,
        b: &mut Body,
        items: &[Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        if items.len() > 8 {
            self.report(
                b.origin(),
                Code::TupleArity,
                span,
                format!("a tuple has at most 8 elements, this one {}", items.len()),
            );
        }
        let wants = self.tuple_expectation(expected, items.len());
        let ts = items
            .iter()
            .zip(wants)
            .map(|(x, want)| self.expr(b, x, want))
            .collect();
        if items.len() > 8 {
            return TypeId::ERROR;
        }
        self.types.tuple(ts)
    }

    fn list(
        &mut self,
        b: &mut Body,
        items: &[ast::ListItem],
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let target = expected.map(|t| self.strip_option(t));
        if target == Some(TypeId::TOOL_SET) {
            for item in items {
                if item.spread {
                    let list = self.types.list(TypeId::TOOL_NAME);
                    self.expect(b, &item.expr, list);
                } else {
                    self.expect(b, &item.expr, TypeId::TOOL_NAME);
                }
            }
            self.table(b.module)
                .coercions
                .insert(Key::from(span), Coercion::ToolSet);
            return TypeId::TOOL_SET;
        }
        let want = match target.map(|t| self.types.get(t)) {
            Some(Ty::List(elem)) => Some(*elem),
            _ => None,
        };
        let mut found: Vec<(TypeId, Span)> = Vec::with_capacity(items.len());
        for item in items {
            if item.spread {
                let want_list = want.map(|w| self.types.list(w));
                let t = self.expr(b, &item.expr, want_list);
                let resolved = self.types.resolve(t);
                match self.types.get(resolved) {
                    Ty::List(elem) => found.push((*elem, item.expr.span)),
                    Ty::Error | Ty::Never => found.push((TypeId::NEVER, item.expr.span)),
                    _ => {
                        let shown = self.types.display(t);
                        self.report(
                            b.origin(),
                            Code::Mismatch,
                            item.expr.span,
                            format!("`..` splices a list, not `{shown}`"),
                        );
                    }
                }
            } else {
                found.push((self.expr(b, &item.expr, want), item.expr.span));
            }
        }
        let elem = if found.is_empty() {
            want.unwrap_or(TypeId::NEVER)
        } else {
            self.join_all(b, &found, want, span)
        };
        self.types.list(elem)
    }

    /// The common supertype of `found`; without one, each is checked against `expected` (when
    /// there is one, which is then the result), or `C0020` is reported.
    pub(crate) fn join_all(
        &mut self,
        b: &Body,
        found: &[(TypeId, Span)],
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let mut acc = Some(TypeId::NEVER);
        for (t, _) in found {
            acc = acc.and_then(|a| self.types.join(&mut self.memo, a, *t));
        }
        if let Some(acc) = acc {
            return acc;
        }
        if let Some(want) = expected {
            for (t, at) in found {
                self.mismatch(b, *at, *t, want);
            }
            return want;
        }
        let shown: Vec<String> = found.iter().map(|(t, _)| self.types.display(*t)).collect();
        let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
        self.report(
            b.origin(),
            Code::NoCommonType,
            span,
            format!("these have no common type: {}", quote(&shown)),
        );
        TypeId::ERROR
    }

    /// A record literal: against an expected record type (exactly its fields), a
    /// `Map<Str, V>` (a map), a `ToolSet` (`{ need, want }`), or on its own. Field mistakes
    /// are reported with `code` (`C0006`, or `C0007` for a variant's payload).
    #[allow(
        clippy::too_many_lines,
        reason = "each form of field, checked in written order"
    )]
    pub(crate) fn record_lit(
        &mut self,
        b: &mut Body,
        lit: &RecordLit,
        expected: Option<TypeId>,
        code: Code,
    ) -> TypeId {
        let target = expected.map(|t| self.strip_option(t));
        if target == Some(TypeId::TOOL_SET) {
            let names = self.types.list(TypeId::TOOL_NAME);
            let shape = self.types.record(vec![
                Field {
                    name: "need".into(),
                    ty: names,
                    optional: true,
                },
                Field {
                    name: "want".into(),
                    ty: names,
                    optional: true,
                },
            ]);
            let got = self.record_lit(b, lit, Some(shape), code);
            self.mismatch(b, lit.span, got, shape);
            self.table(b.module)
                .coercions
                .insert(Key::from(lit.span), Coercion::ToolSet);
            return TypeId::TOOL_SET;
        }
        if let Some(target) = target
            && let Ty::Map(key, value) = self.types.get(target).clone()
            && self.types.subtype(&mut self.memo, TypeId::STR, key)
        {
            for init in &lit.fields {
                match init {
                    FieldInit::Field { value: v, .. } => {
                        self.expect(b, v, value);
                    }
                    FieldInit::Shorthand(id) => {
                        let t = self.name_expr(b, id, Some(value), id.span);
                        self.record_type(b.module, id.span, t);
                        self.mismatch(b, id.span, t, value);
                    }
                    FieldInit::Spread(x) => {
                        self.report(
                            b.origin(),
                            code,
                            x.span,
                            "a map built from a record literal cannot spread".to_owned(),
                        );
                    }
                }
            }
            self.table(b.module)
                .coercions
                .insert(Key::from(lit.span), Coercion::RecordToMap);
            return target;
        }
        let declared: Option<Vec<Field>> = target.and_then(|t| match self.types.get(t) {
            Ty::Record(fields) => Some(fields.to_vec()),
            _ => None,
        });
        let mut given: Vec<Field> = Vec::new();
        let mut written: HashSet<&str> = HashSet::new();
        let mut base: Vec<Field> = Vec::new();
        let mut base_span = lit.span;
        for init in &lit.fields {
            let (name, span, value_ty) = match init {
                FieldInit::Spread(x) => {
                    let t = self.expr(b, x, None);
                    let resolved = self.types.resolve(t);
                    base_span = x.span;
                    match self.types.get(resolved) {
                        Ty::Record(fields) => base = fields.to_vec(),
                        Ty::Error => {}
                        _ => {
                            let shown = self.types.display(t);
                            self.report(
                                b.origin(),
                                code,
                                x.span,
                                format!("`..` spreads a record, not `{shown}`"),
                            );
                        }
                    }
                    continue;
                }
                FieldInit::Field { name, value } => {
                    let want = self.field_expectation(declared.as_deref(), &name.name);
                    let t = self.expr(b, value, want);
                    (name, value.span, t)
                }
                FieldInit::Shorthand(id) => {
                    let want = self.field_expectation(declared.as_deref(), &id.name);
                    let t = self.name_expr(b, id, want, id.span);
                    self.record_type(b.module, id.span, t);
                    (id, id.span, t)
                }
            };
            if !written.insert(&name.name) {
                self.report(
                    b.origin(),
                    code,
                    name.span,
                    format!("the field `{}` is given twice", name.name),
                );
                continue;
            }
            if let Some(fields) = &declared {
                if let Some(f) = crate::types::field_index(fields, &name.name).map(|i| &fields[i]) {
                    let want = if f.optional {
                        self.types.option(f.ty)
                    } else {
                        f.ty
                    };
                    self.mismatch(b, span, value_ty, want);
                } else {
                    let shown = self.types.display(target.unwrap_or(TypeId::ERROR));
                    self.report(
                        b.origin(),
                        code,
                        name.span,
                        format!("`{shown}` has no field `{}`", name.name),
                    );
                }
            }
            given.push(Field {
                name: name.name.as_str().into(),
                ty: value_ty,
                optional: false,
            });
        }
        // Spread fields fill what was not written; against a declared type, those it does not
        // declare are left out (spread builds narrower records), the others are checked.
        for f in base {
            if written.contains(&*f.name) {
                continue;
            }
            if let Some(fields) = &declared {
                let Some(want) = crate::types::field_index(fields, &f.name).map(|i| &fields[i])
                else {
                    continue;
                };
                let want = if want.optional {
                    self.types.option(want.ty)
                } else {
                    want.ty
                };
                let got = if f.optional {
                    self.types.option(f.ty)
                } else {
                    f.ty
                };
                self.mismatch(b, base_span, got, want);
            }
            given.push(f);
        }
        let Some(fields) = declared else {
            return self.types.record(given);
        };
        let mut given_sorted = given;
        given_sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let missing: Vec<&str> = fields
            .iter()
            .filter(|f| !f.optional && crate::types::field_index(&given_sorted, &f.name).is_none())
            .map(|f| &*f.name)
            .collect();
        if !missing.is_empty() {
            let shown = self.types.display(target.unwrap_or(TypeId::ERROR));
            self.report(
                b.origin(),
                code,
                lit.span,
                format!("missing field(s) {} of `{shown}`", quote(&missing)),
            );
        }
        // The literal has exactly the declared fields, each checked against its type.
        target.unwrap_or(TypeId::ERROR)
    }

    fn field_expectation(&mut self, declared: Option<&[Field]>, name: &str) -> Option<TypeId> {
        let fields = declared?;
        let f = &fields[crate::types::field_index(fields, name)?];
        // An optional field of an enum with a `none` variant (`context?: ContextMode`) expects
        // the enum, so `context: none` is the variant; omitting the field leaves it absent.
        let none_variant = self
            .types
            .as_enum(f.ty)
            .is_some_and(|id| self.types.enum_def(id).variant("none").is_some());
        Some(if f.optional && !none_variant {
            self.types.option(f.ty)
        } else {
            f.ty
        })
    }

    // ---- operators ----

    fn unary(&mut self, b: &mut Body, op: UnaryOp, operand: &Expr) -> TypeId {
        match op {
            UnaryOp::Not => {
                self.expect(b, operand, TypeId::BOOL);
                TypeId::BOOL
            }
            UnaryOp::Neg => {
                let t = self.expr(b, operand, None);
                let t = self.types.resolve(t);
                if matches!(
                    self.types.get(t),
                    Ty::Int | Ty::Float | Ty::Duration | Ty::Money | Ty::Error
                ) {
                    t
                } else {
                    let shown = self.types.display(t);
                    self.report(
                        b.origin(),
                        Code::Operator,
                        operand.span,
                        format!("`-` negates a number, not `{shown}`"),
                    );
                    TypeId::ERROR
                }
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per group of operators")]
    fn binary(
        &mut self,
        b: &mut Body,
        e: &Expr,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<TypeId>,
    ) -> TypeId {
        match op {
            BinaryOp::Pipe => self.pipe(b, e, lhs, rhs, expected),
            BinaryOp::And | BinaryOp::Or => {
                self.expect(b, lhs, TypeId::BOOL);
                self.expect(b, rhs, TypeId::BOOL);
                TypeId::BOOL
            }
            BinaryOp::Coalesce => {
                let want = expected.map(|t| self.types.option(t));
                let l = self.expr(b, lhs, want);
                let resolved = self.types.resolve(l);
                let inner = match self.types.get(resolved) {
                    Ty::Option(inner) => *inner,
                    Ty::Error => TypeId::ERROR,
                    _ => {
                        let shown = self.types.display(l);
                        self.report(
                            b.origin(),
                            Code::Operator,
                            lhs.span,
                            format!("`??` needs an option on its left, not `{shown}`"),
                        );
                        TypeId::ERROR
                    }
                };
                let r = self.expr(b, rhs, expected.or(Some(inner)));
                self.join_all(b, &[(inner, lhs.span), (r, rhs.span)], expected, e.span)
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let l = self.expr(b, lhs, None);
                let r = self.expr(b, rhs, Some(l));
                if self.types.join(&mut self.memo, l, r).is_none() {
                    let (l, r) = (self.types.display(l), self.types.display(r));
                    self.report(
                        b.origin(),
                        Code::Operator,
                        e.span,
                        format!("`{l}` and `{r}` cannot be compared"),
                    );
                }
                TypeId::BOOL
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let l = self.expr(b, lhs, None);
                let r = self.expr(b, rhs, Some(l));
                let (l, r) = (self.types.resolve(l), self.types.resolve(r));
                let ordered = l == TypeId::ERROR
                    || r == TypeId::ERROR
                    || (l == r
                        && matches!(
                            self.types.get(l),
                            Ty::Int | Ty::Float | Ty::Duration | Ty::Money
                        ))
                    || (self.types.is_text(l) && self.types.is_text(r));
                if !ordered {
                    let (l, r) = (self.types.display(l), self.types.display(r));
                    self.report(
                        b.origin(),
                        Code::Operator,
                        e.span,
                        format!(
                            "`{}` orders two numbers of one type or two texts, not `{l}` and `{r}`",
                            op.as_str()
                        ),
                    );
                }
                TypeId::BOOL
            }
            BinaryOp::In => {
                let r = self.expr(b, rhs, None);
                let r = self.types.resolve(r);
                let want = match self.types.get(r).clone() {
                    Ty::List(elem) => Some(elem),
                    Ty::Map(key, _) => Some(key),
                    Ty::Range => Some(TypeId::INT),
                    Ty::Str | Ty::UserText | Ty::Brief | Ty::UntrustedStr => {
                        Some(TypeId::UNTRUSTED_STR)
                    }
                    Ty::Error => Some(TypeId::ERROR),
                    _ => None,
                };
                if let Some(want) = want {
                    let l = self.expr(b, lhs, Some(want));
                    if self.types.join(&mut self.memo, l, want).is_none() {
                        self.mismatch(b, lhs.span, l, want);
                    }
                } else {
                    self.expr(b, lhs, None);
                    let shown = self.types.display(r);
                    self.report(
                        b.origin(),
                        Code::Operator,
                        rhs.span,
                        format!(
                            "`in` tests membership of a list, map, range or text, not `{shown}`"
                        ),
                    );
                }
                TypeId::BOOL
            }
            BinaryOp::Is => {
                let l = self.expr(b, lhs, None);
                self.is_test(b, l, rhs);
                TypeId::BOOL
            }
            BinaryOp::Range | BinaryOp::RangeInclusive => {
                self.expect(b, lhs, TypeId::INT);
                self.expect(b, rhs, TypeId::INT);
                TypeId::RANGE
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                let l = self.expr(b, lhs, expected);
                let r = self.expr(b, rhs, Some(l));
                self.arith(b, op, l, r, e.span)
            }
        }
    }

    /// The type of an arithmetic operation, or `C0026`.
    pub(crate) fn arith(
        &mut self,
        b: &Body,
        op: BinaryOp,
        l: TypeId,
        r: TypeId,
        span: Span,
    ) -> TypeId {
        let (l, r) = (self.types.resolve(l), self.types.resolve(r));
        if l == TypeId::ERROR || r == TypeId::ERROR {
            return TypeId::ERROR;
        }
        let (tl, tr) = (self.types.get(l).clone(), self.types.get(r).clone());
        let number = |t: &Ty| matches!(t, Ty::Int | Ty::Float);
        let amount = |t: &Ty| matches!(t, Ty::Duration | Ty::Money);
        let result = match op {
            BinaryOp::Add | BinaryOp::Sub if l == r && (number(&tl) || amount(&tl)) => Some(l),
            BinaryOp::Add if self.types.is_text(l) && self.types.is_text(r) => {
                self.types.join(&mut self.memo, l, r)
            }
            BinaryOp::Add if matches!((&tl, &tr), (Ty::List(_), Ty::List(_))) => {
                self.types.join(&mut self.memo, l, r)
            }
            BinaryOp::Mul | BinaryOp::Div if l == r && number(&tl) => Some(l),
            BinaryOp::Mul | BinaryOp::Div if amount(&tl) && tr == Ty::Int => Some(l),
            BinaryOp::Mul if tl == Ty::Int && amount(&tr) => Some(r),
            BinaryOp::Rem if tl == Ty::Int && tr == Ty::Int => Some(l),
            _ => None,
        };
        result.unwrap_or_else(|| {
            let (l, r) = (self.types.display(l), self.types.display(r));
            self.report(
                b.origin(),
                Code::Operator,
                span,
                format!("`{}` does not apply to `{l}` and `{r}`", op.as_str()),
            );
            TypeId::ERROR
        })
    }

    fn index(&mut self, b: &mut Body, object: &Expr, index: &Expr, span: Span) -> TypeId {
        let t = self.expr(b, object, None);
        let t = self.types.resolve(t);
        match self.types.get(t).clone() {
            Ty::List(elem) => {
                self.expect(b, index, TypeId::INT);
                elem
            }
            Ty::Map(key, value) => {
                self.expect(b, index, key);
                value
            }
            Ty::Json => {
                let i = self.expr(b, index, None);
                let resolved = self.types.resolve(i);
                if !matches!(self.types.get(resolved), Ty::Int | Ty::Error)
                    && !self.types.is_text(i)
                {
                    self.mismatch(b, index.span, i, TypeId::STR);
                }
                self.types.option(TypeId::JSON)
            }
            Ty::Error => {
                self.expr(b, index, None);
                TypeId::ERROR
            }
            _ => {
                self.expr(b, index, None);
                let shown = self.types.display(t);
                self.report(
                    b.origin(),
                    Code::Operator,
                    span,
                    format!("`[…]` indexes a list, map or `Json`, not `{shown}`"),
                );
                TypeId::ERROR
            }
        }
    }

    // ---- control flow ----

    /// The head of an `if` or `while`; `let` bindings go into the current scope.
    fn condition(&mut self, b: &mut Body, cond: &Condition) {
        match cond {
            Condition::Expr(e) => {
                let t = self.expr(b, e, Some(TypeId::BOOL));
                let t = self.types.resolve(t);
                // Branching on a secret `Bool` is allowed (chapter 03): that implicit flow is
                // outside the guarantee, as for taint.
                let t = match self.types.get(t) {
                    Ty::Secret(inner) if *inner == TypeId::BOOL => TypeId::BOOL,
                    _ => t,
                };
                self.mismatch(b, e.span, t, TypeId::BOOL);
            }
            Condition::Let { pattern, value } => {
                let t = self.expr(b, value, None);
                let t = self.types.resolve(t);
                let unwrap = !matches!(pattern.kind, ast::PatternKind::Lit(Lit::None));
                let against = if unwrap { self.strip_option(t) } else { t };
                let mut binders = Vec::new();
                self.pattern(b, pattern, against, PatCx::Refutable, &mut binders);
                self.declare_all(b, &binders, Warn::Plain);
            }
        }
    }

    fn if_expr(
        &mut self,
        b: &mut Body,
        cond: &Condition,
        then: &ast::Block,
        else_: Option<&Else>,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        b.push_scope();
        self.condition(b, cond);
        let Some(else_) = else_ else {
            self.block(b, then, None);
            b.pop_scope();
            return TypeId::UNIT;
        };
        let first = self.block(b, then, expected);
        b.pop_scope();
        let (second, at) = match else_ {
            Else::If(e) => (self.expr(b, e, expected), e.span),
            Else::Block(block) => (self.block(b, block, expected), block.span),
        };
        self.join_all(b, &[(first, then.span), (second, at)], expected, span)
    }

    fn match_expr(
        &mut self,
        b: &mut Body,
        scrutinee: &Expr,
        arms: &[ast::Arm],
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let st = self.expr(b, scrutinee, None);
        let st = self.types.resolve(st);
        let mut found = Vec::with_capacity(arms.len());
        let mut shapes = Vec::new();
        for arm in arms {
            b.push_scope();
            let mut binders = Vec::new();
            let shape = self.pattern(b, &arm.pattern, st, PatCx::Refutable, &mut binders);
            self.declare_all(b, &binders, Warn::Plain);
            if let Some(guard) = &arm.guard {
                self.expect(b, guard, TypeId::BOOL);
            } else {
                shapes.push(shape);
            }
            found.push((self.expr(b, &arm.body, expected), arm.body.span));
            b.pop_scope();
        }
        match self.exhaustive(shapes, st) {
            Outcome::Exhaustive => {}
            Outcome::Missing(case) => self.report(
                b.origin(),
                Code::NonExhaustive,
                span,
                format!("this `match` does not cover `{case}`"),
            ),
            Outcome::TooComplex => self.report(
                b.origin(),
                Code::MatchTooComplex,
                span,
                "this `match` is too large to check for exhaustiveness".to_owned(),
            ),
        }
        if found.is_empty() {
            return TypeId::NEVER;
        }
        self.join_all(b, &found, expected, span)
    }

    /// The element type `for` binds when iterating `iter`.
    fn iterated(&mut self, b: &mut Body, iter: &Expr) -> TypeId {
        let t = self.expr(b, iter, None);
        let t = self.types.resolve(t);
        match self.types.get(t).clone() {
            Ty::List(elem) => elem,
            Ty::Range => TypeId::INT,
            Ty::Map(k, v) => self.types.tuple(vec![k, v]),
            Ty::Error => TypeId::ERROR,
            _ => {
                let shown = self.types.display(t);
                self.report(
                    b.origin(),
                    Code::Mismatch,
                    iter.span,
                    format!("`for` iterates a list, range or map, not `{shown}`"),
                );
                TypeId::ERROR
            }
        }
    }

    /// Binds an irrefutable pattern (`for`, closure parameters) in the current scope.
    pub(crate) fn irrefutable_binding(&mut self, b: &mut Body, pattern: &ast::Pattern, ty: TypeId) {
        let mut binders: Vec<Binder> = Vec::new();
        let shape = self.pattern(b, pattern, ty, PatCx::Irrefutable, &mut binders);
        self.irrefutable(b, shape, ty, pattern.span);
        self.declare_all(b, &binders, Warn::Plain);
    }

    /// A loop body; returns the common type of `break` values for `loop`.
    fn loop_body(
        &mut self,
        b: &mut Body,
        body: &ast::Block,
        value: bool,
        expected: Option<TypeId>,
    ) -> Option<TypeId> {
        b.fn_cx().loops.push(LoopCx {
            value,
            brk: None,
            expected,
        });
        self.block(b, body, None);
        b.fn_cx().loops.pop().and_then(|l| l.brk)
    }

    fn try_expr(
        &mut self,
        b: &mut Body,
        body: &ast::Block,
        catch: &Catch,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let mut found = vec![(self.block(b, body, expected), body.span)];
        let error = self.known_type("Error");
        match catch {
            Catch::Arms(arms) => {
                for arm in arms {
                    b.push_scope();
                    let mut binders = Vec::new();
                    self.pattern(b, &arm.pattern, error, PatCx::Refutable, &mut binders);
                    self.declare_all(b, &binders, Warn::Plain);
                    if let Some(guard) = &arm.guard {
                        self.expect(b, guard, TypeId::BOOL);
                    }
                    found.push((self.expr(b, &arm.body, expected), arm.body.span));
                    b.pop_scope();
                }
            }
            Catch::Bind { name, body } => {
                b.push_scope();
                let binder = Binder {
                    name: name.name.clone(),
                    span: name.span,
                    key: name.span,
                    ty: error,
                };
                self.declare(b, &binder, false, Warn::Plain);
                found.push((self.block(b, body, expected), body.span));
                b.pop_scope();
            }
        }
        self.join_all(b, &found, expected, span)
    }

    fn return_expr(&mut self, b: &mut Body, value: Option<&Expr>, span: Span) {
        let ret = b.fns.last().map_or(Ret::Forbidden, |f| f.ret);
        match ret {
            Ret::Forbidden => {
                if let Some(v) = value {
                    self.expr(b, v, None);
                }
                self.report(
                    b.origin(),
                    Code::MisplacedJump,
                    span,
                    "`return` outside a function".to_owned(),
                );
            }
            Ret::Known(want) => match value {
                Some(v) => {
                    self.expect(b, v, want);
                }
                None => {
                    self.mismatch(b, span, TypeId::UNIT, want);
                }
            },
            Ret::Infer(acc) => {
                let t = match value {
                    Some(v) => self.expr(b, v, acc),
                    None => TypeId::UNIT,
                };
                let joined = match acc {
                    None => t,
                    Some(prev) => self.join_all(b, &[(prev, span), (t, span)], None, span),
                };
                b.fn_cx().ret = Ret::Infer(Some(joined));
            }
        }
    }

    fn break_expr(&mut self, b: &mut Body, value: Option<&Expr>, span: Span) {
        let Some(&LoopCx {
            value: takes,
            brk,
            expected,
        }) = b.fns.last().and_then(|f| f.loops.last())
        else {
            if let Some(v) = value {
                self.expr(b, v, None);
            }
            self.report(
                b.origin(),
                Code::MisplacedJump,
                span,
                "`break` outside a loop".to_owned(),
            );
            return;
        };
        let t = match value {
            Some(v) => {
                let t = self.expr(b, v, brk.or(expected));
                if !takes {
                    self.report(
                        b.origin(),
                        Code::MisplacedJump,
                        span,
                        "only `loop` takes a value with `break`".to_owned(),
                    );
                    return;
                }
                t
            }
            None => TypeId::UNIT,
        };
        let joined = match brk {
            None => t,
            Some(prev) => self.join_all(b, &[(prev, span), (t, span)], None, span),
        };
        if let Some(l) = b.fn_cx().loops.last_mut() {
            l.brk = Some(joined);
        }
    }

    // ---- closures ----

    fn closure(
        &mut self,
        b: &mut Body,
        params: &[ast::ClosureParam],
        body: &Expr,
        expected: Option<TypeId>,
    ) -> TypeId {
        // An optional function parameter (`until: (fn(ModelReply) -> Bool)? = none`) shapes a
        // closure as the function it holds.
        let expected = expected.map(|t| self.strip_option(t));
        let shape = expected.and_then(|t| match self.types.get(t) {
            Ty::Fn(ps, r) | Ty::Harness(ps, r) if ps.len() == params.len() => {
                Some((ps.to_vec(), *r))
            }
            _ => None,
        });
        b.push_scope();
        b.fns.push(FnCx {
            ret: Ret::Infer(None),
            loops: Vec::new(),
        });
        let mut param_types = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let hinted = shape.as_ref().map(|(ps, _)| self.types.resolve(ps[i]));
            let ty = match (&p.ty, hinted) {
                (Some(t), _) => self.lower_type(b.origin(), &b.tparams.clone(), t),
                (None, Some(h)) if !matches!(self.types.get(h), Ty::Var(_)) => h,
                _ => {
                    self.report(
                        b.origin(),
                        Code::CannotInfer,
                        p.pattern.span,
                        "the type of this closure parameter cannot be inferred; annotate it"
                            .to_owned(),
                    );
                    TypeId::ERROR
                }
            };
            self.irrefutable_binding(b, &p.pattern, ty);
            param_types.push(ty);
        }
        let want = shape.map(|(_, r)| self.types.resolve(r));
        let got = self.expr(b, body, want);
        let ret = match b.fns.pop().map(|f| f.ret) {
            Some(Ret::Infer(Some(returned))) => self.join_all(
                b,
                &[(returned, body.span), (got, body.span)],
                None,
                body.span,
            ),
            _ => got,
        };
        b.pop_scope();
        self.types.function(param_types, ret)
    }

    fn fn_expr(
        &mut self,
        b: &mut Body,
        params: &[ast::Param],
        ret: Option<&ast::Type>,
        body: &ast::Block,
    ) -> TypeId {
        let tparams = b.tparams.clone();
        let declared = ret.map(|t| self.lower_type(b.origin(), &tparams, t));
        b.push_scope();
        b.fns.push(FnCx {
            ret: declared.map_or(Ret::Infer(None), Ret::Known),
            loops: Vec::new(),
        });
        let mut param_types = Vec::with_capacity(params.len());
        for p in params {
            let ty = self.lower_type(b.origin(), &tparams, &p.ty);
            if let Some(default) = &p.default {
                self.expect(b, default, ty);
            }
            let binder = Binder {
                name: p.name.name.clone(),
                span: p.name.span,
                key: p.name.span,
                ty,
            };
            self.declare(b, &binder, false, Warn::Plain);
            param_types.push(ty);
        }
        let got = self.block(b, body, declared);
        let ret = match (declared, b.fns.pop().map(|f| f.ret)) {
            (Some(want), _) => {
                self.mismatch(b, body.span, got, want);
                want
            }
            (None, Some(Ret::Infer(Some(returned)))) => self.join_all(
                b,
                &[(returned, body.span), (got, body.span)],
                None,
                body.span,
            ),
            _ => got,
        };
        b.pop_scope();
        self.types.function(param_types, ret)
    }

    // ---- prompts ----

    /// A prompt body: `TrustedPrompt` when every section holds trusted content only (no
    /// `data`, `image` or `context` section, no `Brief` task, no included untrusted prompt),
    /// else `Prompt` (chapter 05). The instruction positions (`system`, `instruct`, `note`,
    /// `example`, `steps`) take trusted text only and `task` takes a `Brief`, so untrusted
    /// text there is refused (`C0035`); the sections that frame it (`data`) take anything.
    pub(crate) fn prompt_body(&mut self, b: &mut Body, body: &ast::PromptBody) -> TypeId {
        let mut trusted = true;
        for section in &body.sections {
            trusted &= self.section(b, section);
        }
        if trusted {
            TypeId::TRUSTED_PROMPT
        } else {
            TypeId::PROMPT
        }
    }

    /// Checks a section; whether its content is trusted.
    fn section(&mut self, b: &mut Body, section: &ast::Section) -> bool {
        use ast::SectionKind as S;
        match &section.kind {
            S::System(e) => {
                let t = self.expr(b, e, None);
                let t = self.types.resolve(t);
                let prompt = self.types.subtype(&mut self.memo, t, TypeId::PROMPT)
                    && !self.types.subtype(&mut self.memo, t, TypeId::UNTRUSTED_STR);
                let want = if prompt {
                    TypeId::TRUSTED_PROMPT
                } else {
                    TypeId::STR
                };
                self.mismatch_at(b, e.span, t, want, Some("the `system` section"))
            }
            S::Instruct(e) => self.text_section(b, e, TypeId::STR, "the `instruct` section"),
            S::Note(e) => self.text_section(b, e, TypeId::STR, "the `note` section"),
            S::Task(e) => self.text_section(b, e, TypeId::BRIEF, "the `task` section"),
            S::Example { input, output } => {
                let a = self.text_section(b, input, TypeId::STR, "an `example` section");
                self.text_section(b, output, TypeId::STR, "an `example` section") && a
            }
            S::Steps(e) => {
                let list = self.types.list(TypeId::STR);
                let t = self.expr(b, e, Some(list));
                self.mismatch_at(b, e.span, t, list, Some("the `steps` section"))
            }
            S::Data { value, label } => {
                self.expr(b, value, None);
                if let Some(label) = label {
                    self.expect(b, label, TypeId::UNTRUSTED_STR);
                }
                false
            }
            S::Image { value, label } => {
                self.expect(b, value, TypeId::IMAGE);
                if let Some(label) = label {
                    self.expect(b, label, TypeId::UNTRUSTED_STR);
                }
                false
            }
            S::Format(t) => {
                let tparams = b.tparams.clone();
                let ty = self.lower_type(b.origin(), &tparams, t);
                self.require_schema(b, ty, t.span, "a `format` section's type");
                true
            }
            S::Context(e) => {
                let t = self.expr(b, e, None);
                let conv = self.types.handle(crate::types::Handle::Conversation);
                if !self.types.subtype(&mut self.memo, t, TypeId::TRANSCRIPT)
                    && !self.types.subtype(&mut self.memo, t, conv)
                {
                    self.mismatch(b, e.span, t, TypeId::TRANSCRIPT);
                }
                false
            }
            S::Include(e) => {
                let t = self.expect(b, e, TypeId::PROMPT);
                self.types.resolve(t) == TypeId::TRUSTED_PROMPT
            }
            S::If { cond, then, else_ } => {
                self.expect(b, cond, TypeId::BOOL);
                let mut trusted = self.prompt_body(b, then) == TypeId::TRUSTED_PROMPT;
                if let Some(other) = else_ {
                    trusted &= self.prompt_body(b, other) == TypeId::TRUSTED_PROMPT;
                }
                trusted
            }
            S::For {
                pattern,
                iter,
                body,
            } => {
                b.push_scope();
                let elem = self.iterated(b, iter);
                self.irrefutable_binding(b, pattern, elem);
                let trusted = self.prompt_body(b, body) == TypeId::TRUSTED_PROMPT;
                b.pop_scope();
                trusted
            }
        }
    }

    /// A section of text that must fit `want` (`Str`, or `Brief` for `task`); whether the
    /// prompt stays trusted (a `Brief` task makes it a `Prompt`).
    ///
    /// The expression is checked expecting `Str` even for `task`: an expected `Brief` would
    /// instantiate a generic call at `Brief` (`apply(|s| s, trusted)`) and make the prompt
    /// untrusted needlessly; whether it fits is then decided against `want`.
    fn text_section(&mut self, b: &mut Body, e: &Expr, want: TypeId, position: &str) -> bool {
        let t = self.expr(b, e, Some(TypeId::STR));
        let fits = self.mismatch_at(b, e.span, t, want, Some(position));
        let t = self.types.resolve(t);
        fits && !self.types.tainted(t)
    }

    /// Reports `ty` unless it is schema-able (`C0038`); `what` says what must be.
    pub(crate) fn require_schema(&mut self, b: &Body, ty: TypeId, span: Span, what: &str) {
        let ty = self.types.resolve(ty);
        if ty == TypeId::ERROR || self.types.has_vars(ty) {
            return;
        }
        if let Err(why) = crate::schema::schema_able(&mut self.types, ty) {
            let shown = self.types.display(ty);
            self.report(
                b.origin(),
                Code::NotSchemaable,
                span,
                format!("{what} must be schema-able, and `{shown}` is not: {why}"),
            );
        }
    }

    // ---- variants ----

    fn variant_lit(
        &mut self,
        b: &mut Body,
        path: &[ast::Ident],
        fields: &RecordLit,
        expected: Option<TypeId>,
        span: Span,
    ) -> TypeId {
        let target = expected.map(|t| self.strip_option(t));
        let Some((enum_id, index)) = self.resolve_variant(b, path, target, span) else {
            // The fields are not checked: without the variant, their names mean nothing.
            let _ = fields;
            return TypeId::ERROR;
        };
        self.table(b.module)
            .res
            .insert(Key::from(span), crate::tables::Res::Variant(enum_id, index));
        let (ty, fresh) = self.enum_instance(enum_id, target);
        let Some(payload) = self.types.payload(ty, index) else {
            let name = &self.types.enum_def(enum_id).variants[index].name;
            let message =
                format!("the variant `{name}` has no payload; write it without `{{ … }}`");
            self.report(b.origin(), Code::UnknownVariant, span, message);
            return TypeId::ERROR;
        };
        let got = self.record_lit(b, fields, Some(payload), Code::UnknownVariant);
        self.types.subtype(&mut self.memo, got, payload);
        let ty = self.types.resolve(ty);
        if fresh {
            self.unbound_args(b, ty, span);
        }
        ty
    }

    /// The enum type for a variant of `enum_id`: the expected type's arguments when it is that
    /// enum, else a fresh inference variable per parameter (then `true`).
    pub(crate) fn enum_instance(
        &mut self,
        enum_id: crate::types::EnumId,
        target: Option<TypeId>,
    ) -> (TypeId, bool) {
        if let Some(t) = target
            && let Ty::Enum(id, _) = self.types.get(t)
            && *id == enum_id
        {
            return (t, false);
        }
        if let Some(t) = target
            && let Ty::Untrusted(inner) = self.types.get(t)
            && self.types.as_enum(*inner) == Some(enum_id)
        {
            return (*inner, false);
        }
        let params = self.types.enum_def(enum_id).params.clone();
        let fresh = !params.is_empty();
        let args = params.into_iter().map(|p| self.types.new_var(p)).collect();
        (self.types.enum_type(enum_id, args), fresh)
    }

    /// Reports type arguments of `ty` (an enum instance) that inference left open.
    pub(crate) fn unbound_args(&mut self, b: &Body, ty: TypeId, span: Span) {
        let Ty::Enum(_, args) = self.types.get(ty).clone() else {
            return;
        };
        for arg in &args {
            if let Ty::Var(v) = *self.types.get(*arg) {
                let param = self.types.vars[v.0 as usize].param;
                let name = self.types.param_def(param).name.clone();
                self.report(
                    b.origin(),
                    Code::CannotInfer,
                    span,
                    format!(
                        "the type parameter `{name}` cannot be inferred here; annotate the binding"
                    ),
                );
                self.types.bind_var(v, TypeId::ERROR);
            }
        }
    }
}
