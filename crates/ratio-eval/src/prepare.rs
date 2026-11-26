//! The prepared form (chapter 01, decision R2): the canonical IR lowered once more into what
//! the evaluator walks.
//!
//! Nodes live in one arena and refer to each other by index; literals are built values; record
//! fields are resolved to layouts and slots; built-ins to direct references into the table;
//! items, closures and prompt bodies to indexes; every node keeps its site id (its IR node
//! index) and source position. The prepared form is never serialised or hashed: changing how
//! it is built changes no hash, effect key or `IR_VERSION`. Step charges follow the IR's
//! nodes one for one (see [`crate::machine`]), so they do not depend on it either.

use std::collections::HashMap;
use std::sync::Arc;

use ratio_ir as ir;
use ratio_ir::{ArgFrom, AssignOp, BinOp, Loc, SectionKind, UnOp};

use crate::builtins;
use crate::value::{Layout, Value};

pub(crate) type NodeId = u32;
pub(crate) type PatId = u32;

#[derive(Debug)]
pub(crate) struct PNode {
    pub kind: PKind,
    pub loc: Loc,
    /// The IR node index (with the package hash, the site id).
    pub site: u32,
}

#[derive(Debug)]
pub(crate) enum PCallee {
    Item(u32),
    Builtin(u16),
    Value(NodeId),
    /// A pure built-in the interpreter does not implement yet (a later stage's).
    Missing(Arc<str>),
}

#[derive(Debug)]
pub(crate) enum PPart {
    Field(u32, NodeId),
    Spread(NodeId),
}

#[derive(Debug)]
pub(crate) enum PStep {
    Field(Arc<str>),
    Index(NodeId),
}

#[derive(Debug)]
pub(crate) enum PStmt {
    Let(PatId, NodeId),
    Var(u32, NodeId),
    Assign {
        slot: u32,
        path: Box<[PStep]>,
        op: AssignOp,
        value: NodeId,
        loc: Loc,
    },
    Defer(NodeId),
    Expr(NodeId),
}

#[derive(Debug)]
pub(crate) struct PArm {
    pub pat: PatId,
    pub guard: Option<NodeId>,
    pub body: NodeId,
}

#[derive(Debug)]
pub(crate) enum PCatch {
    Arms(Box<[PArm]>),
    Bind(u32, NodeId),
}

#[derive(Debug)]
pub(crate) enum PPrompt {
    Section(SectionKind, Box<[NodeId]>),
    Format(Arc<str>),
    If(NodeId, u32, u32),
    For(PatId, NodeId, u32),
}

#[derive(Debug)]
pub(crate) enum PKind {
    Const(Value),
    Local(u32),
    Item(u32),
    Builtin(u16),
    Concat(Box<[NodeId]>),
    Show(NodeId, Option<Arc<str>>),
    Tuple(Box<[NodeId]>),
    List(Box<[(bool, NodeId)]>),
    Record {
        layout: Arc<Layout>,
        parts: Box<[PPart]>,
    },
    Map(Box<[(Value, NodeId)]>),
    Variant {
        enum_: u32,
        index: u32,
        payload: Option<NodeId>,
    },
    Block(Box<[PStmt]>),
    Unary(UnOp, NodeId),
    Binary(BinOp, NodeId, NodeId),
    And(NodeId, NodeId),
    Or(NodeId, NodeId),
    Coalesce(NodeId, NodeId),
    Is(NodeId, u32, u32),
    Call {
        callee: PCallee,
        args: Box<[NodeId]>,
        order: Box<[ArgFrom]>,
        type_args: Box<[u32]>,
    },
    Effect {
        name: Arc<str>,
        args: Box<[NodeId]>,
        order: Box<[ArgFrom]>,
    },
    Field(NodeId, Arc<str>),
    OptChain(NodeId, u32, NodeId),
    Index(NodeId, NodeId),
    If(NodeId, NodeId, Option<NodeId>),
    IfLet {
        pat: PatId,
        value: NodeId,
        unwrap: bool,
        then: NodeId,
        else_: Option<NodeId>,
    },
    Match(NodeId, Box<[PArm]>),
    For(PatId, NodeId, NodeId),
    While(NodeId, NodeId),
    WhileLet {
        pat: PatId,
        value: NodeId,
        unwrap: bool,
        body: NodeId,
    },
    Loop(NodeId),
    Repeat(NodeId, NodeId),
    Break(Option<NodeId>),
    Continue,
    Return(Option<NodeId>),
    Raise(NodeId),
    Try(NodeId, PCatch),
    Closure(u32),
    Prompt(u32),
    Par(Box<[NodeId]>),
    Race(Box<[NodeId]>),
    /// A construct that needs a later stage (frames, the scheduler, a host): raises
    /// `Unsupported` without evaluating anything.
    Unsupported(&'static str),
    ToolName(Value),
}

#[derive(Debug)]
pub(crate) enum PPat {
    Wild,
    Bind(u32, Option<PatId>),
    Lit(Value),
    Variant(u32, u32, Option<PRecPat>),
    Tuple(Box<[PatId]>),
    List {
        before: Box<[PatId]>,
        #[allow(
            clippy::option_option,
            reason = "no rest, an unnamed rest, a named rest"
        )]
        rest: Option<Option<u32>>,
        after: Box<[PatId]>,
    },
    Record(PRecPat),
    Or(Box<[PatId]>),
}

#[derive(Debug)]
pub(crate) struct PRecPat {
    pub fields: Box<[(Arc<str>, PatId)]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FuncKind {
    Fn,
    Harness,
    Prompt,
    Closure,
}

#[derive(Debug)]
pub(crate) struct PFunc {
    pub params: Box<[(PatId, Option<NodeId>)]>,
    pub slots: u32,
    pub captures: Box<[(u32, u32)]>,
    pub body: NodeId,
    pub kind: FuncKind,
    pub name: Arc<str>,
}

#[derive(Debug)]
pub(crate) enum PItemKind {
    Func(u32),
    Const { node: NodeId, slots: u32 },
    State,
}

#[derive(Debug)]
pub(crate) struct PItem {
    pub kind: PItemKind,
}

#[derive(Debug)]
pub(crate) struct PVariant {
    pub name: Arc<str>,
    /// The name as JSON writes it (chapter 03: `snake_case`).
    pub json: Arc<str>,
    pub payload: Option<Arc<Layout>>,
    /// The payload's record type, by index into the program's types.
    pub payload_ty: Option<u32>,
}

#[derive(Debug)]
pub(crate) struct PEnum {
    /// The type parameters, in declared order.
    pub params: Vec<String>,
    pub variants: Box<[PVariant]>,
    pub unit_only: bool,
}

impl PEnum {
    pub(crate) fn variant(&self, name: &str) -> Option<u32> {
        self.variants
            .iter()
            .position(|v| &*v.name == name)
            .and_then(|i| u32::try_from(i).ok())
    }
}

/// The prelude enums and layouts the interpreter builds values of.
#[derive(Debug)]
pub(crate) struct Runtime {
    pub error_kind: u32,
    pub budget_kind: u32,
    pub denied_by: u32,
    pub result: u32,
    /// `DiffOp`, the `op` of a `diff` entry (stage 9).
    pub diff_op: u32,
    pub error: Arc<Layout>,
    pub site: Arc<Layout>,
}

/// Why a program could not be prepared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareError {
    /// A prelude enum the interpreter needs is missing from the program (a lowering bug).
    MissingRuntime(&'static str),
    /// The program is nested too deeply to prepare on this stack.
    TooDeep,
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrepareError::MissingRuntime(n) => {
                write!(f, "the program lacks the prelude enum `{n}`")
            }
            PrepareError::TooDeep => f.write_str("the program is nested too deeply to prepare"),
        }
    }
}

impl std::error::Error for PrepareError {}

/// A program ready to run: the prepared form of every module of a lowered [`ir::Program`].
#[derive(Debug)]
pub struct Prepared {
    pub(crate) nodes: Vec<PNode>,
    pub(crate) pats: Vec<PPat>,
    pub(crate) funcs: Vec<PFunc>,
    pub(crate) items: Vec<PItem>,
    pub(crate) prompts: Vec<Box<[PPrompt]>>,
    pub(crate) enums: Vec<PEnum>,
    pub(crate) types: Vec<ir::Type>,
    /// `(package, path)` of each module.
    pub(crate) modules: Vec<(Arc<str>, Arc<str>)>,
    /// The IR hash of each module's package (the first half of a site id).
    pub(crate) hashes: Vec<[u8; 32]>,
    pub(crate) rt: Runtime,
    by_name: HashMap<(String, String), u32>,
}

impl Prepared {
    /// Every item that can be looked up with [`Prepared::find`]: `(module path, name)`, in
    /// module and source order.
    #[must_use]
    pub fn item_names(&self) -> Vec<(String, String)> {
        let mut names: Vec<((String, String), u32)> =
            self.by_name.iter().map(|(k, v)| (k.clone(), *v)).collect();
        names.sort_by_key(|(_, v)| *v);
        names.into_iter().map(|(k, _)| k).collect()
    }

    /// The item called `name` in the module at `path` (of any package), if it is a function
    /// or constant the evaluator can run.
    #[must_use]
    pub fn find(&self, path: &str, name: &str) -> Option<Entry> {
        self.by_name
            .get(&(path.to_owned(), name.to_owned()))
            .map(|&i| Entry(i))
    }

    pub(crate) fn node(&self, id: NodeId) -> &PNode {
        &self.nodes[id as usize]
    }

    /// The length of a variant's name as a rendering writes it (its weight as a value).
    pub(crate) fn variant_weight(&self, enum_: u32, index: u32) -> u32 {
        self.enums
            .get(enum_ as usize)
            .and_then(|e| e.variants.get(index as usize))
            .map_or(0, |v| u32::try_from(v.json.len()).unwrap_or(u32::MAX))
    }

    pub(crate) fn layout_of(&self, enum_: u32, index: u32) -> Option<&Arc<Layout>> {
        self.enums
            .get(enum_ as usize)?
            .variants
            .get(index as usize)?
            .payload
            .as_ref()
    }
}

/// An item to start an evaluation at (see [`Prepared::find`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry(pub(crate) u32);

/// Prepares a lowered program.
///
/// # Errors
///
/// When the program lacks the prelude enums (a lowering bug) or is too deep for the stack.
pub fn prepare(program: &ir::Program) -> Result<Arc<Prepared>, PrepareError> {
    crate::stack::run(|max_depth| prepare_on(program, max_depth))
        .unwrap_or(Err(PrepareError::TooDeep))
        .map(Arc::new)
}

fn layout_from(types: &[ir::Type], ty: u32) -> Arc<Layout> {
    let names = match types.get(ty as usize) {
        Some(ir::Type::Record(fields)) => {
            fields.iter().map(|f| Arc::from(f.name.as_str())).collect()
        }
        _ => Box::default(),
    };
    Arc::new(Layout { names })
}

fn prepare_on(program: &ir::Program, max_depth: usize) -> Result<Prepared, PrepareError> {
    let enums: Vec<PEnum> = program
        .enums
        .iter()
        .map(|e| PEnum {
            params: e.params.clone(),
            unit_only: e.unit_only(),
            variants: e
                .variants
                .iter()
                .map(|v| PVariant {
                    name: Arc::from(v.name.as_str()),
                    json: Arc::from(ratio_check::snake_case(&v.name).as_str()),
                    payload: v.payload.map(|t| layout_from(&program.types, t)),
                    payload_ty: v.payload,
                })
                .collect(),
        })
        .collect();
    let find = |name: &'static str| {
        program
            .enums
            .iter()
            .position(|e| e.origin == "prelude" && e.name == name)
            .and_then(|i| u32::try_from(i).ok())
            .ok_or(PrepareError::MissingRuntime(name))
    };
    let rt = Runtime {
        error_kind: find("ErrorKind")?,
        budget_kind: find("BudgetKind")?,
        denied_by: find("DeniedBy")?,
        result: find("Result")?,
        diff_op: find("DiffOp")?,
        error: Arc::new(Layout {
            names: ["data", "kind", "message", "site"]
                .into_iter()
                .map(Arc::from)
                .collect(),
        }),
        site: Arc::new(Layout {
            names: ["column", "file", "frame", "line"]
                .into_iter()
                .map(Arc::from)
                .collect(),
        }),
    };
    let mut p = Preparer {
        program,
        out: Prepared {
            nodes: Vec::new(),
            pats: Vec::new(),
            funcs: Vec::new(),
            items: Vec::new(),
            prompts: Vec::new(),
            enums,
            types: program.types.clone(),
            modules: program
                .modules
                .iter()
                .map(|m| (Arc::from(m.package.as_str()), Arc::from(m.path.as_str())))
                .collect(),
            hashes: program
                .modules
                .iter()
                .map(|m| {
                    program
                        .packages
                        .iter()
                        .find(|pk| pk.name == m.package)
                        .map_or([0; 32], |pk| pk.hash)
                })
                .collect(),
            rt,
            by_name: HashMap::new(),
        },
        item_index: HashMap::new(),
        depth: 0,
        max_depth,
        too_deep: false,
    };
    // Number the items first, so references resolve whatever their order.
    for (mi, m) in program.modules.iter().enumerate() {
        for (ii, item) in m.items.iter().enumerate() {
            if item.is_some() {
                let index = u32::try_from(p.item_index.len()).unwrap_or(u32::MAX);
                p.item_index.insert(
                    ir::ItemId {
                        module: u32::try_from(mi).unwrap_or(u32::MAX),
                        item: u32::try_from(ii).unwrap_or(u32::MAX),
                    },
                    index,
                );
            }
        }
    }
    for (mi, m) in program.modules.iter().enumerate() {
        for item in m.items.iter().flatten() {
            let module = u32::try_from(mi).unwrap_or(u32::MAX);
            let name: Arc<str> = Arc::from(item.name.as_str());
            let kind = match &item.kind {
                ir::ItemKind::Fn(f) => PItemKind::Func(p.func(f, FuncKind::Fn, Arc::clone(&name))),
                ir::ItemKind::Harness { func, .. } => {
                    PItemKind::Func(p.func(func, FuncKind::Harness, Arc::clone(&name)))
                }
                ir::ItemKind::Prompt(f) => {
                    PItemKind::Func(p.func(f, FuncKind::Prompt, Arc::clone(&name)))
                }
                ir::ItemKind::Const { value, slots } => PItemKind::Const {
                    node: p.expr(value),
                    slots: *slots,
                },
                ir::ItemKind::State { .. } => PItemKind::State,
            };
            let index = u32::try_from(p.out.items.len()).unwrap_or(u32::MAX);
            p.out
                .by_name
                .insert((m.path.clone(), item.name.clone()), index);
            let _ = module;
            p.out.items.push(PItem { kind });
        }
    }
    if p.too_deep {
        return Err(PrepareError::TooDeep);
    }
    Ok(p.out)
}

struct Preparer<'a> {
    program: &'a ir::Program,
    out: Prepared,
    item_index: HashMap<ir::ItemId, u32>,
    depth: usize,
    max_depth: usize,
    too_deep: bool,
}

fn lit_value(l: &ir::Lit) -> Value {
    match l {
        ir::Lit::Unit => Value::unit(),
        ir::Lit::None => Value::none(),
        ir::Lit::Bool(b) => Value::bool(*b),
        ir::Lit::Int(i) => Value::int(*i),
        ir::Lit::Float(f) => Value::float(*f),
        ir::Lit::Duration(d) => Value::duration(*d),
        ir::Lit::Money(m) => Value::money(*m),
        ir::Lit::Str(s) => Value::text(s),
    }
}

impl Preparer<'_> {
    fn push(&mut self, kind: PKind, e: &ir::Expr) -> NodeId {
        let id = u32::try_from(self.out.nodes.len()).unwrap_or(u32::MAX);
        self.out.nodes.push(PNode {
            kind,
            loc: e.loc,
            site: e.id,
        });
        id
    }

    fn item(&self, id: ir::ItemId) -> Option<u32> {
        self.item_index.get(&id).copied()
    }

    fn func(&mut self, f: &ir::Func, kind: FuncKind, name: Arc<str>) -> u32 {
        let params = f
            .params
            .iter()
            .map(|p| {
                (
                    self.pat(&p.pattern),
                    p.default.as_ref().map(|d| self.expr(d)),
                )
            })
            .collect();
        let body = self.expr(&f.body);
        let id = u32::try_from(self.out.funcs.len()).unwrap_or(u32::MAX);
        self.out.funcs.push(PFunc {
            params,
            slots: f.slots,
            captures: f.captures.clone().into_boxed_slice(),
            body,
            kind,
            name,
        });
        id
    }

    fn exprs(&mut self, xs: &[ir::Expr]) -> Box<[NodeId]> {
        xs.iter().map(|x| self.expr(x)).collect()
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of node")]
    /// Enters one level of the tree; `false` (and the preparation refused) beyond the bound.
    fn enter(&mut self) -> bool {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.too_deep = true;
        }
        !self.too_deep
    }

    fn expr(&mut self, e: &ir::Expr) -> NodeId {
        let id = if self.enter() {
            self.expr_inner(e)
        } else {
            self.push(PKind::Unsupported("nested too deeply"), e)
        };
        self.depth -= 1;
        id
    }

    fn expr_inner(&mut self, e: &ir::Expr) -> NodeId {
        use ir::ExprKind as E;
        let kind = match &e.kind {
            E::Lit(l) => PKind::Const(lit_value(l)),
            E::Local(s) => PKind::Local(*s),
            E::Item(id) => match self.item(*id) {
                Some(i) => PKind::Item(i),
                None => PKind::Unsupported("a reference to an item that was not lowered"),
            },
            E::Builtin(name) => match builtins::lookup(name) {
                Some(b) => PKind::Builtin(b),
                None => PKind::Unsupported("a built-in function value a later stage implements"),
            },
            E::Concat(xs) => PKind::Concat(self.exprs(xs)),
            E::Show { value, spec } => {
                let v = self.expr(value);
                PKind::Show(v, spec.as_deref().map(Arc::from))
            }
            E::Tuple(xs) => PKind::Tuple(self.exprs(xs)),
            E::List(items) => PKind::List(
                items
                    .iter()
                    .map(|i| match i {
                        ir::ListItem::One(x) => (false, self.expr(x)),
                        ir::ListItem::Spread(x) => (true, self.expr(x)),
                    })
                    .collect(),
            ),
            E::Record { parts, ty } => {
                // The record type's fields, and any written field it lacks (a configuration
                // record's type is synthesised from what is written).
                let mut names: Vec<Arc<str>> = layout_from(&self.program.types, *ty).names.to_vec();
                for part in parts {
                    if let ir::RecordPart::Field(n, _) = part
                        && !names.iter().any(|x| &**x == n.as_str())
                    {
                        names.push(Arc::from(n.as_str()));
                    }
                }
                names.sort();
                let layout = Arc::new(Layout {
                    names: names.into_boxed_slice(),
                });
                let parts = parts
                    .iter()
                    .filter_map(|part| match part {
                        ir::RecordPart::Field(n, x) => {
                            let v = self.expr(x);
                            let slot = layout.index(n)?;
                            Some(PPart::Field(u32::try_from(slot).ok()?, v))
                        }
                        ir::RecordPart::Spread(x) => Some(PPart::Spread(self.expr(x))),
                    })
                    .collect();
                PKind::Record { layout, parts }
            }
            E::Map(entries) => PKind::Map(
                entries
                    .iter()
                    .map(|(k, x)| (Value::text(k), self.expr(x)))
                    .collect(),
            ),
            E::Variant {
                enum_,
                index,
                payload,
            } => PKind::Variant {
                enum_: *enum_,
                index: *index,
                payload: payload.as_ref().map(|x| self.expr(x)),
            },
            E::Block(stmts) => PKind::Block(stmts.iter().map(|s| self.stmt(s)).collect()),
            E::Unary(op, x) => PKind::Unary(*op, self.expr(x)),
            E::Binary(op, a, b) => {
                let a = self.expr(a);
                PKind::Binary(*op, a, self.expr(b))
            }
            E::And(a, b) => {
                let a = self.expr(a);
                PKind::And(a, self.expr(b))
            }
            E::Or(a, b) => {
                let a = self.expr(a);
                PKind::Or(a, self.expr(b))
            }
            E::Coalesce(a, b) => {
                let a = self.expr(a);
                PKind::Coalesce(a, self.expr(b))
            }
            E::Is {
                value,
                enum_,
                index,
            } => PKind::Is(self.expr(value), *enum_, *index),
            E::Call {
                callee,
                args,
                order,
                type_args,
            } => {
                let callee = match callee {
                    ir::Callee::Item(id) => match self.item(*id) {
                        Some(i) => PCallee::Item(i),
                        None => PCallee::Missing(Arc::from("an item that was not lowered")),
                    },
                    ir::Callee::Builtin(name) => match builtins::lookup(name) {
                        Some(b) => PCallee::Builtin(b),
                        None => PCallee::Missing(Arc::from(name.as_str())),
                    },
                    ir::Callee::Value(f) => PCallee::Value(self.expr(f)),
                };
                PKind::Call {
                    callee,
                    args: self.exprs(args),
                    order: order.clone().into_boxed_slice(),
                    type_args: type_args.clone().into_boxed_slice(),
                }
            }
            E::Effect {
                builtin,
                args,
                order,
                ..
            } => PKind::Effect {
                name: Arc::from(builtin.as_str()),
                args: self.exprs(args),
                order: order.clone().into_boxed_slice(),
            },
            E::Field { object, name } => PKind::Field(self.expr(object), Arc::from(name.as_str())),
            E::OptChain { object, slot, then } => {
                let o = self.expr(object);
                PKind::OptChain(o, *slot, self.expr(then))
            }
            E::Index { object, index } => {
                let o = self.expr(object);
                PKind::Index(o, self.expr(index))
            }
            E::If { cond, then, else_ } => {
                let c = self.expr(cond);
                let t = self.expr(then);
                PKind::If(c, t, else_.as_ref().map(|x| self.expr(x)))
            }
            E::IfLet {
                pattern,
                value,
                unwrap,
                then,
                else_,
            } => {
                let pat = self.pat(pattern);
                let value = self.expr(value);
                let then = self.expr(then);
                PKind::IfLet {
                    pat,
                    value,
                    unwrap: *unwrap,
                    then,
                    else_: else_.as_ref().map(|x| self.expr(x)),
                }
            }
            E::Match { scrutinee, arms } => {
                let s = self.expr(scrutinee);
                PKind::Match(s, self.arms(arms))
            }
            E::For {
                pattern,
                iter,
                body,
            } => {
                let pat = self.pat(pattern);
                let iter = self.expr(iter);
                PKind::For(pat, iter, self.expr(body))
            }
            E::While { cond, body } => {
                let c = self.expr(cond);
                PKind::While(c, self.expr(body))
            }
            E::WhileLet {
                pattern,
                value,
                unwrap,
                body,
            } => {
                let pat = self.pat(pattern);
                let value = self.expr(value);
                PKind::WhileLet {
                    pat,
                    value,
                    unwrap: *unwrap,
                    body: self.expr(body),
                }
            }
            E::Loop(b) => PKind::Loop(self.expr(b)),
            E::Repeat { count, body } => {
                let c = self.expr(count);
                PKind::Repeat(c, self.expr(body))
            }
            E::Break(x) => PKind::Break(x.as_ref().map(|x| self.expr(x))),
            E::Continue => PKind::Continue,
            E::Return(x) => PKind::Return(x.as_ref().map(|x| self.expr(x))),
            E::Raise(x) => PKind::Raise(self.expr(x)),
            E::Try { body, catch } => {
                let b = self.expr(body);
                let c = match catch {
                    ir::Catch::Arms(arms) => PCatch::Arms(self.arms(arms)),
                    ir::Catch::Bind(slot, x) => PCatch::Bind(*slot, self.expr(x)),
                };
                PKind::Try(b, c)
            }
            E::Closure(f) => PKind::Closure(self.func(f, FuncKind::Closure, Arc::from("closure"))),
            E::Prompt(parts) => PKind::Prompt(self.prompt(parts)),
            E::Par(xs) => PKind::Par(self.exprs(xs)),
            E::Race(xs) => PKind::Race(self.exprs(xs)),
            E::WithTimeout { .. } => {
                PKind::Unsupported("`with timeout` needs the scheduler (stage 11)")
            }
            E::With { .. } => PKind::Unsupported("`with` blocks need frames (stage 10)"),
            E::Override { .. } => PKind::Unsupported("call overrides need frames (stage 10)"),
            E::ToolName(n) => PKind::ToolName(Value::tool(crate::value::Str::plain(n))),
            E::ToolSet(_) => PKind::Unsupported("tool sets need frames (stage 10)"),
        };
        self.push(kind, e)
    }

    fn arms(&mut self, arms: &[ir::Arm]) -> Box<[PArm]> {
        arms.iter()
            .map(|a| PArm {
                pat: self.pat(&a.pattern),
                guard: a.guard.as_ref().map(|g| self.expr(g)),
                body: self.expr(&a.body),
            })
            .collect()
    }

    fn stmt(&mut self, s: &ir::Stmt) -> PStmt {
        match s {
            ir::Stmt::Let { pattern, value } => {
                let v = self.expr(value);
                PStmt::Let(self.pat(pattern), v)
            }
            ir::Stmt::Var { slot, value } => PStmt::Var(*slot, self.expr(value)),
            ir::Stmt::Assign {
                slot,
                path,
                op,
                value,
                loc,
            } => PStmt::Assign {
                slot: *slot,
                path: path
                    .iter()
                    .map(|p| match p {
                        ir::PlaceStep::Field(n) => PStep::Field(Arc::from(n.as_str())),
                        ir::PlaceStep::Index(x) => PStep::Index(self.expr(x)),
                    })
                    .collect(),
                op: *op,
                value: self.expr(value),
                loc: *loc,
            },
            ir::Stmt::Defer(x) => PStmt::Defer(self.expr(x)),
            ir::Stmt::Expr(x) => PStmt::Expr(self.expr(x)),
        }
    }

    fn prompt(&mut self, parts: &[ir::PromptPart]) -> u32 {
        if !self.enter() {
            self.depth -= 1;
            self.out.prompts.push(Box::default());
            return u32::try_from(self.out.prompts.len() - 1).unwrap_or(u32::MAX);
        }
        let id = self.prompt_inner(parts);
        self.depth -= 1;
        id
    }

    fn prompt_inner(&mut self, parts: &[ir::PromptPart]) -> u32 {
        let list: Box<[PPrompt]> = parts
            .iter()
            .map(|p| match p {
                ir::PromptPart::Section(kind, xs) => PPrompt::Section(*kind, self.exprs(xs)),
                ir::PromptPart::Format(t) => PPrompt::Format(Arc::from(t.as_str())),
                ir::PromptPart::If { cond, then, else_ } => {
                    let c = self.expr(cond);
                    let t = self.prompt(then);
                    PPrompt::If(c, t, self.prompt(else_))
                }
                ir::PromptPart::For {
                    pattern,
                    iter,
                    body,
                } => {
                    let pat = self.pat(pattern);
                    let iter = self.expr(iter);
                    PPrompt::For(pat, iter, self.prompt(body))
                }
            })
            .collect();
        let id = u32::try_from(self.out.prompts.len()).unwrap_or(u32::MAX);
        self.out.prompts.push(list);
        id
    }

    fn pat(&mut self, p: &ir::Pattern) -> PatId {
        let id = if self.enter() {
            self.pat_inner(p)
        } else {
            self.out.pats.push(PPat::Wild);
            u32::try_from(self.out.pats.len() - 1).unwrap_or(u32::MAX)
        };
        self.depth -= 1;
        id
    }

    fn pat_inner(&mut self, p: &ir::Pattern) -> PatId {
        let pat = match p {
            ir::Pattern::Wild => PPat::Wild,
            ir::Pattern::Bind { slot, sub } => PPat::Bind(*slot, sub.as_ref().map(|q| self.pat(q))),
            ir::Pattern::Lit(l) => PPat::Lit(lit_value(l)),
            ir::Pattern::Variant {
                enum_,
                index,
                fields,
            } => PPat::Variant(*enum_, *index, fields.as_ref().map(|rp| self.rec_pat(rp))),
            ir::Pattern::Tuple(ps) => PPat::Tuple(ps.iter().map(|q| self.pat(q)).collect()),
            ir::Pattern::List {
                before,
                rest,
                after,
            } => PPat::List {
                before: before.iter().map(|q| self.pat(q)).collect(),
                rest: *rest,
                after: after.iter().map(|q| self.pat(q)).collect(),
            },
            ir::Pattern::Record(rp) => PPat::Record(self.rec_pat(rp)),
            ir::Pattern::Or(ps) => PPat::Or(ps.iter().map(|q| self.pat(q)).collect()),
        };
        let id = u32::try_from(self.out.pats.len()).unwrap_or(u32::MAX);
        self.out.pats.push(pat);
        id
    }

    fn rec_pat(&mut self, rp: &ir::RecordPat) -> PRecPat {
        PRecPat {
            fields: rp
                .fields
                .iter()
                .map(|(n, q)| (Arc::from(n.as_str()), self.pat(q)))
                .collect(),
        }
    }
}

/// A program with nothing in it, for tests of value rendering.
#[cfg(test)]
pub(crate) fn empty_for_tests() -> Prepared {
    let layout = || {
        Arc::new(Layout {
            names: Box::default(),
        })
    };
    Prepared {
        nodes: Vec::new(),
        pats: Vec::new(),
        funcs: Vec::new(),
        items: Vec::new(),
        prompts: Vec::new(),
        enums: Vec::new(),
        types: Vec::new(),
        modules: Vec::new(),
        hashes: Vec::new(),
        rt: Runtime {
            error_kind: 0,
            budget_kind: 0,
            denied_by: 0,
            result: 0,
            diff_op: 0,
            error: layout(),
            site: layout(),
        },
        by_name: HashMap::new(),
    }
}
