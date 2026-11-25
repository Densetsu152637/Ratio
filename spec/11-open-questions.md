# 11 Decisions and open questions

The user answered the twelve open questions of draft 0.1. This chapter records each decision and
where the specification carries it out. One question remains open.

## Decided

| # | Question | Decision | Where |
| --- | --- | --- | --- |
| Q1 | Should `std:default` replace the native loop as the default harness? | **Yes**, after the parity suite passes and one release cycle of opt-in use | Chapter 08 (R14 and the parity criterion), chapter 10 (stages 21 and 23) |
| Q2 | May a model write harnesses? | **Yes**, including for its own use, through the authored-harness path: stored in an authored area, checked and pinned by hash, no trust sites, contained by default, caller authority only for a version the user approves with source and diff, never above the caller, journalled and visible. Plugins (S6) and protected file paths (S7) are unchanged | Chapter 07 (R12, authored harnesses, T7), chapter 08 (tools and events), chapter 10 (stage 19) |
| Q3 | Add a confidentiality label? | **Yes**: `Secret<T>`, with sources, sinks enforced by the checker and at dispatch, and an always-asking `declassify`. Frames' `sees` (S9) narrows what is visible at all | Chapter 03 (secrecy), chapter 04 (`sees`), chapter 10 (stages 5 and 13) |
| Q4 | Should harness calls appear in the Agents panel? | **Yes**, and a separate **live harness view** shows the running source, the executing expression and the call stack, with `harness_site` events, throttling and replay stepping | Chapter 04 (harness agents), chapter 08 (clients), chapter 10 (stage 22) |
| Q5 | Are the default limits right? | **Probably**: keep the draft values, add call-depth and nesting limits, and set the defaults from measurements in the hardening stage | Chapter 07 (limits), chapter 10 (stage 20) |
| Q6 | May built-in prompt texts be overridden per installation? | **No**. Per call only (`instructions:`), so eval results never depend on local files | Chapter 06 (built-in prompts) |
| Q7 | May programs see the model's reasoning text? | **No** | Chapter 05 (reasoning effort) |
| Q8 | Do plugins' harnesses become available to models? | Enabling an approved plugin **offers** its harnesses (they join `harness.offered`), but nothing uses them automatically; selecting one is a separate act. The same holds for authored harnesses. Agent files from plugins and projects may name only their own source's packages or offered ones | Chapter 07 (sources), chapter 08 (settings, subagents) |
| Q10 | Names | **Keep** `.ratio` and `imperium harness` | Chapters 08 and 10 |
| Q11 | Does the Consul escalation threshold stay a constant? | **Yes**: `TAKEOVER_AFTER = 2` in the built-in package; a copy is the user's own workflow | Chapter 08 (item 64), chapter 09 (example 7) |
| Q12 | Where does the specification live? | **In this repository.** Imperium references a pinned revision (submodule or lock file) and vendors the generated `builtins.txt`, checked for drift in CI | Chapter 10 (where the specification lives) |

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

**Recommendation:** B, as a later stage (chapter 10, stage 25) after code mode's own security
review; C only if real tasks need writes from harnesses inside programs, with its own review.

**Decision pending:** the user's.
