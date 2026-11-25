# 08 Integration with Imperium

## Where harnesses come from

A **harness** in Imperium's sense (roadmap item 58) is either the **native** harness (today's Rust
agent loop) or a Ratio package. Packages are found in these sources, lowest precedence first; a
later source replaces an earlier package of the same name, with a notice, as agent files do
(`docs/06-extensibility.md#named-agents-implemented`):

| Source | Folder | Loaded when |
| --- | --- | --- |
| Built-in | Shipped inside `imperium-ratio` as `std:<name>` | Always; `std:` names cannot be replaced by any other source |
| Plugins | A plugin's `harnesses/` | The plugin is enabled |
| User | `<config>/harnesses/` | Always |
| Project | `.imperium/harnesses/` of a project folder | The project is in `mcp.trusted_projects`; otherwise one notice per folder |

A package is a `.ratio` entry file with a `meta` declaration, at most 256 KiB per file and 2 MiB
per package, found up to three folders deep; symbolic links are skipped. Packages are parsed and
checked when first needed and cached by content hash; a package with errors is listed with its
diagnostics and cannot be selected.

Built-in packages: `std:default` (the native loop in Ratio, below), `std:consul-escalation`
(item 64, below and chapter 09), and the examples of chapter 09 that are generally useful
(`std:plan-execute-verify`, `std:self-consistency`, `std:tdd-fix`). Built-ins are reviewed with
the crate and versioned with it.

## Selecting a harness (roadmap item 58)

**Settings.**

```toml
[harness]
select = "native"                 # the default harness for new sessions: "native" or a package name
offered = ["std:*"]               # packages the model may name in spawn_agent's harness (patterns)
# dereference, state, hints, clarify, tool_loading, ... : the harness aids, unchanged

[ratio]
enabled = false                   # false: only "native" can be selected (until the security review)
log_level = "info"                # debug | info | warn | error
limits = { steps = 5000000, heap_mb = 64, depth = 8, width = 16, tool_calls = 500 }
```

`harness.select` and `harness.offered` are read from the user's configuration and an
environment's settings only, never from a project's files, so a repository cannot choose the
harness that runs it.

**Precedence for a session's root agent:** the session's own choice (`session.create` with
`harness`, or `session.control` `set_harness`), then the environment's `harness` setting, then
`harness.select`, then `native`.

**Switching.** `session.control` `set_harness` (`name`) takes effect at the next turn boundary,
never mid-turn: while a turn runs it is refused with `-32006`, as a model switch is
(`docs/03-agent-engine.md#switching-models-and-compaction`). The conversation is kept; the new
harness's `@turn` entry reads it with `context.main()`. The switch is journalled as
`harness_changed` (`from`, `to`, `version`, `hash`) and a resumed session uses the harness the
journal last switched to. A package's `state` belongs to the package: switching away and back
finds it again.

**Subagents.** A subagent's harness is, in order: `spawn_agent`'s new `harness` argument (or
`agents.spawn(harness:)`); the agent file's `harness` field; the spawning agent's package's
`@task` entry, if the spawning agent runs a package that has one; otherwise `native`. Item 58's
other per-subagent fields map onto Ratio as follows:

| Item 58 field | Agent file front matter | `spawn_agent` argument | In a Ratio `@task` entry |
| --- | --- | --- | --- |
| Harness | `harness: std:tdd-fix` | `harness` | The package that runs |
| System prompt | `prompt:` (inline text or a file next to the agent file) | `prompt` | `task.profile.instructions`; also part of `context.system_default(subagent)` |
| Workflow | `workflow:` (a workflow companion reference) | `workflow` | `task.profile.workflow`, loadable with `context.load` |

A value left unset inherits from the parent, as item 58 says. The parent's permission mode, rules
and budgets bound the subagent whatever its harness, prompt or workflow. The model may name only
packages matching `harness.offered`; the user's own configuration and agent files may name any
registered package. `spawn_agent`'s schema gains `harness`, `prompt` and `workflow`
(`crates/imperium-protocol/schemas/tools/spawn_agent.json`, status `planned` until built); because
the Emperor training repository vendors these schemas, the change is announced there.

**Headless evaluation runs** take the harness on the command line (below).

## The default agent loop in Ratio

`std:default` is the native loop written in Ratio. Each line maps to a documented behaviour of
`docs/03-agent-engine.md`; the context assembly, harness aids, steering, compaction, budgets and
events are the engine's own, reached through built-ins, so the two harnesses send the same
requests for the same conversation.

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
  // aids' state, plan and hint blocks, attachments, then the user's own text.
  context.append_turn(conv, turn)
  let r = run(conv, session.info().max_steps)
  { text: r.text, status: if r.stopped is max_steps { ReplyStatus.step_limit } else { ReplyStatus.done } }
}

/// A subagent: one call per turn of its task (the brief, then any follow-up).
@task
pub harness task(task: Task) -> Report
  with { context: main }
{
  let conv = context.main()
  context.append_user(conv, prompt { task task.brief })
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
      if context.pending(conv).any(|p| p.kind is steering) { continue }
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

**Decision R14: the native loop stays the default.** `std:default` ships when the Ratio
interpreter does, but `harness.select` stays `native` until a differential test suite (chapter
10, stage 10) shows the two harnesses produce the same requests, tool calls, events and final
replies on the recorded eval suites, and the security review has passed. Whether the default then
switches to `std:default` is the user's decision (chapter 11). Rejected: replacing the native loop
at once. The research session owns the loop's files and is changing them; a second implementation
must track them, and a regression in the default harness would hit every session.

## The Consul escalation workflow (roadmap item 64)

Item 64 asks for a workflow in which Consul takes over a subagent that keeps being rejected.
Ratio expresses it as the built-in package `std:consul-escalation`, whose full source is
[example 7](09-examples.md#7-consul-escalation-roadmap-item-64). The mapping:

| Item 64 requirement | In `std:consul-escalation` |
| --- | --- |
| A workflow, not a setting; no per-environment threshold; no off switch | A package with the threshold as a constant (`const TAKEOVER_AFTER = 2`); users opt in by selecting it for subagents (`harness.select`, an agent file, or `spawn_agent`'s `harness`), or replace it with their own package |
| Rejections: the user declines an approval; a critic or judge refuses a result; a tool or command fails | `tools.dispatch` results with `outcome: denied { by: user }`; a `think.critique` by Consul that does not pass; results with `outcome: failed` and commands with a non-zero exit |
| Counted per subagent task, within a single turn | The counter is a local of the `@task` entry's call, which is one turn of one subagent |
| Consul takes over after two rejections, continuing from the current state | The same conversation continues with `opts: { model: CONSUL }` on every later `model.chat` of the turn; nothing is restarted or summarised |
| Hands back when finished, or when unsure with a question for whoever dispatched the work | The entry returns its report; when Consul's turn ends with a question, the entry raises `Question`, which the engine routes to the dispatcher as the subagent's question |
| Consul critiques work by any model, in particular Emperor's planning | A `@turn` entry in the same package critiques the root's plan with Consul before it is executed (`think.critique` on `session.plan()`) |

The rejection counting and the handover are app work in item 64; with Ratio they are this package,
and the app work reduces to the harness switch and per-subagent selection of item 58.

## Hooks

- Every tool call a Ratio built-in makes passes through `tool.before` and `tool.after` hooks
  exactly as a model's call does, with the same matchers. Payloads gain an optional `harness`
  field (`{ package, version, harness, frame }`); a hook that blocks raises `Denied { by: hook }`
  in the program, or becomes a blocked result in `tools.dispatch`.
- `agent.spawn` and `agent.complete` fire for `agents.spawn` as for `spawn_agent`;
  `permission.requested` fires for harness approvals (`ratio_approval`).
- `model.before`, `model.after` and `compact.before` fire for Ratio's model calls and
  compactions when Imperium implements them (they are listed but not yet built,
  `docs/06-extensibility.md#hooks`).
- Two new observe-only events: `harness.call` (`agent_id`, `package`, `harness`, `frame`,
  `depth`) and `harness.return` (the same, plus `ok`, `error_kind`, `usage`). They run detached,
  like the other observe-only events, so a slow hook never delays a harness call.
- Ratio programs cannot register or run hooks, and hooks cannot call into Ratio.

## Headless evaluations

`imperium run` (`docs/03-agent-engine.md#running-evals-through-the-harness-implemented`) gains:

| Flag | Meaning |
| --- | --- |
| `--harness <name \| path.ratio>` | The root agent's harness: `native` (default), a registered package, or a file. A file is loaded as a package of its own source root, with user-source trust |
| `--subagent-harness <name \| path.ratio>` | The harness for every subagent the run starts (item 58's per-subagent selection in evals); `spawn_agent`'s own `harness` still wins unless `--force-subagent-harness` |
| `--ratio-tape <file>` | Record every effect to the file (`--ratio-tape-mode record`, the default when the file does not exist) or answer effects from it (`replay`, the default when it does; `lenient` falls through to live calls on a mismatch) |
| `--flag ratio.<limit>=<n>` | Lowers a Ratio limit for the run |

`--subagent-replay` keeps working and is consulted before the tape for subagent reports.

**Output** (schema version 2, additive): `harness` (`{ name, version, hash, source }`, or
`{ name: "native" }`), `subagent_harness`, and `ratio` with `metrics`, `scores`, `labels`,
`expectations`, `trust_sites` and `tape` (`{ mode, entries, replayed, diverged: [key] }`).
`prompt_hash` covers the system prompt the run's first model call sent and the tool schemas, as
today; `harness.hash` covers the program. Two runs compare directly only when both match.
`--print-prompt` with a Ratio harness prints `context.system_default()` as the entry would see it,
the offered tools, and the package hash; a harness that builds its prompts differently shows them
only in a run's transcript. Harness variants (TOML) keep applying to the system prompt and tool
list the engine builds, so `context.system_default()` and `tools.offered()` reflect them.

## Protocol surface

All additive, in one protocol minor version:

| Method or event | Purpose |
| --- | --- |
| `harnesses.list` | Every registered package: `{ name, version, source, path, description, entries, public: [Signature], hash, diagnostics, trust_sites }`. Read-only |
| `harnesses.check` | Parses and checks a package by name or path (inside the session's folders or a harness source); returns diagnostics with positions, the tools it may use, its trust sites and, with `test: true`, the results of its `test` blocks against the scripted host. Read-only; never runs effects |
| `session.create` `harness`, `session.control` `set_harness` | Selecting (above) |
| `SessionSummary.harness`, `AgentSummary.harness` | `{ name, version }` of what each agent runs |
| `harness_changed` | A switch, journalled |
| `harness_call_started`, `harness_call_finished` | Frames opening and closing (`agent_id`, `frame`, `harness`, `depth`; usage and outcome on finish), so clients can nest a harness's work under its caller |
| `ratio_log`, `ratio_trust`, `ratio_metric`, `ratio_span`, `ratio_event` | Logs, trust conversions, metrics, spans and custom events |
| `ratio_state`, `ratio_cache`, `ratio_effect` | Persistence: package state, session-scoped cache entries, the effect tape. Journalled; `ratio_effect` is not streamed to clients unless they ask for it |

No method installs, edits or deletes a harness; packages are files the user manages (the desktop
can open the folder). The CLI gains `imperium harness list`, `imperium harness check <name|path>`
and `imperium harness test <name|path>` over these methods, and `imperium chat` gains
`/harness <name>`. The desktop's harness picker beside the model picker is client work after
item 58.

## What changes in the engine

The engine's side of Ratio is an implementation of the interpreter's `Host` trait plus the harness
switch. Chapter 10 lists every touch point and which of them belong to files another session owns.
