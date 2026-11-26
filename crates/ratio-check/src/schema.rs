//! Schemas and type digests (chapter 03, "Types are schemas" and "Type digests").
//!
//! A type built only from scalars, text, options, lists, tuples, records, maps, enums and
//! `Json` is **schema-able**. Its JSON Schema (draft 2020-12) is derived mechanically by
//! chapter 03's table, with the labels dropped (`Untrusted<T>` and `Secret<T>` have `T`'s
//! schema) and the field descriptions and refinements (`@len`, `@range`, `@pattern`,
//! `@example`) of the record's declaration added. The output is canonical JSON text: no
//! whitespace, keys in a fixed order (below), so equal types give equal bytes on every
//! platform.
//!
//! - Object keys come in this order: `$ref`, `type`, `const`, `enum`, `anyOf`, `oneOf`,
//!   `properties`, `required`, `additionalProperties`, `propertyNames`, `prefixItems`,
//!   `items`, `pattern`, then a field's `description`, `minItems`, `maxItems`, `minimum`,
//!   `maximum`, `pattern`, `examples`, and last, at the root only, `$defs`.
//! - `()` is `{"type":"null"}`. Record properties are in field-name order; a field declared
//!   `b?: B` or `b: B?` is not `required` and has `B`'s schema.
//! - A unit-only enum is a `string` with the variant names in `snake_case`; another enum is a
//!   `oneOf` of objects whose `kind` is the variant's name in `snake_case`, in declaration
//!   order. An enum that contains itself is written once under `$defs` and referred to by
//!   `$ref`, named by its name, or, when an earlier definition took that name, the first of
//!   `Name_2`, `Name_3`, … still free.
//! - `Map<K, V>` is an `object` with `additionalProperties: V`; a key type other than text
//!   adds `propertyNames` (`^-?[0-9]+$` for `Int`, the names for `Bool` and unit-only enums).
//!
//! **Type digests** are `sha256` over a canonical form that keeps the labels, so `Str` and
//! `Untrusted<Str>` digest differently, and leaves out descriptions and refinements: a digest
//! is a key for the *structure* of the values (state, caches), and a changed description or
//! `@len` does not make stored values another type. `@range` bounds are written as the
//! decimal literals of the source (`1.5`, `-3`), never reformatted. The form is `ratio-type-1;` followed by the type's form
//! and then, for each enum met (numbered in the order first met, depth first, left to right),
//! `;#n=` and its definition:
//!
//! ```text
//! bool  int  float  duration  money  str  user_text  brief  json  unit
//! untrusted(T)  secret(T)  opt(T)  list(T)  map(K,V)  tuple(A,B,…)
//! record("a":A,"b"?:B)       fields in name order, names as JSON strings
//! #n                         the n-th enum met (its arguments substituted)
//! #n= "Name"{"v1","v2":record(…),…}   variants in declaration order
//! ```
//!
//! Both are bounded: the work is linear in the type's shared structure, the output is at most
//! [`MAX_SCHEMA_BYTES`], and nesting deeper than [`MAX_SCHEMA_DEPTH`] is refused.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use ratio_syntax::ast::{self, ExprKind, Lit, StrPart, UnaryOp};

use crate::check::Checker;
use crate::diag::Code;
use crate::tables::Origin;
use crate::types::{FieldMeta, Ty, TypeId, Types};

/// The largest schema or canonical form written, in bytes.
pub const MAX_SCHEMA_BYTES: usize = 1 << 20;

/// The deepest nesting a schema or canonical form is written to.
pub const MAX_SCHEMA_DEPTH: usize = 256;

/// The work budget of one schema or digest query after the check.
const QUERY_FUEL: u64 = 2_000_000;

/// Why a type has no schema or digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// The type is not schema-able; the text says which part is not.
    NotSchemaable(String),
    /// The schema or form would be larger or deeper than the bounds.
    TooLarge,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::NotSchemaable(why) => f.write_str(why),
            SchemaError::TooLarge => f.write_str("its schema is too large or too deep to write"),
        }
    }
}

/// A type digest: `sha256` of the canonical form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// The digest as lower-case hexadecimal.
    #[must_use]
    pub fn hex(&self) -> String {
        crate::sha256::hex(&self.0)
    }
}

/// `name` in `snake_case`: an underscore before each upper-case letter that follows a
/// lower-case letter or digit, or that starts a word after an acronym (`HTTPError` is
/// `http_error`), and every letter lower case.
#[must_use]
pub fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Writes `s` as a JSON string.
pub(crate) fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Runs `f` with a fresh work budget, restoring the table's afterwards.
fn with_fuel<R>(types: &mut Types, f: impl FnOnce(&mut Types) -> R) -> R {
    let saved = types.fuel;
    types.fuel = QUERY_FUEL;
    let out = f(types);
    types.fuel = saved;
    out
}

/// What `t`'s payloads and parts are, for the walks below.
enum Shape {
    Leaf,
    Not(&'static str),
    Parts(Vec<TypeId>),
}

fn shape(types: &mut Types, t: TypeId) -> Shape {
    match types.get(t).clone() {
        Ty::Unit
        | Ty::Bool
        | Ty::Int
        | Ty::Float
        | Ty::Duration
        | Ty::Money
        | Ty::Str
        | Ty::UserText
        | Ty::Brief
        | Ty::UntrustedStr
        | Ty::Json => Shape::Leaf,
        Ty::Option(a) | Ty::List(a) | Ty::Secret(a) => Shape::Parts(vec![a]),
        Ty::Map(k, v) => {
            if !types.is_map_key(k) || matches!(types.get(k), Ty::Param(_) | Ty::Var(_)) {
                return Shape::Not("a map whose key type has no schema");
            }
            Shape::Parts(vec![k, v])
        }
        Ty::Tuple(ts) => Shape::Parts(ts.to_vec()),
        Ty::Record(fields) => Shape::Parts(fields.iter().map(|f| f.ty).collect()),
        Ty::Untrusted(inner) => match types.get(inner) {
            Ty::Enum(..) => enum_parts(types, t),
            _ => Shape::Not("a type parameter"),
        },
        Ty::Enum(..) => enum_parts(types, t),
        Ty::Error => Shape::Not("a type with an error"),
        Ty::Never => Shape::Not("`Never`, which has no values"),
        Ty::Image => Shape::Not("an image"),
        Ty::Prompt | Ty::TrustedPrompt => Shape::Not("a prompt"),
        Ty::Transcript => Shape::Not("a transcript"),
        Ty::Range => Shape::Not("a range"),
        Ty::ToolName | Ty::ToolSet => Shape::Not("a tool name or set"),
        Ty::Handle(_) | Ty::Task(_) => Shape::Not("a handle"),
        Ty::Fn(..) | Ty::Harness(..) => Shape::Not("a function or harness"),
        Ty::Param(_) | Ty::Var(_) => Shape::Not("a type parameter"),
    }
}

fn enum_parts(types: &mut Types, t: TypeId) -> Shape {
    let Some(id) = types.as_enum(t) else {
        return Shape::Not("an enum");
    };
    let def = types.enum_def(id);
    let mut names: HashSet<String> = HashSet::new();
    for v in &def.variants {
        if !names.insert(snake_case(&v.name)) {
            return Shape::Not("an enum two of whose variants have the same `snake_case` name");
        }
    }
    let count = def.variants.len();
    let mut parts = Vec::new();
    for i in 0..count {
        if let Some(p) = types.payload(t, i) {
            let kind_field = match types.get(p) {
                Ty::Record(fields) => fields.iter().any(|f| &*f.name == "kind"),
                _ => false,
            };
            if kind_field {
                return Shape::Not("an enum with a payload field named `kind`");
            }
            parts.push(p);
        }
    }
    Shape::Parts(parts)
}

/// Whether `t` is schema-able, or which part of it is not. Linear in the type's shared
/// structure; out of budget, not schema-able.
pub(crate) fn schema_able(types: &mut Types, t: TypeId) -> Result<(), String> {
    let mut seen: HashSet<TypeId> = HashSet::new();
    let mut work = vec![t];
    while let Some(x) = work.pop() {
        if !seen.insert(x) {
            continue;
        }
        if !types.burn() {
            return Err("it is too large to check".to_owned());
        }
        match shape(types, x) {
            Shape::Leaf => {}
            Shape::Not(why) => {
                let shown = types.display(x);
                return Err(if x == t {
                    format!("it is {why}")
                } else {
                    format!("it holds `{shown}`, {why}")
                });
            }
            Shape::Parts(parts) => work.extend(parts),
        }
    }
    Ok(())
}

/// A record's properties and the names it requires.
type Props = (Vec<(String, J)>, Vec<String>);

/// A JSON value being built, serialised once complete.
enum J {
    Obj(Vec<(&'static str, J)>),
    /// An object with arbitrary keys (`properties`, `$defs`).
    Map(Vec<(String, J)>),
    Arr(Vec<J>),
    Str(String),
    /// Raw JSON text (numbers, `true`, `false`, examples).
    Raw(String),
}

impl J {
    fn ty(name: &str) -> J {
        J::Obj(vec![("type", J::Str(name.to_owned()))])
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
            J::Map(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
            J::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            J::Str(s) => json_string(s, out),
            J::Raw(r) => out.push_str(r),
        }
    }
}

struct SchemaWriter<'t> {
    types: &'t mut Types,
    /// Enums that contain themselves, with their `$defs` name.
    defs: Vec<(TypeId, String)>,
    /// Bytes written so far, roughly, to bound the output.
    size: usize,
}

impl SchemaWriter<'_> {
    fn grow(&mut self, by: usize) -> Result<(), SchemaError> {
        self.size += by;
        if self.size > MAX_SCHEMA_BYTES || !self.types.burn() {
            return Err(SchemaError::TooLarge);
        }
        Ok(())
    }

    /// The parts of `t` a schema walks into.
    fn parts(&mut self, t: TypeId) -> Result<Vec<TypeId>, SchemaError> {
        self.grow(1)?;
        match shape(self.types, t) {
            Shape::Parts(p) => Ok(p),
            Shape::Leaf => Ok(Vec::new()),
            Shape::Not(why) => Err(SchemaError::NotSchemaable(why.to_owned())),
        }
    }

    /// Finds the enums that contain themselves: every enum reachable from `t`, in the order
    /// first reached (breadth first), is recursive when a walk from its parts reaches it
    /// again. Each walk is linear in the shared structure, and all of them spend the budget.
    fn find_recursive(&mut self, t: TypeId) -> Result<(), SchemaError> {
        let mut enums: Vec<TypeId> = Vec::new();
        let mut seen: HashSet<TypeId> = HashSet::new();
        let mut queue = std::collections::VecDeque::from([t]);
        while let Some(x) = queue.pop_front() {
            if !seen.insert(x) {
                continue;
            }
            if let Some(e) = self.enum_key(x).filter(|e| !enums.contains(e)) {
                enums.push(e);
            }
            queue.extend(self.parts(x)?);
        }
        for e in enums {
            let mut seen: HashSet<TypeId> = HashSet::new();
            let mut work = self.parts(e)?;
            let mut recursive = false;
            while let Some(x) = work.pop() {
                if self.enum_key(x) == Some(e) {
                    recursive = true;
                    break;
                }
                if seen.insert(x) {
                    work.extend(self.parts(x)?);
                }
            }
            if recursive {
                let base = self.types.display(e);
                let base: String = base
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                // The first of `Name`, `Name_2`, `Name_3`, … no earlier definition took.
                let used = |n: &str| self.defs.iter().any(|(_, d)| d == n);
                let mut name = base.clone();
                let mut k = 2usize;
                while used(&name) {
                    name = format!("{base}_{k}");
                    k += 1;
                }
                self.defs.push((e, name));
            }
        }
        Ok(())
    }

    /// The enum `t` is, without its trust label.
    fn enum_key(&self, t: TypeId) -> Option<TypeId> {
        match self.types.get(t) {
            Ty::Enum(..) => Some(t),
            Ty::Untrusted(inner) if matches!(self.types.get(*inner), Ty::Enum(..)) => Some(*inner),
            _ => None,
        }
    }

    fn schema(&mut self, t: TypeId, depth: usize) -> Result<J, SchemaError> {
        if depth > MAX_SCHEMA_DEPTH {
            return Err(SchemaError::TooLarge);
        }
        self.grow(16)?;
        let def = self
            .enum_key(t)
            .and_then(|e| self.defs.iter().find(|(d, _)| *d == e));
        if let Some((_, name)) = def {
            return Ok(J::Obj(vec![("$ref", J::Str(format!("#/$defs/{name}")))]));
        }
        self.body(t, depth)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of type")]
    fn body(&mut self, t: TypeId, depth: usize) -> Result<J, SchemaError> {
        let d = depth + 1;
        Ok(match self.types.get(t).clone() {
            Ty::Unit => J::ty("null"),
            Ty::Bool => J::ty("boolean"),
            Ty::Int => J::ty("integer"),
            Ty::Float => J::ty("number"),
            Ty::Str | Ty::UserText | Ty::Brief | Ty::UntrustedStr => J::ty("string"),
            Ty::Duration => J::Obj(vec![
                ("type", J::Str("string".to_owned())),
                ("pattern", J::Str("^[0-9]+(ms|s|m|h)$".to_owned())),
            ]),
            Ty::Money => J::Obj(vec![
                ("type", J::Str("string".to_owned())),
                ("pattern", J::Str("^\\$[0-9]+(\\.[0-9]{1,6})?$".to_owned())),
            ]),
            Ty::Json => J::Obj(Vec::new()),
            Ty::Untrusted(a) | Ty::Secret(a) if self.enum_key(t).is_none() => self.schema(a, d)?,
            Ty::Option(a) => J::Obj(vec![(
                "anyOf",
                J::Arr(vec![self.schema(a, d)?, J::ty("null")]),
            )]),
            Ty::List(a) => J::Obj(vec![
                ("type", J::Str("array".to_owned())),
                ("items", self.schema(a, d)?),
            ]),
            Ty::Tuple(ts) => {
                let mut items = Vec::with_capacity(ts.len());
                for x in &*ts {
                    items.push(self.schema(*x, d)?);
                }
                J::Obj(vec![
                    ("type", J::Str("array".to_owned())),
                    ("prefixItems", J::Arr(items)),
                    ("items", J::Raw("false".to_owned())),
                ])
            }
            Ty::Record(_) => {
                let (props, required) = self.properties(t, d)?;
                J::Obj(vec![
                    ("type", J::Str("object".to_owned())),
                    ("properties", J::Map(props)),
                    (
                        "required",
                        J::Arr(required.into_iter().map(J::Str).collect()),
                    ),
                    ("additionalProperties", J::Raw("false".to_owned())),
                ])
            }
            Ty::Map(k, v) => {
                let mut pairs = vec![
                    ("type", J::Str("object".to_owned())),
                    ("additionalProperties", self.schema(v, d)?),
                ];
                match self.types.get(k).clone() {
                    Ty::Int => pairs.push((
                        "propertyNames",
                        J::Obj(vec![("pattern", J::Str("^-?[0-9]+$".to_owned()))]),
                    )),
                    Ty::Bool => pairs.push((
                        "propertyNames",
                        J::Obj(vec![(
                            "enum",
                            J::Arr(vec![J::Str("false".to_owned()), J::Str("true".to_owned())]),
                        )]),
                    )),
                    Ty::Enum(id, _) => {
                        let names = self
                            .types
                            .enum_def(id)
                            .variants
                            .iter()
                            .map(|v| J::Str(snake_case(&v.name)))
                            .collect();
                        pairs.push(("propertyNames", J::Obj(vec![("enum", J::Arr(names))])));
                    }
                    _ => {}
                }
                J::Obj(pairs)
            }
            Ty::Enum(..) | Ty::Untrusted(_) => self.enum_body(t, d)?,
            _ => {
                let shown = self.types.display(t);
                return Err(SchemaError::NotSchemaable(format!(
                    "`{shown}` has no schema"
                )));
            }
        })
    }

    fn enum_body(&mut self, t: TypeId, depth: usize) -> Result<J, SchemaError> {
        let id = self
            .types
            .as_enum(t)
            .ok_or_else(|| SchemaError::NotSchemaable("not an enum".to_owned()))?;
        let def = self.types.enum_def(id).clone();
        if def.unit_only() {
            let names = def
                .variants
                .iter()
                .map(|v| J::Str(snake_case(&v.name)))
                .collect();
            return Ok(J::Obj(vec![
                ("type", J::Str("string".to_owned())),
                ("enum", J::Arr(names)),
            ]));
        }
        let mut cases = Vec::with_capacity(def.variants.len());
        for (i, v) in def.variants.iter().enumerate() {
            let kind = J::Obj(vec![("const", J::Str(snake_case(&v.name)))]);
            let mut props = vec![("kind".to_owned(), kind)];
            let mut required = vec!["kind".to_owned()];
            if let Some(p) = self.types.payload(t, i) {
                let (more, req) = self.properties(p, depth)?;
                props.extend(more);
                required.extend(req);
            }
            cases.push(J::Obj(vec![
                ("type", J::Str("object".to_owned())),
                ("properties", J::Map(props)),
                (
                    "required",
                    J::Arr(required.into_iter().map(J::Str).collect()),
                ),
                ("additionalProperties", J::Raw("false".to_owned())),
            ]));
        }
        Ok(J::Obj(vec![("oneOf", J::Arr(cases))]))
    }

    /// A record's properties and required names.
    fn properties(&mut self, t: TypeId, depth: usize) -> Result<Props, SchemaError> {
        let Ty::Record(fields) = self.types.get(t).clone() else {
            return Err(SchemaError::NotSchemaable("not a record".to_owned()));
        };
        let meta: Vec<FieldMeta> = self
            .types
            .field_meta(t)
            .map(<[FieldMeta]>::to_vec)
            .unwrap_or_default();
        let mut props = Vec::with_capacity(fields.len());
        let mut required = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            let (inner, absent) = match self.types.get(f.ty) {
                Ty::Option(inner) => (*inner, true),
                _ => (f.ty, f.optional),
            };
            if !absent && !f.optional {
                required.push(f.name.to_string());
            }
            let mut schema = self.schema(inner, depth)?;
            if let Some(m) = meta.get(i) {
                refine(&mut schema, m);
            }
            self.grow(f.name.len())?;
            props.push((f.name.to_string(), schema));
        }
        Ok((props, required))
    }
}

/// Adds a field's description and refinements to its schema.
fn refine(schema: &mut J, m: &FieldMeta) {
    let J::Obj(pairs) = schema else {
        return;
    };
    if let Some(d) = &m.description {
        pairs.push(("description", J::Str(d.clone())));
    }
    if let Some((min, max)) = m.len {
        pairs.push(("minItems", J::Raw(min.to_string())));
        pairs.push(("maxItems", J::Raw(max.to_string())));
    }
    if let Some((min, max)) = &m.range {
        pairs.push(("minimum", J::Raw(min.clone())));
        pairs.push(("maximum", J::Raw(max.clone())));
    }
    if let Some(p) = &m.pattern {
        pairs.retain(|(k, _)| *k != "pattern");
        pairs.push(("pattern", J::Str(p.clone())));
    }
    if let Some(e) = &m.example {
        pairs.push(("examples", J::Arr(vec![J::Raw(e.clone())])));
    }
}

/// The JSON Schema of `t` (see the module documentation for its canonical form).
///
/// # Errors
///
/// When `t` is not schema-able, or its schema would exceed the bounds.
pub fn schema(types: &mut Types, t: TypeId) -> Result<String, SchemaError> {
    with_fuel(types, |types| {
        schema_able(types, t).map_err(SchemaError::NotSchemaable)?;
        let mut w = SchemaWriter {
            types,
            defs: Vec::new(),
            size: 0,
        };
        w.find_recursive(t)?;
        let root = w.schema(t, 0)?;
        let mut defs = Vec::new();
        for (e, name) in w.defs.clone() {
            defs.push((name, w.body(e, 1)?));
        }
        let root = match root {
            J::Obj(mut pairs) if !defs.is_empty() => {
                pairs.push(("$defs", J::Map(defs)));
                J::Obj(pairs)
            }
            other => other,
        };
        let mut out = String::new();
        root.write(&mut out);
        if out.len() > MAX_SCHEMA_BYTES {
            return Err(SchemaError::TooLarge);
        }
        Ok(out)
    })
}

/// The canonical form of `t` that its digest is taken over (see the module documentation).
///
/// # Errors
///
/// When `t` is not schema-able, or its form would exceed the bounds.
pub fn canonical_form(types: &mut Types, t: TypeId) -> Result<String, SchemaError> {
    with_fuel(types, |types| {
        schema_able(types, t).map_err(SchemaError::NotSchemaable)?;
        let mut f = FormWriter {
            types,
            out: String::from("ratio-type-1;"),
            enums: Vec::new(),
            index: HashMap::new(),
        };
        f.form(t, 0)?;
        let mut next = 0;
        while next < f.enums.len() {
            let e = f.enums[next];
            let _ = write!(f.out, ";#{next}=");
            f.def(e)?;
            next += 1;
        }
        Ok(f.out)
    })
}

/// The type digest of `t`: `sha256` of its canonical form.
///
/// # Errors
///
/// As [`canonical_form`].
pub fn digest(types: &mut Types, t: TypeId) -> Result<Digest, SchemaError> {
    let form = canonical_form(types, t)?;
    Ok(Digest(crate::sha256::sha256(form.as_bytes())))
}

struct FormWriter<'t> {
    types: &'t mut Types,
    out: String,
    /// The enums met, in order.
    enums: Vec<TypeId>,
    index: HashMap<TypeId, usize>,
}

impl FormWriter<'_> {
    fn push(&mut self, s: &str) -> Result<(), SchemaError> {
        self.out.push_str(s);
        if self.out.len() > MAX_SCHEMA_BYTES || !self.types.burn() {
            return Err(SchemaError::TooLarge);
        }
        Ok(())
    }

    fn form(&mut self, t: TypeId, depth: usize) -> Result<(), SchemaError> {
        if depth > MAX_SCHEMA_DEPTH {
            return Err(SchemaError::TooLarge);
        }
        let d = depth + 1;
        match self.types.get(t).clone() {
            Ty::Unit => self.push("unit"),
            Ty::Bool => self.push("bool"),
            Ty::Int => self.push("int"),
            Ty::Float => self.push("float"),
            Ty::Duration => self.push("duration"),
            Ty::Money => self.push("money"),
            Ty::Str => self.push("str"),
            Ty::UserText => self.push("user_text"),
            Ty::Brief => self.push("brief"),
            Ty::UntrustedStr => self.push("untrusted(str)"),
            Ty::Json => self.push("json"),
            Ty::Untrusted(a) => self.wrap("untrusted(", a, d),
            Ty::Secret(a) => self.wrap("secret(", a, d),
            Ty::Option(a) => self.wrap("opt(", a, d),
            Ty::List(a) => self.wrap("list(", a, d),
            Ty::Map(k, v) => {
                self.push("map(")?;
                self.form(k, d)?;
                self.push(",")?;
                self.form(v, d)?;
                self.push(")")
            }
            Ty::Tuple(ts) => {
                self.push("tuple(")?;
                for (i, x) in ts.iter().enumerate() {
                    if i > 0 {
                        self.push(",")?;
                    }
                    self.form(*x, d)?;
                }
                self.push(")")
            }
            Ty::Record(fields) => {
                self.push("record(")?;
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.push(",")?;
                    }
                    let mut name = String::new();
                    json_string(&f.name, &mut name);
                    self.push(&name)?;
                    self.push(if f.optional { "?:" } else { ":" })?;
                    self.form(f.ty, d)?;
                }
                self.push(")")
            }
            Ty::Enum(..) => {
                let n = if let Some(n) = self.index.get(&t) {
                    *n
                } else {
                    let n = self.enums.len();
                    self.enums.push(t);
                    self.index.insert(t, n);
                    n
                };
                self.push(&format!("#{n}"))
            }
            _ => {
                let shown = self.types.display(t);
                Err(SchemaError::NotSchemaable(format!(
                    "`{shown}` has no digest"
                )))
            }
        }
    }

    fn wrap(&mut self, open: &str, inner: TypeId, depth: usize) -> Result<(), SchemaError> {
        self.push(open)?;
        self.form(inner, depth)?;
        self.push(")")
    }

    fn def(&mut self, e: TypeId) -> Result<(), SchemaError> {
        let Some(id) = self.types.as_enum(e) else {
            return Err(SchemaError::NotSchemaable("not an enum".to_owned()));
        };
        let def = self.types.enum_def(id).clone();
        let mut name = String::new();
        json_string(&def.name, &mut name);
        self.push(&name)?;
        self.push("{")?;
        for (i, v) in def.variants.iter().enumerate() {
            if i > 0 {
                self.push(",")?;
            }
            let mut vname = String::new();
            json_string(&v.name, &mut vname);
            self.push(&vname)?;
            if let Some(p) = self.types.payload(e, i) {
                self.push(":")?;
                self.form(p, 1)?;
            }
        }
        self.push("}")
    }
}

impl Checker<'_> {
    /// The schema metadata a record field's declaration gives: its description (the trailing
    /// string, else the `///` comment) and its refinements, each checked against the field's
    /// type (`C0038`).
    #[allow(clippy::too_many_lines, reason = "one arm per attribute")]
    pub(crate) fn field_meta(
        &mut self,
        origin: Origin,
        f: &ast::FieldType,
        ty: TypeId,
    ) -> FieldMeta {
        let mut meta = FieldMeta::default();
        if let Some(d) = &f.description {
            match plain(d) {
                Some(text) => meta.description = Some(text),
                None => self.report(
                    origin,
                    Code::NotSchemaable,
                    d.span,
                    "a field description is plain text, without interpolations".to_owned(),
                ),
            }
        } else if !f.docs.is_empty() {
            let lines: Vec<&str> = f
                .docs
                .iter()
                .map(|d| d.text.strip_prefix(' ').unwrap_or(&d.text).trim_end())
                .collect();
            meta.description = Some(lines.join("\n"));
        }
        let inner = match self.types.get(ty) {
            Ty::Option(inner) => *inner,
            _ => ty,
        };
        for a in &f.attrs {
            let args: Vec<&ast::Expr> = a.args.iter().flatten().map(|x| &x.value).collect();
            let fits = match a.name.name.as_str() {
                "len" => match (
                    matches!(self.types.get(inner), Ty::List(_)),
                    args.as_slice(),
                ) {
                    (true, [lo, hi]) => match (int_value(lo), int_value(hi)) {
                        (Some(lo), Some(hi)) if 0 <= lo && lo <= hi => {
                            meta.len = u64::try_from(lo).ok().zip(u64::try_from(hi).ok());
                            meta.len.is_some()
                        }
                        _ => false,
                    },
                    _ => false,
                },
                "range" => match (
                    matches!(self.types.get(inner), Ty::Int | Ty::Float),
                    args.as_slice(),
                ) {
                    (true, [lo, hi]) => match (number_value(lo), number_value(hi)) {
                        (Some(lo), Some(hi)) => {
                            meta.range = Some((lo, hi));
                            true
                        }
                        _ => false,
                    },
                    _ => false,
                },
                "pattern" => match (self.types.is_text(inner), args.as_slice()) {
                    (
                        true,
                        [
                            ast::Expr {
                                kind: ExprKind::Lit(Lit::Str(s)),
                                ..
                            },
                        ],
                    ) => match plain(s) {
                        Some(p) => match regex_problem(&p) {
                            None => {
                                meta.pattern = Some(p);
                                true
                            }
                            Some(why) => {
                                self.report(
                                    origin,
                                    Code::NotSchemaable,
                                    a.span,
                                    format!("`@pattern` is not a valid regular expression: {why}"),
                                );
                                continue;
                            }
                        },
                        None => false,
                    },
                    _ => false,
                },
                "example" => match args.as_slice() {
                    [value] => {
                        meta.example = example(value, 0);
                        meta.example.is_some()
                    }
                    _ => false,
                },
                other => {
                    self.report(
                        origin,
                        Code::NotSchemaable,
                        a.span,
                        format!(
                            "`@{other}` is not a schema attribute (a field takes `@len`, \
                             `@range`, `@pattern` and `@example`)"
                        ),
                    );
                    continue;
                }
            };
            if !fits {
                let shown = self.types.display(ty);
                self.report(
                    origin,
                    Code::NotSchemaable,
                    a.span,
                    format!(
                        "`@{}` does not fit the field `{}: {shown}`: `@len(min, max)` takes two \
                         whole numbers on a list, `@range(min, max)` two numbers on a number, \
                         `@pattern(r\"…\")` a string on text, and `@example(value)` a literal",
                        a.name.name, f.name.name
                    ),
                );
            }
        }
        meta
    }
}

/// Why `p` is not a regular expression a schema validator and the standard library's `re`
/// both read, if it is not: the conservative common subset of ECMAScript and RE2 syntax
/// (alternation, groups including `(?:`, classes with ranges, the quantifiers `*`, `+`, `?`
/// and `{m}`, `{m,}`, `{m,n}`, anchors and `\`-escapes of punctuation and the classes `d`,
/// `w`, `s`, `D`, `W`, `S`, `b`, `B`, `n`, `r`, `t`). Anything else (look-around,
/// back-references, named groups, an unbalanced bracket, a quantifier with nothing to repeat)
/// is refused rather than guessed.
#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "one arm per token; the scanner's indexes"
)]
pub fn regex_problem(p: &str) -> Option<String> {
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    let mut depth = 0usize;
    // Whether the last item can take a quantifier.
    let mut atom = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&e) = chars.get(i + 1) else {
                    return Some("it ends with `\\`".to_owned());
                };
                if !(e.is_ascii_punctuation() || "dwsDWSbBnrt".contains(e)) {
                    return Some(format!("`\\{e}` is not an escape both dialects read"));
                }
                atom = !matches!(e, 'b' | 'B');
                i += 2;
            }
            '(' => {
                if chars.get(i + 1) == Some(&'?') {
                    if chars.get(i + 2) != Some(&':') {
                        return Some("only `(?:` groups are allowed".to_owned());
                    }
                    i += 3;
                } else {
                    i += 1;
                }
                depth += 1;
                if depth > 64 {
                    return Some("groups are nested too deeply".to_owned());
                }
                atom = false;
            }
            ')' => {
                if depth == 0 {
                    return Some("a `)` closes no group".to_owned());
                }
                depth -= 1;
                atom = true;
                i += 1;
            }
            '[' => {
                let mut j = i + 1;
                if chars.get(j) == Some(&'^') {
                    j += 1;
                }
                let start = j;
                loop {
                    match chars.get(j) {
                        None => return Some("a `[` is not closed".to_owned()),
                        Some(']') if j > start => break,
                        Some('\\') => {
                            let Some(&e) = chars.get(j + 1) else {
                                return Some("it ends with `\\`".to_owned());
                            };
                            if !(e.is_ascii_punctuation() || "dwsDWSnrt".contains(e)) {
                                return Some(format!(
                                    "`\\{e}` is not an escape both dialects read"
                                ));
                            }
                            j += 2;
                        }
                        Some('[') => {
                            return Some("a `[` inside a class must be escaped".to_owned());
                        }
                        Some(_) => j += 1,
                    }
                }
                atom = true;
                i = j + 1;
            }
            ']' => return Some("a `]` opens no class (escape it as `\\]`)".to_owned()),
            '*' | '+' | '?' => {
                if !atom {
                    return Some(format!("`{c}` has nothing to repeat"));
                }
                atom = false;
                i += 1;
                // A lazy quantifier.
                if chars.get(i) == Some(&'?') {
                    i += 1;
                }
            }
            '{' => {
                if !atom {
                    return Some("`{` has nothing to repeat (escape it as `\\{`)".to_owned());
                }
                let Some(close) = chars[i..].iter().position(|x| *x == '}') else {
                    return Some("a `{` is not closed".to_owned());
                };
                let inner: String = chars[i + 1..i + close].iter().collect();
                let ok = match inner.split_once(',') {
                    None => inner.parse::<u32>().is_ok(),
                    Some((lo, "")) => lo.parse::<u32>().is_ok(),
                    Some((lo, hi)) => match (lo.parse::<u32>(), hi.parse::<u32>()) {
                        (Ok(lo), Ok(hi)) => lo <= hi,
                        _ => false,
                    },
                };
                if !ok {
                    return Some(format!(
                        "`{{{inner}}}` is not a repetition `{{m}}`, `{{m,}}` or `{{m,n}}`"
                    ));
                }
                atom = false;
                i += close + 1;
            }
            '}' => return Some("a `}` closes no repetition (escape it as `\\}`)".to_owned()),
            '|' | '^' | '$' => {
                atom = false;
                i += 1;
            }
            _ => {
                atom = true;
                i += 1;
            }
        }
    }
    (depth > 0).then(|| "a `(` is not closed".to_owned())
}

/// The text of a string literal without interpolations.
fn plain(s: &ast::StringLit) -> Option<String> {
    let mut out = String::new();
    for p in &s.parts {
        match p {
            StrPart::Text(t) => out.push_str(t),
            StrPart::Interp { .. } => return None,
        }
    }
    Some(out)
}

fn int_value(e: &ast::Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::Lit(Lit::Int(n)) => Some(*n),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Lit(Lit::Int(n)) => n.checked_neg(),
            _ => None,
        },
        _ => None,
    }
}

/// A number literal as JSON text.
fn number_value(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Lit(Lit::Int(n)) => Some(n.to_string()),
        ExprKind::Lit(Lit::Float(x)) if x.is_finite() => Some(float_json(*x)),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Lit(Lit::Int(n)) => n.checked_neg().map(|n| n.to_string()),
            ExprKind::Lit(Lit::Float(x)) if x.is_finite() => Some(float_json(-*x)),
            _ => None,
        },
        _ => None,
    }
}

/// A finite float as JSON: Rust's shortest round-trip form, with `.0` kept for whole numbers.
fn float_json(x: f64) -> String {
    let s = format!("{x:?}");
    if s.contains('e') || s.contains('E') {
        format!("{x}")
    } else {
        s
    }
}

/// A literal as JSON text: numbers, text, booleans, `none` and lists of them.
fn example(e: &ast::Expr, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match &e.kind {
        ExprKind::Lit(Lit::Bool(b)) => Some(b.to_string()),
        ExprKind::Lit(Lit::None) => Some("null".to_owned()),
        ExprKind::Lit(Lit::Str(s)) => {
            let mut out = String::new();
            json_string(&plain(s)?, &mut out);
            Some(out)
        }
        ExprKind::List(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                if item.spread {
                    return None;
                }
                parts.push(example(&item.expr, depth + 1)?);
            }
            Some(format!("[{}]", parts.join(",")))
        }
        _ => number_value(e),
    }
}
