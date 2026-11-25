# 10 Implementation plan

## Decision R15: one crate behind a host trait

The interpreter is a new crate, `crates/imperium-ratio`, in the Imperium workspace. It depends on
no other Imperium crate except `imperium-protocol` (for the shared tool schemas and framing
helpers; the helpers move there if they are not already, see dependencies). It defines a `Host`
trait for every effect; `imperium-engine` depends on `imperium-ratio` and implements `Host`.
Dependency direction:

```text
imperium-protocol ◀── imperium-ratio ◀── imperium-engine ◀── imperium-core ◀── imperium-cli
                         (language)       (RatioHost,         (registry, pinning,  (flags,
                                           harness switch)     BudgetGate,          commands)
                                                               protocol)
```

This keeps the language testable without a daemon (a scripted host implements the trait), keeps
the engine the only place that holds authority, and lets the research session change the engine
without touching the language.

Rejected: putting the interpreter inside `imperium-engine`. The engine is large and its agent
files are owned by another session; the language would be untestable without the whole agent.
Rejected: a separate process (chapter 01).

### The `Host` trait (sketch)

The host takes `&self`: concurrent branches hold the host at once and their effects run
concurrently (two critics' model calls really overlap). Implementations use interior mutability
(per-conversation locks, atomic ledgers, a lock-free event channel), and the order of effects on
one conversation is serialised by that conversation's lock, not by the trait.

```rust
#[async_trait]
pub trait Host: Send + Sync {
    // Models
    async fn generate(&self, frame: &FrameCtx, req: ModelRequest) -> Result<ModelResponse, HostError>;
    async fn embed(&self, frame: &FrameCtx, texts: Vec<Labelled<String>>, model: Option<ModelRef>) -> Result<Vec<Vec<f32>>, HostError>;
    fn count_tokens(&self, frame: &FrameCtx, what: Countable) -> TokenCount;
    fn resolve_model(&self, frame: &FrameCtx, m: &ModelRef) -> Result<ModelCard, HostError>;
    // Tools: every call carries the frame's scope and the arguments' secrecy flag;
    // the engine enforces both before its own checks
    async fn call_tool(&self, frame: &FrameCtx, call: ToolInvocation) -> Result<ToolOutcome, HostError>;
    // Conversations (engine-owned, append-only, any number per agent)
    fn conversation(&self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError>;
    async fn chat(&self, frame: &FrameCtx, conv: ConvId, req: ChatRequest) -> Result<ModelReply, HostError>;
    async fn safe_point(&self, frame: &FrameCtx, conv: ConvId) -> Result<SafePoint, HostError>;
    async fn dispatch(&self, frame: &FrameCtx, conv: ConvId, calls: Vec<ToolCall>, concurrent: bool) -> Result<Vec<ToolResult>, HostError>;
    fn append(&self, frame: &FrameCtx, conv: ConvId, msg: NewMessage) -> Result<(), HostError>;
    fn rollback(&self, frame: &FrameCtx, conv: ConvId, mark: MarkId) -> Result<usize, HostError>;
    async fn compact(&self, frame: &FrameCtx, conv: ConvId, opts: CompactOpts) -> Result<Compaction, HostError>;
    // Frames, agents, the session
    fn open_frame(&self, parent: &FrameCtx, req: FrameRequest) -> Result<FrameCtx, HostError>;
    fn close_frame(&self, frame: FrameCtx, outcome: FrameOutcome);
    async fn agents(&self, frame: &FrameCtx, op: AgentOp) -> Result<AgentResult, HostError>;
    async fn memory(&self, frame: &FrameCtx, op: MemoryOp) -> Result<MemoryResult, HostError>;
    async fn session(&self, frame: &FrameCtx, op: SessionOp) -> Result<SessionResult, HostError>;
    async fn approve(&self, frame: &FrameCtx, req: ApprovalRequest) -> Result<bool, HostError>;
    // Recorded, authority-free effects
    fn now(&self) -> Instant;
    fn seed(&self) -> [u8; 32];
    fn emit(&self, frame: &FrameCtx, event: RatioEvent);
    fn cancelled(&self, frame: &FrameCtx) -> bool;
}
```

Every value crossing the trait carries its secrecy labels (`Labelled<T>`: the arguments of tool calls, model requests, appends and agent operations), and the host checks them against the sinks of chapter 03 before calling the engine, whose dispatch checks them again from the per-call secrecy flag.

`FrameCtx` carries the frame path, agent id, attenuated tool set, permission mode, extra rules,
paths, location, `sees`, attended flag, authored tier and budget ledger handle. The tape is a
wrapper (`TapeHost<H>`) that records or replays every call by effect key; because keys depend on
frame paths, frames are built before the tape (stage order below).

## What it touches in Imperium

### Engine seams (owned by the research session: dependencies to coordinate)

| Seam | Today | Needed |
| --- | --- | --- |
| **Turn seam** (`agent/turn.rs`) | The turn loop is the native loop | A `TurnDriver` chosen from the agent's harness. The work before the agent (input guard for oversized inputs, attachments, slash commands, `prompt.submit` hooks, compaction on a model switch, checkpoints, the harness aids' de-reference call **charged to the turn's budget**, `turn_started`) stays in the engine and runs before either driver; the work after (`turn_completed`, conversation save, `turns.jsonl`, the entry-obligation checks of chapter 01) runs after either |
| **Multi-conversation support** (`agent/turn.rs`, agent state) | One conversation per agent (the root's, or a subagent's kept one) | Conversation handles: an agent may own several conversations (harness frames, `context.new`, forks, `fork_at`); model calls, safe points and dispatch work on any of them; rollback within the current turn |
| **Dispatch on any conversation** (`agent/tools.rs`) | `run_tool` appends to the agent's conversation | Dispatch of a model's calls on a given conversation handle, preserving call and result pairing there |
| **Structured outcome** (`agent/tools.rs`) | `run_tool` returns text for the model | A `ToolOutcome`: `ok`, `failed` (tool error or non-zero exit), `denied { by }`, `blocked { hook }`, plus the text; a `CallScope` (frame tool set, mode meet, extra deny and ask rules, paths, location, secrecy flag) checked before every other check; tool-call provenance (`harness` field) for hooks and events |
| **Compaction** (`agent/compaction.rs`, `compact.rs`) | Compacts the root conversation (and subagents' at resume) | Callable for any engine-owned conversation, with a `focus` line, carrying pinned ranges verbatim like kept context |
| **Prompts** (`agent/prompts.rs`) | Assembles the system prompts | `system_default(role)` as a function; the framing helpers (`HARNESS_TAGS` defusing) callable for prompt sections; the per-spawn `prompt` framed when placed in a subagent's system prompt |
| **Model requests** (`imperium-models` `chat.rs`) | Chat Completions with tools and effort | `response_format` / JSON Schema, llama.cpp `json_schema`, vLLM guided JSON; sampling (`top_k`, `min_p`, `seed`, `stop`, penalties, `n`, `logprobs`); `tool_choice`; image parts; a completions route; capability flags per endpoint |
| **Memory** (`imperium-memory`) | Search, read, write tools; automatic recall | Recall with caller-supplied text; `private` entries (for `Secret`); proposals when `memory.approve` lands |
| **Eval runs** (`imperium-core` `eval*`) | `imperium run` with variants and subagent replay | `--harness`, `--subagent-harness`, `--ratio-tape`, output schema v2, `ratio.trust` in runs |

### Other Imperium changes

| Area | Change | Owner |
| --- | --- | --- |
| `crates/imperium-ratio` (new) | The language: token reader, parser, checker, IR, interpreter, standard library, tape, scripted host, built-in packages, conformance suite | This work |
| `imperium-engine` `agent/ratio_host.rs` (new) | `RatioHost`: `Host` over the agent's chat clients, conversations, dispatch, compactor, agent records and events | This work |
| `imperium-engine` agent state and delegation | Harness agents (ephemeral records, ids, slot holders, Agents panel rows); **per-task budgets** carved from the spawning frame; **cancelling a running subagent** (today only paused ones can be cancelled); `spawn_agent` `harness`, `prompt`, `workflow` | This work, reviewed with the research session |
| `imperium-core` budgets | `BudgetGate` lives in `imperium-core`: frame ledgers debit it through a handle the daemon gives the engine; per-task sub-ledgers | This work, reviewed with the research session |
| `imperium-policy` | Permission-mode `meet`; the **safety floor extended to file-tool paths** for harness source folders and authored areas (today it matches command lines and fixed secret files); the pseudo-tools' defaults; secrecy refusals at dispatch | This work |
| `imperium-extensions` (plugins) | A plugin with `harnesses/` counts as code-running: withheld until approved through the existing challenge, approval pinned by content hash; agent files from plugins and projects restricted to their own source or `harness.offered` | This work |
| `imperium-automation` | Automation prompts (auto-fix, schedules, project agents) carry embedded CI logs, comments and issue bodies as structured payloads, not inside the message text | This work, reviewed with the automation owner |
| `imperium-web` | A model-facing browser screenshot (`browser_screenshot`) returning an image part | This work |
| `imperium-protocol` | Events, methods, tools `write_harness` and `run_harness`, `SessionSummary.harness`, `spawn_agent` schema fields (status `planned` until built), a protocol minor bump; the Emperor repository is told of the schema changes | This work |
| `imperium-config` | `[harness] select`, `offered`; `[ratio]` | This work |
| `imperium-core` (daemon) | Harness registry, source-qualified names, loading, pinning by hash, authored areas, `harnesses.list`, `check`, `source`, `replay`, `set_harness`, journal replay of `ratio_*` events | This work |
| Hooks (`imperium-extensions`) | `harness.call`, `harness.return`, `harness.authored`; `harness` field in tool payloads | This work |
| `imperium-cli` | `imperium harness list|check|test|replay`, `/harness` in `imperium chat` | This work |
| Desktop (`apps/desktop`) | Harness picker, Agents panel rows for harness agents, authored harness list and approval, live harness view | This work (client stages) |
| Imperium docs | `docs/03`, `05`, `06`, `10`, `12` (items 58, 64 and 65 status), a new `docs/2x-ratio.md` pointing at the pinned specification, the protocol reference | This work, with each stage |

## Where the specification lives (Q12)

The specification stays in this repository (`Densetsu152637/Ratio`), which versions the language
independently of Imperium. Imperium references it at a **pinned revision**: a git submodule at
`third_party/ratio` (or, if the workspace avoids submodules, a `ratio-spec.lock` file naming the
commit, checked out by CI). From the pinned revision a script generates
`crates/imperium-ratio/conformance/builtins.txt` (every built-in of chapter 06 with its signature
and Needs column) and checks it in; CI regenerates it and fails when the checked-in file differs,
the same drift check as the tool schemas. Every implementation pull request in Imperium names the
spec revision it implements; a language change is a pull request here first, then a pin bump and
implementation there.

## Stages

Each stage is one pull request of about 800 changed lines (code and tests; documentation on top)
against `experimental`, behind `ratio.enabled = false` until the security review. Every stage
keeps `cargo fmt`, the workspace's checks and the drift checks green **in CI**: no stage depends
on a local build. Each stage's documentation updates ship in the same pull request.

| # | Stage | Delivers | Tests (all in CI) |
| --- | --- | --- | --- |
| 1 | Token reader | Lexer with the mode stack (code, strings, nested interpolations), quantity, duration and money literals, contextual keywords, semicolon insertion by innermost delimiter | Token corpus: every literal form, nested interpolations, insertion in every delimiter kind, line-start `with` and `else` |
| 2 | Parser | The whole grammar; AST with spans; diagnostics with file, line, column and a caret; error recovery; nesting limits; a canonical printer | Parse corpus for every production and disambiguation rule; printer round trip as a property test; nesting-limit tests |
| 3 | Modules and names | Source roots, imports by path (no escape, no links), source-qualified names with areas (`authored:session/x`) and trust order, the registry trait, name resolution, prelude | Resolution corpus; shadowing refused across trust levels; import-trust rules per source |
| 4 | Type checker | Records, enums, options, tuples, generics on functions, subtyping (`Never`, `UserText`, `Brief`, `TrustedPrompt`), contextual variants, variant literals, exhaustiveness, turbofish, constant evaluation rules | `conformance/types/`: accepted and rejected programs with diagnostic codes |
| 5 | Schemas and taint | Schema derivation and type digests; `Untrusted` mapping, abstract generics, sinks, `Brief` and `TrustedPrompt` inference; monomorphic `trust` and `as_task`; transitive trust sites; guarded-effect lint; static `Secret<T>` where the label is known; `harnesses.check` as a library call | One rejected program per sink and one accepted per allowed flow; a property test that generated programs never produce a `TrustedPrompt` or `task` section from untrusted sources; schema and digest snapshots |
| 6 | IR and core interpreter | Lowering with site ids; canonical serialisation and IR hash; the evaluator for the pure language with **run-time secrecy labels** on values and their propagation (including through `Json`); steps, heap and call-depth accounting; `Cancelled` and `Budget` rules; branch restrictions | `conformance/eval/`; hash stability snapshots across platforms; label propagation tests for every operator |
| 7 | Data I | `text`, `re` | One conformance test per built-in, with each row's failures and label propagation |
| 8 | Data II | `json`, `list`, `map` | As stage 7 |
| 9 | Data III | `math`, `hash`, `diff`, `template`, `vec`, `path`, `url`, `codec` | As stage 7; float determinism with fixed bit patterns |
| 10 | Frames | Frame model and frame paths; configuration evaluation and merge; attenuation (tools, mode meet, rules, paths, location, budgets, `sees`, depth, width, attended); the budget ledger; context modes; `Question` crossing frames; `meta` | Attenuation monotonicity property; ledger tests; `sees` refusals, including labelled values entering frames without `secrets` |
| 11 | Host, tape and concurrency | The `&self` `Host` trait with `Labelled<T>` values and host-side sink checks; `ScriptedHost`; `TapeHost` with effect keys built from frame paths; scheduling choices on the tape; `par`, `par_map`, `race`, `timeout`, cancellation; `rand`, `time`; `control`; `state` | Replay determinism property under random completion orders; record-then-replay gives identical event streams; divergence tests; real concurrency; every secrecy sink refuses a labelled value at the host |
| 12 | Model and think | Prompt rendering with the engine's framing rules (vendored test vectors until stage 13); `model.*` and `think.*`; structured generation and repair; built-in prompt texts in the hash | Scripted-host tests per built-in; prompt rendering snapshots |
| 13 | Engine host Ia: conversations and models | `RatioHost` model calls; multi-conversation support with handles, forks and rollback; safe points; harness agents and their events; conversation labelling and its journal event. **Needs the turn, multi-conversation, prompts and model-request seams** | Engine tests with the scripted OpenAI-compatible endpoint of `crates/imperium-cli/tests/headless_eval.rs`, extended for `response_format`, `tool_choice`, image parts and logprobs |
| 14 | Engine host Ib: dispatch | Dispatch on any conversation with `CallScope`, structured outcomes, `tools.refuse`, compaction calls (local-only for labelled conversations), secrecy checks at dispatch from the per-call flag. **Needs the dispatch, outcome and compaction seams** | One test per permission path, each the twin of a native-loop test; dispatch refuses labelled arguments even when the host is bypassed |
| 15 | Engine host II: agents | `agents.*` gated by tool names; per-task budgets carved from frames; cancelling running subagents; writer worktrees and `agent_merge`; secrecy on briefs and questions | Delegation limits and rules refuse as the engine does; budget carving; cancel tests |
| 16 | Engine host IIIa: memory and session | `memory.*` (private entries as a secret source), `session.*` with pseudo-tools, `ratio_approval`, `declassify`, checkpoints, branches, rewinds; the command rule for secret paths (floor-style parsing of command lines) | One engine test per built-in group; pseudo-tool rules apply |
| 17 | Engine host IIIb: context and tools | `context.*` extras (pins, artefacts, chunks), every `tools.*` built-in including `browser_screenshot`, `eval.run_tests` | One engine test per built-in group |
| 18 | Harness switch and surfaces | Registry and loading in the daemon; pinning by hash and re-selection; `[harness]` and `[ratio]` settings; `session.create` and `set_harness`; agent file and `spawn_agent` fields; turn payloads from automation; protocol methods and events with **redaction at the daemon boundary**: the per-session HMAC key, `[secret: <digest>]` substitution in every outgoing event and result, `session.subscribe` `secrets: true` and `declassify_conversation` only through the desktop app's single-use challenge and native dialog (never the CLI, never in unattended sessions, never to paired phones); labels on engine state (plan, aids' state, titles including auto-titles, authored source) journalled, carried and redacted; hooks; CLI | Protocol tests; a changed file does not take effect until re-selected; the model cannot name a package outside `harness.offered`; no labelled value leaves the daemon unredacted to a client that did not ask, never to a paired phone, never in an unattended session; the HMAC key never leaves the daemon; **an agent's command that runs the `imperium` CLI (or connects as any client) to declassify or to subscribe with `secrets: true` is refused**; a labelled plan or title is redacted in every client and phone view |
| 19 | Headless evaluation | `--harness`, `--subagent-harness`, tape files with secret redaction, output schema v2. **Needs the eval-runs seam** | Headless runs on the scripted endpoint; tape record and replay through the CLI |
| 20 | Floor and plugin gates | Safety floor on file-tool paths for harness folders and authored areas; plugin-with-harnesses withheld and hash-pinned; agent-file source restriction; `ratio.trusted_projects` | A write to a harness folder asks every time in every mode; an unapproved plugin's harnesses do not load; an update is withheld again |
| 21 | Authored and offered harnesses (engine) | `write_harness`, `run_harness` (deep-untrusted arguments and the not-offerable rule for every source), `meta.author`; areas and qualified names; closure hashes; the authored rules (no trust sites, untrusted parameters for `run_harness`, results forced untrusted); contained tier with the pinned model and provider-side tools off; caller-tier approvals keyed by closure hash and scoped to authority; journal events | A contained harness cannot write, run commands, reach the network, use provider-side tools or see secrets; a route that cannot disable provider tools is refused; a new closure hash or a wider authority asks again; self-use never exceeds the caller |
| 22 | Security-relevant clients | The desktop's approval dialogs (authored caller tier with the whole closure, diffs and summary, untrusted rendering; persistence; plugin-harness approval), the clients' handling of redacted values (the daemon side is stage 18), the labelled-conversation banner with its three recovery paths, and the **live harness view** (`harnesses.source`, `harness_site` subscription and throttling, replay stepping) | Client tests against recorded event streams: authored source never rendered as markdown or links; redacted values stay redacted; throttle tests |
| 23 | Built-in packages and parity | `std:default`, `std:consul-escalation`, `std:plan-execute-verify`, `std:self-consistency`, `std:tdd-fix`; the examples as conformance programs; the **parity suite** as defined in chapter 08, run with the default configuration | The parity suite; item 64 scenarios (each kind of rejection, per task, within a turn; takeover continues the conversation; independent critic after takeover; plan critique before execution when `update_plan` is alone; a question reaches the dispatcher) |
| 24 | Security review and hardening | The independent review (chapter 07's gate), **after** stages 13 to 23, so it covers the engine bindings, the gates, authored harnesses, the dialogs, the live view and redaction; fixes; limit tests; parser and checker robustness properties in normal CI, `cargo fuzz` on the nightly schedule; default limits set from measurements of the examples; `ratio.enabled` may default to true only after this stage | The review's regression tests, limit tests, robustness properties |
| 25 | Remaining clients | The harness picker, Agents panel rows for harness agents, the authored list | Client tests against recorded event streams |
| 26 | Default switch | After one release cycle of opt-in use with the parity suite green, `harness.select` defaults to `std:default` (the user's decision, Q1); the native loop stays selectable for one more release | The parity suite on every pull request |
| 27 (optional) | Resume by replay | After a daemon restart mid-turn, re-run the entry from the journal's tape to rebuild the interpreter state, then continue live | Restart tests: the resumed turn makes no duplicate tool calls |
| 28 (pending Q9) | Harnesses from code mode | Option B of chapter 08 if the user chooses it, after code mode's own security review | As specified with the decision |

## The conformance suite

`crates/imperium-ratio/conformance/` holds `.ratio` files grouped by chapter (`syntax`, `types`,
`taint`, `secrecy`, `eval`, `stdlib/<module>`, `frames`, `replay`, `authored`, `examples`). Each
file starts with expectation lines:

```ratio
// expect: ok                                   (checks and runs; value printed as JSON)
// expect: value {"winner": "42", "agreement": 0.6}
// expect: check-error E0412 at 7:13            (a checker diagnostic code and position)
// expect: runtime-error SchemaMismatch
// fixture: model replies.json                  (scripted model replies for this file)
```

A single test runner (`tests/conformance.rs`) runs them all with the scripted host. Three coverage
checks keep the suite honest:

- `builtins.txt` (generated from the pinned specification, above) lists every built-in. The
  runner fails if a listed built-in has no conformance file using it, or if the interpreter
  registers a built-in the list does not have, or if a registered signature differs from the
  listed one.
- **The effect rule check.** For every built-in whose Needs column is not `pure`, the runner
  derives its declared return type and fails if any string position in it is trusted, unless the
  specification marks that position as an engine-generated identifier (chapter 03). This catches a
  built-in that returns model- or tool-written text as `Str`.
- Every diagnostic code the checker can emit has at least one `check-error` file.
- **Secrecy invariants.** Property tests over random sequences of appends, tool results, model
  replies, `clear_stale`, compactions, rollbacks, forks and branches check that no unlabelled
  message follows a labelled message it was produced after, that labelled engine state (plan,
  aids' state, title, authored source) stays labelled through forks, branches, kept-context blocks
  and compaction, and that re-injecting it labels the receiving conversation.

## Testing principles

- **Entirely in CI.** Unit, property, conformance, engine, parity and client tests run in the
  workspace's CI jobs; nothing needs a local model, a GPU, a network, or a local build.
- **A fake model, never a real one.** The scripted host (inside the crate) and the scripted
  OpenAI-compatible endpoint (for engine tests) answer from fixtures; they also check the
  requests they receive, so a test fails when a prompt or schema changes unexpectedly.
- **Twins of existing tests.** Every permission path the native loop is tested on gets a Ratio
  twin, so attenuation and dispatch cannot drift from the engine.
- **Determinism is tested, not assumed.** Replay and completion-order properties run on every
  pull request.

## Rollout

1. Stages 1 to 12 land with nothing selectable (`ratio.enabled = false`, no switch).
2. Stages 13 to 23 make Ratio selectable in headless runs and, with `ratio.enabled = true`, in
   sessions; documentation marks it experimental. Stage 23 ships the built-in packages, so item 64
   becomes usable by selecting `std:consul-escalation`.
3. Stage 24's review decides whether `ratio.enabled` defaults to true.
4. Stage 26 switches the default harness to `std:default` after a release cycle of parity.

Rollback at any point: set `ratio.enabled = false`; sessions using a package fall back to
`native` at their next turn with a notice.
