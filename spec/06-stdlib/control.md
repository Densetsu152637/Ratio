# `control`: retries, fan-out, caching, assertions, logging

The `control` built-ins are in scope **without** their module prefix (`retry(…)`, `par_map(…)`,
`trust(…)`, `assert(…)`); the qualified names (`control.retry`) work too. `log` and `trace` are
its submodules. The block forms `par { … }`, `race { … }` and `with timeout` are syntax
(chapter 03); the functions below are their list and function forms.

## Retrying and racing

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `retry` | `<T>(f: fn() -> T, attempts: Int = 3, backoff: Backoff = Backoff.exponential { start: 1s, factor: 2.0, max: 30s }, on: [ErrorKind] = [ModelFailed, RateLimited, Timeout]) -> T` | Calls `f` until it returns, or raises an error whose kind is not in `on`, or `attempts` calls have failed (then the last error propagates). Waits between attempts follow `backoff` (`fixed { delay }`, `exponential { start, factor, max }`, among the shared types), with jitter drawn from the branch's seeded generator (`data`, `rand`). `Denied`, `Budget` and `Cancelled` are never retried, whatever `on` says | Errors of `f` | as `f`, plus `effect` |
| `timeout` | `<T>(d: Duration, f: fn() -> T) -> T` | Runs `f`, cancelling it after `d`; `d` is a deadline for every host effect inside (chapter 03) | `Timeout` | as `f` |
| `race` | `<T>(fs: [fn() -> T]) -> T` | Runs all, returns the first success, cancels the rest; the winner and each loser's cancellation point are recorded for replay. Returns without waiting for losers' `drain`-class tool calls, which the drain fence holds (chapter 03) | the error of the first function in list order, if all fail | as `fs` |
| `fallback` | `<T>(fs: [fn() -> T], on: [ErrorKind] = [ModelFailed, RateLimited, Timeout, SchemaMismatch, Unsupported]) -> T` | Tries each in order; moves to the next only on the listed kinds | the last error | as `fs` |

## Fan-out

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `par` | `<T>(fs: [fn() -> T], limit: Int? = none) -> [T]` | Runs all concurrently (at most `limit` at once, within the frame's width); results in list order; fail-fast | the first failure (recorded) | as `fs` |
| `par_map` | `<T, U>(xs: [T], f: fn(T) -> U, limit: Int = 4) -> [U]` | `f` over every element concurrently, at most `limit` at once; results by index; fail-fast | the first failure (recorded) | as `f` |
| `par_map_settled` | `<T, U>(xs: [T], f: fn(T) -> U, limit: Int = 4) -> [Result<U>]` | As `par_map`, never failing fast; `Cancelled` and the frame's own `Budget` still propagate | — | as `f` |
| `par_fold` | `<T, U, A>(xs: [T], f: fn(T) -> U, init: A, step: fn(A, Int, Result<U>) -> Fold<A>, limit: Int = 4, order: FoldOrder = index) -> Folded<A>` | Runs `f` over `xs` as `par_map` does and delivers each outcome, with its index, to `step`, one at a time; stops early when `step` returns `Stop` (below) | Errors raised by `step`; `Cancelled`, `Replay` and the frame's own `Budget` propagate | as `f` and `step` |
| `rate_limit` | `(key: Str, n: Int, per: Duration) -> ()` | Waits until fewer than `n` calls with this key have passed in the last `per`, across the session; the wait counts against `wall` | `Timeout` (when the wait would pass the frame's wall budget) | `effect` |

Branches start within the frame's width and each construct's `limit`, in program order
(chapter 03, width and admission). The width counts running branches only, so nested fan-out such
as `par_map(groups, |g| par_map(g.items, check, limit: 4), limit: 16)` never deadlocks. Branches
share their frame's budgets; to give each branch its own share, open a scoped budget inside it:

```ratio
fn review_all(files: [Str]) -> [Untrusted<Str>] {
  par_map(files, |f| with { budget: { tokens: 20k } } { review(f) })
}
```

**`par_fold`.** Outcomes reach `step` as `Result<U>`, so `step` chooses between fail-fast (raise)
and settled handling; `Cancelled`, `Replay` and the frame's own `Budget` propagate, as in
`par_map_settled`. With `order: index`, outcome i is delivered only after outcomes 0 to i−1; with
`order: completion`, outcomes are delivered as they complete, and each delivery is an event on the
turn sequence (chapter 01), replayed in recorded order. `step` runs in the caller, never
concurrently with itself, while branches are live, so it is under the branch restrictions
(chapter 03): no state writes, no model call on or append to a conversation it did not create,
and no assignment to captured `var`s. It may make other effects (a `session.stream`, a judging
model call); their keys carry the branch-path element `c<site>#<occurrence>.fold`. `Stop` cancels
the running branches (at recorded cancellation points), and branches not yet started never start;
a raise in `step` cancels them likewise and propagates. `delivered` counts the outcomes `step`
saw; `stopped` says whether `Stop` ended the fold. The types `Fold<A>`, `FoldOrder` and
`Folded<A>` are among the shared types (README). `par_map_settled` is `par_fold` whose `step`
collects every outcome.

```ratio
// Self-consistency that stops once one answer holds a strict majority of n.
type Answer = { final: Str, reasoning: Str }
type Tally = { counts: Map<Untrusted<Str>, Int>, best: Untrusted<Answer>? }

fn majority(ask: Prompt, n: Int) -> Folded<Tally> {
  let start: Tally = { counts: {}, best: none }
  par_fold(list.range(0, n), |_| model.generate_as::<Answer>(ask),
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
}
```

## Caching

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `cached` | `<T>(key: Str, f: fn() -> T, scope: CacheScope = turn, ttl: Duration? = none) -> T` | Returns the stored value for `(package, key, type digest of T)` (chapter 03), or calls `f` and stores its result; hits and misses are recorded on the tape. `turn` and `frame` scopes live in memory; `session` scope stores the value's JSON in the journal (`ratio_cache` event, at most 1 MiB per session, oldest evicted) so later turns and resumed sessions reuse it. `T` must be schema-able. Errors are not cached. Keys are namespaced by package | `Invalid` (not schema-able for `session`) | as `f`; `effect` |
| `memo` | `<A, R>(f: fn(A) -> R) -> fn(A) -> R` | A function that remembers results by `hash.stable(argument)` for the rest of the turn | — | `pure` |
| `once` | `<T>(key: Str, f: fn() -> T) -> T` | Calls `f` at most once per session for this key (stored like `cached` with `session` scope and no expiry) | as `cached` | as `f` |

All three are **single-flight**:

- A lookup that finds its key (`(package, key, type digest)` for `cached` and `once`, the argument
  hash for `memo`) being computed by another live branch waits for that computation and takes its
  value, as a hit. The waiting branch is parked and releases its width slot (chapter 03). The
  producer pays; waiters pay nothing, as for any hit. A waiter's cancellation ends only its wait.
- If the producer raises or is cancelled, the waiter first in branch-path order is **promoted** to
  producer; errors are still not cached.
- A lookup whose wait would close a wait-for cycle (the producer is waiting, directly or
  transitively, on the looker's branch) computes instead of waiting. A branch that looks up a key
  it is itself computing raises `Invalid`.
- Every miss that starts a producer, every wait, every hit, every producer's success, failure or
  cancellation, and every promotion is an event on the turn sequence (chapter 01), naming the branch
  paths involved. In a headless tape, a labelled `cached` or `once` key, and a `memo` argument hash
  derived from a labelled argument, is recorded only by the per-session redaction digest, as
  labelled blobs are. Replay checks them against recorded events only: a recorded wait must name a
  producer whose recorded miss for the same key comes earlier in the sequence and whose recorded end
  does not; the recorded waits, taken in sequence order together with the branch-ancestry edges of
  the recorded branch paths (a branch waits for its own descendants), must never form a cycle, so a
  wait naming the waiter's own ancestor as producer is `Replay`; and a recorded promotion must
  follow the previous producer's recorded failure or cancellation and pick, among the recorded waits
  still open at that point, the first in branch-path order. In replay a waiter takes its value from
  the recorded producer, whatever the replaying run's progress: it stays parked until that producer
  finishes in the replaying run, or takes the stored value at once if it already has.
- A hit, single-flight or not, that would deliver a labelled value to a frame whose `sees` lacks
  `secrets` raises `Denied { by: sees }` (chapter 04). A `ratio_cache` entry stores the labels of
  every leaf with the value and a hit restores them; the entry is redacted like `ratio_effect`
  wherever it leaves the daemon.

## Assertions, errors and trust

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `assert` | `(cond: Bool, message: Str = "assertion failed") -> ()` | Raises `Assertion` when `cond` is false | `Assertion` | `pure` |
| `assert_eq` | `<T>(a: T, b: T, message: Str? = none) -> ()` | Raises `Assertion` showing both values (truncated) when they differ | `Assertion` | `pure` |
| `ensure` | `<T>(v: T?, message: Str) -> T` | Unwraps an option or raises `Assertion` | `Assertion` | `pure` |
| `fail` | `(tag: Str, message: Untrusted<Str>, data: Json = null) -> Error` | Builds a `User { tag }` error to `raise` | — | `pure` |
| `question` | `(text: Untrusted<Str>, options: [Untrusted<Str>] = []) -> Error` | Builds a `Question` error to `raise`: the way a frame hands a question back to its caller, and an entry hands it to the user or the dispatcher (chapter 04) | — | `pure` |
| `result` | `<T>(f: fn() -> T) -> Result<T>` | Calls `f` and reifies its outcome; `Cancelled`, `Replay` and the frame's own `Budget` still propagate | — | as `f` |
| `trust` | `<T>(x: Untrusted<T>, because: Str) -> T` | The only conversion from untrusted to trusted (chapter 03). `T` must be monomorphic (a checker error otherwise). Emits a `ratio_trust` event with the package, version, site, reason and the value's `hash.stable` digest (no excerpt). Subject to `ratio.trust`; refused in authored packages | `Denied` (`ratio.trust = deny`) | `effect` |
| `as_task` | `(x: Untrusted<Str>, because: Str) -> Brief` | Converts untrusted text to a `Brief`, usable only in `task` sections (chapter 03); audited exactly like `trust` | as `trust` | `effect` |
| `declassify` | `<T>(x: Secret<T>, because: Str) -> T` | Removes the secrecy label (chapter 03). Monomorphic only; always asks the user (no rule can allow it); refused when unattended; journalled with a digest | `Denied` | `user` |
| `cancelled` | `() -> Bool` | Whether this branch has been asked to stop; long pure loops may check it to stop early (they are cancelled at the next 1,000 steps anyway) | — | `engine` |

## Budgets

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `budget_left` | `() -> Budget` | What this frame may still spend of each budget kind (the minimum over its ancestors) | — | `engine` |
| `spent` | `() -> Spent` | What this frame has spent so far | — | `engine` |

A scoped budget is the `with { budget: { … } } { … }` block (chapter 04). Every reading is an
event on the turn sequence (chapter 01); replay returns the recorded reading after checking its
`tokens`, `cost`, `model_calls` and `tool_calls` against the rebuilt ledgers (chapter 04).

## Logging and tracing

Logs and traces go to the journal and to clients that ask for them, never to the model. They
accept untrusted text, which is shown as plain text.

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `log.debug` | `<T>(message: Untrusted<Str>, data: T? = none) -> ()` | A `ratio_log` event at level `debug` (dropped unless the session's log level includes it) | — | `effect` |
| `log.info` | `<T>(message: Untrusted<Str>, data: T? = none) -> ()` | Level `info` | — | `effect` |
| `log.warn` | `<T>(message: Untrusted<Str>, data: T? = none) -> ()` | Level `warn`; also shown as a notice in the desktop's transcript | — | `effect` |
| `log.error` | `<T>(message: Untrusted<Str>, data: T? = none) -> ()` | Level `error`; also a notice | — | `effect` |
| `trace.span` | `<T>(name: Str, f: fn() -> T) -> T` | Runs `f` inside a named span; the span's start, end, duration, spend and outcome are a `ratio_span` event, nested by frame path | Errors of `f` | `effect` |
| `trace.event` | `<T>(name: Str, data: T) -> ()` | A point event in the current span | — | `effect` |

Log and trace `data` must be schema-able; at most 16 KiB per event, cut with a marker beyond.
