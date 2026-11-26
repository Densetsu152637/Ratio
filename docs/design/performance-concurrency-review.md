# Performance, parallelism and concurrency review

Tasks DESIGN-PC-1 and DESIGN-PC-2 (roadmap stage 2R, goal G4). Reviewed against specification
draft 0.7 (commit `a14ae79`); citations updated to draft 0.8 (commit `09596a5`).
Status: decided by root (see [Decisions](#decisions)); nothing in `spec/` is changed by this
document. Revision 2 incorporates an independent security critique (findings B1 to B4 and S1 to
S8, all accepted); revision 3 incorporates its second round (N1, N2 and seven text fixes, all
accepted).

## Scope and method

Ratio's workload is dominated by host effects: model generation (seconds), tool calls
(milliseconds to minutes), subagents and memory. Pure evaluation is a small share of a turn, which
is R2's premise for a tree-walking interpreter (`spec/01-purpose-and-model.md:91`). This review
therefore weighs proposals by the latency, spend and correctness they buy at the effect level, and
treats interpreter speed as a matter of complexity guarantees and runtime hygiene rather than raw
throughput.

Every proposal was checked against the invariants the brief names, and is rejected or flagged if it
weakens one:

- determinism and replay (R3, `spec/01-purpose-and-model.md:102-155`);
- structured concurrency (R8, `spec/03-semantics.md:457-501`);
- secrecy labels (`spec/03-semantics.md:210-352`);
- frame attenuation and budgets (`spec/04-harnesses.md:149-183`);
- the safety rules: per-install extension approval, approval-gated harness writing, plugins never
  using harnesses automatically (`spec/10-implementation.md:281-286`, `spec/11-open-questions.md:17`).

Read in full or in the relevant ranges: chapters 01, 03, 05, 10 and 11; `06-stdlib/control.md`,
`agents.md`, `model.md`, `think.md`, the `list`, `map`, `rand` and `time` parts of `data.md`, and
the permission legend of `06-stdlib/README.md`; chapter 07's limits and threat table; chapter 04's
configuration, attenuation and budgets; chapter 02's grammar; the fan-out examples of chapter 09.

Numbers marked *estimate* are derived from the specification's own formulas or from typical
hardware figures, not measured. No prototype was built: the consequential findings (PC-1, PC-2,
PC-5) follow from the specification text and arithmetic on it.

Three of the accepted proposals fix gaps rather than add features: PC-1 (effect keys and budget
decisions that depend on timing), PC-2 (a deadlock in nested fan-out) and the scope-exit ambiguity
inside PC-3. They are ranked first for that reason.

## Summary

| ID | Title | Area | Recommendation | Stage | Spec chapters touched | Syntax change | Risk |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PC-1 | Timing-independent effect keys, sequenced and verified budget decisions, per-branch random streams | Replay, budgets | Accepted | 10, 11a | 01, 03, 04, 06 (`control`, `data`), 07 | No | Low |
| PC-2 | Deadlock-free width accounting and program-order admission | Scheduling, fairness | Accepted | 10, 11a | 03, 04, 06 (`control`), 07 | No | Low |
| PC-3 | Cancellation classes, a drain fence and deadlines through the `Host` | Cancellation, host API | Accepted | 11a (Imperium 14, 17) | 01, 03, 06 (`control`, `tools`), 10 | No | Medium |
| PC-4 | `par_fold`: incremental, early-stopping fan-out | Concurrency primitives | Accepted | 11b (follow-on in 12) | 01, 03, 06 (`control`, `think`, README types) | No | Medium |
| PC-5 | Step charges by cost class, and the representations they require | Interpreter, data | Accepted | 6, 7, 8, 9 | 03, 06 (`data`), 07, 10 | No | Low |
| PC-6 | Runtime cooperation: yield points, offloaded large built-ins, `Send` values, a prepared form | Interpreter, CPU | Accepted | 6, 7, 8, 9 | 01, 07, 10 | No | Low |
| PC-7 | Smaller, cheaper tapes | Replay cost | Accepted | 11a (Imperium 13, 19) | 01, 03, 08, 10 | No | Low |
| PC-8 | Early-stopping `model.stream` with deltas on the tape | Streaming | Accepted | 12 | 01, 06 (`model`, README types) | No | Low |
| PC-9 | Single-flight `cached`, `memo` and `once`; labels kept by session caches | Effect caching | Accepted | 11b | 03, 04, 06 (`control`), 08 | No | Medium |
| PC-10 | Provider prefix-cache hints | Effect caching | Deferred | 12 (Imperium 13) | 05, 06 (`model`) | No | Low |
| PC-11 | Eager tool execution while a reply streams | Effect pipelining | Deferred | after 23 | 06 (`model`), 10 | No | Medium |
| PC-12 | Lint for independent sequential effects | Effect parallelism | Deferred | after 5 | 03 | No | Low |
| PC-13 | Closure compilation of the prepared form | Interpreter | Deferred | after 24 | 01 | No | Low |
| PC-14 | Ropes for `Str` | Data | Deferred | after 24 | 03, 06 (`data`) | No | Low |
| PC-15 | Named semaphores and branch priorities | Concurrency primitives | Deferred | after 23 | 03, 06 (`control`) | Maybe | Low |

None of the accepted proposals changes the grammar, so the parser work of stage 2 is unaffected.

## Accepted proposals

### The turn sequence (shared by PC-1, PC-3, PC-4 and PC-9)

In record mode, `TapeHost` gives every **scheduling event** of a turn one number from a single
turn-global sequence, in the order the events happen. The events are:

- **ledger events** (PC-1): each admission with its reservation and the inputs to its arithmetic,
  each refusal, each completion with its actual charge, each `budget_left()` or `spent()` reading,
  and each branch's step total when the branch ends or is cancelled;
- **cancellation events** (PC-1, PC-3): each cancelled branch's cancellation point, each
  `aborted`, `abandoned`, `drained` and `withdrawn` call, and each re-parenting of an outstanding
  drain;
- **cache events** (PC-9): each miss that starts a producer, each wait, each hit (single-flight or
  not), each producer's success, failure or cancellation, and each promotion;
- **ordering events**: the existing scheduling choices (`race` winner, timeout firing,
  `spec/03-semantics.md:484-490`) and `par_fold`'s completion-order deliveries (PC-4).

Replay checks the tape **against the tape only**. It replays the events in sequence order into a
model built from recorded data alone (the ledgers, the table of cache producers, the wait-for
graph) and verifies each event against that model, never against the replaying run's live state.
The replaying run may resolve effects in any order; where the program needs an outcome (a refusal,
a cancellation point, a cache value), it takes the recorded one. A failed check is a `Replay`
divergence.

### PC-1: timing-independent effect keys, sequenced and verified budget decisions, per-branch random streams

**Problem.** Three timing dependencies remain in R3 as written.

1. *Effect keys.* A key is `(frame path, site id, occurrence)`, where the occurrence counts how
   many times the site has run **in this frame**, and a branch index enters the key only in the
   frame-path element of a **harness call** made inside `par`, `race` or `par_map`
   (`spec/01-purpose-and-model.md:125-134`). An effect made directly in a branch shares the frame
   with its siblings, so its occurrence number is assigned in the order the branches reach the
   site. In `par_map(files, |f| retry(|| model.generate(review(f))))`, the second attempts of
   different branches are numbered in completion order. Replay answers effects at once, in another
   order, so keys move between branches, request digests mismatch, and a faithful tape raises
   `Replay` (or, lenient, makes live calls). A single branch index (`b2`) also cannot name a branch
   of a `par` nested in a `par_map` branch. Stage 11's property "replay determinism under random
   completion orders" (`spec/10-implementation.md:316`) cannot pass against this text.
2. *Budget decisions.* Concurrent branches share their frame's budgets
   (`spec/03-semantics.md:492-493`), and an effect "is not started when it could not be paid for"
   (`spec/04-harnesses.md:180-182`). What the siblings have debited when an effect is admitted
   depends on completion order, so which branch gets `Budget { which }`, and at which effect, is a
   timing outcome; it is not among the recorded scheduling choices
   (`spec/03-semantics.md:484-490`). The same holds for the `model_calls` and `tool_calls`
   counters, for the `steps` ledger that concurrent branches share (the claim that step exhaustion
   replays identically, `spec/07-security.md:267-268`, holds only for sequential code), and for
   `budget_left()` and `spent()` (`spec/06-stdlib/control.md:53-54`). Separately, "could not be
   paid for" is undefined for a model call whose output size is unknown: N branches that each pass
   a "remaining above zero" check together overshoot a token cap by up to N replies. Only the
   cancellation points of `race` losers and timed-out blocks are recorded
   (`spec/03-semantics.md:485-490`); siblings cancelled after a `par` failure stop at timing-chosen
   points too.
3. *Randomness.* `rand` draws from "the frame's seeded generator", seeded from the turn's seed and
   the frame path (`spec/06-stdlib/data.md:233-234`). Branches of one `par` share that generator,
   so which branch draws which value depends on timing; every draw must then be taped, and two
   live runs with the same model answers still differ.

**Proposal.**

- **Branch paths in keys.** A key becomes `(frame path, branch path, site id, occurrence)`. The
  branch path lists, from the frame's root to the effect, one element per enclosing concurrent
  construct: its site id, its occurrence within the enclosing frame and branch path, and the
  branch index. Occurrences count per `(frame, branch path, site)`, and a harness call's
  frame-path element counts its occurrence the same way. Branch indexes are the written position
  for `par { … }` and `race { … }`, and the list position for `par([…])`, `par_map`,
  `par_map_settled` and `par_fold` (PC-4); a built-in that runs callbacks concurrently numbers its
  branches by the position of the item it runs (`model.generate_n` and `think.self_consistency` by
  sample, `think.debate` by side within a round, `think.beam_search` by state within a level).
  Rendered: `turn/critic@s41#0/c12#0.b2/c30#1.b0`. Keys then depend only on the program and its
  data, which is what R3 says (`spec/01-purpose-and-model.md:136`).
- **Admission by bounded reservation.** Before an effect starts, the interpreter reserves its
  admission charge on the frame's ledger and every ancestor's. A tool call reserves one
  `tool_calls`. A model call reserves one `model_calls`, its counted input tokens
  (`Host::count_tokens`, `spec/10-implementation.md:65`) and its **effective `max_tokens`**: the
  smallest of the requested `max_tokens` (when set), the model's maximum output (from its card, or
  the window minus the input when the card gives none) and the tokens every ledger can still
  reserve after the input. The request is sent with that effective `max_tokens`, so a reply can
  never overshoot any ancestor's cap. Cost is reserved at the card's prices for input plus the
  effective output; local models price at zero. A model whose card has no prices is not admitted
  while any ledger has a cost cap (`Budget { which: cost }`); this rule is **to be confirmed**
  against Imperium's `BudgetGate` (an open item for stage 11a). Completion replaces the
  reservation with the actual charge.
- **A floor on the reply.** The effective `max_tokens` may not fall below a floor, the smallest
  of `ratio.limits.min_reply_tokens` (default 256, configurable like the other limits,
  `spec/07-security.md:233-236`), the requested `max_tokens` (when set) and the model's maximum
  output, so a call that asks for fewer tokens than the setting is never held to it. When the
  effective `max_tokens` would fall below the floor, the admission waits while other reservations
  in the frame tree are still held (they release as their effects complete); if it is still below
  the floor once none is held, or if the input alone cannot be covered, the effect is not started
  and raises `Budget { which }` in its branch. The waiting branch awaits host effects only, so
  PC-2's argument against deadlock holds. The wait can **starve** one branch (siblings may keep
  taking the released budget first) but cannot deadlock it: once nothing else holds a
  reservation, it is admitted or refused. The admission, after its wait, is an event on the turn
  sequence like any other, so replay admits it in recorded order.
- **Sequenced ledger events.** Every admission, refusal, completion, reading and branch step
  total is an event on [the turn sequence](#the-turn-sequence-shared-by-pc-1-pc-3-pc-4-and-pc-9).
  An admission records the inputs of its arithmetic: the counted input tokens and the model card's
  maximum output, window and prices it used. The interpreter raises `Budget` in replay exactly at
  the recorded refusals and steps exhaustion, and returns the recorded readings.
- **Verified replay.** Replay rebuilds every ledger by applying the recorded ledger events in
  sequence order, and verifies at each event, from recorded data only: that a recorded admission
  was coverable and its effective `max_tokens` follows from its recorded input count, card limits
  and prices (never from a live token count or the current model table); that a recorded refusal
  was not coverable; and, for `budget_left()` and `spent()` readings, that the `tokens`, `cost`,
  `model_calls` and `tool_calls` fields equal the rebuilt ledgers. The `wall` and `steps` fields of
  a reading depend on time and interleaving and are taken from the tape unverified. For steps, it
  verifies that each branch's recomputed step total equals its recorded one, that the per-branch
  totals sum to no more than the configured limit, and that a recorded exhaustion falls where the
  sequence-ordered sum reaches the limit. A crafted tape therefore cannot lift the limits on what a
  replay really consumes, the interpreter steps it runs and the live calls lenient mode falls
  through to, nor make the recorded budget decisions inconsistent with each other (T13,
  `spec/07-security.md:340`).
- **Cancellation points for every cancelled branch.** Each branch cancelled by any cause (`race`
  losers, `par` siblings of a failure, branches after a `par_fold` stop, timed-out blocks,
  siblings of a steps exhaustion) is recorded at its cancellation point as
  `(effect index, step count)` within the branch, on the turn sequence; replay cancels it there.
- **Per-branch random streams.** A branch's generator is ChaCha20 seeded with
  `H(turn seed, frame path, branch path)`. Draws become a function of position alone, so they need
  no tape entries (PC-7) and live runs with equal answers draw equal values. Sequential code is
  unaffected.

Recording is unconditional, not only inside concurrent regions: refusals and readings are rare,
and one rule is simpler to implement and to verify than two.

**Determinism and replay.** Closes the three holes; adds no new source of variation. Replay's own
completion order no longer matters for budgets, because ledgers are rebuilt in recorded order.

**Security.** Budgets only tighten: the effective `max_tokens` makes concurrent overshoot of any
cap impossible, including a child frame's. The verified rebuild keeps every limit in force against
crafted tapes. Keys carry no values, as before (`spec/10-implementation.md:279`). Attenuation and
secrecy are untouched.

**Cost and complexity.** Small to medium: a branch path per branch (a short list shared by its
children), a sequence counter in `TapeHost`, and a replay-side ledger rebuild. The key format
falls under `IR_VERSION` (`spec/10-implementation.md:186-188`), which has not been released, so
landing this before stages 6 and 11a costs no version bump. Lowering `max_tokens` changes the
request, which is recorded and digested like any other request field.

**Stage.** 10 (reservations, effective `max_tokens`, the steps ledger) and 11a (keys, sequenced
tape events, verified replay, random streams).

**Conformance tests.**

- `replay/` property: record under random completion orders with budgets just below total demand,
  then replay under a different random order; results and event streams are identical, and the
  rebuilt ledgers verify at every sequence number. Mutating any sequence number, refusal, reading,
  recorded input count, card limit or price, or step total in the tape yields `Replay`.
- `replay/` property over replay's own resolution order: one fixed tape is replayed many times
  while `TapeHost` resolves effects, cache waits and branch starts in randomly permuted orders;
  every replay gives the same result and passes every check. A check that read live state would
  fail this property.
- Ledger unit test: a request with `max_tokens: 1` has a floor of 1 and is admitted with one
  output token reserved, whatever `min_reply_tokens` says.
- `replay/` property: a tape with an admission that waited for the floor replays it in recorded
  order while `TapeHost` resolves the releasing effects in permuted orders.
- Ledger unit test: an admission whose effective `max_tokens` would fall below the floor waits for
  a held reservation to release, and is refused when none is held.
- `replay/keys-retry-in-par-map.ratio` (`// expect: ok`, fixture failing the first attempt of
  branch 1): replay under a different completion order consumes the same keys.
- `replay/budget-par-exhaustion.ratio` (`// expect: runtime-error Budget`): the same branch fails
  in record and in replay.
- `replay/crafted-tape-steps.ratio`: a tape with a steps exhaustion removed, or with step totals
  summing above the limit, raises `Replay`.
- `replay/par-sibling-cancel-points.ratio`: siblings of a failing `par` branch are cancelled at
  the recorded `(effect index, step count)` in replay.
- `eval/rand-par-branches.ratio` (`// expect: value …`): identical values under permuted
  completion orders, and no per-draw tape entries.
- Ledger unit tests: N concurrent model calls never exceed a token cap, in the frame or in a child
  frame with a lower cap; the effective `max_tokens` sent equals the formula.

### PC-2: deadlock-free width accounting and program-order admission

**Problem.** "At most `limits.width` branches run at once per frame tree (default 16); excess
branches wait" (`spec/03-semantics.md:492-493`, `spec/07-security.md:250`). If a branch suspended
in its own `par` or `par_map` counts as running, nested fan-out deadlocks:
`par_map(groups, |g| par_map(g.items, check, limit: 4), limit: 16)` with 16 or more groups fills
every slot with outer branches, each waiting for inner branches that wait for a slot. Width
attenuates (`spec/04-harnesses.md:166`), so a callee with nested fan-out deadlocks sooner under a
narrow caller. The order in which waiting branches start is also unspecified; arrival order is
timing-dependent and can start late items before early ones, which delays index-ordered
consumption (PC-4) and keeps more half-finished work alive.

**Proposal.**

- A branch holds a width slot only while **running**: evaluating, or awaiting a host effect (a
  model or tool call, `agents.await`, `time.sleep`, `rate_limit`). A branch that waits for
  anything other than a host effect is **parked** and releases its slot: waiting for its own child
  branches, and waiting for a single-flight cache producer (PC-9). A parked branch takes a slot
  back, ahead of other waiters, when its wait ends. Every slot holder then awaits only pure
  evaluation or host effects, which complete without any other branch's help, so no wait-for
  cycle can pass through the width.
- Waiting branches are admitted in lexicographic order of branch path (PC-1): program order,
  depth first. Paths are unique, so there are no ties.
- `limit:` on `par([…])`, `par_map`, `par_map_settled` and `par_fold` stays a per-construct cap
  inside the width. The parallel built-ins (`model.generate_n`, `think.self_consistency`,
  `think.best_of`, `think.debate`, `think.beam_search`) count against the width the same way.
- **Budget fairness needs no new primitive.** The scoped budget block already gives each branch
  a share: `par_map(xs, |x| with { budget: { tokens: 20k } } { f(x) })`
  (`spec/06-stdlib/control.md:56`, `spec/02-syntax.md:327-328`). The specification should show
  this idiom next to `par_map`.

**Determinism and replay.** Admission changes when branches start, never keys or results (given
PC-1); it is not recorded.

**Security.** The bound on running branches is kept; parked branches hold only heap, which the
heap limit covers (`spec/07-security.md:241`). Attenuation is unchanged.

**Cost and complexity.** Small: a parked state and an ordered wait queue in the scheduler.

**Stage.** 11a (scheduler); 10 already attenuates width.

**Conformance tests.**

- `eval/width-nested-fanout.ratio` with `limits: { width: 2 }` and a 3 by 3 nested `par_map`
  (`// expect: ok`; the runner's wall limit turns a deadlock into a failure).
- `eval/width-single-flight.ratio` at width 16:
  `par_map(1..17, |_| cached("k", || par_map(xs, g)), limit: 17)` completes (`// expect: ok`);
  the 16 waiters are parked, so the producer's inner branches find slots.
- Scheduler property: the number of running branches never exceeds the width.
- A scripted host that records start order: branches start in branch-path order.

### PC-3: cancellation classes, a drain fence and deadlines through the `Host`

**Problem.** "A tool call in flight is allowed to finish (as the engine does on stop) and its
result discarded" (`spec/03-semantics.md:495-498`). The text does not say whether the cancelled
scope waits for that call. If it waits, `with timeout 10s { tools.run_command("cargo test",
timeout: 600s) }` can take ten minutes to give up, and `race` returns its winner only after every
loser's tool has finished, which defeats both forms. If it does not wait, the tool outlives its
scope, contradicting R8's nesting lifetimes (`spec/03-semantics.md:459-461`); a later write may
run concurrently with a stale one; and the turn may end with spend still running. The `Host` trait
(`spec/10-implementation.md:59-90`) has no cancellation channel beyond dropping a future, and
carries no deadline, so a model request started a second before a timeout fires runs to the host's
own HTTP timeout. A tool call waiting for the user's approval in a cancelled branch is
unspecified: the dialog may stay open and be answered for a call nobody wants.

**Proposal.**

- **Deadlines.** Every async `Host` method receives the effect's context: its key, a cancellation
  token and a **deadline**, the earliest of the enclosing `with timeout` deadlines and the end of
  the frame's `wall` budget. A host bounds the call by the deadline: request timeouts for models and
  MCP calls, and `run_command`'s effective timeout as the smaller of its argument and the time
  left. A command is therefore killed when an enclosing timeout fires, exactly as its own timeout
  kills it today.
- **Cancellation classes.** Every tool has a class. The host's table is checked by the host
  contract tests; a tool it does not list is `drain`.

  | Class | Tools | On cancellation |
  | --- | --- | --- |
  | `abort` | The built-in read tools (`read_file`, `grep`, `glob`, `list_dir`), `web_search`, `web_fetch`, memory search and read, `run_command` only when the permission broker classified the command line as read-only, and MCP tools that the **user's configuration** marks read-only | Stopped at once; the host acknowledges. A call that had already reached a remote server (`web_*`, MCP) is journalled `abandoned`, not `aborted`, because the remote side may still act on it |
  | `drain` | Everything else: file writes and edits, `run_command` and `run_background` not classified read-only, browser actions, plugin tools, MCP tools not marked read-only by the user, `agents.merge`, `tools.install_extension`, memory writes, session control | Runs to completion (or to its deadline); its result is discarded |

  Server annotations (an MCP server's own read-only hint) never make a tool `abort`: the server is
  not trusted to describe itself.
- **Atomic approve-or-withdraw.** A call still waiting for an approval or a `tool.before` hook is
  **withdrawn** whatever its class, and the host resolves a withdrawal racing the user's answer
  atomically: either the answer is taken first (the call is approved and starts, then is cancelled
  by its class) or the withdrawal is taken first (the call never runs). An answer that arrives for
  a withdrawn prompt is discarded entirely, including any "allow for this session" choice in it,
  and is never remembered as a denial either.
- **Outstanding drains.** An **outstanding drain** is a `drain`-class call that was running when
  its scope was cancelled (a `race` loser, a fired timeout, a `par` sibling of a failure, a
  `par_fold` stop) and that the cancelled scope left running. A `drain`-class call in flight in a
  live scope is an ordinary call and is not fenced.
- **Prompt completion by re-parenting.** A cancelled scope completes as soon as its `abort` calls
  have acknowledged; it does not wait for its outstanding drains. They are **re-parented** to the
  nearest enclosing frame that is not itself being cancelled (or to the turn, when every frame is),
  and the re-parenting is an event on the turn sequence. Their reservations already sit on that
  frame's ledgers and its ancestors', so nothing moves but ownership. Harness frames inside the
  cancelled scope therefore close at once, however deeply nested.
- **The drain fence.** The owning frame's **drain fence** holds three things back until its
  outstanding drains have finished: the frame's own close (its harness call cannot return); every
  later `drain`-class call anywhere in the same frame tree, which waits before it starts, so a new
  write never races a stale one; and the completion of the turn. Each outstanding drain keeps its
  reservation until it finishes, then is debited its actual charge. There is no hand-off to the
  engine and no grace period: lifetimes still nest, in the owning frame rather than in the
  cancelled scope.
- **Exempt from the fence.** Effects inside `defer` blocks, which are already exempt from
  cancellation and bounded by the 10-second grace (`spec/03-semantics.md:450-453`), and
  `tools.command_stop` (`spec/06-stdlib/tools.md:40`) never wait behind the fence: they release
  resources, and stopping a stuck process must not queue behind the call it would end. The
  exemption knowingly accepts the stale-write race inside `defer` blocks: a write made in a
  `defer` block may run concurrently with an outstanding drain. `defer` blocks are meant for
  releasing resources, not for writing; the specification should say so beside the exemption.
- **A finite bound for every drain.** It is a host-contract requirement (stage 11a) that every
  `drain`-class tool finishes within a finite bound once started: its own timeout argument, the
  call's deadline, or a host-declared maximum for that tool, after which the host fails the call.
- **Why the fence cannot deadlock.** A call that was waiting for an approval or a hook when its
  scope was cancelled is withdrawn, not drained (below), so every outstanding drain is already
  running in the host and finishes without any action by the program, within the finite bound
  above. The fence waits only for such calls, and the calls it holds back have not started, so no
  outstanding drain can be waiting for anything the fence holds. The argument relies on the bound:
  a host tool that could run forever would turn the fence into an unbounded wait, which is why the
  bound is a contract requirement and not a recommendation.
- **Session stop is unchanged.** `session.control stop` drains every in-flight tool call whatever
  its class, holds the interpreter future and resumes it later, as the specification and the
  native loop do today (`spec/01-purpose-and-model.md:209-212`). Classes apply only to
  cancellation inside the program.

**Determinism and replay.** Cancellation points are recorded (PC-1). Every aborted, abandoned,
drained and withdrawn call, and every re-parenting, is an event on the turn sequence (with its key
and outcome, and for a drained call the digest of its discarded response); replay verifies that
the same calls end the same way, and a mismatch is `Replay`. None of these outcomes is returned
to the program. A host timeout caused by a deadline produces a response (`Timeout`, or
`timed_out: true` from `run_command`) that is recorded like any other. Replay ignores deadlines.

**Security.** `drain` and the fence prevent torn and reordered writes. Only tools whose effect
the engine or the user vouches for can be aborted. Atomic withdrawal means a withdrawn approval
can never become consent, a session-wide allowance, or a remembered denial, so the per-install
approval of extensions holds: an install withdrawn before approval is not installed, and an
approved install drains. Drains keep their reservations, so cancellation never yields free or
unbounded spend. No value crosses the boundary that did not before. Aborting only reduces
activity; it never extends authority.

**Cost and complexity.** Medium. Ratio: the trait signature (stage 11a ships the first
`HOST_API_VERSION`, so no bump), the fence and class handling in the scheduler, `ScriptedHost`
support, tape entries and contract tests. Imperium: the class table, the broker's read-only
classification exposed to dispatch, a user setting for read-only MCP tools, atomic withdrawal in
the approval broker, and abortable dispatch. Killing a command on timeout and dropping an HTTP
request already exist; stage 11a's `RatioHost` spike should confirm the rest before the trait is
fixed.

**Stage.** 11a (trait, scheduler, fence, contract tests); Imperium stages 14 and 17 implement the
classes and withdrawal.

**Conformance tests.** In the host contract suite:

- an `abort` call is acknowledged within a bound after cancellation and its scope returns;
- every `drain`-class tool in the host's table declares a finite bound, and a scripted
  never-ending drain call is failed by the host at that bound, after which the fence releases;
- a remote `abort` call is journalled `abandoned`;
- `race` returns before a losing `drain` write finishes; the enclosing harness call returns only
  after it; a later write in the frame tree starts only after it; the turn completes only after it;
- nested-harness cancellation: a `race` loser calls harness `a`, which calls harness `b`, which
  starts a `drain` write; when the other branch wins, `b` and `a` close at once, the write is
  re-parented to the frame holding the `race`, and that frame closes only after the write;
- a `defer` block's `tools.command_stop` and its other effects run while a fenced write is still
  outstanding, without waiting for it;
- a `run_command` not classified read-only is not aborted by a `race` cancellation, and is killed
  when an enclosing `with timeout` fires;
- an unlisted tool and an MCP tool whose server claims read-only (without user configuration) are
  `drain`;
- an approval-pending call is withdrawn and never runs; an answer after the withdrawal (including
  "allow for this session") changes no session approval and records no denial; an answer before
  the withdrawal starts the call, which then follows its class;
- `session.control stop` drains every in-flight call, as the native loop's twin test does;
- `ScriptedHost` receives the deadline; replay with a tape whose drained or withdrawn entries were
  altered raises `Replay`.

### PC-4: `par_fold`, incremental and early-stopping fan-out

**Problem.** Fan-out returns either when every branch is done (`par_map`, `par_map_settled`) or
with the first success (`race`) (`spec/03-semantics.md:462-469`). Common harness patterns need the
ground between: stop sampling once a majority can no longer be overturned; take the first k
candidates that pass a check; show the user findings as each file's review lands; act on item i
while later items still run. Today these either cannot be written (early stop) or wait for the
slowest branch; `think.self_consistency` always pays for all n samples
(`spec/06-stdlib/think.md:50`). Channels between branches could express them but break structure
(rejected below).

**Proposal.** One built-in in `control`, with three standard-library types:

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `par_fold` | `<T, U, A>(xs: [T], f: fn(T) -> U, init: A, step: fn(A, Int, Result<U>) -> Fold<A>, limit: Int = 4, order: FoldOrder = index) -> Folded<A>` | Runs `f` over `xs` as `par_map` does and delivers each outcome, with its index, to `step`, one at a time; stops early when `step` returns `Stop` | Errors raised by `step`; `Cancelled`, `Replay` and the frame's own `Budget` propagate | as `f` and `step` |

```ratio
enum Fold<A> { Next { acc: A }, Stop { acc: A } }
enum FoldOrder { index, completion }
type Folded<A> = { value: A, delivered: Int, stopped: Bool }
```

- Branches start under the width and `limit`, in program order (PC-2).
- Outcomes reach `step` as `Result<U>`, so `step` chooses between fail-fast (raise) and settled
  handling. `Cancelled`, `Replay` and the frame's own `Budget` propagate, as in
  `par_map_settled` (`spec/06-stdlib/control.md:23`).
- With `order: index`, outcome i is delivered only after outcomes 0 to i−1. With
  `order: completion`, outcomes are delivered as they complete, and each delivery is an event on
  the turn sequence, replayed in recorded order.
- `step` runs in the caller, never concurrently with itself, while branches are live; it is
  therefore under the branch restrictions (`spec/03-semantics.md:471-478`): no state writes, no
  model call or append on a conversation it did not create, and (as for any closure) no assignment
  to captured `var`s. It may make other effects, such as `session.stream` or a judging model call;
  their keys carry a branch-path element of their own (`c<site>#<occurrence>.fold`).
- `Stop` cancels the running branches (cancellation points recorded, PC-1), and branches not yet
  started never start. A raise in `step` cancels them likewise and propagates. `delivered` counts
  the outcomes `step` saw; `stopped` says whether `Stop` ended the fold.

`par_map_settled` is `par_fold` whose `step` collects every outcome, so the scheduler gains a
delivery queue, not a new kind of concurrency.

```ratio
// Self-consistency that stops once one answer holds a strict majority of n.
type Tally = { counts: Map<Untrusted<Str>, Int>, best: Untrusted<Answer>? }

let n = 7
let start: Tally = { counts: {}, best: none }
let tally = par_fold(list.range(0, n), |_| model.generate_as::<Answer>(ask),
  init: start,
  step: |acc, i, r| match r {
    Ok { value } => do {
      let k = text.lower(value.final)
      let counts = acc.counts |> map.update(k, |c| c + 1, 0)
      if counts[k] * 2 > n {
        Fold.Stop { acc: { counts, best: value } }
      } else {
        Fold.Next { acc: { counts, best: acc.best ?? value } }
      }
    },
    Err { error } => Fold.Next { acc: acc },
  },
  limit: n, order: completion)
```

**Follow-on (decided).** `think.self_consistency` and `think.best_of` gain
`early_stop: Bool = false`, built on `par_fold`: self-consistency stops once the leading key holds
a strict majority of `n`; best-of stops when a sample scores at or above an optional
`good_enough: Float?`. Both report the samples taken. Stage 12, with the rest of `think`.

**Determinism and replay.** With `index`, the sequence of `step` calls depends only on the
results. With `completion`, the delivery order is recorded. Cancellation after `Stop` uses PC-1's
recorded cancellation points.

**Security.** `step` obeys the branch restrictions, so it opens no new race on state or
conversations. Labels travel with values as everywhere else, and `step`'s effects pass the guard
and dispatch like any other. No authority is added.

**Cost and complexity.** Medium-low: a delivery queue and the existing `race` and `par_map`
machinery. `builtins.txt` gains one row.

**Stage.** 11b; the `think` follow-on in 12.

**Conformance tests.**

- `stdlib/control/par-fold-index.ratio`: scripted out-of-order completion gives the in-order
  value.
- `stdlib/control/par-fold-completion.ratio`: record, then replay under a different completion
  order; identical value and events.
- `stdlib/control/par-fold-stop.ratio`: after `Stop`, branches not started made no model calls
  (the fixture counts them).
- A `check-error` file for `state.set` inside `step`, and a `runtime-error Invalid` file for
  `model.chat` in `step` on a conversation created outside it.
- A raise in `step` cancels running branches and propagates its error.
- `stdlib/think/self-consistency-early-stop.ratio`: with a scripted majority after four samples,
  fewer than `n` model calls are made.

### PC-5: step charges by cost class, and the representations they require

**Problem.** Pure data built-ins "cost interpreter steps … proportional to input size, about one
step per 64 characters or elements" (`spec/06-stdlib/data.md:3-4`). For updates of persistent
structures the work is O(log n) but the charge is O(n), so building a list with
`xs = xs.push(x)`, or a map with `m = m.set(k, v)`, N times costs about N²/128 steps: roughly
25,000 pushes exhaust the default 5,000,000 steps (`spec/07-security.md:240`) (*estimate* from the
formula, loop overhead excluded). Grep matches, CSV rows, memory hits and per-file findings reach
that size. Conversely, nothing requires the cheap representation everywhere: lists are persistent
vectors (`spec/03-semantics.md:12`) and "persistent structures make this cheap"
(`spec/03-semantics.md:22-23`), but maps (`spec/03-semantics.md:15`) and strings carry no
complexity statement. An implementation that copied maps on update while charging O(log n) would
let a program burn CPU that the step limit does not see (T10, `spec/07-security.md:337`).

**Proposal.**

- Each pure built-in gets a **cost class**, written in its row, and is charged `1 + ⌈w / 64⌉`
  steps, where `w` is the class's work over input size n (and output size m):

  | Class | Work `w` | Examples |
  | --- | --- | --- |
  | `const` | 1 | `list.len`, `list.first`, `map.has`, `text.len` |
  | `log` | ⌈log2(n + 1)⌉ | `list.push`, `list.get`, `list.insert`, `list.remove_at`, `list.slice`, `list.concat`, `map.get`, `map.set`, `map.remove` |
  | `linear` | n | `list.map`, `list.filter`, `text.split`, `text.contains`, `json.parse`, `hash.stable` |
  | `nlogn` | n · ⌈log2(n + 1)⌉ | `list.sort`, `list.sort_by`, `list.unique` |
  | `output` | m | `list.range`, `list.repeat`, `text.repeat` |

  Callbacks charge their own steps as today.
- **Representation requirements**, normative because the charges rest on them: lists are
  persistent vectors with O(log n) push, get, set, slice and concatenation (an RRB tree); maps are
  persistent ordered maps with O(log n) get, set and remove and in-order iteration; records have a
  fixed layout with O(1) field access (their types are static, R5); `Str` is immutable UTF-8 that
  caches its character count and an all-ASCII flag, so `text.len` is O(1) and character-indexed
  slicing of ASCII text needs no scan. An implementation's work per built-in must stay within a
  constant factor of its class.
- **Charges never depend on representation state.** An implementation may update a uniquely owned
  value in place, but uniqueness depends on how long sibling branches hold references, which is
  timing; a charge that depended on it would break `spec/07-security.md:267-268`. Text
  concatenation and interpolation are therefore charged `linear` in the result, and
  `spec/06-stdlib/data.md` advises building long text with `text.join` or a prompt (ropes:
  PC-14).
- **Label summaries over-approximate.** Each container keeps a bit that is set whenever any leaf
  inside it may be labelled, maintained on construction. The bit may be set when no leaf is
  labelled (for example after the labelled part was sliced away), but is never clear when one is,
  so a clear bit lets the sink guard and the `sees` check skip the value in O(1), and the default
  configuration, where nothing is labelled (`spec/03-semantics.md:350-352`), pays nothing per
  effect. A set bit falls back to the leaf scan. Debug builds also run the full leaf scan at every
  sink and assert that it agrees with the bit's verdict. Leaf labels
  (`spec/03-semantics.md:218-219`) keep their meaning; this is representation only.

**Determinism and replay.** Charges depend on sizes only, so they are deterministic.

**Security.** Strengthens T10: charges are tied to required complexities, and a conformance check
ties the implementation to them. Label semantics are unchanged; because the summary errs only
towards scanning, a summary bug can cost time but cannot let a labelled value pass a sink, and
debug builds catch the opposite error.

**Cost and complexity.** Low to medium: a class for each of about 240 data built-ins, and a crate
choice for the persistent structures (for example `imbl`'s `Vector` and `OrdMap`, which are
`Arc`-based and so also serve PC-6; *estimate*, to be confirmed in stage 6).

**Stage.** 6 (value representations, the charging framework, label summaries); 7 to 9 (a class
per built-in, and the class in the generated `builtins.txt`).

**Conformance tests.**

- A new expectation directive, `// expect: steps <n>`, beside those of
  `spec/10-implementation.md:342-346`, with one file per cost class.
- Property: a loop of N pushes or sets is charged O(N log N) steps.
- Debug-build work counters: each built-in's counted elementary operations stay within a fixed
  multiple of its charge.
- Property: after random operations, no container with a labelled leaf has a clear summary bit;
  the debug-build sink scan never disagrees with the bit.

### PC-6: runtime cooperation: yield points, offloaded large built-ins, `Send` values, a prepared form

**Problem.** Pure evaluation runs on the agent's own task (`spec/01-purpose-and-model.md:72-74`).
The daemon multiplexes sessions on a shared async runtime, so a long pure stretch (up to 5,000,000
steps, `spec/07-security.md:240`) or one `json.parse`, `re.*`, `list.sort`, `vec.top_k` or `diff.*`
over a 16 MiB string or a million elements (`spec/07-security.md:247-248`) occupies a runtime
worker with no await point. Other sessions' text streaming and the agent's own sibling branches
stall for tens to hundreds of milliseconds, or seconds for a full step budget (*estimate*). The
1,000-step checkpoint checks cancellation (`spec/03-semantics.md:495`) but nothing says the
interpreter yields there. Separately, the IR hash covers the canonical serialisation
(`spec/01-purpose-and-model.md:68-71`); if a later speed-up (PC-13) had to change the IR, it would
change every hash and `IR_VERSION` and force every pinned package to be re-selected
(`spec/10-implementation.md:193-200`). That boundary is cheap to draw now and expensive later.

**Proposal.**

- **Yield.** At each 1,000-step checkpoint the interpreter yields to the runtime as well as
  checking cancellation.
- **Offload.** A pure built-in whose input exceeds a threshold (an implementation constant, for
  example 64 KiB of text or 16,384 elements) runs on a bounded blocking pool while its branch
  awaits it. Results and charges are identical; heap is accounted before dispatch. Built-ins that
  take callbacks (`list.sort_by`, `list.map`) never run on the pool: program code stays on the
  agent's task.
- **Pool jobs stay charged until they finish.** Each session may have only a small fixed number of
  pool jobs in flight (an implementation constant, for example 2); further offloads wait. A
  cancelled branch stops waiting at once, but its job, bounded by the size limits, runs to the end
  and keeps its place in the session's in-flight cap, its heap charge and its step charge until it
  does; only then is it dropped. Cancelling and re-issuing therefore cannot multiply a session's
  pool use.
- **`Send` values.** Values are `Send + Sync` (`Arc`-based) from stage 6, because moving from
  `Rc` to `Arc` later would touch the whole interpreter.
- **A prepared form.** Each package's canonical IR, which is hashed and gives site ids, is
  lowered once at load into an executable prepared form: names interned, record fields resolved to
  slots, locals to frame slots, built-ins to direct references, site ids carried through. The
  prepared form is never serialised or hashed, so it can later become closure-compiled (PC-13)
  without changing hashes, keys or `IR_VERSION`.

Real multi-threaded evaluation of Ratio code is rejected below.

**Determinism and replay.** Yields and offloads change only how sibling branches interleave;
with PC-1, keys and budget decisions do not depend on that interleaving. Nothing is added to the
tape.

**Security.** No user code runs on the pool, only the crate's pure built-ins; the interpreter
stays safe Rust (`spec/10-implementation.md:275`). The per-session in-flight cap, held until jobs
finish, bounds what one session can take from the pool. One session can no longer stall others,
which improves T10.

**Cost and complexity.** Low.

**Stage.** 6 (yield points, `Arc` values, prepared form); 7 to 9 (offload of the large
built-ins).

**Conformance tests.**

- Runtime test: while one scripted session runs a 5,000,000-step pure loop on a single-threaded
  runtime, a second scripted session keeps making progress (asserted by a progress counter, not by
  timing).
- Property: every offloadable built-in returns identical values, labels and charges below and
  above the threshold.
- A branch cancelled during an offloaded job: the job still counts against the session's
  in-flight cap until it finishes, and its steps and heap stay charged.
- Hash snapshots (stage 6) are computed from the canonical IR before preparation, and stay
  unchanged when the prepared form changes.

### PC-7: smaller, cheaper tapes

**Problem.** The tape records `(key, request digest, response)` for every effect, as JSON Lines
(`spec/01-purpose-and-model.md:136-145`), with 64 MiB per turn by default; beyond that, recording
stops and the turn can no longer be replayed (`spec/07-security.md:260`). Four costs are avoidable.
Output-only effects (`log.*`, `trace.*`, `session.emit`, `session.stream`) produce entries although
the program reads nothing back. `rand` draws produce entries (removed by PC-1). Large tool
responses are stored again although the journal already holds them as `tool_finished` payloads and
conversation messages (`spec/08-imperium.md:349`), and repeated reads of one file store the same
bytes repeatedly. The request digest of a `model.chat` covers the whole conversation, so a loop
of L steps hashes O(L²) bytes: about 200 MB for 200 steps over a 1 MB conversation (*estimate*).

**Proposal.**

1. **Only input-bearing effects get entries:** those whose response the program reads, including
   unit-returning effects that can be refused. Output-only effects that cannot fail get none.
   During replay they are **suppressed**: re-evaluated by the program but not forwarded to the
   host, so a replay never streams, emits or logs to the live session again; the replay's own
   event stream (for `harnesses.replay` and the live view) is produced by the tape host and
   redacted as live events are. To keep divergence detection, the tape ends each turn with a
   **reply digest** (the digest of the entry's returned `Reply` or `Report`, or of its uncaught
   error); replay compares it, and a mismatch is `Replay`. `--ratio-tape-mode strict` adds
   digest-only entries for output-only effects, to locate a divergence precisely.
2. **Content-addressed bodies.** A response body above 1 KiB is stored once per tape, in a blob
   line named by its BLAKE3 digest, and entries refer to it. In the journal, a `ratio_effect` whose
   body is already journalled refers to that event instead of copying it. Labels are stored with
   the reference. Loading a tape verifies every blob against its digest.
3. **Incremental conversation digests.** Each engine conversation keeps a chained digest,
   `d(k) = H(d(k−1), H(message k with its labels))`, stored per message so that a rollback restores
   it. A chat request's digest is `H(chain head, tools digest, options digest)`, O(1) per call. The
   host exposes the chain head through a synchronous method on the trait.
4. **Compression.** Headless tape files may be zstd-compressed, chosen by the `.jsonl.zst`
   extension.
5. **Replayed labels.** A replayed response carries the union of its recorded labels and the
   labels the source rules set again in the replaying session (`spec/03-semantics.md:226-232`): a
   `read_file` of a path that now matches `ratio.secret_paths` is labelled in replay even if it was
   not when recorded. Replay can add labels, never remove them.

**Determinism and replay.** Replay semantics are unchanged for input-bearing effects. Divergence
in an output-only effect is caught at the next input-bearing effect or by the reply digest at the
end of the turn, or at once in strict mode.

**Security.** The redaction rules for headless tapes are unchanged
(`spec/03-semantics.md:346-348`). A labelled body is never named by a plain hash in a headless
tape, because a plain hash of a low-entropy secret reveals it; it uses the per-session redaction
digest (`spec/03-semantics.md:342-344`). A blob that fails verification rejects the tape, so a
crafted tape cannot substitute bodies (T13, `spec/07-security.md:340`). Suppression stops a replay
from re-publishing to the user. The label union extends `TapeHost`'s restore rule
(`spec/10-implementation.md:279`) so that a tape recorded under weaker secrecy settings cannot
launder a value that the current settings label.

**Cost and complexity.** Low to medium. `TAPE_VERSION` is first defined in stage 11a, so no
migration. The digest chain is engine-side (conversations are engine-owned).

**Stage.** 11a (tape format, entry classes, suppression, reply digest, blobs, label union, digest
method on the trait); Imperium 13 (conversation digest chain) and 19 (compressed headless tapes).

**Conformance tests.**

- Size regression: the example programs' tapes stay under recorded sizes.
- Record then replay with blobs gives identical events; a tampered blob rejects the tape.
- Replay makes no `session.stream`, `session.emit` or log call on the live host; a program altered
  only in an output-only effect's argument and in its reply text fails the reply-digest check.
- Property: over random append and rollback sequences, the chained digest equals a full
  recomputation.
- A labelled blob in a headless tape is named by the redaction digest only.
- A tape recorded with secrecy off, replayed with a matching `ratio.secret_paths`: the replayed
  `read_file` result is labelled and refused at a network sink.

### PC-8: early-stopping `model.stream`, with deltas on the tape

**Problem.** `model.stream` calls `on_text` with each delta but cannot stop the generation
(`spec/06-stdlib/model.md:16`); the only token-level stops are `sampling.stop` strings and
`max_tokens` (`spec/05-thinking.md:183-185`). A harness that watches a stream (stop when the answer
line appears, when a forbidden pattern appears, when the text is long enough) pays for the whole
reply. Replay must also call `on_text` with the same deltas, but the specification does not say the
tape keeps delta boundaries.

**Proposal.**

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.stream` | `(p: Prompt, on_text: fn(Untrusted<Str>) -> (), until: (fn(Untrusted<Str>, Untrusted<Str>) -> Bool)? = none, opts: GenOpts = {}) -> Untrusted<Streamed>` | As today; after each delta, calls `until` (when given) with the delta and the text so far; when it returns `true`, the request is cancelled (an `abort`, PC-3) and the call returns the text so far | `ModelFailed`, `Invalid` (effectful callback) | `model ×1` |

```ratio
enum StreamStop { done, until, max_tokens }
type Streamed = { text: Str, stopped: StreamStop }
```

- `until` receives the new delta first, so a predicate can test only what arrived; the text so far
  is a prefix view of one shared append-only buffer, built in O(1) per delta, so neither the
  interpreter nor a delta-only predicate does O(n²) work over a long reply.
- `until` must be effect-free (the checker refuses effects in it, as `on_text` is restricted).
- A streamed response is taped with its delta boundaries, and its text is the text up to the stop.
  Replay delivers the same deltas, so `on_text` and `until` see what they saw live.
- Tokens are charged as the provider reports them for the cancelled request, or, when it reports
  nothing, as the host counts the received text; the charge is recorded. The reservation is PC-1's.
- `model.chat` gets no `until`: an assistant message cut in the middle of a tool call would break
  call and result pairing (`spec/01-purpose-and-model.md:191-192`).
- The return type changes within the draft (decided).

**Determinism and replay.** `until` is pure over recorded deltas, so replay stops at the same
point. Live delta chunking depends on the provider, so live stop points vary as sampling does.

**Security.** `until` receives untrusted text and returns a trusted scalar, the implicit flow the
specification already accepts for branching on model output (`spec/03-semantics.md:196-203`). No
sink is added, and labels apply as for `model.generate`.

**Cost and complexity.** Low.

**Stage.** 12.

**Conformance tests.** A scripted host streams five deltas and `until` holds after the third: the
host sees a cancellation and the text is the first three deltas; `until` was called with each
delta and the matching prefix; replay gives the same text and the same `on_text` calls; a
`check-error` file for an effectful `until`.

### PC-9: single-flight `cached`, `memo` and `once`; labels kept by session caches

**Problem.** `cached`, `memo` and `once` look a key up and compute it on a miss
(`spec/06-stdlib/control.md:30-32`). Sibling branches start together, so in fan-out every branch
misses the same key and computes it: `par_map(files, |f| review(f, cached("repo-summary", ||
summarise_repo())))` makes as many summary calls as there are files, where one was meant. Hits and
misses are already scheduling choices (`spec/03-semantics.md:488`), so their timing dependence is
nothing new; the waste is. Separately, a `session`-scoped `cached` entry and every `once` entry are
stored as JSON in the journal (`ratio_cache`, `spec/06-stdlib/control.md:30-32`,
`spec/08-imperium.md:349`), and the specification does not say what happens to the secrecy labels
of the stored value.

**Proposal.**

- A lookup that finds its key (`(package, key, type digest)` for `cached` and `once`, the argument
  hash for `memo`) being computed by another live branch waits for that computation and takes its
  value, as a hit. The waiting branch is parked and releases its width slot (PC-2).
- If the producer raises or is cancelled, the waiter first in branch-path order is **promoted** to
  producer; errors are still not cached.
- A lookup whose wait would close a wait-for cycle (the producer is waiting, directly or
  transitively, on the looker's branch) computes instead of waiting. A branch that looks up a key
  it is itself computing raises `Invalid`.
- **Recorded and verified from the tape.** Every miss that starts a producer, every wait, every
  hit, every producer's success, failure or cancellation, and every promotion is an event on
  [the turn sequence](#the-turn-sequence-shared-by-pc-1-pc-3-pc-4-and-pc-9), naming the branch
  paths involved. Replay checks them against recorded events only, never against which producers
  happen to be running in the replaying run: a recorded wait must name a producer whose recorded
  miss for the same `(package, key, type digest)` (or `memo` argument hash) comes earlier in the
  sequence and whose recorded end does not; the recorded waits, taken in sequence order together
  with **branch-ancestry edges** derived from the recorded branch paths (a branch waits for its
  own descendants, since it is parked on them), must never form a cycle, so a wait that names the
  waiter's own ancestor as producer is `Replay`; and a recorded promotion must follow the
  previous producer's recorded failure or cancellation and pick, among the recorded waits still
  open at that point, the first in branch-path order. Any failed check is `Replay`.
- **Replay's own order.** In replay a waiter takes its value from the recorded producer, whatever
  the replaying run's progress: if that producer has not yet started or finished in the replaying
  run, the waiter stays parked until it does; if it has finished, the waiter takes the stored value
  at once.
- Budget: the producer pays; waiters pay nothing, as for any hit. A waiter's cancellation ends
  only its wait.
- **Secrecy, stated explicitly.** A cache hit, single-flight or not, that would deliver a labelled
  value to a frame whose `sees` lacks `secrets` raises `Denied { by: sees }`. This is already
  implied by "running effects whose results would carry the label" (`spec/04-harnesses.md:124`),
  since `turn`-scoped caches cross frames of the same package; the text should say so.
- **Session caches persist labels.** A `ratio_cache` entry stores the labels of every leaf with
  the value, as the journal stores labelled conversations, and a hit restores them. The entry is
  redacted like `ratio_effect` wherever it leaves the daemon. A value cached while labelled
  therefore comes back labelled in a later turn or a resumed session.

**Determinism and replay.** Hit, miss, wait and promotion outcomes are recorded scheduling choices,
verified in replay; replay is exact.

**Security.** No channel is added beyond the existing per-package cache sharing; the secrecy rule
for hits becomes explicit; persisted labels close the gap where a labelled value stored in a
session cache could otherwise come back unlabelled. A crafted tape cannot make a branch take
another key's value, because waits are verified against the recorded events of producers of the
same key.

**Cost and complexity.** Medium: an in-flight table, a wait-for check over branch paths, and
labels in the `ratio_cache` encoding.

**Stage.** 11b.

**Conformance tests.** Eight `par_map` branches calling `cached(k, …)` make one model call; a
failing producer leaves the first waiter in branch-path order to recompute; two branches with
crossed keys complete without deadlock; the width case of PC-2 completes; record then replay is
identical, including when replay resolves the producer and the waiters in permuted orders; a tape
whose recorded wait names a producer of another key, or a producer with no earlier recorded miss,
or whose waits form a cycle, or whose wait names the waiter's own ancestor branch as producer,
raises `Replay`; a labelled cached value reaching a frame without `secrets` raises
`Denied { by: sees }`; a labelled value stored with `cached(scope: session)` or `once` is labelled
when read back in a later turn.

## Deferred proposals

### PC-10: provider prefix-cache hints

**Problem.** Many single-shot calls share long prefixes (several critics over one diff,
self-consistency samples over one prompt). Providers cache prefixes, some automatically and some
only at explicit breakpoints. Rendering is deterministic (`spec/05-thinking.md:55-56`), but
section order decides what the shared prefix is (`spec/05-thinking.md:51-53`).

**Proposal.** A `GenOpts.prefix_cache: auto | off` option; the host places breakpoints after the
system message and after leading `data` sections; chapter 05 advises putting shared data first.
**Why deferred:** it is mostly a host capability. **Determinism and security:** outputs are
unaffected; labelled content cannot reach hosted models anyway (`spec/03-semantics.md:300-305`).
**Stage:** 12, with Imperium 13, if taken up. **Tests:** a scripted host receives the breakpoints.

### PC-11: eager tool execution while a reply streams

**Problem.** In `model.loop`, tools start only after the whole reply has arrived
(`spec/06-stdlib/model.md:25`). **Proposal.** Start `abort`-class tool calls (PC-3) as soon as each
call's arguments are complete in the stream, appending results in call order as
`tools.dispatch_all` does. **Why deferred:** it touches the engine's dispatch and hooks
(`tool.before` would see calls of a reply that may still fail) and the pairing invariant; worth it
after parity (stage 23). **Determinism:** unchanged (keys and append order as today).

### PC-12: lint for independent sequential effects

**Problem.** Authors write `let a = model.generate(p1)` then `let b = model.generate(p2)` with no
dependency, doubling latency. **Proposal.** An info-level checker lint suggesting `par { … }` when
consecutive effects share no data and no conversation. **Why deferred:** needs the effect analysis
of stages 4 and 5. Automatic parallelisation is rejected below; the lint keeps the author in
control.

### PC-13: closure compilation of the prepared form

Compile PC-6's prepared form into Rust closures, typically a few times faster than a naive walk
(*estimate*). **Why deferred:** R2 sets the trigger, profiling that shows the walker above 1 % of
turn time (`spec/01-purpose-and-model.md:91`). PC-6 makes it hash-neutral when it comes.

### PC-14: ropes for `Str`

A rope with character counts would make concatenation O(log n) and character slicing O(log n).
**Why deferred:** prompts are built from sections, not concatenation, so long-text building is not
known to be hot; PC-5 charges copy cost meanwhile. Revisit on measurement.

### PC-15: named semaphores and branch priorities

A shared concurrency limit across `par` sites (`with limit "key" n { … }`, a syntax change) and
priority hints for speculative branches. **Why deferred:** hosts already serialise local-model
slots (`spec/01-purpose-and-model.md:226`, `spec/04-harnesses.md:212`), `rate_limit` exists
(`spec/06-stdlib/control.md:24`), and priorities matter only once providers expose priority tiers.

## Rejected alternatives

| Alternative | Why rejected |
| --- | --- |
| Channels or streams between `par` branches, and `select` over them | Every receive becomes a new kind of scheduling choice; deadlocks and unbounded buffers become possible; results stop being combined in program order (`spec/03-semantics.md:484`). Per-item composition inside `par_map` already pipelines, and `par_fold` (PC-4) covers consuming results as they land |
| Automatic parallelisation of independent sequential effects | Changes failure semantics (an effect runs that would not have run after an earlier failure), spend, the order of approval prompts and of events. `par` is one keyword; PC-12 suggests it |
| Speculative or prefetched model calls by the runtime | Spends tokens and may run tools the author did not write; `race` and `par` make speculation explicit and budgeted |
| Automatic coalescing or memoisation of identical model requests | Identical requests above temperature 0 are meant as independent samples (voting depends on it); at temperature 0 backends are still not bit-deterministic; it hides cost. Explicit `cached` keyed by `model.prompt_hash` exists, and PC-9 makes it single-flight |
| Multi-threaded evaluation of pure Ratio code | Pure code is a small share of turn time (R2's premise); it would need atomic step and heap ledgers and deterministic exhaustion across threads (PC-1 becomes much harder) for no measurable gain. PC-6 moves the only heavy pure work, large built-ins, off the runtime instead |
| A bytecode VM | Stays rejected (`spec/01-purpose-and-model.md:91`); PC-6 and PC-13 cover any later need without a second representation |
| Cancellation checks more often than every 1,000 steps | 1,000 tree-walk steps take microseconds (*estimate*); large built-ins are the real latency, handled by PC-6 |
| Step charges that depend on uniqueness (in-place updates charged as cheap) | Uniqueness depends on sibling-branch lifetimes, which are timing; charges would stop being deterministic (PC-5) |
| Handing drained tool calls to the engine after a grace period (revision 1 of PC-3) | The call would outlive every program scope, could race a later write, and the turn could end with spend still running; replaced by the drain fence |
| Trusting an MCP server's read-only annotation for cancellation | The server describes itself; a hostile or wrong server could get a writing tool aborted mid-way. Only the user's configuration counts (PC-3) |
| Unstructured `spawn` and `await` | Stays rejected (`spec/03-semantics.md:500-501`) |
| Cross-turn caching of tool results by the runtime | File contents go stale and the engine owns freshness; `cached(scope: session)` remains for explicit cases |
| Provider batch APIs (discounted, hours of latency) | Not for interactive turns; a headless-eval host may use them later without any language change |

## Implementation slices

In dependency order, matching the roadmap's rule that language changes land as a spec-only pull
request before the stage that implements them (`docs/plans/ratio-roadmap.md`, stage 2S):

1. **Stage 2S (spec only, draft bump):** the accepted text of PC-1 to PC-9, the split of stage 11
   into 11a and 11b in chapter 10's table, and an erratum line per proposal in `spec/README.md`.
   No grammar change, so no parser work.
2. **Stage 6:** PC-6 (yield points, `Arc` values, prepared form) and PC-5's framework
   (representations, charging, label summaries, the `steps` directive).
3. **Stages 7 to 9:** PC-5's class per built-in; PC-6's offload and its per-session cap.
4. **Stage 10:** PC-1's reservations, effective `max_tokens` and steps ledger; PC-2's width
   semantics.
5. **Stage 11a (trait, tape, keys, scheduler):** the turn sequence; PC-1's keys, sequenced
   events, verified replay and random streams; PC-2's scheduler; PC-3's trait, classes, fence,
   withdrawal and contract tests, including the host-contract requirement of a finite bound for
   every `drain`-class tool; PC-7.
6. **Stage 11b (`control`, `par_fold`, single-flight caches, `rand`, `time`, `state`):** PC-4 and
   PC-9, with the rest of stage 11's original `control`, `rand`, `time` and `state` scope.
7. **Stage 12:** PC-8, and PC-4's `early_stop` follow-on in `think`.
8. **Imperium:** 13 (conversation digest chain, deadlines), 14 and 17 (cancellation classes, the
   broker's read-only classification, read-only MCP configuration, atomic withdrawal), 19
   (compressed tapes).

Splitting stage 11 keeps each pull request near the target of about 800 changed lines
(`spec/10-implementation.md:292`); the original row is `spec/10-implementation.md:316`.

**Validation criteria.** Each proposal's conformance tests above; the replay property under random
completion orders with sequenced ledger verification (PC-1, PC-2, PC-4, PC-9); the host contract
suite's cancellation, drain-fence and withdrawal cases against `ScriptedHost` and, in Imperium,
`RatioHost` (PC-3).

**Review status.** An independent security critique of revision 1 is incorporated: B1 (verified
replay of ledgers), B2 (single-flight waiters park), B3 (drain fence), B4 (effective `max_tokens`),
S1 to S8 (abort only by engine or user classification, stop drains, atomic withdrawal, tape
entries for every cancellation outcome and verified cache waits, labels in session caches,
suppressed output effects with a reply digest, charged pool jobs, label union in replay and
over-approximating summaries). Its second round is incorporated too: N1 (cache checks against
recorded events only), N2 (re-parenting outstanding drains), and the text fixes (verified reading
fields, recorded arithmetic inputs, fence exemptions, the definition of an outstanding drain, why
the fence cannot deadlock, the reply floor), with one turn sequence shared by every recorded
scheduling event.

**Revisit conditions.** The walker above 1 % of turn time (PC-13); long-text building measured hot
(PC-14); Imperium exposing provider cache control (PC-10); real harnesses that `par_fold` cannot
express without channels (the first rejected alternative).

## Decisions

Root's decisions on revision 1, after the security critique:

| Topic | Decision |
| --- | --- |
| Proposals | PC-1 to PC-9 accepted as revised here; PC-10 to PC-15 deferred |
| Stage 11 | Split into 11a (trait, tape, keys, scheduler: PC-1, PC-2, PC-3, PC-7) and 11b (`control`, `par_fold`, single-flight caches, `rand`, `time`, `state`: PC-4, PC-9) |
| PC-4 `step` effects | Allowed, under the branch restrictions |
| PC-3 drained calls | Replaced by the drain fence (B3): no hand-off to the engine and no grace period |
| PC-1 reservation without `max_tokens` | Replaced by the effective `max_tokens` rule (B4): never overshoot any cap, a child's included |
| PC-7 output-only effects | Left off tapes by default, suppressed during replay, with an end-of-turn reply digest (S6); strict mode optional |
| PC-8 return type | `model.stream` changes within the draft |
| PC-10 provider caching | Moot: PC-10 deferred |
| PC-4 follow-on | `early_stop` for `think.self_consistency` and `think.best_of`, stage 12 |

### Open items

- **Stage 11a:** confirm with Imperium's `BudgetGate` that a model whose card has no prices is
  refused while any ledger has a cost cap (PC-1), or agree another way to keep cost caps from
  being overshot.
