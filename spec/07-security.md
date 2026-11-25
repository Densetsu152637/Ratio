# 07 Safety and security

## Principles

1. **Authority lives in code and configuration, never in text.** What a frame may do is fixed by
   the session's permissions, attenuated by the harness code that opened the frame. Nothing a
   model writes, and nothing a tool returns, can add a tool, a path, a permission or a budget.
2. **Model and tool output is data.** It is typed `Untrusted<T>`, framed when it reaches a model,
   and kept out of instruction positions by the type checker (chapter 03).
3. **Ratio has no authority of its own.** Every effect is an engine call, decided by the engine's
   own permission machinery. The interpreter's checks are a first line; the engine's are the
   boundary.
4. **Every run is bounded.** Steps, memory, depth, width, tokens, cost, wall clock and call counts
   all have limits, so a wrong or hostile program ends.
5. **Everything is recorded.** Effects, trust conversions, approvals and harness calls are in the
   journal, so a run can be audited and replayed.

## Who writes Ratio

Ratio programs are written by people: the Imperium project (built-in packages), the user, plugin
authors, and project maintainers. They are loaded only from the sources chapter 08 lists, under
the same trust rules as agent files: a project's packages only when the project is trusted
(`mcp.trusted_projects`), symbolic links refused, size-limited.

**A model never writes Ratio that runs.** There is no path from generated text to Ratio code.

### Decision R12: no `eval`

Ratio has no `eval`, no `compile`, no `load_source`, no way to construct a harness from a string,
and `meta.harness` loads only registered packages by name. This holds in every version of this
specification; a later version that wants dynamic code must say so as a new decision.

| Option | Why not |
| --- | --- |
| A permission-gated `eval(text)` built-in | A model that can get text evaluated as a harness can write a harness that calls tools in a loop with the frame's full authority and no model in between; approval of the text is the only defence, and people approve code they have not read. The gate would also make every prompt injection a code injection attempt |
| `eval` of a restricted subset (pure expressions only) | Useful only for arithmetic and filtering, which code mode (roadmap item 65) already serves with a confined interpreter built for hostile input |
| Model-proposed harnesses, installed after review | Possible later as an extension install (written to a harness folder, approved by the user in the desktop app like a plugin). It is not `eval`: the model proposes a file, a person installs it. Listed as an open question |

Model-written programs have a home in Imperium already: `run_script` (item 65) runs them in a
confined Monty worker, every inner tool call going through dispatch. A Ratio harness can offer
`run_script` to its model as a tool (`tools.run_script`), and that is the only way generated code
runs.

## Permissions

- **Ratio cannot widen.** A frame's authority is the meet of its caller's and its request
  (chapter 04, R10). The root frame's authority is the session's: its permission mode, its rules
  (global and environment), path grants, the agent's permission profile, the safety floor.
- **The engine checks every effect.** Each effect carries its frame's tool set, mode, extra
  rules, paths and location. The engine's dispatch refuses anything outside them before its own
  checks, so an interpreter bug can only refuse more, never allow more.
- **Approvals go through Imperium's broker.** A tool call that asks raises Imperium's own
  `permission_requested`, shown in the clients, answered only by the person (or by the auto mode
  classifier where the session allows it, `docs/05-tools-and-isolation.md#auto-mode-implemented`).
  No Ratio built-in can answer a permission request, a question batch or a sampling request, and
  no Ratio built-in can change the session's permission mode, rules, grants or profile.
- **Harness-level approvals** (`session.request_approval`) are permission requests for the
  pseudo-tool `ratio_approval`, so the user's rules apply to them and they appear in the same
  approval queue. A denial is remembered for the turn: the same summary is refused again without
  asking, so a looping harness cannot flood the person with prompts. At most 3 harness approval
  prompts per turn; further ones are refused.
- **Denials are not retried silently.** `retry` never retries `Denied`. `tools.dispatch` returns a
  denial to the model as the native loop does.
- **Unattended sessions** never wait for a person except where Imperium's safety floor says so
  (floor-covered calls wait in the approval queue with a notification). Everything else that would
  ask is refused (`Denied { by: unattended }`), and questions get their recommendations.
- **Memory writes** are possible only in the root agent's turn, under the scope's write policy.
- **Self-modification is blocked.** Writing into a harness source folder (`<config>/harnesses`,
  plugins' `harnesses/` folders, a project's `.imperium/harnesses/`) would let an agent change
  the harness that runs it next turn. These folders join the safety floor's protected paths
  (asked every time, never pre-approved; chapter 10 names this as an engine change).

## Resource limits

Defaults apply per turn of an agent (the `@turn` or `@task` call and everything inside it). The
configuration (`[ratio.limits]`, chapter 08) may lower them or raise them up to the maximum; a
frame may only lower them.

| Limit | Default | Maximum | When exceeded |
| --- | --- | --- | --- |
| Interpreter steps | 5,000,000 | 100,000,000 | `Budget { which: steps }` |
| Heap (live values) | 64 MiB | 512 MiB | `Budget { which: heap }` |
| Single string | 16 MiB | 64 MiB | `Invalid` |
| Single list or map | 1,000,000 elements | 10,000,000 | `Invalid` |
| Frame depth | 8 | 32 | `Budget { which: depth }` |
| Concurrent branches | 16 | 64 | Excess branches wait |
| Model calls | the session's `agents.max_steps` × 4 | — | `Budget { which: model_calls }` |
| Tool calls | 500 | 5,000 | `Budget { which: tool_calls }` |
| Tokens, cost | the session's budgets (`BudgetGate`) | the session's | `Budget { which: tokens \| cost }` |
| Wall clock | none in attended sessions; the run's `--timeout-s` in headless runs | — | `Budget { which: wall }` |
| Harness approval prompts | 3 | 10 | `Denied { by: unattended }`-style refusal with a notice |
| `state` | 64 KiB | 64 KiB | `Invalid` |
| Session cache | 1 MiB | 16 MiB | Oldest entries evicted |
| Log and event data | 16 KiB per event | — | Cut with a marker |
| Tape per turn | 64 MiB | 1 GiB | Recording stops with a notice; the turn continues live |

Step accounting is deterministic (it depends only on the program and its data), so a run that
exceeds its steps exceeds them identically in replay.

## Prompt injection

**What Ratio adds over today's framing.**

- *Type-level separation of instruction and data.* Untrusted text cannot reach a `system` or
  `instruct` section, a harness's `system`, an approval summary or a harness name, by type. Data
  sections are framed with the engine's own helpers and defused tags.
- *Authority is not in prompts.* A harness decides the tools and permissions of each frame in
  code. A critic frame with `tools: []` cannot be talked into running a command, whatever its
  input says, because the command tool is not in its frame and the engine refuses it.
- *Isolation between frames.* A callee sees only its arguments. An injected page read by one
  frame reaches another only if the program passes it, and then as an untrusted, framed argument.
- *Auditable trust.* Every `trust` is journalled with a reason and listed by `harnesses.check`.

**What it cannot stop.**

- A model may still follow instructions in data it was shown. Framing and isolation lower the odds
  and the blast radius; they do not change the model.
- A model chooses among the branches a harness offers it (scalars are trusted). Its influence is
  bounded by those branches and their authority.
- **Exfiltration through tool arguments.** Taint guards integrity, not confidentiality: an agent
  steered by injected text can still put what it read into a `web_search` query or a `web_fetch`
  URL (`docs/05-tools-and-isolation.md#built-in-tools`). Ratio's mitigation is structural: a
  harness can read sensitive material in a frame without web tools and pass only a derived result
  to a frame that has them. Confidentiality labels (a `Secret<T>` that may not reach network
  tools) are an open question (chapter 11).

## Threat model

**Assets.** The user's files and repositories; credentials and secrets on the machine; the
user's memory entries; the user's money (hosted model spend); the integrity of the user's
decisions (approvals); the availability of the session and the machine; the confidentiality of
what agents read.

**Actors.**

| Actor | Capability |
| --- | --- |
| A1 Author of web pages, documents, issues, repository files the agent reads | Controls text that reaches models as data |
| A2 A compromised or malicious MCP server or tool provider | Controls tool results and tool descriptions |
| A3 A malicious plugin or project harness author | Controls Ratio source the user enabled or a trusted project ships |
| A4 The model itself, misaligned or confused | Controls generated text and tool-call choices |
| A5 A local process of another user, or a page in the app's window | Can reach the daemon's loopback port |

**Entry points.** Tool results, file reads, web content and MCP results (A1, A2); model output
(A4, steered by A1); harness sources (A3); the daemon's protocol (A5); replay tapes and task files
given to headless runs (the person running the eval).

**Trust boundaries.** Generated text versus harness code; frame versus frame; the interpreter
versus the engine's dispatch; the daemon versus the person's approvals; trusted versus untrusted
harness sources.

| # | Threat | Mitigation | Residual risk |
| --- | --- | --- | --- |
| T1 | Injected text reaches an instruction position (A1, A2) | `Untrusted<T>` sinks checked at load time; framing of `task` and `data` sections; `trust` audited | A program that `trust`s carelessly; reviewers see every site |
| T2 | Generated text is executed as code (A4) | No `eval` (R12); code only through item 65's confined worker | — |
| T3 | A harness widens its own authority (A3, A4) | Attenuation in the interpreter and independently in the engine; no built-in changes modes, rules, grants or profiles, or answers approvals | Interpreter and engine bugs; the engine check is the boundary |
| T4 | A malicious harness package does harm with the session's authority (A3) | Project harnesses only for trusted projects; plugin harnesses selectable only after the user enables them (`harness.offered`); harnesses run with exactly the authority a model with the same tools has, under the same rules and floor; `harnesses.check` shows tools used and trust sites | A harness the user enabled can do what its tools allow without a model in the loop; enabling a harness is a trust decision, like enabling a plugin |
| T5 | An agent rewrites the harness it runs under (A4 steered by A1) | Harness folders are safety-floor paths: every write asks; harness selection is the user's configuration, not a tool | A user approving such a write |
| T6 | The model selects a dangerous harness for a subagent (A4) | `spawn_agent`'s `harness` accepts only packages in `harness.offered` (default: built-ins); unknown names fail | — |
| T7 | Denial of wallet: loops that spend tokens (A4, A3) | Step, call, token, cost and wall budgets per frame and per session; `Budget` cannot be swallowed inside its scope | Spend up to the configured budgets |
| T8 | Approval fatigue: a harness floods the person with prompts (A3, A4) | Harness approvals capped per turn; denials remembered; tool approvals go through Imperium's broker, which shows the exact call | The person approving without reading |
| T9 | Resource exhaustion of the daemon (A3) | Heap, string and collection limits; linear-time regex; steps; JSON nesting limits; no threads | A heavy but legal harness slows its own session |
| T10 | Exfiltration of read data through network tool arguments (A1 via A4) | Frames without web tools for sensitive reads; Imperium's own web rules (fetch asks outside allowed domains) | Search queries leave the machine without asking (Imperium's existing residual risk) |
| T11 | Cross-frame leakage of a user's private context into a frame that talks to the web (A1) | Isolation by default; `fork` is explicit and visible in the IR | Programs that fork carelessly |
| T12 | A crafted tape or task file subverts a headless run | Replay answers effects with recorded values, which are untrusted; replay runs no real tools (lenient mode falls through to live calls under the run's own permissions) | A tape can make a run produce wrong results, which is a scoring problem, not an authority problem |
| T13 | Sensitive data persisted in tapes, caches and state | All live in the session journal and data folder with the journal's protections; tapes written by headless runs go only where the person running them says | Tapes contain file contents and tool output by design |
| T14 | A process reaching the daemon loads or changes harnesses (A5) | `harnesses.list` and `harnesses.check` are read-only; selecting a harness uses existing session and environment methods under existing client authorisation; no method installs a harness | As for every daemon method |
| T15 | Replay divergence hides a behaviour change | Strict replay fails on any request digest mismatch; the IR hash is recorded with every run | Lenient replay, if chosen, mixes live and recorded calls (listed in the output) |

**Out of scope for Ratio** (owned by Imperium): sandboxing of commands (isolation tiers), the
correctness of the safety floor's command parsing, MCP server isolation, the desktop app's
approval dialogs.

**Review gate.** Before the harness switch ships with Ratio enabled, an independent security
review covers the attenuation path end to end (interpreter and engine), taint sinks, the
approval path, limits, and the harness source loader, as code mode's plan requires for its worker
(chapter 10, stage 11).
