//! What the checker leaves for later stages: side tables keyed by the source span of the node
//! they describe.
//!
//! The syntax tree has no node ids, and the parser builds it once; later stages walk the same
//! tree. Every expression of a cleanly parsed module has its own span (a parent's span strictly
//! contains its children's), so `(start, end)` byte offsets name an expression within its
//! module, and likewise a pattern. The tables are per module ([`ModuleTable`]):
//!
//! - `types`: the type of every expression the checker visited (and of a shorthand record
//!   field `{ x }`, keyed by the identifier's span);
//! - `res`: what a name, member or variant literal resolved to ([`Res`]);
//! - `calls`: for every call (including method calls and `|>`), the callee, the inferred type
//!   arguments and which argument fills each parameter ([`CallInfo`]);
//! - `coercions`: literals that build a value of another type ([`Coercion`]);
//! - `patterns`: what each binding or variant pattern means ([`PatRes`]);
//! - `locals`: every local binding, by [`LocalId`].

use std::collections::HashMap;

use ratio_names::ModuleId;
use ratio_syntax::Span;

use crate::types::{EnumId, TypeId};

/// A node's key in a module's tables: its span's byte offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub start: usize,
    pub end: usize,
}

impl From<Span> for Key {
    fn from(span: Span) -> Self {
        Key {
            start: span.start,
            end: span.end,
        }
    }
}

/// Where a declaration lives: a loaded module, or a standard module's built-in declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    Module(ModuleId),
    /// A built-in source, by its index in [`crate::Checked::std_names`] (`0` is the prelude of
    /// shared types).
    Std(usize),
}

/// A declaration: an item of a module (by its index in the module's items) or a built-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemRef {
    pub origin: Origin,
    pub index: usize,
}

/// A local binding of a module, by its index in [`ModuleTable::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// A local binding.
#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub ty: TypeId,
    /// Declared with `var`.
    pub mutable: bool,
    pub span: Span,
}

/// What a name or member resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Res {
    Local(LocalId),
    /// A function, harness, prompt, constant or built-in function.
    Item(ItemRef),
    /// A module (an import's or a standard module), as the object of a member access.
    Module(Origin),
    /// An enum variant: a contextual or qualified unit variant, a variant literal or pattern.
    Variant(EnumId, usize),
    /// A tool name where a `ToolName` or `ToolSet` is expected (chapter 02, rule 10).
    Tool(String),
    /// A field of a record value.
    Field(String),
}

/// Where a parameter's value comes from in a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgSource {
    /// The receiver of a method call or the left side of `|>`.
    Receiver,
    /// The argument at this index of the call's argument list.
    Arg(usize),
    /// The parameter's default.
    Default,
}

/// A checked call.
#[derive(Clone, Debug)]
pub struct CallInfo {
    /// The declaration called, or `None` for a call of a function value.
    pub callee: Option<ItemRef>,
    /// The type arguments, inferred or from the turbofish, in parameter order.
    pub type_args: Vec<TypeId>,
    /// For each parameter in order, where its value comes from (for a call of a function
    /// value: the arguments in order).
    pub args: Vec<ArgSource>,
}

/// A literal that builds a value of another type than its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coercion {
    /// A string literal where a `ModelRef` is expected builds `Named { id }`.
    ModelName,
    /// A string literal where a `ToolName` is expected names a tool (or a pattern).
    ToolPattern,
    /// A record literal where a `Map<Str, V>` is expected builds a map.
    RecordToMap,
    /// A list or record literal where a `ToolSet` is expected builds a tool set.
    ToolSet,
}

/// What a pattern means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatRes {
    /// It binds a local (a bare name, `name @ p`, a field shorthand or a list rest).
    Bind(LocalId),
    /// It tests for a variant (a bare name of the scrutinee's enum, or a qualified one).
    Variant(EnumId, usize),
}

/// The tables of one module.
#[derive(Clone, Debug, Default)]
pub struct ModuleTable {
    pub types: HashMap<Key, TypeId>,
    pub res: HashMap<Key, Res>,
    pub calls: HashMap<Key, CallInfo>,
    pub coercions: HashMap<Key, Coercion>,
    pub patterns: HashMap<Key, PatRes>,
    pub locals: Vec<LocalInfo>,
    /// The type of each item, by its index: a function's or harness's type, a constant's, an
    /// alias's target, an enum's type with its parameters; `None` for imports, `state` and tests.
    pub items: Vec<Option<TypeId>>,
}

impl ModuleTable {
    /// The type of the expression at `span`.
    #[must_use]
    pub fn type_at(&self, span: Span) -> Option<TypeId> {
        self.types.get(&Key::from(span)).copied()
    }
}
