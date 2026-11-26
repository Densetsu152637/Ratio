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

## Stage 9: `math`, `hash`, `diff`, `template`, `vec`, `path`, `url`, `codec`

The data III built-ins of chapter 06 (`data`), 64 of them, in `src/builtins/<module>.rs` with the
shared helpers in `builtins/data3.rs` (argument reading, refusals, heap-charged buffers and the
16 MiB text builder). Every one is pure, raises `Invalid` for what chapter 06 says it refuses (a
refusal is made before anything proportional to the input is allocated), and gives its result
the labels of everything that decided it: the arguments (a list's elements, a map's keys and
values count), what a lookup consulted, and for a failure's message the data it describes.

| Module | Built-ins and their classes |
| --- | --- |
| `math` | `const`: `abs sign min max clamp floor ceil round trunc to_float sqrt pow exp ln log2 log10 is_nan`. `linear`: `mean variance stdev softmax argmax argmin entropy`. `nlogn`: `median percentile mode` |
| `hash` | `linear`: `sha256 blake3 stable short` |
| `diff` | `product`: `lines words unified`. `linear`: `apply stats` |
| `template` | `linear`: `render names` |
| `vec` | `linear`: `dot norm normalize cosine add mean`. `nlogn`: `top_k` |
| `path` | `linear`: all eight, on the characters read |
| `url` | `linear`: `parse host with_query encode decode` |
| `codec` | `linear`: `base64_encode base64_decode hex_encode hex_decode csv_parse csv_write` |

**Floats are deterministic.** `+ - * /`, `sqrt`, `floor`, `ceil` and conversions are exactly
specified by IEEE 754; `exp`, `ln`, `log2`, `log10` and `pow` are not (a C library or a CPU
rounds the last bit its own way), so `builtins/fmath.rs` carries them from fdlibm, written with
only the exactly specified operations and bit manipulation (no `f64::exp` and friends, no fused
multiply-add). They are accurate to under one unit in the last place (within one of the platform's
library in the tests, exact where the answer is exactly representable: `pow(2, 10)`,
`log2(8)`, `log10(1000)`), O(1), and their results are pinned by bit pattern (a handful of values
and a checksum over 20,000 inputs per function) in debug and release builds, so a platform whose
compiler or library changes one bit fails CI. A `NaN` result is the canonical quiet `NaN`
(`0x7ff8000000000000`: the hardware's choice of sign differs), decided on the bits because the
optimiser may treat every `NaN` as one. `math.round(x, digits)` rounds half away from zero on the
**exact** binary value (`round(2.675, 2)` is 2.67, `round(0.125, 2)` is 0.13), by an exact
two-product scaling. Statistics use a compensated sum in list order; those of `Duration` and
`Money` are in seconds and whole units. `vec` computes lengths and cosines on the vector scaled by
its largest component when the plain sum of squares would overflow or underflow.

**Hashes.** `hash.sha256` is `ratio_check::sha256`; `hash.blake3` is written in-crate
(`builtins/blake3.rs`) and checked against the reference implementation's digests of the
standard inputs at the lengths that cross every boundary of its tree. `hash.stable` hashes the
value's canonical JSON: keys sorted, no spaces, `Float` in its shortest round-trip form,
`none` fields left out (so an absent optional field and one set to `none`, which are equal values,
hash alike), written by an iterative walk with an explicit stack. It is a task that **pays for
what it writes**: a number weighs 1 but writes up to 24 bytes, so when the work passes what the
class covers it asks the evaluator for more steps first, in doubling instalments (like
`text.show`'s charge for what it wrote).

**Diffs.** `diff.lines`, `words` and `unified` are charged `n₁ · n₂ + c₁ + c₂` before they run, so
two 20,000-line files raise `Budget { which: steps }` without a line being read past the
counting. The algorithm is a shortest edit script by Hirschberg's linear-space dynamic program
(an explicit stack, two rows of `m + 1` counters, at most `2 · n · m` cell updates, cancellation
checked every checkpoint's worth), after trimming the common prefix and suffix. Chapter 06 says
"(Myers)"; Myers' greedy algorithm is `O((n + m) · d)` and exceeds the product class on two very
unequal inputs, so it cannot be the bound; the script has Myers' length. `apply` is exact (no
fuzz, no offset) and refuses a patch for more than one file; the line splitting is `text.lines`'.

**Templates.** `template.render` is a task: the template is parsed into a flat node list and
walked with an explicit stack, so a template can reach nothing but its two arguments and no
nesting reaches the Rust stack. Outer scopes are searched for a name, which can make the walk do
more than `n + m` work (a section that writes nothing, over two lists, one inside the other); the
walk pays for that in doubling instalments *before* doing it, so a budget that cannot pay stops it
early, and a template whose tags all write something is charged exactly the class's formula.

**URLs** follow the WHATWG standard for what `url.parse` accepts and answer `none` for everything
else it is not certain to read the way that parser does (the module's documentation lists what is
refused: whitespace and backslashes, non-ASCII hosts, a second `@`, a port over 65,535, a URL over
8 KiB). Hosts that end in a number are IPv4 addresses in any spelling and are written in dotted
decimal; IPv6 is written compressed in brackets; default ports are dropped; `.` and `..` segments
resolve; the query map takes the last of repeated keys. **Paths** are lexical: `normalize` keeps a
leading `..` of a relative path and drops it at a root, and `relative_to` never returns a path that
leaves its base.

**Limits.** Output built piece by piece (`TextBuf`) stops at 16 MiB and pays for what it wrote;
every buffer proportional to an input (a vector copied, a token table, a component list, a sorted
copy) is charged to the heap before it is allocated; an encoder knows its output size from its
argument and refuses over 16 MiB before allocating. Nothing recurses over a value's depth.

**Tests** for this stage: `tests/data3.rs` runs `conformance/data/<module>-<built-in>.ratio`, one
file per built-in with the values, failures and label propagation of its chapter 06 row (the
directives are stage 7's); `tests/data3_labels.rs` labels every argument of every call, at each
level a label can sit, and checks the result carries it (and a failure's message the labels of
what decided it); `tests/data3_limits.rs` pins the charge formulas, the refusal of large diffs
before they run, the heap and size refusals, the work counters (debug builds count the work each
built-in does and the test holds it to 16 units per unit of charge, 64 to a step), the
shortest-script property of the diffs and the float bit patterns.

## Tests

`tests/conformance.rs` runs `conformance/eval/` (`ok`, `value`, `runtime-error` and `steps`
expectations); `tests/labels.rs` covers label propagation for every operator and built-in,
`Json`, `declassify` and the sink check; `src/properties.rs` checks the persistent structures
against models and the summary property (including labels a replay adds; the `ratio_cache`
restore is a stub until stage 11b); `tests/limits.rs` covers the cost classes, the heap ledger,
call and value depth, hostile programs on a 256 KiB stack, cancellation, and a 5,000,000-step
loop that lets a second session progress on a single-threaded executor.
