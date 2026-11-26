//! `json.validate(j, schema)`: JSON Schema draft 2020-12 without remote references.
//!
//! Supported: `$ref` (to `#` and `#/…` pointers into the same schema), `type`, `enum`, `const`,
//! `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `minLength`,
//! `maxLength`, `pattern`, `prefixItems`, `items`, `minItems`, `maxItems`, `uniqueItems`,
//! `contains` with `minContains` and `maxContains`, `properties`, `patternProperties`,
//! `additionalProperties`, `propertyNames`, `required`, `minProperties`, `maxProperties`,
//! `dependentRequired`, `dependentSchemas`, `allOf`, `anyOf`, `oneOf`, `not`, and `if` with
//! `then` and `else`. Other keywords (annotations such as `title` and `description`, `format`,
//! `$defs`, `unevaluatedProperties`, …) are ignored, as the draft allows. A schema that is not a
//! boolean or an object, a keyword of the wrong shape, a `$ref` that does not resolve, a remote
//! `$ref` or a bad `pattern` is `Invalid`.
//!
//! The walk recurses over the instance's and the schema's nesting (at most 192 levels, an
//! `Invalid` beyond; `$ref` hops are counted apart, at most 32 at one node), on the caller's
//! stack when it is small and on a large-stack worker when it is not. Its work is the visits of
//! a schema node to an instance node, which the `product` charge pays for: it is counted, and
//! a walk that visits more than a multiple of the charge raises `Budget { which: steps }`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use crate::error::{Kind, RtError, raise};
use crate::heap::{Charge, cost};
use crate::jsonedit::{array, node, object, string_of, tokens};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::plist::List;
use crate::pmap::Map;
use crate::value::{Json, Value};
use crate::work::len_cost;

use super::util::{Pace, heap_error};
use crate::re::{Regex, Stopped, scratch_bytes, search_work};

/// The deepest the walk nests (instance levels, applicators and `$ref` hops together).
const MAX_NEST: u32 = 192;
/// The most `$ref` hops at one instance node (a cycle of references never ends otherwise).
const MAX_HOPS: u32 = 32;

type Check = Result<bool, RtError>;

/// The keywords of one schema object, found in one pass over its entries.
#[derive(Default)]
struct Kw<'a> {
    reference: Option<&'a Value>,
    ty: Option<&'a Value>,
    enumeration: Option<&'a Value>,
    constant: Option<&'a Value>,
    minimum: Option<&'a Value>,
    maximum: Option<&'a Value>,
    exclusive_minimum: Option<&'a Value>,
    exclusive_maximum: Option<&'a Value>,
    multiple_of: Option<&'a Value>,
    min_length: Option<&'a Value>,
    max_length: Option<&'a Value>,
    pattern: Option<&'a Value>,
    prefix_items: Option<&'a Value>,
    items: Option<&'a Value>,
    min_items: Option<&'a Value>,
    max_items: Option<&'a Value>,
    unique_items: Option<&'a Value>,
    contains: Option<&'a Value>,
    min_contains: Option<&'a Value>,
    max_contains: Option<&'a Value>,
    properties: Option<&'a Value>,
    pattern_properties: Option<&'a Value>,
    additional: Option<&'a Value>,
    property_names: Option<&'a Value>,
    required: Option<&'a Value>,
    min_properties: Option<&'a Value>,
    max_properties: Option<&'a Value>,
    dependent_required: Option<&'a Value>,
    dependent_schemas: Option<&'a Value>,
    all_of: Option<&'a Value>,
    any_of: Option<&'a Value>,
    one_of: Option<&'a Value>,
    not: Option<&'a Value>,
    if_: Option<&'a Value>,
    then: Option<&'a Value>,
    else_: Option<&'a Value>,
}

#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

fn num(v: &Value) -> Option<Num> {
    match node(v)? {
        Json::Int(i) => Some(Num::Int(*i)),
        Json::Float(f) => Some(Num::Float(*f)),
        _ => None,
    }
}

#[allow(clippy::cast_precision_loss, reason = "numbers compare by value")]
fn cmp_num(a: Num, b: Num) -> Ordering {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => x.cmp(&y),
        (Num::Int(x), Num::Float(y)) => (x as f64).total_cmp(&y),
        (Num::Float(x), Num::Int(y)) => x.total_cmp(&(y as f64)),
        (Num::Float(x), Num::Float(y)) => x.total_cmp(&y),
    }
}

pub(super) struct Validator<'a, 'b> {
    cx: &'a Cx<'b>,
    root: &'a Value,
    /// The instance pointer of the node being checked, as segments (a key is shared, not
    /// copied): it is written out only when a violation is reported.
    path: Vec<Seg>,
    /// While above 0, violations are not reported (an `anyOf` branch, a `not`, an `if`).
    silent: u32,
    sink: &'a mut Sink<'a>,
    pace: Pace,
    /// How deep the walk may nest (instance levels and applicators, not `$ref` hops): the most
    /// the stack it runs on takes.
    nest: u32,
    /// The most `$ref` hops at one instance node.
    max_hops: u32,
    /// Runs on the caller's stack with small bounds, buffering what it reports; going past a
    /// bound sets `restart` and the walk is made again on a large stack.
    inline: bool,
    restart: bool,
    buffer: Vec<(String, String)>,
    /// The visits of a schema node to an instance node so far, and the most there may be: a
    /// multiple of the product the built-in was charged, so shared sub-schemas reached over and
    /// over (`$ref` into `allOf`) cannot do more work than was paid for.
    visits: u64,
    budget: u64,
    /// The steps debited for pattern work (compiling, searching), which the product charge does
    /// not size: added to the call's `debit`, and never more than the steps left.
    paid: &'a AtomicU64,
    /// The patterns compiled so far, with the heap each holds, and where each is.
    patterns: Vec<(String, Regex, Charge)>,
    pattern_index: HashMap<String, usize>,
    /// The nodes the `$ref`s resolved to, by their text.
    refs: HashMap<String, Value>,
    /// The labels of the instance and the schema: what a bad schema's error depends on.
    labels: Labels,
}

/// One step of an instance pointer.
#[derive(Clone, Debug)]
enum Seg {
    Key(Value),
    Index(u64),
}

/// Receives each violation: the instance pointer and a message.
pub(super) type Sink<'a> = dyn FnMut(&str, String) -> Result<(), RtError> + Send + 'a;

/// The nesting and `$ref` hops the walk takes on the caller's stack, and the violations it
/// buffers there.
const INLINE_NEST: u32 = 12;
const INLINE_HOPS: u32 = 8;
const INLINE_BUFFER: usize = 256;

/// The walk's work allowed for a charge of `n1 * n2 + n1 + n2` (the sizes of the two inputs):
/// four units per pair of nodes (a visit, the lookups, comparisons and copies inside it), and a floor for tiny inputs.
pub(super) fn visit_budget(n1: u64, n2: u64) -> u64 {
    n1.saturating_mul(n2)
        .saturating_add(n1)
        .saturating_add(n2)
        .saturating_mul(4)
        .saturating_add(256)
}

/// Validates `instance` against `schema`, reporting each violation to `sink`. `budget` bounds
/// the visits (see [`visit_budget`]); past it the call raises `Budget { which: steps }`.
///
/// Small walks run on the caller's stack. A walk that nests deeper than [`INLINE_NEST`] (or
/// hops further, or reports more than a few violations) is made again on a thread with a large
/// stack, as the preparation pass does, so no caller's stack can be exhausted by a nested
/// document or schema; its counted work is added to the caller's.
///
/// # Errors
///
/// `Invalid` for a bad schema or too deep a nesting; `Budget` past the budget; what `sink`
/// returns; `Cancelled`; heap.
pub(super) fn validate(
    cx: &Cx<'_>,
    instance: &Value,
    schema: &Value,
    budget: u64,
    paid: &AtomicU64,
    sink: &mut Sink<'_>,
) -> Result<(), RtError> {
    let labels = instance.quick() | schema.quick();
    let counted_before = crate::work::total();
    let walk = |sink: &mut Sink<'_>, nest: u32, max_hops: u32, inline: bool| {
        let mut v = Validator {
            cx,
            root: schema,
            path: Vec::new(),
            silent: 0,
            sink,
            pace: Pace::new(),
            nest,
            max_hops,
            inline,
            restart: false,
            buffer: Vec::new(),
            visits: 0,
            budget,
            paid,
            patterns: Vec::new(),
            pattern_index: HashMap::new(),
            refs: HashMap::new(),
            labels,
        };
        let r = v.check(instance, schema, 0, 0).map(|_| ());
        (r, v.restart, std::mem::take(&mut v.buffer))
    };
    let (r, restart, buffered) = walk(&mut *sink, INLINE_NEST, INLINE_HOPS, true);
    if !restart {
        guard(counted_before, budget, paid);
        for (pointer, message) in buffered {
            sink(&pointer, message)?;
        }
        return r;
    }
    // The pattern work of the first attempt is done again, and paid for once.
    paid.store(0, AtomicOrdering::Relaxed);
    let (r, units) = crate::stack::run(|max_depth| {
        let nest = MAX_NEST.min(u32::try_from(max_depth).unwrap_or(MAX_NEST));
        let (r, _, _) = walk(sink, nest, MAX_HOPS, false);
        (r, crate::work::total())
    })
    .unwrap_or_else(|| {
        (
            Err(raise(
                cx.p,
                Kind::Invalid,
                "json.validate could not start its worker",
                Labels::NONE,
                cx.loc,
            )),
            0,
        )
    });
    crate::work::add(units);
    guard(counted_before, budget, paid);
    r
}

/// The most counted work units there may be per unit of the visit budget (and per unit of
/// pattern work paid for): a general guard, so a loop that escapes the budget is found by the
/// property test, not by a hang.
const GUARD_FACTOR: u64 = 64;

/// In debug builds: the work this walk counted stays within [`GUARD_FACTOR`] times its budget
/// and what it paid for pattern work.
fn guard(before: u64, budget: u64, paid: &AtomicU64) {
    if crate::work::ENABLED {
        let counted = crate::work::total().saturating_sub(before);
        let allowed = GUARD_FACTOR
            .saturating_mul(budget.saturating_add(64 * paid.load(AtomicOrdering::Relaxed)))
            .saturating_add(4096);
        debug_assert!(
            counted <= allowed,
            "json.validate counted {counted} units of work for a budget of {budget}"
        );
    }
}

impl Validator<'_, '_> {
    fn bad(&self, what: &str) -> RtError {
        raise(
            self.cx.p,
            Kind::Invalid,
            &format!("bad schema: {what}"),
            self.labels,
            self.cx.loc,
        )
    }

    /// Reports a violation at the current path, unless a branch is being tried silently.
    fn fail(&mut self, message: &str) -> Check {
        if self.silent == 0 {
            let pointer = self.pointer()?;
            if self.inline {
                if self.buffer.len() >= INLINE_BUFFER {
                    return Err(self.restart());
                }
                self.buffer.push((pointer, message.to_owned()));
            } else {
                (self.sink)(&pointer, message.to_owned())?;
            }
        }
        Ok(false)
    }

    /// The pointer of the current node, paid for by its length.
    fn pointer(&mut self) -> Result<String, RtError> {
        let mut out = String::new();
        let mut units = 1u64;
        for seg in &self.path {
            out.push('/');
            match seg {
                Seg::Index(i) => out.push_str(&i.to_string()),
                Seg::Key(k) => {
                    let key = k.as_text().unwrap_or("");
                    units = units.saturating_add(2 * crate::work::len_cost(key));
                    out.push_str(&crate::jsonedit::escape(key));
                }
            }
        }
        self.work(units.saturating_add(self.path.len() as u64))?;
        Ok(out)
    }

    /// The value of `name` among an object's entries, each comparison paid for by the length
    /// compared.
    fn get<'e>(
        &mut self,
        entries: &'e [(Value, Value)],
        name: &str,
    ) -> Result<Option<&'e Value>, RtError> {
        let mut units = 1u64;
        let mut found = None;
        for (k, x) in entries {
            let key = k.as_text().unwrap_or("");
            units = units.saturating_add(crate::work::eq_cost(key, name));
            if key == name {
                found = Some(x);
                break;
            }
        }
        self.work(units)?;
        Ok(found)
    }

    /// Gives up the walk on the caller's stack: it is made again on a large one.
    fn restart(&mut self) -> RtError {
        self.restart = true;
        raise(
            self.cx.p,
            Kind::Invalid,
            "json.validate restarts on a larger stack",
            Labels::NONE,
            self.cx.loc,
        )
    }

    fn count(&self, v: &Value, what: &str) -> Result<u64, RtError> {
        match num(v) {
            Some(Num::Int(i)) if i >= 0 => Ok(i.unsigned_abs()),
            _ => Err(self.bad(&format!("`{what}` is a non-negative integer"))),
        }
    }

    fn schemas<'v>(&self, v: &'v Value, what: &str) -> Result<&'v List, RtError> {
        array(v).ok_or_else(|| self.bad(&format!("`{what}` is an array of schemas")))
    }

    fn collect<'v>(&self, schema: &'v Value) -> Result<Box<Kw<'v>>, RtError> {
        let Some(entries) = object(schema) else {
            return Err(self.bad("a schema is an object or a boolean"));
        };
        let mut kw = Box::<Kw<'v>>::default();
        for (k, v) in entries {
            let slot = match k.as_text().unwrap_or("") {
                "$ref" => &mut kw.reference,
                "type" => &mut kw.ty,
                "enum" => &mut kw.enumeration,
                "const" => &mut kw.constant,
                "minimum" => &mut kw.minimum,
                "maximum" => &mut kw.maximum,
                "exclusiveMinimum" => &mut kw.exclusive_minimum,
                "exclusiveMaximum" => &mut kw.exclusive_maximum,
                "multipleOf" => &mut kw.multiple_of,
                "minLength" => &mut kw.min_length,
                "maxLength" => &mut kw.max_length,
                "pattern" => &mut kw.pattern,
                "prefixItems" => &mut kw.prefix_items,
                "items" => &mut kw.items,
                "minItems" => &mut kw.min_items,
                "maxItems" => &mut kw.max_items,
                "uniqueItems" => &mut kw.unique_items,
                "contains" => &mut kw.contains,
                "minContains" => &mut kw.min_contains,
                "maxContains" => &mut kw.max_contains,
                "properties" => &mut kw.properties,
                "patternProperties" => &mut kw.pattern_properties,
                "additionalProperties" => &mut kw.additional,
                "propertyNames" => &mut kw.property_names,
                "required" => &mut kw.required,
                "minProperties" => &mut kw.min_properties,
                "maxProperties" => &mut kw.max_properties,
                "dependentRequired" => &mut kw.dependent_required,
                "dependentSchemas" => &mut kw.dependent_schemas,
                "allOf" => &mut kw.all_of,
                "anyOf" => &mut kw.any_of,
                "oneOf" => &mut kw.one_of,
                "not" => &mut kw.not,
                "if" => &mut kw.if_,
                "then" => &mut kw.then,
                "else" => &mut kw.else_,
                _ => continue,
            };
            *slot = Some(v);
        }
        Ok(kw)
    }

    /// Checks `inst` against `schema`; `Ok(true)` when it is valid.
    fn check(&mut self, inst: &Value, schema: &Value, depth: u32, hops: u32) -> Check {
        self.spend(1)?;
        if depth > self.nest {
            if self.inline {
                return Err(self.restart());
            }
            return Err(self.bad("the instance and the schema nest too deeply to validate"));
        }
        match node(schema) {
            Some(Json::Bool(true)) => return Ok(true),
            Some(Json::Bool(false)) => return self.fail("no value is valid against `false`"),
            Some(Json::Object(entries)) => self.work(1 + entries.len() as u64)?,
            _ => return Err(self.bad("a schema is an object or a boolean")),
        }
        let kw = self.collect(schema)?;
        let mut ok = true;
        if let Some(r) = kw.reference {
            ok &= self.reference(inst, r, depth, hops)?;
        }
        ok &= self.shape(inst, &kw)?;
        ok &= self.number(inst, &kw)?;
        ok &= self.string(inst, &kw)?;
        ok &= self.list(inst, &kw, depth)?;
        ok &= self.members(inst, &kw, depth)?;
        ok &= self.combine(inst, &kw, depth)?;
        Ok(ok)
    }

    fn reference(&mut self, inst: &Value, r: &Value, depth: u32, hops: u32) -> Check {
        let Some(target) = string_of(r) else {
            return Err(self.bad("`$ref` is a string"));
        };
        let Some(pointer) = target.strip_prefix('#') else {
            return Err(self.bad("only references into the schema itself (`#…`) are supported"));
        };
        if hops >= self.max_hops {
            if self.inline && hops < MAX_HOPS {
                return Err(self.restart());
            }
            return Err(self.bad("`$ref` refers to itself"));
        }
        // Hashing the target, and comparing it with the entry found.
        self.work(1 + 2 * target.len() as u64)?;
        let sub = if let Some(sub) = self.refs.get(target).cloned() {
            sub
        } else {
            // Resolved once per walk: the pointer's length and the widths scanned are the
            // work, and the node is kept.
            self.work(1 + pointer.len() as u64)?;
            let toks = tokens(pointer).map_err(|f| self.cx.fault(f, self.labels))?;
            let (found, scanned) = crate::jsonedit::lookup_scanned(self.root, &toks);
            self.work(scanned)?;
            let Some(sub) = found else {
                return Err(self.bad(&format!("`$ref` `{target}` does not resolve")));
            };
            self.refs.insert(target.to_owned(), sub.clone());
            sub
        };
        // A hop is counted in `hops`, not in the nesting: a recursive schema reaches as deep as
        // the instance nests.
        self.check(inst, &sub, depth, hops + 1)
    }

    fn shape(&mut self, inst: &Value, kw: &Kw<'_>) -> Check {
        let mut ok = true;
        if let Some(t) = kw.ty {
            let mut known = true;
            let mut any = false;
            let mut count = 0u64;
            match (string_of(t), array(t)) {
                (Some(s), _) => match type_matches(inst, s) {
                    Some(m) => any = m,
                    None => known = false,
                },
                (_, Some(l)) => {
                    // Every name is looked at on every visit: paid for by the list's length.
                    self.work(1 + l.len())?;
                    let mut c = l.cursor();
                    while let Some(x) = c.next() {
                        let Some(s) = string_of(&x) else {
                            return Err(self.bad("`type` names types"));
                        };
                        count += 1;
                        match type_matches(inst, s) {
                            Some(m) => any |= m,
                            None => known = false,
                        }
                    }
                }
                _ => return Err(self.bad("`type` is a string or an array of strings")),
            }
            if !known {
                return Err(self.bad("`type` names an unknown type"));
            }
            if !any {
                let mut names: Vec<String> = Vec::new();
                match (string_of(t), array(t)) {
                    (Some(s), _) => names.push(s.to_owned()),
                    // The message shows at most the first 8 names.
                    (_, Some(l)) => {
                        let mut c = l.cursor();
                        while let (Some(x), true) = (c.next(), names.len() < 8) {
                            names.push(string_of(&x).unwrap_or("").to_owned());
                        }
                    }
                    _ => {}
                }
                let more = if count > 8 { " or …" } else { "" };
                ok &= self.fail(&format!("expected {}{more}", names.join(" or ")))?;
            }
        }
        if let Some(e) = kw.enumeration {
            let Some(l) = array(e) else {
                return Err(self.bad("`enum` is an array"));
            };
            let mut found = false;
            let mut c = l.cursor();
            while let Some(x) = c.next() {
                self.work(1 + inst.weight().min(x.weight()))?;
                if crate::ops::eq(inst, &x) {
                    found = true;
                    break;
                }
            }
            if !found {
                ok &= self.fail("is not one of the allowed values")?;
            }
        }
        if let Some(c) = kw.constant {
            self.work(1 + inst.weight().min(c.weight()))?;
        }
        if let Some(c) = kw.constant
            && !crate::ops::eq(inst, c)
        {
            ok &= self.fail("is not the required value")?;
        }
        Ok(ok)
    }

    fn number(&mut self, inst: &Value, kw: &Kw<'_>) -> Check {
        let Some(x) = num(inst) else {
            return Ok(true);
        };
        let mut ok = true;
        let bound = |this: &Self, v: Option<&Value>, what: &str| -> Result<Option<Num>, RtError> {
            match v {
                None => Ok(None),
                Some(v) => num(v)
                    .map(Some)
                    .ok_or_else(|| this.bad(&format!("`{what}` is a number"))),
            }
        };
        if let Some(m) = bound(self, kw.minimum, "minimum")?
            && cmp_num(x, m) == Ordering::Less
        {
            ok &= self.fail("is below the minimum")?;
        }
        if let Some(m) = bound(self, kw.maximum, "maximum")?
            && cmp_num(x, m) == Ordering::Greater
        {
            ok &= self.fail("is above the maximum")?;
        }
        if let Some(m) = bound(self, kw.exclusive_minimum, "exclusiveMinimum")?
            && cmp_num(x, m) != Ordering::Greater
        {
            ok &= self.fail("is not above the exclusive minimum")?;
        }
        if let Some(m) = bound(self, kw.exclusive_maximum, "exclusiveMaximum")?
            && cmp_num(x, m) != Ordering::Less
        {
            ok &= self.fail("is not below the exclusive maximum")?;
        }
        if let Some(m) = bound(self, kw.multiple_of, "multipleOf")? {
            let multiple = match (x, m) {
                (_, Num::Int(b)) if b <= 0 => {
                    return Err(self.bad("`multipleOf` is above 0"));
                }
                (Num::Int(a), Num::Int(b)) => a.checked_rem(b) == Some(0) || b == -1,
                (a, b) => {
                    let (a, b) = (as_f64(a), as_f64(b));
                    if b <= 0.0 {
                        return Err(self.bad("`multipleOf` is above 0"));
                    }
                    let q = a / b;
                    q.is_finite() && (q - q.round()).abs() < 1e-9
                }
            };
            if !multiple {
                ok &= self.fail("is not a multiple of the required number")?;
            }
        }
        Ok(ok)
    }

    /// Counts `units` of work (for the counters and for cancellation) and spends them from the
    /// walk's budget, so what is done inside one check is paid for, not only the checks.
    fn work(&mut self, units: u64) -> Result<(), RtError> {
        self.spend(units)?;
        self.pace.tick(self.cx, units)
    }

    /// Adds work to the walk's count: past the budget the call is refused (`Budget { steps }`).
    fn spend(&mut self, units: u64) -> Result<(), RtError> {
        self.visits = self.visits.saturating_add(units);
        if self.visits > self.budget {
            return Err(raise(
                self.cx.p,
                Kind::Budget("steps"),
                "json.validate did more work than its charge paid for",
                Labels::NONE,
                self.cx.loc,
            ));
        }
        Ok(())
    }

    /// Debits `work` units of pattern work as steps, or raises `Budget { which: steps }` before
    /// it is done when the steps left cannot pay.
    fn pay(&mut self, work: u64) -> Result<(), RtError> {
        let steps = work.div_ceil(64);
        let total = self
            .paid
            .load(AtomicOrdering::Relaxed)
            .saturating_add(steps);
        if total > self.cx.steps_left {
            return Err(raise(
                self.cx.p,
                Kind::Budget("steps"),
                "json.validate's patterns need more steps than are left",
                Labels::NONE,
                self.cx.loc,
            ));
        }
        self.paid.store(total, AtomicOrdering::Relaxed);
        Ok(())
    }

    fn compiled(&mut self, pattern: &str) -> Result<usize, RtError> {
        self.work(1 + 2 * pattern.len() as u64)?;
        if let Some(&i) = self.pattern_index.get(pattern) {
            return Ok(i);
        }
        let translated = super::ecma::to_re(pattern)
            .map_err(|why| self.bad(&format!("`pattern` {pattern:?}: {why}")))?;
        let re = match Regex::new(&translated) {
            Ok(re) => re,
            Err(refusal) => {
                self.pay(refusal.work)?;
                return Err(self.bad(&format!("`pattern` {pattern:?}: {}", refusal.message)));
            }
        };
        self.pay(re.compile_work)?;
        let mut charge = Charge::none();
        charge
            .grow(
                self.cx.heap,
                cost::SLOT.saturating_mul(re.weight().saturating_add(re.insts())),
            )
            .map_err(|_| heap_error(self.cx))?;
        self.patterns.push((pattern.to_owned(), re, charge));
        self.pattern_index
            .insert(pattern.to_owned(), self.patterns.len() - 1);
        Ok(self.patterns.len() - 1)
    }

    fn matches(&mut self, pattern: &str, text: &str) -> Result<bool, RtError> {
        let i = self.compiled(pattern)?;
        // The text's characters are counted, and the search is paid for before it runs: a
        // program's weight per character.
        self.work(len_cost(text))?;
        let re = &self.patterns[i].1;
        let predicted = search_work(re.weight(), text.chars().count() as u64, 0);
        let scratch = scratch_bytes(re.insts(), 0);
        self.pay(predicted)?;
        let mut held = Charge::none();
        held.grow(self.cx.heap, scratch)
            .map_err(|_| heap_error(self.cx))?;
        let re = &self.patterns[i].1;
        let mut vm = re.vm(0);
        let mut work = 0u64;
        let cancel = self.cx.cancel;
        let found = vm.search(&re.program, text, 0, &mut work, &|| cancel.is_cancelled());
        drop(held);
        crate::work::add(work);
        match found {
            Ok(m) => Ok(m.is_some()),
            Err(Stopped) => Err(raise(
                self.cx.p,
                Kind::Cancelled,
                "the evaluation was cancelled",
                Labels::NONE,
                self.cx.loc,
            )),
        }
    }

    fn string(&mut self, inst: &Value, kw: &Kw<'_>) -> Check {
        let Some(Json::Str(s)) = node(inst) else {
            return Ok(true);
        };
        let mut ok = true;
        if let Some(m) = kw.min_length
            && s.chars() < self.count(m, "minLength")?
        {
            ok &= self.fail("is shorter than `minLength`")?;
        }
        if let Some(m) = kw.max_length
            && s.chars() > self.count(m, "maxLength")?
        {
            ok &= self.fail("is longer than `maxLength`")?;
        }
        if let Some(p) = kw.pattern {
            let Some(pattern) = string_of(p) else {
                return Err(self.bad("`pattern` is a string"));
            };
            if !self.matches(pattern, s.as_str())? {
                ok &= self.fail("does not match the pattern")?;
            }
        }
        Ok(ok)
    }

    /// Checks the child `child` of the current node at `seg` against `sub`.
    fn descend(&mut self, child: &Value, sub: &Value, seg: Seg, depth: u32) -> Check {
        self.path.push(seg);
        let r = self.check(child, sub, depth + 1, 0);
        self.path.pop();
        r
    }

    fn silently(&mut self, inst: &Value, sub: &Value, depth: u32) -> Check {
        self.silent += 1;
        let r = self.check(inst, sub, depth + 1, 0);
        self.silent -= 1;
        r
    }

    fn list(&mut self, inst: &Value, kw: &Kw<'_>, depth: u32) -> Check {
        let Some(l) = array(inst) else {
            return Ok(true);
        };
        let mut ok = true;
        let len = l.len();
        if let Some(m) = kw.min_items
            && len < self.count(m, "minItems")?
        {
            ok &= self.fail("has fewer items than `minItems`")?;
        }
        if let Some(m) = kw.max_items
            && len > self.count(m, "maxItems")?
        {
            ok &= self.fail("has more items than `maxItems`")?;
        }
        let prefix = match kw.prefix_items {
            Some(p) => Some(self.schemas(p, "prefixItems")?),
            None => None,
        };
        let mut c = l.cursor();
        let mut i = 0u64;
        while let Some(item) = c.next() {
            self.work(1)?;
            let sub = prefix.and_then(|p| p.get(i)).or(kw.items);
            if let Some(sub) = sub {
                ok &= self.descend(&item, sub, Seg::Index(i), depth)?;
            }
            i += 1;
        }
        if let Some(u) = kw.unique_items
            && matches!(node(u), Some(Json::Bool(true)))
        {
            let mut seen = Map::empty();
            let mut c = l.cursor();
            let mut duplicate = false;
            while let Some(item) = c.next() {
                // Each item is compared with the ones on a path of the map: its weight times
                // the path's length.
                self.work(
                    1 + item
                        .weight()
                        .saturating_mul(crate::ops::log_work(seen.len()) + 1),
                )?;
                if seen.get_entry(&item).is_some() {
                    duplicate = true;
                    break;
                }
                seen = seen
                    .insert(self.cx.heap, item, Value::unit())
                    .map_err(|f| self.cx.fault(f, self.labels))?;
            }
            if duplicate {
                ok &= self.fail("has duplicate items")?;
            }
        }
        if let Some(sub) = kw.contains {
            let mut hits = 0u64;
            let mut c = l.cursor();
            while let Some(item) = c.next() {
                if self.silently(&item, sub, depth)? {
                    hits += 1;
                }
            }
            let min = match kw.min_contains {
                Some(m) => self.count(m, "minContains")?,
                None => 1,
            };
            if hits < min {
                ok &= self.fail("has too few items matching `contains`")?;
            }
            if let Some(m) = kw.max_contains
                && hits > self.count(m, "maxContains")?
            {
                ok &= self.fail("has too many items matching `contains`")?;
            }
        }
        Ok(ok)
    }

    fn members(&mut self, inst: &Value, kw: &Kw<'_>, depth: u32) -> Check {
        let Some(entries) = object(inst) else {
            return Ok(true);
        };
        let mut ok = true;
        let width = entries.len() as u64;
        if let Some(m) = kw.min_properties
            && width < self.count(m, "minProperties")?
        {
            ok &= self.fail("has fewer properties than `minProperties`")?;
        }
        if let Some(m) = kw.max_properties
            && width > self.count(m, "maxProperties")?
        {
            ok &= self.fail("has more properties than `maxProperties`")?;
        }
        if let Some(r) = kw.required {
            let Some(l) = array(r) else {
                return Err(self.bad("`required` is an array of names"));
            };
            let mut c = l.cursor();
            while let Some(n) = c.next() {
                let Some(name) = string_of(&n) else {
                    return Err(self.bad("`required` is an array of names"));
                };
                self.work(1 + width)?;
                if self.get(entries, name)?.is_none() {
                    ok &= self.fail(&format!("is missing the required property `{name}`"))?;
                }
            }
        }
        let declared = kw.properties.and_then(object);
        if kw.properties.is_some() && declared.is_none() {
            return Err(self.bad("`properties` is an object"));
        }
        let patterns = kw.pattern_properties.and_then(object);
        if kw.pattern_properties.is_some() && patterns.is_none() {
            return Err(self.bad("`patternProperties` is an object"));
        }
        for (k, v) in entries {
            let name = k.as_text().unwrap_or("");
            self.work(1)?;
            let mut matched = false;
            if let Some(d) = declared {
                self.work(1 + d.len() as u64)?;
                if let Some(sub) = self.get(d, name)? {
                    matched = true;
                    ok &= self.descend(v, sub, Seg::Key(k.clone()), depth)?;
                }
            }
            for (p, sub) in patterns.unwrap_or(&[]) {
                let Some(pattern) = p.as_text() else {
                    continue;
                };
                if self.matches(pattern, name)? {
                    matched = true;
                    ok &= self.descend(v, sub, Seg::Key(k.clone()), depth)?;
                }
            }
            if !matched && let Some(extra) = kw.additional {
                ok &= self.descend(v, extra, Seg::Key(k.clone()), depth)?;
            }
            if let Some(names) = kw.property_names {
                self.work(len_cost(name))?;
                let key = crate::jsonedit::text_node(self.cx.heap, name)
                    .map_err(|f| self.cx.fault(f, self.labels))?;
                self.silent += 1;
                let valid = self.check(&key, names, depth + 1, 0);
                self.silent -= 1;
                if !valid? {
                    ok &= self.fail(&format!("the property name `{name}` is not allowed"))?;
                }
            }
        }
        if let Some(d) = kw.dependent_required {
            let Some(deps) = object(d) else {
                return Err(self.bad("`dependentRequired` is an object"));
            };
            for (name, needs) in deps {
                self.work(1 + width)?;
                if self.get(entries, name.as_text().unwrap_or(""))?.is_none() {
                    continue;
                }
                let Some(l) = array(needs) else {
                    return Err(self.bad("`dependentRequired` maps names to arrays of names"));
                };
                let mut c = l.cursor();
                while let Some(n) = c.next() {
                    let Some(need) = string_of(&n) else {
                        return Err(self.bad("`dependentRequired` maps names to arrays of names"));
                    };
                    self.work(1 + width)?;
                    if self.get(entries, need)?.is_none() {
                        ok &= self.fail(&format!(
                            "has `{}` but not `{need}`",
                            name.as_text().unwrap_or("")
                        ))?;
                    }
                }
            }
        }
        if let Some(d) = kw.dependent_schemas {
            let Some(deps) = object(d) else {
                return Err(self.bad("`dependentSchemas` is an object"));
            };
            for (name, sub) in deps {
                self.work(1 + width)?;
                if self.get(entries, name.as_text().unwrap_or(""))?.is_some() {
                    ok &= self.check(inst, sub, depth + 1, 0)?;
                }
            }
        }
        Ok(ok)
    }

    fn combine(&mut self, inst: &Value, kw: &Kw<'_>, depth: u32) -> Check {
        let mut ok = true;
        if let Some(a) = kw.all_of {
            let mut c = self.schemas(a, "allOf")?.cursor();
            while let Some(sub) = c.next() {
                ok &= self.check(inst, &sub, depth + 1, 0)?;
            }
        }
        if let Some(a) = kw.any_of {
            let mut any = false;
            let mut c = self.schemas(a, "anyOf")?.cursor();
            while let Some(sub) = c.next() {
                if self.silently(inst, &sub, depth)? {
                    any = true;
                    break;
                }
            }
            if !any {
                ok &= self.fail("matches none of the `anyOf` schemas")?;
            }
        }
        if let Some(a) = kw.one_of {
            let mut hits = 0;
            let mut c = self.schemas(a, "oneOf")?.cursor();
            while let Some(sub) = c.next() {
                if self.silently(inst, &sub, depth)? {
                    hits += 1;
                }
            }
            if hits != 1 {
                ok &= self.fail(&format!(
                    "matches {hits} of the `oneOf` schemas, not exactly 1"
                ))?;
            }
        }
        if let Some(n) = kw.not
            && self.silently(inst, n, depth)?
        {
            ok &= self.fail("matches the schema `not` forbids")?;
        }
        if let Some(c) = kw.if_ {
            let branch = if self.silently(inst, c, depth)? {
                kw.then
            } else {
                kw.else_
            };
            if let Some(b) = branch {
                ok &= self.check(inst, b, depth + 1, 0)?;
            }
        }
        Ok(ok)
    }
}

fn as_f64(n: Num) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "numbers compare by value")]
    match n {
        Num::Int(i) => i as f64,
        Num::Float(f) => f,
    }
}

/// Whether the instance is of the named JSON Schema type; `None` for an unknown name.
fn type_matches(inst: &Value, name: &str) -> Option<bool> {
    let n = node(inst);
    Some(match name {
        "null" => matches!(n, Some(Json::Null)),
        "boolean" => matches!(n, Some(Json::Bool(_))),
        "integer" => match n {
            Some(Json::Int(_)) => true,
            Some(Json::Float(f)) => f.is_finite() && f.fract() == 0.0,
            _ => false,
        },
        "number" => matches!(n, Some(Json::Int(_) | Json::Float(_))),
        "string" => matches!(n, Some(Json::Str(_))),
        "array" => matches!(n, Some(Json::Array(_))),
        "object" => matches!(n, Some(Json::Object(_))),
        _ => return None,
    })
}
