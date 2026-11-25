# The Ratio specification

Version: draft 0.3 (2026-11-26). Status: design; nothing is implemented.

**Changes in draft 0.3.** Design review round 2 (N1 to N10 and nits): secrecy is a run-time
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
| [08 Integration with Imperium](08-imperium.md) | Sources, selection and pinning (item 58), the default harness and parity, Consul (item 64), hooks, the Agents panel and live harness view, code mode, headless evals, protocol |
| [09 Examples](09-examples.md) | Nine complete programs |
| [10 Implementation plan](10-implementation.md) | The `imperium-ratio` crate in 28 stages, engine seams, the pinned specification, CI-only testing, conformance suite |
| [11 Decisions and open questions](11-open-questions.md) | The user's decisions on Q1 to Q12; Q9 still open |

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
| R12 | **No `eval`** in a running program; models write harnesses only through the **authored-harness** path (stored, checked, pinned by closure hash, taint forced at the boundary, contained by default, caller authority only for a user-approved closure at the authority shown, never above the caller) | 07 |
| R13 | **Every side effect is an engine call**, decided by Imperium's dispatch (rules, floor, modes, hooks, approvals, events); delegation and session control are tool-gated; Ratio holds no authority of its own | 07, 08 |
| R14 | **`std:default` replaces the native loop** after the parity suite passes and one release cycle (the user's decision); until then the Rust loop is the default | 08 |
| R15 | Built as **one crate**, `imperium-ratio`, independent of the engine behind a concurrent `Host` trait; the specification stays in this repository and Imperium pins a revision | 10 |
| R16 | **Secrecy is a run-time label** (`Secret<T>` statically only where known), opt-in and off by default; labelled values and the conversations they enter never reach network tools, hosted models, commands, non-secret writes, delegation or stored outputs, enforced at the host and at dispatch, and redacted from every event leaving the daemon | 03, 07 |

## Size of the standard library

| Module | Built-ins | Module | Built-ins |
| --- | --- | --- | --- |
| [`model`](06-stdlib/model.md) | 29 | [`session`](06-stdlib/session.md) | 22 |
| [`think`](06-stdlib/think.md) | 30 | [`state`](06-stdlib/session.md#state-package-state) | 4 |
| [`tools`](06-stdlib/tools.md) | 50 | [`data`](06-stdlib/data.md) (15 submodules) | 213 |
| [`agents`](06-stdlib/agents.md) | 20 | [`control`](06-stdlib/control.md) | 29 |
| [`memory`](06-stdlib/memory.md) | 12 | [`eval`](06-stdlib/eval.md) | 14 |
| [`context`](06-stdlib/context.md) | 34 | [`meta`](06-stdlib/meta.md) | 20 |
| | | **Total** | **477** |

The counts are maintained by hand; chapter 10's conformance suite checks, from a `builtins.txt`
generated from these tables, that every built-in has a test, that no implemented built-in is
missing, and that every effectful built-in's return type follows the effect rule.
