//! The Ratio Core IR (chapter 01, decision R2): a tree of expressions in which names are
//! resolved, every effect is an explicit node with a site id, every loop is an explicit loop
//! node, closures name the outer locals they capture, and the sugar of the surface syntax is
//! gone (`|>` and method calls are calls, interpolation is [`ExprKind::Concat`] of
//! [`ExprKind::Show`] parts, optional chains are [`ExprKind::OptChain`], record literals that
//! build maps are [`ExprKind::Map`], defaults are explicit).
//!
//! Locals are numbered per function (a function's frame), captures first; items, enums and
//! types are referred to by index into the [`Program`], and the canonical serialisation
//! ([`crate::canon`]) writes every such reference by name, so the hash never depends on load
//! order or on any platform detail.

/// Where a node came from, for messages and error sites. Not part of the canonical form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loc {
    /// The module, by its index in [`Program::modules`].
    pub module: u32,
    pub line: u32,
    pub col: u32,
}

/// A whole load, lowered: every module that checked cleanly.
#[derive(Clone, Debug, Default)]
pub struct Program {
    /// By the loader's module id.
    pub modules: Vec<Module>,
    /// Every enum a lowered node or type refers to (the prelude's error enums always).
    pub enums: Vec<EnumDef>,
    /// Every type a node refers to (type arguments of built-in calls, record layouts).
    pub types: Vec<Type>,
    /// One entry per package of the load, in the order its first module was loaded.
    pub packages: Vec<Package>,
}

/// A package: its source-qualified name and the hash of its canonical IR.
#[derive(Clone, Debug)]
pub struct Package {
    /// `user:app`, `std:debate`.
    pub name: String,
    /// The modules of the package, by index into [`Program::modules`].
    pub modules: Vec<u32>,
    /// `sha256` over the canonical serialisation (see [`crate::canon`]).
    pub hash: [u8; 32],
}

/// One module.
#[derive(Clone, Debug)]
pub struct Module {
    /// The package's name, as in [`Package::name`].
    pub package: String,
    /// The module's path in its package (`main.ratio`, `lib/critics.ratio`).
    pub path: String,
    /// By the item's index in the source; `None` for items with no run-time meaning (imports,
    /// types, enums, tests) and for items not lowered.
    pub items: Vec<Option<Item>>,
}

/// A reference to an item: the module's index in [`Program::modules`] and the item's index
/// in its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId {
    pub module: u32,
    pub item: u32,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub public: bool,
    /// The attributes' names (`turn`, `task`), in source order.
    pub attrs: Vec<String>,
    pub kind: ItemKind,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Fn(Func),
    /// A harness: a function of a special kind (chapter 04), with its configuration record.
    Harness {
        func: Func,
        config: Option<Expr>,
    },
    /// A prompt template: a function whose body builds a `Prompt`.
    Prompt(Func),
    /// A constant: evaluated once, on first use.
    Const {
        value: Expr,
        /// The constant's frame size (a constant may bind locals in blocks).
        slots: u32,
    },
    /// The package's `state` initialiser.
    State {
        value: Expr,
        slots: u32,
    },
}

/// A function body: a declared function, a harness, a prompt, a closure or a `fn` expression.
#[derive(Clone, Debug)]
pub struct Func {
    pub params: Vec<Param>,
    /// The number of local slots in the function's frame, captures included.
    pub slots: u32,
    /// For a closure: `(outer, own)` pairs, the slot of the enclosing frame whose value it
    /// captures when it is made, and the slot of its own frame that value is copied into.
    pub captures: Vec<(u32, u32)>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct Param {
    /// What the argument is bound to (a slot, or a pattern for closure parameters).
    pub pattern: Pattern,
    /// The default, evaluated in the callee's frame after the given arguments are bound.
    pub default: Option<Expr>,
}

/// An expression. `id` is its node index within its package, assigned by the canonical
/// serialisation in pre-order; with the package hash it is the node's **site id**.
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub loc: Loc,
    pub id: u32,
}

/// A constant value of the IR.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Unit,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Whole microseconds.
    Duration(i64),
    /// Millionths of a dollar.
    Money(i64),
    Str(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    Range,
    RangeInclusive,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinOp {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::In => "in",
            BinOp::Range => "..",
            BinOp::RangeInclusive => "..=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
        }
    }
}

/// What a call calls.
#[derive(Clone, Debug)]
pub enum Callee {
    /// A declared function, prompt or harness of the program.
    Item(ItemId),
    /// A built-in function, by the name a program writes (`list.push`, `fail`).
    Builtin(String),
    /// A function value.
    Value(Box<Expr>),
}

/// Where a parameter's value comes from: the written argument at this index, or the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgFrom {
    Written(u32),
    Default,
}

#[derive(Clone, Debug)]
pub enum ListItem {
    One(Expr),
    /// `..xs`.
    Spread(Expr),
}

#[derive(Clone, Debug)]
pub enum RecordPart {
    Field(String, Expr),
    /// `..base`.
    Spread(Expr),
}

/// One step of an assignment's place: `x.f`, `x[i]`.
#[derive(Clone, Debug)]
pub enum PlaceStep {
    Field(String),
    Index(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        pattern: Pattern,
        value: Expr,
    },
    Var {
        slot: u32,
        value: Expr,
    },
    Assign {
        slot: u32,
        path: Vec<PlaceStep>,
        op: AssignOp,
        value: Expr,
        loc: Loc,
    },
    Defer(Expr),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub enum Catch {
    Arms(Vec<Arm>),
    /// `catch e { … }`: binds the error to the slot.
    Bind(u32, Box<Expr>),
}

/// The kinds of prompt section (chapter 05).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    System,
    Instruct,
    Task,
    Data,
    Image,
    Example,
    Steps,
    Context,
    Note,
    Include,
}

impl SectionKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SectionKind::System => "system",
            SectionKind::Instruct => "instruct",
            SectionKind::Task => "task",
            SectionKind::Data => "data",
            SectionKind::Image => "image",
            SectionKind::Example => "example",
            SectionKind::Steps => "steps",
            SectionKind::Context => "context",
            SectionKind::Note => "note",
            SectionKind::Include => "include",
        }
    }
}

#[derive(Clone, Debug)]
pub enum PromptPart {
    /// A section with its values (a `data` section's label, an `example`'s output).
    Section(SectionKind, Vec<Expr>),
    /// `format T`: the type as written (canonically spaced); stage 12 resolves it for
    /// structured generation.
    Format(String),
    If {
        cond: Expr,
        then: Vec<PromptPart>,
        else_: Vec<PromptPart>,
    },
    For {
        pattern: Pattern,
        iter: Expr,
        body: Vec<PromptPart>,
    },
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(Lit),
    Local(u32),
    /// A function, harness or prompt as a value, or a constant's value.
    Item(ItemId),
    /// A built-in function as a value.
    Builtin(String),
    /// Text built from parts (string interpolation), each of which is text.
    Concat(Vec<Expr>),
    /// `text.show(value)`, or an interpolation's format spec applied to it.
    Show {
        value: Box<Expr>,
        spec: Option<String>,
    },
    Tuple(Vec<Expr>),
    List(Vec<ListItem>),
    /// A record literal; `ty` is the record type it builds, by index into [`Program::types`].
    Record {
        parts: Vec<RecordPart>,
        ty: u32,
    },
    /// A record literal where a `Map<Str, V>` is expected.
    Map(Vec<(String, Expr)>),
    /// A variant literal: the enum by index into [`Program::enums`], the variant's index, and
    /// its payload record.
    Variant {
        enum_: u32,
        index: u32,
        payload: Option<Box<Expr>>,
    },
    /// Statements; the value is the last statement's when it is an expression, else `()`.
    Block(Vec<Stmt>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    /// `x is Variant`.
    Is {
        value: Box<Expr>,
        enum_: u32,
        index: u32,
    },
    /// A call of a pure function (or of a harness, which needs frames: stage 10). `args` are
    /// the written arguments, the receiver first, evaluated in this order; `order` says which
    /// fills each parameter.
    Call {
        callee: Callee,
        args: Vec<Expr>,
        order: Vec<ArgFrom>,
        /// The type arguments, by index into [`Program::types`].
        type_args: Vec<u32>,
    },
    /// A call of a built-in that makes effects: an explicit effect node whose id is the
    /// effect's site.
    Effect {
        builtin: String,
        args: Vec<Expr>,
        order: Vec<ArgFrom>,
        type_args: Vec<u32>,
    },
    /// A record field read.
    Field {
        object: Box<Expr>,
        name: String,
    },
    /// `object?.…`: `none` when the object is `none`, else `then` with the object's value in
    /// the slot.
    OptChain {
        object: Box<Expr>,
        slot: u32,
        then: Box<Expr>,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Option<Box<Expr>>,
    },
    /// `if let pattern = value`; `unwrap` when the value is an option matched as its present
    /// value (chapter 03, "`if let` and `while let`").
    IfLet {
        pattern: Pattern,
        value: Box<Expr>,
        unwrap: bool,
        then: Box<Expr>,
        else_: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    For {
        pattern: Pattern,
        iter: Box<Expr>,
        body: Box<Expr>,
    },
    While {
        cond: Box<Expr>,
        body: Box<Expr>,
    },
    WhileLet {
        pattern: Pattern,
        value: Box<Expr>,
        unwrap: bool,
        body: Box<Expr>,
    },
    Loop(Box<Expr>),
    Repeat {
        count: Box<Expr>,
        body: Box<Expr>,
    },
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
    Raise(Box<Expr>),
    Try {
        body: Box<Expr>,
        catch: Catch,
    },
    Closure(Box<Func>),
    Prompt(Vec<PromptPart>),
    Par(Vec<Expr>),
    Race(Vec<Expr>),
    WithTimeout {
        duration: Box<Expr>,
        body: Box<Expr>,
        else_: Option<Box<Expr>>,
    },
    With {
        config: Box<Expr>,
        body: Box<Expr>,
    },
    /// `call with { … }`.
    Override {
        call: Box<Expr>,
        config: Box<Expr>,
    },
    /// A tool name where a `ToolName` is expected (chapter 02, rule 10).
    ToolName(String),
    /// A list or record literal where a `ToolSet` is expected.
    ToolSet(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Wild,
    Bind {
        slot: u32,
        sub: Option<Box<Pattern>>,
    },
    Lit(Lit),
    Variant {
        enum_: u32,
        index: u32,
        fields: Option<RecordPat>,
    },
    Tuple(Vec<Pattern>),
    /// `[a, b, ..rest, z]`: the patterns before the rest, the rest (its slot, if named), and
    /// the patterns after it.
    List {
        before: Vec<Pattern>,
        rest: Option<Option<u32>>,
        after: Vec<Pattern>,
    },
    Record(RecordPat),
    Or(Vec<Pattern>),
}

#[derive(Clone, Debug)]
pub struct RecordPat {
    /// Fields in written order.
    pub fields: Vec<(String, Pattern)>,
    /// A trailing `..`.
    pub rest: bool,
}

/// An enum, named by where it is declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumDef {
    /// `prelude`, a standard module's name, or `<package> <path>` for a program's enum.
    pub origin: String,
    pub name: String,
    /// The type parameters, in declared order (only the prelude's `Result<T>` has one today).
    pub params: Vec<String>,
    pub variants: Vec<VariantDef>,
}

impl EnumDef {
    /// Whether no variant has a payload.
    #[must_use]
    pub fn unit_only(&self) -> bool {
        self.variants.iter().all(|v| v.payload.is_none())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantDef {
    pub name: String,
    /// The payload's record type, by index into [`Program::types`], in terms of the enum's
    /// parameters ([`Type::Param`]).
    pub payload: Option<u32>,
}

/// A type, structurally. Type parameters are kept by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// The type of something the checker reported (never in a clean program).
    Error,
    Never,
    Unit,
    Bool,
    Int,
    Float,
    Duration,
    Money,
    Str,
    UserText,
    Brief,
    UntrustedStr,
    Json,
    Image,
    Prompt,
    TrustedPrompt,
    Transcript,
    Range,
    ToolName,
    ToolSet,
    Handle(String),
    Task(u32),
    Option(u32),
    List(u32),
    Map(u32, u32),
    Tuple(Vec<u32>),
    /// Fields in name order.
    Record(Vec<FieldType>),
    /// An enum by index into [`Program::enums`], with its type arguments.
    Enum(u32, Vec<u32>),
    Untrusted(u32),
    Secret(u32),
    Fn(Vec<u32>, u32),
    Harness(Vec<u32>, u32),
    Param(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldType {
    pub name: String,
    pub ty: u32,
    pub optional: bool,
    /// The schema refinements of chapter 03 (`@len`, `@range`, `@pattern`, the description),
    /// canonically written; empty when there are none.
    pub meta: String,
}
