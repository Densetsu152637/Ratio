# 08 Integration with Imperium

## Where harnesses come from

A **harness** in Imperium's sense (roadmap item 58) is either the **native** harness (today's Rust
agent loop) or a Ratio package. Packages come from five sources (chapter 07 gives their trust):

| Source | Qualified name | Folder | Loaded when |
| --- | --- | --- | --- |
| Built-in | `std:<name>` | `std/` of the Ratio repository, embedded in the `ratio` library | Always |
| User | `user:<name>` | `<config>/harnesses/` | Always |
| Plugins | `plugin:<plugin>/<name>` | A plugin's `harnesses/` | The plugin is enabled **and** approved (a plugin with harnesses is code-running, chapter 07) |
| Project | `project:<name>` | `.imperium/harnesses/` of a project folder | The project is in `ratio.trusted_projects`; otherwise one notice per folder |
| Authored | `authored:session/<name>`, `authored:env/<name>` | The session's authored area, or the environment's once the user persists a package | Written by `write_harness` (chapter 07) |

A bare name resolves to `std`, then `user`; a plugin, project or authored package is reached
only by its qualified name, so a lower source never shadows a higher one. A package is a `.ratio` entry file with a `meta` declaration
(size limits in chapter 07), found up to three folders deep; symbolic links are skipped.
Packages are parsed and checked when first needed and cached by content hash; a package with
errors is listed with its diagnostics and cannot be selected.

Built-in packages: `std:default` (the native loop in Ratio, below), `std:consul_escalation`
(item 64), `std:plan_execute_verify`, `std:self_consistency` and `std:tdd_fix` (chapter 09).

## Settings

```toml
[harness]
select = "native"                 # the default harness for new sessions: "native" or a package name
offered = ["std:*"]               # packages a model may name in spawn_agent's harness or run_harness
# dereference, state, hints, clarify, tool_loading, ... : the harness aids, unchanged

[ratio]
enabled = false                   # false: only "native" can be selected (until the security review)
trust = "ask"                     # allow | ask | deny: packages with trust sites (chapter 03)
trusted_projects = []             # project folders whose .imperium/harnesses are loaded
secrets = { enabled = false }     # secrecy labels are opt-in (chapter 03)
secret_paths = []                 # path globs whose contents are secret; empty by default
secret_commands = []              # command prefixes whose output is secret; empty by default
log_level = "info"                # debug | info | warn | error
limits = { steps = 5000000, heap_mb = 64, call_depth = 512, depth = 8, width = 16, tool_calls = 500, min_reply_tokens = 256 }
authored = { enabled = true, contained_budget = { tokens = 50000, model_calls = 20, wall_s = 120 } }
```

`harness.select`, `harness.offered` and every `ratio.*` key are read from the user's configuration
and an environment's settings only, never from a project's files, so a repository cannot choose
or widen the harness that runs it.

**Offered harnesses.** `harness.offered` lists, by pattern, the packages models may name.
Enabling an approved plugin adds its harnesses to the offered set (`plugin:<plugin>/*`), so models
can see and name them, but nothing uses them automatically: selecting one as a session,
environment or agent harness is a separate act of the user. Authored packages of the session are
always offered to the model that may run them, under the authored tiers of chapter 07. Whatever
the source, `run_harness` decodes every argument as deep-`Untrusted`, and a harness with trusted
text types in its parameters is left out of the offered list and refused by name (chapter 07,
harnesses reached by models).

## Selecting a harness (roadmap item 58)

**Precedence for a session's root agent:** the session's own choice (`session.create` with
`harness`, or `session.control` `set_harness`), then the environment's `harness` setting, then
`harness.select`, then `native`. Only `std`, `user`, `plugin` and `project` packages with a `@turn`
entry can be a root harness.

**Pinning.** Selecting a package records its IR hash and the hashes of everything it imports
(`harness_changed`: `from`, `to`, `version`, `hash`). The session runs exactly that version until
the user selects again, even when the files change; clients show "a newer version exists" and the
re-selection shows the diff and, under `ratio.trust = "ask"`, any new trust sites to approve.

**Switching.** `set_harness` takes effect at the next turn boundary, never mid-turn: while a turn
runs it is refused with `-32006`, as a model switch is
(`docs/03-agent-engine.md#switching-models-and-compaction`). The conversation is kept; the new
harness's `@turn` entry reads it with `context.main()`. A resumed session uses the pinned version
the journal last recorded. A package's `state` belongs to the package: switching away and back
finds it again.

**Subagents.** A subagent's harness is, in order: `spawn_agent`'s new `harness` argument (or
`agents.spawn(package:)`); the agent file's `harness` field (restricted for plugin and project
agent files, chapter 07); the spawning agent's package's `@task` entry, if it has one; otherwise
`native`. Item 58's other per-subagent fields map onto Ratio as follows:

| Item 58 field | Agent file front matter | `spawn_agent` argument | In a Ratio `@task` entry |
| --- | --- | --- | --- |
| Harness | `harness: std:tdd_fix` | `harness` (offered packages only) | The package that runs |
| System prompt | `prompt:` (inline text or a file next to the agent file) | `prompt` | `task.profile.file_instructions` (trusted, from the file) or `task.profile.spawn_prompt` (untrusted, from the dispatching model); both are also placed by the engine in `context.system_default(subagent)`, the spawn prompt framed |
| Workflow | `workflow:` (a workflow companion reference) | `workflow` | `task.profile.workflow`, loadable with `context.load` |

A value left unset inherits from the parent. The parent's permission mode, rules, budgets and the
spawning frame's attenuation bound the subagent whatever its harness, prompt or workflow.
`spawn_agent`'s schema gains `harness`, `prompt` and `workflow`
(`crates/imperium-protocol/schemas/tools/spawn_agent.json`, status `planned` until built);
because the Emperor training repository vendors these schemas, the change is announced there.

## The default agent loop in Ratio

`std:default` is the native loop written in Ratio. Each line maps to a documented behaviour of
`docs/03-agent-engine.md`; context assembly, harness aids, steering, compaction, budgets and
events are the engine's own, reached through built-ins.

```ratio
meta {
  name: "std:default",
  version: "1.0.0",
  description: "Imperium's standard agent loop: call the model, run its tools, repeat",
}

/// The root agent: one call per user turn.
@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  // The user message the native loop builds: workflow manifest, recalled memories, the harness
  // aids' state, plan and hint blocks, attachments, payloads, then the user's own text.
  context.append_turn(conv, turn)
  let r = run(conv, session.info().max_steps)
  let status = if r.stopped is max_steps { ReplyStatus.step_limit } else { ReplyStatus.done }
  { text: r.text, status: status }
}

/// A subagent: one call per turn of its task (the brief, then any follow-up).
@task
pub harness task(task: TaskInput) -> Report
  with { context: main }
{
  let conv = context.main()
  context.append_task(conv, task)
  let r = run(conv, session.info().max_steps)
  { text: r.text, ok: not (r.stopped is max_steps) }
}

type LoopEnd = { text: Untrusted<Str>, stopped: LoopStop }
enum LoopStop { done, max_steps }

fn run(conv: Conversation, max_steps: Int) -> LoopEnd {
  var steps = 0
  var last: Untrusted<Str> = ""
  loop {
    if steps >= max_steps {
      break { text: last, stopped: LoopStop.max_steps }
    }
    // model.chat runs a safe point first: steering messages and background notices join here,
    // and mid-turn compaction runs when the next request would pass context.auto_compact_at.
    let reply = model.chat(conv)
    steps += 1
    last = reply.text
    if reply.tool_calls.is_empty() {
      // A steering message that arrived while the model wrote its final answer keeps the turn
      // going for one more call (docs/03-agent-engine.md#queueing-and-steering).
      if context.pending(conv).any(|p| p.kind is steering) {
        continue
      }
      break { text: reply.text, stopped: LoopStop.done }
    }
    // Every call goes through dispatch: rules, floor, mode, hooks, approval, events; independent
    // calls run concurrently where the engine allows; results are appended in call order.
    tools.dispatch_all(conv, reply.tool_calls)
  }
}
```

`session.info().max_steps` is `agents.max_steps` (or the headless `max_steps` flag). Usage-limit
waits, stops and resumes happen inside `model.chat` as they do in the native loop.

**Decision R14: `std:default` replaces the native loop, after parity.** The user has decided that
`std:default` becomes the default harness once the parity suite passes and one release cycle of
opt-in use has gone by without regressions (chapter 11, Q1). Until then `harness.select` stays
`native`. **Parity** means: with the **default configuration** (in particular no secret sources,
so no value or conversation is ever labelled, chapter 03), on every recorded headless task of the
eval suites, run against the
same scripted endpoint with the same configuration, `native` and `std:default` produce
byte-identical model request bodies, the same tool calls with the same arguments in the same
order, the same final replies, and the same journal events, **excluding**: `harness_call_*`,
`harness_site`, `harness_changed` and every `ratio_*` event; event ids, timestamps and durations;
agent ids of harness agents; and the framing line of a subagent's brief where the native loop
sends the brief unframed and `context.append_task` would frame a non-trusted brief (the two must
then differ only in that line). After the switch the native loop is kept as `native` for one more
release, then removed with the research session's agreement.

## The Consul escalation workflow (roadmap item 64)

Item 64 asks for a workflow in which Consul takes over a subagent that keeps being rejected.
Ratio expresses it as the built-in package `std:consul_escalation`, whose full source is
[example 7](09-examples.md#7-consul-escalation-roadmap-item-64).

| Item 64 requirement | In `std:consul_escalation` |
| --- | --- |
| A workflow, not a setting; no per-environment threshold; no off switch | A package with the threshold as a constant (`TAKEOVER_AFTER = 2`, a user decision, chapter 11); users opt in by selecting it for subagents, or replace it with their own package |
| Rejections: the user declines an approval; a critic or judge refuses a result; a tool or command fails | `tools.dispatch` results with `outcome: Denied { by: user }`; a critique that does not pass; results with `outcome: failed` (tool errors and non-zero command exits) |
| Counted per subagent task, within a single turn | The counter is a local of the `@task` entry's call, which is one turn of one subagent |
| Consul takes over after two rejections, continuing from the current state | The same conversation continues with `opts: { model: CONSUL }` on every later `model.chat` of the turn; nothing is restarted or summarised |
| Hands back when finished, or when unsure with a question for whoever dispatched the work | The entry returns its report; when the working model ends with a question, the entry raises `Question`, which reaches the dispatcher (chapter 04, questions) |
| Consul critiques work by any model | Before takeover, Consul critiques the subagent's results. After takeover Consul is the worker, so a different critic (a frontier route, never Consul) reviews its results; a model does not grade its own work |
| In particular Emperor's planning | The `@turn` entry has Consul critique the root's plan. **Before execution only when `update_plan` arrives alone** in a reply: the plan is dispatched, critiqued, and the critique is in the conversation before the model's next call can act on it. When `update_plan` arrives together with other tool calls, those calls run in the same dispatch, so the critique necessarily comes after them |

The rejection counting and the handover are app work in item 64; with Ratio they are this package,
and the app work reduces to the harness switch and per-subagent selection of item 58.

## Hooks

- Every tool call a Ratio built-in makes passes through `tool.before` and `tool.after` hooks
  exactly as a model's call does, with the same matchers; pseudo-tools are matched by their names.
  Payloads gain an optional `harness` field (`{ package, version, harness, frame }`); a hook that
  blocks raises `Denied { by: hook }` in the program, or becomes a `Blocked` result in
  `tools.dispatch`.
- `agent.spawn` and `agent.complete` fire for `agents.spawn` as for `spawn_agent`;
  `permission.requested` fires for harness approvals.
- `model.before`, `model.after` and `compact.before` fire for Ratio's model calls and compactions
  when Imperium implements them (they are listed but not yet built, `docs/06-extensibility.md#hooks`).
- New observe-only events, run detached: `harness.call` and `harness.return` (`agent_id`,
  `package`, `harness`, `frame`, `depth`; on return `ok`, `error_kind`, `usage`) and
  `harness.authored` (a new authored version: name, version, hash, agent).
- Ratio programs cannot register or run hooks, and hooks cannot call into Ratio.

## Clients: the Agents panel and the live harness view

**Agents panel.** Harness agents (chapter 04) appear in the desktop's Agents panel nested under
the agent whose turn they run in, with title, package and harness, model, depth, spend and
status, and stay listed until the turn ends. Frames without a conversation appear as rows of their
parent's frame stack. Authored packages have their own list in the session sidebar (every
version, who wrote it, approval status, and its source).

**Live harness view.** A separate desktop panel shows the Ratio source of the harness an agent is
running, highlights the expression executing now, and shows the harness call stack, so a person
can watch a method work as one watches a program step through its lines.

- **Redaction.** Everything the view receives is redacted at the daemon's boundary (chapter 07):
  secret-labelled values appear as `[secret: <digest>]` unless the local client asked for them,
  and authored source is marked untrusted and rendered as plain text.
- **Source.** `harnesses.source` (`name`, `hash`) returns the pinned source files of a package
  version, with a map from site ids to source spans, so the panel shows exactly the version that
  runs even after the files on disk change. It is read-only and available for every source,
  authored packages included.
- **Stack.** From `harness_call_started` and `harness_call_finished` (`agent_id`, `frame`
  path, `harness`, call-site id, `depth`); the frame path elements are those of the effect keys
  (chapter 01), so a stack row and a tape entry name the same frame.
- **Current position.** `harness_site` events: `{ agent_id, frame, site, kind }`, where `site` is
  the IR site id (mapped to a span by the source map) and `kind` is `effect_start`,
  `effect_end`, `statement` or `waiting` (on a model, a tool, an approval, a subagent). Effect
  sites are always reported; statement sites only to clients that subscribe with
  `session.subscribe` `harness_sites: true`, since they are frequent.
- **Throttling.** Statement sites are sampled: at most 20 events per second per frame
  (`[ratio] live_view_rate`, at most 60), always including the latest position when the frame
  next waits or ends, so a slow-moving harness is shown exactly and a fast loop is shown as a
  moving highlight rather than a flood. Effect sites are never dropped. Concurrent branches each
  have their own frame row and highlight.
- **Journal.** `harness_site` events are streamed, not journalled. The journal's
  `harness_call_*` and `ratio_effect` events (whose keys contain the site ids) are enough to show,
  for a past turn, the call stack and every effect's position.
- **Replay.** Re-running a turn from its tape (`imperium harness replay`, or a headless run with
  `--ratio-tape` in replay mode) re-executes the program, so it emits the same `harness_site`
  stream as the live run did; the panel can step through it at any speed (`replay_speed` on the
  replay request: real time, faster, or one effect at a time). Since replay runs no real tools and
  model calls, stepping through a past run costs nothing.

## Code mode and harnesses (roadmap item 65)

**Decision pending (the user's, chapter 11 Q9).** Code mode lets a model write a short Python
program (`run_script`) in which every tool is a function, run in a confined Monty worker. The
question is whether such a program may also call Ratio harnesses as functions.

**Use cases.**

- A model computing over many items wants a reasoning method per item: `for f in files:
  notes.append(summarise(f))` where `summarise` is a harness with its own model and prompt.
- A model wants to filter or rank with a judge harness (`[x for x in xs if judge(x).pass]`)
  without pasting every item into its own context.
- A model wants to drive an authored harness it just wrote over a list of inputs.

**Risks across the boundary.**

- **Recursion.** A harness could offer `run_script` to its model, whose program calls the harness,
  and so on. Each crossing nests a worker inside a frame inside a worker.
- **Authority.** Code mode's inner calls pass the agent's dispatch; a harness frame has its own
  attenuation. A harness called from a program must not run with more than the program's caller
  could, and a program must not become a way to reach harnesses the model could not run directly.
- **Budgets.** Code mode has its own limits (calls, suspensions, wall time, approval wait); a
  harness call from a program can spend far more tokens than a tool call. Both sets of budgets
  must bind the call.
- **Replay.** Ratio replays effects by key; code mode's inner calls are effects of the program.
  A harness inside a program needs effect keys that include the program's call index, or replay
  of the outer run cannot locate the inner effects.
- **Visibility.** Code mode hides intermediate results from the conversation by design; a harness
  inside a program would hide a whole reasoning process, including its model calls, unless its
  events are shown nested.

**Options.**

| Option | Shape | Assessment |
| --- | --- | --- |
| A. No calls | Programs cannot see harnesses | Safest and simplest; loses the use cases |
| B. Offered harnesses only, contained | Programs get host functions for harnesses in `harness.offered` and the session's authored packages, each running at contained tier (chapter 07) regardless of approvals; no `run_script` inside such a frame | Covers the summarise and judge cases; harnesses that need writes or network are out |
| C. Offered harnesses, strictly attenuated | As B, but a harness runs at most with the program's caller's authority, attenuated further by an explicit `script` profile: no delegation, no user interaction, no `run_script`; approvals inside count against code mode's approval-wait budget | Covers more, at the cost of a second approval surface inside programs |
| D. Any harness at full authority | Programs call harnesses like tools | Rejected: a program becomes a way around attenuation and review |

**Rules any of B or C would need.** Harness frames opened from a program are children of the
agent's frame, with frame-path elements `script@<call id>#<inner index>`; `run_script` is never
in their tool set (no recursion); each call is one inner call of the program and is charged to
both code mode's call budget and the harness's budgets; the harness's model calls, tool calls and
`harness_call_*` events are shown nested under the program in the transcript; harnesses whose
return type is not schema-able cannot be called (the program gets JSON).

**Recommendation.** B first, as a later stage after code mode's own security review: it serves
the computation-over-items use cases with no new approval surface, since contained frames need
none. Move to C only if real tasks need writes from harnesses inside programs, and then with its
own review.

## Cancellation, deadlines and budgets in the engine

The host side of chapter 03's cancellation and chapter 04's admission rules is Imperium's. These
are requirements on `RatioHost` and the engine behind it, checked by the host contract tests
(chapter 10) where a test can observe them:

- **Cancellation classes.** `RatioHost` declares the class of every tool (chapter 06, `tools`); a
  tool it does not list is `drain`. Dispatch can abort an `abort`-class call and acknowledges it,
  and records a call that already reached a remote server as `abandoned`.
- **Read-only commands.** The command classifier that makes a `run_command` `abort`-class is a
  conservative allowlist owned by Imperium, in its permission broker: a command line it does not
  recognise is not read-only. It is built in stage 17; until then no command is
  read-only, so every command is `drain` and is killed only at its deadline.
- **Read-only MCP tools** come only from the user's configuration (a per-tool flag in the user's
  or an environment's MCP settings, never a project's files and never the server's own
  annotations).
- **A bound on every drain.** Every `drain`-class call has a finite bound of at most 3600 s once
  started, after which the host fails it. `run_command` already clamps its timeout to 1 s to
  3600 s; `RatioHost` adds an upper clamp to the MCP `timeout_s` and a per-call bound to the
  browser tools. A contract test (stage 11a) checks that every `drain`-class tool in the host's
  table declares a bound and that a never-ending scripted call is failed at it.
- **Deadlines.** Every host call receives its deadline (chapter 10, `EffectCtx`): model and MCP
  requests use it as their request timeout, and `run_command` takes the smaller of its timeout and
  the time left.
- **Atomic approve-or-withdraw.** The approval broker resolves a withdrawal racing the user's
  answer atomically (chapter 03); an answer to a withdrawn prompt changes no session approval and
  records no denial. An `install_extension` withdrawn before its approval is not installed.
- **Session stop is unchanged**: it drains every in-flight call whatever its class, as the native
  loop does.
- **Unpriced models under a cost cap.** Imperium admits unpriced models under a cost cap today.
  With Ratio, `BudgetGate` refuses a call to a hosted model (any location other than `local`,
  chapter 04) under any cost cap unless the model has an explicitly configured price (a price of
  0 counts as priced); the card Imperium gives a `local` model has price 0, which counts as
  priced. So admission by reservation (chapter 04) can always bound cost; Imperium implements
  this from stage 13.
- **Conversation digests.** The engine keeps each conversation's chained digest per message,
  restored by rollback, and exposes the head through the host (chapter 01, tape storage).

## Headless evaluations

`imperium run` (`docs/03-agent-engine.md#running-evals-through-the-harness-implemented`) gains:

| Flag | Meaning |
| --- | --- |
| `--harness <name \| path.ratio>` | The root agent's harness: `native` (default), a registered package, or a file. A file is loaded as a package of its own source root, with user-source trust |
| `--subagent-harness <name \| path.ratio>` | The harness for every subagent the run starts (item 58's per-subagent selection in evals); `spawn_agent`'s own `harness` still wins unless `--force-subagent-harness` |
| `--ratio-tape <file>` | Record every effect to the file (`--ratio-tape-mode record`, the default when the file does not exist) or answer effects from it (`replay`, the default when it does; `lenient` falls through to live calls on a mismatch). `full` records like `record` and adds a digest-only entry for every output-only effect (chapter 01). A file name ending in `.jsonl.zst` is zstd-compressed |
| `--flag ratio.<limit>=<n>` | Lowers a Ratio limit for the run |

`--subagent-replay` keeps working and is consulted before the tape for subagent reports.
`ratio.trust` applies to headless runs; `--flag ratio.trust=allow` relaxes it for a run of the
user's own files.

**Output** (schema version 2, additive): `harness` (`{ name, version, hash, source }`, or
`{ name: "native" }`), `subagent_harness`, and `ratio` with `metrics`, `scores`, `labels`,
`expectations`, `trust_sites`, `authored` (versions written during the run) and `tape`
(`{ mode, entries, replayed, diverged: [key], redacted: Int }`). `prompt_hash` covers the system
prompt the run's first model call sent and the tool schemas, as today; `harness.hash` covers the
program. Two runs compare directly only when both match. `--print-prompt` with a Ratio harness
prints `context.system_default()` as the entry would see it, the offered tools, and the package
hash. Harness variants (TOML) keep applying to the system prompt and tool list the engine builds.

## Protocol surface

All additive, in one protocol minor version:

| Method, event or tool | Purpose |
| --- | --- |
| `harnesses.list` | Every registered package: `{ name (qualified), version, source, path, description, entries, public: [Signature], hash, diagnostics, trust_sites, offered, approved }`. Read-only |
| `harnesses.check` | Parses and checks a package by name or path (inside the session's folders or a harness source); returns diagnostics with positions, the tools it may use, a summary (writes, commands, network, delegation), its transitive trust sites and, with `test: true`, the results of its `test` blocks against the scripted host. Read-only; never runs effects |
| `harnesses.source` | The pinned source files and site map of a package version (for authored packages, the whole closure, each file marked `untrusted: true`), for the live harness view. Read-only; redacted (chapter 07) |
| `harnesses.replay` | Re-runs a recorded turn from the journal's tape with no live effects, emitting its events (for the live view; `replay_speed`); every replayed event is redacted like a live one |
| `session.create` `harness`, `session.control` `set_harness` | Selecting and pinning (above) |
| `SessionSummary.harness`, `AgentSummary.harness` | `{ name, version, hash }` of what each agent runs |
| `harness_changed` | A selection, journalled with the pinned hashes |
| `harness_call_started`, `harness_call_finished` | Frames opening and closing (`agent_id`, `frame`, `harness`, call site, `depth`, `tier` for authored; usage and outcome on finish) |
| `harness_site` | The live position of each frame (streamed only, opt-in for statement sites) |
| `harness_authored`, `harness_approved` | Authored versions (closure hash, every source in the closure with `untrusted: true`, diffs) and the user's approvals (closure hash and the authority approved) |
| `ratio_log`, `ratio_trust`, `ratio_metric`, `ratio_span`, `ratio_event` | Logs, trust conversions (digest only), metrics, spans and custom events |
| `ratio_state`, `ratio_cache`, `ratio_effect` | Persistence: package state with its type digest, session-scoped cache entries with the labels of their leaves, the effect tape. Journalled; a `ratio_effect` whose body is already journalled refers to that event instead of copying it; `ratio_effect` is not streamed to clients unless they ask for it |
| Tools `write_harness`, `run_harness` | Model-facing tools for authored and offered harnesses, specified in chapter 06 (`meta`, model-facing harness tools) and chapter 07, defined in `imperium-protocol` like the other built-in tools and exported to `schemas/tools/` |
| `session.control` `declassify_conversation`, `session.subscribe` `secrets: true` | The user's recovery path for a labelled conversation, and unredacted events for the user's own view (chapters 03 and 07). Both need a single-use challenge bound to the installed desktop app and its native dialog (`ratio.declassify_challenge`, `ratio.secrets_challenge`, as `plugins.approval_challenge`); no CLI or other client can do either; refused in unattended sessions |

No method installs, edits or deletes a user, plugin or project harness; those are files the user
manages. The CLI gains `imperium harness list`, `imperium harness check <name|path>`,
`imperium harness test <name|path>` and `imperium harness replay <session> <turn>`, and
`imperium chat` gains `/harness <name>`. The desktop's harness picker, the Agents panel rows, the
authored list and the live harness view are client work (chapter 10).

## Which repository owns what

Ratio is a language in its own repository; Imperium is a host that depends on a released version
of it (chapter 10, the repository split). For this chapter's subjects:

| Subject | Ratio repository | Imperium repository |
| --- | --- | --- |
| Harness sources | The package format, `meta`, imports and their limits, qualified-name syntax and the shadowing rule, the source registry trait | The five sources' folders, which are loaded and with what trust, plugin and project gates |
| Built-in packages | Their source (`std:default`, `std:consul_escalation`, `std:plan_execute_verify`, `std:self_consistency`, `std:tdd_fix`) in `std/`, embedded in the library | Offering and selecting them, and the notice and re-selection after a pin miss (chapter 10, versions) |
| Settings | The limits' defaults and maxima (chapter 07) | `[harness]` and `[ratio]`, and that projects cannot set them |
| Selection, pinning, switching, subagents | IR hashes, closure hashes, `IR_VERSION` and the library version, which pins record | The precedence rules, `harness_changed`, turn-boundary switching, `spawn_agent`'s fields and agent files |
| The default loop and parity | `std:default` | The parity suite and the default switch (R14) |
| Hooks | Nothing: programs cannot register or run hooks | Every hook and its payloads |
| Agents panel and live harness view | Site ids, source maps and the effect keys that name frames | Events, throttling, redaction, the panels |
| Code mode (Q9) | Frame-path elements for frames opened from a program | `run_script`, its budgets, and the option the user chooses |
| Headless evaluations | The tape format (`TAPE_VERSION`), the turn sequence and verified replay | `imperium run`'s flags and output, compressed tape files |
| Cancellation and budgets | Cancellation points, the drain fence, withdrawal on the tape, admission by reservation and the reply floor, the host contract tests | The class of each tool, the read-only command classifier, read-only MCP configuration, the drain bounds, deadlines in dispatch, atomic withdrawal in the broker, `BudgetGate`'s refusal of unpriced models, conversation digest chains |
| Protocol | Nothing | Every method, event and tool |

The engine's side of Ratio is an implementation of the library's `Host` trait plus the harness
switch. Chapter 10 lists every touch point and which of them belong to files another session owns.
