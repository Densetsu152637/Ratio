//! The type table: every type the checker builds is interned here once, so a type is a small
//! copyable [`TypeId`], equal types have equal ids, and a type shared by many values (or built
//! from many aliases) is stored once.
//!
//! Interning normalises as chapter 03 defines the types: `T??` is `T?`, `()` is the empty
//! tuple, record fields are kept in name order, and `Untrusted<T>` is mapped by structure (a
//! record of untrusted fields, `Untrusted<Int>` is `Int`, …), so an `Untrusted<_>` node remains
//! only around the primitive untrusted text, an enum with payloads, and a type parameter.
//!
//! Hostile programs can build large types from small sources (an alias of a pair of an alias of
//! a pair …). Every node records its depth, and a type deeper than [`MAX_TYPE_DEPTH`] is refused
//! (the checker reports `C0022`); work on types (relations, substitution, display) is memoised
//! or bounded, never exponential in the size of the source.

use std::collections::HashMap;
use std::fmt::Write as _;

/// The deepest type the checker builds. Deeper types (from long alias chains or nested
/// generic instantiations) are refused, which also bounds generic instantiation depth.
pub const MAX_TYPE_DEPTH: u16 = 128;

/// An interned type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(u32);

/// An enum declaration (user or built-in), by its index in [`Types::enum_def`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(pub u32);

/// A type parameter of a generic function, alias or enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamId(pub u32);

/// An inference variable: an unknown type argument of one generic call while it is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarId(pub u32);

/// The engine-owned handles of chapter 03 (other than `Task<T>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Handle {
    Conversation,
    Process,
    Scratchpad,
    Mark,
    Pin,
    Artefact,
    Checkpoint,
}

impl Handle {
    pub const ALL: [Handle; 7] = [
        Handle::Conversation,
        Handle::Process,
        Handle::Scratchpad,
        Handle::Mark,
        Handle::Pin,
        Handle::Artefact,
        Handle::Checkpoint,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Handle::Conversation => "Conversation",
            Handle::Process => "Process",
            Handle::Scratchpad => "Scratchpad",
            Handle::Mark => "Mark",
            Handle::Pin => "Pin",
            Handle::Artefact => "Artefact",
            Handle::Checkpoint => "Checkpoint",
        }
    }
}

/// A bound on a type parameter of a built-in's signature (chapter 06's `S` and `Num`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bound {
    None,
    /// `S`: `Str` or `Untrusted<Str>` (any text, normalised to one of the two).
    Text,
    /// `Num`: `Int`, `Float`, `Duration` or `Money`.
    Num,
}

/// A field of a record type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Field {
    pub name: Box<str>,
    pub ty: TypeId,
    /// `name?: T`: the field may be absent, and reads as `T?`.
    pub optional: bool,
}

/// The index of the field called `name` in a record type's fields (which are in name order).
pub(crate) fn field_index(fields: &[Field], name: &str) -> Option<usize> {
    fields.binary_search_by(|f| (*f.name).cmp(name)).ok()
}

/// A type. Build them with the constructors of [`Types`], which normalise.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// The type of something already reported: compatible with everything, so one mistake
    /// gives one diagnostic.
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
    /// `Untrusted<Str>`.
    UntrustedStr,
    Json,
    Image,
    Prompt,
    TrustedPrompt,
    Transcript,
    /// The value of `a..b` and `a..=b`.
    Range,
    ToolName,
    ToolSet,
    Handle(Handle),
    Task(TypeId),
    Option(TypeId),
    List(TypeId),
    Map(TypeId, TypeId),
    Tuple(Box<[TypeId]>),
    /// Fields in name order.
    Record(Box<[Field]>),
    Enum(EnumId, Box<[TypeId]>),
    /// Only around an enum with payloads, a type parameter or an inference variable.
    Untrusted(TypeId),
    Secret(TypeId),
    Fn(Box<[TypeId]>, TypeId),
    Harness(Box<[TypeId]>, TypeId),
    Param(ParamId),
    Var(VarId),
}

/// A variant of an enum.
#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    /// The payload's record type, in terms of the enum's own parameters; `None` for a unit
    /// variant, and also until the declaration's payloads are lowered.
    pub payload: Option<TypeId>,
    /// Whether the variant was declared with a payload.
    pub has_payload: bool,
}

/// An enum declaration.
#[derive(Clone, Debug)]
pub struct EnumDef {
    pub name: String,
    pub params: Vec<ParamId>,
    pub variants: Vec<VariantDef>,
    by_name: HashMap<String, usize>,
    unit_only: bool,
}

impl EnumDef {
    /// An enum with these (distinct) variants.
    pub(crate) fn new(name: String, params: Vec<ParamId>, variants: Vec<VariantDef>) -> Self {
        let by_name = variants
            .iter()
            .enumerate()
            .map(|(i, v)| (v.name.clone(), i))
            .collect();
        let unit_only = variants.iter().all(|v| !v.has_payload);
        EnumDef {
            name,
            params,
            variants,
            by_name,
            unit_only,
        }
    }

    /// Whether no variant has a payload (option-like enums: `Effort`, `Tier`).
    #[must_use]
    pub fn unit_only(&self) -> bool {
        self.unit_only
    }

    /// The index of the variant called `name`.
    #[must_use]
    pub fn variant(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
}

/// A type parameter's name and bound.
#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub bound: Bound,
}

const HAS_VAR: u8 = 1;
const HAS_PARAM: u8 = 2;
const TAINTED: u8 = 4;

#[derive(Clone, Debug)]
struct Node {
    ty: Ty,
    depth: u16,
    flags: u8,
}

/// What an inference variable is bound to.
#[derive(Clone, Debug)]
pub(crate) struct VarState {
    pub(crate) param: ParamId,
    pub(crate) binding: Option<TypeId>,
}

/// The type table.
#[derive(Clone, Debug)]
pub struct Types {
    nodes: Vec<Node>,
    ids: HashMap<Ty, TypeId>,
    enums: Vec<EnumDef>,
    params: Vec<ParamDef>,
    pub(crate) vars: Vec<VarState>,
    /// Old bindings of variables bound inside a quiet relation, so a failed attempt undoes them.
    pub(crate) trail: Vec<(VarId, Option<TypeId>)>,
    pub(crate) quiet: u32,
    untrusted_memo: HashMap<TypeId, TypeId>,
    /// Set when a constructor refused a type deeper than [`MAX_TYPE_DEPTH`]; the checker takes
    /// it and reports where.
    pub(crate) too_deep: bool,
    /// Units of work left for relations and substitutions; at zero, every relation holds and
    /// the checker reports `C0022` once.
    pub(crate) fuel: u64,
}

macro_rules! fixed {
    ($($name:ident = $index:literal => $ty:expr,)*) => {
        impl TypeId {
            $(pub const $name: TypeId = TypeId($index);)*
        }
        const FIXED: &[Ty] = &[$($ty,)*];
    };
}

fixed! {
    ERROR = 0 => Ty::Error,
    NEVER = 1 => Ty::Never,
    UNIT = 2 => Ty::Unit,
    BOOL = 3 => Ty::Bool,
    INT = 4 => Ty::Int,
    FLOAT = 5 => Ty::Float,
    DURATION = 6 => Ty::Duration,
    MONEY = 7 => Ty::Money,
    STR = 8 => Ty::Str,
    USER_TEXT = 9 => Ty::UserText,
    BRIEF = 10 => Ty::Brief,
    UNTRUSTED_STR = 11 => Ty::UntrustedStr,
    JSON = 12 => Ty::Json,
    IMAGE = 13 => Ty::Image,
    PROMPT = 14 => Ty::Prompt,
    TRUSTED_PROMPT = 15 => Ty::TrustedPrompt,
    TRANSCRIPT = 16 => Ty::Transcript,
    RANGE = 17 => Ty::Range,
    TOOL_NAME = 18 => Ty::ToolName,
    TOOL_SET = 19 => Ty::ToolSet,
}

/// The work budget of one check: relations, substitutions and normalisations each cost a unit
/// per node they visit.
pub(crate) const FUEL: u64 = 20_000_000;

impl Default for Types {
    fn default() -> Self {
        Self::new()
    }
}

impl Types {
    #[must_use]
    pub fn new() -> Self {
        let mut types = Types {
            nodes: Vec::new(),
            ids: HashMap::new(),
            enums: Vec::new(),
            params: Vec::new(),
            vars: Vec::new(),
            trail: Vec::new(),
            quiet: 0,
            untrusted_memo: HashMap::new(),
            too_deep: false,
            fuel: FUEL,
        };
        for ty in FIXED {
            types.intern(ty.clone());
        }
        types
    }

    /// What `id` is.
    #[must_use]
    pub fn get(&self, id: TypeId) -> &Ty {
        &self.nodes[id.0 as usize].ty
    }

    /// The nesting depth of `id` (a scalar is 1).
    #[must_use]
    pub fn depth(&self, id: TypeId) -> u16 {
        self.nodes[id.0 as usize].depth
    }

    pub(crate) fn has_vars(&self, id: TypeId) -> bool {
        self.nodes[id.0 as usize].flags & HAS_VAR != 0
    }

    /// Whether a value of type `id` may hold text that is not trusted: untrusted text, a
    /// `Brief`, `Json`, an `Image`, or a type parameter (which generic code treats as possibly
    /// untrusted, chapter 03). Interpolating such a value makes a string untrusted.
    #[must_use]
    pub fn tainted(&self, id: TypeId) -> bool {
        self.nodes[id.0 as usize].flags & TAINTED != 0
    }

    /// The enum `id` names.
    #[must_use]
    pub fn enum_def(&self, id: EnumId) -> &EnumDef {
        &self.enums[id.0 as usize]
    }

    pub(crate) fn enum_def_mut(&mut self, id: EnumId) -> &mut EnumDef {
        &mut self.enums[id.0 as usize]
    }

    pub(crate) fn add_enum(&mut self, def: EnumDef) -> EnumId {
        let id = EnumId(u32::try_from(self.enums.len()).unwrap_or(u32::MAX));
        self.enums.push(def);
        id
    }

    /// The name and bound of a type parameter.
    #[must_use]
    pub fn param_def(&self, id: ParamId) -> &ParamDef {
        &self.params[id.0 as usize]
    }

    pub(crate) fn add_param(&mut self, name: &str, bound: Bound) -> ParamId {
        let id = ParamId(u32::try_from(self.params.len()).unwrap_or(u32::MAX));
        self.params.push(ParamDef {
            name: name.to_owned(),
            bound,
        });
        id
    }

    pub(crate) fn new_var(&mut self, param: ParamId) -> TypeId {
        let id = VarId(u32::try_from(self.vars.len()).unwrap_or(u32::MAX));
        self.vars.push(VarState {
            param,
            binding: None,
        });
        self.intern(Ty::Var(id))
    }

    /// Spends one unit of work; `false` once the budget is gone.
    pub(crate) fn burn(&mut self) -> bool {
        if self.fuel == 0 {
            return false;
        }
        self.fuel -= 1;
        true
    }

    fn intern(&mut self, ty: Ty) -> TypeId {
        if let Some(&id) = self.ids.get(&ty) {
            return id;
        }
        let (mut depth, mut flags) = (0u16, 0u8);
        let mut child = |id: TypeId| {
            let node = &self.nodes[id.0 as usize];
            depth = depth.max(node.depth);
            flags |= node.flags;
        };
        match &ty {
            Ty::Task(t) | Ty::Option(t) | Ty::List(t) | Ty::Secret(t) => child(*t),
            Ty::Untrusted(t) => {
                child(*t);
                flags |= TAINTED;
            }
            Ty::Map(k, v) => {
                child(*k);
                child(*v);
            }
            Ty::Tuple(ts) | Ty::Enum(_, ts) => ts.iter().for_each(|t| child(*t)),
            Ty::Record(fields) => fields.iter().for_each(|f| child(f.ty)),
            Ty::Fn(ps, r) | Ty::Harness(ps, r) => {
                ps.iter().for_each(|t| child(*t));
                child(*r);
            }
            Ty::Param(_) => flags |= HAS_PARAM | TAINTED,
            Ty::Var(_) => flags |= HAS_VAR,
            Ty::UntrustedStr | Ty::Brief | Ty::Json | Ty::Image => flags |= TAINTED,
            _ => {}
        }
        let depth = depth + 1;
        if depth > MAX_TYPE_DEPTH {
            self.too_deep = true;
            return TypeId::ERROR;
        }
        let id = TypeId(u32::try_from(self.nodes.len()).unwrap_or(u32::MAX));
        self.nodes.push(Node {
            ty: ty.clone(),
            depth,
            flags,
        });
        self.ids.insert(ty, id);
        id
    }

    // ---- constructors ----

    pub fn handle(&mut self, handle: Handle) -> TypeId {
        self.intern(Ty::Handle(handle))
    }

    pub fn param(&mut self, param: ParamId) -> TypeId {
        self.intern(Ty::Param(param))
    }

    /// `T?`; `T??` is `T?`.
    pub fn option(&mut self, t: TypeId) -> TypeId {
        match self.get(t) {
            Ty::Option(_) | Ty::Error => t,
            _ => self.intern(Ty::Option(t)),
        }
    }

    pub fn list(&mut self, t: TypeId) -> TypeId {
        self.wrap(t, Ty::List)
    }

    pub fn task(&mut self, t: TypeId) -> TypeId {
        self.wrap(t, Ty::Task)
    }

    pub fn secret(&mut self, t: TypeId) -> TypeId {
        self.wrap(t, Ty::Secret)
    }

    fn wrap(&mut self, t: TypeId, make: fn(TypeId) -> Ty) -> TypeId {
        if t == TypeId::ERROR {
            return t;
        }
        self.intern(make(t))
    }

    pub fn map(&mut self, k: TypeId, v: TypeId) -> TypeId {
        if k == TypeId::ERROR || v == TypeId::ERROR {
            return TypeId::ERROR;
        }
        self.intern(Ty::Map(k, v))
    }

    /// A tuple; the empty tuple is `()`.
    pub fn tuple(&mut self, ts: Vec<TypeId>) -> TypeId {
        if ts.is_empty() {
            return TypeId::UNIT;
        }
        if ts.contains(&TypeId::ERROR) {
            return TypeId::ERROR;
        }
        self.intern(Ty::Tuple(ts.into()))
    }

    /// A record; the fields are sorted by name (names must be distinct).
    pub fn record(&mut self, mut fields: Vec<Field>) -> TypeId {
        if fields.iter().any(|f| f.ty == TypeId::ERROR) {
            return TypeId::ERROR;
        }
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        self.intern(Ty::Record(fields.into()))
    }

    pub fn enum_type(&mut self, id: EnumId, args: Vec<TypeId>) -> TypeId {
        if args.contains(&TypeId::ERROR) {
            return TypeId::ERROR;
        }
        self.intern(Ty::Enum(id, args.into()))
    }

    pub fn function(&mut self, params: Vec<TypeId>, ret: TypeId) -> TypeId {
        if params.contains(&TypeId::ERROR) || ret == TypeId::ERROR {
            return TypeId::ERROR;
        }
        self.intern(Ty::Fn(params.into(), ret))
    }

    pub fn harness(&mut self, params: Vec<TypeId>, ret: TypeId) -> TypeId {
        if params.contains(&TypeId::ERROR) || ret == TypeId::ERROR {
            return TypeId::ERROR;
        }
        self.intern(Ty::Harness(params.into(), ret))
    }

    /// `Untrusted<T>`, mapped by structure (chapter 03, "`Untrusted<T>` is deep").
    pub fn untrusted(&mut self, t: TypeId) -> TypeId {
        if let Some(&done) = self.untrusted_memo.get(&t) {
            return done;
        }
        if !self.burn() {
            return TypeId::ERROR;
        }
        let mapped = match self.get(t).clone() {
            Ty::Str | Ty::UserText | Ty::Brief | Ty::UntrustedStr => TypeId::UNTRUSTED_STR,
            Ty::TrustedPrompt => TypeId::PROMPT,
            Ty::List(a) => {
                let a = self.untrusted(a);
                self.list(a)
            }
            Ty::Option(a) => {
                let a = self.untrusted(a);
                self.option(a)
            }
            Ty::Secret(a) => {
                let a = self.untrusted(a);
                self.secret(a)
            }
            Ty::Map(k, v) => {
                let (k, v) = (self.untrusted(k), self.untrusted(v));
                self.map(k, v)
            }
            Ty::Tuple(ts) => {
                let ts = ts.iter().map(|t| self.untrusted(*t)).collect();
                self.tuple(ts)
            }
            Ty::Record(fields) => {
                let fields = fields
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: self.untrusted(f.ty),
                        optional: f.optional,
                    })
                    .collect();
                self.record(fields)
            }
            Ty::Enum(id, _) if self.enum_def(id).unit_only() => t,
            Ty::Enum(..) | Ty::Param(_) | Ty::Var(_) => self.intern(Ty::Untrusted(t)),
            _ => t,
        };
        self.untrusted_memo.insert(t, mapped);
        mapped
    }

    /// `t` with every node rebuilt by `leaf` where it answers, through the constructors (so the
    /// result is normalised); `memo` keeps shared subtypes from being visited twice.
    pub(crate) fn rebuild(
        &mut self,
        t: TypeId,
        leaf: &mut dyn FnMut(&mut Types, &Ty) -> Option<TypeId>,
        memo: &mut HashMap<TypeId, TypeId>,
    ) -> TypeId {
        if self.nodes[t.0 as usize].flags & (HAS_VAR | HAS_PARAM) == 0 {
            return t;
        }
        if let Some(&done) = memo.get(&t) {
            return done;
        }
        if !self.burn() {
            return TypeId::ERROR;
        }
        let ty = self.get(t).clone();
        if let Some(done) = leaf(self, &ty) {
            memo.insert(t, done);
            return done;
        }
        let mut go = |types: &mut Types, t: TypeId| types.rebuild(t, leaf, memo);
        let done = match ty {
            Ty::Task(a) => {
                let a = go(self, a);
                self.task(a)
            }
            Ty::Option(a) => {
                let a = go(self, a);
                self.option(a)
            }
            Ty::List(a) => {
                let a = go(self, a);
                self.list(a)
            }
            Ty::Secret(a) => {
                let a = go(self, a);
                self.secret(a)
            }
            Ty::Untrusted(a) => {
                let a = go(self, a);
                self.untrusted(a)
            }
            Ty::Map(k, v) => {
                let (k, v) = (go(self, k), go(self, v));
                self.map(k, v)
            }
            Ty::Tuple(ts) => {
                let ts = ts.iter().map(|t| go(self, *t)).collect();
                self.tuple(ts)
            }
            Ty::Record(fields) => {
                let fields = fields
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: go(self, f.ty),
                        optional: f.optional,
                    })
                    .collect();
                self.record(fields)
            }
            Ty::Enum(id, args) => {
                let args = args.iter().map(|t| go(self, *t)).collect();
                self.enum_type(id, args)
            }
            Ty::Fn(ps, r) => {
                let ps = ps.iter().map(|t| go(self, *t)).collect();
                let r = go(self, r);
                self.function(ps, r)
            }
            Ty::Harness(ps, r) => {
                let ps = ps.iter().map(|t| go(self, *t)).collect();
                let r = go(self, r);
                self.harness(ps, r)
            }
            _ => t,
        };
        memo.insert(t, done);
        done
    }

    /// `t` with the type parameters in `args` replaced.
    pub fn substitute(&mut self, t: TypeId, args: &[(ParamId, TypeId)]) -> TypeId {
        if args.is_empty() {
            return t;
        }
        let mut leaf = |_: &mut Types, ty: &Ty| match ty {
            Ty::Param(p) => args.iter().find(|(q, _)| q == p).map(|(_, a)| *a),
            _ => None,
        };
        self.rebuild(t, &mut leaf, &mut HashMap::new())
    }

    /// `t` with every bound inference variable replaced by its binding.
    pub(crate) fn resolve(&mut self, t: TypeId) -> TypeId {
        if !self.has_vars(t) {
            return t;
        }
        let mut leaf = |types: &mut Types, ty: &Ty| match ty {
            Ty::Var(v) => types.vars[v.0 as usize].binding.map(|b| types.resolve(b)),
            _ => None,
        };
        self.rebuild(t, &mut leaf, &mut HashMap::new())
    }

    /// The payload record of variant `index` of the enum type `t` (an `Enum` or an
    /// `Untrusted` enum), with the enum's arguments substituted.
    pub fn payload(&mut self, t: TypeId, index: usize) -> Option<TypeId> {
        let (id, args, untrusted) = match self.get(t) {
            Ty::Enum(id, args) => (*id, args.clone(), false),
            Ty::Untrusted(inner) => match self.get(*inner) {
                Ty::Enum(id, args) => (*id, args.clone(), true),
                _ => return None,
            },
            _ => return None,
        };
        let def = self.enum_def(id);
        let payload = def.variants.get(index)?.payload?;
        let pairs: Vec<(ParamId, TypeId)> = def
            .params
            .iter()
            .copied()
            .zip(args.iter().copied())
            .collect();
        let payload = self.substitute(payload, &pairs);
        Some(if untrusted {
            self.untrusted(payload)
        } else {
            payload
        })
    }

    /// The enum behind `t` (looking through `Untrusted`), if it is one.
    #[must_use]
    pub fn as_enum(&self, t: TypeId) -> Option<EnumId> {
        match self.get(t) {
            Ty::Enum(id, _) => Some(*id),
            Ty::Untrusted(inner) => match self.get(*inner) {
                Ty::Enum(id, _) => Some(*id),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether `t` is one of the text types (`Str`, `UserText`, `Brief`, `Untrusted<Str>`).
    #[must_use]
    pub fn is_text(&self, t: TypeId) -> bool {
        matches!(
            self.get(t),
            Ty::Str | Ty::UserText | Ty::Brief | Ty::UntrustedStr
        )
    }

    /// Whether `t` may be a map key (chapter 03: `Str`, `Untrusted<Str>`, `Int`, `Bool` or a
    /// unit-only enum; the other text types are `Str`s).
    #[must_use]
    pub fn is_map_key(&self, t: TypeId) -> bool {
        match self.get(t) {
            Ty::Error
            | Ty::Str
            | Ty::UserText
            | Ty::Brief
            | Ty::UntrustedStr
            | Ty::Int
            | Ty::Bool
            | Ty::Param(_)
            | Ty::Var(_) => true,
            Ty::Enum(id, _) => self.enum_def(*id).unit_only(),
            _ => false,
        }
    }

    /// `t` as written in messages, cut to about 120 characters.
    #[must_use]
    pub fn display(&self, t: TypeId) -> String {
        let mut out = String::new();
        self.write(t, &mut out);
        if out.len() > 120 {
            let mut cut = 117;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push('…');
        }
        out
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of type")]
    fn write(&self, t: TypeId, out: &mut String) {
        if out.len() > 120 {
            return;
        }
        let list = |types: &Types, ts: &[TypeId], out: &mut String| {
            for (i, t) in ts.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                types.write(*t, out);
            }
        };
        match self.get(t) {
            Ty::Error => out.push_str("{error}"),
            Ty::Never => out.push_str("Never"),
            Ty::Unit => out.push_str("()"),
            Ty::Bool => out.push_str("Bool"),
            Ty::Int => out.push_str("Int"),
            Ty::Float => out.push_str("Float"),
            Ty::Duration => out.push_str("Duration"),
            Ty::Money => out.push_str("Money"),
            Ty::Str => out.push_str("Str"),
            Ty::UserText => out.push_str("UserText"),
            Ty::Brief => out.push_str("Brief"),
            Ty::UntrustedStr => out.push_str("Untrusted<Str>"),
            Ty::Json => out.push_str("Json"),
            Ty::Image => out.push_str("Image"),
            Ty::Prompt => out.push_str("Prompt"),
            Ty::TrustedPrompt => out.push_str("TrustedPrompt"),
            Ty::Transcript => out.push_str("Transcript"),
            Ty::Range => out.push_str("Range"),
            Ty::ToolName => out.push_str("ToolName"),
            Ty::ToolSet => out.push_str("ToolSet"),
            Ty::Handle(h) => out.push_str(h.name()),
            Ty::Task(a) => {
                out.push_str("Task<");
                self.write(*a, out);
                out.push('>');
            }
            Ty::Option(a) => {
                let fn_like = matches!(self.get(*a), Ty::Fn(..) | Ty::Harness(..));
                if fn_like {
                    out.push('(');
                }
                self.write(*a, out);
                out.push_str(if fn_like { ")?" } else { "?" });
            }
            Ty::List(a) => {
                out.push('[');
                self.write(*a, out);
                out.push(']');
            }
            Ty::Map(k, v) => {
                out.push_str("Map<");
                self.write(*k, out);
                out.push_str(", ");
                self.write(*v, out);
                out.push('>');
            }
            Ty::Tuple(ts) => {
                out.push('(');
                list(self, ts, out);
                out.push(')');
            }
            Ty::Record(fields) => {
                out.push_str("{ ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{}{}: ", f.name, if f.optional { "?" } else { "" });
                    self.write(f.ty, out);
                }
                out.push_str(" }");
            }
            Ty::Enum(id, args) => {
                out.push_str(&self.enum_def(*id).name);
                if !args.is_empty() {
                    out.push('<');
                    list(self, args, out);
                    out.push('>');
                }
            }
            Ty::Untrusted(a) => {
                out.push_str("Untrusted<");
                self.write(*a, out);
                out.push('>');
            }
            Ty::Secret(a) => {
                out.push_str("Secret<");
                self.write(*a, out);
                out.push('>');
            }
            Ty::Fn(ps, r) | Ty::Harness(ps, r) => {
                out.push_str(if matches!(self.get(t), Ty::Fn(..)) {
                    "fn("
                } else {
                    "harness("
                });
                list(self, ps, out);
                out.push_str(") -> ");
                self.write(*r, out);
            }
            Ty::Param(p) => out.push_str(&self.param_def(*p).name),
            Ty::Var(v) => match self.vars[v.0 as usize].binding {
                Some(b) => self.write(b, out),
                None => out.push_str(&self.param_def(self.vars[v.0 as usize].param).name),
            },
        }
    }
}
