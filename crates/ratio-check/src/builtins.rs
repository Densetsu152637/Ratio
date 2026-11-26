//! The built-in names the checker knows: the primitive types, the shared types of chapter 06
//! (declared in Ratio, `std/prelude.ratio`), and a declarative table of built-in function
//! signatures, one Ratio source per standard module.
//!
//! The table is written in Ratio itself: each module is a list of `fn` declarations with empty
//! bodies, `@pure` on those whose Needs column is `pure`. A function without `@pure` counts as
//! making effects, so a signature added without the attribute fails closed (it is refused in
//! constants, and counts as an effect of an unknown kind). Two type-parameter names are bounds,
//! as in chapter 06: `S` stands for any text (`Str` or `Untrusted<Str>`) and `Num` for `Int`,
//! `Float`, `Duration` or `Money`. Default values are written for the reader only; the checker
//! uses only whether a parameter has one.
//!
//! Further attributes say what stage 5 needs to know ([`Traits`]): `@audited(kind)` marks the
//! three trust conversions, `@effect(kind)` and `@tool(name)` what a built-in does (for the
//! guarded-effect lint and `harnesses.check`'s summary), `@secrecy_sink` the built-ins no
//! statically secret value may reach, `@schema(T)` type parameters that must be schema-able,
//! `@user` built-ins whose result is the person's answer, `@secret_answers` `session.ask`,
//! `@loads` `meta.harness`, and `@local` effectful built-ins that keep their arguments on the
//! machine (not secrecy sinks; every `@local` entry is reviewed against the rule on
//! `Traits::local`). A name that is a keyword in Ratio (`harness`) is written with
//! a trailing `_` (`harness_`), which the checker drops, since a declaration cannot be called by
//! a keyword.
//!
//! The table holds what the conformance programs need. Later stages (7 to 12) extend it module
//! by module, and stage 7's `builtins.txt` check compares it with chapter 06.

use crate::analysis::Effects;
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
@audited(trust) @local fn trust<T>(x: Untrusted<T>, because: Str) -> T {}
@audited(as_task) @local fn as_task(x: Untrusted<Str>, because: Str) -> Brief {}
@audited(declassify) @user @local fn declassify<T>(x: Secret<T>, because: Str) -> T {}
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
@pure fn repeat_<S>(s: S, n: Int) -> S {}
@pure fn similarity<S>(a: S, b: S) -> Float {}
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
@pure @schema(T) fn decode<T>(j: Json) -> Untrusted<T> {}
@pure fn get(j: Json, pointer: Str) -> Json? {}
@pure @schema(T) fn encode<T>(v: T) -> Json {}
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
@effect(model) fn generate(p: Prompt, opts: GenOpts = {}) -> Untrusted<Str> {}
@effect(model) @schema(T) fn generate_as<T>(p: Prompt, opts: GenOpts = {}) -> Untrusted<T> {}
@effect(model) fn classify<E>(input: Untrusted<Str>, opts: GenOpts = {}) -> Classified<E> {}
@effect(model) fn complete(text: Str, opts: GenOpts = {}) -> Untrusted<Str> {}
@pure fn route(tier: Tier? = none, location: Location? = none, tags: [Str] = [], min_context: Int? = none) -> ModelRef {}
",
    ),
    (
        "tools",
        r#"
@tool(read_file) @local fn read_file(path: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> FileText {}
@effect(write) @tool(write_file) @secrecy_sink fn write_file(path: Untrusted<Str>, content: Untrusted<Str>) -> Written {}
@effect(write) @tool(edit_file) @secrecy_sink fn edit_file(path: Untrusted<Str>, old: Untrusted<Str>, new: Untrusted<Str>) -> Written {}
@effect(command) @tool(run_command) @secrecy_sink fn run_command(command: Untrusted<Str>, timeout: Duration = 120s, check: Bool = false) -> Untrusted<CommandResult> {}
@effect(command) @tool(run_command) @secrecy_sink fn run_background(command: Untrusted<Str>) -> Process {}
@effect(network) @tool(web_search) @secrecy_sink fn web_search(query: Untrusted<Str>, count: Int = 10) -> Untrusted<[SearchHit]> {}
@effect(network) @tool(web_fetch) @secrecy_sink fn web_fetch(url: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> Untrusted<Page> {}
@effect(network) @tool("mcp__*") @secrecy_sink fn mcp_call(server: Str, tool: Str, arguments: Json) -> Untrusted<McpResult> {}
@effect(any_tool) @secrecy_sink fn call_tool(name: Str, arguments: Json) -> Untrusted<ToolResult> {}
@effect(any_tool) fn dispatch(conv: Conversation, call: ToolCall) -> ToolResult {}
"#,
    ),
    (
        "session",
        r"
@user @secret_answers @local fn ask(questions: [Question]) -> [UserText] {}
@user @local fn request_approval(summary: Str, detail: Untrusted<Str>? = none) -> Bool {}
@secrecy_sink fn emit(name: Str, data: Json) -> () {}
",
    ),
    (
        "context",
        r"
fn notice(conv: Conversation, text: Str, data: Untrusted<Str>? = none) -> () {}
",
    ),
    (
        "meta",
        r"
@loads fn harness_<H>(name: Str) -> H {}
",
    ),
    (
        "agents",
        r"
@effect(delegate) @tool(spawn_agent) @secrecy_sink fn spawn(brief: Untrusted<Str>, package: Str? = none, agent: Str? = none, model: ModelRef? = none, tier: Tier? = none, location: Location? = none, tags: [Str] = [], effort: Effort? = none, access: Access = read, knowledge: Str? = none, title: Str? = none, single_call: Bool = false, budget: Budget? = none) -> Task<Report> {}
@effect(delegate) @tool(steer_agent) @secrecy_sink fn steer(task: Task<Report>, text: Untrusted<Str>) -> () {}
@effect(delegate) @tool(queue_agent) @secrecy_sink fn queue(task: Task<Report>, text: Untrusted<Str>) -> () {}
@effect(delegate) @tool(ask_agent) @secrecy_sink fn ask(agent: Str, question: Untrusted<Str>, timeout: Duration = 300s) -> Untrusted<Str> {}
",
    ),
];

/// The conversions chapter 03 audits ("Conversion"): each call is a trust site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrustKind {
    /// `trust(x, because:)`: untrusted to trusted.
    Trust,
    /// `as_task(x, because:)`: untrusted text to a `Brief`.
    AsTask,
    /// `declassify(x, because:)`: removes a secrecy label.
    Declassify,
}

impl TrustKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            TrustKind::Trust => "trust",
            TrustKind::AsTask => "as_task",
            TrustKind::Declassify => "declassify",
        }
    }
}

/// What a built-in's attributes in the table say about it, beyond its signature. User
/// declarations have the default (nothing).
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "independent attributes")]
pub(crate) struct Traits {
    /// `@audited(kind)`: a trust site.
    pub(crate) audited: Option<TrustKind>,
    /// `@user`: its result is the person's answer, not model or tool output (for the
    /// guarded-effect lint).
    pub(crate) user: bool,
    /// `@effect(kind)`: the kinds of effect it makes; a built-in without `@pure` or
    /// `@effect` makes effects of an unknown kind ([`Effects::OTHER`]).
    pub(crate) effects: Effects,
    /// `@tool(name)`: the tools it runs.
    pub(crate) tools: Vec<String>,
    /// `@secrecy_sink`: no argument may hold a statically secret value (chapter 03's secrecy
    /// sinks), whatever its parameter's type.
    pub(crate) secrecy_sink: bool,
    /// `@schema(T)`: these type parameters must be schema-able.
    pub(crate) schema: Vec<String>,
    /// `@secret_answers`: `session.ask`, whose answers are `Secret` unless every question is
    /// written out without `secret: true`.
    pub(crate) secret_answers: bool,
    /// `@loads`: `meta.harness`, which loads a harness by name.
    pub(crate) loads: bool,
    /// `@local`: its effects keep its arguments on the machine, so it is no secrecy sink.
    ///
    /// Review every use of `@local` in the table against this: a `@local` built-in must not
    /// hand any argument (or anything computed from one) to a model, the network, a command,
    /// another agent, or persisted or published state (files, memory, events, the journal
    /// beyond a digest). Marking such a built-in `@local` would let a statically secret value
    /// reach it through a generic function unrefused.
    pub(crate) local: bool,
}

impl Traits {
    /// The traits the attributes of a built-in declaration give; an attribute the table does
    /// not define (a bug in the table) is pushed to `internal`.
    pub(crate) fn of(attrs: &[ratio_syntax::ast::Attribute], internal: &mut Vec<String>) -> Self {
        let mut t = Traits::default();
        let mut pure = false;
        for a in attrs {
            let words: Vec<String> = a
                .args
                .iter()
                .flatten()
                .filter_map(|arg| word(&arg.value))
                .collect();
            match (a.name.name.as_str(), words.as_slice()) {
                ("pure", []) => pure = true,
                ("user", []) => t.user = true,
                ("secrecy_sink", []) => t.secrecy_sink = true,
                ("secret_answers", []) => t.secret_answers = true,
                ("loads", []) => t.loads = true,
                ("local", []) => t.local = true,
                ("audited", [kind]) => {
                    t.audited = match kind.as_str() {
                        "trust" => Some(TrustKind::Trust),
                        "as_task" => Some(TrustKind::AsTask),
                        "declassify" => Some(TrustKind::Declassify),
                        _ => {
                            internal.push(format!("unknown audited kind `{kind}`"));
                            None
                        }
                    };
                }
                ("effect", [kind]) => match Effects::named(kind) {
                    Some(e) => t.effects = t.effects.union(e),
                    None => internal.push(format!("unknown effect kind `{kind}`")),
                },
                ("tool", [name]) => t.tools.push(name.clone()),
                ("schema", names) if !names.is_empty() => t.schema.extend(names.iter().cloned()),
                (other, _) => internal.push(format!("unknown built-in attribute `@{other}`")),
            }
        }
        if !pure && t.effects.is_empty() {
            t.effects = Effects::OTHER;
        }
        if t.secrecy_sink || (!pure && !t.local) {
            t.effects = t.effects.union(Effects::SINK);
        }
        t
    }

    /// Why the built-in may not be used as a function value, if it may not: every rule its
    /// attributes impose must also hold for a value, and these are enforced only where it is
    /// called (the call's arguments, its literal name, its written questions, its type
    /// arguments at a generic secrecy sink). `None` when every rule also holds for a value:
    /// effects, tools and purity count where a value is named, `@user` only relaxes the lint,
    /// a secrecy sink without type parameters refuses secrets by its parameter types, and
    /// `@schema(T)` is checked where the value is named.
    pub(crate) fn value_refusal(&self, generic: bool) -> Option<&'static str> {
        if self.audited.is_some() {
            Some("each call is an audited trust site at one known type")
        } else if self.loads {
            Some("each load must be listed by the name it is called with")
        } else if self.secret_answers {
            Some("its answers are labelled from the questions written at the call")
        } else if self.secrecy_sink && generic {
            Some("it is a secrecy sink with type parameters, checked at each call")
        } else {
            None
        }
    }
}

/// An attribute argument as a word: a name, or a string literal without interpolations.
fn word(e: &ratio_syntax::ast::Expr) -> Option<String> {
    use ratio_syntax::ast::{ExprKind, Lit, StrPart};
    match &e.kind {
        ExprKind::Name(id) => Some(id.name.clone()),
        ExprKind::Lit(Lit::Str(s)) => {
            let mut out = String::new();
            for part in &s.parts {
                match part {
                    StrPart::Text(x) => out.push_str(x),
                    StrPart::Interp { .. } => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// A built-in's name as the table spells it, without the trailing `_` of a keyword.
pub(crate) fn builtin_name(name: &str) -> &str {
    name.strip_suffix('_').unwrap_or(name)
}

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
