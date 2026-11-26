# ratio-eval

Ratio's core interpreter: the second half of stage 6 of the
[implementation plan](../../spec/10-implementation.md). Hosts reach it through the
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
swallow. `with timeout` and `state` raise `Unsupported` until the host (stage 11) exists;
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
(`Invalid` beyond), so every walk and comparison of a value fits a small stack; dropping a value
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

`builtins::implemented()` lists the built-ins this stage carries, one or more per cost class:
the pure functions of the checker's table, among them `text.repeat` (`output`: a negative count
or a result over 16 MiB is refused before anything is built, with checked arithmetic) and
`text.similarity` (`product`: two rows over the shorter text, charged to the heap, checking
cancellation every checkpoint's worth of cells).

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
around its body. The configuration record is read into a `Request` (overrides win field by
field, and within `budget`, `limits`, `sees` and `permissions` sub-field by sub-field; `deny`
and `ask` rules of both apply); a value that cannot be read exactly (a negative cap, a tool
pattern with an inner `*`, a path with `..`, a width of 0) raises `Invalid`.

**Attenuation.** `frame::attenuate(parent, room, request, kind)` gives the child's `Scope` as
the meet of the parent's and the request in every dimension: tools (names and prefix patterns
intersected exactly; a missing `need` tool is `Denied { by: attenuation }`, a missing `want`
tool is dropped), the permission mode (`plan < ask < default < auto, accept_edits < full`),
rules (the caller's plus the callee's `deny` and `ask`, never an `allow`), paths (folders
intersected component-wise), location (`local` if either is; asking for `cloud` under `local`
is refused), each budget cap (at most the request, the parent's cap and what the parent's
ledgers have left), `sees` (fresh and `none` frames see nothing unless they ask; fork frames,
blocks and entries inherit; asking beyond the caller is refused), depth (one more per harness
call; past the limit, `Budget { which: depth }`), width, heap, attended (an unattended caller
cannot open an attended frame), memory writes, and the context mode (`main` only in an entry's
own frame). `frame::check_within(parent, child, kind)` is the independent check, a pure
function of two scopes: the machine runs it on every frame it opens, and a host runs it
against its own record of the parent (Imperium's `RatioHost::open_frame`), so a forged parent
scope cannot widen a child.

**`sees` and secrets.** A labelled argument entering a frame whose `sees` lacks `secrets`, a
labelled result or error leaving a frame into one, and an effect answered with a labelled
value (or a labelled error) in one, raise `Denied { by: sees }`. The ambient reads chapter 04
gates (`session.turn`, `session.plan`, `memory.index`, …: `frame::gate`) are refused before
the host is asked.

**Budgets and the ledgers.** `frame::Ledgers` is the frame tree's ledgers, one node per frame
under its caller's, behind one lock. An effect is admitted by reserving its charge on its
frame's node and every ancestor's at once: a tool call one `tool_calls`; a model call one
`model_calls`, its counted input and its effective `max_tokens` (the smallest of the requested
`max_tokens`, the model's maximum output or window less the input, and what every ledger can
still cover in tokens and in cost after the input), which the host sends. Below the reply
floor (the smallest of `min_reply_tokens`, the request and the model's maximum, at least one)
an admission waits while any reservation is held and is refused once none is; a hosted model
without prices is refused under any cost cap on the path, and a `local` model or a price of 0
counts as priced. Completion replaces the reservation with the actual charge, so unspent
budget stays with the caller; every sum saturates towards "spent". The `steps` ledger is the
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

## Tests

`tests/conformance.rs` runs `conformance/eval/` (`ok`, `value`, `runtime-error` and `steps`
expectations); `tests/labels.rs` covers label propagation for every operator and built-in,
`Json`, `declassify` and the sink check; `src/properties.rs` checks the persistent structures
against models and the summary property (including labels a replay adds; the `ratio_cache`
restore is a stub until stage 11b); `tests/limits.rs` covers the cost classes, the heap ledger,
call and value depth, hostile programs on a 256 KiB stack, cancellation, and a 5,000,000-step
loop that lets a second session progress on a single-threaded executor.

Frames: `conformance.rs` also runs `conformance/frames/` (harness calls, configuration and
overrides, attenuation of tools, modes, locations, `sees`, attended and context, scoped
`steps` and depth budgets caught only outside their scope, questions, `meta`, frame paths);
`tests/attenuation.rs` is the monotonicity property over random parents and requests in every
dimension, with the check refusing a child widened in any dimension and a child of a forged
parent; `tests/ledgers.rs` checks the effective `max_tokens` formula, the reply floor, unpriced
models under cost caps, and that concurrent model calls never pass a token cap (in a frame or a
child frame with a lower cap) under a deterministic interleaving harness and under threads;
`tests/frames.rs` covers the `sees` refusals of labelled arguments, results and errors, the
frame context effects carry, and harness entries in a host's session.
