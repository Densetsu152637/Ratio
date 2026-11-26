//! Lowering: the checked syntax tree and the checker's side tables to the Core IR.
//!
//! The checker leaves what each name, call, pattern and literal means in tables keyed by
//! source span ([`ratio_check::ModuleTable`]); lowering walks the same tree, reads those
//! tables, and builds the IR. Only a clean check is lowered: a node the tables do not describe
//! is an internal error ([`LowerError`]), never a guess. The walk is recursive and the parser
//! bounds the tree's depth, so it runs on a large stack (as the checker does) with its own
//! depth bound.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

use ratio_check::{
    ArgSource, CallInfo, Checked, Coercion, EnumId, ItemRef, Key, LocalId, ModuleTable, Origin,
    PatRes, Res, Ty, TypeId,
};
use ratio_names::{Loaded, ModuleId};
use ratio_syntax::Span;
use ratio_syntax::ast::{
    self, Arg, BinaryOp, Condition, Else, FieldInit, ListPattern, PatternKind, RecordLit, StrPart,
    UnaryOp,
};

use crate::ir::{
    ArgFrom, Arm, AssignOp, BinOp, Callee, Catch, EnumDef, Expr, ExprKind, FieldType, Func, Item,
    ItemId, ItemKind, ListItem, Lit, Loc, Module, Package, Param, Pattern, PlaceStep, Program,
    PromptPart, RecordPart, RecordPat, SectionKind, Stmt, Type, UnOp, VariantDef,
};

/// Why a program could not be lowered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LowerError {
    /// The check reported errors (or the load did): only clean programs are lowered.
    NotClean,
    /// The tree is nested deeper than the lowering follows (only when no large stack could be
    /// started).
    TooDeep,
    /// The checker's tables do not describe a node: a bug in the checker or here.
    Internal(String),
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::NotClean => f.write_str("only a cleanly checked program can be lowered"),
            LowerError::TooDeep => f.write_str("the program is nested too deeply to lower"),
            LowerError::Internal(m) => write!(f, "internal lowering error: {m}"),
        }
    }
}

impl std::error::Error for LowerError {}

type Res_<T> = Result<T, LowerError>;

fn internal<T>(what: impl Into<String>) -> Res_<T> {
    Err(LowerError::Internal(what.into()))
}

/// The prelude enums the interpreter builds values of itself (errors and results).
pub const RUNTIME_ENUMS: [&str; 5] = ["ErrorKind", "BudgetKind", "DeniedBy", "Result", "DiffOp"];

/// Whether the built-in a program calls by `name` is pure (its table entry has `@pure`).
fn pure_builtins() -> &'static HashMap<String, bool> {
    static PURE: OnceLock<HashMap<String, bool>> = OnceLock::new();
    PURE.get_or_init(|| {
        ratio_check::builtin_rules()
            .into_iter()
            .map(|r| {
                let pure = r.attrs.iter().any(|a| a == "pure");
                (r.name, pure)
            })
            .collect()
    })
}

/// One function's frame while it is lowered.
#[derive(Default)]
struct Frame {
    slots: HashMap<LocalId, u32>,
    next: u32,
    captures: Vec<(u32, u32)>,
}

struct Lowerer<'a> {
    loaded: &'a Loaded,
    checked: &'a Checked,
    module: ModuleId,
    frames: Vec<Frame>,
    enums: HashMap<EnumId, u32>,
    type_ids: HashMap<TypeId, u32>,
    type_dedup: HashMap<Type, u32>,
    prog: Program,
    depth: usize,
    max_depth: usize,
}

/// Lowers every module of a cleanly checked load.
///
/// # Errors
///
/// [`LowerError::NotClean`] when the load or the check has errors; [`LowerError::Internal`]
/// when the tables do not describe the tree (a bug).
pub fn lower(loaded: &Loaded, checked: &Checked) -> Result<Program, LowerError> {
    if !loaded.is_clean() || !checked.is_clean() {
        return Err(LowerError::NotClean);
    }
    let done = crate::stack::run(|max_depth| {
        let mut program = lower_on(loaded, checked, max_depth)?;
        crate::canon::finish(&mut program, max_depth).ok_or(LowerError::TooDeep)?;
        Ok(program)
    });
    done.unwrap_or(Err(LowerError::TooDeep))
}

fn lower_on(loaded: &Loaded, checked: &Checked, max_depth: usize) -> Res_<Program> {
    let mut l = Lowerer {
        loaded,
        checked,
        module: ModuleId(0),
        frames: Vec::new(),
        enums: HashMap::new(),
        type_ids: HashMap::new(),
        type_dedup: HashMap::new(),
        prog: Program::default(),
        depth: 0,
        max_depth,
    };
    // The enums the interpreter itself builds values of.
    for (id, r) in checked.enum_items.iter().enumerate() {
        if r.origin == Origin::Std(0) {
            let def = checked
                .types
                .enum_def(EnumId(u32::try_from(id).unwrap_or(u32::MAX)));
            if RUNTIME_ENUMS.contains(&def.name.as_str()) {
                l.enum_index(EnumId(u32::try_from(id).unwrap_or(u32::MAX)))?;
            }
        }
    }
    let mut packages: Vec<Package> = Vec::new();
    for (i, m) in loaded.modules.iter().enumerate() {
        l.module = ModuleId(i);
        let package = m.package.to_string();
        let mut items = Vec::with_capacity(m.ast.items.len());
        let has_table = checked
            .table(ModuleId(i))
            .is_some_and(|t| !t.items.is_empty())
            || m.ast.items.is_empty();
        for item in &m.ast.items {
            items.push(if has_table { l.item(item)? } else { None });
        }
        l.prog.modules.push(Module {
            package: package.clone(),
            path: m.path.to_string(),
            items,
        });
        let index = u32::try_from(i).unwrap_or(u32::MAX);
        match packages.iter_mut().find(|p| p.name == package) {
            Some(p) => p.modules.push(index),
            None => packages.push(Package {
                name: package,
                modules: vec![index],
                hash: [0; 32],
            }),
        }
    }
    l.prog.packages = packages;
    Ok(l.prog)
}

impl Lowerer<'_> {
    fn table(&self) -> &ModuleTable {
        // A module without a table lowers no items, so this is reached only with one.
        &self.checked.tables[self.module.0]
    }

    fn loc(&self, span: Span) -> Loc {
        Loc {
            module: u32::try_from(self.module.0).unwrap_or(u32::MAX),
            line: span.line,
            col: span.col,
        }
    }

    fn enter(&mut self) -> Res_<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(LowerError::TooDeep);
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn expr_of(&self, kind: ExprKind, span: Span) -> Expr {
        Expr {
            kind,
            loc: self.loc(span),
            id: 0,
        }
    }

    // ---- frames and locals ----

    fn push_frame(&mut self) {
        self.frames.push(Frame::default());
    }

    fn fresh_slot(&mut self) -> u32 {
        let f = self.frames.last_mut().expect("a frame");
        let s = f.next;
        f.next += 1;
        s
    }

    /// Declares a local of the current frame.
    fn declare(&mut self, id: LocalId) -> u32 {
        let slot = self.fresh_slot();
        self.frames
            .last_mut()
            .expect("a frame")
            .slots
            .insert(id, slot);
        slot
    }

    /// The slot of local `id` in frame `level`, capturing it from enclosing frames if needed.
    fn slot_in(&mut self, level: usize, id: LocalId) -> Res_<u32> {
        if let Some(&s) = self.frames[level].slots.get(&id) {
            return Ok(s);
        }
        if level == 0 {
            return internal(format!("local {} is not in scope", id.0));
        }
        let outer = self.slot_in(level - 1, id)?;
        let f = &mut self.frames[level];
        let own = f.next;
        f.next += 1;
        f.slots.insert(id, own);
        f.captures.push((outer, own));
        Ok(own)
    }

    fn local(&mut self, id: LocalId) -> Res_<u32> {
        let level = self.frames.len() - 1;
        self.slot_in(level, id)
    }

    fn bound_at(&mut self, span: Span) -> Res_<u32> {
        match self.table().patterns.get(&Key::from(span)) {
            Some(PatRes::Bind(id)) => {
                let id = *id;
                Ok(self.declare(id))
            }
            _ => internal(format!("no binding recorded at {}:{}", span.line, span.col)),
        }
    }

    // ---- enums and types ----

    fn enum_index(&mut self, id: EnumId) -> Res_<u32> {
        if let Some(&i) = self.enums.get(&id) {
            return Ok(i);
        }
        let def = self.checked.types.enum_def(id).clone();
        let origin = match self.checked.enum_items.get(id.0 as usize) {
            Some(ItemRef {
                origin: Origin::Std(s),
                ..
            }) => self
                .checked
                .std_names()
                .get(*s)
                .copied()
                .unwrap_or("std")
                .to_owned(),
            Some(ItemRef {
                origin: Origin::Module(m),
                ..
            }) => match self.loaded.module(*m) {
                Some(m) => format!("{} {}", m.package, m.path),
                None => return internal("an enum of an unknown module"),
            },
            None => return internal("an enum with no declaration"),
        };
        let index = u32::try_from(self.prog.enums.len()).unwrap_or(u32::MAX);
        self.enums.insert(id, index);
        self.prog.enums.push(EnumDef {
            origin,
            name: def.name.clone(),
            params: def
                .params
                .iter()
                .map(|p| self.checked.types.param_def(*p).name.clone())
                .collect(),
            variants: def
                .variants
                .iter()
                .map(|v| VariantDef {
                    name: v.name.clone(),
                    payload: None,
                })
                .collect(),
        });
        for (vi, v) in def.variants.iter().enumerate() {
            if let Some(p) = v.payload {
                let t = self.type_index(p)?;
                self.prog.enums[index as usize].variants[vi].payload = Some(t);
            }
        }
        Ok(index)
    }

    fn intern_type(&mut self, t: Type) -> u32 {
        if let Some(&i) = self.type_dedup.get(&t) {
            return i;
        }
        let i = u32::try_from(self.prog.types.len()).unwrap_or(u32::MAX);
        self.prog.types.push(t.clone());
        self.type_dedup.insert(t, i);
        i
    }

    fn type_index(&mut self, id: TypeId) -> Res_<u32> {
        if let Some(&i) = self.type_ids.get(&id) {
            return Ok(i);
        }
        self.enter()?;
        let ty = self.checked.types.get(id).clone();
        let t = match ty {
            Ty::Error | Ty::Var(_) => Type::Error,
            Ty::Never => Type::Never,
            Ty::Unit => Type::Unit,
            Ty::Bool => Type::Bool,
            Ty::Int => Type::Int,
            Ty::Float => Type::Float,
            Ty::Duration => Type::Duration,
            Ty::Money => Type::Money,
            Ty::Str => Type::Str,
            Ty::UserText => Type::UserText,
            Ty::Brief => Type::Brief,
            Ty::UntrustedStr => Type::UntrustedStr,
            Ty::Json => Type::Json,
            Ty::Image => Type::Image,
            Ty::Prompt => Type::Prompt,
            Ty::TrustedPrompt => Type::TrustedPrompt,
            Ty::Transcript => Type::Transcript,
            Ty::Range => Type::Range,
            Ty::ToolName => Type::ToolName,
            Ty::ToolSet => Type::ToolSet,
            Ty::Handle(h) => Type::Handle(h.name().to_owned()),
            Ty::Task(t) => Type::Task(self.type_index(t)?),
            Ty::Option(t) => Type::Option(self.type_index(t)?),
            Ty::List(t) => Type::List(self.type_index(t)?),
            Ty::Map(k, v) => Type::Map(self.type_index(k)?, self.type_index(v)?),
            Ty::Tuple(ts) => Type::Tuple(self.type_list(&ts)?),
            Ty::Record(fields) => {
                let metas = self.checked.types.field_meta(id).map(<[_]>::to_vec);
                let mut out = Vec::with_capacity(fields.len());
                for (i, f) in fields.iter().enumerate() {
                    let meta = metas
                        .as_ref()
                        .and_then(|m| m.get(i))
                        .map(meta_text)
                        .unwrap_or_default();
                    out.push(FieldType {
                        name: f.name.to_string(),
                        ty: self.type_index(f.ty)?,
                        optional: f.optional,
                        meta,
                    });
                }
                Type::Record(out)
            }
            Ty::Enum(e, args) => Type::Enum(self.enum_index(e)?, self.type_list(&args)?),
            Ty::Untrusted(t) => Type::Untrusted(self.type_index(t)?),
            Ty::Secret(t) => Type::Secret(self.type_index(t)?),
            Ty::Fn(ps, r) => Type::Fn(self.type_list(&ps)?, self.type_index(r)?),
            Ty::Harness(ps, r) => Type::Harness(self.type_list(&ps)?, self.type_index(r)?),
            Ty::Param(p) => Type::Param(self.checked.types.param_def(p).name.clone()),
        };
        self.leave();
        let i = self.intern_type(t);
        self.type_ids.insert(id, i);
        Ok(i)
    }

    fn type_list(&mut self, ts: &[TypeId]) -> Res_<Vec<u32>> {
        ts.iter().map(|t| self.type_index(*t)).collect()
    }

    fn type_at(&self, span: Span) -> Option<TypeId> {
        self.table().type_at(span)
    }

    // ---- items ----

    fn item(&mut self, item: &ast::Item) -> Res_<Option<Item>> {
        let attrs = item.attrs.iter().map(|a| a.name.name.clone()).collect();
        let (name, kind) = match &item.kind {
            ast::ItemKind::Fn(d) => (
                d.name.name.clone(),
                ItemKind::Fn(self.func(&d.params, &d.body)?),
            ),
            ast::ItemKind::Harness(d) => {
                let func = self.func(&d.params, &d.body)?;
                let config = match &d.config {
                    Some(c) => {
                        self.push_frame();
                        let e = self.record_lit(c, c.span)?;
                        self.frames.pop();
                        Some(e)
                    }
                    None => None,
                };
                (d.name.name.clone(), ItemKind::Harness { func, config })
            }
            ast::ItemKind::Prompt(d) => {
                self.push_frame();
                let params = self.params(&d.params)?;
                let body = self.prompt_expr(&d.body)?;
                let frame = self.frames.pop().expect("a frame");
                (
                    d.name.name.clone(),
                    ItemKind::Prompt(Func {
                        params,
                        slots: frame.next,
                        captures: Vec::new(),
                        body,
                    }),
                )
            }
            ast::ItemKind::Const(d) => {
                self.push_frame();
                let value = self.expr(&d.value)?;
                let frame = self.frames.pop().expect("a frame");
                (
                    d.name.name.clone(),
                    ItemKind::Const {
                        value,
                        slots: frame.next,
                    },
                )
            }
            ast::ItemKind::State(d) => {
                self.push_frame();
                let value = self.expr(&d.value)?;
                let frame = self.frames.pop().expect("a frame");
                (
                    "state".to_owned(),
                    ItemKind::State {
                        value,
                        slots: frame.next,
                    },
                )
            }
            ast::ItemKind::Import(_)
            | ast::ItemKind::Type(_)
            | ast::ItemKind::Enum(_)
            | ast::ItemKind::Test(_) => return Ok(None),
        };
        Ok(Some(Item {
            name,
            public: item.public,
            attrs,
            kind,
        }))
    }

    fn params(&mut self, params: &[ast::Param]) -> Res_<Vec<Param>> {
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            let slot = self.bound_at(p.name.span)?;
            out.push(Param {
                pattern: Pattern::Bind { slot, sub: None },
                default: None,
            });
        }
        // Defaults may read earlier parameters, so they are lowered once all are bound.
        for (p, param) in params.iter().zip(out.iter_mut()) {
            if let Some(d) = &p.default {
                param.default = Some(self.expr(d)?);
            }
        }
        Ok(out)
    }

    fn func(&mut self, params: &[ast::Param], body: &ast::Block) -> Res_<Func> {
        self.push_frame();
        let params = self.params(params)?;
        let body = self.block(body)?;
        let frame = self.frames.pop().expect("a frame");
        Ok(Func {
            params,
            slots: frame.next,
            captures: frame.captures,
            body,
        })
    }

    // ---- blocks and statements ----

    fn block(&mut self, b: &ast::Block) -> Res_<Expr> {
        let mut stmts = Vec::with_capacity(b.stmts.len());
        for s in &b.stmts {
            stmts.push(self.stmt(s)?);
        }
        Ok(self.expr_of(ExprKind::Block(stmts), b.span))
    }

    fn stmt(&mut self, s: &ast::Stmt) -> Res_<Stmt> {
        Ok(match &s.kind {
            ast::StmtKind::Let { pattern, value, .. } => {
                let value = self.expr(value)?;
                let pattern = self.pattern(pattern, None)?;
                Stmt::Let { pattern, value }
            }
            ast::StmtKind::Var { name, value, .. } => {
                let value = self.expr(value)?;
                let slot = self.bound_at(name.span)?;
                Stmt::Var { slot, value }
            }
            ast::StmtKind::Assign { target, op, value } => {
                let mut steps = Vec::new();
                let mut root = target;
                loop {
                    match &root.kind {
                        ast::ExprKind::Member { object, name, .. } => {
                            steps.push(PlaceStep::Field(name.name.clone()));
                            root = object;
                        }
                        ast::ExprKind::Index { object, index } => {
                            steps.push(PlaceStep::Index(self.expr(index)?));
                            root = object;
                        }
                        ast::ExprKind::Paren(inner) => root = inner,
                        _ => break,
                    }
                }
                steps.reverse();
                let slot = match self.table().res.get(&Key::from(root.span)).cloned() {
                    Some(Res::Local(id)) => self.local(id)?,
                    _ => return internal("an assignment to something that is not a local"),
                };
                let op = match op {
                    ast::AssignOp::Set => AssignOp::Set,
                    ast::AssignOp::Add => AssignOp::Add,
                    ast::AssignOp::Sub => AssignOp::Sub,
                    ast::AssignOp::Mul => AssignOp::Mul,
                    ast::AssignOp::Div => AssignOp::Div,
                };
                let value = self.expr(value)?;
                Stmt::Assign {
                    slot,
                    path: steps,
                    op,
                    value,
                    loc: self.loc(s.span),
                }
            }
            ast::StmtKind::Defer(b) => Stmt::Defer(self.block(b)?),
            ast::StmtKind::Expr(e) => Stmt::Expr(self.expr(e)?),
        })
    }

    // ---- expressions ----

    fn expr(&mut self, e: &ast::Expr) -> Res_<Expr> {
        self.enter()?;
        let out = self.expr_inner(e);
        self.leave();
        out
    }

    fn boxed(&mut self, e: &ast::Expr) -> Res_<Box<Expr>> {
        Ok(Box::new(self.expr(e)?))
    }

    fn res(&self, span: Span) -> Option<Res> {
        self.table().res.get(&Key::from(span)).cloned()
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of expression")]
    fn expr_inner(&mut self, e: &ast::Expr) -> Res_<Expr> {
        use ast::ExprKind as K;
        let kind = match &e.kind {
            K::Error => return internal("an error node in a clean program"),
            K::Lit(lit) => return self.literal(lit, e.span),
            K::Name(id) => self.name(id.span, e.span)?,
            K::Paren(inner) => return self.expr(inner),
            K::Tuple(items) => ExprKind::Tuple(self.exprs(items)?),
            K::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let x = self.expr(&item.expr)?;
                    out.push(if item.spread {
                        ListItem::Spread(x)
                    } else {
                        ListItem::One(x)
                    });
                }
                let list = self.expr_of(ExprKind::List(out), e.span);
                if self.table().coercions.get(&Key::from(e.span)) == Some(&Coercion::ToolSet) {
                    ExprKind::ToolSet(Box::new(list))
                } else {
                    return Ok(list);
                }
            }
            K::Record(lit) => return self.record_lit(lit, e.span),
            K::Variant { fields, .. } => {
                let Some(Res::Variant(en, index)) = self.res(e.span) else {
                    return internal("a variant literal the checker did not resolve");
                };
                let enum_ = self.enum_index(en)?;
                let payload_ty = self.checked.types.enum_def(en).variants[index].payload;
                let payload = match payload_ty {
                    Some(t) => {
                        let ty = self.type_index(t)?;
                        let parts = self.record_parts(fields)?;
                        Some(Box::new(
                            self.expr_of(ExprKind::Record { parts, ty }, fields.span),
                        ))
                    }
                    None => None,
                };
                ExprKind::Variant {
                    enum_,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    payload,
                }
            }
            K::Block(b) | K::Do(b) => return self.block(b),
            K::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Neg => UnOp::Neg,
                    UnaryOp::Not => UnOp::Not,
                };
                ExprKind::Unary(op, self.boxed(operand)?)
            }
            K::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs)?,
            K::Call { callee, args, .. } => return self.call(e, callee, args),
            K::Member {
                object,
                name,
                optional,
            } => return self.member(e, object, &name.name, *optional),
            K::Index { object, index } => ExprKind::Index {
                object: self.boxed(object)?,
                index: self.boxed(index)?,
            },
            K::WithOverride { call, config } => ExprKind::Override {
                call: self.boxed(call)?,
                config: Box::new(self.record_lit(config, config.span)?),
            },
            K::If { cond, then, else_ } => {
                if let Condition::Let { .. } = cond.as_ref() {
                    // The pattern's bindings are declared before the branch that uses them.
                    return self.if_let(e, cond, then, else_.as_deref());
                }
                let Condition::Expr(c) = cond.as_ref() else {
                    return internal("an if without a condition");
                };
                let cond = self.boxed(c)?;
                let then = Box::new(self.block(then)?);
                ExprKind::If {
                    cond,
                    then,
                    else_: self.else_branch(else_.as_deref())?,
                }
            }
            K::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: self.boxed(scrutinee)?,
                arms: self.arms(arms)?,
            },
            K::For {
                pattern,
                iter,
                body,
            } => {
                let iter = self.boxed(iter)?;
                let pattern = self.pattern(pattern, None)?;
                ExprKind::For {
                    pattern,
                    iter,
                    body: Box::new(self.block(body)?),
                }
            }
            K::While { cond, body } => match cond.as_ref() {
                Condition::Expr(c) => ExprKind::While {
                    cond: self.boxed(c)?,
                    body: Box::new(self.block(body)?),
                },
                Condition::Let { pattern, value } => {
                    let unwrap = self.unwraps(pattern, value);
                    let value = self.boxed(value)?;
                    let pattern = self.pattern(pattern, None)?;
                    ExprKind::WhileLet {
                        pattern,
                        value,
                        unwrap,
                        body: Box::new(self.block(body)?),
                    }
                }
            },
            K::Loop(b) => ExprKind::Loop(Box::new(self.block(b)?)),
            K::Repeat { count, body } => ExprKind::Repeat {
                count: self.boxed(count)?,
                body: Box::new(self.block(body)?),
            },
            K::Try { body, catch } => {
                let body = Box::new(self.block(body)?);
                let catch = match catch {
                    ast::Catch::Arms(arms) => Catch::Arms(self.arms(arms)?),
                    ast::Catch::Bind { name, body } => {
                        let slot = self.bound_at(name.span)?;
                        Catch::Bind(slot, Box::new(self.block(body)?))
                    }
                };
                ExprKind::Try { body, catch }
            }
            K::Par(xs) => ExprKind::Par(self.exprs(xs)?),
            K::Race(xs) => ExprKind::Race(self.exprs(xs)?),
            K::WithTimeout {
                duration,
                body,
                else_,
            } => ExprKind::WithTimeout {
                duration: self.boxed(duration)?,
                body: Box::new(self.block(body)?),
                else_: match else_ {
                    Some(b) => Some(Box::new(self.block(b)?)),
                    None => None,
                },
            },
            K::With { config, body } => ExprKind::With {
                config: Box::new(self.record_lit(config, config.span)?),
                body: Box::new(self.block(body)?),
            },
            K::Closure { params, body } => {
                self.push_frame();
                let mut ps = Vec::with_capacity(params.len());
                for p in params {
                    ps.push(Param {
                        pattern: self.pattern(&p.pattern, None)?,
                        default: None,
                    });
                }
                let body = self.expr(body)?;
                let frame = self.frames.pop().expect("a frame");
                ExprKind::Closure(Box::new(Func {
                    params: ps,
                    slots: frame.next,
                    captures: frame.captures,
                    body,
                }))
            }
            K::FnExpr { params, body, .. } => ExprKind::Closure(Box::new(self.func(params, body)?)),
            K::Prompt(body) => return self.prompt_expr(body),
            K::Return(x) => ExprKind::Return(self.opt(x.as_deref())?),
            K::Break(x) => ExprKind::Break(self.opt(x.as_deref())?),
            K::Continue => ExprKind::Continue,
            K::Raise(x) => ExprKind::Raise(self.boxed(x)?),
        };
        Ok(self.expr_of(kind, e.span))
    }

    fn opt(&mut self, x: Option<&ast::Expr>) -> Res_<Option<Box<Expr>>> {
        match x {
            Some(x) => Ok(Some(self.boxed(x)?)),
            None => Ok(None),
        }
    }

    fn exprs(&mut self, xs: &[ast::Expr]) -> Res_<Vec<Expr>> {
        xs.iter().map(|x| self.expr(x)).collect()
    }

    fn arms(&mut self, arms: &[ast::Arm]) -> Res_<Vec<Arm>> {
        let mut out = Vec::with_capacity(arms.len());
        for a in arms {
            let pattern = self.pattern(&a.pattern, None)?;
            let guard = match &a.guard {
                Some(g) => Some(self.expr(g)?),
                None => None,
            };
            out.push(Arm {
                pattern,
                guard,
                body: self.expr(&a.body)?,
            });
        }
        Ok(out)
    }

    /// Whether `if let pattern = value` matches the present value of an option.
    fn unwraps(&self, pattern: &ast::Pattern, value: &ast::Expr) -> bool {
        let is_option = self
            .type_at(value.span)
            .is_some_and(|t| matches!(self.checked.types.get(t), Ty::Option(_)));
        let none_pattern = matches!(pattern.kind, PatternKind::Lit(ast::Lit::None))
            && !matches!(
                self.table().patterns.get(&Key::from(pattern.span)),
                Some(PatRes::Variant(..))
            );
        is_option && !none_pattern
    }

    fn else_branch(&mut self, else_: Option<&Else>) -> Res_<Option<Box<Expr>>> {
        Ok(match else_ {
            Some(Else::If(x)) => Some(self.boxed(x)?),
            Some(Else::Block(b)) => Some(Box::new(self.block(b)?)),
            None => None,
        })
    }

    fn if_let(
        &mut self,
        e: &ast::Expr,
        cond: &Condition,
        then: &ast::Block,
        else_: Option<&Else>,
    ) -> Res_<Expr> {
        let Condition::Let { pattern, value } = cond else {
            return internal("if_let without a let condition");
        };
        let unwrap = self.unwraps(pattern, value);
        let value = self.boxed(value)?;
        let pattern = self.pattern(pattern, None)?;
        let then = Box::new(self.block(then)?);
        let else_ = self.else_branch(else_)?;
        Ok(self.expr_of(
            ExprKind::IfLet {
                pattern,
                value,
                unwrap,
                then,
                else_,
            },
            e.span,
        ))
    }

    fn literal(&mut self, lit: &ast::Lit, span: Span) -> Res_<Expr> {
        let kind = match lit {
            ast::Lit::Int(v) | ast::Lit::Quantity(v) => ExprKind::Lit(Lit::Int(*v)),
            ast::Lit::Float(v) => ExprKind::Lit(Lit::Float(*v)),
            ast::Lit::Duration(v) => ExprKind::Lit(Lit::Duration(*v)),
            ast::Lit::Money(v) => ExprKind::Lit(Lit::Money(*v)),
            ast::Lit::Bool(v) => ExprKind::Lit(Lit::Bool(*v)),
            ast::Lit::None => match self.res(span) {
                Some(Res::Variant(en, index)) => ExprKind::Variant {
                    enum_: self.enum_index(en)?,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    payload: None,
                },
                _ => ExprKind::Lit(Lit::None),
            },
            ast::Lit::Str(s) => return self.string(s, span),
        };
        Ok(self.expr_of(kind, span))
    }

    fn string(&mut self, s: &ast::StringLit, span: Span) -> Res_<Expr> {
        let plain = s.parts.iter().all(|p| matches!(p, StrPart::Text(_)));
        if plain {
            let text: String = s
                .parts
                .iter()
                .map(|p| match p {
                    StrPart::Text(t) => t.as_str(),
                    StrPart::Interp { .. } => "",
                })
                .collect();
            let coercion = self.table().coercions.get(&Key::from(span)).copied();
            return Ok(match coercion {
                Some(Coercion::ModelName) => self.model_name(text, span)?,
                Some(Coercion::ToolPattern) => self.expr_of(ExprKind::ToolName(text), span),
                _ => self.expr_of(ExprKind::Lit(Lit::Str(text)), span),
            });
        }
        let mut parts = Vec::with_capacity(s.parts.len());
        for p in &s.parts {
            match p {
                StrPart::Text(t) => {
                    if !t.is_empty() {
                        parts.push(self.expr_of(ExprKind::Lit(Lit::Str(t.clone())), span));
                    }
                }
                StrPart::Interp { expr, spec, span } => {
                    let value = self.boxed(expr)?;
                    parts.push(self.expr_of(
                        ExprKind::Show {
                            value,
                            spec: spec.clone(),
                        },
                        *span,
                    ));
                }
            }
        }
        Ok(self.expr_of(ExprKind::Concat(parts), span))
    }

    /// A string literal where a `ModelRef` is expected: `ModelRef.Named { id }`.
    fn model_name(&mut self, text: String, span: Span) -> Res_<Expr> {
        let Some(t) = self.type_at(span) else {
            return internal("a model name without a type");
        };
        let t = match self.checked.types.get(t) {
            Ty::Option(inner) => *inner,
            _ => t,
        };
        let Some(en) = self.checked.types.as_enum(t) else {
            return internal("a model name whose type is not an enum");
        };
        let Some(index) = self.checked.types.enum_def(en).variant("Named") else {
            return internal("ModelRef has no Named variant");
        };
        let payload_ty = self.checked.types.enum_def(en).variants[index].payload;
        let enum_ = self.enum_index(en)?;
        let ty = match payload_ty {
            Some(p) => self.type_index(p)?,
            None => return internal("ModelRef.Named has no payload"),
        };
        let id = self.expr_of(ExprKind::Lit(Lit::Str(text)), span);
        let payload = self.expr_of(
            ExprKind::Record {
                parts: vec![RecordPart::Field("id".to_owned(), id)],
                ty,
            },
            span,
        );
        Ok(self.expr_of(
            ExprKind::Variant {
                enum_,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                payload: Some(Box::new(payload)),
            },
            span,
        ))
    }

    /// A name expression at `span` (the identifier's span is `id_span`).
    fn name(&mut self, id_span: Span, span: Span) -> Res_<ExprKind> {
        let res = self.res(span).or_else(|| self.res(id_span));
        self.resolved(res, span)
    }

    fn resolved(&mut self, res: Option<Res>, span: Span) -> Res_<ExprKind> {
        Ok(match res {
            Some(Res::Local(id)) => ExprKind::Local(self.local(id)?),
            Some(Res::Item(r)) => self.item_ref(r)?,
            Some(Res::Variant(en, index)) => ExprKind::Variant {
                enum_: self.enum_index(en)?,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                payload: None,
            },
            Some(Res::Tool(name)) => ExprKind::ToolName(name),
            other => {
                return internal(format!(
                    "a name at {}:{} resolved to {other:?}",
                    span.line, span.col
                ));
            }
        })
    }

    fn item_ref(&mut self, r: ItemRef) -> Res_<ExprKind> {
        Ok(match r.origin {
            Origin::Module(m) => ExprKind::Item(ItemId {
                module: u32::try_from(m.0).unwrap_or(u32::MAX),
                item: u32::try_from(r.index).unwrap_or(u32::MAX),
            }),
            Origin::Std(_) => match self.checked.builtin_name(r) {
                Some(name) => ExprKind::Builtin(name),
                None => return internal("a built-in with no name"),
            },
        })
    }

    fn member(
        &mut self,
        e: &ast::Expr,
        object: &ast::Expr,
        name: &str,
        optional: bool,
    ) -> Res_<Expr> {
        let res = self.res(e.span);
        match res {
            Some(Res::Field(_)) | None => {
                let obj = self.expr(object)?;
                if optional {
                    let slot = self.fresh_slot();
                    let local = self.expr_of(ExprKind::Local(slot), object.span);
                    let then = self.expr_of(
                        ExprKind::Field {
                            object: Box::new(local),
                            name: name.to_owned(),
                        },
                        e.span,
                    );
                    Ok(self.expr_of(
                        ExprKind::OptChain {
                            object: Box::new(obj),
                            slot,
                            then: Box::new(then),
                        },
                        e.span,
                    ))
                } else {
                    Ok(self.expr_of(
                        ExprKind::Field {
                            object: Box::new(obj),
                            name: name.to_owned(),
                        },
                        e.span,
                    ))
                }
            }
            other => {
                let kind = self.resolved(other, e.span)?;
                Ok(self.expr_of(kind, e.span))
            }
        }
    }

    fn binary(
        &mut self,
        e: &ast::Expr,
        op: BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Res_<ExprKind> {
        let op = match op {
            BinaryOp::Pipe => {
                let (callee, args): (&ast::Expr, &[Arg]) = match &rhs.kind {
                    ast::ExprKind::Call { callee, args, .. } => (callee, args),
                    _ => (rhs, &[]),
                };
                return Ok(self
                    .call_with(e.span, callee, Some((lhs, false)), args)?
                    .kind);
            }
            BinaryOp::Coalesce => {
                return Ok(ExprKind::Coalesce(self.boxed(lhs)?, self.boxed(rhs)?));
            }
            BinaryOp::Or => return Ok(ExprKind::Or(self.boxed(lhs)?, self.boxed(rhs)?)),
            BinaryOp::And => return Ok(ExprKind::And(self.boxed(lhs)?, self.boxed(rhs)?)),
            BinaryOp::Is => {
                let value = self.boxed(lhs)?;
                let Some(Res::Variant(en, index)) = self.res(rhs.span) else {
                    return internal("an `is` test the checker did not resolve");
                };
                return Ok(ExprKind::Is {
                    value,
                    enum_: self.enum_index(en)?,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                });
            }
            BinaryOp::Eq => BinOp::Eq,
            BinaryOp::Ne => BinOp::Ne,
            BinaryOp::Lt => BinOp::Lt,
            BinaryOp::Le => BinOp::Le,
            BinaryOp::Gt => BinOp::Gt,
            BinaryOp::Ge => BinOp::Ge,
            BinaryOp::In => BinOp::In,
            BinaryOp::Range => BinOp::Range,
            BinaryOp::RangeInclusive => BinOp::RangeInclusive,
            BinaryOp::Add => BinOp::Add,
            BinaryOp::Sub => BinOp::Sub,
            BinaryOp::Mul => BinOp::Mul,
            BinaryOp::Div => BinOp::Div,
            BinaryOp::Rem => BinOp::Rem,
        };
        Ok(ExprKind::Binary(op, self.boxed(lhs)?, self.boxed(rhs)?))
    }

    fn call(&mut self, e: &ast::Expr, callee: &ast::Expr, args: &[Arg]) -> Res_<Expr> {
        // A method call: `x.f(a)` (receiver `x`) unless `x` is a module or the member is a
        // field holding a function.
        if let ast::ExprKind::Member {
            object, optional, ..
        } = &callee.kind
        {
            let info = self.table().calls.get(&Key::from(e.span)).cloned();
            let has_receiver = info
                .as_ref()
                .is_some_and(|i| i.args.contains(&ArgSource::Receiver));
            if has_receiver {
                return self.call_with(e.span, callee, Some((object, *optional)), args);
            }
            if let Some(Res::Field(_)) = self.res(callee.span) {
                // `r.f(a)`: call the function in field `f` (through `?.` when optional).
                return self.call_with(e.span, callee, None, args);
            }
        }
        self.call_with(e.span, callee, None, args)
    }

    /// A call at `span` of `callee`, with a receiver (`x.f(…)` or `x |> f(…)`) and the
    /// written arguments.
    fn call_with(
        &mut self,
        span: Span,
        callee: &ast::Expr,
        receiver: Option<(&ast::Expr, bool)>,
        args: &[Arg],
    ) -> Res_<Expr> {
        let Some(info) = self.table().calls.get(&Key::from(span)).cloned() else {
            return internal(format!(
                "a call at {}:{} the checker did not record",
                span.line, span.col
            ));
        };
        let CallInfo {
            callee: target,
            type_args,
            args: sources,
        } = info;
        // Optional receiver: `x?.f(a)` is `none` when `x` is.
        if let Some((object, true)) = receiver {
            let obj = self.expr(object)?;
            let slot = self.fresh_slot();
            let local = self.expr_of(ExprKind::Local(slot), object.span);
            let then = self.finish_call(
                span,
                callee,
                target,
                &type_args,
                &sources,
                Some(local),
                args,
            )?;
            return Ok(self.expr_of(
                ExprKind::OptChain {
                    object: Box::new(obj),
                    slot,
                    then: Box::new(then),
                },
                span,
            ));
        }
        // A function held in an optional record's field: `r?.f(a)`.
        if receiver.is_none()
            && target.is_none()
            && let ast::ExprKind::Member {
                object,
                name,
                optional: true,
            } = &callee.kind
            && matches!(self.res(callee.span), Some(Res::Field(_)))
        {
            let obj = self.expr(object)?;
            let slot = self.fresh_slot();
            let local = self.expr_of(ExprKind::Local(slot), object.span);
            let field = self.expr_of(
                ExprKind::Field {
                    object: Box::new(local),
                    name: name.name.clone(),
                },
                callee.span,
            );
            let then = self.value_call(span, field, &sources, None, args)?;
            return Ok(self.expr_of(
                ExprKind::OptChain {
                    object: Box::new(obj),
                    slot,
                    then: Box::new(then),
                },
                span,
            ));
        }
        let recv = match receiver {
            Some((object, _)) => Some(self.expr(object)?),
            None => None,
        };
        self.finish_call(span, callee, target, &type_args, &sources, recv, args)
    }

    #[allow(clippy::too_many_arguments, reason = "a call's parts")]
    fn finish_call(
        &mut self,
        span: Span,
        callee: &ast::Expr,
        target: Option<ItemRef>,
        type_args: &[TypeId],
        sources: &[ArgSource],
        receiver: Option<Expr>,
        args: &[Arg],
    ) -> Res_<Expr> {
        let Some(r) = target else {
            // A function value: the callee expression itself (for `r.f(a)`, the field).
            let f = match &callee.kind {
                ast::ExprKind::Member { object, name, .. }
                    if matches!(self.res(callee.span), Some(Res::Field(_))) =>
                {
                    let obj = self.expr(object)?;
                    self.expr_of(
                        ExprKind::Field {
                            object: Box::new(obj),
                            name: name.name.clone(),
                        },
                        callee.span,
                    )
                }
                _ => self.expr(callee)?,
            };
            return self.value_call(span, f, sources, receiver, args);
        };
        let has_receiver = receiver.is_some();
        let mut written: Vec<Expr> = Vec::with_capacity(args.len() + 1);
        if let Some(r) = receiver {
            written.push(r);
        }
        for a in args {
            written.push(self.expr(&a.value)?);
        }
        let shift = u32::from(has_receiver);
        let order: Vec<ArgFrom> = sources
            .iter()
            .map(|s| match s {
                ArgSource::Receiver => ArgFrom::Written(0),
                ArgSource::Arg(k) => {
                    ArgFrom::Written(u32::try_from(*k).unwrap_or(u32::MAX) + shift)
                }
                ArgSource::Default => ArgFrom::Default,
            })
            .collect();
        let type_args = self.type_list(type_args)?;
        let kind = match self.item_ref(r)? {
            ExprKind::Item(id) => ExprKind::Call {
                callee: Callee::Item(id),
                args: written,
                order,
                type_args,
            },
            ExprKind::Builtin(name) => {
                let pure = pure_builtins().get(&name).copied().unwrap_or(false);
                if pure {
                    ExprKind::Call {
                        callee: Callee::Builtin(name),
                        args: written,
                        order,
                        type_args,
                    }
                } else {
                    ExprKind::Effect {
                        builtin: name,
                        args: written,
                        order,
                        type_args,
                    }
                }
            }
            _ => return internal("a call of something that is not a function"),
        };
        Ok(self.expr_of(kind, span))
    }

    fn value_call(
        &mut self,
        span: Span,
        f: Expr,
        sources: &[ArgSource],
        receiver: Option<Expr>,
        args: &[Arg],
    ) -> Res_<Expr> {
        let has_receiver = receiver.is_some();
        let mut written: Vec<Expr> = Vec::with_capacity(args.len() + 1);
        if let Some(r) = receiver {
            written.push(r);
        }
        for a in args {
            written.push(self.expr(&a.value)?);
        }
        let shift = u32::from(has_receiver);
        let order = sources
            .iter()
            .map(|s| match s {
                ArgSource::Receiver => ArgFrom::Written(0),
                ArgSource::Arg(k) => {
                    ArgFrom::Written(u32::try_from(*k).unwrap_or(u32::MAX) + shift)
                }
                ArgSource::Default => ArgFrom::Default,
            })
            .collect();
        Ok(self.expr_of(
            ExprKind::Call {
                callee: Callee::Value(Box::new(f)),
                args: written,
                order,
                type_args: Vec::new(),
            },
            span,
        ))
    }

    fn record_parts(&mut self, lit: &RecordLit) -> Res_<Vec<RecordPart>> {
        let mut parts = Vec::with_capacity(lit.fields.len());
        for f in &lit.fields {
            parts.push(match f {
                FieldInit::Field { name, value } => {
                    RecordPart::Field(name.name.clone(), self.expr(value)?)
                }
                FieldInit::Shorthand(id) => {
                    let kind = self.name(id.span, id.span)?;
                    RecordPart::Field(id.name.clone(), self.expr_of(kind, id.span))
                }
                FieldInit::Spread(x) => RecordPart::Spread(self.expr(x)?),
            });
        }
        Ok(parts)
    }

    fn record_lit(&mut self, lit: &RecordLit, span: Span) -> Res_<Expr> {
        let coercion = self
            .table()
            .coercions
            .get(&Key::from(span))
            .or_else(|| self.table().coercions.get(&Key::from(lit.span)))
            .copied();
        match coercion {
            Some(Coercion::RecordToMap) => {
                let mut entries = Vec::with_capacity(lit.fields.len());
                for f in &lit.fields {
                    match f {
                        FieldInit::Field { name, value } => {
                            entries.push((name.name.clone(), self.expr(value)?));
                        }
                        FieldInit::Shorthand(id) => {
                            let kind = self.name(id.span, id.span)?;
                            entries.push((id.name.clone(), self.expr_of(kind, id.span)));
                        }
                        FieldInit::Spread(_) => return internal("a spread in a map literal"),
                    }
                }
                return Ok(self.expr_of(ExprKind::Map(entries), span));
            }
            Some(Coercion::ToolSet) => {
                let parts = self.record_parts(lit)?;
                let ty = self.synth_record(&parts);
                let inner = self.expr_of(ExprKind::Record { parts, ty }, span);
                return Ok(self.expr_of(ExprKind::ToolSet(Box::new(inner)), span));
            }
            _ => {}
        }
        let parts = self.record_parts(lit)?;
        let ty = match self.type_at(span).or_else(|| self.type_at(lit.span)) {
            Some(t) if matches!(self.checked.types.get(t), Ty::Record(_)) => self.type_index(t)?,
            _ => self.synth_record(&parts),
        };
        Ok(self.expr_of(ExprKind::Record { parts, ty }, span))
    }

    /// A record type with the written field names (for configuration records, whose type the
    /// tables do not hold): every field of type `Error`.
    fn synth_record(&mut self, parts: &[RecordPart]) -> u32 {
        let err = self.intern_type(Type::Error);
        let mut fields: Vec<FieldType> = parts
            .iter()
            .filter_map(|p| match p {
                RecordPart::Field(n, _) => Some(FieldType {
                    name: n.clone(),
                    ty: err,
                    optional: false,
                    meta: String::new(),
                }),
                RecordPart::Spread(_) => None,
            })
            .collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        fields.dedup_by(|a, b| a.name == b.name);
        self.intern_type(Type::Record(fields))
    }

    // ---- prompts ----

    fn prompt_expr(&mut self, body: &ast::PromptBody) -> Res_<Expr> {
        let parts = self.prompt_parts(body)?;
        Ok(self.expr_of(ExprKind::Prompt(parts), body.span))
    }

    fn prompt_parts(&mut self, body: &ast::PromptBody) -> Res_<Vec<PromptPart>> {
        use ast::SectionKind as S;
        self.enter()?;
        let mut out = Vec::with_capacity(body.sections.len());
        for s in &body.sections {
            out.push(match &s.kind {
                S::System(x) => PromptPart::Section(SectionKind::System, vec![self.expr(x)?]),
                S::Instruct(x) => PromptPart::Section(SectionKind::Instruct, vec![self.expr(x)?]),
                S::Task(x) => PromptPart::Section(SectionKind::Task, vec![self.expr(x)?]),
                S::Steps(x) => PromptPart::Section(SectionKind::Steps, vec![self.expr(x)?]),
                S::Context(x) => PromptPart::Section(SectionKind::Context, vec![self.expr(x)?]),
                S::Note(x) => PromptPart::Section(SectionKind::Note, vec![self.expr(x)?]),
                S::Include(x) => PromptPart::Section(SectionKind::Include, vec![self.expr(x)?]),
                S::Data { value, label } | S::Image { value, label } => {
                    let kind = if matches!(s.kind, S::Data { .. }) {
                        SectionKind::Data
                    } else {
                        SectionKind::Image
                    };
                    let mut xs = vec![self.expr(value)?];
                    if let Some(l) = label {
                        xs.push(self.expr(l)?);
                    }
                    PromptPart::Section(kind, xs)
                }
                S::Example { input, output } => PromptPart::Section(
                    SectionKind::Example,
                    vec![self.expr(input)?, self.expr(output)?],
                ),
                S::Format(t) => PromptPart::Format(type_text(t)),
                S::If { cond, then, else_ } => PromptPart::If {
                    cond: self.expr(cond)?,
                    then: self.prompt_parts(then)?,
                    else_: match else_ {
                        Some(b) => self.prompt_parts(b)?,
                        None => Vec::new(),
                    },
                },
                S::For {
                    pattern,
                    iter,
                    body,
                } => {
                    let iter = self.expr(iter)?;
                    let pattern = self.pattern(pattern, None)?;
                    PromptPart::For {
                        pattern,
                        iter,
                        body: self.prompt_parts(body)?,
                    }
                }
            });
        }
        self.leave();
        Ok(out)
    }

    // ---- patterns ----

    /// Lowers a pattern. `or_names` maps the names the first alternative of an enclosing
    /// or-pattern bound to their slots (the checker records only the first alternative's).
    fn pattern(
        &mut self,
        p: &ast::Pattern,
        or_names: Option<&HashMap<String, u32>>,
    ) -> Res_<Pattern> {
        self.enter()?;
        let mut names = HashMap::new();
        let out = self.pattern_inner(p, or_names, &mut names);
        self.leave();
        out
    }

    fn bind_name(
        &mut self,
        key: Span,
        name: &str,
        or_names: Option<&HashMap<String, u32>>,
        names: &mut HashMap<String, u32>,
    ) -> Res_<u32> {
        if let Some(map) = or_names
            && let Some(&slot) = map.get(name)
        {
            return Ok(slot);
        }
        let slot = self.bound_at(key)?;
        names.insert(name.to_owned(), slot);
        Ok(slot)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of pattern")]
    fn pattern_inner(
        &mut self,
        p: &ast::Pattern,
        or_names: Option<&HashMap<String, u32>>,
        names: &mut HashMap<String, u32>,
    ) -> Res_<Pattern> {
        self.enter()?;
        let recorded = self.table().patterns.get(&Key::from(p.span)).copied();
        let out = match &p.kind {
            PatternKind::Error => return internal("an error pattern in a clean program"),
            PatternKind::Wildcard => Pattern::Wild,
            PatternKind::Lit(lit) => match (lit, recorded) {
                (_, Some(PatRes::Variant(en, index))) => Pattern::Variant {
                    enum_: self.enum_index(en)?,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    fields: None,
                },
                (ast::Lit::Int(v) | ast::Lit::Quantity(v), _) => Pattern::Lit(Lit::Int(*v)),
                (ast::Lit::Float(v), _) => Pattern::Lit(Lit::Float(*v)),
                (ast::Lit::Duration(v), _) => Pattern::Lit(Lit::Duration(*v)),
                (ast::Lit::Money(v), _) => Pattern::Lit(Lit::Money(*v)),
                (ast::Lit::Bool(v), _) => Pattern::Lit(Lit::Bool(*v)),
                (ast::Lit::None, _) => Pattern::Lit(Lit::None),
                (ast::Lit::Str(s), _) => {
                    let text: String = s
                        .parts
                        .iter()
                        .map(|x| match x {
                            StrPart::Text(t) => t.as_str(),
                            StrPart::Interp { .. } => "",
                        })
                        .collect();
                    Pattern::Lit(Lit::Str(text))
                }
            },
            PatternKind::Name(id) => match recorded {
                Some(PatRes::Variant(en, index)) => Pattern::Variant {
                    enum_: self.enum_index(en)?,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    fields: None,
                },
                _ => Pattern::Bind {
                    slot: self.bind_name(p.span, &id.name, or_names, names)?,
                    sub: None,
                },
            },
            PatternKind::Bind { name, pattern } => {
                let slot = self.bind_name(name.span, &name.name, or_names, names)?;
                let sub = self.pattern_inner(pattern, or_names, names)?;
                Pattern::Bind {
                    slot,
                    sub: Some(Box::new(sub)),
                }
            }
            PatternKind::Variant { fields, .. } => {
                let Some(PatRes::Variant(en, index)) = recorded else {
                    return internal("a variant pattern the checker did not resolve");
                };
                let fields = match fields {
                    Some(rp) => Some(self.record_pattern(rp, or_names, names)?),
                    None => None,
                };
                Pattern::Variant {
                    enum_: self.enum_index(en)?,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    fields,
                }
            }
            PatternKind::Tuple(items) => {
                let mut out = Vec::with_capacity(items.len());
                for q in items {
                    out.push(self.pattern_inner(q, or_names, names)?);
                }
                Pattern::Tuple(out)
            }
            PatternKind::List(items) => {
                let mut before = Vec::new();
                let mut after = Vec::new();
                let mut rest = None;
                for item in items {
                    match item {
                        ListPattern::Pattern(q) => {
                            let q = self.pattern_inner(q, or_names, names)?;
                            if rest.is_some() {
                                after.push(q);
                            } else {
                                before.push(q);
                            }
                        }
                        ListPattern::Rest(name) => {
                            rest = Some(match name {
                                Some(n) => Some(self.bind_name(n.span, &n.name, or_names, names)?),
                                None => None,
                            });
                        }
                    }
                }
                Pattern::List {
                    before,
                    rest,
                    after,
                }
            }
            PatternKind::Record(rp) => Pattern::Record(self.record_pattern(rp, or_names, names)?),
            PatternKind::Or(alts) => {
                let mut out = Vec::with_capacity(alts.len());
                let mut first: Option<HashMap<String, u32>> = None;
                for alt in alts {
                    let mut own = HashMap::new();
                    let map = first.as_ref().or(or_names);
                    out.push(self.pattern_inner(alt, map, &mut own)?);
                    if first.is_none() {
                        let mut merged = or_names.cloned().unwrap_or_default();
                        merged.extend(own.iter().map(|(k, v)| (k.clone(), *v)));
                        names.extend(own);
                        first = Some(merged);
                    }
                }
                Pattern::Or(out)
            }
        };
        self.leave();
        Ok(out)
    }

    fn record_pattern(
        &mut self,
        rp: &ast::RecordPattern,
        or_names: Option<&HashMap<String, u32>>,
        names: &mut HashMap<String, u32>,
    ) -> Res_<RecordPat> {
        let mut fields = Vec::with_capacity(rp.fields.len());
        for f in &rp.fields {
            let pat = match &f.pattern {
                Some(q) => self.pattern_inner(q, or_names, names)?,
                None => Pattern::Bind {
                    slot: self.bind_name(f.name.span, &f.name.name, or_names, names)?,
                    sub: None,
                },
            };
            fields.push((f.name.name.clone(), pat));
        }
        Ok(RecordPat {
            fields,
            rest: rp.rest,
        })
    }
}

/// A field's schema refinements, canonically written.
fn meta_text(m: &ratio_check::FieldMeta) -> String {
    let mut out = String::new();
    if let Some(d) = &m.description {
        out.push_str("desc=");
        out.push_str(&crate::canon::quote(d));
        out.push(' ');
    }
    if let Some((a, b)) = m.len {
        let _ = write!(out, "len={a},{b} ");
    }
    if let Some((a, b)) = &m.range {
        let _ = write!(
            out,
            "range={},{} ",
            crate::canon::quote(a),
            crate::canon::quote(b)
        );
    }
    if let Some(p) = &m.pattern {
        out.push_str("pattern=");
        out.push_str(&crate::canon::quote(p));
        out.push(' ');
    }
    if let Some(x) = &m.example {
        out.push_str("example=");
        out.push_str(&crate::canon::quote(x));
        out.push(' ');
    }
    out.trim_end().to_owned()
}

/// A written type, canonically spaced (for `format` sections).
fn type_text(t: &ast::Type) -> String {
    let mut out = String::new();
    write_type(t, &mut out, 0);
    out
}

fn write_type(t: &ast::Type, out: &mut String, depth: usize) {
    use ast::TypeKind as T;
    if depth > 256 {
        out.push('…');
        return;
    }
    let list = |ts: &[ast::Type], out: &mut String| {
        for (i, x) in ts.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_type(x, out, depth + 1);
        }
    };
    match &t.kind {
        T::Error => out.push('?'),
        T::Named { path, args } => {
            let names: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
            out.push_str(&names.join("."));
            if !args.is_empty() {
                out.push('<');
                list(args, out);
                out.push('>');
            }
        }
        T::List(x) => {
            out.push('[');
            write_type(x, out, depth + 1);
            out.push(']');
        }
        T::Record(fields) => {
            out.push('{');
            for (i, f) in fields.iter().enumerate() {
                out.push_str(if i > 0 { ", " } else { " " });
                out.push_str(&f.name.name);
                if f.optional {
                    out.push('?');
                }
                out.push_str(": ");
                write_type(&f.ty, out, depth + 1);
            }
            out.push_str(" }");
        }
        T::Tuple(ts) => {
            out.push('(');
            list(ts, out);
            out.push(')');
        }
        T::Fn { params, ret } | T::Harness { params, ret } => {
            out.push_str(if matches!(t.kind, T::Fn { .. }) {
                "fn("
            } else {
                "harness("
            });
            list(params, out);
            out.push_str(") -> ");
            write_type(ret, out, depth + 1);
        }
        T::Option(x) => {
            write_type(x, out, depth + 1);
            out.push('?');
        }
    }
}
