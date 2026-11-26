# Loops, vectorised operations and functional syntax

Task DESIGN-LOOPS (user request: "make looping and vectorised operations compatible and
optimised", widened to "also add in other function syntax", then to "`Result<T>` types and
monadic chain expressions", under the style "a nice fusion between Haskell and Java").
Revision 3 applies the 17 accepted findings of the critic review. Revision 2: the user adopted FS-1, FS-2, FS-3 and `scan`/`zip_with`, and added
[Style](#style), [Result and monadic chains](#result-and-monadic-chains) and re-evaluates the
syntax options (revision 2). Reviewed against `main` at
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
4. proposes and ranks **functional syntax** (FS-1 to FS-13), all desugared in lowering, so no
   IR node and no `IR_VERSION` bump;
5. proposes **Result combinators, a `?` operator and chain blocks** (R-1 to R-6), with their
   equivalence to `raise`/`try`;
6. sequences the work around the in-flight branches so no two touch the same file at once.

Read: the `ratio-eval` README and sources (`machine`, `prepare`, `value`, `plist`, `pmap`,
`heap`, `labels`, `builtins`), the in-flight branches' diffs, chapters 02, 03, 06 (`data`,
`control`) and 07, `lint.rs`, `lower.rs`, and PC-4 to PC-6 of the
[performance review](performance-concurrency-review.md).

Numbers in the baseline are **measured** (release build, see [Baseline](#baseline)). Gains
marked *prototype* come from a throwaway patch measured on the same benchmark; gains marked
*estimate* are reasoned from the code and the baseline, not measured.

The security invariants found by earlier reviews are the acceptance bar for every proposal:
charge before work; no Rust-stack recursion over data depth; labels from everything that
decides a result (misses and composite keys included); heap-charged buffers; the 16 MiB output
stop; cancellation checkpoints in long built-ins. Each proposal states its effect on them.

## Style

The user's principle for every syntax proposal: **a fusion of Haskell and Java**. From Java:
braces and blocks, `.method()` chains, explicit readable type annotations, familiar keywords,
statement-like layout. From Haskell: expressions everywhere, `<-` binding in `do` blocks,
comprehensions, composition, `where`, pattern matching, `Result`/`Maybe` error values.

Ratio v0.1.0 already reads as Rust-like Java (`fn`, braces, `let`, `match`, `|x|` closures,
`|>`), and `IR_VERSION` 1 is fixed. So every form here is **additive** and desugars to the
existing Core IR; breaking alternatives are listed apart, as options with their migration cost
([Breaking options](#breaking-options-not-adopted)).

| Form | Java side | Haskell side |
| --- | --- | --- |
| FS-1 comprehensions `[f(x) for x in xs if p(x)]` | `for … in` keyword reading, brackets | list comprehensions |
| FS-2 placeholders `xs.map(_.score)`, `add(_, 2)` | method chains stay short | sections and partial application |
| FS-3 `xs \|> .filter(p) \|> .map(f)` | `.method()` chaining, one stage per line | point-free pipelines |
| FS-4 `f.then(g)`, `g.compose(f)` | `Function.andThen` / `compose` as methods | composition `g . f` |
| FS-6 `fn area(…) -> Float { … } where { let … }` | braces, typed signature first | `where` clauses |
| R-2 `r.map(f).and_then(g).unwrap_or(d)` | `Optional.map(…).flatMap(…).orElse(…)` | `fmap`, `>>=`, `fromMaybe` |
| R-4 `let v = r?` | checked exceptions bridged in one character | `Either` unwrapping |
| R-5 `do { a <- f(x); b <- g(a); h(b) }` | a braced block of statements | do-notation |

**Naming: snake_case.** New built-ins and methods are `and_then`, `or_else`, `unwrap_or`,
`map_err`, `ok_or`, `zip_with`. Reasons: the 479 existing built-ins are snake_case; the checker
already warns on non-snake names for everything that is not a type or a variant
(`spec/02-syntax.md:28-31`), so camelCase built-ins would contradict the language's own lint;
and method syntax already gives the Java reading (`r.and_then(f)` reads as `r.andThen(f)`).

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
| LV-7 | Cancellation polling in `vec` and `math` | Security fix | Dropped: fixed on stage 9 at `abb1702` | — | — |
| LV-8 | Last-use moves and in-place appends for `xs = list.push(xs, x)` | Interpreter, data | Accept, after LV-1–3 | P2b | Medium |
| LV-9 | Packed `f64` leaves for homogeneous `[Float]` | Data | Defer | after 24 | Medium |
| LV-10 | Fusing `filter`/`map` chains in the prepared form | Interpreter | Defer | after 24 | Medium |
| — | Per-element step discounts for callback built-ins | Cost model | Reject | — | — |
| — | Parallel chunked float reductions on the pool | Pool | Reject | — | — |
| FS-1 | List comprehensions | Syntax | Adopted (user) | F (after C and 10) | Low |
| FS-2 | Placeholder closures and partial application (`_ + 1`, `f(_, 2)`, `_.name`) | Syntax | Adopted (user) | F | Medium |
| FS-3 | Method stages in pipes (`xs \|> .map(f)`) | Syntax | Adopted (user) | F | Low |
| FS-4 | Composition as methods `f.then(g)`, `g.compose(f)` (no `>>` operator) | Syntax | Adopt | F | Low |
| FS-6 | `where` after a function body | Syntax | Adopt | F | Low |
| FS-7 | `list.scan`, `list.zip_with` | Stdlib | Adopted (user) | D4 | Low |
| FS-5, 8–13 | Multi-clause functions, fold sugar, arrow lambdas, lazy sequences, … | Syntax | Options / rejected | — | — |
| R-1 | Keep `Result<T>` with the fixed `Error`; no user generic enums | Types | Accept | — | — |
| R-2 | `outcome.*` and `maybe.*` combinators (methods on `Result<T>` and `T?`) | Stdlib | Adopt | D4 | Low |
| R-3 | `outcome.sequence`, `outcome.traverse` | Stdlib | Adopt | D4 | Low |
| R-4 | Postfix `?`: unwrap `Ok` or re-raise | Syntax | Adopt | F | Medium |
| R-5 | Chain blocks `do { a <- f(x); … }` over `Result<T>` and `T?` | Syntax | Adopt | F | Medium |
| R-6 | Lint: branching combinators and `?` as guards in `C0039` | Checker | Adopt | F | Low |

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

**E-1 Values.** Equal, structurally, for the built-ins that have a canonical loop: `list.map`,
`map_indexed`, `filter`, `filter_map`, `flat_map`, `fold`, `reduce`, `find`, `find_index`,
`any`, `all`, `count`, `partition`, `take_while`, `contains`, `index_of`, `sum`, `min`, `max`,
`min_by`, `max_by`, `zip`, `enumerate`, `flatten`, `reverse`, `unique`, `unique_by`,
`group_by`; `map.keys`, `values`, `entries`, `map_values`, `filter`, `count_by`; and `vec.dot`,
`vec.add` (the sorts, slices and constructors have none); `Float` results are bit-identical, because both sum in index order with
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
- `Budget { which: call_depth }`: a callback runs one call deeper than a loop body (the
  built-in's call), so at the depth limit the built-in raises where the loop does not.
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
the program, its data and the engine version. Offloading (PC-6) and the optimisations below
change no value, label or step; they may change heap peaks within stated bounds, so a
`Budget { which: heap }` point on a tape is valid for the engine version that recorded it.
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
expectations give **identical values, labels and steps** with each fast path on and off (a
test-only switch on `Prepared`). The heap is not required to be identical (LV-4 and LV-8 change
what is held): it must be **deterministic across runs** of one engine version, and its live
total may exceed the switched-off run's by at most the bound each proposal states (4 KiB for
LV-4b, none for LV-8). A heap-exhaustion point recorded on a tape is therefore tied to the
engine version, as step totals already are to the step rules.

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

**Proposal.** One `Vec<Slot>` **per branch** (a single one until 11a's scheduler runs branches
concurrently; then one per branch it creates) with a base index per call (popped on `CallRet`);
each slot keeps its `level` for the branch restriction; `TaskStep::Call` carries up to three arguments inline (`SmallArgs`); `bind_params`
binds from that. Slot memory is already outside the heap ledger (frames' fixed part, at most
`call_depth` deep); the shared stack keeps that bound.

**Gain.** *Estimate* −15% to −25% per callback element after LV-1. **Risk.** Low (slot indexes
are already per-function). **Security.** Call depth stays checked before any slot is pushed.
**Test.** Branches interleaved by the scheduler (scripted completion orders) keep their own
slots and the `var` restriction. **Coordination.** 11a owns branch state; P2 builds on it.
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

The fast path charges **the walk's whole charge set**, through free functions both paths call
(`ops::charge_binary`, `ops::charge_show`, a shared `labels_of_choice`): the node step; the
extra charges of `Machine::binary` (comparisons of containers and text `⌈w/64⌉` over the
weights, membership, text `+` `1 + ⌈m/64⌉`); the labels `and`, `or` and `??` add (today pushed
as `K::AddLabels`); the `Held` slot charge for each gathered operand; and the call-depth check
of the call it replaces. It is resumable: when a charge sets `yield_due`, it saves its position
and value stack in the task and returns to `Machine::run`, so yield and cancellation points do
not move.

**Gain.** *Estimate* `list.map(xs, |x| x + 1)` from 628 to 60 to 120 ns per element; `fold` and
`filter` alike; the steps are unchanged. **Cost.** Medium: a mini-evaluator over prepared nodes,
reusing `ops`. **Risk.** Medium: a divergence between the two evaluators. Mitigated by sharing
the operations, by a differential property test (random simple lambdas, both paths, identical
value, labels, steps, error), and by the on/off switch. **Security.** Charge before work holds
(per node, as the walk); no recursion (a flat array and an explicit stack); labels come from
the same `ops` functions. **Stage.** P2. **Files.** `prepare.rs` (the mark and the flat array), a new
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

### LV-7 (dropped): cancellation polling in `vec` and `math`

Fixed on stage 9 at `abb1702` (the aggregates poll every 64,000 units of work).

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

**Moves under `try`.** A moved slot must never be observable empty: the move reads the slot
**after every other operand** of the statement has been evaluated (`f(x)` in
`xs = list.push(xs, f(x))` runs first), so a raise caught by a `try` leaves `xs` intact. If an
operand after the move can still raise, the unwind restores the slot.

**Heap.** In-place growth is charged exactly as the copy path: the new node's bytes are
allocated (and the limit checked) first, the old node's `Charge` released after, so the live
total and its peak are those of the copy path whatever the uniqueness. In place is used only
when every node on the path is unshared **and** its `Charge` belongs to this run's ledger (never
a host value or another session's); uniqueness depends on timing (pool jobs, sibling drops), so
it may change wall time only, never a charge (`spec/06-stdlib/data.md:62-64`).

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

### FS-1 (adopted): list comprehensions

```ratio
let names = [f.path for f in files if f.size > 0]
let pairs = [(a, b) for a in xs for b in ys if a < b]
```

Grammar: `list_lit = "[" , expr , comp_clause , { comp_clause } , "]" | …` with
`comp_clause = "for" , pattern , "in" , pipe_expr | "if" , expr`. After the first expression of a
list literal the parser sees `for` (a keyword, rule 8 already gives `in` its loop meaning there),
which no other list form allows, so there is no ambiguity. The source of a `for` clause is a
`pipe_expr` (`for f in files |> .filter(p)`), and `if` or `for` ends it. A record or variant
literal in a clause must be parenthesised, as in a loop head (brace rule 1).

Desugaring (innermost first):

| Form | Core IR |
| --- | --- |
| `[e for p in xs]` | `list.map(xs, \|p\| e)` |
| `[e for p in xs if c]` | `list.flat_map(xs, \|p\| list.filter([()], \|_\| c) \|> .map(\|_\| e))` |
| `[e for p in xs for q in ys …]` | `list.flat_map(xs, \|p\| [e for q in ys …])` |
| source a map `m` | `map.entries(m)` in its place (pairs in key order, as `for`) |
| source a range `a..b` | `list.range(a, b)`: a range of more than 1,000,000 elements raises `Invalid` before any element is produced, as `list.range` does (a `for` loop over it does not) |

`flat_map` keeps the loop's interleaving (the condition, then the element, element by element),
so effects and errors come in the same order as in the canonical loop, and it never collapses
`T??` as a `filter_map` would. The one-element `filter` gives the condition **filter
semantics**: its result's own labels carry the labels of `c`, so the comprehension's result is
labelled as `list.filter`'s is, without Q2 (chapter 03 states it: "a comprehension's result
carries the labels of every condition that decided which elements it kept"). A refutable pattern
raises `Invalid` on a mismatch, as in `for`. **Jumps.** The clauses and the element are closure
bodies, so `return`, `break` and `continue` there, and assigning a `var` declared outside, are
check errors naming the comprehension ("a comprehension's element cannot `break` the enclosing
loop; use a `for` loop").

**Equivalence.** By construction a comprehension *is* its vectorised form, so E-1 to E-6 relate it
to the loop: labels equal `list.filter` then `list.map`, ⊇ the loop's.
**Cost.** Parser, checker (element type from the source), lowering, printer, about 400 lines
with tests. **Risk.** Low. **Interchangeability.** High: it is the loop's shape with the
built-in's semantics.

### FS-2 (adopted): placeholder closures and partial application

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
`f(|a| g(a))`. A method call's **receiver is not an argument**: in `_.path.len()` the `_` is
inside the receiver, so the smallest enclosing argument or stage is taken (`xs.map(_.path.len())`
is `xs.map(|a| a.path.len())`), and a bare `_` receiver (`_.len()` as an argument) is the same.
A **pipe stage** containing `_` is itself the closure, applied to the piped value:
`xs |> list.take(_, 3)` is `xs |> (|a| list.take(a, 3))`, that is `list.take(xs, 3)`, and
`n |> math.max(0, _)` is `math.max(0, n)`. At most one `_` per implicit closure (a second is a
parse error: write a closure); `_` never crosses a closure, block or `match` arm, and its body
may not `return`, `break`, `continue` or assign an outer `var` (check errors, as for FS-1). In patterns `_` keeps its wildcard meaning; the
parser knows which position it is in, so rule 5 (`|`) and rule 6 (patterns) are unaffected.

Desugaring: an ordinary closure node with a fresh slot. Inference needs an expected function type,
as for an unannotated closure; `let f = _ + 1` is the same error as `let f = |a| a + 1` without a
type. **Cost.** Parser rule plus checker scoping, about 300 lines. **Risk.** Medium: the scope rule
must be taught; the printer writes the sugar back unchanged, so diffs stay readable.
**Interchangeability.** Medium-high: it makes the vectorised form as short as the loop body.

### FS-3 (adopted): method stages in pipes

`xs |> .filter(p) |> .map(f)` is `xs.filter(p).map(f)`: grammar `pipe_expr = coalesce_expr ,
{ "|>" , ( postfix_expr | "." , MEMBER , postfix ) }`. A line starting with `|>` or `.` already
continues the expression (semicolon rule 3), so long chains format one stage per line. Desugars
to the method call. **Cost.** Small. **Risk.** Low. **Interchangeability.** Low; it is about
reading pipelines uniformly.

### FS-4 (adopt): composition as methods on function values

`f.then(g)` is `|a| g(f(a))` and `g.compose(f)` the same, as Java's `Function.andThen` and
`compose`; with FS-2, `xs.map(parse.then(_.score))`. The checker knows `then` and `compose` as
intrinsic methods of function types (no module: a function value has none), and lowering
evaluates `f` and `g` once, in order, into fresh locals, then builds the closure. Calling it
adds the labels of `f` and `g` as any call of a labelled function value does. The `>>`
operator (FS-13) is rejected: the token reader would have to split `>>` back into two `>` when it
closes nested type arguments (`f::<Map<K, Map<K, V>>>(…)`), and the method form is the Java
reading anyway. **Cost.** Small (checker, lowering). **Risk.** Low. **Stage.** F.

### FS-6 (adopt): `where` after a function body

```ratio
fn score(r: Review) -> Float {
  weight * coverage
} where {
  let weight = if r.critical { 2.0 } else { 1.0 }
  let coverage = r.covered / math.to_float(r.total)
}
```

Grammar: `fn_decl = … , block , [ "where" , where_block ]`, `where_block = "{" , { let_stmt ,
";" } , "}"`, on `fn` and `harness` declarations only. `where` is a **contextual** keyword: a
keyword only directly after the `}` of an item's body, an identifier everywhere else, so no
existing name breaks. The token reader continues a line that starts with `where` **only when
the previous token is the `}` closing an item's body** (it already tracks the item keyword that
opened the brace), never elsewhere. Desugaring: the `let`s are placed at the start of the body block, in written order,
after the parameters are bound; they see the parameters and each earlier `where` binding, and
the body sees them all. Evaluation is **strict and in order** (Ratio is strict), so the
bindings must be effect-free: an effect there is refused as in a constant (`C0016`, with the
context named), which makes eager evaluation unobservable except for the steps it charges and
an error a binding raises. **Cost.** Small. **Risk.** Low. **Stage.** F.

### Options (re-evaluated for the Haskell–Java style)

| ID | Form | Desugars to | Recommendation and trade-off |
| --- | --- | --- | --- |
| FS-5 | Match-bodied functions `fn fact(n: Int) -> Int match { 0 => 1, n => n * fact(n - 1) }` | `fn fact(n: Int) -> Int { match n { … } }` (a tuple of the parameters when there are several) | Option. The braced, typed variant of Haskell's clauses; Java readers see a `switch`. Separate clauses (`fn fact(0) = 1`) are rejected: merging items by name meets the duplicate-definition rule and the item index |
| FS-7 | `list.scan`, `list.zip_with` | `linear` callback built-ins | Adopted by the user; stage D4 |
| FS-8 | Fold sugar `fold a = 0 for x in xs { a + x }` | `list.fold(xs, 0, \|a, x\| a + x)` | Reject: `fold` with FS-2 already reads well |
| FS-9 | Lazy sequences, generators | — | Reject: needs a coroutine IR node (`IR_VERSION` 2); effects in a producer would interleave with its consumer |
| FS-10 | Java arrow lambdas `x -> x + 1`, `(a, b) -> a + b`, beside `\|x\| …` | the same closure node | Option, additive (`->` never follows an expression today), but two spellings of one thing; recommended not now |
| FS-11 | Automatic currying | — | Reject: conflicts with defaults and named arguments; FS-2 covers partial application |
| FS-12 | Named arguments on function values | — | Reject: function types carry no parameter names |
| FS-13 | `f >> g`, `g << f` | `\|a\| g(f(a))` | Reject in favour of FS-4 (token-reader conflict with nested type closers) |

### Breaking options (not adopted)

Each would move further towards the Haskell–Java look but changes what v0.1.0 programs mean or
how they are written. The canonical printer could rewrite most of them mechanically (the AST is
the same), but every package, the conformance corpus, `std`, the chapter 09 examples and the
Imperium harness sessions would need the rewrite and a re-selection by hash.

| ID | Change | Migration cost |
| --- | --- | --- |
| B-1 | `<-` as one token everywhere (today `a<-b` is `a < -b`, `spec/02-syntax.md:178`) | Silent meaning change of `a<-b` anywhere; the adopted rule confines `<-` to `do` blocks instead |
| B-2 | Replace `\|x\| e` with `x -> e` or `\x -> e` | Every closure; the printer can rewrite them |
| B-3 | Java-style declarations `Int add(Int a, Int b) { … }` | Every signature; type-first parsing conflicts with expression statements; registry signatures change text |
| B-4 | camelCase built-ins (`andThen`, `flatMap`) | 479 renames, `builtins.txt`, every program; contradicts the snake_case lint |
| B-5 | `return e` meaning "wrap" inside `do` blocks (Haskell's `return`) | `return` inside existing `do` blocks leaves the function today |

## Result and monadic chains

### What exists

`enum Result<T> { Ok { value: T }, Err { error: Error } }` is one of two generic built-in enums
(`spec/06-stdlib/README.md:144`); programs cannot declare generic enums (`C0033`,
`spec/02-syntax.md:244-246`; `conformance/types/err-generic-enum.ratio` shows the laundering a
contravariant parameter would allow, `C0035`). Errors are raised and caught (`raise`,
`try … catch`); `result(|| e)` reifies an outcome, letting `Cancelled`, `Replay` and the frame's
own `Budget` through (`spec/06-stdlib/control.md:124`); `par_map_settled` returns
`[Result<U>]`. Options have `x?.f` and `x ?? d`. There is no `?` on `Result`, no combinator and
no chain syntax.

### R-1 (accept): keep `Result<T>` with the fixed `Error`; no user generic enums

- **`Result<T, E = Error>`.** For: typed domain errors, as Haskell's `Either e` or Java's checked
  exception types. Against: `raise`, `catch` and the R-4 bridge need an `Error`, so every `E`
  would need a conversion Ratio does not have; `catch` arms are exhaustive over the closed
  `ErrorKind` (decision R7) and an open `E` breaks that; `par_map_settled`, `par_fold`'s `step`,
  schemas and tapes all fix `Error`; every combinator gains a parameter. Domain errors are
  already expressible: `raise fail("tag", message, data)` gives `User { tag }` with a `Json`
  payload, read back with `json.decode::<MyError>(e.data)`.
- **User generic enums.** Reverses stage 4's decision; sound only with variance checking (the
  laundering in `err-generic-enum.ratio`) and schemas for generic enums. A larger change for a
  different goal.

Revisit (Q4) if typed errors are widely wanted: then `Result<T, E = Error>` with `E` limited to
user enums without function-typed fields (covariant by construction), and `?` only on `E = Error`.

### R-2 and R-3 (adopt): combinators on `Result<T>` and `T?`

Two modules whose first parameter is the receiver, so every row is also a method
(`r.map(f).and_then(g).unwrap_or(d)`): `outcome` for `Result<T>` and `maybe` for `T?`. Names: `maybe` (Haskell's
`Maybe`, for options) and `outcome`. `result` is taken by the control function `result(f)`, and
the short `res` and `opt` are common local names that would draw the shadowing warning
(`spec/03-semantics.md:394-398`) in ordinary code; neither `maybe` nor `outcome` is a std module,
a prelude name or a local in the corpus, `std` or the spec (`outcome` appears only as a parameter
name of `think.reflect`'s signature, which shadows nothing). Method lookup goes
by the receiver's static type, so `x.map(f)` on `[Int]?` is `maybe.map` and `x?.map(f)` keeps its
optional-chaining meaning (`list.map` on the present value).

**Label rule.** Every result carries the labels of everything that decided it: `Lr`, the
receiver's own labels (for a `Result`, which variant it is; for `T?`, the present value's own
labels or those of the `none`), is added to whatever the call returns on either branch, and the
labels of any callback result that decided a choice (`maybe.filter`) too. Values that pass through
keep their own labels. **Charge:** `const` (1 step) plus the callback's own steps; `sequence`
and `traverse` are `linear` (`1 + ⌈n/64⌉` before, the output part on return), stopping early as
`list.any` does. **Needs:** `pure`, or "as `f`" for the rows with a callback, like `list.map`.

| Built-in | Signature | Result and labels |
| --- | --- | --- |
| `outcome.map` | `<T, U>(r: Result<T>, f: fn(T) -> U) -> Result<U>` | `Ok { f(v) }` or `r`; own labels `+ Lr` |
| `outcome.and_then` | `<T, U>(r: Result<T>, f: fn(T) -> Result<U>) -> Result<U>` | `f(v)` or `r`; `+ Lr` |
| `outcome.map_err` | `<T>(r: Result<T>, f: fn(Error) -> Error) -> Result<T>` | `Err { f(e) }` or `r`; `+ Lr` |
| `outcome.or_else` | `<T>(r: Result<T>, f: fn(Error) -> Result<T>) -> Result<T>` | `f(e)` or `r`; `+ Lr` |
| `outcome.unwrap_or` | `<T>(r: Result<T>, d: T) -> T` | `v` or `d`; `+ Lr` |
| `outcome.unwrap_or_else` | `<T>(r: Result<T>, f: fn(Error) -> T) -> T` | `v` or `f(e)`; `+ Lr` |
| `outcome.or_raise` | `<T>(r: Result<T>) -> T` | `v + Lr`, or `raise e` with `Lr` added to the error value's own labels; the target of R-4 |
| `outcome.ok` | `<T>(r: Result<T>) -> T?` | `v` or `none`; `+ Lr` |
| `outcome.is_ok`, `outcome.is_err` | `<T>(r: Result<T>) -> Bool` | `Lr` |
| `outcome.zip` | `<A, B>(a: Result<A>, b: Result<B>) -> Result<(A, B)>` | the first `Err` in argument order, else `Ok { (x, y) }`; `+ La + Lb` |
| `outcome.sequence` | `<T>(rs: [Result<T>]) -> Result<[T]>` | the first `Err` by index, else `Ok` of the values; own labels: the list's own and `Lr` of every element read |
| `outcome.traverse` | `<T, U>(xs: [T], f: fn(T) -> Result<U>) -> Result<[U]>` | stops calling `f` at the first `Err`; labels as `sequence`, plus `xs`'s own |
| `maybe.map` | `<T, U>(x: T?, f: fn(T) -> U) -> U?` | `f(v)` or `none`; `+ Lx` (a `U` that is itself an option collapses, `T??` is `T?`) |
| `maybe.and_then` | `<T, U>(x: T?, f: fn(T) -> U?) -> U?` | `f(v)` or `none`; `+ Lx` |
| `maybe.or_else` | `<T>(x: T?, f: fn() -> T?) -> T?` | `x` or `f()`; `+ Lx` |
| `maybe.unwrap_or` | `<T>(x: T?, d: T) -> T` | not a built-in: the checker resolves the method and lowering writes `x ?? d`, so `d` is evaluated only when `x` is `none` |
| `maybe.unwrap_or_else` | `<T>(x: T?, f: fn() -> T) -> T` | `v` or `f()`; `+ Lx` |
| `maybe.ok_or` | `<T>(x: T?, err: Error) -> Result<T>` | `Ok { v }` or `Err { err }`; `+ Lx` |
| `maybe.filter` | `<T>(x: T?, p: fn(T) -> Bool) -> T?` | `x` or `none`; `+ Lx +` the labels of `p`'s result |
| `maybe.zip` | `<A, B>(a: A?, b: B?) -> (A, B)?` | `+ La + Lb` |
| `maybe.is_some`, `maybe.is_none` | `<T>(x: T?) -> Bool` | `Lx` |

`outcome.sequence(par_map_settled(xs, f))` is `par_map` without cancellation whose error is the first
by index (E-4's `par_map` exception does not apply).

### R-4 (adopt): postfix `?`

`e?`, for `e: Result<T>`, is `outcome.or_raise(e)`: the `Ok` value, or exactly `raise e.error`. It is the bridge from `Result` values back to raised errors, as `result(|| …)` is the
bridge the other way: `result(|| r?)` equals `r` in value. Grammar: `postfix = … | "?"`
(level 11, with calls and fields). Only on `Result`: on a `T?` it is a check error suggesting
`x ?? d` or `x.ok_or(err)?` (Rust's `?` on an option returns `none` from the function; Ratio
functions have no implicit option return). The token reader is unchanged.

| Written | Today | With R-4 |
| --- | --- | --- |
| `let v = r?` (line end) | `?` already ends a statement (rule 2 of insertion) | propagate; the statement ends |
| `r?` then a line starting `.f()` | — | `(r?).f()`: a line starting with `.` continues |
| `r?.f` | `?.` is one token: optional chaining | unchanged; on a `Result` receiver a check error with the fix `(r?).f` (no type-directed parsing) |
| `r ?? d`, `r??d` | `??` is one token: coalescing | unchanged; on a `Result` a check error suggesting `r.unwrap_or(d)` |
| `r? ?? d` | parse error | `(r?) ?? d` |
| `r?[i]`, `r?(x)` | parse error | `(r?)[i]`, `(r?)(x)` |
| `T?`, `x?: T` | option type, optional field | unchanged: types are parsed in type position only |
| `{ r? }` | — | a block (rule 1 skips a `?` only before `:` or `,`) |

**Contexts.** `?` is a raise, so it behaves as one everywhere: inside a closure it leaves the
closure's call (a `list.map` stops there, E-4); in a `par` or `par_map` branch it fails the
branch, which cancels its siblings fail-fast (recorded, PC-1); in a `race` branch it is that
branch's failure; in `par_fold`'s `step`, `Fold.Next { acc: acc.push(r?) }` turns an `Err`
outcome into fail-fast, cancelling the running branches; inside `result(|| …)` it is caught back
into an `Err`. **Safety** rests on `result`, not on `?`: `result` lets `Cancelled`, `Replay`
and the frame's own `Budget` through, so an `Err` never holds an error that may not be caught
there, and `?` only raises what a `raise` of that value would. **Labels:** the value gets `Lr`;
a raised error value gets `Lr` added to its own labels (so its kind, message and data read as
labelled).
**Charge:** the call node and the `const` built-in, 2 steps.

### R-5 (adopt): chain blocks `do { a <- f(x); … }`

Three candidates were compared on the same program:

| Candidate | Example | Assessment |
| --- | --- | --- |
| (a) chain block | `do { a <- f(x); b <- g(a); h(b) }` | Chosen: Java statements in braces, Haskell `<-`; generic over `Result` and `T?`; labels exact |
| (b) `?` in a `result` call | `result(\|\| { let a = f(x)?; let b = g(a)?; h(b) })` | Already possible with R-4, no new syntax; but it also turns any raise inside into `Err`, and it is `Result`-only |
| (c) pipes | `f(x) \|> .and_then(\|a\| g(a) \|> .map(\|b\| h(b)))` | Possible with R-2 and FS-3; later steps nest to keep earlier names in scope |

Grammar (the `do` expression is unchanged; one statement form is added, allowed only directly
in a `do` block):

```text
bind_stmt  = pattern , "<-" , expr ;     (* "<-" is "<" immediately followed by "-" *)
```

The parser recognises a bind when a statement directly in a `do` block reads as a pattern
followed by `<` and `-` with no space between; `do { x < -y }` stays a comparison. A `do` block
with at least one bind is a **chain block**:

- **Kind.** The first bind's source decides: `Result<_>` or an option. Every bind must be of that
  kind (convert with `.ok()` or `.ok_or(err)`), else a check error.
- **Value.** The final expression `e: U`, wrapped: `Ok { value: e }` for a `Result` chain
  (`Result<U>`), `e` as `U?` for an option chain (an option passes through, `T??` is `T?`).
- **Other statements.** `let` and expression statements run in order. **Any discarded value of
  the chain's kind** (a `Result` in a `Result` chain, an option in an option chain), whether a
  top-level statement or a value dropped inside a nested block or `if`, is a check error ("bind
  it with `_ <- e`, or ignore it with `let _ = e`"), so no `Err` is dropped silently.
- **Final expression.** If it is itself of the chain's kind it is refused ("bind it: `x <- e; x`"),
  since wrapping would nest (`Result<Result<U>>`).
- **`<-` in a nested block.** Binds are recognised only directly in the chain's block; a `x<-y`
  inside a nested block or `if` gets a hint ("`<-` binds only at the top of a `do` block").
- **Exits and `var`s.** `return`, `break` and `continue` may not leave a chain block, and it may
  not assign a `var` declared outside it (check errors): its body becomes closures. A `var`
  declared in the chain before a bind is **captured by value** by the closures after it; assigning
  it after the bind is a check error naming the bind ("`n` is captured by the bind at 4:3; use
  `let` or a new name"). `return` keeps its Java
  meaning, leaving the function, everywhere (Q5); the user's sketch `return h(b)` is written
  `h(b)`.
- **Desugaring.**

  ```text
  do { p1 <- e1; s1…; p2 <- e2; s2…; e }
    ⇒ outcome.and_then(e1, |p1| { s1…; outcome.and_then(e2, |p2| { s2…; Ok { value: e } }) })
  ```

  `maybe.and_then` and a bare `e` for an option chain. A refutable pattern that does not match
  raises `Invalid`, as in `for`. Raises inside the block propagate as raises: the chain
  short-circuits on `Err` values only (wrap it in `result(|| …)` to reify those too).
- **Labels.** Each `and_then` adds its receiver's own labels, so the chain's result carries the
  labels of every bind that decided it, independently of Q2.
- **Charge.** Per bind about 4 steps (a call, a closure, `and_then`'s step, the parameter
  pattern) and one closure allocation; LV-3 does not apply (the bodies are not simple).
- **Checker.** Types flow from each bind into its pattern; `C0016` applies to the desugared
  calls; the printer keeps the `<-` form.

**With comprehensions (FS-1).** A comprehension builds a list and does not short-circuit: over a
function returning `Result<U>` it gives `[Result<U>]`. To stop at the first `Err`, use
`outcome.traverse(xs, f)` (stops calling `f`) or `outcome.sequence([f(x) for x in xs])` (calls every
`f`); `[f(x)? for x in xs]` raises at the first `Err` by index. Comprehensions are deliberately
not made monadic: the same brackets would then mean two things.

**With `par_fold`.** `Fold<A>` is a control enum, not a monad, so chain blocks do not apply to it;
`?` in `step` gives fail-fast as above.

### R-6 (adopt): guards for the guarded-effect lint

`C0039` reports an effect whose only guards are model-derived conditions. The new forms hide a
branch inside a call, so the lint must see it: the callback of a branching combinator
(`outcome.map`, `and_then`, `map_err`, `or_else`, `unwrap_or_else`, and the `maybe.*` rows with a
callback) is treated as guarded by its receiver (a model-only guard when the receiver is
derived); the statements after a `?` on a derived `Result` are guarded by it; chain blocks follow
from their desugaring. `r.is_ok()` as an `if` condition is already handled as a derived
condition. **Stage.** F (`lint.rs`, after stage C).

### Equivalence for results (E-7 to E-9)

**E-7 Chains.** Four spellings of one computation:

- A, the chain `do { a <- f(x); b <- g(a); h(b) }`;
- B, its desugared combinators `f(x).and_then(|a| g(a).and_then(|b| Ok { value: h(b) }))`;
- C, `result(|| { let a = f(x)?; let b = g(a)?; h(b) })`;
- D, explicit matching: `match f(x) { Ok { value: a } => match g(a) { Ok { value: b } => Ok { value: h(b) }, Err { error } => Err { error } }, Err { error } => Err { error } }`.

When no expression in the block raises:

- **Values** are equal. **First failing step:** the first bind, in written order, whose source is
  an `Err`; its `Error` is returned unchanged (kind, message, data, site) in all four.
- **Labels.** `A = B ⊇ C ⊇ D`, with equality when the final value depends on every bound value
  (or Q2 is adopted, which makes `D = B`).
- **Steps.** `A = B` exactly (the same IR). C and D differ from A by at most a constant per bind,
  pinned by `steps` expectations in the corpus.

When an expression raises, A, B and D propagate the raise, and C returns it as an `Err`.

**E-8 Options.** `x.unwrap_or(d)` is lowered to `x ?? d`, so it is lazy in `d` and identical in
value, labels and steps; `x.map(f)` equals `if let v = x { f(v) }
else { none }` in value, with labels ⊇ (the `if let` drops `Lx` on `none`, an implicit flow).

**E-9 Lists of results.** `outcome.traverse(xs, f)`, `result(|| list.map(xs, |x| f(x)?))` and the loop
`for x in xs { out = out.push(f(x)?) }` inside `result` agree on value and on the first failing
index, and none calls `f` after it; `outcome.sequence(list.map(xs, f))` agrees on value but calls `f`
on every element.

**Errata.** `spec/06-stdlib/README.md:144` (a note: the combinators live in `outcome` and `maybe`);
`spec/06-stdlib/data.md` (two new sections `outcome` and `maybe`, rows in the submodule table and the
`const`/`linear` classes); `spec/06-stdlib/control.md:124` (`result(|| r?)` equals `r`);
`spec/03-semantics.md:480` (after `result`: the `?` operator and chain blocks, with E-7);
`spec/03-semantics.md:228` (implicit flows: state what the machine already does, that `and`,
`or` and `??` add the labels of the value that chose the result, and FS-1's comprehension rule);
`spec/02-syntax.md` (`postfix` gains `"?"`, `bind_stmt` in `do` blocks, `where` after a body, a
note at `:178` that the token reader still reads `<` then `-`, and new disambiguation rules for
`<-` and `?`).

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
| 5 | `if x % 2 == 0 { out = out.push(x) }` | `list.filter`; FS-1 with `if` | `[1, 2, secret 3, 4]` | `[2, 4]`; `filter` and comprehension own labels `{0}`, loop none (implicit flow) |
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
| 16 | E-7 forms C and D | chain A, combinators B | `f` gives a secret `Ok 1`, `g` an `Err` `User { tag: "b" }` | `Err` tag `b`, same site in all four; labels `A = B ⊇ C ⊇ D`; steps `A = B` |
| 17 | `result(\|\| list.map(rs, \|r\| r?))` | `outcome.sequence(rs)` | `[Ok 1, Err a, Err b]` | `Err a` in both |
| 18 | `x ?? d` | `x.unwrap_or(d)` | `x` present, `d` a call that raises | no raise in both (`d` not evaluated); identical value, labels and steps; the same IR |
| 19 | loop with `f(x)?` inside `result` | `outcome.traverse(xs, f)` | `f` fails at index 1 of 3 | `Err` of index 1; `f` called twice in both |
| 20 | `par_fold` with `step` using `r?` | `par_map` | one failing element | the same error kind |
| 21 | `try { xs = list.push(xs, f(x)) } catch e { }` (LV-8) | the same with fast paths off | `f` raises | `xs` unchanged in both; identical values, labels, steps |
| 22 | loop body at the depth limit | `list.map` | `call_depth` set to the loop's depth + 1 | loop completes; built-in raises `Budget { which: call_depth }` (E-4) |

**Fast-path tests** (P2): the whole corpus, `conformance/eval` and `conformance/data` with the
fast paths on and off give identical values, labels and steps, and heap peaks deterministic and within the stated bounds; a differential property
test for LV-3 over random simple lambdas (overflow and division by zero included); for LV-8, the
summary property after random in-place and shared pushes; the existing hash snapshots stay
unchanged (no IR change).

**Syntax tests** (stage F): `conformance/syntax/` parse and round-trip files for FS-1 to FS-3
(nested clauses, `_` scoping, `_` in patterns unaffected, `|> .m()` across lines); `conformance/types/`
errors for two `_` in one closure, `_` without an expected type, and a comprehension over a
non-iterable; a hash test that the sugar and its desugared form hash identically. For R-4 to
R-6 and FS-4, FS-6: every row of the R-4 parse table (`r?.f` and `r ?? d` on a `Result` as check
errors with their fixes, `r? ?? d`, `r?` before a line starting with `.`); `do { x < -y }` a
comparison and `do { x <- y }` a bind; a chain-kind mismatch, a dropped `Result` statement and a
`return` inside a chain as check errors; `where` as an ordinary identifier elsewhere and an
effect in a `where` binding refused; `C0039` warnings for an effect inside `and_then` on a
model-derived `Result` and after a `?` on one.

## Sequencing and file ownership

| Slice | Content | Starts after | Owned files | In-flight overlap |
| --- | --- | --- | --- | --- |
| **P0** errata and corpus | E-1 to E-6 in the spec; `conformance/equiv/` and `tests/equiv.rs` | stages 8, 9 and 10 merged (needs their built-ins; 10 edits chapters 03 and 07) | `spec/03-semantics.md`, `spec/06-stdlib/data.md`, `spec/06-stdlib/control.md`, `spec/07-security.md`, `spec/README.md`, `conformance/equiv/**`, `conformance/spec-map.txt`, `crates/ratio-eval/tests/equiv.rs` | Draft bump: only one open PR bumps the draft; P0 takes the next free draft at merge time and is never open beside another draft-bumping PR |
| **P1** built-in-local | LV-4a, LV-6 | stages 8 and 9 merged; runs beside 11a; before D4 (both own `builtins/list.rs`) | `plist.rs`, `builtins/util.rs`, `builtins/list.rs`, `builtins/vec.rs`, `builtins/math.rs`, `tests/data3_limits.rs` | none: 10 and 11a do not touch these files (11a owns `control` and the machine) |
| **P2** machine hot path | LV-1, LV-2, LV-3, LV-4b, LV-5 | stage 11a merged (10 and 11a rewrite `machine.rs`: frames, scheduler) | `machine.rs` (and a new `machine/fast.rs`), `prepare.rs`, `stack.rs`, `heap.rs`, `builtins.rs` (`Task` protocol), `builtins/list.rs`, `builtins/map.rs` | 11b (control, `par_fold`) also touches `machine.rs` and `builtins.rs`: land P2 before 11b starts, or after it merges, never both open |
| **P2b** | LV-8 | P2 merged | `prepare.rs`, `machine.rs`, `plist.rs`, `pmap.rs` | as P2 |
| **Q2** (optional, user decision) | the value of `if`, `if let`, `match` and `while let` carries the labels of the condition or scrutinee that chose the branch (own labels, and for `match` the labels the tested patterns read); statements unchanged | 11a merged | `machine.rs`, `spec/03-semantics.md`, `crates/ratio-eval/tests/labels.rs` | as P2; never open beside P2 |
| **D4** built-ins | R-2, R-3 (`outcome.*`, `maybe.*`), FS-7 (`list.scan`, `zip_with`) | stages 8, 9, 10 and C merged (C owns the checker's signatures; 10 edits the built-in tables); after P0 (both edit `data.md`) and P1 (both own `builtins/list.rs`) | `crates/ratio-eval/src/builtins/{outcome,maybe}.rs` (new), `builtins/list.rs`, one new block in `builtins.rs`'s table, `crates/ratio-check/src/builtins.rs` (signatures), `conformance/builtins.txt`, `conformance/data/{outcome,maybe}-*.ratio`, `spec/06-stdlib/data.md`, `spec/06-stdlib/README.md` | 11a adds control rows to `builtins.rs`'s table: a separate block, rebased by whichever lands second |
| **F** functional syntax | FS-1 to FS-4, FS-6, R-4 to R-6 | D4 merged (the desugarings call `outcome.and_then` and `outcome.or_raise`); stages C and 10 merged (C rewrites `ratio-check`'s `body.rs`, `call.rs`, `expr.rs`, `lint.rs`; 10 edits `lower.rs`) and before stage L (the linter needs the final syntax) | `crates/ratio-syntax/**` (token reader, parser, printer), `crates/ratio-check/src/{expr,call,body,lint}.rs`, `crates/ratio-ir/src/lower.rs`, `spec/02-syntax.md`, `spec/03-semantics.md`, `spec/06-stdlib/control.md`, `conformance/syntax/**`, `conformance/types/**`, `conformance/taint/**` | FS-1 needs stage 8's `flat_map` and `map.entries` at run time; the checker side needs none |

Every spec-bearing slice takes the next free draft at merge time. P1, then D4, then F can run beside 11a; P2 is the only slice that waits for the scheduler. The
deferred items (LV-9, LV-10) and the remaining options wait for stage 24's measurements.

## Open questions

Decided by the user: FS-1, FS-2, FS-3 and FS-7 (the former Q1). Recorded as following the
recommendations: Q4 (the fixed `Error`), Q5 (`return` keeps its function meaning), Q6 (bare
`<-` with the nested-block hint); Q7 defers FS-5 and FS-10, pending the user. Remaining:

- **Q2 (implicit flows in values).** FS-1 no longer depends on it (its filter keeps the labels in
  lowering). Adopt the optional Q2 slice after 11a, so that `if secret { a } else { b }` and
  `match` values are labelled (statements unchanged)? Recommended yes; it relabels existing
  programs' values, so it is the user's call.
- **Q3 (calibration).** Agree that stage 24 calibrates the 64-units-per-step rate only after
  LV-1 to LV-3 land, since they widen the wall-time gap between interpreted and built-in steps
  from 4–8× to an *estimated* 10–20×?
- **Q4 (typed errors).** Keep `Result<T>` with the fixed `Error` (recommended, R-1), or add
  `Result<T, E = Error>` with `E` limited to user enums without function-typed fields?
- **Q5 (`return` in chain blocks).** The chain's value is its final expression and `return`
  keeps its Java meaning (recommended), or `return e` as the last statement of a chain block
  means the chain's value, as in Haskell (B-5 shows the cost)?
- **Q6 (`<-` spelling).** Bare `a <- f(x)` in `do` blocks, recognised by the adjacent `<-`
  (recommended; the only previously valid text it changes, a statement `x<-y` directly in a `do`
  block, now fails to check instead of changing meaning silently, and none exists in the corpus,
  `std` or the spec), or the fully compatible `let a <- f(x)`?
- **Q7 (FS-5, FS-10).** Adopt match-bodied functions and Java arrow lambdas as additive
  spellings? Recommended: FS-5 yes if the user wants clause-style recursion, FS-10 not now.
