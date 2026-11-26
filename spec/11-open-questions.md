# 11 Decisions and open questions

The user answered the twelve open questions of draft 0.1. This chapter records each decision and
where the specification carries it out. One question remains open.

## Decided

| # | Question | Decision | Where |
| --- | --- | --- | --- |
| Q1 | Should `std:default` replace the native loop as the default harness? | **Yes**, after the parity suite passes and one release cycle of opt-in use | Chapter 08 (R14 and the parity criterion), chapter 10 (stages 23 and 26) |
| Q2 | May a model write harnesses? | **Yes**, including for its own use, through the authored-harness path: stored in an authored area, checked and pinned by hash, no trust sites, contained by default, caller authority only for a version the user approves with source and diff, never above the caller, journalled and visible. Plugins (S6) and protected file paths (S7) are unchanged | Chapter 07 (R12, authored harnesses, T7), chapter 08 (tools and events), chapter 10 (stages 21 and 22) |
| Q3 | Add a confidentiality label? | **Yes**: secrecy as a run-time label (`Secret<T>` statically where known), opt-in sources, sinks enforced at the host and at dispatch, labelled conversations, and an always-asking `declassify`. Frames' `sees` (S9) narrows what is visible at all | Chapter 03 (secrecy), chapter 04 (`sees`), chapter 10 (stages 5, 6, 11, 14 and 18) |
| Q4 | Should harness calls appear in the Agents panel? | **Yes**, and a separate **live harness view** shows the running source, the executing expression and the call stack, with `harness_site` events, throttling and replay stepping | Chapter 04 (harness agents), chapter 08 (clients), chapter 10 (stages 22 and 25) |
| Q5 | Are the default limits right? | **Probably**: keep the draft values, add call-depth and nesting limits, and set the defaults from measurements in the hardening stage | Chapter 07 (limits), chapter 10 (stage 24) |
| Q6 | May built-in prompt texts be overridden per installation? | **No**. Per call only (`instructions:`), so eval results never depend on local files | Chapter 06 (built-in prompts) |
| Q7 | May programs see the model's reasoning text? | **No** | Chapter 05 (reasoning effort) |
| Q8 | Do plugins' harnesses become available to models? | Enabling an approved plugin **offers** its harnesses (they join `harness.offered`), but nothing uses them automatically; selecting one is a separate act. The same holds for authored harnesses. Agent files from plugins and projects may name only their own source's packages or offered ones | Chapter 07 (sources), chapter 08 (settings, subagents) |
| Q10 | Names | **Keep** `.ratio` and `imperium harness` | Chapters 08 and 10 |
| Q11 | Does the Consul escalation threshold stay a constant? | **Yes**: `TAKEOVER_AFTER = 2` in the built-in package; a copy is the user's own workflow | Chapter 08 (item 64), chapter 09 (example 7) |
| Q12 | Where does the specification live? | **In this repository**, which since draft 0.6 also holds the implementation: Ratio is a language with its own build chain, and Imperium depends on a release tag of it (the user's later decision). `builtins.txt` is generated and drift-checked here | Chapter 10 (R15, where the specification lives, the repository split) |

## Open

### Q9: may code-mode programs call harnesses?

The user's answer was "maybe; flesh this out". Chapter 08 ("Code mode and harnesses") gives the
use cases, the risks at the boundary (recursion, authority, budgets, replay, visibility), four
options, and the rules any permitted option needs.

| Option | Summary |
| --- | --- |
| A | No calls from programs to harnesses |
| B | Offered and authored harnesses only, always at contained tier; no `run_script` inside them |
| C | Offered harnesses at the program's caller's authority, under a `script` profile (no delegation, no user interaction, no `run_script`); approvals count against code mode's approval-wait budget |
| D | Any harness at full authority (rejected) |

**Recommendation:** B, as a later stage (chapter 10, stage 28) after code mode's own security
review; C only if real tasks need writes from harnesses inside programs, with its own review.

**Decision pending:** the user's.

## Deferred proposals

The performance, parallelism and concurrency review that draft 0.9 carries out deferred six
proposals. They are recorded here so they are not lost; none is part of the language.

| Proposal | Summary | Revisit when |
| --- | --- | --- |
| Provider prefix-cache hints | A `GenOpts.prefix_cache: auto \| off` option; the host places cache breakpoints after the system message and leading `data` sections | Imperium exposes provider cache control (stage 12 with Imperium 13, if taken up) |
| Eager tool execution while a reply streams | `model.loop` starts `abort`-class calls as soon as each call's arguments are complete in the stream | After parity (stage 23); it touches the engine's dispatch, hooks and the pairing invariant |
| A lint for independent sequential effects | An info-level checker lint suggesting `par { … }` for consecutive effects that share no data and no conversation | The effect analysis of stages 4 and 5 exists |
| Closure compilation of the prepared form | Compiling the prepared form (chapter 01) into Rust closures | Profiling shows the walker above 1 % of turn time (R2) |
| Ropes for `Str` | O(log n) concatenation and character slicing | Long-text building is measured hot |
| Named semaphores and branch priorities | A concurrency limit shared across `par` sites, and priority hints for speculative branches; would need syntax | Real harnesses need them after stage 23 |
