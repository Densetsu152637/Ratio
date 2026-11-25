# 03 Semantics

## Values

| Kind | Types | Notes |
| --- | --- | --- |
| Unit | `()` | The value of statements and of blocks ending in one |
| Scalars | `Bool`, `Int`, `Float`, `Duration`, `Money` | `Int` is 64-bit signed, overflow raises `Invalid`; `Duration` is whole microseconds; `Money` is exact to 1e-6 USD |
| Text | `Str`, `Untrusted<Str>` | Unicode scalar values; indexes and lengths count characters, not bytes |
| Options | `T?` | `none` or a `T`; `T??` is `T?` |
| Lists | `[T]` | Persistent vectors; indexing out of range raises `Invalid` (use `list.get` for `T?`) |
| Tuples | `(A, B, …)` | Fixed arity, at most 8 |
| Records | `{ a: A, b?: B }` | Structural; a field declared with `?` may be absent, and reads as `B?` |
| Maps | `Map<K, V>` | Ordered by key; `K` is `Str`, `Int`, `Bool` or a unit-only enum. Where a `Map<Str, V>` is expected, a record literal (`{ attempt: "2" }`) builds one |
| Enums | `enum E { A, B { x: Int } }` | Nominal tagged unions; variant payloads are records |
| Functions | `fn(A, B) -> C` | Named functions and closures; harnesses are functions of a special kind (chapter 04) |
| Dynamic | `Json` | Any JSON value; must be decoded through a schema before typed use |
| Handles | `Conversation`, `Task<T>`, `Process`, `Tab`, `Scratchpad`, `Mark` | References to things the engine owns; see below |

All values are **immutable**. "Updating" a record makes a new one (`{ ..r, x: 1 }`); `list.push`
returns a new list. Persistent structures make this cheap.

**Handles** are the only references. A handle names something the engine owns and keeps (a
conversation, a subagent task, a background process, a browser tab). Operations on a handle are
effects. A handle cannot be put in `state`, encoded to JSON, or returned from a harness to a
caller in a different frame, except `Task<T>` (which the engine tracks across frames) and
`Conversation` returned as a `Transcript` snapshot value.

## Types

### Decision R5: static types with local inference and one dynamic escape

Every binding, parameter, field and return has a type known before the program runs. Function
and harness signatures are written out; local `let` and `var` bindings and closure parameters are
inferred (bidirectional checking: the expected type flows into closures and literals). Generic
type parameters are allowed on functions and type aliases; harnesses are monomorphic, because a
harness signature is a contract recorded in the registry and used to derive schemas.

Records are structural: two record types with the same fields are the same type. A record value
must have exactly the declared fields (no width subtyping); spread and `record.pick` build
narrower records explicitly. Enums are nominal.

**Subtyping** is deliberately small:

- `T` is a subtype of `T?`.
- `Never`, the type of `raise`, `return`, `break` and `continue`, is a subtype of every type, so
  `else { raise … }` fits any branch type.
- `UserText` (the user's own message) is a subtype of `Str`: trusted text with a known origin.
- `T` is a subtype of `Untrusted<T>` (trusted data may always be treated as untrusted).
- Lists, options, tuples, maps (in values) and records are covariant in their element types.
  This is sound because values are immutable.
- A function type is contravariant in parameters and covariant in result.

`Json` is the one dynamic type. It comes from `json.parse`, from `tools.call_tool` and MCP
results, and from `meta` introspection. It supports only JSON operations (`json.get`, `json.keys`,
indexing that returns `Json?`); it is turned into a typed value by `json.decode<T>(j)`, which
validates against `T`'s schema and raises `SchemaMismatch` with every violation listed. So a
dynamic value never reaches typed code unchecked.

Rejected: gradual typing with an `any` type that flows silently into typed code. It would make
schema derivation partial and taint unsound (an `any` could carry untrusted text into a system
prompt). Rejected: full Hindley-Milner inference. Signatures are documentation for harness
authors and for the registry; inferring them would make errors appear far from their cause.

### Types are schemas

Every type built only from scalars, text, options, lists, tuples, records, maps, enums and `Json`
is **schema-able**. Its JSON Schema (draft 2020-12) is derived mechanically and is what
structured generation constrains the model to (chapter 05).

| Ratio type | JSON Schema |
| --- | --- |
| `Bool`, `Int`, `Float`, `Str` | `boolean`, `integer`, `number`, `string` |
| `Duration` | `string`, pattern `^[0-9]+(ms\|s\|m\|h)$` |
| `Money` | `string`, pattern `^\$[0-9]+(\.[0-9]{1,6})?$` |
| `T?` (field) | the field is not `required` |
| `T?` (elsewhere) | `anyOf: [T, null]` |
| `[T]` | `array`, `items: T`; `@len(min, max)` on the field adds `minItems`/`maxItems` |
| `(A, B)` | `array`, `prefixItems: [A, B]`, `items: false` |
| `{ a: A }` | `object`, `properties`, `required`, `additionalProperties: false` |
| `Map<Str, V>` | `object`, `additionalProperties: V` |
| unit-only `enum` | `string`, `enum` of the variant names in `snake_case` |
| other `enum` | `oneOf` of objects with a `kind` constant and the payload's fields |
| `Json` | `{}` |
| `Untrusted<T>` | the schema of `T` |

Field descriptions come from the trailing string or `///` comment. Attributes on fields refine
the schema: `@len(min, max)`, `@range(min, max)`, `@pattern(r"…")`, `@example(value)`. The
derived schema is part of the IR hash.

## Taint: `Untrusted<T>`

### Decision R6

Imperium's rule is that tool output, web content, file contents and other agents' answers are
**data, never instructions** (`docs/05-tools-and-isolation.md#built-in-tools`,
`docs/21-knowledge-agents.md#safety`). Today that rule is upheld by framing text in tags and
telling the model. Ratio makes it a type rule as well, so a harness author cannot accidentally put
a web page into a system prompt.

**What is untrusted.** Every built-in that returns text produced outside the harness's own code
returns an untrusted type: model output, tool results, file contents, web pages, MCP results,
memory entries, other agents' reports and answers, subagent briefs, command output, environment
listings. The user's own message in a turn (`Turn.message`, type `UserText`) is trusted: it is
the instruction channel. An unattended session's starting prompt is trusted the same way, because
it was written by whoever configured the session.

**`Untrusted<T>` is deep.** It is defined by structure:

| `T` | `Untrusted<T>` is |
| --- | --- |
| `Str` | the primitive untrusted text type |
| `Bool`, `Int`, `Float`, `Duration`, `Money`, `()` | `T` itself |
| unit-only enum `E` | `E` itself |
| `[A]`, `A?`, `(A, B)`, `Map<K, V>` | `[Untrusted<A>]`, `Untrusted<A>?`, … (map keys follow the same rule) |
| `{ a: A, b: B }` | `{ a: Untrusted<A>, b: Untrusted<B> }` |
| other enum | the same variants with untrusted payloads |
| `Json` | `Json`, which is already untrusted by construction (see below) |
| `Untrusted<A>` | `Untrusted<A>` |

So `Untrusted<Verdict>` where `Verdict = { pass: Bool, reasons: [Str] }` lets the program branch
on `v.pass` directly, while `v.reasons` stays untrusted text.

**Scalars are trusted.** A number, a boolean or a choice from a closed set cannot carry an
instruction to a model. A model can still influence which branch the program takes, but every
branch is code the harness author wrote, with the authority that code already had (chapter 07).

**`Json` counts as untrusted.** `json.decode<T>` on any `Json` returns `Untrusted<T>`; a `Json`
built by the program from trusted values with `json.encode` and decoded again is still untrusted.
This is conservative and costs nothing in practice, since trusted values do not need a round trip.

**Propagation.** Operations on untrusted text give untrusted text: concatenation, interpolation
(`"Found: {page}"` is `Untrusted<Str>`), `text.*`, `re.*` captures, `json.stringify` of a value
with untrusted parts, template rendering. Operations that return scalars return trusted scalars
(`text.len`, `re.is_match`, `text.contains`). Comparison of untrusted text gives a trusted `Bool`.

**Sinks.** Untrusted text may flow anywhere except **instruction positions**:

- the `system` and `instruct` sections of a prompt (chapter 05);
- the `system` field of a harness or call configuration;
- the `summary` of `session.request_approval` and the text of `context.notice` (the user and the
  model read these as coming from the harness itself);
- harness and package names given to `meta.harness` and `agents.spawn(harness: …)`;
- the `reason` of `trust` itself.

Everywhere else it is accepted: tool arguments (authority comes from permissions, not from the
text), `task` and `data` prompt sections (where it is framed), replies to the user, logs, memory
writes (which the user approves by default), `state` fields, and harness arguments whose
parameter type is untrusted.

**Conversion.** `trust(x: Untrusted<T>, because: Str) -> T` is the only conversion. It is an
effect: it emits a `ratio_trust` event into the journal with the harness, the call site, the
reason, and a digest and the first 200 characters of the value. `harnesses.check` lists every
`trust` site of a package so a reviewer sees them all. There is no implicit conversion, no
"sanitise" function that returns trusted text, and no way to trust without leaving a record.

Rejected: dynamic taint tracking at run time only. It finds the mistake after a turn has spent
tokens, and cannot be listed for review before a package is enabled. Rejected: no taint, framing
only. Framing is still applied (chapter 05), but a type error at load time is cheaper than a
successful injection.

## Evaluation

Evaluation is strict and left to right: arguments before the call, fields in written order,
operands of a binary operator left before right. `and`, `or` and `??` short-circuit.

**Bindings.** `let` binds immutably, with a pattern that must be irrefutable (or the checker asks
for `if let` or `match`). `var` binds a mutable local; assignment is a statement. A closure
captures variables **by value at the moment it is created**, and cannot assign to a captured
`var` (a checker error). This is what makes concurrent branches race-free.

**Functions.** `fn` declarations are pure functions over values unless they call effectful
built-ins or harnesses; there is no effect annotation. Recursion is allowed and bounded by the
step and depth limits (chapter 07).

**Options.** `x?.f` is `none` if `x` is `none`; `x ?? d` supplies a default; `if let v = x { … }`
unwraps. Reading an absent optional record field gives `none`.

## Control flow

- `if` without `else` has type `()`. With `else`, both branches must have a common supertype.
- `match` must be exhaustive; the checker names the missing variants or literal cases. Guards
  (`if …`) do not count towards exhaustiveness.
- `for p in xs { … }` iterates lists, ranges and maps (as `(key, value)` tuples, in key order); its
  value is `()`. `while cond { … }` and `loop { … }` run until `break`; `break v` gives `loop` its
  value. `repeat n { … }` runs the block `n` times.
- Every loop iteration costs steps (chapter 07), so an unbounded loop ends with a `Budget` error
  rather than hanging the session.
- `return` leaves the enclosing function or harness; inside a closure, it leaves the closure.

## Errors

### Decision R7: exceptions with a closed set of kinds

Effects fail often and for reasons a harness must tell apart (the user declined; the test
failed; the model returned garbage; the budget ran out). Ratio uses exceptions so that the common
path stays short, with a **closed** set of kinds so that `catch` arms are exhaustive and every
failure has a known shape.

```ratio
type Error = {
  kind: ErrorKind,
  message: Untrusted<Str>,     // may quote tool or model output
  data: Json,                  // kind-specific details, documented per kind
  site: Site,                  // file, line, column, frame path
}
```

| `ErrorKind` | Raised when | Catchable |
| --- | --- | --- |
| `Denied { by }` | A call was refused: `by` is `rule`, `mode`, `floor`, `user`, `hook`, `profile`, `attenuation` or `unattended` | Yes |
| `ToolFailed` | A tool ran and reported failure (`data`: tool, exit code, output excerpt) | Yes |
| `ModelFailed` | An endpoint error, an empty reply, a refusal, or a reply that could not be parsed as tool calls | Yes |
| `RateLimited` | A usage limit the session's resume policy chose not to wait out | Yes |
| `SchemaMismatch` | Structured output or `json.decode` did not match the schema after repairs (`data`: violations) | Yes |
| `Budget { which }` | `tokens`, `cost`, `steps`, `wall`, `model_calls`, `tool_calls`, `depth`, `heap`, `width` ran out | Only outside the scope whose budget ran out |
| `Timeout` | A `with timeout` or a `timeout:` argument elapsed | Yes |
| `Cancelled` | The session was stopped for good, a `race` sibling won, or a `par` sibling failed | No (observed by `defer`) |
| `Assertion` | `assert` failed | Yes |
| `NotFound` | A named thing does not exist (file, memory, agent, harness, model) | Yes |
| `Invalid` | A bad argument, an out-of-range index, an integer overflow, a malformed regex | Yes |
| `Unsupported` | The backend cannot do what was asked (logprobs on an endpoint without them) | Yes |
| `Replay` | A replay run diverged from its tape | No |
| `Question { question, options }` | A callee needs an answer it cannot get (chapter 04, chapter 08) | Yes |
| `User { tag }` | `raise fail("tag", message)` | Yes |

```ratio
let report = try {
  tools.run_command("cargo test -q")
} catch {
  e if e.kind is ToolFailed => e.data |> json.decode<CommandResult>,
  e if e.kind is Denied => raise fail("no-tests", "running tests was refused"),
}
```

`try { … } catch e { … }` binds the error for a single arm. Uncaught kinds in an arms form
propagate. `result(|| expr)` evaluates `expr` and returns `Result<T>` (`Ok { value }` or
`Err { error }`), which is how fan-out collects per-branch outcomes without stopping at the first
failure.

**Budgets cannot be swallowed where they run out.** A `Budget` error raised because the budget of
a scope ran out (a harness call's, or a `with { budget: … }` block's) cannot be caught inside that
scope, only by the code that opened it. Catching it inside would let the code keep spending what
it does not have.

**Cancellation cannot be swallowed.** `Cancelled` passes through `catch`. `defer { … }` blocks run
on every exit from their scope (normal, error, cancellation) in reverse order; a `defer` block may
make effects that release resources (`command_stop`, `browser_close`), and those effects are
exempt from cancellation but bounded by a 10-second grace period.

## Concurrency

### Decision R8: structured concurrency only

Concurrency serves fan-out: several critics, several samples, several searches, several
independent tool calls. Ratio offers it only in forms whose lifetimes nest in the code:

| Form | Value | Semantics |
| --- | --- | --- |
| `par { a; b; c }` | `(A, B, C)` | Runs the expressions concurrently; returns their values in written order. If one raises, the others are cancelled and the error propagates |
| `par_map(xs, f, limit: n)` | `[B]` | Applies `f` to each element with at most `n` running at once (default 4); results by index; fail-fast like `par` |
| `par_map_settled(xs, f, limit: n)` | `[Result<B>]` | As `par_map` but never fails fast; every element gets a result |
| `race { a; b }` | common supertype | Returns the first to finish successfully; cancels the rest; if all fail, raises the last failure |
| `with timeout d { … } else { … }` | block's type | Cancels the block after `d`; runs `else` instead, or raises `Timeout` without one |
| `agents.spawn(…)` | `Task<T>` | An engine subagent; the only thing that can outlive its creating scope (chapter 06, `agents`) |

**Implementation model.** All branches of a frame tree run on the agent's own task as futures; the
interpreter has no threads. Pure evaluation is cooperative; effects are where branches interleave.
Because closures capture by value and values are immutable, no branch can observe another's
progress except through the host (and through engine state such as the conversation, which is
owned by exactly one frame).

**Determinism.** Results are combined in program order. Two outcomes depend on timing: which
`race` branch finished first, and which `par` branch failed first. Both are recorded on the tape
as scheduling choices, so replay picks the same winner and the same failure.

**Limits.** At most `limits.width` branches run at once per frame tree (default 16); excess
branches wait. Every branch draws on the same budgets as its frame; a `par` of three critics
spends three critics' tokens.

**Cancellation** reaches a branch at its next effect boundary or at its next 1,000 steps,
whichever is first. A model request in flight is aborted; a tool call in flight is allowed to
finish (as the engine does on stop) and its result discarded; a background process it started is
stopped unless it was handed out of the scope.

Rejected: unstructured `spawn`/`await` of local tasks. Lifetimes escape the code, cancellation
and budgets become guesswork, and a forgotten task keeps spending after the turn ends.

## Modules and imports

- A **package** is its entry file and the files it imports. A file is a module; its `pub` items
  are visible to importers, everything else is private.
- `import "./lib/critics.ratio" as critics` imports a file by path, relative to the importing
  file. The path must stay inside the package's **source root** (the folder its entry file was
  found in); `..` that leaves it, absolute paths, and symbolic links are refused. This is the
  rule Imperium applies to agent files (`docs/06-extensibility.md#named-agents-implemented`).
- `import debate` or `import debate.judges` imports a registered package by name (chapter 08 lists
  the sources and their precedence). `import debate@^1.2` pins a semver range; without one the
  highest version found is used and recorded.
- The standard library modules (`model`, `think`, `tools`, `agents`, `memory`, `context`,
  `session`, `data` and its submodules, `control`, `eval`, `meta`) are in scope in every file
  without an import. The `data` submodules (`text`, `list`, `map`, `json`, …) are also in scope by
  their short names, and their functions are callable as methods (`xs.map(f)` is
  `list.map(xs, f)`). Method syntax `x.f(a)` resolves to the module of `x`'s type first, then to a
  function `f` in scope whose first parameter accepts `x`.
- Import cycles are errors. Name resolution is lexical: a local binding shadows an import, which
  shadows the prelude; the checker warns on shadowing a standard module.
- **Trust follows source.** A package from a plugin or a project may import only files in its own
  source root, the standard library, and packages from the same source or from built-in packages.
  A user's own package may import anything registered. So a project cannot pull in a user's
  private harness, and a plugin cannot reach into a project.

## Persistent state

A package may declare one `state`:

```ratio
state: { rejections: Map<Str, Int>, plan: Plan? } = { rejections: {}, plan: none }
```

- The type must be schema-able and contain no handles. The initialiser is a constant expression.
- `state.get()` returns the current value; `state.set(v)` and `state.update(|s| …)` replace it.
  Changes are visible at once in the turn and are written when the turn ends (or the subagent
  task's turn ends): one `ratio_state` event with the new JSON, if it changed.
- State belongs to **one agent**: the session root's state for a `@turn` entry; each subagent's own
  state for a `@task` entry. Harness calls within a turn see their package's state of that agent;
  a package imported by another sees its own state, separately.
- At most 64 KiB of JSON. A larger value raises `Invalid` at `state.set`.
- On resume after a restart, the newest `ratio_state` event is read back. A branch
  (`session.branch`) or a rewind of the conversation keeps the state as of its cut point, because
  the events up to that point are what the branch copies.
- Changing a package's `state` type between versions: the stored JSON is decoded into the new
  type; when that fails the state is reset to the initialiser and a notice says so.

## Tests

`test "name" { … }` blocks are compiled only for `ratio test` and `harnesses.check` with
`test: true`. They run against a scripted host (chapter 10) in which `model.*`, tools and agents
answer from fixtures declared with `eval.fixture` and `eval.script_model`. Tests never run in a
session.
