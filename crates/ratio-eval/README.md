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
`Evaluation` is the `Future` over it: it yields to the runtime at each checkpoint, and awaits
the host at each effect (`evaluate_with`; `evaluate` and `evaluate_in` use a host that answers
every effect with `Unsupported`), passing the answer to `Machine::resume_effect`. Built-ins that call back
into the program (`list.map`, `result`, …) are small state machines the walk drives, so program
code always runs on the walk's own stack. Values are `Send + Sync`, and so is the evaluation.

`par` and `race` run their branches concurrently (the scheduler, below) and enforce the branch
restriction: a `var` declared outside a branch cannot be assigned inside it. That is a program
defect, not a recoverable error: it raises `Invalid` with the class `Fatal`, which no `catch`,
`result` or sibling `race` branch can swallow. `state` raises `Unsupported` until stage 11b;
harness calls, `with` blocks, call overrides and tool sets open frames (below).

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
most 32 at a node; 256 levels of type) and run on the caller's stack while they stay within 12
levels, and on the large-stack worker the preparation pass uses (its counted work added to the
caller's) beyond that. `json.validate` also counts its visits of a schema node to an instance
node and raises `Budget { which: steps }` past four units a pair of nodes of its charge (plus a floor), so shared
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

## Frames

Stage 10 (chapter 04). The frame model lives in [`src/frame/`](src/frame/mod.rs), inside this
crate rather than a crate of its own: opening a frame reads configuration values, labels and
the step counter of the machine, so it belongs with the evaluator, while its pure half (scopes,
attenuation, ledgers) depends on nothing else here and is public as `ratio_eval::frame`
(`ratio::frame` through the facade) for a host to mirror.

**Opening a frame.** A harness call evaluates its arguments, then its call's overrides
(`f(x) with { … }`) in the caller's frame, then binds its parameters and evaluates its declared
`with { … }` with them bound (the IR lowers the declaration in the harness's own frame for
that), and opens its frame; an entry that is a harness opens its frame from the host's
`Session` the same way, and a `with { … } { … }` block opens a frame at its frame's depth
around its body. The parameter defaults and the declared configuration run before the frame
opens, so both must be effect-free: the checker refuses effects there, including calls of
function-valued parameters (`C0016`), and the machine refuses any effect reached while a
harness is being set up. The configuration record is read into a `Request` (overrides win
field by field, and within `budget`, `limits`, `sees` and `permissions` sub-field by
sub-field; `deny` and `ask` rules of both apply); a value that cannot be read exactly (a
negative cap, a tool pattern with an inner `*`, a folder that is not absolute or holds `..`,
a width of 0, a list of more than 256 tools, folders or rules) raises `Invalid`. Opening is
charged a step per comparison it makes, before it makes them: reading the lists (an entry,
16 bytes of text and the cover filter), the tools' and paths' meets in both directions, the
cover filters over their results, and `check_within`; a test keeps the time of a 256-folder
opening within a fixed multiple of a pure loop's time per step.

**Attenuation.** `frame::attenuate(parent, room, request, kind)` gives the child's `Scope` as
the meet of the parent's and the request in every dimension: tools (names and prefix patterns
intersected exactly; a missing `need` tool is `Denied { by: attenuation }`, a missing `want`
tool is dropped), the permission mode (`plan < ask < default < auto, accept_edits < full`),
rules (the caller's plus the callee's `deny` and `ask`, never an `allow`), paths (folders
intersected component-wise: every folder is absolute, under a POSIX root `/`, a drive
root matched without case (`C:/`; a bare `C:` is that drive's current directory and is
refused), or a UNC share, and different roots never contain each other),
location (`local` if either is; asking for `cloud` under `local`
is refused), each budget cap (at most the request, the parent's cap and what the parent's
ledgers have left), `sees` (fresh and `none` frames see nothing unless they ask; fork frames,
blocks and entries inherit; asking beyond the caller is refused), depth (one more per harness
call; past the limit, `Budget { which: depth }`), width, heap, attended (an unattended caller
cannot open an attended frame), memory writes, and the context mode (`main` only in an entry's
own frame). `frame::check_within(parent, child, kind)` is the independent check, a pure
function of two scopes: the machine runs it on every frame it opens, and a host runs it
against its own record of the parent (Imperium's `RatioHost::open_frame`), so a forged parent
scope cannot widen a child. The host must take `kind` and the parent's depth from its own
record too, never from the `FrameCtx`: a forged `kind` (a harness call passed off as a block)
would skip the depth check.

**`sees` and secrets.** A labelled value entering a frame whose `sees` lacks `secrets` raises
`Denied { by: sees }`: any bound parameter (given or defaulted), any configuration value of the
call's overrides or the declaration, any setting the frame holds (model, effort, thinking,
sampling, system prompt, title; inherited ones included), a result or error leaving a frame
into one, an effect answered with a labelled value or error in one, and a `with` block that
dropped `secrets` reading a labelled local of its enclosing scope (by name, by capturing it
in a closure, or by a compound assignment or an assignment into part of it). The ambient reads chapter 04
gates (`session.turn`, `session.plan`, `memory.index`, …: `frame::gate`) are refused before
the host is asked.

**Budgets and the ledgers.** `frame::Ledgers` is the frame tree's ledgers, one node per frame
under its caller's, behind one lock; a closed node is freed once nothing is held on it and it
has no children (node ids carry a generation), and a closed node admits and opens nothing
below it. An effect is admitted by reserving its charge on its
frame's node and every ancestor's at once: a tool call one `tool_calls`; a model call one
`model_calls`, its counted input and its effective `max_tokens` (the smallest of the requested
`max_tokens`, the model's maximum output or window less the input, and what every ledger can
still cover in tokens and in cost after the input), which the host sends. Below the reply
floor (the smallest of `min_reply_tokens`, the request and the model's maximum, at least one)
an admission waits while a reservation is held on the ledger whose cap binds, and is refused
once none is; the same holds when the input alone cannot be covered. `Admission::Wait` carries
the release epoch read under the lock: `wait_for_release` blocks the calling thread (for hosts and tests on threads of
their own) and `register_waker` wakes an async waiter once a release moves it, so no release
is lost; stage 11a's scheduler uses `register_waker` on the interpreter's thread, and wakers
are woken after the ledgers' lock is dropped. A hosted
model without prices is refused under any cost cap on the path, and a `local` model or a
price of 0 counts as priced. Completion replaces the reservation with the actual charge, so
unspent budget stays with the caller; a charge above the reservation is debited in full and
returned as the overshoot, which the host records (stage 11a). A `Reservation` belongs to its
tree and is given back once: another tree's, or a second give-back, is a `LedgerError`. Every
sum saturates towards "spent". The `steps` ledger is the
same tree's: the machine keeps its frame's limit as an absolute step count and debits the
ledgers at every frame boundary and effect. A `Budget` raised because a frame's own cap ran
out (a harness's, or a `with` block's) passes every `catch` and `result` inside that frame and
becomes catchable once it leaves it; the turn's never does. Running-branch slots
(`acquire_slot`, `release_slot`) are the width's ledger for the scheduler (stage 11a): a
parked branch gives its slot back.

**What a host receives.** Every `EffectRequest` carries the `FrameCtx` (the frame path, its
parent's path, its kind and harness, the attenuated `Scope`, the `Ledgers` and its node, and the
settings: model, effort, thinking, sampling, system prompt and title, inherited where left out)
and the branch path within the frame. Frame paths follow chapter 01: `turn/critic@s41#0`, each
element preceded by the branch path the call was made in (`c12#0.b2/`); a `with` block is an
element named `with`. `meta.frame`, `meta.caller`, `meta.depth`, `meta.limits` and
`meta.permissions` are answered from the frame without the host; `question(text, options)`
builds the `Question` error a callee raises to its caller, which propagates and is caught like
any error (and is refused by `sees` like any other value).

## The host, the scheduler and the tape (stage 11a)

**The `Host` trait** ([`src/host/`](src/host/mod.rs), `HOST_API_VERSION` 1) is everything outside
the interpreter: model calls (`generate`, `embed`, `count_tokens`, `resolve_model`), tool calls
(`call_tool`, `tool_class`, `is_secret_path`, `source_labels`), conversations (`conversation`,
`conversation_digest`, `conversation_labels`, `chat`), frames (`open_frame`, `close_frame`), the engine's operations
(`operate`, for agents, memory, session, context, eval and time until their stages type them),
approvals (`approve`), and the recorded, authority-free `now`, `seed`, `timer` and `emit`. The
asynchronous methods take `self: Arc<Self>` and return a `HostFuture`, a boxed `Send + 'static`
future: the trait is object-safe (`Arc<dyn Host>`), the futures borrow nothing (the scheduler
keeps many in flight next to the machines; a host may spawn them on its runtime), and no runtime
or `async_trait` is needed. Every asynchronous call gets an `EffectCtx`: the effect key
(`frame path / branch path / e<node>#<occurrence>`), a cancellation token and the deadline.
Values cross with their labels (`Value`s carry their own; text is `Labelled<String>`). How the
machine's effects map to these calls is [`src/host/bridge.rs`](src/host/bridge.rs), read from
the checker's table of built-ins: every `@effect(model)` built-in to `generate` (admitted by
reservation); `tools.*` built-ins to `call_tool` with the tool's name (`mcp__server__tool` for
MCP, the named tool for `tools.call_tool`, `dispatch` for the dispatch built-ins, admitted one
`tool_calls` per call they make) and the arguments by parameter name; `declassify` and
`session.request_approval` to `approve`; `log.*`, `trace.event`, `session.emit`,
`session.stream` and `session.notify` to `emit` (output-only, which a host may refuse); the rest
to `operate`; a built-in the table does not know reaches no host. **Every frame carries its
`FrameOrigin`**, filled from the IR and consistent with its kind (the interpreter refuses to open
one that is not): `Entry`; `Block { site }` (the package's IR hash and the node); or
`Harness { package, entry_hash, key }` (the harness's package, that package's IR hash and the
harness's key, `<module path>::<name>`). A host verifies it against what it loaded
(`Prepared::loaded` lists a program's packages and harnesses; `ScriptedHost::load` takes them):
a harness it did not load, or a block at a site of a package it did not load, is refused, so a
harness call forged as a block fails on its origin. `count_tokens` is asynchronous (an exact
tokenizer may be); admission awaits it, a failure is the model call's error, and a replay takes
the count recorded with the admission instead. Host futures and `timer` may be polled from any
executor: a host on tokio spawns its own work and hands back a `Send` future (`host::Sleep` is a
timer on the standard library alone).

**The guard** ([`src/host/guard.rs`](src/host/guard.rs)) wraps every host an evaluation is
given, and the scheduler calls the host through it for every effect; no host can opt out. It
recomputes the per-call secrecy flag from the labels and refuses (`Denied { by: secrecy }`,
before the host sees anything) a labelled value at every sink of chapter 03. Its rules fail
closed, from the checker's table: a tool or operation is a sink unless the table marks it
`@local` or `@pure` (so `web_*`, `run_command`, `run_background`, every `agents.*` built-in,
memory writes, `session.emit`, `eval` metrics and any built-in the table does not know are
sinks); `tools.call_tool`, `tools.mcp_call` and the dispatch built-ins are sinks whatever they
name; `write_file` and `edit_file` may write a secret only to a path in `ratio.secret_paths`;
a model whose card does not say `local` may receive no secret (a prompt's sections included,
so a secret reaching a hosted model through a prompt's `data` is refused at run time), and
neither may a call on a labelled conversation (`conversation_labels`; an unreadable one counts
as labelled). Logs, traces and streams pass with the flag set, for the host to redact. The
labelled parts of an approval's summary and detail, and of a `@user` built-in's arguments (`session.ask`'s
questions), reach the host as `[secret: source N]`. It
also repeats `frame::check_within` on every frame it forwards.

**The scheduler** ([`src/driver.rs`](src/driver.rs)) is the `Evaluation` future. `par { … }`,
`race { … }`, `par([…])`, `race([…])`, `par_map` and `par_map_settled` suspend their machine
(`Suspend::Branches`, [`src/fork.rs`](src/fork.rs)); each branch becomes a machine of its own
(`Machine::spawn`: the same program, heap ledger and frames with the branch element on the
path, the locals it may read, its own step count), and the construct is answered with
`Machine::join`. All the tasks are polled on the thread that polls the evaluation, and their
effects run concurrently as host futures. A task holds a running slot on its frame's ledgers
while it evaluates or awaits a host effect; a task waiting for its branches is parked and gives
its slot back, and resumes ahead of every other waiter. Waiting branches start in branch-path
order within each construct's `limit:` (default 4 for `par_map`). Every tool call is admitted
with one `tool_calls` and every model call by reservation (`count_tokens`, the card from
`resolve_model`, the effective `max_tokens` the request is sent with; below the reply floor it
waits on `Ledgers::register_waker`, never blocking the thread); a refusal raises `Budget`.
Branches debit the same ledgers between their slices, so a frame's `steps` cap binds them all.

A branch is cancelled (a race lost, a `par` sibling failed, its construct cancelled) at its next
effect boundary or checkpoint. An `abort` call in flight is stopped and the branch waits for the
host's acknowledgement; a `drain` call is left running as an outstanding drain, re-parented to
the frame of the nearest enclosing construct whose task is not itself being cancelled (or the
turn's), and the branch unwinds at once, so harness frames inside the cancelled scope close at
once. The fence (`fork::Fence`) then holds that frame's close (the machine reports
`Suspend::Fence`), every later `drain`-class call of the frame tree (`defer` blocks and
`tools.command_stop` excepted) and the end of the turn, until the drain finishes and is debited
its actual charge. The scheduler bounds calls itself, whatever the host does: a `drain` call
(an engine write operation too) is failed at its declared bound (at most 3600 s; an
outstanding one releases the fence there, recorded `failed`), and a cancelled `abort` call the
host has not acknowledged within `ABORT_GRACE` (30 s) is recorded `unacknowledged`, charged
all its reservation held, and its scope returns. Each bound is the earlier of the host's timer and
Ratio's own clock, so a host whose timer never fires cannot lift it. Effects in `defer` blocks are not cancelled. `with timeout d { … } else { … }`
and `timeout(d, f)` are scopes of their machine (no branch element on the path): the scheduler
sets the deadline when it first sees the scope, arms the host's `timer`, and fires the scope by
cancelling it, which unwinds to it (running `defer` blocks) and becomes the `else` block or
`Timeout`. Every effect carries the earliest deadline of its scopes, its ancestors' and its
frame's `wall` budget (measured from the start of the evaluation). A `return`, `break` or
`continue` cannot leave a branch (`Invalid`).

**`ScriptedHost`** ([`src/scripted.rs`](src/scripted.rs)) answers from scripts: a tool table
(class, declared bound, behaviour: a reply, a delay, a failure, never, a barrier that answers
only under real concurrency, a sequence, answers by first argument, an approval gate), model
replies and cards, operation and approval answers, secret paths and the session's scope. It
checks frames against its own records (a forged parent or kind is refused), enforces the sinks
from the per-call flag as an engine's dispatch does, honours tokens by class, bounds calls by
deadlines and declared bounds, and records what it saw (calls with their start and end times
and deadlines, outcomes, events, frames). `jitter(seed)` delays answers by pseudo-random numbers
of polls, for random completion orders.

**The contract suite** ([`src/host_tests.rs`](src/host_tests.rs), feature `host-tests`) is what
every host must do, driven through the raw trait without the guard: `frames` (a child wider than
its parent in any dimension is refused, against the host's record, also under a forged parent
or kind), `sinks` (the network, command and write tools and a hosted model refuse a call with
the secrecy flag set) and `cancellation` (an abort call is aborted, a remote one abandoned,
every listed tool declares a bound of at most 3600 s and unlisted or server-claimed read-only
tools are `drain`, a never-ending drain call is failed at its bound, a call ends at its
deadline, an approval-pending call is withdrawn and a later answer changes nothing, an answer
first runs the call, a session stop drains). A host supplies a `Fixture`; `ScriptedFixture` is
`ScriptedHost`'s. The scheduler's own cancellation cases (race and the fence, re-parenting,
`defer`, a command killed by its timeout but not by a race) are `tests/scheduler.rs`.

**The tape** ([`src/tape/`](src/tape/mod.rs), `TAPE_VERSION` 1) is a JSON Lines file
(uncompressed: `.jsonl.zst` compression is the host's, applied to the file): a header (tape and
IR versions, the library version, the mode, the turn's seed, the session frame's caps and
width), blob lines (a body above 1 KiB, once, named by its BLAKE3 digest; in a headless tape a
labelled body is not stored and is named only by the redaction digest, HMAC-SHA-256 with the
session's key, which `RecordOptions::new` requires and refuses when all zero; in a headless tape
every request, output and reply digest over labelled data is keyed the same way, `k:<hmac>`),
entry lines for input-bearing effects (key, request digest over the built-in and
its named arguments without labels, and the value or error with its labels), event lines
numbered on one turn sequence, digest-only lines for output-only effects in `full` mode, and the
end line with the reply digest. The events: frame opens with their caps, admissions with their
reservation and the inputs of their arithmetic, waits, refusals, completions, each branch's step
total, cancellation points `(effects, steps)`, how cancelled calls ended (a drained call's
discarded answer is its entry), re-parentings, race winners, failing `par` branches, fired
timeouts and branches that never started. Approvals and every `@user` built-in (`session.ask`) are never answered from a tape: a replay asks the user live, under its own
session's rules, and a declassification's request digest covers a keyed digest of what was
declassified. Recording stops at the size limit (64 MiB, blobs and held events counted); the
tape then says it was truncated and replay refuses it. `TapeHost::record` writes it; `TapeHost::replay`
loads it within fixed bounds (64 MiB, 2^20 lines, nesting 64, numbers of at most 20 digits),
checks every blob against its digest and the versions, then verifies it against itself:
numbered without gaps; the ledgers rebuilt from the frame events reproduce every admission (its
held amounts and effective `max_tokens`), refusal (by the same budget) and completion in
sequence order; the step totals stay within the turn's limit; every outcome agrees with the
entries. In replay, effects are answered by key after the request digest is checked (lenient
mode falls through to a live call), answers wait permuted numbers of polls when asked,
admissions, refusals, cancellation points, timeouts, winners and failing branches come from the
tape, each branch stops at its recorded step total, output-only effects are suppressed (the
replay never calls `emit`), replayed values keep their labels and gain those the replaying
host's source rules give them (`Host::source_labels`), and the reply digest is compared at the
end. Any failure raises `Replay`.

**Per-branch random generators** ([`src/rng.rs`](src/rng.rs)): `ChaCha20` keyed by
`BLAKE3(seed, frame path, 0, branch path)`, for stage 11b's `rand`. BLAKE3 is
[`src/builtins/blake3.rs`](src/builtins/blake3.rs), the same file stage 9's `hash` uses.

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

Frames: `conformance.rs` also runs `conformance/frames/` (harness calls, configuration and
overrides, attenuation of tools, modes, locations, `sees`, attended and context, scoped
`steps` and depth budgets caught only outside their scope, questions, `meta`, frame paths);
`tests/attenuation.rs` is the monotonicity property over random parents and requests in every
dimension (with a folder-containment oracle written over the source strings, independent of
`Folder::contains`), the check refusing a child widened in any dimension and a child of a
forged parent, and the folder rules; `tests/ledgers.rs` checks the effective `max_tokens` formula, the reply floor, unpriced
models under cost caps, and that concurrent model calls never pass a token cap (in a frame or a
child frame with a lower cap) under a deterministic interleaving harness and under threads;
`tests/frames.rs` covers the `sees` refusals of labelled arguments, results and errors, the
frame context effects carry, and harness entries in a host's session; `tests/regressions.rs`
holds the security review's probes (effects in defaults and configuration, secrets in
settings and in blocks, bounded and charged opening, freed ledger nodes, which scope catches a
`Budget`).

Stage 11a: `tests/scheduler.rs` (real concurrency through a barrier only concurrent calls pass;
running branches never above the width and starting in branch-path order; nested fan-out at
width 2; a `race` returning before a losing `drain` write, which a later write and the turn wait
for; a cancelled nested harness closing at once with its write re-parented; a `defer` write not
fenced; a command killed when its timeout fires, with the deadline the host received, and not
aborted by a `race`; an abort call aborted; concurrent calls never passing a `tool_calls` cap;
every secrecy sink refused at the guard before the host, and the allowed flows passing with the
flag set); `tests/contract.rs` (the contract suite against `ScriptedHost`, and the guard refusing
in front of a host that checks nothing); `tests/replay.rs` (`conformance/replay/` recorded under
four completion orders and replayed under three permutations each, with identical results and
event streams, and the crafted step totals; the determinism property over sixteen completion
orders with budgets below demand; every divergence mutation: a sequence number, a refusal, an
input count, a card limit, a price, a step total, a drained and a withdrawn outcome, a blob;
suppression and the reply digest, and `full` mode; labels kept and gained in replay; labelled
blobs of a headless tape named by the redaction digest only; crafted tapes; the tape's size);
`conformance/eval/width-nested-fanout.ratio`; and `src/rng.rs`'s `ChaCha20` vector.

The security review of stage 11a, round 1: `tests/review_11a.rs` (every model built-in guarded and
admitted; the guard failing closed; approvals never answered from a tape; keyed digests in
headless tapes and the required key; truncated tapes refused; the scheduler bounding drains and
unacknowledged aborts on a host that breaks the contract; frame kinds from the host's own
knowledge; approvals redacted; labelled conversations refused to hosted models).
