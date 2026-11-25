# `control`: retries, fan-out, caching, assertions, logging

The `control` built-ins are in scope **without** their module prefix (`retry(…)`, `par_map(…)`,
`trust(…)`, `assert(…)`); the qualified names (`control.retry`) work too. `log` and `trace` are
its submodules. The block forms `par { … }`, `race { … }` and `with timeout` are syntax
(chapter 03); the functions below are their list and function forms.

## Retrying and racing

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `retry` | `<T>(f: fn() -> T, attempts: Int = 3, backoff: Backoff = Backoff.exponential(1s, 2.0, 30s), on: [ErrorKind] = [ModelFailed, RateLimited, Timeout]) -> T` | Calls `f` until it returns, or raises an error whose kind is not in `on`, or `attempts` calls have failed (then the last error propagates). Waits between attempts follow `backoff` (`fixed(d)`, `exponential(start, factor, max)`), with jitter drawn from the frame's seeded generator. `Denied`, `Budget` and `Cancelled` are never retried, whatever `on` says | Errors of `f` | as `f`, plus `effect` |
| `timeout` | `<T>(d: Duration, f: fn() -> T) -> T` | Runs `f`, cancelling it after `d` | `Timeout` | as `f` |
| `race` | `<T>(fs: [fn() -> T]) -> T` | Runs all, returns the first success, cancels the rest; the winner and each loser's cancellation point are recorded for replay | the error of the first function in list order, if all fail | as `fs` |
| `fallback` | `<T>(fs: [fn() -> T], on: [ErrorKind] = [ModelFailed, RateLimited, Timeout, SchemaMismatch, Unsupported]) -> T` | Tries each in order; moves to the next only on the listed kinds | the last error | as `fs` |

## Fan-out

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `par` | `<T>(fs: [fn() -> T], limit: Int? = none) -> [T]` | Runs all concurrently (at most `limit` at once, within the frame's width); results in list order; fail-fast | the first failure (recorded) | as `fs` |
| `par_map` | `<T, U>(xs: [T], f: fn(T) -> U, limit: Int = 4) -> [U]` | `f` over every element concurrently, at most `limit` at once; results by index; fail-fast | the first failure (recorded) | as `f` |
| `par_map_settled` | `<T, U>(xs: [T], f: fn(T) -> U, limit: Int = 4) -> [Result<U>]` | As `par_map`, never failing fast; `Cancelled` and the frame's own `Budget` still propagate | — | as `f` |
| `rate_limit` | `(key: Str, n: Int, per: Duration) -> ()` | Waits until fewer than `n` calls with this key have passed in the last `per`, across the session; the wait counts against `wall` | `Timeout` (when the wait would pass the frame's wall budget) | `effect` |

## Caching

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `cached` | `<T>(key: Str, f: fn() -> T, scope: CacheScope = turn, ttl: Duration? = none) -> T` | Returns the stored value for `(package, key, type digest of T)` (chapter 03), or calls `f` and stores its result; hits and misses are recorded on the tape. `turn` and `frame` scopes live in memory; `session` scope stores the value's JSON in the journal (`ratio_cache` event, at most 1 MiB per session, oldest evicted) so later turns and resumed sessions reuse it. `T` must be schema-able. Errors are not cached. Keys are namespaced by package | `Invalid` (not schema-able for `session`) | as `f`; `effect` |
| `memo` | `<A, R>(f: fn(A) -> R) -> fn(A) -> R` | A function that remembers results by `hash.stable(argument)` for the rest of the turn | — | `pure` |
| `once` | `<T>(key: Str, f: fn() -> T) -> T` | Calls `f` at most once per session for this key (stored like `cached` with `session` scope and no expiry) | as `cached` | as `f` |

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

A scoped budget is the `with { budget: { … } } { … }` block (chapter 04).

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
