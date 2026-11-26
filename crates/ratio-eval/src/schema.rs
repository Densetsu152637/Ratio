//! `json.schema::<T>()`: the JSON Schema (draft 2020-12) of a schema-able type, written in the
//! canonical form of chapter 03 ("Types are schemas"): no whitespace, keys in a fixed order.
//!
//! This derives the schema from the IR's types, as the checker (`ratio_check::schema`, whose
//! tests pin the same texts) derives it from its own; `tests/schema.rs` compares the two on
//! the types of chapter 03's table. The work is bounded: output of at most 1 MiB, nesting of
//! at most 256, and a work budget, as the checker's. A recursive enum is written once under
//! `$defs` and referred to by `$ref`.

use std::collections::HashSet;

use ratio_ir::{FieldType, Type};

use crate::prepare::Prepared;
use crate::value::Fault;

/// The largest schema written, in bytes.
const MAX_SCHEMA_BYTES: usize = 1 << 20;
/// The deepest nesting written.
const MAX_SCHEMA_DEPTH: usize = 256;
/// The work budget of one derivation.
const FUEL: u64 = 2_000_000;

/// Type parameters bound while walking a generic enum's payload.
type Env = Vec<(String, u32)>;

/// An enum instance: the enum and its type arguments.
type EnumKey = (u32, Vec<u32>);

enum J {
    Obj(Vec<(&'static str, J)>),
    /// An object with arbitrary keys (`properties`, `$defs`).
    Map(Vec<(String, J)>),
    Arr(Vec<J>),
    Str(String),
    /// Raw JSON text (numbers, `false`, examples).
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
                    crate::json::quote(k, out);
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
                    crate::json::quote(k, out);
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
            J::Str(s) => crate::json::quote(s, out),
            J::Raw(r) => out.push_str(r),
        }
    }
}

fn too_large() -> Fault {
    Fault::Invalid("the schema is too large or too deep to write".to_owned())
}

fn not_schemaable(what: &str) -> Fault {
    Fault::Invalid(format!("the type has no schema: {what}"))
}

/// A field's refinements, as the IR writes them (`desc="…" len=a,b range="a","b"
/// pattern="…" example="…"`).
#[derive(Default)]
struct Meta {
    description: Option<String>,
    len: Option<(String, String)>,
    range: Option<(String, String)>,
    pattern: Option<String>,
    example: Option<String>,
}

/// A string in the IR's quoted form, from `s[*i..]` (which starts at the opening quote).
fn unquote(s: &[char], i: &mut usize) -> Option<String> {
    if s.get(*i) != Some(&'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while let Some(&c) = s.get(*i) {
        *i += 1;
        match c {
            '"' => return Some(out),
            '\\' => {
                let e = *s.get(*i)?;
                *i += 1;
                match e {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        if s.get(*i) != Some(&'{') {
                            return None;
                        }
                        let close = s[*i..].iter().position(|c| *c == '}')? + *i;
                        let hex: String = s[*i + 1..close].iter().collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        *i = close + 1;
                    }
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
    }
    None
}

fn parse_meta(text: &str) -> Meta {
    let s: Vec<char> = text.chars().collect();
    let mut meta = Meta::default();
    let mut i = 0;
    while i < s.len() {
        while s.get(i) == Some(&' ') {
            i += 1;
        }
        let start = i;
        while i < s.len() && s[i] != '=' {
            i += 1;
        }
        let key: String = s[start..i].iter().collect();
        i += 1;
        match key.as_str() {
            "desc" => meta.description = unquote(&s, &mut i),
            "pattern" => meta.pattern = unquote(&s, &mut i),
            "example" => meta.example = unquote(&s, &mut i),
            "range" => {
                let lo = unquote(&s, &mut i);
                i += 1;
                let hi = unquote(&s, &mut i);
                meta.range = lo.zip(hi);
            }
            "len" => {
                let start = i;
                while i < s.len() && s[i] != ' ' {
                    i += 1;
                }
                let both: String = s[start..i].iter().collect();
                meta.len = both
                    .split_once(',')
                    .map(|(a, b)| (a.to_owned(), b.to_owned()));
            }
            _ => break,
        }
    }
    meta
}

fn refine(schema: &mut J, m: &Meta) {
    let J::Obj(pairs) = schema else {
        return;
    };
    if let Some(d) = &m.description {
        pairs.push(("description", J::Str(d.clone())));
    }
    if let Some((min, max)) = &m.len {
        pairs.push(("minItems", J::Raw(min.clone())));
        pairs.push(("maxItems", J::Raw(max.clone())));
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

struct Writer<'a> {
    p: &'a Prepared,
    /// How deep the walk may nest: the most the stack it runs on takes.
    nest: usize,
    /// Enums that contain themselves, with their `$defs` name.
    defs: Vec<(EnumKey, String)>,
    size: usize,
    fuel: u64,
}

/// What a type is made of, for the walks that look for recursion.
fn parts(p: &Prepared, t: u32, env: &Env) -> Result<Vec<(u32, Env)>, Fault> {
    let ty = resolve(p, t, env)?;
    Ok(match &ty.0 {
        Type::Option(a) | Type::List(a) | Type::Secret(a) | Type::Untrusted(a) => {
            vec![(*a, ty.1.clone())]
        }
        Type::Map(k, v) => vec![(*k, ty.1.clone()), (*v, ty.1.clone())],
        Type::Tuple(ts) => ts.iter().map(|x| (*x, ty.1.clone())).collect(),
        Type::Record(fields) => fields.iter().map(|f| (f.ty, ty.1.clone())).collect(),
        Type::Enum(e, args) => {
            let env2 = enum_env(p, *e, args, &ty.1);
            let mut out = Vec::new();
            if let Some(en) = p.enums.get(*e as usize) {
                for v in &en.variants {
                    if let Some(pt) = v.payload_ty {
                        out.push((pt, env2.clone()));
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    })
}

/// The type `t` stands for under `env` (a type parameter is looked up), with the environment
/// to read its parts in.
fn resolve<'a>(p: &'a Prepared, mut t: u32, env: &Env) -> Result<(&'a Type, Env), Fault> {
    for _ in 0..64 {
        match p.types.get(t as usize) {
            Some(Type::Param(name)) => {
                t = env
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, id)| *id)
                    .ok_or_else(|| not_schemaable("a type parameter"))?;
            }
            Some(other) => return Ok((other, env.clone())),
            None => return Err(not_schemaable("an unknown type")),
        }
    }
    Err(not_schemaable("a type parameter"))
}

/// An enum's parameters bound to the arguments it is used with (themselves resolved in the
/// environment the use is in).
fn enum_env(p: &Prepared, e: u32, args: &[u32], outer: &Env) -> Env {
    let params = p
        .enums
        .get(e as usize)
        .map(|en| en.params.clone())
        .unwrap_or_default();
    // An argument that is itself a parameter of the enclosing enum is looked up there.
    let bound = |a: u32| match p.types.get(a as usize) {
        Some(Type::Param(n)) => outer
            .iter()
            .find(|(name, _)| name == n)
            .map_or(a, |(_, id)| *id),
        _ => a,
    };
    params
        .into_iter()
        .zip(args.iter().map(|a| bound(*a)))
        .collect()
}

fn enum_key(p: &Prepared, t: u32, env: &Env) -> Option<EnumKey> {
    match resolve(p, t, env).ok()?.0 {
        Type::Enum(e, args) => Some((*e, args.clone())),
        Type::Untrusted(a) => enum_key(p, *a, env),
        _ => None,
    }
}

impl Writer<'_> {
    fn grow(&mut self, by: usize) -> Result<(), Fault> {
        self.size += by;
        self.fuel = self.fuel.saturating_sub(1);
        if self.size > MAX_SCHEMA_BYTES || self.fuel == 0 {
            return Err(too_large());
        }
        Ok(())
    }

    /// Finds the enums that contain themselves: every enum reachable from `t`, in the order
    /// first reached (breadth first), is recursive when a walk from its parts reaches it
    /// again.
    fn find_recursive(&mut self, t: u32) -> Result<(), Fault> {
        let mut enums: Vec<EnumKey> = Vec::new();
        let mut seen: HashSet<(u32, Env)> = HashSet::new();
        let mut queue = std::collections::VecDeque::from([(t, Env::new())]);
        while let Some((x, env)) = queue.pop_front() {
            if !seen.insert((x, env.clone())) {
                continue;
            }
            self.grow(1)?;
            if let Some(e) = enum_key(self.p, x, &env).filter(|e| !enums.contains(e)) {
                enums.push(e);
            }
            queue.extend(parts(self.p, x, &env)?);
        }
        for e in enums {
            let (id, args) = e.clone();
            let env = enum_env(self.p, id, &args, &Env::new());
            let mut seen: HashSet<(u32, Env)> = HashSet::new();
            let mut work: Vec<(u32, Env)> = Vec::new();
            if let Some(en) = self.p.enums.get(id as usize) {
                for v in &en.variants {
                    if let Some(pt) = v.payload_ty {
                        work.push((pt, env.clone()));
                    }
                }
            }
            let mut recursive = false;
            while let Some((x, xenv)) = work.pop() {
                if enum_key(self.p, x, &xenv).as_ref() == Some(&e) {
                    recursive = true;
                    break;
                }
                if seen.insert((x, xenv.clone())) {
                    self.grow(1)?;
                    work.extend(parts(self.p, x, &xenv)?);
                }
            }
            if recursive {
                let base: String = self
                    .p
                    .enums
                    .get(id as usize)
                    .map(|en| en.name.to_string())
                    .unwrap_or_default();
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

    fn schema(&mut self, t: u32, env: &Env, depth: usize) -> Result<J, Fault> {
        if depth > self.nest {
            return Err(too_large());
        }
        self.grow(16)?;
        if let Some(key) = enum_key(self.p, t, env)
            && let Some((_, name)) = self.defs.iter().find(|(d, _)| *d == key)
        {
            return Ok(J::Obj(vec![("$ref", J::Str(format!("#/$defs/{name}")))]));
        }
        self.body(t, env, depth)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of type")]
    fn body(&mut self, t: u32, env: &Env, depth: usize) -> Result<J, Fault> {
        let d = depth + 1;
        let (ty, env) = resolve(self.p, t, env)?;
        Ok(match ty {
            Type::Unit => J::ty("null"),
            Type::Bool => J::ty("boolean"),
            Type::Int => J::ty("integer"),
            Type::Float => J::ty("number"),
            Type::Str | Type::UserText | Type::Brief | Type::UntrustedStr => J::ty("string"),
            Type::Duration => J::Obj(vec![
                ("type", J::Str("string".to_owned())),
                ("pattern", J::Str("^[0-9]+(ms|s|m|h)$".to_owned())),
            ]),
            Type::Money => J::Obj(vec![
                ("type", J::Str("string".to_owned())),
                ("pattern", J::Str("^\\$[0-9]+(\\.[0-9]{1,6})?$".to_owned())),
            ]),
            Type::Json => J::Obj(Vec::new()),
            Type::Untrusted(a) | Type::Secret(a) => self.schema(*a, &env, d)?,
            Type::Option(a) => J::Obj(vec![(
                "anyOf",
                J::Arr(vec![self.schema(*a, &env, d)?, J::ty("null")]),
            )]),
            Type::List(a) => J::Obj(vec![
                ("type", J::Str("array".to_owned())),
                ("items", self.schema(*a, &env, d)?),
            ]),
            Type::Tuple(ts) => {
                let mut items = Vec::with_capacity(ts.len());
                for x in ts {
                    items.push(self.schema(*x, &env, d)?);
                }
                J::Obj(vec![
                    ("type", J::Str("array".to_owned())),
                    ("prefixItems", J::Arr(items)),
                    ("items", J::Raw("false".to_owned())),
                ])
            }
            Type::Record(fields) => {
                let (props, required) = self.properties(fields, &env, d)?;
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
            Type::Map(k, v) => {
                let mut pairs = vec![
                    ("type", J::Str("object".to_owned())),
                    ("additionalProperties", self.schema(*v, &env, d)?),
                ];
                match resolve(self.p, *k, &env)?.0 {
                    Type::Int => pairs.push((
                        "propertyNames",
                        J::Obj(vec![("pattern", J::Str("^-?[0-9]+$".to_owned()))]),
                    )),
                    Type::Bool => pairs.push((
                        "propertyNames",
                        J::Obj(vec![(
                            "enum",
                            J::Arr(vec![J::Str("false".to_owned()), J::Str("true".to_owned())]),
                        )]),
                    )),
                    Type::Enum(id, _) => {
                        let names = self
                            .p
                            .enums
                            .get(*id as usize)
                            .map(|en| {
                                en.variants
                                    .iter()
                                    .map(|v| J::Str(v.json.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        pairs.push(("propertyNames", J::Obj(vec![("enum", J::Arr(names))])));
                    }
                    _ => {}
                }
                J::Obj(pairs)
            }
            Type::Enum(e, args) => self.enum_body(*e, args, &env, d)?,
            _ => {
                return Err(not_schemaable(
                    "a type that is not built from schema-able parts",
                ));
            }
        })
    }

    fn enum_body(&mut self, e: u32, args: &[u32], outer: &Env, depth: usize) -> Result<J, Fault> {
        let en = self
            .p
            .enums
            .get(e as usize)
            .ok_or_else(|| not_schemaable("an unknown enum"))?;
        if en.unit_only {
            let names = en
                .variants
                .iter()
                .map(|v| J::Str(v.json.to_string()))
                .collect();
            return Ok(J::Obj(vec![
                ("type", J::Str("string".to_owned())),
                ("enum", J::Arr(names)),
            ]));
        }
        let env = enum_env(self.p, e, args, outer);
        let mut cases = Vec::with_capacity(en.variants.len());
        for v in &en.variants {
            let kind = J::Obj(vec![("const", J::Str(v.json.to_string()))]);
            let mut props = vec![("kind".to_owned(), kind)];
            let mut required = vec!["kind".to_owned()];
            if let Some(pt) = v.payload_ty {
                let (ty, penv) = resolve(self.p, pt, &env)?;
                if let Type::Record(fields) = ty {
                    let (more, req) = self.properties(fields, &penv, depth)?;
                    props.extend(more);
                    required.extend(req);
                }
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
    #[allow(clippy::type_complexity, reason = "a pair of lists")]
    fn properties(
        &mut self,
        fields: &[FieldType],
        env: &Env,
        depth: usize,
    ) -> Result<(Vec<(String, J)>, Vec<String>), Fault> {
        let mut props = Vec::with_capacity(fields.len());
        let mut required = Vec::new();
        for f in fields {
            let (inner, absent) = match resolve(self.p, f.ty, env)?.0 {
                Type::Option(inner) => (*inner, true),
                _ => (f.ty, f.optional),
            };
            if !absent && !f.optional {
                required.push(f.name.clone());
            }
            let mut schema = self.schema(inner, env, depth)?;
            if !f.meta.is_empty() {
                refine(&mut schema, &parse_meta(&f.meta));
            }
            self.grow(f.name.len())?;
            props.push((f.name.clone(), schema));
        }
        Ok((props, required))
    }
}

/// The JSON Schema of type `t` of the program, as canonical JSON text.
///
/// # Errors
///
/// `Invalid`, when the type is not schema-able or its schema is over the bounds.
pub(crate) fn schema_text(p: &Prepared, t: u32) -> Result<String, Fault> {
    // The walks recurse over the type's nesting (at most 256); they run on a thread with a
    // large stack, as the preparation pass does, so a small caller's stack is safe.
    crate::stack::run(|max_depth| schema_on(p, t, MAX_SCHEMA_DEPTH.min(max_depth))).unwrap_or_else(
        || {
            Err(Fault::Invalid(
                "the schema's worker could not start".to_owned(),
            ))
        },
    )
}

fn schema_on(p: &Prepared, t: u32, nest: usize) -> Result<String, Fault> {
    let mut w = Writer {
        p,
        nest,
        defs: Vec::new(),
        size: 0,
        fuel: FUEL,
    };
    w.find_recursive(t)?;
    let root = w.schema(t, &Env::new(), 0)?;
    let mut defs = Vec::new();
    for (key, name) in w.defs.clone() {
        let body = w.enum_body(key.0, &key.1, &Env::new(), 1)?;
        defs.push((name, body));
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
        return Err(too_large());
    }
    Ok(out)
}
