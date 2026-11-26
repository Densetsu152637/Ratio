//! Patterns: checking a pattern against the type of the value it matches, collecting the
//! names it binds, and reducing it for exhaustiveness.
//!
//! - A pattern other than a binding, `_` or `none` matched against an option `T?` matches a
//!   present value: it is checked against `T` (there is no `some(…)` pattern to write).
//! - In `match` and `catch` arms, and in `if let` and `while let`, a bare name that is a
//!   variant of the matched value's enum is a variant pattern (chapter 02, rule 6); any other
//!   bare name binds. In `let`, `for` and parameters a bare name always binds.
//! - A record pattern names every field, or ends with `..`.

use std::collections::HashSet;

use ratio_syntax::Span;
use ratio_syntax::ast::{self, ListPattern, Lit, Pattern, PatternKind, RecordPattern, StrPart};

use crate::body::Body;
use crate::check::Checker;
use crate::diag::Code;
use crate::exhaust::{Ctor, DPat, LitKey, Outcome};
use crate::tables::{Key, PatRes, Res};
use crate::types::{Field, Ty, TypeId};

/// Where a pattern stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatCx {
    /// `let`, `for`, closure parameters: must be irrefutable; bare names bind.
    Irrefutable,
    /// `match`, `catch`, `if let`, `while let`: may be refutable; bare names may be variants.
    Refutable,
}

/// A name a pattern binds.
#[derive(Clone, Debug)]
pub(crate) struct Binder {
    pub(crate) name: String,
    pub(crate) span: Span,
    /// The span of the pattern node the binding is recorded under.
    pub(crate) key: Span,
    pub(crate) ty: TypeId,
}

impl Checker<'_> {
    /// Checks `p` against `ty`, adding the names it binds to `binders`.
    pub(crate) fn pattern(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        ty: TypeId,
        cx: PatCx,
        binders: &mut Vec<Binder>,
    ) -> DPat {
        if !self.enter(b.origin(), p.span) {
            return DPat::Wild;
        }
        let ty = self.types.resolve(ty);
        let shape = self.pattern_inner(b, p, ty, cx, binders);
        self.leave();
        if ty == TypeId::ERROR {
            return DPat::Wild;
        }
        shape
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of pattern")]
    fn pattern_inner(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        ty: TypeId,
        cx: PatCx,
        binders: &mut Vec<Binder>,
    ) -> DPat {
        let present = match self.types.get(ty) {
            Ty::Option(inner) => Some(*inner),
            _ => None,
        };
        let structural = match &p.kind {
            PatternKind::Wildcard
            | PatternKind::Error
            | PatternKind::Bind { .. }
            | PatternKind::Or(_)
            | PatternKind::Lit(Lit::None) => false,
            PatternKind::Name(id) => {
                cx == PatCx::Refutable && self.variant_named(ty, &id.name).is_some()
            }
            _ => true,
        };
        if structural && let Some(inner) = present {
            let shape = self.pattern_inner(b, p, inner, cx, binders);
            return DPat::Ctor(Ctor::Some, vec![shape]);
        }
        match &p.kind {
            PatternKind::Error | PatternKind::Wildcard => DPat::Wild,
            PatternKind::Name(id) => {
                if cx == PatCx::Refutable
                    && let Some((enum_id, index)) = self.variant_named(ty, &id.name)
                {
                    self.table(b.module)
                        .patterns
                        .insert(Key::from(p.span), PatRes::Variant(enum_id, index));
                    return DPat::Ctor(Ctor::Variant(index), Vec::new());
                }
                binders.push(Binder {
                    name: id.name.clone(),
                    span: id.span,
                    key: p.span,
                    ty,
                });
                DPat::Wild
            }
            PatternKind::Bind { name, pattern } => {
                binders.push(Binder {
                    name: name.name.clone(),
                    span: name.span,
                    key: name.span,
                    ty,
                });
                self.pattern(b, pattern, ty, cx, binders)
            }
            PatternKind::Lit(lit) => self.literal_pattern(b, lit, ty, p.span),
            PatternKind::Variant { path, fields } => {
                self.variant_pattern(b, path, fields.as_ref(), ty, cx, binders, p.span)
            }
            PatternKind::Tuple(items) => {
                let elems = match self.types.get(ty) {
                    Ty::Tuple(ts) if ts.len() == items.len() => ts.to_vec(),
                    Ty::Error => vec![TypeId::ERROR; items.len()],
                    _ => {
                        self.no_fit(b, ty, "a tuple pattern", p.span);
                        vec![TypeId::ERROR; items.len()]
                    }
                };
                let subs = items
                    .iter()
                    .zip(elems)
                    .map(|(q, t)| self.pattern(b, q, t, cx, binders))
                    .collect();
                DPat::Ctor(Ctor::Single, subs)
            }
            PatternKind::List(items) => self.list_pattern(b, items, ty, cx, binders, p.span),
            PatternKind::Record(rp) => {
                let fields = match self.types.get(ty) {
                    Ty::Record(fields) => Some(fields.to_vec()),
                    Ty::Error => None,
                    _ => {
                        self.no_fit(b, ty, "a record pattern", p.span);
                        None
                    }
                };
                self.record_pattern(b, rp, fields.as_deref(), cx, binders)
            }
            PatternKind::Or(alts) => {
                let mut first: Option<Vec<Binder>> = None;
                let mut shapes = Vec::with_capacity(alts.len());
                for alt in alts {
                    let mut own = Vec::new();
                    shapes.push(self.pattern(b, alt, ty, cx, &mut own));
                    let mut names: Vec<&str> = own.iter().map(|x| x.name.as_str()).collect();
                    names.sort_unstable();
                    match &first {
                        None => first = Some(own),
                        Some(prev) => {
                            let mut want: Vec<&str> =
                                prev.iter().map(|x| x.name.as_str()).collect();
                            want.sort_unstable();
                            if want != names {
                                self.report(
                                    b.origin(),
                                    Code::PatternMismatch,
                                    alt.span,
                                    "every alternative of an or-pattern must bind the same names"
                                        .to_owned(),
                                );
                            }
                        }
                    }
                }
                binders.extend(first.unwrap_or_default());
                DPat::Or(shapes)
            }
        }
    }

    fn no_fit(&mut self, b: &Body, ty: TypeId, what: &str, span: Span) {
        let shown = self.types.display(ty);
        self.report(
            b.origin(),
            Code::PatternMismatch,
            span,
            format!("{what} cannot match a value of type `{shown}`"),
        );
    }

    /// The variant called `name` of the enum `ty` is (through an option), if any.
    pub(crate) fn variant_named(
        &self,
        ty: TypeId,
        name: &str,
    ) -> Option<(crate::types::EnumId, usize)> {
        let ty = match self.types.get(ty) {
            Ty::Option(inner) => *inner,
            _ => ty,
        };
        let id = self.types.as_enum(ty)?;
        let index = self.types.enum_def(id).variant(name)?;
        Some((id, index))
    }

    fn literal_pattern(&mut self, b: &mut Body, lit: &Lit, ty: TypeId, span: Span) -> DPat {
        let (lit_ty, shape) = match lit {
            Lit::None => {
                if let Some((id, index)) = self.variant_named(ty, "none")
                    && !matches!(self.types.get(ty), Ty::Option(_))
                {
                    self.table(b.module)
                        .patterns
                        .insert(Key::from(span), PatRes::Variant(id, index));
                    return DPat::Ctor(Ctor::Variant(index), Vec::new());
                }
                if !matches!(self.types.get(ty), Ty::Option(_) | Ty::Error) {
                    self.no_fit(b, ty, "`none`", span);
                }
                return DPat::Ctor(Ctor::None, Vec::new());
            }
            Lit::Bool(v) => (TypeId::BOOL, DPat::Ctor(Ctor::Bool(*v), Vec::new())),
            Lit::Int(v) | Lit::Quantity(v) => (TypeId::INT, lit_ctor(LitKey::Int(*v))),
            Lit::Float(v) => (TypeId::FLOAT, lit_ctor(LitKey::Float(v.to_bits()))),
            Lit::Duration(v) => (TypeId::DURATION, lit_ctor(LitKey::Duration(*v))),
            Lit::Money(v) => (TypeId::MONEY, lit_ctor(LitKey::Money(*v))),
            Lit::Str(s) => {
                let mut text = String::new();
                for part in &s.parts {
                    match part {
                        StrPart::Text(t) => text.push_str(t),
                        StrPart::Interp { span, .. } => {
                            self.report(
                                b.origin(),
                                Code::PatternMismatch,
                                *span,
                                "a string pattern cannot interpolate".to_owned(),
                            );
                            return DPat::Wild;
                        }
                    }
                }
                (TypeId::STR, lit_ctor(LitKey::Str(text)))
            }
        };
        let fits = self.types.subtype(&mut self.memo, lit_ty, ty)
            || self.types.subtype(&mut self.memo, ty, lit_ty);
        if !fits {
            let what = format!("a literal of type `{}`", self.types.display(lit_ty));
            self.no_fit(b, ty, &what, span);
        }
        shape
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the pattern's parts and its context"
    )]
    fn variant_pattern(
        &mut self,
        b: &mut Body,
        path: &[ast::Ident],
        fields: Option<&RecordPattern>,
        ty: TypeId,
        cx: PatCx,
        binders: &mut Vec<Binder>,
        span: Span,
    ) -> DPat {
        let Some(last) = path.last() else {
            return DPat::Wild;
        };
        if ty == TypeId::ERROR {
            if let Some(rp) = fields {
                self.record_pattern(b, rp, None, cx, binders);
            }
            return DPat::Wild;
        }
        let Some(enum_id) = self.types.as_enum(ty) else {
            self.no_fit(b, ty, "a variant pattern", span);
            if let Some(rp) = fields {
                self.record_pattern(b, rp, None, cx, binders);
            }
            return DPat::Wild;
        };
        if path.len() > 1 {
            let named = self.type_path_idents(b, &path[..path.len() - 1]);
            if named != Some(enum_id) {
                self.no_fit(b, ty, "a variant of another enum", span);
                return DPat::Wild;
            }
        }
        let Some(index) = self.types.enum_def(enum_id).variant(&last.name) else {
            let name = self.types.enum_def(enum_id).name.clone();
            self.report(
                b.origin(),
                Code::UnknownVariant,
                last.span,
                format!("`{name}` has no variant `{}`", last.name),
            );
            return DPat::Wild;
        };
        self.table(b.module)
            .patterns
            .insert(Key::from(span), PatRes::Variant(enum_id, index));
        self.table(b.module)
            .res
            .insert(Key::from(span), Res::Variant(enum_id, index));
        let Some(rp) = fields else {
            return DPat::Ctor(Ctor::Variant(index), Vec::new());
        };
        let Some(payload) = self.types.payload(ty, index) else {
            self.report(
                b.origin(),
                Code::UnknownVariant,
                last.span,
                format!("the variant `{}` has no payload to match", last.name),
            );
            return DPat::Wild;
        };
        let payload_fields = match self.types.get(payload) {
            Ty::Record(fields) => Some(fields.to_vec()),
            _ => None,
        };
        let shape = self.record_pattern(b, rp, payload_fields.as_deref(), cx, binders);
        DPat::Ctor(Ctor::Variant(index), vec![shape])
    }

    fn list_pattern(
        &mut self,
        b: &mut Body,
        items: &[ListPattern],
        ty: TypeId,
        cx: PatCx,
        binders: &mut Vec<Binder>,
        span: Span,
    ) -> DPat {
        let elem = match self.types.get(ty) {
            Ty::List(elem) => *elem,
            Ty::Error => TypeId::ERROR,
            _ => {
                self.no_fit(b, ty, "a list pattern", span);
                TypeId::ERROR
            }
        };
        let (mut prefix, mut suffix, mut rest) = (Vec::new(), Vec::new(), false);
        for item in items {
            match item {
                ListPattern::Pattern(q) => {
                    let shape = self.pattern(b, q, elem, cx, binders);
                    if rest {
                        suffix.push(shape);
                    } else {
                        prefix.push(shape);
                    }
                }
                ListPattern::Rest(name) => {
                    if rest {
                        self.report(
                            b.origin(),
                            Code::PatternMismatch,
                            span,
                            "a list pattern has at most one `..`".to_owned(),
                        );
                    }
                    rest = true;
                    if let Some(name) = name {
                        let list = if elem == TypeId::ERROR {
                            TypeId::ERROR
                        } else {
                            self.types.list(elem)
                        };
                        binders.push(Binder {
                            name: name.name.clone(),
                            span: name.span,
                            key: name.span,
                            ty: list,
                        });
                    }
                }
            }
        }
        if rest {
            DPat::Slice(prefix, suffix)
        } else {
            DPat::Ctor(Ctor::Exact(prefix.len()), prefix)
        }
    }

    /// A record pattern against `fields` (`None` when the type is unknown or wrong, already
    /// reported): the shape lists the fields in the type's order.
    fn record_pattern(
        &mut self,
        b: &mut Body,
        rp: &RecordPattern,
        fields: Option<&[Field]>,
        cx: PatCx,
        binders: &mut Vec<Binder>,
    ) -> DPat {
        let mut subs = vec![DPat::Wild; fields.map_or(0, <[Field]>::len)];
        let mut seen: HashSet<&str> = HashSet::new();
        for fp in &rp.fields {
            if !seen.insert(&fp.name.name) {
                self.report(
                    b.origin(),
                    Code::RecordFields,
                    fp.name.span,
                    format!("the field `{}` is matched twice", fp.name.name),
                );
                continue;
            }
            let found = fields
                .and_then(|fs| crate::types::field_index(fs, &fp.name.name).map(|i| (i, &fs[i])));
            let fty = match (fields, found) {
                (None, _) => TypeId::ERROR,
                (Some(_), Some((_, f))) => {
                    if f.optional {
                        self.types.option(f.ty)
                    } else {
                        f.ty
                    }
                }
                (Some(_), None) => {
                    self.report(
                        b.origin(),
                        Code::RecordFields,
                        fp.name.span,
                        format!("the record has no field `{}`", fp.name.name),
                    );
                    TypeId::ERROR
                }
            };
            let shape = if let Some(q) = &fp.pattern {
                self.pattern(b, q, fty, cx, binders)
            } else {
                binders.push(Binder {
                    name: fp.name.name.clone(),
                    span: fp.name.span,
                    key: fp.name.span,
                    ty: fty,
                });
                DPat::Wild
            };
            if let Some((i, _)) = found {
                subs[i] = shape;
            }
        }
        if let Some(fs) = fields
            && !rp.rest
        {
            let missing: Vec<&str> = fs
                .iter()
                .filter(|f| !seen.contains(&*f.name))
                .map(|f| &*f.name)
                .collect();
            if !missing.is_empty() {
                self.report(
                    b.origin(),
                    Code::RecordFields,
                    rp.span,
                    format!(
                        "the pattern does not name the field(s) {}; add `..` to ignore them",
                        quote(&missing)
                    ),
                );
            }
        }
        DPat::Ctor(Ctor::Single, subs)
    }

    /// Reports a refutable pattern where an irrefutable one is needed.
    pub(crate) fn irrefutable(&mut self, b: &Body, shape: DPat, ty: TypeId, span: Span) {
        match self.exhaustive(vec![shape], ty) {
            Outcome::Exhaustive => {}
            Outcome::Missing(case) => self.report(
                b.origin(),
                Code::Refutable,
                span,
                format!("this pattern does not match every value (not `{case}`); use `if let` or `match`"),
            ),
            Outcome::TooComplex => self.report(
                b.origin(),
                Code::MatchTooComplex,
                span,
                "this pattern is too large to check".to_owned(),
            ),
        }
    }
}

fn lit_ctor(key: LitKey) -> DPat {
    DPat::Ctor(Ctor::Lit(key), Vec::new())
}

/// `a`, `b` and `c` as "`a`, `b`, `c`".
pub(crate) fn quote(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
