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
4. **Every run is bounded.** Steps, memory, call depth, nesting, frame depth, width, tokens, cost,
   wall clock and call counts all have limits, so a wrong or hostile program ends.
5. **Everything is recorded.** Effects, trust conversions, approvals, harness calls and authored
   harnesses are in the journal, so a run can be audited and replayed.

## Who writes Ratio

Ratio programs come from five sources, in decreasing order of trust:

| Source | Written by | Trust it gets |
| --- | --- | --- |
| `std` | The Ratio project, shipped and reviewed with the `ratio` library | Full: may be selected and offered |
| `user` | The user, in `<config>/harnesses/` | Full |
| `plugin` | A plugin author; loaded only when the plugin is approved (below) | Selectable by the user once approved; offered to models only if listed in `harness.offered` (enabling the plugin offers it, chapter 08) |
| `project` | A repository; loaded only for projects in `ratio.trusted_projects` | Selectable by the user; never offered to models by default |
| `authored` | A model, through `write_harness` (below) | Runs contained, or at its caller's authority after approval of the exact version |

A lower-trust source never shadows a higher-trust one (chapter 03, source-qualified names).
Project trust is `ratio.trusted_projects`, a list of its own: trusting a repository's MCP servers
(`mcp.trusted_projects`) does not trust its harnesses, and the reverse.

**Plugins with harnesses are code-running plugins.** A plugin that contains a `harnesses/` folder
is withheld when installed, like a plugin with hooks or MCP servers, until the user approves it
in the desktop app through the existing plugin-approval challenge
(`docs/06-extensibility.md#plugin-marketplaces`); the approval is pinned to the plugin's content
hash, so an update is withheld again until approved. The same Windows conditions apply (roadmap
items 51 and 59).

**Agent files cannot reach across sources.** An agent file from a plugin or a project may name in
its `harness:` field only a package from its own source (its plugin's or its project's) or one in
`harness.offered`; any other name skips the field with a notice and the agent runs under the
inherited harness. A user's own agent files may name any registered package.

**Selection pins.** The package selected for a session, environment or agent, and every package it
imports, is pinned by IR hash at selection (chapter 08). A change to the files takes effect only
after the user selects the package again; until then the pinned version runs (kept in the
daemon's package cache) and the clients show that a newer version exists.

**Harness source folders are protected.** Writing into `<config>/harnesses`, a plugin's
`harnesses/`, a project's `.imperium/harnesses/`, or the authored areas would let an agent change
a harness that runs it later. These paths join the safety floor: file-tool writes and edits to
them ask every time and are never pre-approved, and commands that write there are caught where the
floor can see them. Imperium's floor today matches command lines and a fixed set of protected
files; matching file-tool paths against these folders is an Imperium change, staged in chapter 10.

### Decision R12: no `eval`, and a separate path for model-written harnesses

A running Ratio program never evaluates generated text: there is no `eval`, no `compile`, no
`load_source`, and `meta.harness` loads only registered packages by name. Models may nevertheless
**write harnesses**, including for their own use, through a separate path that turns text into a
package the same way a person's file becomes one: stored, checked, pinned by hash, journalled,
and run under bounds the model cannot lift.

| Option | Why not |
| --- | --- |
| A permission-gated `eval(text)` built-in | Code would be created and run in one step, inside the running frame, with that frame's authority; the only defence would be approving text mid-turn. No pinning, no versioning, no diff, nothing for the user to review later |
| `eval` of a restricted subset (pure expressions only) | Useful only for arithmetic and filtering, which code mode (roadmap item 65) already serves |
| Models never write harnesses | Loses the method the user asked for: a model that notices it needs, say, a vote over critics can write that method once and reuse it |
| **Authored harnesses** (chosen) | Authoring and running are separate acts; running is contained unless the user approves the exact version |

## Harnesses reached by models

Models reach harnesses through two tools: `spawn_agent`'s `harness` (a package's `@task` entry,
whose input is a `TaskInput` with an untrusted `Brief`) and `run_harness` (a public harness called like
a function, defined in chapter 06, `meta`). The rules for `run_harness` hold for **every** source,
`std`, user, plugin, project and authored alike:

- It reaches only `pub` harnesses that are not entries, of packages in `harness.offered` or the
  session's authored areas.
- **Every argument is deep-`Untrusted`.** The model's JSON is decoded through the parameter's
  schema and given untrusted text, whatever the harness declares.
- **A harness with trusted text in its parameters is not offerable.** If any parameter's type
  contains `Str`, `UserText`, `Brief` or `TrustedPrompt` anywhere (inside records, lists, options
  and enums included), the harness is refused for `run_harness`: at check time
  (`harnesses.check` reports it "not runnable by models", and `std` packages are built so that
  their offered harnesses pass) and at offer time (the daemon leaves it out of the offered list
  and refuses a call by name). A harness that needs trusted text from a model's argument must
  take it untrusted and convert it inside with an audited `as_task` or `trust`.
- Results go back to the model as a tool result, which it reads as data. For authored harnesses
  the results are also forced untrusted for Ratio callers (below).

## Authored harnesses

**Names.** Authored packages are qualified by area: `authored:session/<name>` lives with the
session, `authored:env/<name>` with the environment once persisted. A bare `authored:<name>`
means the session area first, then the environment area; inside an authored package's import
it means the importer's own area only.

**Writing.** A model writes a harness with the model-facing tool `write_harness(name, source,
description)` (a Ratio program with `meta.author`). The engine:

1. stores the source in the session's authored area (`<data>/sessions/<id>.harnesses/`), never in
   the user, plugin or project folders; the area is written only by the daemon and is a protected
   path for file tools and commands;
2. parses and checks it as a package of source `authored`: types, taint, the guarded-effect lint,
   and the authored rules below; a package with errors is stored with its diagnostics and is not
   loadable;
3. gives it a version number (1, 2, …) and pins it by a **closure hash**: the IR hash of the
   package together with the IR hashes of its whole transitive import closure (other authored
   packages of the area and the `std` packages it imports, with their versions). A change anywhere
   in the closure is a new version; versions are never edited in place;
4. journals `harness_authored` (`name`, `version`, `closure_hash`, the agent that wrote it, the
   source of every package in the closure, the diagnostics, and the diffs from the previous
   version), with `untrusted: true` on every source field, and shows it in the clients' authored
   harness list.

`write_harness` runs without asking: storing a checked package changes nothing else. It is a
pseudo-tool, so a frame without it in its tools cannot author, and a user rule can deny it. At
most 64 KiB per package, 32 versions per name and 16 names per session.

**Authored rules.** An authored package:

- may contain no `trust`, `as_task` or `declassify` sites at all (a checker error);
- may import only the standard library and authored packages of the same area;
- may not declare `@turn` (it can never become a session's root harness), and may declare `@task`;
- follows the `run_harness` rules above for every harness it exposes (and has no `trust` or
  `as_task` to convert with).

Rejected: allowing trust sites with per-site approval. A person cannot judge from a site what text
will flow through it, and a model under injection would write exactly the site it needs.

**The authored boundary forces taint.** Whatever an authored harness declares:

- every argument a model passes through `run_harness` is deep-`Untrusted`, as for every harness;
- every result of an authored harness reaches its caller as deep-`Untrusted<R>`, at both tiers
  and by every route: `run_harness`, `meta.harness`, `spawn_agent`'s report. `meta.harness::<H>`
  for an authored name is checked against `H` with its result type mapped to `Untrusted<R>`, so
  the caller's code is typed for untrusted results from the start.

**Running.** An authored harness runs as a frame, from Ratio with
`meta.harness::<H>("authored:session/vote")` or from a model with the model-facing tool
`run_harness(name, arguments)`. It runs in one of two tiers:

| Tier | Authority | Approval |
| --- | --- | --- |
| **Contained** (default) | The caller's frame attenuated further: tools only from the read set (`read_file`, `list_dir`, `glob`, `grep`, `memory_search`, `memory_read`, `search_sessions`); mode `plan`; no network, browser, MCP, command, write, delegation, memory-write, user-interaction or pseudo-tools; `sees` empty unless the caller grants items it has, and never `secrets`; the model **pinned to the caller's current model**, with provider-side tools (hosted web search, code execution, file search and similar) turned off; budget at most `ratio.authored.contained_budget` (default 50k tokens, 20 model calls, 2 minutes) and at most the caller's | None |
| **Caller** | At most the caller's frame: its tools, mode, rules, paths, location, `sees` and budget, attenuated as any harness call | The user approves **this closure hash at this authority** (below) |

A route that cannot turn provider-side tools off (an endpoint that always searches, a model whose
card says it may call hosted tools on its own) is refused at contained tier with
`Denied { by: attenuation }`.

Why the contained tier needs no approval: it can change nothing (no writes, commands or memory
writes), reach nothing outside the machine except through the model the caller is already using,
with that model's provider-side tools off, ask the user nothing, see no secrets and nothing
ambient, and spend at most a small, fixed budget. What it returns is untrusted data. Its worst
case, a harness that reads files the caller could already read and burns its small budget, is no
worse than the calling model doing the same.

**Caller-tier approval.** The permission request for `run_harness` shows:

- the **whole import closure**: the source of every authored package in it, rendered as untrusted
  model-written text (visually marked, never as markdown or with active links), and the diff of
  each package from the version last approved;
- `harnesses.check`'s summary of what the closure can do. Any use of `model.loop`,
  `tools.dispatch`, `tools.dispatch_all` or `tools.call_tool` counts as "**any tool of the
  frame**", since those run tools a model chooses; otherwise the summary lists the tools named;
- the **authority** it would run with: the caller frame's tools, mode, paths, location, `sees` and
  budget.

The approval is keyed by the closure hash and **scoped to the authority shown**: "allow once" or
"allow this version at this authority for the session"; it never becomes an "always" rule. A
later run from a caller frame with wider authority (more tools, a more permissive mode, more paths,
a wider `sees`, a larger budget) asks again; a narrower one does not. Approvals count against the
per-turn harness approval cap.

**Persistence.** Keeping an authored package for the environment (`authored:env/<name>`, stored
in `<workspaces.root>/<env>/harnesses.authored/`), so later sessions can run it, is the user's act
in the desktop app, through a native dialog with the plugin-approval challenge, showing the whole
closure and its hash. A persisted package is still source `authored`: contained by default, and
at caller tier only for a closure hash and authority the user approved (in that dialog,
persistently, or per session).

**Offered, never automatic.** Authored packages are listed to the model that may run them (in
`run_harness`'s description and `meta.harnesses()`), and may be named in `spawn_agent`'s
`harness` (the subagent then runs the `@task` entry at contained tier, or at caller tier for an
approved closure and authority), but they are never selected automatically: making an authored
package a session's or environment's harness is impossible (no `@turn`), and making it an agent
file's harness is the user's edit.

**Self-use.** A model that runs a harness it wrote runs it as a child of its own frame. Attenuation
bounds the child by the parent, and the results come back untrusted, so writing a harness and
running it can never raise the model's own authority: at best (caller tier, approved) the harness
does what the model could already do, without a model in between.

**Unattended sessions** may author and run contained; caller tier needs a closure hash and
authority the user approved persistently beforehand; otherwise `run_harness` falls back to
contained and says so.

## Permissions

- **Ratio cannot widen.** A frame's authority is the meet of its caller's and its request
  (chapter 04, R10). The root frame's authority is the session's: its permission mode, its rules
  (global and environment), path grants, the agent's permission profile, the safety floor.
- **The engine checks every effect.** Each effect carries its frame's tool set, mode, extra
  rules, paths, location and secrecy flag. The engine's dispatch refuses anything outside them
  before its own checks, so an interpreter bug can only refuse more, never allow more.
- **Approvals go through Imperium's broker.** A tool call that asks raises Imperium's own
  `permission_requested`, answered only by the person (or by the auto mode classifier where the
  session allows it, `docs/05-tools-and-isolation.md#auto-mode-implemented`). No Ratio built-in can
  answer a permission request, a question batch or a sampling request, and none can change the
  session's permission mode, rules, grants or profile.
- **Harness-level approvals** (`session.request_approval`, `run_harness` at caller tier,
  `declassify`) are permission requests under pseudo-tool names, so they appear in the same
  queue. A denial is remembered for the turn: the same request is refused again without asking.
  At most 3 harness approval prompts per turn; further ones are refused.
- **Delegation and session control are tool-gated.** `agents.*` built-ins are decided as
  `spawn_agent`, `ask_agent` and the other delegation tools; checkpoints, branches, rewinds and new
  sessions as `start_session` or pseudo-tools (chapter 06). A frame without the tool cannot use
  the built-in, and the user's rules apply.
- **Denials are not retried silently.** `retry` never retries `Denied`. `tools.dispatch` returns a
  denial to the model as the native loop does.
- **Unattended sessions** never wait for a person except where Imperium's safety floor says so.
  Everything else that would ask is refused (`Denied { by: unattended }`), questions get their
  recommendations, and `declassify` is refused.
- **Memory writes** are possible only in the root agent's turn, under the scope's write policy.
- **`ratio.trust`** (`allow`, `ask`, `deny`; default `ask`) decides whether packages with trust
  sites may be selected (chapter 03).

## Resource limits

Defaults apply per turn of an agent (the `@turn` or `@task` call and everything inside it). The
configuration (`[ratio.limits]`, chapter 08) may lower them or raise them up to the maximum; a
frame may only lower them. The defaults are draft values, to be set from measurements of the
examples on the eval suites (chapter 10, stage 24).

| Limit | Default | Maximum | When exceeded |
| --- | --- | --- | --- |
| Interpreter steps | 5,000,000 | 100,000,000 | `Budget { which: steps }` |
| Heap (live values) | 64 MiB | 512 MiB | `Budget { which: heap }` |
| Call depth (nested function and closure calls) | 512 | 4,096 | `Budget { which: call_depth }` |
| Frame depth (nested harness calls) | 8 | 32 | `Budget { which: depth }` |
| Parser nesting (tree depth: brackets, blocks, expressions, types, patterns, and every link of an operator or `.` chain) | 128 | 256 | A check-time diagnostic |
| Interpolation nesting inside strings | 4 | 8 | A check-time diagnostic |
| JSON nesting (`json.parse`, decoding) | 128 | 256 | `Invalid` |
| Single string | 16 MiB | 64 MiB | `Invalid` |
| Single list or map | 1,000,000 elements | 10,000,000 | `Invalid` |
| Package source | 256 KiB per file, 2 MiB per package (authored: 64 KiB) | — | Not loadable |
| Package loading | 1024 files, 64 packages, 4096 registry calls per load (answers, failures included, remembered); at most 64 modules in one import chain, the entry counted; package and plugin names at most 128 bytes | — | Not loadable |
| Concurrent branches (running; parked branches hold no slot, chapter 03) | 16 | 64 | Excess branches wait, and start in branch-path order |
| Reply floor (`min_reply_tokens`: the fewest output tokens a model call is admitted with, chapter 04) | 256 | — | The admission waits for held reservations, then raises `Budget` |
| Offloaded built-in jobs in flight per session (an implementation constant, chapter 01) | 2 | — | Further offloads wait; heap is accounted before dispatch, and a cancelled branch's job keeps its place, heap and steps until it ends |
| Running time of one `drain`-class tool call (chapter 06, `tools`) | the tool's timeout or the call's deadline | 3600 s | The host fails the call |
| Model calls | the session's `agents.max_steps` × 4 | — | `Budget { which: model_calls }` |
| Tool calls | 500 | 5,000 | `Budget { which: tool_calls }` |
| Tokens, cost | the session's budgets (`BudgetGate`) | the session's | `Budget { which: tokens \| cost }` |
| Wall clock | none in attended sessions; the run's `--timeout-s` in headless runs | — | `Budget { which: wall }` |
| Harness approval prompts | 3 per turn | 10 | Refused with a notice |
| `state` | 64 KiB | 64 KiB | `Invalid` |
| Session cache | 1 MiB | 16 MiB | Oldest entries evicted |
| Log and event data | 16 KiB per event | — | Cut with a marker |
| Live-view site events | 20 per second per frame (chapter 08) | 60 | Coalesced |
| Tape per turn | 64 MiB | 1 GiB | Recording stops with a notice; the turn continues live |

Parser nesting counts the depth of the syntax tree, not only brackets: a nested expression, type
or pattern costs one level, a `defer` block and a prompt `if` or `for` section one each, a parenthesis one, a call two, a tuple pattern two, and every link of
a left-associative chain costs one (`a + b + c` and `x |> f |> g`, a `.` chain, a postfix chain),
so a chain of 128 or more terms exceeds the default limit; split it with `let` bindings.

Step accounting is deterministic (it depends only on the program and its data): pure built-ins
are charged by cost class from the sizes of their inputs and outputs, never from representation
state (chapter 06, `data`). Sequential code that exceeds its steps exceeds them identically in
replay. Concurrent branches share their frame's steps ledger, and which branch reaches the limit
depends on timing, so each branch's step total and the exhaustion are events on the turn sequence,
replayed as recorded and verified against the limit (chapter 04).

## Prompt injection

**What Ratio adds over today's framing.**

- *Type-level separation of instruction and data.* Untrusted text cannot reach a `system`,
  `instruct`, `note` or `example` section, a harness's `system`, an approval summary or a harness
  name; it reaches `task` sections only as a `Brief` (chapter 03). Data sections are framed with
  the engine's own helpers.
- *Authority is not in prompts.* A critic frame with `tools: []` cannot be talked into running a
  command, whatever its input says, because the command tool is not in its frame and the engine
  refuses it.
- *Isolation between frames.* A callee sees only its arguments and what its `sees` grants.
- *Auditable trust.* Every `trust`, `as_task` and `declassify` is journalled with a digest and a
  reason and listed transitively by `harnesses.check`; `ratio.trust` controls whether packages
  with such sites may run at all.
- *Turn structure.* Automation prompts carry embedded logs, comments and issue bodies in
  `Turn.payloads`, not in the trusted `Turn.message`.

**What it cannot stop.**

- A model may still follow instructions in data it was shown.
- Implicit flows: a model chooses among the branches a harness offers it. The guarded-effect lint
  shows where a model-derived scalar alone guards an effect.
- Exfiltration through tool arguments of values that are not `Secret`: taint guards integrity, and
  `Secret<T>` guards only what is labelled. Unlabelled sensitive text read by an agent with web
  tools can still leave in a search query (`docs/05-tools-and-isolation.md#built-in-tools`);
  structuring frames (read without network, act with network on derived results) remains the
  harness author's tool.

## Threat model

**Assets.** The user's files and repositories; credentials and secrets on the machine; the
user's memory entries; the user's money (hosted model spend); the integrity of the user's
decisions (approvals); the integrity of the harnesses that run; the availability of the session
and the machine; the confidentiality of what agents read.

**Actors.**

| Actor | Capability |
| --- | --- |
| A1 Author of web pages, documents, issues, CI logs, repository files the agent reads | Controls text that reaches models as data or as turn payloads |
| A2 A compromised or malicious MCP server or tool provider | Controls tool results, tool names and descriptions |
| A3 A malicious plugin or project harness author | Controls Ratio source the user approved or a trusted project ships |
| A4 The model itself, misaligned or confused, or steered by A1 | Controls generated text, tool-call choices, and authored harness source |
| A5 A local process of another user, or a page in the app's window | Can reach the daemon's loopback port |

**Entry points.** Tool results, file reads, web content, MCP results, turn payloads (A1, A2);
model output and authored source (A4, steered by A1); harness sources (A3); the daemon's protocol
(A5); replay tapes and task files given to headless runs.

**Trust boundaries.** Generated text versus harness code; authored source versus registered
source; frame versus frame; the interpreter versus the engine's dispatch; the daemon versus the
person's approvals; trusted versus untrusted harness sources.

| # | Threat | Mitigation | Residual risk |
| --- | --- | --- | --- |
| T1 | Injected text reaches an instruction position (A1, A2) | `Untrusted<T>` and `Brief` sinks checked at load time, including generic code; framing of `task` and `data` sections; trust sites audited and gated by `ratio.trust` | A package whose author trusts carelessly; reviewers see every site |
| T2 | Generated text is executed as code inside a running frame (A4) | No `eval` (R12); model-written code only through authored harnesses (checked, pinned, contained) or code mode's confined worker | — |
| T3 | A harness widens its own authority (A3, A4) | Attenuation in the interpreter and independently in the engine; no built-in changes modes, rules, grants or profiles, or answers approvals | Interpreter and engine bugs; the engine check is the boundary |
| T4 | A malicious plugin or project package does harm with the session's authority (A3) | Plugins with harnesses withheld until approved and pinned by hash; projects only with `ratio.trusted_projects`; no shadowing of higher-trust names; selection pinned by hash; `harnesses.check` summaries | A package the user approved and selected can do what its tools allow without a model in the loop |
| T4b | **An injected install**: text steers a model or a program into installing a plugin, skill or agent file that then runs with the session's authority (A1 via A4) | `tools.install_extension` is a program's only install path and every install from it is agent-origin: the user is asked per install in every permission mode, the answer is never remembered, no allow rule, auto mode or `ratio_approval` skips the question, and unattended sessions refuse it; the installed extension stays withheld until the user approves it in the desktop app (chapter 06, `tools`; chapter 10, extension installs). The `ratio-lang` library itself is installed in Imperium only from a released tag (chapter 10, how Imperium pins Ratio) | The user approving an install they did not read, twice |
| T5 | An agent rewrites a harness that runs it later (A4 steered by A1) | Harness folders and authored areas are safety-floor paths; selection pins by hash and needs the user to re-select after a change | A user approving such a write and re-selecting |
| T6 | The model selects a dangerous harness for a subagent (A4) | `spawn_agent`'s `harness` accepts only packages in `harness.offered` and authored packages (which run contained unless approved); agent files from plugins and projects cannot name other sources | — |
| T6b | A model passes injected text into a harness's trusted parameter through `run_harness` (A4 steered by A1) | Every `run_harness` argument is deep-`Untrusted` for every source; harnesses with trusted text in their parameters are not offerable, refused at check time and at offer time | — |
| T7 | **Injection leads the model to author a malicious harness** (A1 via A4): one that reads secrets and smuggles them out, loops to burn money, edits files, or tries to persist | Contained tier: read tools only, the caller's model pinned with provider-side tools off, no network or writes, no secrets, no ambient reads, small fixed budget; no trust sites; arguments and results forced untrusted; caller tier only for a closure hash and authority the user approved with the whole closure, its diffs and its summary in view; wider authority asks again; persistence only through the native dialog; authored areas are protected paths; every version and run journalled and visible | The user approving a malicious version at caller tier; a contained harness wasting its small budget; contained reads returned to a caller that has network tools (the caller could read those files itself) |
| T8 | Denial of wallet (A4, A3) | Step, call, token, cost and wall budgets per frame and session; `Budget` cannot be swallowed inside its scope; contained-tier budget cap; admission by reservation with an effective `max_tokens`, so concurrent calls never overshoot any cap; unpriced hosted models refused under a cost cap; cancelled `drain` calls keep their reservations (chapter 04) | Spend up to the configured budgets |
| T9 | Approval fatigue (A3, A4) | Harness approvals capped per turn; denials remembered; versions approved once per session at most per prompt | The person approving without reading |
| T10 | Resource exhaustion of the daemon (A3, A4) | Heap, call-depth, nesting, string and collection limits; linear-time regex; steps charged by cost class against required representations, the input-size part debited before a built-in runs, so a quadratic built-in over large inputs fails with `Budget` instead of running; yields at every 1,000 steps and large built-ins offloaded to a bounded pool with a per-session cap, so one session cannot stall others; no threads for program code; package size limits | A heavy but legal harness slows its own session |
| T11 | Exfiltration of secret data (A1 via A4) | **Only where secrecy is configured**: labelled values, and calls on labelled conversations, are refused at the sinks of chapter 03 (network tools, hosted models, commands, non-secret writes, `call_tool`, delegation, persistence) by the host and by the engine at dispatch; commands whose line references a secret path ask every time and are refused when unattended; the checker adds load-time refusals only where the label is known statically. Elsewhere: frames without network tools for sensitive reads | Everything not labelled (by default nothing is); implicit flows; **a command that reads and uploads a secret file without naming it recognisably on its command line** (a script, a config-driven tool, an alias, a variable set earlier), which the best-effort command matching cannot see; a local model that saw a secret and later writes about it in an unlabelled conversation of another frame; search queries of unlabelled text leave the machine without asking (Imperium's existing residual risk) |
| T12 | Cross-frame leakage of private context into a frame that talks to the web | Isolation by default; `sees` empty for fresh frames; `fork` is explicit | Programs that fork or grant `sees` carelessly |
| T13 | A crafted tape or task file subverts a headless run | Replay answers effects with recorded values, which are untrusted; replay runs no real tools (lenient mode falls through to live calls under the run's own permissions); replay verifies the tape against itself (chapter 01): ledgers are rebuilt from recorded events and every admission, refusal, reading and step total checked, cache waits and promotions checked against recorded producers, cancellation outcomes checked, blobs checked against their digests, and labels only added, so a crafted tape cannot lift the limits on what a replay consumes, substitute bodies, hand a branch another key's cached value, or unlabel a value | Wrong results, a scoring problem, not an authority problem |
| T14 | Sensitive data persisted in tapes, caches and state, or sent to clients | Journal-protected; labelled values redacted from headless tape files and from every event leaving the daemon (below); caches keyed by type digest so untrusted values never return as trusted | Tapes and events contain unlabelled file contents and tool output by design |
| T14b | An agent runs the `imperium` CLI (or its own client) to declassify a conversation or subscribe with `secrets: true`, unredacting its own secrets (A4) | Both operations need the desktop app's single-use challenge and native dialog; no CLI or other client path exists; refused in unattended sessions | A user approving the dialog for a request they did not make |
| T15 | A process reaching the daemon loads or changes harnesses (A5) | `harnesses.list`, `harnesses.check` and `harnesses.source` are read-only; selection uses existing authorised session methods; persisting an authored package and approving a plugin need the desktop app's native dialog | As for every daemon method |
| T16 | Replay divergence hides a behaviour change | Strict replay fails on any request digest mismatch; the IR hash is recorded with every run | Lenient replay mixes live and recorded calls (listed in the output) |
| T17 | Cancellation tears or reorders effects, or turns into consent (A4, A3) | Only tools the engine or the user vouches for are aborted (never on a server's own claim); every other call drains, and the drain fence holds later writes, the frame's close and the turn's end until it finishes; every drain has a finite bound; a pending approval is withdrawn atomically and its late answer is discarded, never remembered as consent, a session allowance or a denial; an extension install withdrawn before approval is not installed (chapter 03) | Writes in `defer` blocks are exempt from the fence; a remote side may still act on an `abandoned` call |

## Redaction of events leaving the daemon

Every event, method result and stream the daemon sends to a client replaces secret-labelled
values with `[secret: <digest>]` (chapter 03), **unless** the client is a local process on the
daemon's own socket that explicitly asked for unredacted values. That client can only be the
**installed desktop app**: `session.subscribe` with `secrets: true` requires a single-use challenge bound to the installed desktop app's identity and answered through its native dialog, the pattern of `plugins.approval_challenge` and `pull_requests.auto_merge_challenge` (`docs/05-tools-and-isolation.md#auto-merge-the-desktop-app-alone-implemented`), and
the user enables it per session in that dialog. There is no CLI path: Imperium counts the CLI as
the user only for processes that do not descend from the daemon, and an agent's detached process
could pass that check, so a command an agent runs could otherwise unredact its own secrets. In an **unattended** session (schedules, auto-fix, project agents) no
client may subscribe with `secrets: true` and the per-session enablement does not exist: its
events are always redacted. The digest is an HMAC under a key generated per session, kept in the
session's data folder, never sent to a client, and deleted with the session. This covers `harness_site` and the live view, `ratio_effect`,
`ratio_state`, `ratio_log`, `harness_authored` (a model can put a secret into source it writes),
`harnesses.source`, `harnesses.replay`, every replayed event, and labelled engine state such as
plans, aids' state and **session titles** (including auto-titles and the session lists phones
show). **Paired phones never receive
unredacted values**, whatever they ask. Redaction is applied at the daemon's protocol boundary,
from the labels, so no client-side code is trusted with it.

**Out of scope for Ratio** (owned by Imperium): sandboxing of commands (isolation tiers), the
correctness of the safety floor's command parsing, MCP server isolation, the desktop app's
approval dialogs.

**Review gate.** Before Ratio is enabled by default, an independent security review covers the
attenuation path end to end (interpreter and engine), taint and secrecy sinks, the approval path,
authored harnesses, the approval dialogs and the live view, redaction, limits, and the source
loader and pinning (chapter 10, stage 24, which runs after those stages).
