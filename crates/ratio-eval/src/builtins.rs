//! The built-in functions stage 6 implements, and the cost-class charging framework of
//! chapter 06 (`data`, "Cost classes and representations").
//!
//! Each pure built-in has a [`Class`] and is charged `1 + ⌈w / 64⌉` steps, where `w` is the
//! class's work over the sizes of its inputs (characters of text, elements of lists, entries
//! of maps, nodes of JSON) and of its output. The part that follows from the arguments is
//! debited **before** the built-in runs, so one the steps left cannot pay for raises
//! `Budget { which: steps }` without running; a `linear` built-in's output part is debited
//! when it returns (the machine does both, [`crate::machine`]). Callbacks charge their own
//! steps as any code does.
//!
//! Stage 6 carried the framework and one or more built-ins of each class; stage 7 adds every
//! `text` built-in (`text`) and the `re` built-ins (`regex`) over a linear-time engine
//! (`crate::re`). Stages 8 and 9 add the rest of chapter 06 to this table.
//!
//! A built-in's working memory (a regex VM's thread lists, `text.similarity`'s rows) is part
//! of its sizes and charged to the heap before it runs, with its steps; a pure built-in
//! without callbacks whose input is large may run on the pool ([`crate::Pool`]) instead of
//! inline, with the same result, labels and charges.

use std::fmt::Debug;

// The BLAKE3 hash (tape blob names, digests); the same file as stage 9's `hash` module uses.
pub(crate) mod blake3;
mod ecma;
mod json;
mod list;
mod map;
mod regex;
mod text;
mod util;
mod validate;

pub(crate) use regex::{Compiled, ReCache};

use crate::error::{Kind, RtError, raise, raise_with};
use crate::heap::{Charge, Heap, cost};
use crate::labels::Labels;
use crate::machine::Cx;
use crate::ops::{cmp_natural, eq, size};
use crate::plist::{Cursor, List};
use crate::prepare::Prepared;
use crate::value::{Fault, Json, Repr, Value};
use ratio_ir::Loc;

/// A pure built-in's cost class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `w = 1`.
    Const,
    /// `w = ⌈log2(n + 1)⌉`.
    Log,
    /// `w = n · ⌈log2(n + 1)⌉`.
    NLogN,
    /// `w = n₁ · n₂ + c₁ + c₂`.
    Product,
    /// `w = m`, the output size the arguments fix.
    Output,
    /// `w = n + m`; the output part is debited on return.
    Linear,
}

impl Class {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Class::Const => "const",
            Class::Log => "log",
            Class::NLogN => "nlogn",
            Class::Product => "product",
            Class::Output => "output",
            Class::Linear => "linear",
        }
    }
}

/// The input sizes a charge is computed from: `n` (the input size, or `n₁`), `n2` and `c`
/// for `product`, and the output size `m` for `output`; and the bytes of working memory
/// charged to the heap before the built-in runs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Sizes {
    pub n: u64,
    pub n2: u64,
    pub c: u64,
    pub m: u64,
    pub scratch: u64,
}

impl Sizes {
    fn n(n: u64) -> Sizes {
        Sizes {
            n,
            ..Sizes::default()
        }
    }
}

/// The work charged before running.
pub(crate) fn pre_work(class: Class, s: Sizes) -> u64 {
    match class {
        Class::Const => 1,
        // A map's key comparisons walk the key: `n2` is its weight (0 when there is none).
        Class::Log => crate::ops::log_work(s.n).saturating_mul(s.n2.max(1)),
        // A comparison walks the values it compares: built-ins that compare whole elements give
        // their weight in `n2`, and the charge is that weight times `⌈log2(n + 1)⌉`.
        Class::NLogN => s.n.max(s.n2).saturating_mul(crate::ops::log_work(s.n)),
        Class::Product => s.n.saturating_mul(s.n2).saturating_add(s.c),
        Class::Output => s.m,
        Class::Linear => s.n,
    }
}

/// What a callback-taking built-in is told when it resumes.
#[derive(Debug)]
pub(crate) enum Resume {
    Start,
    Value(Value),
    /// The callback raised (only for tasks that catch).
    Raised(RtError),
    /// The steps a [`TaskStep::Charge`] asked for were debited.
    Charged,
}

/// What a callback-taking built-in asks for next.
#[derive(Debug)]
pub(crate) enum TaskStep {
    /// Call a function value with these arguments and resume with its result.
    Call(Value, Vec<Value>),
    Done(Value),
    /// Debit these steps (a charge known only once the callbacks have run), then resume with
    /// [`Resume::Charged`].
    Charge(u64),
}

/// A built-in that calls back into the program: a small state machine the evaluator drives,
/// so the program's code runs on the evaluator's own (heap-allocated) stack.
pub(crate) trait Task: Send + Debug {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError>;
    /// Whether a catchable error the callback raises is given to the task.
    fn catches(&self) -> bool {
        false
    }
}

type Args = [Option<Value>];
pub(crate) type PureFn = fn(&mut Cx<'_>, &Args) -> Result<Value, RtError>;
type TaskFn = fn(&mut Cx<'_>, &Args) -> Result<Box<dyn Task>, RtError>;

/// How a built-in's inputs are sized: from the arguments, or (`re`) from the arguments and
/// the compiled pattern.
#[derive(Clone, Copy)]
pub(crate) enum Sizer {
    Args(fn(&Args) -> Sizes),
    Re(fn(&Args, &crate::re::Regex) -> Sizes),
}

#[derive(Clone, Copy)]
pub(crate) enum Imp {
    Pure(PureFn),
    Task(TaskFn),
}

/// One built-in: its name as a program writes it, its class, how to size its inputs, what
/// else the machine must know ([`OFFLOAD`], [`REGEX`]), and its implementation.
#[derive(Clone, Copy)]
pub(crate) struct Builtin {
    pub name: &'static str,
    pub class: Class,
    pub size: Sizer,
    pub flags: u8,
    pub imp: Imp,
}

/// May run on the pool when its input is large (pure, no callbacks).
const OFFLOAD: u8 = 1;
/// A `re` built-in: its first argument is a pattern, compiled before it is charged.
const REGEX: u8 = 2 | OFFLOAD;
/// A `re` built-in that iterates over matches, debiting its later work as it goes: with a
/// pool, it is offloaded whenever the steps left could pay for a long run.
const REGEX_ITER: u8 = 4 | REGEX;
const NONE: u8 = 0;

impl Builtin {
    pub(crate) fn offloadable(&self) -> bool {
        self.flags & OFFLOAD != 0 && matches!(self.imp, Imp::Pure(_))
    }

    pub(crate) fn regex(&self) -> bool {
        self.flags & REGEX == REGEX
    }

    /// Whether it debits work as it goes (beyond what was charged before running).
    pub(crate) fn debits_as_it_goes(&self) -> bool {
        self.flags & REGEX_ITER == REGEX_ITER
    }

    /// The sizes of a call with these arguments (and pattern, for `re`). A pattern that did
    /// not compile is charged the work its refusal took: the built-in raises `Invalid`.
    pub(crate) fn sizes(&self, args: &Args, re: Option<&Compiled>) -> Sizes {
        match (self.size, re) {
            (Sizer::Args(f), _) => f(args),
            (Sizer::Re(f), Some(Ok(re))) => f(args, re),
            // A refused pattern is charged the work it took to refuse.
            (Sizer::Re(_), Some(Err(refusal))) => Sizes {
                c: refusal.work,
                ..Sizes::default()
            },
            (Sizer::Re(_), None) => Sizes::default(),
        }
    }
}

impl Debug for Builtin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

// ---- sizing ----

fn sz(args: &Args, i: usize) -> u64 {
    args.get(i).and_then(Option::as_ref).map_or(0, size)
}

fn weight(args: &Args, i: usize) -> u64 {
    args.get(i)
        .and_then(Option::as_ref)
        .map_or(0, Value::weight)
}

fn s_none(_: &Args) -> Sizes {
    Sizes::default()
}
fn s_first(a: &Args) -> Sizes {
    Sizes::n(sz(a, 0))
}
fn s_two(a: &Args) -> Sizes {
    Sizes::n(sz(a, 0).saturating_add(sz(a, 1)))
}
fn s_weight_first(a: &Args) -> Sizes {
    Sizes::n(weight(a, 0))
}
fn s_weight_two(a: &Args) -> Sizes {
    Sizes::n(weight(a, 0).saturating_add(weight(a, 1)))
}
/// A map lookup or update: the entries, and the weight of the key every comparison walks.
fn s_map_key(a: &Args) -> Sizes {
    Sizes {
        n: sz(a, 0),
        n2: weight(a, 1),
        ..Sizes::default()
    }
}
fn s_three(a: &Args) -> Sizes {
    Sizes::n(sz(a, 0).saturating_add(sz(a, 1)).saturating_add(sz(a, 2)))
}
fn s_first_third(a: &Args) -> Sizes {
    Sizes::n(sz(a, 0).saturating_add(sz(a, 2)))
}
fn s_join(a: &Args) -> Sizes {
    Sizes::n(weight(a, 0).saturating_add(sz(a, 1)))
}
/// `text.repeat`: the output is `|s| · n` characters, fixed by the arguments. A count that is
/// negative, or a result that would overflow or pass 16 MiB, is charged nothing here: the
/// built-in refuses it (`Invalid`) before allocating anything.
fn s_repeat(a: &Args) -> Sizes {
    let s = a.first().and_then(Option::as_ref);
    let count = a.get(1).and_then(Option::as_ref).and_then(Value::as_int);
    let (Some(s), Some(count)) = (s, count) else {
        return Sizes::default();
    };
    let Ok(count) = u64::try_from(count) else {
        return Sizes::default();
    };
    let bytes = s.as_text().map_or(0, str::len) as u64;
    match bytes.checked_mul(count) {
        Some(total) if total <= crate::value::MAX_STR_BYTES as u64 => Sizes {
            m: size(s).saturating_mul(count),
            ..Sizes::default()
        },
        _ => Sizes::default(),
    }
}
/// `text.similarity`: the product of the lengths plus both, and two rows of counts over the
/// shorter text with its characters as working memory.
fn s_product(a: &Args) -> Sizes {
    let (n1, n2) = (sz(a, 0), sz(a, 1));
    Sizes {
        n: n1,
        n2,
        c: n1.saturating_add(n2),
        m: 0,
        scratch: n1
            .min(n2)
            .saturating_add(1)
            .saturating_mul(SIMILARITY_CELL_BYTES),
    }
}

/// The working memory per character of the shorter text in `text.similarity`: the character
/// and a count in each of two rows.
const SIMILARITY_CELL_BYTES: u64 = 4 + 2 * 8;

/// The output size of a `linear` built-in's result.
pub(crate) fn output_size(v: &Value) -> u64 {
    match &v.repr {
        Repr::Tuple(_) | Repr::Json(_) => v.weight(),
        _ => size(v),
    }
}

// ---- the table ----

macro_rules! table {
    ($($name:literal, $class:ident, $size:expr, $flags:ident, $imp:expr;)*) => {
        pub(crate) const BUILTINS: &[Builtin] = &[
            $(Builtin { name: $name, class: Class::$class, size: $size, flags: $flags, imp: $imp },)*
        ];
    };
}

use Sizer::{Args as A, Re as R};
use text as t;

table! {
    "fail", Const, A(s_none), NONE, Imp::Pure(fail);
    "result", Const, A(s_none), NONE, Imp::Task(result);
    "assert", Const, A(s_none), NONE, Imp::Pure(assert);
    "assert_eq", Linear, A(s_weight_two), NONE, Imp::Pure(assert_eq);
    "list.len", Const, A(s_none), NONE, Imp::Pure(list_len);
    "list.get", Log, A(s_first), NONE, Imp::Pure(list_get);
    "list.first", Const, A(s_none), NONE, Imp::Pure(list_first);
    "list.push", Log, A(s_first), NONE, Imp::Pure(list_push);
    "list.map", Linear, A(s_first), NONE, Imp::Task(list_map);
    "list.filter", Linear, A(s_first), NONE, Imp::Task(list_filter);
    "list.filter_map", Linear, A(s_first), NONE, Imp::Task(list_filter_map);
    "list.fold", Linear, A(s_first), NONE, Imp::Task(list_fold);
    "list.contains", Linear, A(s_weight_two), NONE, Imp::Pure(list_contains);
    "list.sort_by", NLogN, A(s_first), NONE, Imp::Task(list_sort_by);
    "list.take", Log, A(s_first), NONE, Imp::Pure(list_take);
    "list.sum", Linear, A(s_first), NONE, Imp::Pure(list_sum);
    "text.len", Const, A(s_none), NONE, Imp::Pure(text_len);
    "text.is_empty", Const, A(s_none), NONE, Imp::Pure(t::is_empty);
    "text.lower", Linear, A(s_first), OFFLOAD, Imp::Pure(t::lower);
    "text.upper", Linear, A(s_first), OFFLOAD, Imp::Pure(text_upper);
    "text.trim", Linear, A(s_first), OFFLOAD, Imp::Pure(text_trim);
    "text.trim_start", Linear, A(s_first), OFFLOAD, Imp::Pure(t::trim_start);
    "text.trim_end", Linear, A(s_first), OFFLOAD, Imp::Pure(t::trim_end);
    "text.contains", Linear, A(s_two), OFFLOAD, Imp::Pure(text_contains);
    "text.starts_with", Linear, A(s_two), OFFLOAD, Imp::Pure(t::starts_with);
    "text.ends_with", Linear, A(s_two), OFFLOAD, Imp::Pure(t::ends_with);
    "text.find", Linear, A(s_two), OFFLOAD, Imp::Pure(t::find);
    "text.rfind", Linear, A(s_two), OFFLOAD, Imp::Pure(t::rfind);
    "text.count", Linear, A(s_two), OFFLOAD, Imp::Pure(t::count);
    "text.slice", Linear, A(s_first), OFFLOAD, Imp::Pure(t::slice);
    "text.truncate", Linear, A(s_first_third), OFFLOAD, Imp::Pure(t::truncate);
    "text.split", Linear, A(s_two), OFFLOAD, Imp::Pure(t::split);
    "text.split_once", Linear, A(s_two), OFFLOAD, Imp::Pure(text_split_once);
    "text.lines", Linear, A(s_first), OFFLOAD, Imp::Pure(t::lines);
    "text.words", Linear, A(s_first), OFFLOAD, Imp::Pure(t::words);
    "text.chars", Linear, A(s_first), OFFLOAD, Imp::Pure(t::chars);
    "text.join", Linear, A(s_join), OFFLOAD, Imp::Pure(text_join);
    "text.replace", Linear, A(s_three), OFFLOAD, Imp::Pure(t::replace);
    "text.replace_first", Linear, A(s_three), OFFLOAD, Imp::Pure(t::replace_first);
    "text.repeat", Output, A(s_repeat), OFFLOAD, Imp::Pure(text_repeat);
    "text.reverse", Linear, A(s_first), OFFLOAD, Imp::Pure(t::reverse);
    "text.pad_start", Output, A(t::s_pad), OFFLOAD, Imp::Pure(t::pad_start);
    "text.pad_end", Output, A(t::s_pad), OFFLOAD, Imp::Pure(t::pad_end);
    "text.before", Linear, A(s_two), OFFLOAD, Imp::Pure(t::before);
    "text.after", Linear, A(s_two), OFFLOAD, Imp::Pure(t::after);
    "text.before_last", Linear, A(s_two), OFFLOAD, Imp::Pure(t::before_last);
    "text.after_last", Linear, A(s_two), OFFLOAD, Imp::Pure(t::after_last);
    "text.strip_prefix", Linear, A(s_two), OFFLOAD, Imp::Pure(t::strip_prefix);
    "text.strip_suffix", Linear, A(s_two), OFFLOAD, Imp::Pure(t::strip_suffix);
    "text.trim_end_matches", Linear, A(s_two), OFFLOAD, Imp::Pure(t::trim_end_matches);
    "text.dedent", Linear, A(s_first), OFFLOAD, Imp::Pure(t::dedent);
    "text.indent", Linear, A(s_two), OFFLOAD, Imp::Pure(t::indent);
    "text.wrap", Linear, A(s_first), OFFLOAD, Imp::Pure(t::wrap);
    "text.number_lines", Linear, A(s_first), OFFLOAD, Imp::Pure(t::number_lines);
    "text.show", Linear, A(s_weight_first), OFFLOAD, Imp::Pure(text_show);
    "text.parse_int", Linear, A(s_first), OFFLOAD, Imp::Pure(t::parse_int);
    "text.parse_float", Linear, A(s_first), OFFLOAD, Imp::Pure(t::parse_float);
    "text.similarity", Product, A(s_product), OFFLOAD, Imp::Pure(text_similarity);
    "text.extract_json", Linear, A(s_first), OFFLOAD, Imp::Pure(t::extract_json);
    "text.code_blocks", Linear, A(s_two), OFFLOAD, Imp::Pure(t::code_blocks);
    "text.quote", Linear, A(s_two), OFFLOAD, Imp::Pure(t::quote);
    "re.is_match", Product, R(regex::s_is_match), REGEX, Imp::Pure(regex::is_match);
    "re.find", Product, R(regex::s_find), REGEX, Imp::Pure(regex::find);
    "re.find_all", Product, R(regex::s_find), REGEX_ITER, Imp::Pure(regex::find_all);
    "re.captures", Product, R(regex::s_captures), REGEX, Imp::Pure(regex::captures);
    "re.captures_all", Product, R(regex::s_captures), REGEX_ITER, Imp::Pure(regex::captures_all);
    "re.replace", Product, R(regex::s_replace), REGEX, Imp::Pure(regex::replace);
    "re.replace_all", Product, R(regex::s_replace), REGEX_ITER, Imp::Pure(regex::replace_all);
    "re.split", Product, R(regex::s_find), REGEX_ITER, Imp::Pure(regex::split);
    "re.escape", Linear, A(s_first), OFFLOAD, Imp::Pure(regex::escape);
    "map.get", Log, A(s_map_key), NONE, Imp::Pure(map_get);
    "map.set", Log, A(s_map_key), NONE, Imp::Pure(map_set);
    "map.keys", Linear, A(s_first), NONE, Imp::Pure(map_keys);
    "json.parse", Linear, A(s_first), OFFLOAD, Imp::Pure(json_parse);
    "json.decode", Linear, A(s_weight_first), NONE, Imp::Pure(json_decode);
    "json.get", Linear, A(s_two), NONE, Imp::Pure(json_get);
    "json.encode", Linear, A(s_weight_first), NONE, Imp::Pure(json_encode);
    // ---- stage 8 (data II): the rest of json, list and map (chapter 06, data) ----
    "list.is_empty", Const, A(s_none), OFFLOAD, Imp::Pure(list::is_empty);
    "list.last", Const, A(s_none), OFFLOAD, Imp::Pure(list::last);
    "list.insert", Log, A(s_first), OFFLOAD, Imp::Pure(list::insert);
    "list.remove_at", Log, A(s_first), OFFLOAD, Imp::Pure(list::remove_at);
    "list.concat", Log, A(s_two), OFFLOAD, Imp::Pure(list::concat);
    "list.flatten", Linear, A(s_weight_first), OFFLOAD, Imp::Pure(list::flatten);
    "list.range", Output, A(list::s_range), OFFLOAD, Imp::Pure(list::range);
    "list.repeat", Output, A(list::s_repeat), OFFLOAD, Imp::Pure(list::repeat);
    "list.map_indexed", Linear, A(s_first), NONE, Imp::Task(list::map_indexed);
    "list.flat_map", Linear, A(s_first), NONE, Imp::Task(list::flat_map);
    "list.reduce", Linear, A(s_first), NONE, Imp::Task(list::reduce);
    "list.find", Linear, A(s_first), NONE, Imp::Task(list::find);
    "list.find_index", Linear, A(s_first), NONE, Imp::Task(list::find_index);
    "list.index_of", Linear, A(s_weight_two), OFFLOAD, Imp::Pure(list::index_of);
    "list.any", Linear, A(s_first), NONE, Imp::Task(list::any);
    "list.all", Linear, A(s_first), NONE, Imp::Task(list::all);
    "list.count", Linear, A(s_first), NONE, Imp::Task(list::count);
    "list.min", Linear, A(s_weight_first), OFFLOAD, Imp::Pure(list::min);
    "list.max", Linear, A(s_weight_first), OFFLOAD, Imp::Pure(list::max);
    "list.min_by", Linear, A(s_first), NONE, Imp::Task(list::min_by);
    "list.max_by", Linear, A(s_first), NONE, Imp::Task(list::max_by);
    "list.sort", NLogN, A(list::s_compare), OFFLOAD, Imp::Pure(list::sort);
    "list.reverse", Linear, A(s_first), OFFLOAD, Imp::Pure(list::reverse);
    "list.unique", NLogN, A(list::s_compare), OFFLOAD, Imp::Pure(list::unique);
    "list.unique_by", NLogN, A(s_first), NONE, Imp::Task(list::unique_by);
    "list.group_by", NLogN, A(s_first), NONE, Imp::Task(list::group_by);
    "list.partition", Linear, A(s_first), NONE, Imp::Task(list::partition);
    "list.chunk", Linear, A(s_first), OFFLOAD, Imp::Pure(list::chunk);
    "list.window", Linear, A(list::s_window), OFFLOAD, Imp::Pure(list::window);
    "list.zip", Linear, A(s_two), OFFLOAD, Imp::Pure(list::zip);
    "list.unzip", Linear, A(s_first), OFFLOAD, Imp::Pure(list::unzip);
    "list.enumerate", Linear, A(s_first), OFFLOAD, Imp::Pure(list::enumerate);
    "list.drop", Log, A(s_first), OFFLOAD, Imp::Pure(list::take_rest);
    "list.take_while", Linear, A(s_first), NONE, Imp::Task(list::take_while);
    "list.slice", Log, A(s_first), OFFLOAD, Imp::Pure(list::slice);
    "map.from", NLogN, A(list::s_compare), OFFLOAD, Imp::Pure(map::from);
    "map.len", Const, A(s_none), OFFLOAD, Imp::Pure(map::len);
    "map.is_empty", Const, A(s_none), OFFLOAD, Imp::Pure(map::is_empty);
    "map.has", Log, A(s_map_key), OFFLOAD, Imp::Pure(map::has);
    "map.remove", Log, A(s_map_key), OFFLOAD, Imp::Pure(map::remove);
    "map.update", Log, A(s_map_key), NONE, Imp::Task(map::update);
    "map.values", Linear, A(s_first), OFFLOAD, Imp::Pure(map::values);
    "map.entries", Linear, A(s_first), OFFLOAD, Imp::Pure(map::entries);
    "map.merge", Linear, A(s_weight_two), OFFLOAD, Imp::Pure(map::merge);
    "map.map_values", Linear, A(s_first), NONE, Imp::Task(map::map_values);
    "map.filter", Linear, A(s_first), NONE, Imp::Task(map::filter);
    "map.count_by", NLogN, A(s_first), NONE, Imp::Task(map::count_by);
    "json.stringify", Linear, A(s_weight_first), OFFLOAD, Imp::Pure(json::stringify);
    "json.schema", Linear, A(s_none), OFFLOAD, Imp::Pure(json::schema);
    "json.validate", Product, A(s_product), OFFLOAD, Imp::Pure(json::validate_);
    "json.set", Linear, A(s_two), OFFLOAD, Imp::Pure(json::set);
    "json.keys", Linear, A(s_first), OFFLOAD, Imp::Pure(json::keys);
    "json.type_of", Const, A(s_none), OFFLOAD, Imp::Pure(json::type_of);
    "json.merge", Linear, A(s_weight_two), OFFLOAD, Imp::Pure(json::merge);
    "json.patch", Product, A(json::s_patch), OFFLOAD, Imp::Pure(json::patch);
    "json.diff", Product, A(s_product), OFFLOAD, Imp::Pure(json::diff_);
    "json.equal", Linear, A(s_weight_two), OFFLOAD, Imp::Pure(json::equal);
    // ---- end of stage 8 ----
    "math.abs", Const, A(s_none), NONE, Imp::Pure(math_abs);
    "math.min", Const, A(s_none), NONE, Imp::Pure(math_min);
    "math.max", Const, A(s_none), NONE, Imp::Pure(math_max);
    // ---- frames (stage 10) ----
    "question", Const, A(s_none), NONE, Imp::Pure(crate::frame::meta::question);
}

/// The table index of the built-in a program calls `name`.
pub(crate) fn lookup(name: &str) -> Option<u16> {
    BUILTINS
        .iter()
        .position(|b| b.name == name)
        .and_then(|i| u16::try_from(i).ok())
}

/// Every built-in this stage implements, with its cost class (for documentation and tests).
#[must_use]
pub fn implemented() -> Vec<(&'static str, Class)> {
    BUILTINS.iter().map(|b| (b.name, b.class)).collect()
}

// ---- helpers ----

fn arg<'a>(cx: &Cx<'_>, args: &'a Args, i: usize) -> Result<&'a Value, RtError> {
    args.get(i).and_then(Option::as_ref).ok_or_else(|| {
        raise(
            cx.p,
            Kind::Invalid,
            "a built-in was called without a required argument",
            Labels::NONE,
            cx.loc,
        )
    })
}

fn opt(args: &Args, i: usize) -> Option<&Value> {
    args.get(i).and_then(Option::as_ref)
}

fn text<'a>(cx: &Cx<'_>, v: &'a Value) -> Result<&'a str, RtError> {
    v.as_text()
        .ok_or_else(|| raise(cx.p, Kind::Invalid, "expected text", v.labels, cx.loc))
}

fn int(cx: &Cx<'_>, v: &Value) -> Result<i64, RtError> {
    v.as_int()
        .ok_or_else(|| raise(cx.p, Kind::Invalid, "expected an Int", v.labels, cx.loc))
}

fn list<'a>(cx: &Cx<'_>, v: &'a Value) -> Result<&'a List, RtError> {
    match &v.repr {
        Repr::List(l) => Ok(l),
        _ => Err(raise(
            cx.p,
            Kind::Invalid,
            "expected a list",
            v.labels,
            cx.loc,
        )),
    }
}

fn map<'a>(cx: &Cx<'_>, v: &'a Value) -> Result<&'a crate::pmap::Map, RtError> {
    match &v.repr {
        Repr::Map(m) => Ok(m),
        _ => Err(raise(
            cx.p,
            Kind::Invalid,
            "expected a map",
            v.labels,
            cx.loc,
        )),
    }
}

fn new_text(cx: &Cx<'_>, s: String, labels: Labels) -> Result<Value, RtError> {
    Value::new_text(cx.heap, s)
        .map(|v| v.with_labels(labels))
        .map_err(|f| cx.fault(f, labels))
}

fn new_list(cx: &Cx<'_>, items: Vec<Value>, own: Labels) -> Result<Value, RtError> {
    List::from_vec(cx.heap, items)
        .map(|l| Value::new_list(l).with_labels(own))
        .map_err(|f| cx.fault(f, own))
}

/// `Result.Ok { value }` or `Result.Err { error }`.
fn result_value(cx: &Cx<'_>, ok: bool, v: Value) -> Result<Value, RtError> {
    result_variant(cx.p, cx.heap, ok, v, cx.loc)
}

/// `Result.Ok { value }` or `Result.Err { error }`, charged to `heap`.
pub(crate) fn result_variant(
    p: &Prepared,
    heap: &Heap,
    ok: bool,
    v: Value,
    loc: Loc,
) -> Result<Value, RtError> {
    let e = p.rt.result;
    let index = p.enums[e as usize]
        .variant(if ok { "Ok" } else { "Err" })
        .unwrap_or(0);
    let layout = p.layout_of(e, index).cloned().ok_or_else(|| {
        raise(
            p,
            Kind::Invalid,
            "the prelude's Result has no payload",
            Labels::NONE,
            loc,
        )
    })?;
    let fault = |f| crate::machine::fault_pub(p, f, Labels::NONE, loc);
    let payload = Value::new_record(heap, layout, vec![v]).map_err(fault)?;
    Value::new_variant(heap, e, index, p.variant_weight(e, index), Some(payload)).map_err(fault)
}

// ---- control ----

fn fail(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let tag = arg(cx, args, 0)?;
    let message = arg(cx, args, 1)?.clone().with_labels(tag.labels);
    let data = opt(args, 2).cloned();
    Ok(crate::error::error_value(
        cx.p,
        Kind::User(text(cx, tag)?, tag.labels),
        message,
        data,
        cx.loc,
    ))
}

fn assert(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let cond = arg(cx, args, 0)?;
    if cond.as_bool() == Some(true) {
        return Ok(Value::unit());
    }
    let message = opt(args, 1)
        .cloned()
        .unwrap_or_else(|| Value::text("assertion failed"))
        .with_labels(cond.labels);
    Err(raise_with(cx.p, Kind::Assertion, message, None, cx.loc))
}

fn assert_eq(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    if eq(a, b) {
        return Ok(Value::unit());
    }
    let labels = a.quick() | b.quick();
    let message = match opt(args, 2) {
        Some(m) if !m.is_none() => m.clone().with_labels(labels),
        _ => Value::text("assertion failed: the values differ").with_labels(labels),
    };
    Err(raise_with(cx.p, Kind::Assertion, message, None, cx.loc))
}

#[derive(Debug)]
struct ResultTask {
    f: Value,
}

impl Task for ResultTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        match input {
            Resume::Start => Ok(TaskStep::Call(self.f.clone(), Vec::new())),
            Resume::Value(v) => Ok(TaskStep::Done(result_value(cx, true, v)?)),
            Resume::Raised(e) => Ok(TaskStep::Done(result_value(cx, false, e.value)?)),
            Resume::Charged => Err(raise(
                cx.p,
                Kind::Invalid,
                "a task resumed out of order",
                Labels::NONE,
                cx.loc,
            )),
        }
    }

    fn catches(&self) -> bool {
        true
    }
}

fn result(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    Ok(Box::new(ResultTask {
        f: arg(cx, args, 0)?.clone(),
    }))
}

// ---- list ----

fn list_len(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let n = i64::try_from(list(cx, xs)?.len()).unwrap_or(i64::MAX);
    Ok(Value::int(n).with_labels(xs.labels))
}

fn list_get(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, i) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let l = list(cx, xs)?;
    let labels = xs.labels | i.labels;
    let idx = int(cx, i)?;
    let len = i64::try_from(l.len()).unwrap_or(i64::MAX);
    let at = if idx < 0 {
        len.checked_add(idx)
    } else {
        Some(idx)
    };
    Ok(
        match at
            .and_then(|a| u64::try_from(a).ok())
            .and_then(|a| l.get(a))
        {
            Some(v) => v.clone().with_labels(labels),
            None => Value::none().with_labels(labels),
        },
    )
}

fn list_first(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    Ok(match list(cx, xs)?.get(0) {
        Some(v) => v.clone().with_labels(xs.labels),
        None => Value::none().with_labels(xs.labels),
    })
}

fn list_push(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, x) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let l = list(cx, xs)?
        .push(cx.heap, x.clone())
        .map_err(|f| cx.fault(f, xs.labels))?;
    Ok(Value::new_list(l).with_labels(xs.labels))
}

fn list_contains(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, x) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let mut found = false;
    list(cx, xs)?.for_each(&mut |v| found = found || eq(v, x));
    Ok(Value::bool(found).with_labels(xs.quick() | x.quick()))
}

fn list_take(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (xs, n) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let count = u64::try_from(int(cx, n)?).unwrap_or(0);
    let labels = xs.labels | n.labels;
    let l = list(cx, xs)?
        .slice(cx.heap, 0, count)
        .map_err(|f| cx.fault(f, labels))?;
    Ok(Value::new_list(l).with_labels(labels))
}

fn list_sum(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let xs = arg(cx, args, 0)?;
    let labels = xs.quick();
    let zero = match cx
        .type_args
        .first()
        .and_then(|t| cx.p.types.get(*t as usize))
    {
        Some(ratio_ir::Type::Float) => Value::float(0.0),
        Some(ratio_ir::Type::Duration) => Value::duration(0),
        Some(ratio_ir::Type::Money) => Value::money(0),
        _ => Value::int(0),
    };
    let mut acc = zero;
    let mut err = None;
    list(cx, xs)?.for_each(&mut |v| {
        if err.is_none() {
            match crate::ops::arith(ratio_ir::BinOp::Add, &acc, v) {
                Ok(s) => acc = s,
                Err(e) => err = Some(e),
            }
        }
    });
    if let Some(e) = err {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            &e.message(ratio_ir::BinOp::Add),
            labels,
            cx.loc,
        ));
    }
    Ok(acc.with_labels(labels))
}

/// Shared state of the tasks that walk a list calling `f` on each element.
#[derive(Debug)]
struct Walk {
    items: Cursor,
    f: Value,
    /// The list's own labels: added to every element read.
    own: Labels,
}

impl Walk {
    fn new(cx: &Cx<'_>, args: &Args) -> Result<Walk, RtError> {
        let xs = arg(cx, args, 0)?;
        let f = arg(cx, args, 1)?.clone();
        Ok(Walk {
            items: list(cx, xs)?.cursor(),
            f,
            own: xs.labels,
        })
    }

    fn next(&mut self) -> Option<Value> {
        self.items.next().map(|v| v.with_labels(self.own))
    }
}

/// A result being gathered by a built-in, charged to the heap as it grows (one slot per
/// element) and given back when dropped.
#[derive(Debug)]
struct Buf {
    items: Vec<Value>,
    charge: Charge,
}

impl Buf {
    fn new() -> Buf {
        Buf {
            items: Vec::new(),
            charge: Charge::none(),
        }
    }

    fn push(&mut self, cx: &Cx<'_>, v: Value) -> Result<(), RtError> {
        self.charge
            .grow(cx.heap, cost::SLOT)
            .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
        self.items.push(v);
        Ok(())
    }

    fn take(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.items)
    }
}

#[derive(Debug)]
struct MapTask {
    walk: Walk,
    out: Buf,
}

impl Task for MapTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(v) = input {
            self.out.push(cx, v)?;
        }
        match self.walk.next() {
            Some(x) => Ok(TaskStep::Call(self.walk.f.clone(), vec![x])),
            None => Ok(TaskStep::Done(new_list(
                cx,
                self.out.take(),
                self.walk.own,
            )?)),
        }
    }
}

fn list_map(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    Ok(Box::new(MapTask {
        walk: Walk::new(cx, args)?,
        out: Buf::new(),
    }))
}

/// `filter` and `filter_map`: which elements survive depends on the callback's results, so
/// their labels become the result list's own labels.
#[derive(Debug)]
struct FilterTask {
    walk: Walk,
    out: Buf,
    current: Option<Value>,
    decided: Labels,
    keep_result: bool,
}

impl Task for FilterTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(v) = input {
            self.decided |= v.labels;
            let item = self.current.take();
            if self.keep_result {
                if !v.is_none() {
                    self.out.push(cx, v)?;
                }
            } else if v.as_bool() == Some(true)
                && let Some(item) = item
            {
                self.out.push(cx, item)?;
            }
        }
        match self.walk.next() {
            Some(x) => {
                self.current = Some(x.clone());
                Ok(TaskStep::Call(self.walk.f.clone(), vec![x]))
            }
            None => Ok(TaskStep::Done(new_list(
                cx,
                self.out.take(),
                self.walk.own | self.decided,
            )?)),
        }
    }
}

fn list_filter(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    Ok(Box::new(FilterTask {
        walk: Walk::new(cx, args)?,
        out: Buf::new(),
        current: None,
        decided: Labels::NONE,
        keep_result: false,
    }))
}

fn list_filter_map(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    Ok(Box::new(FilterTask {
        walk: Walk::new(cx, args)?,
        out: Buf::new(),
        current: None,
        decided: Labels::NONE,
        keep_result: true,
    }))
}

#[derive(Debug)]
struct FoldTask {
    items: Cursor,
    own: Labels,
    f: Value,
    acc: Option<Value>,
}

impl Task for FoldTask {
    fn resume(&mut self, _cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(v) = input {
            self.acc = Some(v);
        }
        let acc = self.acc.take().unwrap_or_else(Value::unit);
        match self.items.next() {
            Some(x) => Ok(TaskStep::Call(
                self.f.clone(),
                vec![acc, x.with_labels(self.own)],
            )),
            // How many steps were folded depends on the list's own labels.
            None => Ok(TaskStep::Done(acc.with_labels(self.own))),
        }
    }
}

fn list_fold(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    let xs = arg(cx, args, 0)?;
    Ok(Box::new(FoldTask {
        items: list(cx, xs)?.cursor(),
        own: xs.labels,
        acc: Some(arg(cx, args, 1)?.clone()),
        f: arg(cx, args, 2)?.clone(),
    }))
}

#[derive(Debug)]
struct SortTask {
    items: Vec<Value>,
    /// The heap charge of `items`.
    _items_charge: Charge,
    keys: Buf,
    f: Value,
    descending: bool,
    own: Labels,
    /// Whether the comparisons' charge was debited.
    charged: bool,
}

impl Task for SortTask {
    fn resume(&mut self, cx: &mut Cx<'_>, input: Resume) -> Result<TaskStep, RtError> {
        if let Resume::Value(k) = input {
            self.keys.push(cx, k)?;
        }
        if let Some(x) = self.items.get(self.keys.items.len()) {
            return Ok(TaskStep::Call(self.f.clone(), vec![x.clone()]));
        }
        if !self.charged {
            // Comparing keys walks them: on top of the class's `n * log2(n + 1)`, the keys'
            // weight times the comparisons each takes part in.
            self.charged = true;
            let n = self.items.len() as u64;
            let weight = self
                .keys
                .items
                .iter()
                .fold(0u64, |w, k| w.saturating_add(k.weight()));
            return Ok(TaskStep::Charge(
                weight.saturating_mul(crate::ops::log_work(n)).div_ceil(64),
            ));
        }
        let keys = &self.keys.items;
        let descending = self.descending;
        let order = util::merge_sort(cx, self.items.len(), &|i| keys[i].weight(), &mut |a, b| {
            if descending {
                cmp_natural(&keys[b], &keys[a])
            } else {
                cmp_natural(&keys[a], &keys[b])
            }
        })?;
        // The order depends on every key.
        let decided = self.keys.items.iter().fold(self.own, |l, k| l | k.quick());
        let mut out = util::Gather::with_capacity(cx, order.indexes.len() as u64, decided)?;
        for &i in &order.indexes {
            out.push(cx, self.items[i].clone())?;
        }
        Ok(TaskStep::Done(out.finish(cx, decided)?))
    }
}

fn list_sort_by(cx: &mut Cx<'_>, args: &Args) -> Result<Box<dyn Task>, RtError> {
    let xs = arg(cx, args, 0)?;
    let own = xs.labels;
    let source = list(cx, xs)?;
    // The copy the sort works on is charged before it is made.
    let mut items_charge = Charge::none();
    items_charge
        .grow(cx.heap, cost::SLOT.saturating_mul(source.len()))
        .map_err(|_| cx.fault(Fault::Heap, Labels::NONE))?;
    let items: Vec<Value> = source
        .to_vec()
        .into_iter()
        .map(|v| v.with_labels(own))
        .collect();
    let descending = opt(args, 2).and_then(Value::as_bool).unwrap_or(false);
    Ok(Box::new(SortTask {
        keys: Buf::new(),
        items,
        _items_charge: items_charge,
        f: arg(cx, args, 1)?.clone(),
        descending,
        own: own | opt(args, 2).map_or(Labels::NONE, |d| d.labels),
        charged: false,
    }))
}

// ---- text ----

fn text_len(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    match &s.repr {
        Repr::Str(t) => {
            Ok(Value::int(i64::try_from(t.chars()).unwrap_or(i64::MAX)).with_labels(s.labels))
        }
        _ => Err(raise(
            cx.p,
            Kind::Invalid,
            "expected text",
            s.labels,
            cx.loc,
        )),
    }
}

fn text_upper(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    // An upper-cased text can be longer than its source (`ß` is `SS`): its size is checked
    // before it is built.
    let size: usize = t
        .chars()
        .map(|c| c.to_uppercase().map(char::len_utf8).sum::<usize>())
        .sum();
    cx.work += 2 * size as u64 + 1;
    if size > crate::value::MAX_STR_BYTES {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "the upper-cased text is longer than 16 MiB",
            s.labels,
            cx.loc,
        ));
    }
    new_text(cx, t.to_uppercase(), s.labels)
}

fn text_trim(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    let t = text(cx, s)?;
    cx.work += 2 * t.len() as u64 + 1;
    new_text(cx, t.trim().to_owned(), s.labels)
}

fn text_contains(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (s, part) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let (t, p) = (text(cx, s)?, text(cx, part)?);
    cx.work += (t.len() + p.len()) as u64 + 1;
    Ok(Value::bool(t.contains(p)).with_labels(s.labels | part.labels))
}

fn text_split_once(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (s, sep) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = s.labels | sep.labels;
    cx.work += 2 * text(cx, s)?.len() as u64 + 1;
    let Some((a, b)) = text(cx, s)?.split_once(text(cx, sep)?) else {
        return Ok(Value::none().with_labels(labels));
    };
    let parts = vec![
        new_text(cx, a.to_owned(), labels)?,
        new_text(cx, b.to_owned(), labels)?,
    ];
    Value::new_tuple(cx.heap, parts)
        .map(|v| v.with_labels(labels))
        .map_err(|f| cx.fault(f, labels))
}

fn text_join(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let parts = arg(cx, args, 0)?;
    let sep = opt(args, 1).cloned().unwrap_or_else(|| Value::text(""));
    let labels = parts.quick() | sep.labels;
    let sep_text = text(cx, &sep)?.to_owned();
    let mut out = String::new();
    let mut bad = false;
    let mut first = true;
    list(cx, parts)?.for_each(&mut |v| match v.as_text() {
        Some(t) if !bad => {
            if !first {
                out.push_str(&sep_text);
            }
            first = false;
            if out.len() + t.len() > crate::value::MAX_STR_BYTES {
                bad = true;
            } else {
                out.push_str(t);
            }
        }
        _ => bad = true,
    });
    cx.work += 2 * out.len() as u64 + 1;
    if bad {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "the joined text is too long",
            labels,
            cx.loc,
        ));
    }
    new_text(cx, out, labels)
}

fn text_show(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let v = arg(cx, args, 0)?;
    let shown = crate::ops::show(cx.p, v);
    cx.work += shown.len() as u64 + 1;
    if shown.len() > crate::value::MAX_STR_BYTES {
        // The writer stopped just past the limit: what it rendered is charged before the
        // refusal, as a returned result's would be.
        cx.debit = (shown.len() as u64).div_ceil(64);
    }
    new_text(cx, shown, v.quick())
}

fn text_repeat(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (s, n) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = s.labels | n.labels;
    let t = text(cx, s)?;
    let count = int(cx, n)?;
    let Ok(count) = usize::try_from(count) else {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "a negative repeat count",
            labels,
            cx.loc,
        ));
    };
    // Checked: an overflowing size is refused like any over-long one, before allocating.
    if t.len()
        .checked_mul(count)
        .is_none_or(|total| total > crate::value::MAX_STR_BYTES)
    {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "the repeated text is longer than 16 MiB",
            labels,
            cx.loc,
        ));
    }
    let repeated = t.repeat(count);
    cx.work += repeated.len() as u64 + 1;
    new_text(cx, repeated, labels)
}

/// The cells between cancellation checks in `text.similarity` (one checkpoint's worth of
/// work at 64 cells a step).
const SIMILARITY_CHECK_CELLS: u64 = 64 * crate::machine::CHECKPOINT;

/// 1 minus the Levenshtein distance over the longer length, in O(n₁·n₂) time and O(min(n₁, n₂))
/// space: two rows over the shorter text (the distance is symmetric), charged to the heap,
/// never an n₁ × n₂ matrix. The whole `product` charge was debited before it runs; it checks
/// for cancellation every checkpoint's worth of cells, so a cancelled run stops within one
/// checkpoint interval as pure code does.
fn text_similarity(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = a.labels | b.labels;
    let (ta, tb) = (text(cx, a)?, text(cx, b)?);
    let (long, short) = if ta.chars().count() >= tb.chars().count() {
        (ta, tb)
    } else {
        (tb, ta)
    };
    let y: Vec<char> = short.chars().collect();
    let longest = long.chars().count();
    if longest == 0 {
        return Ok(Value::float(1.0).with_labels(labels));
    }
    // The working memory (the shorter text's characters and two rows of counts) was charged
    // to the heap before the built-in ran ([`s_product`]).
    let mut prev: Vec<usize> = (0..=y.len()).collect();
    let mut cur = vec![0; y.len() + 1];
    let mut since_check = 0u64;
    for (i, cx_) in long.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cy) in y.iter().enumerate() {
            let sub = prev[j] + usize::from(cx_ != *cy);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
        since_check += y.len() as u64 + 1;
        cx.work += y.len() as u64 + 1;
        if since_check >= SIMILARITY_CHECK_CELLS {
            since_check = 0;
            if cx.cancel.is_cancelled() {
                return Err(raise(
                    cx.p,
                    Kind::Cancelled,
                    "the evaluation was cancelled",
                    Labels::NONE,
                    cx.loc,
                ));
            }
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "a ratio of lengths")]
    let sim = 1.0 - prev[y.len()] as f64 / longest as f64;
    Ok(Value::float(sim).with_labels(labels))
}

// ---- map ----

fn map_get(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (m, k) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = m.labels | k.labels;
    Ok(match map(cx, m)?.get_entry(k) {
        Some((stored, v)) => v.clone().with_labels(labels | stored.quick() | k.quick()),
        // A miss depends on every key the probe was compared with.
        None => Value::none().with_labels(labels | m.quick()),
    })
}

fn map_set(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (m, k, v) = (arg(cx, args, 0)?, arg(cx, args, 1)?, arg(cx, args, 2)?);
    // Whether the key set grew depends on the key, and on the keys it was compared with.
    let (_, path) = map(cx, m)?.probe(k);
    let labels = m.labels | k.labels | path;
    let out = map(cx, m)?
        .insert(cx.heap, k.clone(), v.clone())
        .map_err(|f| cx.fault(f, labels))?;
    Ok(Value::new_map(out).with_labels(labels))
}

fn map_keys(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let m = arg(cx, args, 0)?;
    let mut keys = Vec::new();
    let mut own = m.labels;
    map(cx, m)?.for_each(&mut |k, _| {
        own |= k.quick();
        keys.push(k.clone().with_labels(m.labels));
    });
    new_list(cx, keys, own)
}

// ---- json ----

fn json_parse(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let s = arg(cx, args, 0)?;
    crate::json::parse(cx.heap, text(cx, s)?, s.labels).map_err(|f| cx.fault(f, s.labels))
}

fn json_encode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let v = arg(cx, args, 0)?;
    let Some(&t) = cx.type_args.first() else {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "json.encode without a type",
            Labels::NONE,
            cx.loc,
        ));
    };
    crate::json::encode(cx.p, cx.heap, v, t, &[], 0).map_err(|f| cx.fault(f, v.quick()))
}

fn json_decode(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let j = arg(cx, args, 0)?;
    let Some(&t) = cx.type_args.first() else {
        return Err(raise(
            cx.p,
            Kind::Invalid,
            "json.decode without a type",
            Labels::NONE,
            cx.loc,
        ));
    };
    match crate::json::decode(cx.p, cx.heap, j, t).map_err(|f| cx.fault(f, j.quick()))? {
        Ok(v) => Ok(v),
        Err(violations) => {
            let labels = j.quick();
            let items: Result<Vec<Value>, Fault> = violations
                .iter()
                .map(|v| {
                    let s = crate::value::Str::new(cx.heap, v.clone())?;
                    Ok(Value::new_json(cx.heap, Json::Str(s))?.with_labels(labels))
                })
                .collect();
            let items = items.map_err(|f| cx.fault(f, labels))?;
            let data = List::from_vec(cx.heap, items)
                .and_then(|l| Value::new_json(cx.heap, Json::Array(l)))
                .map_err(|f| cx.fault(f, labels))?
                .with_labels(labels);
            let message = Value::text(&format!(
                "the JSON does not match the type: {}",
                violations.join("; ")
            ))
            .with_labels(labels);
            Err(raise_with(
                cx.p,
                Kind::SchemaMismatch,
                message,
                Some(data),
                cx.loc,
            ))
        }
    }
}

fn json_get(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let (j, ptr) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    let labels = ptr.labels;
    // A hit or a miss: the pointer's labels reach it either way.
    Ok(crate::json::get(j, text(cx, ptr)?)
        .map_err(|f| cx.fault(f, labels))?
        .with_labels(labels))
}

// ---- math ----

fn math_abs(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    let x = arg(cx, args, 0)?;
    let v = match x.repr {
        Repr::Int(i) => i.checked_abs().map(Value::int),
        Repr::Float(f) => Some(Value::float(f.abs())),
        Repr::Duration(d) => d.checked_abs().map(Value::duration),
        Repr::Money(m) => m.checked_abs().map(Value::money),
        _ => None,
    };
    v.map(|v| v.with_labels(x.labels)).ok_or_else(|| {
        raise(
            cx.p,
            Kind::Invalid,
            "`math.abs` overflowed",
            x.labels,
            cx.loc,
        )
    })
}

fn min_max(cx: &Cx<'_>, args: &Args, max: bool) -> Result<Value, RtError> {
    let (a, b) = (arg(cx, args, 0)?, arg(cx, args, 1)?);
    // Which value is picked depends on all of both.
    let labels = a.quick() | b.quick();
    // IEEE 754 minNum / maxNum: a NaN operand gives the other.
    if let (Repr::Float(x), Repr::Float(y)) = (&a.repr, &b.repr) {
        return Ok(Value::float(if max { x.max(*y) } else { x.min(*y) }).with_labels(labels));
    }
    let pick_a = match cmp_natural(a, b) {
        std::cmp::Ordering::Less => !max,
        std::cmp::Ordering::Greater => max,
        std::cmp::Ordering::Equal => true,
    };
    Ok(if pick_a { a.clone() } else { b.clone() }.with_labels(labels))
}

fn math_min(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    min_max(cx, args, false)
}

fn math_max(cx: &mut Cx<'_>, args: &Args) -> Result<Value, RtError> {
    min_max(cx, args, true)
}
