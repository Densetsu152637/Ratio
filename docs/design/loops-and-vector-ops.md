# Loops, vectorised operations and functional syntax

Task DESIGN-LOOPS (user request: "make looping and vectorised operations compatible and
optimised", widened to "also add in other function syntax"). Reviewed against `main` at
`4837678` (stages 1 to 7, specification draft 0.14) and the in-flight branches
`feat/wt-s8-data2` (`1278e59`), `feat/wt-s9-data3` (`834e7db`), `feat/wt-s10-frames`
(`f1f2429`), `feat/wt-sC-checker` (`79b1e37`) and `feat/wt-s11a-host` (local, no commits of its
own yet beyond the stage 10 base `b390cea`). Status: proposal for root and the user; nothing in
`spec/` or the code is changed by this document.

## Scope and method

Ratio offers two ways to walk data: **loops** (`for` over lists, ranges and maps; `while`,
`while let`, `loop` and `repeat`; with `break`, `continue` and `var` accumulators) and
**vectorised forms** (the callback built-ins `list.map`, `filter`, `fold`, `any`, …; the pure
aggregates `list.sum`, `math.*` statistics and `vec.*`; `par_map` and `par_fold`; pipes such as
`xs |> list.map(f)`). Nothing today says how the two must agree, and both are slower than they
need to be. This document

1. defines **equivalence**: when a loop and a vectorised form must give the same values, labels,
   errors, charges and float bits, and exactly where they may differ (E-1 to E-6);
2. measures the **baseline** on `main` and stage 9;
3. ranks **optimisations** (LV-1 to LV-10), each keeping the hashed Core IR (`IR_VERSION` 1,
   fixed by v0.1.0) apart from the prepared form;
4. proposes and ranks **functional syntax** (FS-1 to FS-11), all desugared in lowering, so no
   IR node and no `IR_VERSION` bump;
5. sequences the work around the in-flight branches so no two touch the same file at once.

Read in full or in the relevant ranges: `crates/ratio-eval/README.md`; `machine.rs` (the walk,
loop frames, `call_builtin`, `drive_task`, `enter`), `prepare.rs` (`PKind`), `value.rs`
(`Repr`, `Meta`, labels), `plist.rs`, `pmap.rs` (cursors), `heap.rs`, `labels.rs`,
`builtins.rs`; stage 8's `builtins/list.rs`, `work.rs` and its machine diff; stage 9's
`builtins/vec.rs` and `math.rs`; stage 10's machine diff; `spec/02-syntax.md` (grammar,
disambiguation, canonical form), `spec/03-semantics.md` (control flow, implicit flows,
secrecy, concurrency), `spec/06-stdlib/data.md` (cost classes, `list`, `map`, `math`, `vec`),
`control.md` (`par_map`, `par_fold`), `spec/07-security.md` (limits, step accounting),
`ratio-check/src/lint.rs` (`C0039`), `ratio-ir/src/lower.rs` (pipe lowering); PC-4, PC-5 and
PC-6 of the [performance review](performance-concurrency-review.md).

Numbers in the baseline are **measured** (release build, see [Baseline](#baseline)). Gains
marked *prototype* come from a throwaway patch measured on the same benchmark; gains marked
*estimate* are reasoned from the code and the baseline, not measured.

The security invariants found by earlier reviews are the acceptance bar for every proposal:
charge before work; no Rust-stack recursion over data depth; labels from everything that
decides a result (misses and composite keys included); heap-charged buffers; the 16 MiB output
stop; cancellation checkpoints in long built-ins. Each proposal states its effect on them.

## Summary

| ID | Title | Kind | Recommendation | Stage | Risk |
| --- | --- | --- | --- | --- | --- |
| E-1–E-6 | Equivalence rules for loops and vectorised forms | Spec errata | Accept | P0 (after 9) | Low |
| LV-1 | Borrow the prepared form instead of cloning its `Arc` per node | Interpreter | Accept | P2 (after 11a) | Low |
| LV-2 | One slot stack and no per-call vectors | Interpreter | Accept | P2 | Low |
| LV-3 | Direct evaluation of straight-line pure lambdas | Interpreter | Accept | P2 | Medium |
| LV-4 | Batched heap credit for frames and buffers | Accounting | Accept | P1 (buffers), P2 (frames) | Medium |
| LV-5 | Tuple-free `for (k, v) in m` | Interpreter | Accept | P2 | Low |
| LV-6 | Leaf-slice walks in built-ins | Data | Accept | P1 | Low |
| LV-7 | Cancellation polling in `vec` and `math` | Security fix | Accept | P1 (or stage 9 review) | Low |
| LV-8 | Last-use moves and in-place appends for `xs = list.push(xs, x)` | Interpreter, data | Accept, after LV-1–3 | P2b | Medium |
| LV-9 | Packed `f64` leaves for homogeneous `[Float]` | Data | Defer | after 24 | Medium |
| LV-10 | Fusing `filter`/`map` chains in the prepared form | Interpreter | Defer | after 24 | Medium |
| — | Per-element step discounts for callback built-ins | Cost model | Reject | — | — |
| — | Parallel chunked float reductions on the pool | Pool | Reject | — | — |
| FS-1 | List comprehensions | Syntax | Adopt | F (after C and 10) | Low |
| FS-2 | Placeholder closures and partial application (`_ + 1`, `f(_, 2)`, `_.name`) | Syntax | Adopt | F | Medium |
| FS-3 | Method stages in pipes (`xs \|> .map(f)`) | Syntax | Adopt | F | Low |
| FS-4–FS-11 | Composition, multi-clause functions, `where`, `scan`, lazy sequences, … | Syntax, stdlib | Options for the user | — | — |

## Equivalence rules

### What the code does today

- **Loops.** `for` takes a cursor over the list (`plist::Cursor`), the map (`pmap::MapCursor`) or
  the range, held in the `K::Loop` frame; no per-step allocation for lists and ranges, one
  heap-charged tuple per map entry. Each element read gets the container's own labels
  (`machine.rs:2627-2648`). One step per iteration (`loop_next`), plus the nodes of the pattern
  and the body. `for` over text is not in the language (`spec/03-semantics.md:427`; the
  machine raises `Invalid`); `text.chars(s)` is the route.
- **Callback built-ins** (`list.map`, `filter`, `fold`, …) are `Task` state machines the walk
  drives, so callbacks run on the evaluator's heap stack. They are charged `1 + ⌈n/64⌉` before
  running and `⌈m/64⌉` on return (`linear`), and the callback's own nodes as any code.
  Stage 8's `Each` task gives every result the labels of whatever decided it: the list's own
  labels, and the labels of every callback result that chose which elements were kept or where
  the scan stopped (`builtins/list.rs:508-680` on stage 8).
- **Pure aggregates.** `list.sum` adds in index order with plain `+` and labels the result with
  `xs.quick()` (every label inside). Stage 9's `vec.dot`, `add` and the first pass of `norm` and
  `cosine` also add in order with plain `+` and `*`; `math.mean`, `variance`, `stdev`,
  `softmax` and `entropy` use **Neumaier's compensated sum**; `vec.norm`, `normalize` and
  `cosine` rescale by the largest component when the plain sum of squares overflows or
  underflows, and `cosine` clamps to `[-1, 1]`.
- **Implicit flows.** Branching on a labelled value is outside the guarantee
  (`spec/03-semantics.md:228-236`, `275`): `if secret { out = out.push(x) }` gives an `out`
  without the condition's labels, while `list.filter` puts the predicate results' labels on the
  result list. So the vectorised form is *stronger* today, and the rules below make that the
  stated direction.

### Proposed rules (errata)

A **canonical loop** of a vectorised form is the loop chapter 06 would write for it: index order,
one call of the callback per element, the accumulator updated with the same operator, stopping
where the built-in stops (`any`, `all`, `find`, `take_while`). The rules compare a built-in with
its canonical loop on the same arguments.

**E-1 Values.** Equal, structurally, for every built-in of `list` and `map` and for `list.sum`,
`vec.dot` and `vec.add`; `Float` results are bit-identical, because both sum in index order with
plain IEEE 754 binary64 operations and no fused multiply-add. Measured: `list.sum` and a loop
over 100,000 floats both give `0x40ed45c000000000`; `vec.dot` and the loop `s += a[i] * b[i]`
both give `0x40b6f3013953def9`.

**E-2 Documented float differences.** These built-ins are *not* their naive loop, and the spec
says so in their rows:

| Built-in | Differs from the naive loop because | Example (corpus pair 10) |
| --- | --- | --- |
| `math.mean`, `variance`, `stdev`, `softmax`, `entropy` | Neumaier-compensated sum in index order; falls back to the plain sum once a partial sum is infinite | `[1e16, 1.0, …]` cancelling: mean `0x3fd5555555555555` (1/3), loop `0x0` |
| `math.mean` on `[Int]` | each element converted to `Float` before summing (no `Int` overflow; inexact past 2^53) | a loop summing `Int`s raises `Invalid` on overflow |
| `vec.norm`, `normalize`, `cosine` | rescaled by the largest component when the plain sum of squares overflows or falls below 1e-290; `cosine` clamped to `[-1, 1]`, 0.0 for a zero vector | `vec.norm([1e200, 1e200])` is finite; the loop gives `inf` |
| `vec.mean`, `vec.top_k` | as `math.mean` and `cosine`, per component | — |

Every one of them is still **deterministic**: a fixed order of plain binary64 operations, so the
bits are the same on every machine and in replay. The rule the spec states is: *a built-in
whose row names a summation method gives the bits of that method in index order; every other
`Float` aggregate gives the bits of the naive index-order loop.*

**E-3 Labels.** `labels(built-in) ⊇ labels(canonical loop)`, at every level (the result's own
labels, each element's, the summary). They are equal when the loop takes no decision on a
labelled value: no `if`, `match`, `while` condition, `break` or `continue` decided by a labelled
value, and a container with no own labels. The built-in may carry more in exactly two cases,
both implicit flows the loop does not track: (a) callback results that decided a choice
(filter, find, any, all, count, partition, take_while, sort keys); (b) the container's own
labels on an empty or length-dependent result (`list.sum([])` carries the list's own labels;
the loop's `0` does not). A pure aggregate's result carries `xs.quick()`, which equals the union
of what the loop's accumulator collects for a non-empty list.

**E-4 Errors.** For the sequential forms, the same **kind** is raised at the same **element**
(the first, in index order, whose callback or operation fails), and nothing after it runs.
Messages and sites may differ. Three exceptions:

- `Budget { which: steps }`: a built-in debits its class charge before its first callback, so it
  can raise at a different point than the loop (earlier when the charge does not fit, possibly
  later when its per-element steps are fewer). Effects made before the raise may therefore
  differ; both are deterministic, so replay is unaffected.
- `Budget { which: heap }`: the forms allocate differently (a buffer of slots against persistent
  pushes), so the point of exhaustion may differ.
- `par_map` and `par_fold`: the error is the first failure **in the turn sequence** (recorded,
  PC-1), which equals `list.map`'s when at most one element fails; `par_map_settled` returns
  every outcome by index.

**E-5 Steps.** For a pure built-in `B` with canonical loop `L` over `n ≥ 1` elements,
`steps(B) = 1 + ⌈w/64⌉ (+ ⌈m/64⌉ for a linear output) ≤ steps(L)`, since `L` pays `n + 1`
iteration steps plus at least a pattern and a body node per element (`3n + 1`), and `w` is at
most about `2n + 2` (the weight of two vectors). For a
callback built-in, `steps(B) ≤ steps(L) + 1 + ⌈n/64⌉ + ⌈m/64⌉`: the callback's nodes replace the
loop body's, and its parameter patterns cost what the loop's pattern and iteration step cost
(measured: `list.fold` 5.02 steps per element, the `for` loop 5.00; `list.map` 4.03 against
11.00 for the loop that pushes). Both stay within the existing work bound: the debug-build work
counters stay within 8 × 64 units per charged step (stage 7's check), which is the "fixed multiple
of the real work".

**E-6 Determinism and replay.** Every form's value, labels, steps and heap peak depend only on
the program and its data; offloading (PC-6) and the optimisations below change none of them.
`par_map` and `par_fold` record what depends on timing (PC-1).

### Where the errata go

| File:line | Text |
| --- | --- |
| `spec/03-semantics.md:431` (after "Every loop iteration costs steps") | New bullet: "A loop and the vectorised built-in that does the same work (chapter 06, `data`, *Loops and built-ins*) give the same values and raise the same error kind at the same element; the built-in's labels include the loop's, and add those of every callback result that decided a choice, an implicit flow the loop does not track." |
| `spec/06-stdlib/data.md:65` (after *Charges never depend on representation state*) | New paragraph **Loops and built-ins** with E-1, E-3, E-4 and E-5 as above, and the definition of a canonical loop. |
| `spec/06-stdlib/data.md:215` (`list.sum` row) | Semantics: "Sum in index order with plain `+` (the bits of the loop)". |
| `spec/06-stdlib/data.md:262` (`math` intro) | Add: "Sums inside `mean`, `variance`, `stdev`, `softmax` and `entropy` are Neumaier-compensated, in index order, falling back to the plain sum once a partial sum is infinite; `mean` of `[Int]` converts each element first. Their bits are fixed but differ from a naive loop's." |
| `spec/06-stdlib/data.md:363` (`vec` intro) | Add: "`dot` and `add` sum in index order with plain operations (the bits of the loop). `norm`, `normalize` and `cosine` rescale by the largest component when the plain sum of squares is not finite or below 1e-290; `cosine` is clamped to `[-1, 1]` and is 0.0 for a zero vector." |
| `spec/06-stdlib/control.md:22` (`par_map` row) | "the first failure (recorded)" becomes "the first failure in the turn sequence (recorded; `list.map`'s when only one element fails)". |
| `spec/07-security.md:281` (after "exceeds them identically in replay") | "Built-ins debit their class charge before their first callback, so the point at which a loop and its built-in exhaust the steps may differ; each is deterministic." |
| `spec/README.md` | Erratum line for the draft that carries these (see [Sequencing](#sequencing-and-file-ownership)). |

## Baseline

Benchmark: a scratch crate (not committed) linking `ratio-lang` by path, release profile,
`rustc 1.99.1`, Intel Core i7-13700KF, Windows 11; median of five runs, evaluation only (load,
check and prepare excluded); inputs passed as host values (unmetered); limits at their maxima
(100,000,000 steps, 512 MiB). `main` at `4837678`; the `vec` and `math` rows on stage 9 at
`834e7db` (its other rows match `main` within 5%).

| Case | n | Steps | Steps / element | ms | ns / step | ns / element |
| --- | --- | --- | --- | --- | --- | --- |
| `for x in xs { s += x }` | 1,000,000 | 5,000,010 | 5.00 | 422.0 | 84 | 422 |
| `for i in 0..n { s += i }` | 1,000,000 | 5,000,012 | 5.00 | 410.4 | 82 | 410 |
| `while i < n { s += i; i += 1 }` | 1,000,000 | 9,000,014 | 9.00 | 978.6 | 109 | 979 |
| `list.fold(xs, 0, \|a, x\| a + x)` | 1,000,000 | 5,015,634 | 5.02 | 611.0 | 122 | 611 |
| `list.sum(xs)` | 1,000,000 | 15,632 | 0.016 | 10.3 | 659 | 10.3 |
| `list.map(xs, \|x\| x + 1)` | 1,000,000 | 4,031,257 | 4.03 | 627.9 | 156 | 628 |
| loop: `out = list.push(out, x + 1)` | 1,000,000 | 11,000,009 | 11.00 | 3,713.0 | 338 | 3,713 |
| `xs \|> list.filter(even) \|> list.map(* 3)` | 1,000,000 | 8,039,073 | 8.04 | 1,147.7 | 143 | 1,148 |
| loop: `if even { out = list.push(out, x * 3) }` | 1,000,000 | 14,500,009 | 14.50 | 2,600.2 | 179 | 2,600 |
| build: `m = map.set(m, "k{i}", i)` | 100,000 | 1,795,915 | 17.96 | 632.5 | 352 | 6,325 |
| `for (k, v) in m { s += v }` | 100,000 | 700,010 | 7.00 | 83.9 | 120 | 839 |
| `list.fold(map.keys(m), 0, \|a, k\| a + 1)` | 100,000 | 504,698 | 5.05 | 76.7 | 152 | 768 |
| loop dot: `s += a[i] * b[i]` | 100,000 | 1,100,016 | 11.00 | 124.5 | 113 | 1,245 |
| loop float sum | 100,000 | 500,010 | 5.00 | 44.6 | 89 | 446 |
| `list.sum(floats)` | 100,000 | 1,569 | 0.016 | 1.0 | 646 | 10.1 |
| `vec.dot(a, b)` (stage 9) | 100,000 | 3,134 | 0.031 | 1.5 | 464 | 14.6 |
| `math.mean(xs)` (stage 9) | 100,000 | 1,570 | 0.016 | 0.5 | 313 | 4.9 |
| loop mean (stage 9) | 100,000 | 500,018 | 5.00 | 42.1 | 84 | 421 |

**Reading the table.**

- **Interpreted forms cost 80 to 350 ns per step.** Callbacks are dearer per step than loop
  bodies (fold 122 ns, map 156 ns against 84 ns for `for`) at the same charge: a callback call
  costs the same steps as a loop iteration but more real work (the arguments vector, an `Env`,
  `CallRet`, the `Task` resume). Persistent updates in a loop are the dearest (`list.push` 338 ns
  and `map.set` 352 ns per step): their `log` charge is two steps, but each copies a leaf of up to
  32 elements and recomputes its meta.
- **Built-in steps are 4 to 8 times dearer in wall time than interpreted steps** (313 to 659 ns
  for 64 elements), because a step pays for 64 elementary units there. This is within the
  "fixed multiple" the cost model promises, but the ratio grows as the interpreter gets faster
  (LV-1 to LV-3); stage 24's calibration should be done after them (open item O-3).
- **The vectorised forms already never cost more than 1 + ⌈n/64⌉ steps beyond their loop**, and
  the pure aggregates are 300 to 700 times cheaper in steps and 30 to 90 times faster.
- **Float bits** as in E-1 and E-2: `list.sum` and `vec.dot` equal the loop; `math.mean` equals
  it on benign data but on `[1e16, 1, …, −1e16, …]` (150,000 elements) gives the exact 1/3 where
  the loop gives 0.

**Prototype.** Two changes measured on a throwaway copy (unsafe code allowed there only):

| Case | Baseline ns / element | Without the per-node `Arc` clone (LV-1) | And without per-operand heap charges (LV-4 bound) |
| --- | --- | --- | --- |
| `for x in xs` | 422 | 269 (−36%) | 240 (−43%) |
| `while` | 979 | 663 (−32%) | 569 (−42%) |
| `list.fold` | 611 | 371 (−39%) | 341 (−44%) |
| `list.map` | 628 | 389 (−38%) | 432 (noise) |
| filter then map | 1,148 | 714 (−38%) | 661 (−42%) |
| push loop | 3,713 | 2,133 (−43%) | 2,243 (noise) |
| `map.set` build | 6,325 | 3,392 (−46%) | 3,881 (noise) |

The second column removes the heap charge entirely, so it is the *upper bound* of LV-4, not a
sound implementation.

## Optimisations

Every proposal below lives in the prepared form, the machine or the built-ins. None changes
the Core IR, its canonical serialisation, `IR_VERSION`, any hash, site id or effect key, or any
charge: steps follow the IR's nodes one for one (`prepare.rs:8-9`), and every fast path must
charge exactly what the general walk charges, before the work. A shared acceptance check for all
of them: the equivalence corpus (below) and the existing `conformance/eval` `steps`
expectations give identical values, labels, steps and heap peaks with each fast path on and off
(a test-only switch on `Prepared`).

### LV-1: borrow the prepared form instead of cloning its `Arc` per node

**Problem.** `eval`, `ret`, `enter`, `bind_params`, `drive_task` and ten other functions start
with `let p = Arc::clone(&self.p)` to satisfy the borrow checker: two atomic operations per node.

**Proposal.** Split `Machine` into the immutable `Arc<Prepared>` and a `State` holding
everything mutable; the walk's methods take `(&Prepared, &mut State)`. No `unsafe`.

**Gain.** −32% to −46% wall time on every interpreted row (*prototype*). **Cost.** Mechanical but
touches most of `machine.rs`. **Risk.** Low. **Security.** None: no charge, label or limit
moves. **Stage.** P2. **Files.** `machine.rs`.

### LV-2: one slot stack and no per-call vectors

**Problem.** Each call allocates the arguments `Vec<Value>` in `TaskStep::Call`, a
`Vec<Option<Value>>` in `drive_task`, and `Env { slots: Vec<Slot> }` in `enter`; three
allocations and frees per element of `list.map`.

**Proposal.** One `Vec<Slot>` for the whole machine with a base index per call (popped on
`CallRet`); `TaskStep::Call` carries up to three arguments inline (`SmallArgs`); `bind_params`
binds from that. Slot memory is already outside the heap ledger (frames' fixed part, at most
`call_depth` deep); the shared stack keeps that bound.

**Gain.** *Estimate* −15% to −25% per callback element after LV-1. **Risk.** Low (slot indexes
are already per-function). **Security.** Call depth stays checked before any slot is pushed.
**Stage.** P2. **Files.** `machine.rs`, `stack.rs`, `builtins.rs` (the `Task` protocol),
`builtins/list.rs`, `builtins/map.rs`.

### LV-3: direct evaluation of straight-line pure lambdas

**Problem.** A closure such as `|x| x + 1`, `|a, x| a + x` or `|r| r.score > 0.5` costs a full
call per element: `invoke`, `enter`, parameter matching, a `K::Collect` per binary node, `CallRet`
and the task resume (about 190 ns per element more than the same body in a `for` loop).

**Proposal.** At preparation, mark a function as **simple** when its parameters are plain
bindings or `_`, and its body is a tree of `Local`, `Const`, `Field`, `Unary`, `Binary`
(arithmetic, comparison), `And`, `Or`, `Coalesce`, `Index` on a list and calls of `const`-class
pure built-ins: no call of a function value, no effect, no loop, no `raise`, no closure creation,
no allocation beyond what those nodes make. Preparation compiles such a body to a flat
post-order array. A callback built-in given a simple closure evaluates that array in a small
loop with a value stack sized at preparation, **charging one step per node as it enters it,
exactly as the walk does** (short-circuit nodes skip the nodes the walk would skip), so the
steps, and the point where they run out, are the walk's. A fault raises exactly as the walk
would, with the same kind, message and labels, because the operations are the same `ops::*`
functions.

**Gain.** *Estimate* `list.map(xs, |x| x + 1)` from 628 to 60 to 120 ns per element; `fold` and
`filter` alike; the steps are unchanged. **Cost.** Medium: a mini-evaluator over prepared nodes,
reusing `ops`. **Risk.** Medium: a divergence between the two evaluators. Mitigated by sharing
the operations, by a differential property test (random simple lambdas, both paths, identical
value, labels, steps, error), and by the on/off switch. **Security.** Charge before work holds
(per node, as the walk); no recursion (a flat array and an explicit stack); labels come from
the same `ops` functions. **Stage.** P2. **Files.** `prepare.rs` (the mark and `c`), a new
`machine/fast.rs`, `builtins/list.rs`, `builtins/map.rs`.

### LV-4: batched heap credit for frames and buffers

**Problem.** Every value a frame gathers (`Held::push`) and every element a built-in buffers
(`Buf`, `Gather`) does a compare-and-swap, a `fetch_max` and an `Arc` clone on the shared
ledger, and the reverse on drop.

**Proposal.** (a) **Buffers** whose final size is known before the work (`list.map`,
`map_indexed`, `vec`, `math`, `sort`): charge `n` slots in one allocation before running, which
is also the earlier, safer point. (b) **Frames:** a per-machine credit of up to 4 KiB drawn from
the ledger in one charge and handed out to `Held` locally; returned when the frame stack empties
or at each checkpoint. Live heap is over-approximated by at most 4 KiB, the safe direction;
the reported peak includes the credit, so it stays deterministic.

**Gain.** Up to −10% on loops (*prototype* bound); fewer atomics under concurrent branches.
**Risk.** Medium: the credit must be returned on every unwind path (a `Drop` on the credit
handles it). **Security.** Heap-charged buffers kept; a `Budget { which: heap }` can come up to
4 KiB earlier, deterministically. **Stage.** (a) P1; (b) P2. **Files.** (a)
`builtins/util.rs`, `builtins/list.rs`, `builtins/vec.rs`, `builtins/math.rs`; (b) `machine.rs`,
`heap.rs`.

### LV-5: tuple-free `for (k, v) in m`

**Problem.** Each map entry builds a heap-charged tuple only to destructure it (7 steps and
839 ns per entry).

**Proposal.** When a `for` over a map has an irrefutable tuple pattern of two bindings or `_`,
bind key and value directly, charging the steps `matches` would charge for the tuple pattern
and its parts, and adding the map's own labels to both as today. Other patterns keep the tuple.

**Gain.** *Estimate* −20% to −30% per entry. **Risk.** Low. **Security.** Labels: both parts
get the map's own labels (unchanged). **Stage.** P2. **Files.** `machine.rs`.

### LV-6: leaf-slice walks in built-ins

**Problem.** Built-ins read lists through `for_each` with a `&mut dyn FnMut` per element, or
through the owned `Cursor` (an `Arc` clone per node).

**Proposal.** `List::leaves()`: an iterator of `&[Value]` slices (an explicit stack of node
references, no recursion), used by every pure built-in that reads a whole list (`sum`, `vec`,
`math`, `contains`, `index_of`, `json.encode`). Loops keep the cursor, which must outlive a step.

**Gain.** *Estimate* −20% to −40% on pure aggregates (already 5 to 15 ns per element).
**Risk.** Low. **Security.** None. **Stage.** P1. **Files.** `plist.rs`, `builtins/list.rs`,
`builtins/vec.rs`, `builtins/math.rs`.

### LV-7: cancellation polling in `vec` and `math` (finding)

**Problem.** Stage 9's `vec` and `math` aggregates do not poll cancellation. At the measured 5 to
15 ns per element, a `vec.top_k` over the largest input the heap admits (about 22 million
`Float` slots at 512 MiB) runs a few hundred milliseconds without a checkpoint; `text.similarity`
and `re` poll every checkpoint's worth of work.

**Proposal.** Poll `cx.cancel` every 64,000 units of work (one checkpoint's worth) in every pure
built-in whose work can exceed it, as `text.similarity` does; add the `vec.top_k` and
`math.median` cases to the cancellation tests.

**Risk.** Low. **Security.** Restores "cancellation checkpoints in long built-ins". **Stage.**
P1, or folded into stage 9's next review round if root prefers. **Files.** `builtins/vec.rs`,
`builtins/math.rs`, `tests/data3_limits.rs`.

### LV-8: last-use moves and in-place appends for `xs = list.push(xs, x)`

**Problem.** The loop that builds a list by pushing costs 3.7 µs per element (338 ns per step):
`Local` clones the list out of its slot, so `push` never owns it uniquely and must copy the last
leaf (up to 32 elements) and the path.

**Proposal.** In the prepared form, mark a `Local` read as a **move** when it is the last use of
the slot before the slot is reassigned in the same statement (`xs = list.push(xs, …)`,
`m = map.set(m, …)`, `s = s + t` for lists). The machine then takes the value out of the slot.
`List::push` and `Map::set` use `Arc::get_mut` along the path and update in place when every node
is unshared, copying as today otherwise. Charges stay the class charge (they never depend on
representation state, `spec/06-stdlib/data.md:62`); only wall time changes. Values shared with
other branches or captured by closures are never unique, so persistence is unaffected.

**Gain.** *Estimate* 3 to 5 times faster push and set loops. **Cost.** Medium: a liveness pass
over each function's statements in `prepare.rs`. **Risk.** Medium; the in-place path must keep
each node's `Meta` (summary, weight, depth) exact, checked by the existing summary property.
**Security.** Label summaries are recomputed on the touched path as today; no `unsafe`. **Stage.**
P2b, after LV-1 to LV-3. **Files.** `prepare.rs`, `machine.rs`, `plist.rs`, `pmap.rs`.

### LV-9 (deferred): packed `f64` leaves

A leaf variant holding `Box<[f64]>` for unlabelled homogeneous `Float` lists would turn
`vec`/`math` gathers into copies (*estimate* 3 to 10 times faster `vec.*`). But `List::get`
returns `&Value`, so every reader of a list changes; labels on single elements force a
conversion back; and `vec.dot` is already 85 times faster than its loop and offloaded above 16,384
elements. Revisit if stage 24's measurements show `vec.top_k` over large embedding sets on the
critical path. Files when taken: `plist.rs`, `value.rs`, every list reader.

### LV-10 (deferred): fusing `filter`/`map` chains

Running `xs |> list.filter(p) |> list.map(f)` as one pass would save the intermediate list.
But `map`'s pre-charge `1 + ⌈k/64⌉` depends on the filter's output size `k`, known only after
the filter finishes, so charging before work forces an upper-bound pre-charge and a refund,
which moves the steps exhaustion point; and the fused form's heap peak differs. With LV-3 the
remaining saving is one buffer per element (*estimate* under 15%). Revisit after LV-3 is
measured.

### Rejected

- **Per-element step discounts for callback built-ins** (charging a `list.map` element less than
  its callback's nodes). Steps are defined as IR nodes entered (`spec/07-security.md:276-278`);
  a discount would make charges depend on which built-in called the code, and every security
  review so far has refused charges below the work done. The vectorised forms are already
  never dearer than their loop beyond `1 + ⌈n/64⌉` (E-5); the gains belong in wall time.
- **Parallel chunked float reductions on the pool.** A fixed pairwise tree would be
  deterministic but would change the bits against the loop (E-1) and against today's results;
  large aggregates are already offloaded whole.
- **A cursor rewrite.** Loops already iterate by cursor with no per-step allocation for lists and
  ranges; only maps allocate (LV-5).
- **Recomputing label summaries less often.** Summaries are computed once per built leaf, not
  per element; the per-push leaf recomputation disappears with LV-8.

## Functional syntax

The user wants Ratio "much more functional than DOD or OOP". Chapter 02 already has closures
(`|x| …`, `|| …`, `fn(…) -> T { … }`), pipes (`x |> f(a)` is `f(x, a)`, lowered in
`lower.rs:1038` with no IR node of its own), method calls on a module's first parameter type
(`xs.map(f)`), `do` blocks, expression-valued `if` and `match`, and `loop` with `break v`.

Every form below is **sugar resolved by the checker and desugared in lowering**, as pipes are:
the Core IR, its hash and `IR_VERSION` do not change, and a program written with the sugar
hashes the same as one written in the desugared form. Common to all of them:

- **Types and inference.** The checker checks the sugar node directly (diagnostics point at what
  the user wrote) and records the resolution lowering needs, as for pipes.
- **Taint and secrecy.** A desugared closure is an ordinary closure: its parameters are untrusted
  or labelled exactly as the values it is called with, and the `Secret` instantiation rule
  (`C0037`) applies to the built-in it is passed to.
- **Effects.** The effect rule (`C0016`, no effects in constants, configuration and `state`)
  sees the desugared calls; a comprehension or placeholder over an effectful function in a
  `const` is refused as the explicit form is.
- **Guarded-effect lint (`C0039`).** It treats closure parameters as derived and tracks function
  values; the sugar produces the same closures and calls, so it reports the same effects.
- **Printer.** The syntax tree keeps the sugar node; the canonical printer writes it back as
  written, and the round trip (parse, print, parse) holds as for every production.

### FS-1 (adopt): list comprehensions

```ratio
let names = [f.path for f in files if f.size > 0]
let pairs = [(a, b) for a in xs for b in ys if a < b]
```

Grammar: `list_lit = "[" , expr , comp_clause , { comp_clause } , "]" | …` with
`comp_clause = "for" , pattern , "in" , or_expr | "if" , expr`. After the first expression of a
list literal the parser sees `for` (a keyword, rule 8 already gives `in` its loop meaning there),
which no other list form allows, so there is no ambiguity. The source of a `for` clause is an
`or_expr`, so `if` ends it.

Desugaring (innermost first):

| Form | Core IR |
| --- | --- |
| `[e for p in xs]` | `list.map(xs, \|p\| e)` |
| `[e for p in xs if c]` | `list.flat_map(xs, \|p\| if c { [e] } else { [] })` |
| `[e for p in xs for q in ys …]` | `list.flat_map(xs, \|p\| [e for q in ys …])` |
| source a map `m` | `map.entries(m)` in its place (pairs in key order, as `for`) |
| source a range `a..b` | `list.range(a, b)` (at most 1,000,000 elements, as any list) |

`flat_map` keeps the loop's interleaving (the condition, then the element, element by element),
so effects and errors come in the same order as in the canonical loop, and it never collapses
`T??` as a `filter_map` would. A refutable pattern raises `Invalid` on a mismatch, as in `for`.

**Equivalence.** By construction a comprehension *is* its vectorised form, so E-1 to E-6 relate it
to the loop. One caveat: the labels of `c` are an implicit flow in the `if` and are dropped,
exactly as in the loop; the explicit `list.filter` keeps them (open question Q2 closes this).
**Cost.** Parser, checker (element type from the source), lowering, printer, about 400 lines
with tests. **Risk.** Low. **Interchangeability.** High: it is the loop's shape with the
built-in's semantics.

### FS-2 (adopt): placeholder closures and partial application

```ratio
xs.map(_ + 1)                 // |a| a + 1
files |> list.sort_by(_.path) // |a| a.path
items.filter(_.score > 0.5)   // |a| a.score > 0.5
let add2 = add(_, 2)          // |a| add(a, 2)
```

Rule: `_` in expression position (today an error: `_` is only the wildcard pattern) stands for
the parameter of an implicit one-parameter closure whose body is the **smallest argument, pipe
stage or call that properly contains the `_`**. So `xs.map(_ + 1)` is `xs.map(|a| a + 1)`, a bare
`_` argument makes the enclosing call the body (`add(_, 2)` is `|a| add(a, 2)`), and `f(g(_))` is
`f(|a| g(a))`. At most one `_` per implicit closure (a second is a parse error: write a closure); `_`
never crosses a closure, block or `match` arm. In patterns `_` keeps its wildcard meaning; the
parser knows which position it is in, so rule 5 (`|`) and rule 6 (patterns) are unaffected.

Desugaring: an ordinary closure node with a fresh slot. Inference needs an expected function type,
as for an unannotated closure; `let f = _ + 1` is the same error as `let f = |a| a + 1` without a
type. **Cost.** Parser rule plus checker scoping, about 300 lines. **Risk.** Medium: the scope rule
must be taught; the printer writes the sugar back unchanged, so diffs stay readable.
**Interchangeability.** Medium-high: it makes the vectorised form as short as the loop body.

### FS-3 (adopt): method stages in pipes

`xs |> .filter(p) |> .map(f)` is `xs.filter(p).map(f)`: grammar `pipe_expr = coalesce_expr ,
{ "|>" , ( postfix_expr | "." , MEMBER , postfix ) }`. A line starting with `|>` or `.` already
continues the expression (semicolon rule 3), so long chains format one stage per line. Desugars
to the method call. **Cost.** Small. **Risk.** Low. **Interchangeability.** Low; it is about
reading pipelines uniformly.

### Options for the user (FS-4 to FS-11)

| ID | Form | Desugars to | Trade-off |
| --- | --- | --- | --- |
| FS-4 | Composition `f >> g`, `g << f` | `\|a\| g(f(a))` | Pipes cover most uses; `>>` must be split back into two `>` when it closes nested type arguments (`f::<Map<K, Map<K, V>>>(…)`), a token-reader change |
| FS-5 | Multi-clause functions (`fn fact(0) -> Int = 1` then `fn fact(n) -> Int = n * fact(n - 1)`) | one `fn` whose body is a `match` on the parameter tuple | Exhaustiveness comes free from `match`; merging items by name meets the duplicate-definition rule and the item index; an alternative is `fn f(…) -> T match { … }` |
| FS-6 | `e where { let a = …; let b = … }` | `do { let a = …; let b = …; e }` | `do` blocks already do this; adds a contextual keyword for word order only |
| FS-7 | `list.scan` and `list.zip_with` built-ins | new `linear` callback built-ins | Stdlib rows, not syntax; `scan` is the missing fold-with-history; adds `builtins.txt` rows and the stdlib version, no IR change |
| FS-8 | Fold sugar (`fold a = 0 for x in xs { a + x }`) | `list.fold(xs, 0, \|a, x\| a + x)` | Readable accumulators without `var`; a new keyword form for something `fold` and FS-2 already express |
| FS-9 | Lazy sequences or generators | — | Needs suspended producers: new IR (a coroutine node) and `IR_VERSION` 2; effects in a producer would interleave with the consumer, and an abandoned producer needs cancellation; reject for now |
| FS-10 | Named arguments on function values | — | Function types have no parameter names; a type-system change for little gain; reject |
| FS-11 | Automatic currying | — | Conflicts with defaults and named arguments; FS-2's `f(_, 2)` covers partial application; reject |

## Conformance tests

**Equivalence corpus** `conformance/equiv/`: each file holds a `loop_*` and a `vec_*` function on
the same arguments; a new runner, `crates/ratio-eval/tests/equiv.rs`, runs both and asserts the
relation named in the file (`// equiv: value`, `labels ⊇`, `labels =`, `error <kind> at <i>`,
`steps ≤ +<k>`, `bits <hex>` per form). Arguments are written with the existing `// arg:`
directive, labelled ones with `secret` (source 0).

| # | Loop | Vectorised | Arguments | Expected |
| --- | --- | --- | --- | --- |
| 1 | `for x in xs { s += x }` | `list.sum(xs)`; `list.fold(xs, 0, \|a, x\| a + x)` | `[1, 2, 3, 4]` | value 10 both; steps: sum < loop, fold ≤ loop + 1 + ⌈4/64⌉ |
| 2 | same | same | `[1, secret 2, 3]` | value 6, labels `{0}` in both (labels =) |
| 3 | same | `list.sum` | `[]` with own label `secret` | value 0; loop unlabelled, built-in `{0}` (labels ⊇) |
| 4 | push loop | `list.map(xs, \|x\| x * 2)`; FS-1 `[x * 2 for x in xs]` | `[1, 2, 3]` | `[2, 4, 6]`; element labels equal; list own labels ⊇ |
| 5 | `if x % 2 == 0 { out = out.push(x) }` | `list.filter`; FS-1 with `if` | `[1, 2, secret 3, 4]` | `[2, 4]`; `filter` result own labels `{0}`, loop and comprehension none (implicit flow) |
| 6 | `for x in xs { if p(x) { found = true; break } }` | `list.any(xs, p)` | `p` true at index 1 | `true`; callback called twice in both (a counter in the result) |
| 7 | loop calling `f` | `list.map(xs, f)` | `f` raises `Invalid` at indexes 2 and 4 | `Invalid` at element 2 in both; element 3 never called |
| 8 | `for x in xs { s += x }` | `list.sum` | `[9223372036854775807, 1]` | `Invalid` (overflow) in both |
| 9 | float loop sum | `list.sum` | `[0.1, 0.2, 0.3]` | bits `0x3fe3333333333334` both |
| 10 | `s += a[i] * b[i]`; loop mean | `vec.dot(a, [1.0, …])`; `math.mean` | `[1e16, 1.0, …]` ×100,000 then `−1e16` ×50,000 | dot: same bits; mean: built-in `0x3fd5555555555555`, loop `0x0` (E-2) |
| 11 | loop `ss += x * x` then `sqrt` | `vec.norm` | `[1e200, 1e200]` | loop `inf`, built-in finite (E-2) |
| 12 | `for (k, v) in m { s += v }` | `list.fold(map.values(m), …)` | `{"a": 1, "b": secret 2}` | value 3, labels `{0}` both; key order `a`, `b` |
| 13 | `repeat n { … }` | `for _ in 0..n { … }` | `n = 100` | same value; `repeat` steps ≤ `for` steps |
| 14 | `par_map(xs, f)` | `list.map(xs, f)` | one failing element | same error kind and element |
| 15 | loop of length 10,000 | `list.map` | steps left 5,000 | `Budget { which: steps }` in both (kind only) |

**Fast-path tests** (P2): the whole corpus, `conformance/eval` and `conformance/data` with the
fast paths on and off give identical values, labels, steps and heap peaks; a differential property
test for LV-3 over random simple lambdas (overflow and division by zero included); for LV-8, the
summary property after random in-place and shared pushes; the existing hash snapshots stay
unchanged (no IR change).

**Syntax tests** (stage F): `conformance/syntax/` parse and round-trip files for FS-1 to FS-3
(nested clauses, `_` scoping, `_` in patterns unaffected, `|> .m()` across lines); `conformance/types/`
errors for two `_` in one closure, `_` without an expected type, and a comprehension over a
non-iterable; a hash test that the sugar and its desugared form hash identically.

## Sequencing and file ownership

| Slice | Content | Starts after | Owned files | In-flight overlap |
| --- | --- | --- | --- | --- |
| **P0** errata and corpus | E-1 to E-6 in the spec; `conformance/equiv/` and `tests/equiv.rs` | stages 8 and 9 merged (needs their built-ins) | `spec/03-semantics.md`, `spec/06-stdlib/data.md`, `spec/06-stdlib/control.md`, `spec/07-security.md`, `spec/README.md`, `conformance/equiv/**`, `conformance/spec-map.txt`, `crates/ratio-eval/tests/equiv.rs` | Draft bump: only one open PR bumps the draft; stage 10 takes 0.15, so P0 takes the next free number and must not be open at the same time as 11a's spec PR |
| **P1** built-in-local | LV-4a, LV-6, LV-7 | stages 8 and 9 merged; runs beside 11a | `plist.rs`, `builtins/util.rs`, `builtins/list.rs`, `builtins/vec.rs`, `builtins/math.rs`, `tests/data3_limits.rs` | none: 10 and 11a do not touch these files (11a owns `control` and the machine) |
| **P2** machine hot path | LV-1, LV-2, LV-3, LV-4b, LV-5 | stage 11a merged (10 and 11a rewrite `machine.rs`: frames, scheduler) | `machine.rs` (and a new `machine/fast.rs`), `prepare.rs`, `stack.rs`, `heap.rs`, `builtins.rs` (`Task` protocol), `builtins/list.rs`, `builtins/map.rs` | 11b (control, `par_fold`) also touches `machine.rs` and `builtins.rs`: land P2 before 11b starts, or after it merges, never both open |
| **P2b** | LV-8 | P2 merged | `prepare.rs`, `machine.rs`, `plist.rs`, `pmap.rs` | as P2 |
| **F** functional syntax | FS-1 to FS-3 (and any options chosen) | stages C and 10 merged (C rewrites `ratio-check`'s `body.rs`, `call.rs`, `expr.rs`, `lint.rs`; 10 edits `lower.rs`) and before stage L (the linter needs the final syntax) | `crates/ratio-syntax/**` (token reader, parser, printer), `crates/ratio-check/src/{expr,call,body}.rs`, `crates/ratio-ir/src/lower.rs`, `spec/02-syntax.md`, `spec/03-semantics.md` (one paragraph), `conformance/syntax/**`, `conformance/types/**` | FS-1 needs stage 8's `flat_map` and `map.entries` at run time; the checker side needs none |

P1 and F can run in parallel with 11a; P2 is the only slice that waits for the scheduler. The
deferred items (LV-9, LV-10) and the remaining options wait for stage 24's measurements.

## Open questions

- **Q1 (scope of syntax).** Adopt FS-1 to FS-3 now, and which of FS-4 to FS-8 besides? My
  recommendation: FS-1 to FS-3 now, FS-7 (`scan`, `zip_with`) with them, the rest not.
- **Q2 (implicit flows in values).** Should an `if` or `match` *value* carry the labels of its
  condition or scrutinee? Then a comprehension's filter labels its result as `list.filter` does,
  and `if secret { a } else { b }` is labelled, at no cost to statements (assignments under a
  labelled branch stay an implicit flow, as today). Recommended yes (a chapter 03 change and one
  line in the machine), but it relabels existing programs' values, so it is the user's call.
- **Q3 (calibration).** Agree that stage 24 calibrates the 64-units-per-step rate only after
  LV-1 to LV-3 land, since they widen the wall-time gap between interpreted and built-in steps
  from 4–8× to an *estimated* 10–20×?
