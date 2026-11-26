# ratio-eval

Ratio's core interpreter: the second half of stage 6 of the
[implementation plan](../../spec/10-implementation.md), stage 7's data built-ins (`text`,
`re`), cost classes and offload pool, and the `json`, `list` and `map` built-ins of stage 8
([below](#stage-8-json-list-and-map)). Hosts reach it through the
[`ratio` facade](../ratio/README.md) (`ratio::prepare`, `ratio::evaluate`, `ratio::interp`).

```rust
let program = ratio::lower(&loaded, &checked)?;
let prepared = ratio::prepare(&program)?;
let entry = prepared.find("main.ratio", "main").unwrap();
let eval = ratio::evaluate(prepared, entry, args, ratio::Limits::default(), ratio::CancelToken::new())?;
let finished = ratio::block_on(eval); // or `.await` on any runtime
```

## The prepared form

`prepare` lowers the canonical IR once more into an arena of nodes with literals built, record
fields resolved to layouts and slots, built-ins to table entries and items to indexes; each node
keeps its site id and source position. It is never serialised or hashed.

## The evaluator

A tree walker over the prepared form with an **explicit continuation stack** on the heap: a
program's nesting, recursion and callbacks never touch the Rust stack, whoever polls the
evaluation. `Machine::run` returns at every 1,000-step checkpoint (`Suspend::Yield`, after
checking cancellation) and at every effect (`Suspend::Effect`), and continues where it stopped.
`Evaluation` is the `Future` over it: it yields to the runtime at each checkpoint and, in this
stage, answers every effect with `Unsupported`. Stage 11's host plugs in there: the driver awaits
`host.call(request)` and passes the answer to `Machine::resume_effect`. Built-ins that call back
into the program (`list.map`, `result`, …) are small state machines the walk drives, so program
code always runs on the walk's own stack. Values are `Send + Sync`, and so is the evaluation.

`par` and `race` run their branches in written order in this stage (a valid schedule; stage 11
brings real concurrency) and enforce the branch restriction: a `var` declared outside a branch
cannot be assigned inside it. That is a program defect, not a recoverable error: it raises
`Invalid` with the class `Fatal`, which no `catch`, `result` or sibling `race` branch can
swallow. Harness calls, `with` blocks, call overrides, `with
timeout`, tool sets and `state` raise `Unsupported` until frames (stage 10) and the host (stage
11) exist.

## Values and secrecy labels

Every value carries its **own labels** (`Labels`, a set of up to 32 sources): a leaf's labels,
or, on a container, the labels of whatever decided its shape (its length, which elements it
kept, their order, its key set). Reading anything out of a container adds the container's own
labels. Every operator gives its result the union of its operands' labels (the deep labels for
`==`, `in` and `text.show`), and so does every built-in; `??`, `and`, `or` and `?.` add the labels
of the value that chose the result; an index adds the index's labels; an error raised by an
operation on labelled data has a labelled message. `Json` keeps labels on its nodes through
`json.parse`, `json.encode`, `json.decode` and `json.get`. Branching on a labelled value is an
implicit flow outside the guarantee (chapter 03). Only `declassify` removes labels: the host
answers only whether the user agreed, and the interpreter itself clears the argument.

Every container keeps a **label summary**, the union of the own labels inside it, maintained on
construction (it may be set when nothing below is labelled any more, never clear when something
is). `sink::check` passes a value whose labels and summary are clear in O(1) and otherwise scans
it; debug builds scan every value and assert that the summary covers the scan.

Lists are persistent AVL ropes with leaves of up to 32 elements (O(log n) push, get, set, slice
and concatenation); maps are persistent AVL trees in key order; records have a fixed, sorted
layout; text caches its character count and an ASCII flag. Values nest at most 256 deep
(`Invalid` beyond), so every walk and comparison of a value fits a small stack in a release build (in a debug build the frames of stage 6's recursive walks (`==` and order on lists and records, `json.encode`, `json.decode`) are larger and a 256 KiB stack may not hold a value nested near the limit; stage 8's JSON walks are iterative and are tested on 256 KiB in debug); dropping a value
never recurses at all (every container frees its children through a work-list). Rendering a
value as text (`text.show`, interpolation, the `json` spec) stops writing as soon as the text is
longer than 16 MiB, and the result is refused; `text.show` is then charged for what it wrote
(`⌈len / 64⌉`) as well, so a refusal costs what a result would.

## Limits and charges

| Limit | Default | Maximum | Exceeded |
| --- | --- | --- | --- |
| Steps | 5,000,000 | 100,000,000 | `Budget { which: steps }` |
| Live heap | 64 MiB | 512 MiB | `Budget { which: heap }` |
| Call depth | 512 | 4,096 | `Budget { which: call_depth }` |

**Steps** are charged before the work and depend only on the program and its data: one for every
IR expression, statement and pattern node the walk enters; one per loop iteration; a built-in's
cost-class charge `1 + ⌈w / 64⌉` (chapter 06, `data`), its argument part debited before it runs
(so a `product` built-in over large inputs raises `Budget` without running) and a `linear`
built-in's output part on return; `+` on text and interpolation `1 + ⌈m / 64⌉` for an `m`-character
result; a formatted part `1 + ⌈(n + m) / 64⌉`; comparisons of containers or text `⌈n / 64⌉` over
the operands' weights (a record's weight counts its field names, a variant's its name); `in` and indexing `Json`
linear, list `+` and spreads `log`; a map lookup or update (`map.get`, `map.set`, `m[k]`, `k in
m`, `m[k] = v`) `1 + ⌈log2(n + 1) · |k| / 64⌉`, since every comparison walks the key;
`list.sort_by` its class's charge plus `⌈W · log2(n + 1) / 64⌉` for keys of total weight `W`;
a prompt's `include` `1 + ⌈s / 64⌉` for the `s` sections and values it copies. The **heap**
ledger charges every allocation a fixed, platform-independent size before it is made and credits
it when the value is freed, so it counts live values: values, each prompt section as it is
built, and the buffers built-ins gather into (a slot per element). The values the evaluator's frames gather while they work (a node's children, `par`
results, a section's values, an assignment's indexes) are charged a slot each as they arrive
and given back when the frame is done, so every value the evaluation holds is on the ledger;
what remains uncharged is the frames' fixed part (a few words each, at most one per IR node
being evaluated per call, for at most `call_depth` calls). `Budget` and `Cancelled` pass through every
`catch`; `defer` blocks run on every exit, in reverse order (the first error wins when one
raises).

`builtins::implemented()` lists the built-ins carried so far, with their cost classes: the pure
functions of the checker's table, among them `text.repeat` (`output`: a negative count or a
result over 16 MiB is refused before anything is built, with checked arithmetic) and
`text.similarity` (`product`: two rows over the shorter text, charged to the heap before it
runs, checking cancellation every checkpoint's worth of cells). A built-in's **working memory**
(those rows, a regex VM's thread lists) is part of its sizes and charged to the heap before it
runs, with its steps, and held until it finishes.

## `text` and `re` (stage 7)

Every `text` built-in of chapter 06 is carried (`src/builtins/text.rs`). Indexes and lengths
count characters; every result carries the labels of every argument that shaped it (the text, a
separator, a count, a width), including the messages of the errors they raise. No result is
built past 16 MiB: the size is checked before building, or a capped writer stops at the limit
and the refusal is charged for what it wrote. Choices the table leaves open: `text.find` treats
a negative `from` as 0; `text.split` refuses a `limit` below 1 (`Invalid`); `text.pad_start`
and `pad_end` cycle a multi-character `fill` and leave the text as is for an empty one;
`text.wrap` treats a width below 1 as 1; `text.pad_start` and `pad_end` read only the fill
characters they use and are charged for the fill as input; `text.split`, `lines`, `words` and
`chars` count their parts first and refuse more than 1,000,000 (`Invalid`) before building
any, charging the list's slots before they are filled; `text.lower` and `upper` check the
result's size before building it; `text.parse_float` accepts surrounding whitespace, as
`parse_int` does, and refuses `inf`, `NaN` and out-of-range values; `text.extract_json` tries
at most the first 32 `{` or `[` positions (so it stays linear); `text.code_blocks` follows
CommonMark fences (an unclosed block runs to the end); `text.quote` frames with a provisional
`HARNESS_TAGS` list and preamble line until stage 12's shared framing vectors fix them.

The `re` built-ins (`src/builtins/regex.rs`) run on a linear-time engine (`src/re/`): a parser
for Rust `regex` syntax, a Thompson NFA and a Pike VM. Nothing backtracks. Supported: literals
and escapes (`\n`, `\x7F`, `\x{…}`, `\u{…}`, `\U{…}`, escaped punctuation), `.`, classes
(ranges, negation, nesting, `[:alpha:]`, `&&`, `--`, `~~`), `\d \w \s` and their negations
(Unicode-aware: `\d` is `Nd`; `\w` is `Alphabetic`, `Nd`, connector punctuation, the join
controls and the common combining-mark blocks, an approximation of Rust's `M`), the Unicode
classes `Alphabetic`, `Lowercase`, `Uppercase`, `White_Space`, `Nd`, `N`, `Cc`, `Any` and
`ASCII`, the anchors `^ $ \A \z \b \B`, capturing, non-capturing and named groups (`(?P<n>…)`,
`(?<n>…)`), alternation, `* + ? {n} {n,} {n,m}` and their lazy forms (compiled as
regex-automata compiles them, so a body that can match empty has Rust's priorities; `e{0}` and
repetitions of nothing collapse in the parser, and every copy weighs at least 1), and the flags
`i m s x U` (case-insensitivity uses the standard library's simple case mappings). Refused,
with a message saying what (never matched differently): backreferences, lookaround, atomic
groups, other Unicode classes (scripts, general categories such as `\pL`), byte mode
`(?-u)`, CRLF mode `R`, `\Z`, `\<`, `\>`, `\b{…}`, octal escapes, `{,m}`, a repetition of a
repetition (`a**`, which Rust accepts), a class range starting with a class (`[\d-z]`), counts
above 1000, nesting deeper than 64, patterns over 4 KiB and programs over
10,000 instructions of weight (`(a{1000}){1000}` is `Invalid` before it allocates much).
Matching is leftmost-first, as Rust's; an iteration skips an empty match touching the last
one. Spans (`start`, `end`) count characters. `Match`, `Captures` and `CodeBlock` are generic
in the text type (`Match<S>`), so parts of untrusted text stay untrusted. Each call site keeps
its compiled patterns (at most 32, keyed by site and pattern text, charged to the heap).

**Charges.** The `re` built-ins are charged as `product`: before running, the text's characters
plus one, times the program's weight times one plus the slots the search tracks (0 for
`is_match`, 2 for spans, every group's for `captures` and for a replacement that names groups),
plus the compile work (sixteen units per pattern character and the counted compile steps, one per
node compiled with every repeated copy and parts that match nothing dropped first, charged on a
cache hit as on a miss; a refused pattern is charged the work its refusal took) and the
characters of the text and the replacement. The output is charged as it is built: one unit per
element (a match record's three fields, a group, a named entry, a split part) and per character.
A replacement template is resolved against the group names once per call, and captures share
the group-name values. Searches and iterations check cancellation every checkpoint's worth of
work. A search counts at most four
times that work. Iterating (`find_all`, `captures_all`, `replace_all`, `split`) searches again
from each match's end, which a pathological pattern can make quadratic (as in Rust's `regex`):
work past four times the charge is debited as it is done, `Budget { which: steps }` when the
steps left cannot pay, so no call runs unpaid. `limit` arguments must lie in `0..=1,000,000`.
Chapter 06 lists `re` under `linear`; the difference is a proposed erratum.

## The offload pool

`Pool` (`src/pool.rs`) is a bounded set of worker threads the host's driver owns; each session
gets a `PoolSession` (`Evaluation::offload_to`). A pure built-in without callbacks whose input
passes the thresholds (`OFFLOAD_TEXT_BYTES`, 64 KiB of one text argument;
`OFFLOAD_ELEMENTS`, 16,384 elements of one container; or `OFFLOAD_STEPS`, a charge before
running of more than ten checkpoints, as for a heavy pattern over a short text; and the
iterating `re` built-ins, which debit as they go, whenever the steps left exceed that) runs on a worker while `Machine::run`
returns `Suspend::Wait`; the `Evaluation` future registers its waker with the job and the
cancellation token and returns `Pending`, so the thread serves other sessions. The steps and the
working memory are charged before dispatch exactly as inline, so results, labels and charges
are identical above and below the threshold. Each session has at most `JOBS_IN_FLIGHT` (2) jobs
in flight; further offloads wait in its queue. A cancelled evaluation stops waiting at once, but
its job runs to its end and keeps its place, its steps (never refunded) and its heap (its
arguments and working memory) until then. A job sees the cancellation at its next check (within
a checkpoint's worth of work) and one still queued ends without running, so a cancelled job's
unpaid work is bounded. Queued jobs hold their heap from submission; the per-session queue is
bounded only by the session's waiting evaluations, an open item for stage 11a's scheduler.
Charges per element (for every `linear` built-in, `text.split` as much as `re.find_all`) are
calibrated at one unit an element, an open item for stage 24's measurements. No program code runs on a worker. Dropping the pool
runs the queued jobs and joins every worker; a job submitted after that runs where it is
submitted. Without a pool, everything runs inline.

## Stage 8: `json`, `list` and `map`

Every `json.*`, `list.*` and `map.*` built-in of chapter 06 (`data`): 14, 47 and 15 rows, in
`src/builtins/` (`json.rs`, `list.rs`, `map.rs`; `util.rs` holds what they share, `validate.rs`
the schema validator, `ecma.rs` its `pattern` front end), on stage 6's persistent lists
(`plist.rs`) and maps (`pmap.rs`, with `probe` and `from_sorted` added), `Json` with labels
(`json.rs`) and the charging framework. `jsonedit.rs` edits JSON (pointers, `set`, `merge`,
`patch`, `diff`, rendering, equality and order); `schema.rs` derives `json.schema::<T>()`.

**Charges** (the class's work `w`, charged `1 + ⌈w / 64⌉`, debited before the built-in runs
from the arguments' sizes, with saturating arithmetic; `n` is a length, `W` a weight, `|k|` a
key's weight, `L = ⌈log2(n + 1)⌉`):

| Class | Built-ins and their `w` |
| --- | --- |
| `const` | `list.is_empty`, `list.last`, `map.len`, `map.is_empty`, `json.type_of` |
| `log` | `list.insert`, `remove_at`, `drop`, `slice`: `L`; `list.concat`: `L` over both lengths; `map.has`, `remove`, `update`: `L · \|k\|` |
| `nlogn` | `list.sort`, `unique`, `map.from`: `W · L` over the whole list (elements are compared whole); `list.unique_by`, `group_by`, `map.count_by`: `n · L`, then `W_keys · L` once the keys are known, as `list.sort_by` does |
| `output` | `list.range`, `list.repeat`: the number of elements the arguments fix (at most 1,000,000, else `Invalid` before anything is built) |
| `linear` | everything else; `n` is the length, or the weight where values are compared or walked (`list.flatten`, `index_of`, `min`, `max`, `map.merge`, `json.stringify`, `set`, `merge`, `equal`), and for `list.window` `n + windows · (L + 64)` since each window is a slice |
| `product` | `json.diff` and, **as the specification's `linear` is unsound for them**, `json.validate` (the product of the instance's and the schema's weights: each pair of nodes may be visited) and `json.patch` (`(W_doc + W_ops) · W_ops`: each operation may copy a path through the document) |

`json.schema::<T>()` has no arguments to size: its work is bounded by the type (an output of
at most 1 MiB and a work budget) and its output is charged on return.

**Labels.** Every result carries the labels of everything that decided it: a lookup or update
the labels of every key on its path (`Map::probe`), a scan the labels of every callback result it
examined (`find`, `any`, `take_while`, `partition`, ...), an order-dependent result (`sort`,
`unique`, `min`, `index_of`, `json.diff`, `json.validate`) the labels anywhere in its inputs, a
rebuilt JSON container its old labels, the pointer's or patch's, and its keys'. A failure's
message carries the same labels.

**Resources.** Every buffer proportional to an input is charged to the heap when it is made
(`util::Gather`: one slot per element, charged up front when the size is known, at most
1,000,000 elements); sorting uses a stable merge sort that cannot panic on an odd order;
long loops check for cancellation every checkpoint's worth of work (`util::Pace`).
`json.parse`, JSON equality and order, `merge`, `set`, `patch`, `diff` and rendering keep their
nesting on an explicit stack; the recursive walks that remain (`json.validate`'s, and the
derivation of `json.schema`) are bounded (192 levels of nesting, `$ref` hops counted apart, at
most 32 at a node; 256 levels of type) and run on the caller's stack while they stay within 24
levels, and on the large-stack worker the preparation pass uses (its counted work added to the
caller's) beyond that. `json.validate` also counts its visits of a schema node to an instance
node and raises `Budget { which: steps }` past four times its charge (plus a floor), so shared
sub-schemas reached over and over (`$ref` into `allOf`) cannot do more work than was paid for;
a failing or finished `json.schema` is charged one step for each unit of its derivation work. `json.stringify` stops writing at 16 MiB and is charged for
what it wrote.

**The work counters (one API for stages 7 to 9).** A built-in's work is counted in two places,
and the machine adds them: `Cx::work`, which a body with a `Cx` increments directly (stage 7's
text and `re` bodies), and the thread-local `work` module (`work::add(units)`), which code
without a `Cx` (the persistent lists and maps, `ops::eq` and `cmp_natural`, `jsonedit`, the
built-ins' own loops) calls. Around each **pure** built-in call, on whichever thread runs it
(inline or on the pool), `run_pure` does `work::reset()`, runs the body, and returns
`cx.work + work::total()` as `Ran::work`; `Machine::work_log()` (debug builds) records it with
the call's steps as a `BuiltinWork { name, steps, work, offloaded }`. A test compares `work` with
`64 × FACTOR × steps` per entry; stage 7's `tests/data.rs` and stage 8's `tests/data_limits.rs`
both read that one log (`tests/support`'s `run_logged` gives it). New code in a built-in
counts what it does with `work::add(n)` (one unit per element visited, node built or character
compared; pace long loops with `Pace::tick`, which counts too), and a body that hands work to
another thread (`json.validate`'s large-stack worker) adds that thread's `work::total()` back
into its own counter before it returns. Release builds count nothing. Callbacks (tasks) are not
logged: they have no single call to measure.

**What the specification leaves open, and this stage decided.** `json.validate` reads the
keywords `$ref` (into the same schema only), `type`, `enum`, `const`, the numeric, string, array
and object keywords, `allOf`, `anyOf`, `oneOf`, `not` and `if/then/else`, and ignores the rest
(`format`, `unevaluated*`, annotations); `pattern` goes through the
linear-time `re` engine of stage 7 behind a front end (`ecma.rs`) that accepts only chapter 03's
`@pattern` subset (`ratio_check::regex_problem`) and makes it mean what ECMAScript means: `.`
stops at `\r`, U+2028 and U+2029, `\d` and `\w` are ASCII, `\s` is ECMAScript's white space,
a character outside the basic plane is one character (the `u` flag); `\b`, `\B`, `\D`-style
escapes inside a class and a class starting `]` or `^]` are refused (`Invalid`). Compiling and
searching are debited as steps (never more than the steps left) before they run. A pointer is strict RFC 6901 (`~` is followed by `0` or
`1`) and names at most 257 tokens. `list.insert` takes an index in `0..=len` and `remove_at` one in
`0..len`: negative indexes are `Invalid`. `list.unique`, `unique_by`, `group_by`, `count_by` and
`map.from` take keys equal when the natural order says so (so every `NaN` is one key).
The natural order compares an `Int` with a `Float` through `f64`, so beyond 2^53 it is not transitive
(a sort still terminates and never panics, but its order among such numbers is arbitrary).
`json.keys` and `json.stringify` give an object's keys in the order the IR keeps (a record's
fields are in name order). `json.stringify` returns `Untrusted<Str>`, as `text.show` does.

## Tests

`tests/data_ii.rs` runs the `json.*`, `list.*` and `map.*` files of `conformance/data/`, one file per `json`, `list` and `map` row (values,
failures, steps, and label propagation for each argument: `labels NAME: args`), and checks that
the files cover the chapter's tables; `tests/data_limits.rs` holds the debug-build work counters
against each built-in's charge, the refusal without running (`Budget { which: steps }`), the
O(N log N) loops of pushes and sets, hostile nesting on a 256 KiB stack, cancellation and the
heap; `tests/data_review.rs` holds the regressions of the security review (a bounded `$ref` walk, labels of `patch` paths and composite keys, ECMAScript patterns, linear record `==`); `tests/data_props.rs` compares the built-ins with `Vec` and `BTreeMap` models and
round-trips random JSON through `diff`, `patch`, `set` and `get`; `tests/schema.rs` compares
`json.schema` with the checker's own derivation byte for byte.
`tests/conformance.rs` runs `conformance/eval/` (`ok`, `value`, `runtime-error` and `steps`
expectations); `tests/labels.rs` covers label propagation for every operator and built-in,
`Json`, `declassify` and the sink check; `src/properties.rs` checks the persistent structures
against models and the summary property (including labels a replay adds; the `ratio_cache`
restore is a stub until stage 11b); `tests/limits.rs` covers the cost classes, the heap ledger,
call and value depth, hostile programs on a 256 KiB stack, cancellation, and a 5,000,000-step
loop that lets a second session progress on a single-threaded executor.

Stage 7: `tests/conformance.rs` also runs `conformance/data/`, one file per `text` and `re`
built-in, with secret-labelled arguments (`// arg:`) and the labels of each result element
(`// expect: labels`); `tests/data.rs` checks that every built-in's counted work (the debug-build
`Machine::work_log`) stays within 8 × 64 units per charged step, that offloaded calls give the
same values, labels, steps and heap peak as inline ones (at threshold 0 over the whole corpus,
and at the default threshold), that a charge the steps left cannot pay raises `Budget` without
running (`text.similarity`, `re.find_all`), that a quadratic iteration pays as it goes, and that
ReDoS probes (`(a+)+$`, `(a|a)*b`, `(.*a){20}`, …) finish on 1 MiB of `a`s; `src/re/` and
`src/pool.rs` unit-test the engine (syntax, refusals, the size limit) and the pool (the cap,
shutdown, panics); `src/offload_tests.rs` checks that a cancelled branch's job keeps its place,
steps and heap until it ends, and that a waiting evaluation is woken when its job finishes.
