# The Ratio specification

Version: draft 0.8 (2026-11-26). Status: design; stages 1 and 2 of the implementation (the
token reader and the parser) are built in this repository.

**Changes in draft 0.8.** Errata from the stage 2 parser. Chapter 02: a `>` that closes a generic
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
| R8 | **Structured concurrency only** (`par`, `par_map`, `race`, `timeout`); no detached tasks except engine subagents, which the engine owns | 03 |
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
| [`agents`](06-stdlib/agents.md) | 20 | [`control`](06-stdlib/control.md) | 29 |
| [`memory`](06-stdlib/memory.md) | 12 | [`eval`](06-stdlib/eval.md) | 14 |
| [`context`](06-stdlib/context.md) | 35 | [`meta`](06-stdlib/meta.md) | 20 |
| | | **Total** | **478** |

The counts are maintained by hand; chapter 10's conformance suite checks, from a `builtins.txt`
generated from these tables, that every built-in has a test, that no implemented built-in is
missing, and that every effectful built-in's return type follows the effect rule.
