//! Run-time values (chapter 03, "Values").
//!
//! A [`Value`] is a small representation plus its **own labels**: for a scalar or text, the
//! leaf's secrecy labels; for a container, the labels of whatever decided its shape (its
//! length, which elements it kept, their order, its key set), while each element carries its
//! own. Reading an element out of a container adds the container's own labels to it, so a
//! label put on a container reaches everything taken out of it.
//!
//! Every container also keeps a **label summary** (chapter 06, `data`): the union of the own
//! labels of everything inside it, maintained on construction. It may be set when no leaf is
//! labelled any more but is never clear when one is, so `quick()` (own labels and summary) lets
//! a sink skip a clear value in O(1), and `scan()` walks the leaves. The summary is
//! representation only; [`crate::sink`] checks that the two agree in debug builds.
//!
//! Values are immutable and shared by atomic reference counts, so they are `Send + Sync`.
//! Containers also record their **weight** (a bound on the work of walking them, for charges)
//! and **depth** (how deeply they nest, at most [`MAX_DEPTH`], so every recursive walk of a
//! value is bounded).

use std::sync::Arc;

use crate::heap::{Charge, Heap, HeapExhausted, cost};
use crate::labels::Labels;
use crate::plist::List;
use crate::pmap::Map;

/// How deeply values may nest (lists of lists, records of records, closures capturing
/// closures). Deeper values are refused with `Invalid`, so walking, comparing and dropping a
/// value always fits a small stack (the parser's nesting limit is 256 too).
pub const MAX_DEPTH: u32 = 256;

/// The largest string, in bytes (chapter 07: 16 MiB).
pub const MAX_STR_BYTES: usize = 16 * 1024 * 1024;

/// The most elements of one list or map (chapter 07: 1,000,000).
pub const MAX_ELEMS: u64 = 1_000_000;

/// Why a value could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The heap limit.
    Heap,
    /// Nested deeper than [`MAX_DEPTH`].
    Depth,
    /// Over a size limit (string bytes, list or map elements).
    TooLarge(&'static str),
    /// Anything else the operation refuses (raised as `Invalid`).
    Invalid(String),
}

impl From<HeapExhausted> for Fault {
    fn from(_: HeapExhausted) -> Self {
        Fault::Heap
    }
}

/// What a container records about its contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Meta {
    /// The union of the own labels of everything inside.
    pub summary: Labels,
    /// One plus the weight of every element (saturating): a bound on a full walk's work.
    pub weight: u64,
    /// One more than the deepest element.
    pub depth: u32,
}

impl Meta {
    /// The meta of a container of `items`.
    pub(crate) fn of<'a>(items: impl IntoIterator<Item = &'a Value>) -> Meta {
        let mut m = Meta {
            summary: Labels::NONE,
            weight: 1,
            depth: 1,
        };
        for v in items {
            m.add(v);
        }
        m
    }

    pub(crate) fn add(&mut self, v: &Value) {
        self.summary |= v.quick();
        self.weight = self.weight.saturating_add(v.weight());
        self.depth = self.depth.max(v.depth().saturating_add(1));
    }

    pub(crate) fn join(a: Meta, b: Meta) -> Meta {
        Meta {
            summary: a.summary | b.summary,
            weight: a.weight.saturating_add(b.weight),
            depth: a.depth.max(b.depth),
        }
    }

    pub(crate) fn check(self) -> Result<Meta, Fault> {
        if self.depth > MAX_DEPTH {
            Err(Fault::Depth)
        } else {
            Ok(self)
        }
    }
}

/// An immutable string with its character count and an all-ASCII flag cached.
#[derive(Clone, Debug)]
pub struct Str(Arc<StrData>);

#[derive(Debug)]
pub(crate) struct StrData {
    text: Box<str>,
    chars: u64,
    ascii: bool,
    _charge: Charge,
}

impl Str {
    /// A string, charged to `heap`.
    ///
    /// # Errors
    ///
    /// Over the heap limit or [`MAX_STR_BYTES`].
    pub fn new(heap: &Heap, text: String) -> Result<Str, Fault> {
        if text.len() > MAX_STR_BYTES {
            return Err(Fault::TooLarge("a string longer than 16 MiB"));
        }
        let charge = heap.alloc(cost::STR + text.len() as u64)?;
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len() as u64
        } else {
            text.chars().count() as u64
        };
        Ok(Str(Arc::new(StrData {
            text: text.into_boxed_str(),
            chars,
            ascii,
            _charge: charge,
        })))
    }

    /// An unmetered string (literals, host values).
    #[must_use]
    pub fn plain(text: &str) -> Str {
        let ascii = text.is_ascii();
        Str(Arc::new(StrData {
            text: text.into(),
            chars: if ascii {
                text.len() as u64
            } else {
                text.chars().count() as u64
            },
            ascii,
            _charge: Charge::none(),
        }))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0.text
    }

    /// The number of characters (Unicode scalar values).
    #[must_use]
    pub fn chars(&self) -> u64 {
        self.0.chars
    }

    #[must_use]
    pub fn is_ascii(&self) -> bool {
        self.0.ascii
    }
}

/// The field names of a record, in name order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub names: Box<[Arc<str>]>,
}

impl Layout {
    pub(crate) fn index(&self, name: &str) -> Option<usize> {
        self.names.binary_search_by(|n| (**n).cmp(name)).ok()
    }
}

#[derive(Debug)]
pub(crate) struct Agg {
    pub items: Box<[Value]>,
    pub meta: Meta,
    _charge: Charge,
}

#[derive(Debug)]
pub(crate) struct RecordData {
    pub layout: Arc<Layout>,
    pub items: Box<[Value]>,
    pub meta: Meta,
    _charge: Charge,
}

#[derive(Debug)]
pub(crate) struct VariantData {
    pub enum_: u32,
    pub index: u32,
    /// The payload record.
    pub payload: Value,
    pub meta: Meta,
    _charge: Charge,
}

#[derive(Debug)]
pub(crate) struct ClosureData {
    /// The prepared function.
    pub func: u32,
    /// `(own slot, value)`: what it captured when it was made.
    pub captured: Box<[(u32, Value)]>,
    pub meta: Meta,
    _charge: Charge,
}

#[derive(Debug)]
pub(crate) struct RangeData {
    pub start: i64,
    pub end: i64,
    pub inclusive: bool,
    _charge: Charge,
}

/// A JSON node; the labels of each node are on the [`Value`] that holds it.
#[derive(Debug)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Str),
    /// Elements are JSON values.
    Array(List),
    /// Keys (text values) and JSON values, in document order.
    Object(Box<[(Value, Value)]>),
}

#[derive(Debug)]
pub(crate) struct JsonData {
    pub node: Json,
    pub meta: Meta,
    _charge: Charge,
}

/// One section of a prompt value.
#[derive(Debug)]
pub(crate) struct Section {
    pub kind: ratio_ir::SectionKind,
    /// For `format`: the type as written.
    pub format: Option<Arc<str>>,
    pub values: Box<[Value]>,
    /// Each section is charged to the heap as it is built, before the prompt that holds it.
    _charge: Charge,
}

impl Section {
    /// A section, charged to `heap`.
    pub(crate) fn new(
        heap: &Heap,
        kind: ratio_ir::SectionKind,
        format: Option<Arc<str>>,
        values: Box<[Value]>,
    ) -> Result<Section, Fault> {
        let charge = heap.alloc(cost::SLOT.saturating_mul(1 + values.len() as u64))?;
        Ok(Section {
            kind,
            format,
            values,
            _charge: charge,
        })
    }
}

#[derive(Debug)]
pub(crate) struct PromptData {
    pub sections: Box<[Section]>,
    pub meta: Meta,
    _charge: Charge,
}

#[derive(Clone, Debug)]
pub(crate) enum Repr {
    Unit,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Duration(i64),
    Money(i64),
    Str(Str),
    Range(Arc<RangeData>),
    Tuple(Arc<Agg>),
    Record(Arc<RecordData>),
    List(List),
    Map(Map),
    /// A unit variant: the enum (by prepared index) and the variant.
    Variant(u32, u32),
    VariantP(Arc<VariantData>),
    /// A declared function, prompt or harness, by prepared item index.
    Item(u32),
    /// A built-in function, by its index in the table.
    Builtin(u16),
    Closure(Arc<ClosureData>),
    Json(Arc<JsonData>),
    Prompt(Arc<PromptData>),
    /// A tool name.
    Tool(Str),
}

/// A run-time value with its own secrecy labels.
#[derive(Clone, Debug)]
pub struct Value {
    pub(crate) repr: Repr,
    pub(crate) labels: Labels,
}

fn plain(repr: Repr) -> Value {
    Value {
        repr,
        labels: Labels::NONE,
    }
}

impl Value {
    // ---- scalars and text (never charged) ----

    #[must_use]
    pub fn unit() -> Value {
        plain(Repr::Unit)
    }
    #[must_use]
    pub fn none() -> Value {
        plain(Repr::None)
    }
    #[must_use]
    pub fn bool(b: bool) -> Value {
        plain(Repr::Bool(b))
    }
    #[must_use]
    pub fn int(i: i64) -> Value {
        plain(Repr::Int(i))
    }
    #[must_use]
    pub fn float(f: f64) -> Value {
        plain(Repr::Float(f))
    }
    /// A duration of whole microseconds.
    #[must_use]
    pub fn duration(us: i64) -> Value {
        plain(Repr::Duration(us))
    }
    /// An amount in millionths of a dollar.
    #[must_use]
    pub fn money(micro: i64) -> Value {
        plain(Repr::Money(micro))
    }
    /// Unmetered text (for hosts and tests).
    #[must_use]
    pub fn text(s: &str) -> Value {
        plain(Repr::Str(Str::plain(s)))
    }

    pub(crate) fn from_str(s: Str) -> Value {
        plain(Repr::Str(s))
    }

    pub(crate) fn new_text(heap: &Heap, s: String) -> Result<Value, Fault> {
        Ok(plain(Repr::Str(Str::new(heap, s)?)))
    }

    // ---- containers ----

    /// An unmetered list (for hosts and tests).
    ///
    /// # Errors
    ///
    /// Over the element or depth limits.
    pub fn list(items: Vec<Value>) -> Result<Value, Fault> {
        Ok(plain(Repr::List(List::from_vec(
            &Heap::unmetered(),
            items,
        )?)))
    }

    /// An unmetered tuple.
    ///
    /// # Errors
    ///
    /// Over the depth limit.
    pub fn tuple(items: Vec<Value>) -> Result<Value, Fault> {
        Value::new_tuple(&Heap::unmetered(), items)
    }

    /// An unmetered record of these fields (in any order).
    ///
    /// # Errors
    ///
    /// Over the depth limit, or a name given twice.
    pub fn record(fields: Vec<(&str, Value)>) -> Result<Value, Fault> {
        let mut fields: Vec<(Arc<str>, Value)> =
            fields.into_iter().map(|(n, v)| (Arc::from(n), v)).collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        if fields.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(Fault::Invalid("a field given twice".to_owned()));
        }
        let layout = Arc::new(Layout {
            names: fields.iter().map(|(n, _)| Arc::clone(n)).collect(),
        });
        Value::new_record(
            &Heap::unmetered(),
            layout,
            fields.into_iter().map(|(_, v)| v).collect(),
        )
    }

    pub(crate) fn new_tuple(heap: &Heap, items: Vec<Value>) -> Result<Value, Fault> {
        let meta = Meta::of(&items).check()?;
        let charge = heap.alloc(cost::AGG + cost::SLOT * items.len() as u64)?;
        Ok(plain(Repr::Tuple(Arc::new(Agg {
            items: items.into_boxed_slice(),
            meta,
            _charge: charge,
        }))))
    }

    pub(crate) fn new_record(
        heap: &Heap,
        layout: Arc<Layout>,
        items: Vec<Value>,
    ) -> Result<Value, Fault> {
        let mut meta = Meta::of(&items).check()?;
        // A rendering writes the field names: they count towards the weight charges use.
        let names = layout
            .names
            .iter()
            .fold(0u64, |n, x| n.saturating_add(x.len() as u64));
        meta.weight = meta.weight.saturating_add(names);
        let charge = heap.alloc(cost::AGG + cost::SLOT * items.len() as u64)?;
        Ok(plain(Repr::Record(Arc::new(RecordData {
            layout,
            items: items.into_boxed_slice(),
            meta,
            _charge: charge,
        }))))
    }

    pub(crate) fn new_list(list: List) -> Value {
        plain(Repr::List(list))
    }

    pub(crate) fn new_map(map: Map) -> Value {
        plain(Repr::Map(map))
    }

    pub(crate) fn new_variant(
        heap: &Heap,
        enum_: u32,
        index: u32,
        payload: Option<Value>,
    ) -> Result<Value, Fault> {
        let Some(payload) = payload else {
            return Ok(plain(Repr::Variant(enum_, index)));
        };
        let meta = Meta::of([&payload]).check()?;
        let charge = heap.alloc(cost::AGG)?;
        Ok(plain(Repr::VariantP(Arc::new(VariantData {
            enum_,
            index,
            payload,
            meta,
            _charge: charge,
        }))))
    }

    pub(crate) fn new_closure(
        heap: &Heap,
        func: u32,
        captured: Vec<(u32, Value)>,
    ) -> Result<Value, Fault> {
        let meta = Meta::of(captured.iter().map(|(_, v)| v)).check()?;
        let charge = heap.alloc(cost::AGG + cost::SLOT * captured.len() as u64)?;
        Ok(plain(Repr::Closure(Arc::new(ClosureData {
            func,
            captured: captured.into_boxed_slice(),
            meta,
            _charge: charge,
        }))))
    }

    pub(crate) fn new_range(
        heap: &Heap,
        start: i64,
        end: i64,
        inclusive: bool,
    ) -> Result<Value, Fault> {
        let charge = heap.alloc(cost::RANGE)?;
        Ok(plain(Repr::Range(Arc::new(RangeData {
            start,
            end,
            inclusive,
            _charge: charge,
        }))))
    }

    pub(crate) fn new_json(heap: &Heap, node: Json) -> Result<Value, Fault> {
        let meta = match &node {
            Json::Array(l) => l.meta(),
            Json::Object(entries) => Meta::of(entries.iter().flat_map(|(k, v)| [k, v])),
            Json::Str(s) => Meta {
                summary: Labels::NONE,
                weight: s.chars().max(1),
                depth: 0,
            },
            _ => Meta {
                summary: Labels::NONE,
                weight: 1,
                depth: 0,
            },
        }
        .check()?;
        let extra = match &node {
            Json::Object(e) => cost::SLOT * 2 * e.len() as u64,
            _ => 0,
        };
        let charge = heap.alloc(cost::JSON + extra)?;
        Ok(plain(Repr::Json(Arc::new(JsonData {
            node,
            meta,
            _charge: charge,
        }))))
    }

    pub(crate) fn new_prompt(heap: &Heap, sections: Vec<Section>) -> Result<Value, Fault> {
        let meta = Meta::of(sections.iter().flat_map(|s| s.values.iter())).check()?;
        // Each section carries its own charge.
        let charge = heap.alloc(cost::AGG)?;
        Ok(plain(Repr::Prompt(Arc::new(PromptData {
            sections: sections.into_boxed_slice(),
            meta,
            _charge: charge,
        }))))
    }

    pub(crate) fn item(index: u32) -> Value {
        plain(Repr::Item(index))
    }

    pub(crate) fn builtin(index: u16) -> Value {
        plain(Repr::Builtin(index))
    }

    pub(crate) fn tool(name: Str) -> Value {
        plain(Repr::Tool(name))
    }

    // ---- labels ----

    /// The value with `labels` added to its own labels (a container's reach everything read
    /// out of it). Labels are only ever added this way, never removed.
    #[must_use]
    pub fn with_labels(mut self, labels: Labels) -> Value {
        self.labels |= labels;
        self
    }

    /// The value's own labels.
    #[must_use]
    pub fn labels(&self) -> Labels {
        self.labels
    }

    /// The container's label summary (empty for scalars and text).
    #[must_use]
    pub fn summary(&self) -> Labels {
        self.meta().map_or(Labels::NONE, |m| m.summary)
    }

    /// Own labels and summary: a superset of every label in the value, in O(1).
    #[must_use]
    pub fn quick(&self) -> Labels {
        self.labels | self.summary()
    }

    /// Whether any part of the value may be labelled (the O(1) test).
    #[must_use]
    pub fn may_be_labelled(&self) -> bool {
        !self.quick().is_empty()
    }

    /// The union of the own labels of every part of the value, by a full walk.
    #[must_use]
    pub fn scan(&self) -> Labels {
        let mut out = self.labels;
        self.for_each_child(&mut |c| out |= c.scan());
        out
    }

    /// Whether every container inside the value has a summary covering its contents (the
    /// representation invariant; for tests and the debug-build sink check).
    #[must_use]
    pub fn summaries_hold(&self) -> bool {
        let mut inner = Labels::NONE;
        let mut ok = true;
        self.for_each_child(&mut |c| {
            inner |= c.scan();
            ok &= c.summaries_hold();
        });
        ok && self.summary().contains(inner)
    }

    /// Calls `f` on each direct part of the value.
    pub(crate) fn for_each_child(&self, f: &mut dyn FnMut(&Value)) {
        match &self.repr {
            Repr::Tuple(a) => a.items.iter().for_each(f),
            Repr::Record(r) => r.items.iter().for_each(f),
            Repr::List(l) => l.for_each(f),
            Repr::Map(m) => m.for_each(&mut |k, v| {
                f(k);
                f(v);
            }),
            Repr::VariantP(v) => f(&v.payload),
            Repr::Closure(c) => c.captured.iter().for_each(|(_, v)| f(v)),
            Repr::Json(j) => match &j.node {
                Json::Array(l) => l.for_each(f),
                Json::Object(e) => e.iter().for_each(|(k, v)| {
                    f(k);
                    f(v);
                }),
                _ => {}
            },
            Repr::Prompt(p) => p
                .sections
                .iter()
                .for_each(|s| s.values.iter().for_each(&mut *f)),
            _ => {}
        }
    }

    pub(crate) fn meta(&self) -> Option<Meta> {
        Some(match &self.repr {
            Repr::Tuple(a) => a.meta,
            Repr::Record(r) => r.meta,
            Repr::List(l) => l.meta(),
            Repr::Map(m) => m.meta(),
            Repr::VariantP(v) => v.meta,
            Repr::Closure(c) => c.meta,
            Repr::Json(j) => j.meta,
            Repr::Prompt(p) => p.meta,
            _ => return None,
        })
    }

    /// A bound on the work of walking the value: 1 for a scalar, the characters of text, one
    /// plus the elements' weights for a container.
    #[must_use]
    pub fn weight(&self) -> u64 {
        match &self.repr {
            Repr::Str(s) | Repr::Tool(s) => s.chars().max(1),
            _ => self.meta().map_or(1, |m| m.weight),
        }
    }

    pub(crate) fn depth(&self) -> u32 {
        self.meta().map_or(0, |m| m.depth)
    }

    /// The value with every label removed, everywhere in it: what `declassify` returns once
    /// the user has agreed. The only way a label leaves a value.
    pub(crate) fn declassified(&self, heap: &Heap) -> Result<Value, Fault> {
        let mut v = self.map_children(heap, &mut |c, heap| c.declassified(heap))?;
        v.labels = Labels::NONE;
        Ok(v)
    }

    /// A copy with each direct part replaced by `f`'s result.
    fn map_children(
        &self,
        heap: &Heap,
        f: &mut dyn FnMut(&Value, &Heap) -> Result<Value, Fault>,
    ) -> Result<Value, Fault> {
        let labels = self.labels;
        let mut out = match &self.repr {
            Repr::Tuple(a) => {
                let items = a
                    .items
                    .iter()
                    .map(|c| f(c, heap))
                    .collect::<Result<_, _>>()?;
                Value::new_tuple(heap, items)?
            }
            Repr::Record(r) => {
                let items = r
                    .items
                    .iter()
                    .map(|c| f(c, heap))
                    .collect::<Result<_, _>>()?;
                Value::new_record(heap, Arc::clone(&r.layout), items)?
            }
            Repr::List(l) => {
                let mut items = Vec::with_capacity(usize::try_from(l.len()).unwrap_or(0));
                let mut err = None;
                l.for_each(&mut |c| {
                    if err.is_none() {
                        match f(c, heap) {
                            Ok(v) => items.push(v),
                            Err(e) => err = Some(e),
                        }
                    }
                });
                if let Some(e) = err {
                    return Err(e);
                }
                Value::new_list(List::from_vec(heap, items)?)
            }
            Repr::Map(m) => {
                let mut out = Map::empty();
                let mut err = None;
                let mut pairs = Vec::new();
                m.for_each(&mut |k, v| pairs.push((k.clone(), v.clone())));
                for (k, v) in pairs {
                    let k = match f(&k, heap) {
                        Ok(k) => k,
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    };
                    let v = f(&v, heap)?;
                    out = out.insert(heap, k, v)?;
                }
                if let Some(e) = err {
                    return Err(e);
                }
                Value::new_map(out)
            }
            Repr::VariantP(v) => {
                Value::new_variant(heap, v.enum_, v.index, Some(f(&v.payload, heap)?))?
            }
            Repr::Closure(c) => {
                let captured = c
                    .captured
                    .iter()
                    .map(|(s, v)| Ok((*s, f(v, heap)?)))
                    .collect::<Result<_, Fault>>()?;
                Value::new_closure(heap, c.func, captured)?
            }
            Repr::Json(j) => {
                let node = match &j.node {
                    Json::Array(l) => {
                        let mut items = Vec::new();
                        let mut err = None;
                        l.for_each(&mut |c| {
                            if err.is_none() {
                                match f(c, heap) {
                                    Ok(v) => items.push(v),
                                    Err(e) => err = Some(e),
                                }
                            }
                        });
                        if let Some(e) = err {
                            return Err(e);
                        }
                        Json::Array(List::from_vec(heap, items)?)
                    }
                    Json::Object(e) => Json::Object(
                        e.iter()
                            .map(|(k, v)| Ok((f(k, heap)?, f(v, heap)?)))
                            .collect::<Result<Vec<_>, Fault>>()?
                            .into_boxed_slice(),
                    ),
                    Json::Null => Json::Null,
                    Json::Bool(b) => Json::Bool(*b),
                    Json::Int(i) => Json::Int(*i),
                    Json::Float(x) => Json::Float(*x),
                    Json::Str(s) => Json::Str(s.clone()),
                };
                Value::new_json(heap, node)?
            }
            Repr::Prompt(p) => {
                let mut sections = Vec::with_capacity(p.sections.len());
                for s in &p.sections {
                    let values = s
                        .values
                        .iter()
                        .map(|v| f(v, heap))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_boxed_slice();
                    sections.push(Section::new(heap, s.kind, s.format.clone(), values)?);
                }
                Value::new_prompt(heap, sections)?
            }
            _ => self.clone(),
        };
        out.labels = labels;
        Ok(out)
    }

    // ---- reading ----

    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self.repr {
            Repr::Int(i) => Some(i),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self.repr {
            Repr::Bool(b) => Some(b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self.repr {
            Repr::Float(f) => Some(f),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match &self.repr {
            Repr::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self.repr, Repr::None)
    }

    /// The elements of a list (or a JSON array), each with the list's own labels added.
    #[must_use]
    pub fn elements(&self) -> Option<Vec<Value>> {
        let l = match &self.repr {
            Repr::List(l) => l,
            Repr::Json(j) => match &j.node {
                Json::Array(l) => l,
                _ => return None,
            },
            _ => return None,
        };
        let mut out = Vec::new();
        l.for_each(&mut |v| out.push(v.clone().with_labels(self.labels)));
        Some(out)
    }

    /// The parts of a tuple, each with the tuple's own labels added.
    #[must_use]
    pub fn parts(&self) -> Option<Vec<Value>> {
        match &self.repr {
            Repr::Tuple(a) => Some(
                a.items
                    .iter()
                    .map(|v| v.clone().with_labels(self.labels))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// A record's field (with the record's own labels added); `None` when absent or not a
    /// record.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<Value> {
        match &self.repr {
            Repr::Record(r) => r
                .layout
                .index(name)
                .and_then(|i| r.items.get(i))
                .map(|v| v.clone().with_labels(self.labels)),
            _ => None,
        }
    }

    /// The entries of a map, in key order, each with the map's own labels added.
    #[must_use]
    pub fn entries(&self) -> Option<Vec<(Value, Value)>> {
        match &self.repr {
            Repr::Map(m) => {
                let mut out = Vec::new();
                m.for_each(&mut |k, v| {
                    out.push((
                        k.clone().with_labels(self.labels),
                        v.clone().with_labels(self.labels),
                    ));
                });
                Some(out)
            }
            _ => None,
        }
    }

    /// A short name of the value's kind, for messages.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match &self.repr {
            Repr::Unit => "()",
            Repr::None => "none",
            Repr::Bool(_) => "Bool",
            Repr::Int(_) => "Int",
            Repr::Float(_) => "Float",
            Repr::Duration(_) => "Duration",
            Repr::Money(_) => "Money",
            Repr::Str(_) => "Str",
            Repr::Range(_) => "range",
            Repr::Tuple(_) => "tuple",
            Repr::Record(_) => "record",
            Repr::List(_) => "list",
            Repr::Map(_) => "map",
            Repr::Variant(..) | Repr::VariantP(_) => "variant",
            Repr::Item(_) | Repr::Builtin(_) | Repr::Closure(_) => "function",
            Repr::Json(_) => "Json",
            Repr::Prompt(_) => "Prompt",
            Repr::Tool(_) => "ToolName",
        }
    }
}

// Dropping a payload frees its children through the work-list of [`crate::drop`], so freeing
// a value never recurses, whatever its shape.

impl Drop for Agg {
    fn drop(&mut self) {
        crate::drop::drop_values(&mut self.items);
    }
}

impl Drop for RecordData {
    fn drop(&mut self) {
        crate::drop::drop_values(&mut self.items);
    }
}

impl Drop for VariantData {
    fn drop(&mut self) {
        if self.payload.meta().is_some() {
            crate::drop::dispose(vec![crate::drop::Garbage::Value(std::mem::take(
                &mut self.payload,
            ))]);
        }
    }
}

impl Drop for ClosureData {
    fn drop(&mut self) {
        if self.captured.iter().any(|(_, v)| v.meta().is_some()) {
            let captured = std::mem::take(&mut self.captured);
            crate::drop::dispose(
                captured
                    .into_vec()
                    .into_iter()
                    .map(|(_, v)| crate::drop::Garbage::Value(v))
                    .collect(),
            );
        }
    }
}

impl Drop for JsonData {
    fn drop(&mut self) {
        if matches!(self.node, Json::Array(_) | Json::Object(_)) {
            let mut stack = Vec::new();
            crate::drop::take_json(&mut self.node, &mut stack);
            crate::drop::dispose(stack);
        }
    }
}

impl Drop for PromptData {
    fn drop(&mut self) {
        let mut stack = Vec::new();
        for s in &mut self.sections {
            crate::drop::take_values(&mut s.values, &mut stack);
        }
        crate::drop::dispose(stack);
    }
}
