# The Ratio specification

Version: draft 0.1 (2026-11-26). Status: design; nothing is implemented.

Ratio is a language for writing **agent harnesses** for Imperium. A harness decides how an agent
works: what its model is told, which tools it may use, how results are checked, when it stops.
Imperium today has one harness, its Rust agent loop. Roadmap item 58 makes harnesses selectable
per session, environment, agent file and subagent; Ratio is the language those harnesses are
written in.

## Reading order

| Chapter | Contents |
| --- | --- |
| [01 Purpose and execution model](01-purpose-and-model.md) | What a program is, how the daemon runs it, the IR, determinism and replay, how an agent turn maps to execution |
| [02 Syntax](02-syntax.md) | Lexical structure and the complete EBNF grammar |
| [03 Semantics](03-semantics.md) | Values, types, taint (`Untrusted<T>`), errors, control flow, concurrency, modules, persistent state |
| [04 Harnesses as functions](04-harnesses.md) | Declaring and calling harnesses, frames, context isolation, overrides, attenuation, depth and budgets |
| [05 Shaping model thinking](05-thinking.md) | Prompts, thinking steps, scratchpads, effort, sampling, structured generation, stopping |
| [06 Standard library](06-stdlib/README.md) | Every built-in, by module, with signature, semantics, failures and permission |
| [07 Safety and security](07-security.md) | Trust model, no `eval`, permissions, approvals, limits, prompt injection, threat model |
| [08 Integration with Imperium](08-imperium.md) | Harness selection (item 58), hooks, headless evals, protocol, the default harness, Consul (item 64) |
| [09 Examples](09-examples.md) | Nine complete programs |
| [10 Implementation plan](10-implementation.md) | The `imperium-ratio` crate in stages, engine bindings, CI-only testing, conformance suite |
| [11 Open questions](11-open-questions.md) | Decisions left to the user |

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
| R3 | **Determinism by construction**: immutable values, no ambient effects, structured concurrency; every effect goes through one host interface and is recorded on a tape that replays a run exactly | 01 |
| R4 | **Expression-oriented**, brace-delimited, newline-terminated syntax; small enough to read in one sitting | 02 |
| R5 | **Static types with local inference**, structural records and tagged enums, one dynamic escape (`Json`) that must be decoded through a schema; types double as generation schemas | 03 |
| R6 | **Taint typing**: everything a model, tool, page or other agent produces is `Untrusted<T>`; it may flow anywhere except instruction positions; `trust()` converts it and is audited | 03, 07 |
| R7 | **Exceptions with a closed set of error kinds**, `try … catch` as an expression, `result()` to reify; cancellation and the budget that ran out cannot be swallowed by the scope they stop | 03 |
| R8 | **Structured concurrency only** (`par`, `par_map`, `race`, `timeout`); no detached tasks except engine subagents, which the engine owns | 03 |
| R9 | **Harness calls are function calls** with their own frame (model, tools, permissions, budget, conversation); a callee sees nothing of its caller's conversation unless passed or forked | 04 |
| R10 | **Attenuation only**: every frame's tools, permissions, model locations and budgets are the meet of its caller's and its own request | 04, 07 |
| R11 | **Prompts are typed values** built from sections (`system`, `instruct`, `task`, `data`, `example`, `format`, `steps`); the section decides how text is framed, not string concatenation | 05 |
| R12 | **No `eval`** of generated text, at all, in any version of this specification; model-written programs belong to Imperium's code mode (roadmap item 65) | 07 |
| R13 | **Every side effect is an engine call**: Ratio tool built-ins run through the engine's own dispatch (rules, safety floor, modes, hooks, approvals, events); Ratio holds no authority of its own | 07, 08 |
| R14 | The **Rust loop stays the default harness** until a Ratio transliteration (`std:default`) is proven equivalent by differential tests; then switching is the user's decision | 08 |
| R15 | Built as **one crate**, `imperium-ratio`, independent of the engine behind a `Host` trait; the engine implements the host | 10 |

## Size of the standard library

| Module | Built-ins | Module | Built-ins |
| --- | --- | --- | --- |
| [`model`](06-stdlib/model.md) | 27 | [`session`](06-stdlib/session.md) | 22 |
| [`think`](06-stdlib/think.md) | 30 | [`data`](06-stdlib/data.md) (15 submodules) | 212 |
| [`tools`](06-stdlib/tools.md) | 46 | [`control`](06-stdlib/control.md) | 27 |
| [`agents`](06-stdlib/agents.md) | 18 | [`eval`](06-stdlib/eval.md) | 14 |
| [`memory`](06-stdlib/memory.md) | 12 | [`meta`](06-stdlib/meta.md) | 18 |
| [`context`](06-stdlib/context.md) | 31 | **Total** | **457** |

The counts are maintained by hand; chapter 10's conformance suite checks that every built-in in
these tables has at least one test and that no implemented built-in is missing from them.
