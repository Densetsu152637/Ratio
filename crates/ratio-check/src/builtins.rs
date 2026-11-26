//! The built-in names the checker knows: the primitive types, the shared types of chapter 06
//! (declared in Ratio, `std/prelude.ratio`), and a declarative table of built-in function
//! signatures, one Ratio source per standard module.
//!
//! The table is written in Ratio itself: each module is a list of `fn` declarations with empty
//! bodies, `@pure` on those whose Needs column is `pure`. A function without `@pure` counts as
//! making effects, so a signature added without the attribute fails closed (it is refused in
//! constants). Two type-parameter names are bounds, as in chapter 06: `S` stands for any text
//! (`Str` or `Untrusted<Str>`) and `Num` for `Int`, `Float`, `Duration` or `Money`. Default
//! values are written for the reader only; the checker uses only whether a parameter has one.
//!
//! The table holds what the conformance programs need. Later stages (7 to 12) extend it module
//! by module, and stage 7's `builtins.txt` check compares it with chapter 06.

use crate::types::{Bound, Handle, TypeId};

/// The shared types, in scope in every file behind the file's own names.
pub(crate) const PRELUDE: &str = include_str!("std/prelude.ratio");

/// The standard modules the checker has signatures for: the module's name and its source.
/// `control` holds the functions in scope without a prefix.
pub(crate) const MODULES: &[(&str, &str)] = &[
    (
        "control",
        r#"
@pure fn fail(tag: Str, message: Untrusted<Str>, data: Json = null) -> Error {}
@pure fn result<T>(f: fn() -> T) -> Result<T> {}
@pure fn assert(cond: Bool, message: Str = "assertion failed") -> () {}
@pure fn assert_eq<T>(a: T, b: T, message: Str? = none) -> () {}
"#,
    ),
    (
        "list",
        r"
@pure fn len<T>(xs: [T]) -> Int {}
@pure fn get<T>(xs: [T], i: Int) -> T? {}
@pure fn first<T>(xs: [T]) -> T? {}
@pure fn push<T>(xs: [T], x: T) -> [T] {}
@pure fn map<T, U>(xs: [T], f: fn(T) -> U) -> [U] {}
@pure fn filter<T>(xs: [T], f: fn(T) -> Bool) -> [T] {}
@pure fn filter_map<T, U>(xs: [T], f: fn(T) -> U?) -> [U] {}
@pure fn fold<T, A>(xs: [T], init: A, f: fn(A, T) -> A) -> A {}
@pure fn contains<T>(xs: [T], x: T) -> Bool {}
@pure fn sort_by<T, K>(xs: [T], key: fn(T) -> K, descending: Bool = false) -> [T] {}
@pure fn take<T>(xs: [T], n: Int) -> [T] {}
@pure fn sum<Num>(xs: [Num]) -> Num {}
",
    ),
    (
        "text",
        r#"
@pure fn len<S>(s: S) -> Int {}
@pure fn upper<S>(s: S) -> S {}
@pure fn trim<S>(s: S) -> S {}
@pure fn contains<S>(s: S, part: S) -> Bool {}
@pure fn split_once<S>(s: S, sep: S) -> (S, S)? {}
@pure fn join<S>(parts: [S], sep: S = "") -> S {}
@pure fn show<T>(v: T) -> Untrusted<Str> {}
"#,
    ),
    (
        "map",
        r"
@pure fn get<K, V>(m: Map<K, V>, k: K) -> V? {}
@pure fn set<K, V>(m: Map<K, V>, k: K, v: V) -> Map<K, V> {}
@pure fn keys<K, V>(m: Map<K, V>) -> [K] {}
",
    ),
    (
        "json",
        r"
@pure fn parse<S>(s: S) -> Json {}
@pure fn decode<T>(j: Json) -> Untrusted<T> {}
@pure fn get(j: Json, pointer: Str) -> Json? {}
",
    ),
    (
        "math",
        r"
@pure fn abs<Num>(x: Num) -> Num {}
@pure fn min<Num>(a: Num, b: Num) -> Num {}
@pure fn max<Num>(a: Num, b: Num) -> Num {}
",
    ),
    (
        "model",
        r"
fn generate(p: Prompt, opts: GenOpts = {}) -> Untrusted<Str> {}
fn generate_as<T>(p: Prompt, opts: GenOpts = {}) -> Untrusted<T> {}
fn classify<E>(input: Untrusted<Str>, opts: GenOpts = {}) -> Classified<E> {}
@pure fn route(tier: Tier? = none, location: Location? = none, tags: [Str] = [], min_context: Int? = none) -> ModelRef {}
",
    ),
    (
        "tools",
        r"
fn read_file(path: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> FileText {}
",
    ),
];

/// The module a method call on a value of a type looks in first (chapter 03, "Modules and
/// imports": `x.f(a)` resolves to the module of `x`'s type first).
pub(crate) fn method_module(types: &crate::types::Types, t: TypeId) -> Option<&'static str> {
    use crate::types::Ty;
    match types.get(t) {
        Ty::Str | Ty::UserText | Ty::Brief | Ty::UntrustedStr => Some("text"),
        Ty::List(_) => Some("list"),
        Ty::Map(..) => Some("map"),
        Ty::Json => Some("json"),
        Ty::Int | Ty::Float | Ty::Duration | Ty::Money => Some("math"),
        _ => None,
    }
}

/// The bound a built-in's type parameter has by its name.
pub(crate) fn bound(name: &str) -> Bound {
    match name {
        "S" => Bound::Text,
        "Num" => Bound::Num,
        _ => Bound::None,
    }
}

/// A primitive type name: what it is and how many type arguments it takes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Primitive {
    Plain(TypeId),
    Handle(Handle),
    /// `Map<K, V>`.
    Map,
    /// `Untrusted<T>`.
    Untrusted,
    /// `Secret<T>`.
    Secret,
    /// `Task<T>`, the handle (the shared record is `TaskInput`).
    Task,
}

impl Primitive {
    pub(crate) fn arity(self) -> usize {
        match self {
            Primitive::Plain(_) | Primitive::Handle(_) => 0,
            Primitive::Untrusted | Primitive::Secret | Primitive::Task => 1,
            Primitive::Map => 2,
        }
    }
}

/// The primitive type called `name`.
pub(crate) fn primitive(name: &str) -> Option<Primitive> {
    let plain = match name {
        "Never" => TypeId::NEVER,
        "Bool" => TypeId::BOOL,
        "Int" => TypeId::INT,
        "Float" => TypeId::FLOAT,
        "Duration" => TypeId::DURATION,
        "Money" => TypeId::MONEY,
        "Str" => TypeId::STR,
        "UserText" => TypeId::USER_TEXT,
        "Brief" => TypeId::BRIEF,
        "Json" => TypeId::JSON,
        "Image" => TypeId::IMAGE,
        "Prompt" => TypeId::PROMPT,
        "TrustedPrompt" => TypeId::TRUSTED_PROMPT,
        "Transcript" => TypeId::TRANSCRIPT,
        "ToolName" => TypeId::TOOL_NAME,
        "ToolSet" => TypeId::TOOL_SET,
        "Map" => return Some(Primitive::Map),
        "Untrusted" => return Some(Primitive::Untrusted),
        "Secret" => return Some(Primitive::Secret),
        "Task" => return Some(Primitive::Task),
        _ => {
            return Handle::ALL
                .iter()
                .find(|h| h.name() == name)
                .map(|h| Primitive::Handle(*h));
        }
    };
    Some(Primitive::Plain(plain))
}
