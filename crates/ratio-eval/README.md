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
cannot be assigned inside it (`Invalid`). Harness calls, `with` blocks, call overrides, `with
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
(`Invalid` beyond), so every walk, comparison and drop of a value fits a small stack.

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
the operands' weights; `in` and indexing `Json` linear, list `+` and spreads `log`. The **heap**
ledger charges every allocation a fixed, platform-independent size before it is made and credits
it when the value is freed, so it counts live values. `Budget` and `Cancelled` pass through every
`catch`; `defer` blocks run on every exit, in reverse order (the first error wins when one
raises).

`builtins::implemented()` lists the built-ins this stage carries, one or more per cost class:
the pure functions of the checker's table, plus `text.repeat` (`output`) and `text.similarity`
(`product`), which programs can call once the checker's table declares them.

## Tests

`tests/conformance.rs` runs `conformance/eval/` (`ok`, `value`, `runtime-error` and `steps`
expectations); `tests/labels.rs` covers label propagation for every operator and built-in,
`Json`, `declassify` and the sink check; `src/properties.rs` checks the persistent structures
against models and the summary property (including labels a replay adds; the `ratio_cache`
restore is a stub until stage 11b); `tests/limits.rs` covers the cost classes, the heap ledger,
call and value depth, hostile programs on a 256 KiB stack, cancellation, and a 5,000,000-step
loop that lets a second session progress on a single-threaded executor.
