//! The syntax tree the parser builds (chapter 02's grammar).
//!
//! Every node carries the [`Span`] of the source it was read from. The tree is faithful to the
//! source: parentheses are kept ([`ExprKind::Paren`]), `T??` is two [`TypeKind::Option`] layers,
//! and a long string stays a long string, so the canonical printer can write it back and
//! [`Module::sexpr`] (which leaves spans out) can compare two trees.
//!
//! Its depth is bounded: the parser refuses a source nested deeper than the configured limit
//! (chapter 07), so code that walks the tree recursively stays within a fixed stack.

use crate::token::Span;

/// A whole source file: the optional `meta { … }` header, then the items.
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub meta: Option<Meta>,
    pub items: Vec<Item>,
    pub span: Span,
}

/// The file's `meta { … }` header.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

/// A name as written, with where it was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// A `///` documentation comment: the text after the three slashes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub text: String,
    pub span: Span,
}

/// `@name` or `@name(arguments)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub name: Ident,
    /// `None` for `@name`, `Some` (possibly empty) for `@name(…)`.
    pub args: Option<Vec<Arg>>,
    pub span: Span,
}

/// One item of a module, with the documentation and attributes written before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub docs: Vec<Doc>,
    pub attrs: Vec<Attribute>,
    pub public: bool,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Import(Import),
    Type(TypeDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    Fn(FnDecl),
    Harness(HarnessDecl),
    Prompt(PromptDecl),
    State(StateDecl),
    Test(TestDecl),
}

/// `import path [as name] [{ a, b }]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub path: ImportPath,
    pub alias: Option<Ident>,
    /// `None` without a list, `Some` with one (never empty).
    pub names: Option<Vec<Ident>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImportPath {
    /// `import "./helpers.ratio"`.
    File(StringLit),
    /// `std:debate`, `plugin:acme/review @"^1"`.
    Source(SourceName),
}

/// `[source:] a.b / c.d [@"version"]`.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceName {
    pub source: Option<Ident>,
    /// The `/`-separated parts, each a dotted name path.
    pub parts: Vec<Vec<Ident>>,
    pub version: Option<StringLit>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: Ident,
    /// The payload's record type, if the variant has one.
    pub fields: Option<Vec<FieldType>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub name: Ident,
    pub ty: Option<Type>,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: Ident,
    pub type_params: Vec<Ident>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// The configuration record after `with`.
    pub config: Option<RecordLit>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub body: PromptBody,
}

/// `state: T = initial`.
#[derive(Clone, Debug, PartialEq)]
pub struct StateDecl {
    pub ty: Type,
    pub value: Expr,
}

/// `test "name" { … }`.
#[derive(Clone, Debug, PartialEq)]
pub struct TestDecl {
    pub name: StringLit,
    pub body: Block,
}

/// `name: T [= default]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub ty: Type,
    pub default: Option<Expr>,
    pub span: Span,
}

// ---- types ----

#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// What the parser puts where it could not read a type.
    Error,
    /// `Str`, `std.Thing`, `Map<K, V>`.
    Named { path: Vec<Ident>, args: Vec<Type> },
    /// `[T]`.
    List(Box<Type>),
    /// `{ a: T, b?: U "description" }`.
    Record(Vec<FieldType>),
    /// `()` (Unit) and `(A, B)`; `(A)` is a one-element tuple type.
    Tuple(Vec<Type>),
    /// `fn(A, B) -> R`.
    Fn { params: Vec<Type>, ret: Box<Type> },
    /// `harness(A) -> R`.
    Harness { params: Vec<Type>, ret: Box<Type> },
    /// `T?`; `T??` (one token) is two layers.
    Option(Box<Type>),
}

/// A field of a record type.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldType {
    pub docs: Vec<Doc>,
    pub attrs: Vec<Attribute>,
    pub name: Ident,
    /// `name?: T`: the field may be absent.
    pub optional: bool,
    pub ty: Type,
    /// The schema description after the type.
    pub description: Option<StringLit>,
    pub span: Span,
}

// ---- blocks and statements ----

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Let {
        pattern: Pattern,
        ty: Option<Type>,
        value: Expr,
    },
    Var {
        name: Ident,
        ty: Option<Type>,
        value: Expr,
    },
    /// `place op value`; the parser checks that `target` is a place (a name followed by `.name`
    /// and `[index]` steps).
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    Defer(Block),
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
}

impl AssignOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AssignOp::Set => "=",
            AssignOp::Add => "+=",
            AssignOp::Sub => "-=",
            AssignOp::Mul => "*=",
            AssignOp::Div => "/=",
        }
    }
}

// ---- expressions ----

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// What the parser puts where it could not read an expression (a diagnostic says why).
    Error,
    Lit(Lit),
    Name(Ident),
    /// `( e )`, kept so the tree says what was written.
    Paren(Box<Expr>),
    /// `(a,)`, `(a, b)`.
    Tuple(Vec<Expr>),
    List(Vec<ListItem>),
    Record(RecordLit),
    /// `Denied { by: user }`, `Verdict.Fail { why }`.
    Variant {
        path: Vec<Ident>,
        fields: RecordLit,
    },
    Block(Block),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `f(args)` and `f::<T>(args)`.
    Call {
        callee: Box<Expr>,
        type_args: Vec<Type>,
        args: Vec<Arg>,
    },
    /// `x.name`, or `x?.name` when `optional`.
    Member {
        object: Box<Expr>,
        name: Ident,
        optional: bool,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    /// `call with { … }`: a call override (chapter 04).
    WithOverride {
        call: Box<Expr>,
        config: RecordLit,
    },
    If {
        cond: Box<Condition>,
        then: Block,
        else_: Option<Box<Else>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    For {
        pattern: Pattern,
        iter: Box<Expr>,
        body: Block,
    },
    While {
        cond: Box<Condition>,
        body: Block,
    },
    Loop(Block),
    Repeat {
        count: Box<Expr>,
        body: Block,
    },
    Try {
        body: Block,
        catch: Catch,
    },
    Par(Vec<Expr>),
    Race(Vec<Expr>),
    /// `with timeout d { … } [else { … }]`.
    WithTimeout {
        duration: Box<Expr>,
        body: Block,
        else_: Option<Block>,
    },
    /// `with { config } { … }`.
    With {
        config: RecordLit,
        body: Block,
    },
    Do(Block),
    /// `|a, b: T| body` and `|| body`.
    Closure {
        params: Vec<ClosureParam>,
        body: Box<Expr>,
    },
    /// `fn(a: T) -> R { … }`.
    FnExpr {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
    },
    Prompt(PromptBody),
    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Continue,
    Raise(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Int(i64),
    /// A quantity (`20k`): its scaled value, and the token kept apart from a plain integer.
    Quantity(i64),
    Float(f64),
    /// Whole microseconds.
    Duration(i64),
    /// Millionths of a dollar.
    Money(i64),
    Bool(bool),
    None,
    Str(StringLit),
}

/// A string literal: a short or long string with its interpolations, or a raw string.
#[derive(Clone, Debug, PartialEq)]
pub struct StringLit {
    pub long: bool,
    pub raw: bool,
    /// The text and interpolations in order (a raw string is one `Text` part, or none when empty).
    pub parts: Vec<StrPart>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// Text with escapes decoded and, in a long string, the indentation removed.
    Text(String),
    Interp {
        expr: Expr,
        /// The format spec after `:`.
        spec: Option<String>,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    /// `..xs` splices a list.
    pub spread: bool,
    pub expr: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordLit {
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldInit {
    /// `name: value`.
    Field { name: Ident, value: Expr },
    /// `name`, short for `name: name`.
    Shorthand(Ident),
    /// `..base`.
    Spread(Expr),
}

/// A call argument: positional, or `name: value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Pipe,
    Coalesce,
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    Is,
    Range,
    RangeInclusive,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinaryOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Pipe => "|>",
            BinaryOp::Coalesce => "??",
            BinaryOp::Or => "or",
            BinaryOp::And => "and",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::In => "in",
            BinaryOp::Is => "is",
            BinaryOp::Range => "..",
            BinaryOp::RangeInclusive => "..=",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
        }
    }

    /// The precedence level of chapter 02's table (1 binds loosest).
    #[must_use]
    pub const fn level(self) -> u8 {
        match self {
            BinaryOp::Pipe => 1,
            BinaryOp::Coalesce => 2,
            BinaryOp::Or => 3,
            BinaryOp::And => 4,
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::In
            | BinaryOp::Is => 6,
            BinaryOp::Range | BinaryOp::RangeInclusive => 7,
            BinaryOp::Add | BinaryOp::Sub => 8,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => 9,
        }
    }
}

/// The head of an `if` or `while`: an expression, or `let pattern = value`.
#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    Expr(Expr),
    Let { pattern: Pattern, value: Expr },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Else {
    If(Expr),
    Block(Block),
}

/// A `match` or `catch` arm: `pattern [if guard] => body`.
#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Catch {
    /// `catch { arms }`.
    Arms(Vec<Arm>),
    /// `catch e { … }`.
    Bind { name: Ident, body: Block },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClosureParam {
    pub pattern: Pattern,
    pub ty: Option<Type>,
}

// ---- patterns ----

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    /// What the parser puts where it could not read a pattern.
    Error,
    Wildcard,
    Lit(Lit),
    /// A bare name: a binding, or a variant of the scrutinee's enum (rule 6 decides later).
    Name(Ident),
    /// `name @ pattern`.
    Bind { name: Ident, pattern: Box<Pattern> },
    /// A qualified variant (`Verdict.Fail`) or a variant with a payload (`Denied { by }`).
    Variant {
        path: Vec<Ident>,
        fields: Option<RecordPattern>,
    },
    Tuple(Vec<Pattern>),
    List(Vec<ListPattern>),
    Record(RecordPattern),
    /// `a | b | c`.
    Or(Vec<Pattern>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ListPattern {
    Pattern(Pattern),
    /// `..` or `..rest`.
    Rest(Option<Ident>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordPattern {
    pub fields: Vec<FieldPattern>,
    /// A trailing `..`: other fields are ignored.
    pub rest: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldPattern {
    pub name: Ident,
    pub pattern: Option<Pattern>,
}

// ---- prompts ----

#[derive(Clone, Debug, PartialEq)]
pub struct PromptBody {
    pub sections: Vec<Section>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub kind: SectionKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SectionKind {
    System(Expr),
    Instruct(Expr),
    Task(Expr),
    Data { value: Expr, label: Option<Expr> },
    Image { value: Expr, label: Option<Expr> },
    Example { input: Expr, output: Expr },
    Format(Type),
    Steps(Expr),
    Context(Expr),
    Note(Expr),
    Include(Expr),
    If {
        cond: Expr,
        then: PromptBody,
        else_: Option<PromptBody>,
    },
    For {
        pattern: Pattern,
        iter: Expr,
        body: PromptBody,
    },
}
