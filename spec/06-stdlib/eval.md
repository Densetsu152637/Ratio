# `eval`: tests, metrics and the record and replay seam

These built-ins serve three readers: a harness that verifies its own work against tests, an
evaluation run (`imperium run --headless --json --harness …`, chapter 08) that scores harnesses,
and the package's own `test` blocks, which run against a scripted host.

## Verifying work

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `eval.run_tests` | `(command: Str, format: TestFormat = auto, timeout: Duration = 600s) -> Untrusted<TestReport>` | Runs a test command through `run_command` and parses its output: `auto` recognises cargo test, pytest, Jest and Vitest, Go test, TAP and JUnit XML written to stdout. `{ passed: Int, failed: Int, skipped: Int, ok: Bool, cases: [{ name, status: pass \| fail \| skip, message? }], exit_code, raw }`. Parsing never fails: unrecognised output gives `cases: []` and `ok` from the exit code | `Denied`, `Timeout` | `tool:run_command` |

## Recording results

Metrics, scores and labels are journalled as `ratio_metric` events and, in a headless run, appear
in the output's `ratio.metrics` (chapter 08).

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `eval.metric` | `(name: Str, value: Float, tags: Map<Str, Str> = {}) -> ()` | Records a named measurement (counts, latencies, rounds used) | `Invalid` (`NaN`) | `effect` |
| `eval.score` | `(name: Str, value: Float, reason: Untrusted<Str>? = none) -> ()` | Records a score in 0..1 the harness assigns to its own result (for example, the fraction of tests passing); scorers may use it or ignore it | `Invalid` (out of range) | `effect` |
| `eval.label` | `(key: Str, value: Str) -> ()` | A run-level label (the strategy chosen, the branch taken) | — | `effect` |
| `eval.expect` | `(cond: Bool, name: Str) -> Bool` | A soft assertion: records `name` as passed or failed and returns `cond`, without raising | — | `effect` |
| `eval.timer` | `<T>(name: Str, f: fn() -> T) -> T` | Runs `f` and records its wall time as a metric | Errors of `f` | `effect` |

## The headless task

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `eval.task` | `() -> EvalTask?` | In a headless run, the task: `{ id, text, raw: Json }` (`text` is the task's prompt) (the task file's other fields, as `task.raw` in the output); `none` in an ordinary session | — | `engine` |
| `eval.is_headless` | `() -> Bool` | Whether the session is an evaluation session | — | `engine` |
| `eval.fixture` | `(name: Str) -> Json` | A named fixture: in a headless run, `ratio_fixtures.<name>` of the task file; in `test` blocks, a fixture declared next to the package (`<package>.fixtures/<name>.json`) | `NotFound` | `engine` |

## Record and replay

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `eval.mode` | `() -> TapeMode` | `live`, `record` or `replay` (chapter 01) | — | `engine` |
| `eval.tape` | `() -> TapeInfo?` | `{ path?, entries, replayed, diverged }` when a tape is in use | — | `engine` |
| `eval.golden` | `(name: Str, value: Untrusted<Str>) -> Bool` | In `test` blocks: compares with `<package>.golden/<name>.txt` (writes it when run with `--update-golden`); elsewhere records the value as a metric attachment and returns `true` | — | `effect` |

## Scripting the host in tests

Only available in `test` blocks; elsewhere they raise `Invalid`.

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `eval.script_model` | `(replies: [ScriptedReply]) -> ()` | Queues model replies for the scripted host: `{ when?: Str, text?: Str, json?: Json, tool_calls?: [ToolCall], error?: ErrorKind }`. Each model call takes the first reply whose `when` occurs in the rendered request (or the next unmatched reply) | `Invalid` (outside tests) | `pure` |
| `eval.script_tool` | `(name: Str, replies: [ScriptedToolReply]) -> ()` | Queues results for a tool: `{ when?: Json, ok: Bool, text: Str, denied?: DeniedBy }` | `Invalid` (outside tests) | `pure` |
