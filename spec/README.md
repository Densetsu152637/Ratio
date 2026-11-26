# The Ratio specification

Version: draft 0.16 (2026-11-30). Status: design; stages 1 to 8 and 10 of the implementation
(the token reader, the parser, modules and names, the type checker, schemas and taint, the IR
and core interpreter, the `text` and `re` built-ins with the offload pool, the `json`, `list`
and `map` built-ins, and frames) are built in this repository.

**Changes in draft 0.16.** Errata from stage 8, Data II. No grammar change.

- `json.validate` and `json.patch` are `product`, not `linear`: validation visits pairs of
  instance and schema nodes, and each patch operation may copy a path through the document
  (chapter 06, `data`).
- `list.sort`, `list.unique` and `map.from` compare whole elements, so their `nlogn` work is the
  elements' weight times ⌈log2(n + 1)⌉; the keyed ones add their keys' weight (chapter 06).
- `JsonType` and `Violation` are defined among the shared types (chapter 06).
- `list.insert` takes an index in `0..=len` and `list.remove_at` one in `0..len`, a negative
  index being out of range; `list.window` needs a size of at least 1 (chapter 06).
- `json.stringify` writes a record's keys in field-name order and returns `Untrusted<Str>`;
  `list.unique`, `group_by` and `map.from` take keys equal by natural order (chapter 06).
- Chapter 10 says a loop of N pushes or sets is charged *at most* O(N log N) steps.
- A schema `pattern` is read as ECMAScript reads it within the `@pattern` subset, and the forms
  the two syntaxes read differently are refused (chapter 03; chapter 06, `json.validate`).
- Walks over a value fit a small stack in release builds; the JSON walks of `data` in every
  build (chapter 07).

**Changes in draft 0.15** (kept for reference). Errata from stage 10, frames. No grammar change.

- A harness's parameter defaults and declared configuration run before its frame opens, so both
  are effect-free, with no calls of function values; every bound parameter, configuration value
  and setting (inherited ones included) is checked against the new frame's `sees` like an
  argument (chapter 04).
- A `with` block whose `sees` lacks `secrets` refuses reads of labelled values from its
  enclosing scope, by name, by capture and by assignment (chapter 04).
- `paths` are absolute folders under `/`, a drive root (`C:/`; a bare `C:` is refused) or a UNC
  share, roots never containing each other; tool patterns are prefixes ending in one `*`
  (chapter 04).
- Configuration lists hold at most 256 entries, and opening a frame is charged a step per
  comparison (chapter 07).
- An admission below the reply floor, or with its input uncovered, waits only while
  reservations are held on the ledger whose cap binds; an overshoot is debited in full
  (chapter 04).
- Overrides merge `budget`, `limits`, `sees` and `permissions` sub-field by sub-field; an
  attended request under an unattended caller, `context: main` outside an entry and `cloud`
  under `local` raise `Denied { by: attenuation }`; a frame's `limits.heap` is attenuated, not
  separately enforced (chapter 04).
- A `with` block is a frame-path element named `with` (chapter 01); of equal caps the outermost
  scope's runs out (chapter 03).
- `FrameInfo` and `CallerInfo` name the harness `name`, since `harness` is a keyword (chapter 06,
  `meta`).

**Changes in draft 0.14** (kept for reference). Errata from stage 7, Data I. No grammar change.

- The `re` built-ins are `product`: the text's characters times the compiled pattern's size
  (times one plus the capture slots tracked), plus the compile work, charged on a cache hit and
  for a refused pattern too; an iteration's work past a fixed multiple of the charge is debited
  as it is done, and the output as it is built (chapter 06, `data`).
- Each regex search is linear in its input; iterating over matches can be quadratic, as in
  Rust's `regex`, and is bounded by the step charge (chapters 06, 07).
- `Match<S>`, `Captures<S>` and `CodeBlock<S>` take the call's text type, so parts of untrusted
  text stay untrusted; `re` spans count characters (chapter 06).
- `text.extract_json` tries at most the first 32 `{` or `[` (chapter 06).
- `text.find` treats a negative `from` as 0, `text.split` refuses a `limit` below 1, and an
  empty `fill` leaves `pad_start` and `pad_end` text as it is (chapter 06).
- `re.replace`'s `with` is a keyword, so it is passed by position only (chapter 06).
- `text.pad_start` and `text.pad_end` are charged for their fill as input (chapter 06).
- A pure built-in is offloaded by its charge before running as well as by its input size, and a
  built-in that debits as it goes (an iteration over matches) whenever the steps left exceed the
  threshold (chapter 01).
- A pattern's compile work is its counted compile steps (one per node compiled, every repeated
  copy included), and parts that match nothing are dropped before compiling; the `product` row
  points to the `re` charges, where later work and output are debited as they go (chapter 06).

**Changes in draft 0.13** (kept for reference). Errata from stage 6, the IR and the core interpreter. No grammar
change.

- A step is defined: one IR expression, statement or pattern node entered, or one loop
  iteration, plus the charges of built-ins, text building and walking values by their weight
  (chapter 07); an `eval` conformance file runs `fn main()` with no arguments, and its
  expectations are stated (chapter 10).
- Division by zero is `Invalid` for `Float` too (chapter 02).
- Chapter 07's limits add value nesting (256, `Invalid`), say the heap counts live values with
  prompt sections and the values the interpreter gathers, and that rendering stops at the string
  limit.
- The interpreter enforces the branch restriction on `var`s, which the checker does not yet; a
  violation is fatal, and no `catch`, `result` or `race` branch can swallow it (chapter 03).
- Map built-ins and operators are charged by the key's size, `list.sort_by` by its keys'
  weight, `include` by what it copies, and comparisons by weight (chapter 06, `data`).
- `IR_VERSION` 1 is fixed at the first release; changes before it do not bump it (chapter 10).
- Wording: `C0037` covers a generic function returned too (chapter 03); the call-only criterion
  names every source or sink whose rule depends on the written arguments (chapter 06).

**Changes in draft 0.12** (kept for reference). Errata from stage 5, schemas and taint. No grammar change.

- Concatenating or interpolating a `Brief` with trusted text only gives a `Brief` (chapter 03).
- `steps` sections are instruction positions, with `system`, `instruct`, `note` and `example`
  (chapter 03).
- `trust`'s `T`, unless a turbofish gives it, is the argument's type with its trust labels
  removed (chapter 03).
- `trust`, `as_task`, `declassify`, `session.ask` and `meta.harness` may only be called, never
  used as function values: every rule a built-in imposes must hold for values too (chapters 03,
  06).
- The guarded-effect lint is silenced with `@allow(model_guard)` on the enclosing item, since
  statements take no attributes (chapter 03).
- The schema table gives `()` (`null`), map keys other than text (`propertyNames`), the `kind`
  constant's spelling (`snake_case`), that an optional field does not take `null`, and `$defs`
  for an enum that contains itself; schema attributes are checked against their fields,
  `@pattern` is the regular-expression syntax ECMAScript and `re` share, `@range` keeps its
  decimal literals, and the schema is canonical JSON (chapter 03).
- Type digests key the structure of values: their canonical form is given, keeps the labels and
  leaves out descriptions and refinements (chapter 03).
- `agents.spawn`, `agents.delegate` and `agents.map` name the package by `package:` (a keyword
  cannot name an argument, so `harness:` could not be written) (chapters 03, 06, 08).
- `session.ask`'s questions are the shared `Question` record, whose `secret?: Bool` is `false`
  when absent (chapter 06).
- Chapter 10 adds the codes `C0035` to `C0039` and the `conformance/taint/` directory.
- `session.start` names the package by `package:` too, and a parameter whose name is a reserved
  keyword (`prompt`, `type`) is passed only by position (chapter 06).
- The guarded-effect lint (`C0039`) also covers running a tool a model chose (chapter 03).
- The checker refuses instantiating a generic function that may reach a secrecy sink at a type
  holding a `Secret` (`C0037`, chapter 03).
- The call-only rule states its criterion (trust sites, sinks whose rule depends on the call's
  written arguments, generic secrecy sinks); the five built-ins are its current members
  (chapter 06).
- Canonical JSON's key order is given (fixed order for schema keywords, field names by UTF-8
  bytes), and digest enums are numbered from 0 (chapter 03).
- A `meta.harness` name's source is chosen first, then the longest prefix that names a
  registered package, then the harness; an ambiguous name makes the `harnesses.check` listing
  incomplete (chapters 04, 06).
- Wording: chapter 10's `C0036` row.

**Changes in draft 0.11** (kept for reference). Errata from stage 4, the type checker. One grammar change.

- Enums a program declares take no type parameters: `enum_decl` loses `[ type_params ]`, and
  only built-in enums (`Result<T>`, `Fold<A>`) may be generic (chapters 02, 03). An enum's type arguments relate by
  how its payloads use each parameter (chapter 03).
- A `prompt` declaration's type, `TrustedPrompt` or `Prompt`, is inferred; it has no written
  result type (chapter 05).
- The shared record `Task` is renamed `TaskInput`, so it no longer shares its name with the
  handle `Task<T>` (chapters 01, 03, 04, 06, 07, 08, 09).
- `ContextMode`'s variant `none` is read where a `ContextMode` is expected; where a
  `ContextMode?` is, `none` is the absent value (chapters 02, 04).
- A required record field may stand where it is declared optional, and spread against a declared
  record type takes only its fields (chapter 03).
- A pattern other than a binding, `_` or `none` matched against `T?` matches a present value
  (chapter 03).
- Chapter 10 adds the `C` codes (`C0001` to `C0034`) and the `check-ok`, `check-error` and
  `check-warning` expectations of `conformance/types/`.

**Changes in draft 0.10** (kept for reference). Errata from stage 3, modules and names. No grammar change.

- Bare names resolve to `std`, then `user` only; plugin, project and authored packages need their
  qualified name (chapters 03, 08).
- Trust follows source: the rule is stated as the list it always was, without "at least as
  trusted" (chapter 03).
- Import bindings, `pub` on imports, path characters, case conflicts, and package and plugin
  names (ASCII identifiers that are not keywords, at most 128 bytes, no device names) are
  specified (chapter 03).
- Version ranges are `^`, `~` or `=` with one to three numbers, checked against `meta.version`
  (chapter 03).
- Package names are importable identifiers, so the built-in packages are renamed with
  underscores: `std:consul_escalation`, `std:plan_execute_verify`, `std:self_consistency`,
  `std:tdd_fix` (chapters 08, 09, 10).
- Inside an authored package's import, `authored:<name>` means the importer's own area only
  (chapter 07).
- A warning (`N0021`) for shadowing `trust`, `as_task` or `declassify` (chapter 03).
- Chapter 07 adds the package-loading limits (1024 files, 64 packages, 4096 registry calls per
  load, 64 modules per import chain, 128-byte names); chapter 10 adds the `N` codes and the
  `conformance/names/` worlds.

**Changes in draft 0.9** (kept for reference). The accepted proposals of the performance, parallelism and concurrency
review (`docs/design/performance-concurrency-review.md`, proposals PC-1 to PC-9; PC-10 to PC-15
deferred). No grammar change.

- Effect keys gain a **branch path**, so keys no longer depend on completion order (chapter 01).
- One **turn sequence** numbers every scheduling event (ledger, cancellation, cache and ordering
  events); replay verifies it against the tape alone (chapter 01).
- Budgets admit effects by **bounded reservation** with an effective `max_tokens` and a reply
  floor (`ratio.limits.min_reply_tokens`, default 256); a hosted model (any location other than
  `local`) without an explicitly configured price is refused under a cost cap, while a `local`
  model's price of 0 counts as priced; budget decisions and readings are sequenced and
  verified in replay (chapters 04, 07).
- Every cancelled branch's cancellation point is recorded, whatever cancelled it (chapter 03).
- Width counts **running** branches only; parked branches release their slot, so nested fan-out
  cannot deadlock; waiting branches start in branch-path order (chapters 03, 04, 06 `control`).
- Tools have **cancellation classes** (`abort`, `drain`); pending approvals are withdrawn
  atomically; outstanding drains are re-parented and held by a **drain fence**; every host call
  carries a deadline; every `drain` call has a finite bound of at most 3600 s (chapters 03, 06
  `tools`, 07 T17, 10).
- New built-in `par_fold` and the types `Fold`, `FoldOrder`, `Folded` (chapter 06 `control` and
  README; `control` 30 built-ins, 479 in all).
- `cached`, `memo` and `once` are **single-flight**, with recorded and verified waits;
  session caches keep labels, a labelled hit is refused without `sees.secrets`, and headless
  tapes record labelled cache keys only by redaction digest (chapters 01, 03, 04, 06 `control`).
- Pure data built-ins are charged by **cost class** (including `product` for `text.similarity`,
  `diff.*` on lines or words, and `json.diff`), debited before running from the input sizes, with
  required representations and label summaries (chapters 01, 06 `data`, 07).
- The interpreter yields every 1,000 steps, offloads large built-ins to a bounded pool (heap
  accounted before dispatch), uses
  `Send` values and a prepared form that is never hashed (chapters 01, 07).
- Tapes: entries for input-bearing effects only, suppressed output effects and an end-of-turn
  reply digest, a `full` recording mode, content-addressed blobs, chained conversation digests,
  zstd files, and replayed labels that can only be added (chapters 01, 03, 08).
- Per-branch random generators: `rand` draws need no tape entries (chapter 06 `data`).
- `model.stream` gains `until` and returns `Streamed`; `think.self_consistency` and
  `think.best_of` gain `early_stop` (`best_of` refuses it without `good_enough`) (chapters 05,
  06 `model`, `think`, README).
- Chapter 08 states Imperium's host requirements: the class table, a conservative read-only
  command allowlist (stage 17; until then no command is read-only), read-only MCP tools from user
  configuration only, drain clamps, atomic withdrawal, and refusal of unpriced hosted models
  (from stage 13).
- Chapter 10: the `Host` sketch gains `EffectCtx`, `tool_class` and `conversation_digest`;
  `TAPE_VERSION` notes; ownership rows; stage 11 splits into **11a** (trait, tape, keys,
  scheduler) and **11b** (`control`, `par_fold`, single-flight caches, `rand`, `time`, `state`),
  and stages 6 to 14, 17 and 19 carry the new work and tests; the `steps` expectation.
- Chapter 07 adds limit rows (reply floor, pool jobs, drain bound) and threat T17, and widens
  T8, T10 and T13; chapter 11 lists the deferred proposals.

**Changes in draft 0.8** (kept for reference). Errata from the stage 2 parser. Chapter 02: a `>` that closes a generic
argument list ends a statement at a line end inside a type declaration or a prompt `format`
section (insertion condition 2); a block head's `{` must be on the head's line (`P0008`); the
parser takes the token reader's brace kind as disambiguation rule 1's decision (`P0003` when it
does not fit); `record_pattern` becomes `"{" [ ( field_pattern {"," field_pattern} ["," ".."] |
".." ) [","] ] "}"`; the grammar is applied strictly (trailing commas only where a production
allows one, jumps are not operands, `()` is not an expression); parser nesting counts tree depth,
one level per nested expression, type or pattern, per prefix operator and per link of a chain,
limited by `max_nesting` (`P0002`). Chapter 10 adds the parse diagnostic series `P0001` to
`P0010` (unexpected token, nesting too deep, wrong brace, not a place, chained comparison or
range, misplaced modifier, too many errors, head split across lines, positional after named,
internal error),
the expectation kinds `parse-ok`, `parse-error CODE at L:C` and `tree:`, says that
`RatioHost::open_frame` looks up the parent by frame path, and says that only the release
workflow creates a release, given the tag ruleset. Also in 0.8: in a type declaration or a
`format` section a line-ending `??` that ends a type ends the statement, like `>`; chapter 06's
`type Sampling` example uses a `//` comment (there are no block comments) and `ToolChoice`'s
`none` variant is renamed `no_tools` (`none` is a keyword; chapter 05 follows); chapter 07's
nesting row says the limit counts tree depth, chain links included. Chapter 02 also records
the token reader's reach: after a `:`, `fn()` is a function expression when a `{` follows it or
its result type, and a closure's parameter commas do not end a `match` arm; and the canonical
form (a new subsection): the printer's explicit `;` after a statement ending in `>`, `??` or a
`fn` or `harness` type, no trailing comma in an `import` list, `{ x, }` for a one-field
shorthand record, canonical quantity, duration and money units, ranges without spaces, long and
raw long string layout. A `defer` block and a prompt `if` or `for` section each count one level of nesting (chapter 02,
chapter 07). Chapter 10 says what a `tree:` line is. The round trip is stated for printed text within the
source limit, and `print`, the tree dump and the parser run on a large stack, clamping depth to
12 if no thread can be started (chapter 02, "Canonical form").

**Changes in draft 0.7** (kept for reference). Errata from the stage 1 review and the repository-split review.
Chapter 02: blocks and prompt bodies separate statements with `;` and allow one after the last;
insertion's condition 1 includes the top level of the file, condition 3's `.` includes `..` and
`..=`, and a line holding only attributes never ends a statement; disambiguation rule 1 is
ordered (head keywords, `with` and a record, variant literals after a type name such as
`Denied { by }`, then records) and says the token reader approximates the parser with it; the
number rules are written down (leading zeros, the `_` separator rule, durations as whole numbers
of their unit in microseconds, so `1.5s` is a lexical error, and why `-9223372036854775808` is
written `-9223372036854775807 - 1`); the long-string dedent algorithm is defined, the first
`"""` ends a long string, a bare carriage return is an error everywhere and `\r\n` reads as `\n`
in long strings; operators use maximal munch (`T??` is the token `??`, meaning `T?`; `> =` needs
its space). Chapter 03 pins the `Duration` unit for literals. Chapter 07 adds T4b, an injected
install. Chapter 10 lists the token reader's expectation lines and the `L0001` to `L0016`
codes, adds attribute lines to the stage 1 corpus, makes `RatioHost::open_frame` check against
Imperium's own record of the parent frame (never the `FrameCtx` passed in), adds a per-session
notice when a pinned `user:` or `plugin:` package was selected with another `ratio-lang`
version, and requires a GitHub release, not only a tag, in Imperium's source check.

**Changes in draft 0.6** (kept for reference). The repository split: Ratio is built as a language in this repository,
with its own Cargo workspace, CI and releases, and Imperium becomes a host that depends on a
release tag (R15 revised). Chapter 10 assigns each of the 28 stages to a repository (stages 1 to
12 here, the engine host, daemon, gates, clients and headless evaluation in Imperium, four stages
in both), defines the boundary (the `Host` trait as the public API of the `ratio` library,
semantic versioning with `HOST_API_VERSION`, `IR_VERSION` and `TAPE_VERSION`, pinning by tag,
releases, where cross-repository tests live) and says which side owns each security requirement:
the engine independently verifies every frame's attenuation at `open_frame`, label propagation is
named as Ratio's residual risk, and every `tools.install_extension` call is an agent-origin
install that asks per install in every mode (chapter 06); chapter 08 gains the same split
for its subjects. The specification is no longer pinned by revision; the conformance corpus names
the chapters it tests instead. No language change.

**Changes in draft 0.5** (kept for reference). Design review round 4: declassifying a conversation and subscribing
with `secrets: true` are desktop-only, through a single-use challenge and native dialog (no CLI
path); tool-call arguments on labelled conversations and engine state written from labelled
values (plans, aids' state, titles including auto-titles, authored source) carry the label, which
is journalled, carried into copies and kept context, and redacted; stubs keep labels, and the
invariant that replies after a secret are labelled is stated and tested; shadowing with `let` is
defined.

**Changes in draft 0.4** (kept for reference). Design review round 3: `run_harness` forces every argument untrusted
for every source and refuses harnesses with trusted text in their parameters (`std:self-consistency`
now takes its question untrusted); with secrecy enabled, commands whose line references a secret
path ask every time and are refused when unattended (best-effort, floor-style matching); labelled
conversations get a user notice and three recovery paths, and labels follow forks, excerpts and
snapshots; daemon-side redaction is staged explicitly (stage 18) and impossible to lift in
unattended sessions; `if let` over options is defined.

**Changes in draft 0.3** (kept for reference). Design review round 2 (N1 to N10 and nits): secrecy is a run-time
label on values, enforced at the host and at dispatch, opt-in and off by default, with more sinks,
labels on `Json` and on conversations; authored harnesses are pinned and approved by the hash of
their whole import closure, force taint on arguments and results, take approvals scoped to the
authority shown, use area-qualified names, and run contained on the caller's model with
provider-side tools off; every event leaving the daemon is redacted; example 8's path handling is
fixed; the security-relevant clients come before the security review, and engine stages are
split (28 stages); grammar nits (block heads, `/` in source names, constants).

Ratio is a language for writing **agent harnesses** for Imperium. A harness decides how an agent
works: what its model is told, which tools it may use, how results are checked, when it stops.
Imperium today has one harness, its Rust agent loop. Roadmap item 58 makes harnesses selectable
per session, environment, agent file and subagent; Ratio is the language those harnesses are
written in.

**Changes in draft 0.2** (kept for reference). The design-gate review's findings (security S1 to S13, grammar and
types G1 to G7, integration I1 to I3, missing built-ins, nits) and the user's answers to the
twelve open questions are folded in: the effect rule for untrusted strings, `Brief`,
`TrustedPrompt`, abstract generics, monomorphic and gated `trust`, `Secret<T>`, frame `sees`,
turn payloads, source-qualified names, selection pinned by hash, plugin and floor gates, authored
(model-written) harnesses, the live harness view, contextual keywords, the turbofish, a full
string-interpolation grammar, recorded scheduling choices, a concurrent `&self` host, honest
engine seams and resized stages.

## Reading order

| Chapter | Contents |
| --- | --- |
| [01 Purpose and execution model](01-purpose-and-model.md) | What a program is, how the daemon runs it, the IR, determinism and replay, how an agent turn maps to execution, entry obligations |
| [02 Syntax](02-syntax.md) | Lexical structure, string interpolation, semicolon insertion and the complete EBNF grammar |
| [03 Semantics](03-semantics.md) | Values, types, taint (`Untrusted<T>`, `Brief`), secrecy (`Secret<T>`), errors, control flow, concurrency, modules, persistent state |
| [04 Harnesses as functions](04-harnesses.md) | Declaring and calling harnesses, frames, `sees`, context isolation, overrides, attenuation, depth, questions |
| [05 Shaping model thinking](05-thinking.md) | Prompts and `TrustedPrompt`, thinking steps, scratchpads, effort, sampling, tool choice, structured generation, stopping |
| [06 Standard library](06-stdlib/README.md) | Every built-in, by module, with signature, semantics, failures and permission |
| [07 Safety and security](07-security.md) | Sources and trust, no `eval`, authored harnesses, permissions, limits, prompt injection, threat model |
| [08 Integration with Imperium](08-imperium.md) | Sources, selection and pinning (item 58), the default harness and parity, Consul (item 64), hooks, the Agents panel and live harness view, code mode, headless evals, protocol, which repository owns what |
| [09 Examples](09-examples.md) | Nine complete programs |
| [10 Implementation plan](10-implementation.md) | The crates of this repository, the 28 stages split between Ratio and Imperium, the `Host` boundary and its versions, engine seams, the conformance suite, testing |
| [11 Decisions and open questions](11-open-questions.md) | The user's decisions on Q1 to Q12; Q9 still open |

## The repository

This specification is the `spec/` folder of the Ratio repository, next to the implementation:

| Path | Contents |
| --- | --- |
| `spec/` | This specification |
| `crates/ratio/` | The `ratio` library (package `ratio-lang`), the public facade hosts depend on |
| `crates/ratio-syntax/` | The token reader (stage 1); the parser from stage 2 |
| `std/` | The built-in packages' sources, embedded by the library (from stage 23) |
| `conformance/` | The conformance corpus by chapter, `spec-map.txt` (which chapters each directory tests) and `builtins.txt` |
| `scripts/` | The checks CI runs |

The repository's [README](../README.md) explains how to build and test it; chapter 10 explains how
the work is split with Imperium.

## Conventions

- **Must**, **must not** and **may** are normative. Everything else is explanation.
- Code is Ratio unless fenced otherwise. Comments start with `//`.
- Imperium references name the document on `origin/experimental` they come from, such as
  `docs/03-agent-engine.md#agent-loop`. Ratio never changes Imperium's behaviour by itself:
  where Ratio needs the engine to change, chapter 10 names the change as a dependency.
- "The engine" is `imperium-engine`; "the daemon" is `imperium-core`; "the host" is the side of
  the interpreter that performs effects (chapter 01).

## Decisions at a glance

Each decision is argued, with the alternatives rejected, in the chapter named.

| # | Decision | Chapter |
| --- | --- | --- |
| R1 | A Ratio program is a **harness package**: a file of typed declarations whose `@turn` entry runs once per user turn and whose `@task` entry runs a subagent's task | 01 |
| R2 | An **interpreter inside the daemon**, in process, over a typed intermediate representation (Ratio Core IR); no bytecode VM, no separate worker process | 01 |
| R3 | **Determinism by construction**: immutable values, no ambient effects, structured concurrency with branch restrictions; every effect and scheduling choice goes through one host interface and onto a tape that replays a run exactly | 01, 03 |
| R4 | **Expression-oriented**, brace-delimited, newline-terminated syntax with contextual keywords and turbofish type arguments | 02 |
| R5 | **Static types with local inference**, structural records and tagged enums, one dynamic escape (`Json`) that must be decoded through a schema; types double as generation schemas | 03 |
| R6 | **Taint typing**: every string an effect returns is `Untrusted` unless it is an engine identifier; it may flow anywhere except instruction positions; `task` sections take only `Brief`; `trust` and `as_task` are monomorphic, audited and gated by `ratio.trust`; guarantees cover explicit flows | 03, 07 |
| R7 | **Exceptions with a closed set of error kinds**, `try … catch` as an expression, `result()` to reify; cancellation and the budget that ran out cannot be swallowed by the scope they stop | 03 |
| R8 | **Structured concurrency only** (`par`, `par_map`, `par_fold`, `race`, `timeout`); no detached tasks except engine subagents, which the engine owns; cancellation by tool class, with a drain fence | 03 |
| R9 | **Harness calls are function calls** with their own frame (model, tools, permissions, budget, `sees`, conversation); a callee sees nothing of its caller or the session unless passed, forked or granted | 04 |
| R10 | **Attenuation only**: every frame's tools, permissions, model locations, `sees`, budgets and subagents are bounded by its caller's | 04, 07 |
| R11 | **Prompts are typed values** built from sections; only a `TrustedPrompt` may be a system prompt; the section decides how text is framed | 05 |
| R12 | **No `eval`** in a running program; every harness a model runs through `run_harness` gets untrusted arguments and must have untrusted parameters; models write harnesses only through the **authored-harness** path (stored, checked, pinned by closure hash, taint forced at the boundary, contained by default, caller authority only for a user-approved closure at the authority shown, never above the caller) | 07 |
| R13 | **Every side effect is an engine call**, decided by Imperium's dispatch (rules, floor, modes, hooks, approvals, events); delegation and session control are tool-gated; Ratio holds no authority of its own | 07, 08 |
| R14 | **`std:default` replaces the native loop** after the parity suite passes and one release cycle (the user's decision); until then the Rust loop is the default | 08 |
| R15 | Built as a **language in its own repository** (this one: specification, crates, conformance suite, CI, releases), independent of the engine behind a concurrent `Host` trait; Imperium implements the trait and depends on a release tag | 10 |
| R16 | **Secrecy is a run-time label** (`Secret<T>` statically only where known), opt-in and off by default; labelled values and the conversations they enter never reach network tools, hosted models, commands, non-secret writes, delegation or stored outputs, enforced at the host and at dispatch, and redacted from every event leaving the daemon | 03, 07 |

## Size of the standard library

| Module | Built-ins | Module | Built-ins |
| --- | --- | --- | --- |
| [`model`](06-stdlib/model.md) | 29 | [`session`](06-stdlib/session.md) | 22 |
| [`think`](06-stdlib/think.md) | 30 | [`state`](06-stdlib/session.md#state-package-state) | 4 |
| [`tools`](06-stdlib/tools.md) | 50 | [`data`](06-stdlib/data.md) (15 submodules) | 213 |
| [`agents`](06-stdlib/agents.md) | 20 | [`control`](06-stdlib/control.md) | 30 |
| [`memory`](06-stdlib/memory.md) | 12 | [`eval`](06-stdlib/eval.md) | 14 |
| [`context`](06-stdlib/context.md) | 35 | [`meta`](06-stdlib/meta.md) | 20 |
| | | **Total** | **479** |

The counts are maintained by hand; chapter 10's conformance suite checks, from a `builtins.txt`
generated from these tables, that every built-in has a test, that no implemented built-in is
missing, and that every effectful built-in's return type follows the effect rule.
