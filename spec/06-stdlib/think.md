# `think`: reasoning methods

These built-ins package common ways of making a model reason better. Each one is a short program
over `model.*` and control flow, shipped in the standard library with its prompt texts
(`std/prompts/think.*.md`); a harness can rewrite any of them in Ratio when it needs a variant.
Single-shot calls (`model.generate*`) never see a conversation, so a critique, a judgement or a
vote is not anchored by the reasoning that produced the work it looks at.

Functions that take another function (`sample`, `expand`, `score`, …) accept harnesses as well
(chapter 04): pass a harness to run that part with its own model, tools and budget.

## Structured thinking

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `think.steps` | `<A>(p: Prompt, steps: [Str], visible: Bool = true, opts: GenOpts = {}) -> Stepped<A>` | One structured call whose schema has one note field per step, in order, and an `answer: A`. Returns `{ answer: Untrusted<A>, trace: [Untrusted<StepNote>] }`. With `visible: false` the notes are returned in `trace` only and never logged | `SchemaMismatch`, `ModelFailed`, `Invalid` (no steps, more than 12) | `model ×1..3` |
| `think.scratchpad` | `() -> Scratchpad` | A new private notes buffer for this frame (chapter 05). At most 64 KiB; older notes are dropped first with a marker | — | `pure` |
| `think.note` | `(pad: Scratchpad, text: Untrusted<Str>) -> ()` | Appends a note (also `pad.note(text)`) | `Invalid` (pad from another frame) | `pure` |
| `think.notes` | `(pad: Scratchpad, last: Int? = none) -> Untrusted<Str>` | The notes, newest last, optionally only the last `n` (also `pad.text()`) | — | `pure` |
| `think.step_back` | `(question: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Str>` | Asks for the general principle or concept behind the question before answering it; returns the principle text, for use as a `data` section in the answering prompt | `ModelFailed` | `model ×1` |
| `think.until` | `<T>(init: T, step: fn(T) -> T, stop: Stop, score: fn(T) -> Float = none) -> Iterated<T>` | Applies `step` repeatedly from `init` until `stop` fires (chapter 05); with `score`, `no_progress` and `converged` use it. Returns `{ value, rounds, stopped, best_score? }`, where `value` is the best-scoring value when `score` is given, else the last | Errors of `step` | as `step` |

## Planning, decomposition, reflection

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `think.plan` | `(goal: Untrusted<Str>, context: Prompt? = none, max_steps: Int = 8, opts: GenOpts = {}) -> Untrusted<Plan>` | A plan in Imperium's `update_plan` shape: goal, ordered steps with acceptance checks and dependencies, assumptions, open questions. Steps that depend on later steps are refused and repaired | `SchemaMismatch`, `ModelFailed` | `model ×1..3` |
| `think.replan` | `(plan: Untrusted<Plan>, step: Untrusted<Str>, reason: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Plan>` | Revises the remaining steps after `step` failed for `reason`; steps marked `done` are kept unchanged (checked) | `SchemaMismatch`, `ModelFailed`, `NotFound` (no such step) | `model ×1..3` |
| `think.decompose` | `(task: Untrusted<Str>, max_parts: Int = 6, opts: GenOpts = {}) -> Untrusted<[Subtask]>` | Splits a task into self-contained parts, each `{ title, brief, depends_on: [Str], kind: research \| change \| check }`, each brief written to stand alone (the shape of a `spawn_agent` brief) | `SchemaMismatch`, `ModelFailed` | `model ×1..3` |
| `think.hypothesize` | `(observations: Untrusted<Str>, n: Int = 3, opts: GenOpts = {}) -> Untrusted<[Hypothesis]>` | `n` distinct explanations, each `{ statement, check, likelihood: Float }`, where `check` is how to tell it apart from the others | `SchemaMismatch`, `ModelFailed` | `model ×1` |
| `think.reflect` | `(trace: Untrusted<Str>, outcome: Str, opts: GenOpts = {}) -> Untrusted<Reflection>` | After an attempt, `{ what_happened, mistakes: [Str], lessons: [Str] }`; lessons are phrased as reusable rules (the shape of a `lesson` memory) | `SchemaMismatch`, `ModelFailed` | `model ×1` |
| `think.rubric` | `(task: Untrusted<Str>, n: Int = 5, opts: GenOpts = {}) -> Untrusted<[Criterion]>` | Writes `n` grading criteria for a task, each `{ name, description, weight }`, for `model.score` and `think.critique` | `SchemaMismatch`, `ModelFailed` | `model ×1` |

## Critique, revision, verification

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `think.critique` | `(work: Untrusted<Str>, rubric: Str, opts: GenOpts = {}) -> Untrusted<Critique>` | `{ pass: Bool, issues: [{ severity: blocker \| major \| minor, location?, problem, fix? }], summary }`. `pass` must be false when any blocker is listed (checked, repaired) | `SchemaMismatch`, `ModelFailed` | `model ×1..3` |
| `think.revise` | `(work: Untrusted<Str>, critique: Untrusted<Critique>, opts: GenOpts = {}) -> Untrusted<Str>` | Rewrites the work to address the issues, blockers first, changing nothing the critique did not raise | `ModelFailed` | `model ×1` |
| `think.refine` | `<T>(draft: T, critique: fn(T) -> Untrusted<Critique>, revise: fn(T, Untrusted<Critique>) -> T, stop: Stop = { max_rounds: 3 }) -> Refined<T>` | The critique-and-revise loop: critique; stop if `pass` (or `stop.until`); else revise. Returns `{ value, rounds, history: [Untrusted<Critique>], stopped }`; `value` is the last revision, or the draft when the first critique passes | Errors of the callbacks | as callbacks |
| `think.verify` | `(claim: Untrusted<Str>, evidence: [Untrusted<Str>], opts: GenOpts = {}) -> Untrusted<Verification>` | Checks a claim against numbered evidence only: `{ status: supported \| refuted \| unknown, citations: [Int], reasons }`. A `supported` with no citations is repaired to `unknown` | `SchemaMismatch`, `ModelFailed` | `model ×1..2` |
| `think.check` | `<T>(value: T, checks: [Check<T>]) -> CheckReport` | Runs each `Check { name, run: fn(T) -> Bool }` in order (all of them, not stopping at the first failure); `{ passed, results: [{ name, ok, error? }] }`. A check that raises counts as failed with its error. Checks may make effects (run tests, read files) | — | as checks |
| `think.counterexample` | `(claim: Untrusted<Str>, context: Prompt? = none, opts: GenOpts = {}) -> Untrusted<Counterexample?>` | Tries to find an input or case that breaks the claim; `none` when the model finds none. A returned counterexample is a candidate to check, not proof | `SchemaMismatch`, `ModelFailed` | `model ×1` |

## Sampling, voting, consensus

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `think.vote` | `<T>(candidates: [T], key: fn(T) -> Untrusted<Str>) -> Vote<T>` | Groups candidates by `key` and picks the largest group; ties go to the group whose first member came first. Returns `{ winner: T, key, counts: Map<Untrusted<Str>, Int>, agreement: Float, failures: Int }` where `winner` is the first member of the winning group, `agreement` its share of the candidates, and `failures` is 0 | `Invalid` (no candidates) | `pure` |
| `think.self_consistency` | `<T>(n: Int, sample: fn() -> T, key: fn(T) -> Untrusted<Str>, parallel: Bool = true, early_stop: Bool = false) -> Vote<T>` | Calls `sample` `n` times (in parallel within the frame's width when `parallel`), then `think.vote` over the samples that returned. Samples that raise are dropped and counted in `Vote.failures` (never as a key of `counts`, so no answer can collide with them); if all raise, the error of the first sample in order propagates. With `early_stop`, sampling runs as a `par_fold` and stops once the leading key holds a strict majority of `n`; `counts` and `failures` then cover only the samples taken | Errors of `sample` (all failed) | as `sample` ×n |
| `think.best_of` | `<T>(n: Int, sample: fn() -> T, score: fn(T) -> Float, parallel: Bool = true, early_stop: Bool = false, good_enough: Float? = none) -> Scored<T>` | `n` samples, each scored; returns the highest (first on ties), with `samples` set to the number of samples taken. With `early_stop`, sampling runs as a `par_fold` and stops when a sample scores at or above `good_enough`; `early_stop: true` with `good_enough: none` is refused (a checker error when both are written as literals, else `Invalid`) | as `self_consistency`; `Invalid` (`early_stop` without `good_enough`) | as callbacks ×n |
| `think.consensus` | `<T>(answers: [T], method: ConsensusMethod = majority, show: fn(T) -> Untrusted<Str> = text.show, opts: GenOpts = {}) -> Consensus<T>` | `majority` (more than half share a `show` text), `unanimous` (all do), or `judge` (a model reads all answers as data and picks or merges one). Returns `{ value?: T, agreed: Bool, dissent: [T] }` | `ModelFailed` (judge) | `pure`, or `model ×1` for `judge` |
| `think.reconcile` | `(answers: [Untrusted<Str>], question: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Str>` | Writes one answer consistent with the points the answers agree on, and states the points of disagreement it could not settle | `ModelFailed` | `model ×1` |
| `think.debate` | `(question: Untrusted<Str>, sides: [fn(DebateTurn) -> Untrusted<Str>], judge: fn(DebateRecord) -> Untrusted<Judgement>, rounds: Int = 2) -> DebateResult` | Each round, each side gets `DebateTurn { question: Untrusted<Str>, round: Int, record: DebateRecord }` (every earlier argument as data) and argues; after the last round the judge gets the whole record. Sides of a round run in parallel. Returns `{ record, verdict, winner?: Int }`. Sides are typically harnesses with different models or system prompts | Errors of callbacks; `Invalid` (fewer than 2 sides) | as callbacks |

## Search

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `think.tree_search` | `<N>(root: N, expand: fn(N) -> [N], score: fn(N) -> Float, is_goal: fn(N) -> Bool, strategy: SearchStrategy = best_first, beam: Int? = none, max_nodes: Int = 64, max_depth: Int = 6) -> SearchResult<N>` | Tree-of-thought search. `bfs`, `dfs` or `best_first` over nodes; `beam` keeps only the best `beam` children per level. Stops at the first goal, at `max_nodes` expansions or when the frontier is empty. Returns `{ best: N, path: [N], goal_reached: Bool, expanded: Int, stopped }` with `best` the goal or else the highest-scoring node | Errors of callbacks | as callbacks |
| `think.beam_search` | `<N>(init: [N], expand: fn(N) -> [N], score: fn(N) -> Float, width: Int = 3, depth: Int = 4) -> [Scored<N>]` | Keeps the `width` best states at each of `depth` levels; expansions of one level run in parallel; returns the final beam, best first | Errors of callbacks | as callbacks |
| `think.mcts` | `<N>(root: N, expand: fn(N) -> [N], rollout: fn(N) -> Float, iterations: Int = 32, exploration: Float = 1.4) -> SearchResult<N>` | Monte Carlo tree search with UCT selection; `rollout` returns a value in 0..1. Ties in selection break by child order, so a run is replayable | Errors of callbacks | as callbacks |

## Confidence and clarification

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `think.confidence` | `(question: Untrusted<Str>, answer: Untrusted<Str>, method: ConfidenceMethod = self_report, n: Int = 5, opts: GenOpts = {}) -> Confidence` | An estimate in 0..1 with its method: `self_report` (asks the model to rate, calibrated by nothing: a weak signal), `samples` (agreement of `n` fresh answers with `answer`), `logprob` (mean token probability of `answer` given the question) | `Unsupported` (`logprob` without logprobs), `ModelFailed` | `model ×1` or `×n` |
| `think.uncertainty` | `(keys: [Untrusted<Str>]) -> Float` | Normalised entropy of the distribution of keys, 0 (all equal) to 1 (all different) | `Invalid` (empty) | `pure` |
| `think.clarify` | `(missing_parameter: Str, options: [Untrusted<Str>], question: Str? = none, confidence: Float? = none) -> UserText?` | Asks the user about one missing piece, through Imperium's `request_clarification` (`docs/05-tools-and-isolation.md#request_clarification`); untrusted options are shown wrapped as quoted choices. Returns what the user typed or chose (the user's own act, so `UserText`), or `none` when the user left it blank or the prompt timed out; a timeout never selects an option. In an unattended frame, or in a subagent, it raises `Question` instead, so the dispatcher can answer | `Question` (unattended) | `user` |
