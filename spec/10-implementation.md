# 10 Implementation plan

## Decision R15: a language repository, and Imperium as a host

Ratio is built as a language in its own repository (`Densetsu152637/Ratio`), with its own
Cargo workspace, toolchain, CI and releases. Imperium does not contain the interpreter; it
depends on the released `ratio` library and implements the `Host` trait against its engine, as
any other host could. This is the user's decision: Ratio is a language with its own build chain,
and Imperium uses the engine it provides to run harnesses.

**Crates** (in `crates/` of this repository):

| Crate (package) | Library | Contents |
| --- | --- | --- |
| `ratio-lang` | `ratio` | The public facade: everything a host uses, re-exported from the crates below. The only crate Imperium depends on |
| `ratio-syntax` | `ratio_syntax` | The token reader (stage 1) and, from stage 2, the parser, AST and canonical printer |
| Later stages | | More crates as the stages land (for example a checker, the IR and interpreter, the standard library); each is an implementation detail behind the facade |

The built-in packages' sources (`std:default`, `std:consul_escalation` and the rest) live in
`std/` at the root of this repository, one folder per package, and the facade embeds them in the
library at build time, so a host gets exactly the `std` of the release it depends on. The folder
arrives with stage 23.

The facade's package is `ratio-lang` because a bare `ratio` may be taken on crates.io; its library
is `ratio`, so code reads `ratio::lex`, `ratio::Host`. The crates depend on no Imperium crate
(not even `imperium-protocol`): tool schemas reach a program at run time through its frame
(`FrameCtx`), and the prompt framing rules of chapter 05 are implemented here and checked against
the engine's by shared test vectors (below).

Dependency direction, across the two repositories:

```text
  Ratio repository                         │  Imperium repository
                                           │
 ratio-syntax ◀── … ◀── ratio-lang ◀───────┼── imperium-engine ◀── imperium-core ◀── imperium-cli
                        (facade: Host,     │   (RatioHost,          (registry, pinning,  (flags,
                         check, run, tape) │    harness switch)      BudgetGate,          commands)
                                           │                         protocol)
```

This keeps the language testable without a daemon (a scripted host implements the trait), keeps
the engine the only place that holds authority, versions the language with its specification,
and lets the engine change without touching the language.

Rejected: a crate inside the Imperium workspace (the plan of earlier drafts, `imperium-ratio`,
where stage 1 first landed as Imperium pull request 314). The specification and its
implementation would live in different repositories and need a pinned revision to stay in step,
and the language could not be built, tested or released without the whole agent. Rejected:
putting the interpreter inside `imperium-engine` (untestable without the agent). Rejected: a
separate process (chapter 01).

### The `Host` trait (sketch)

The host takes `&self`: concurrent branches hold the host at once and their effects run
concurrently (two critics' model calls really overlap). Implementations use interior mutability
(per-conversation locks, atomic ledgers, a lock-free event channel), and the order of effects on
one conversation is serialised by that conversation's lock, not by the trait.

```rust
#[async_trait]
pub trait Host: Send + Sync {
    // Every async method takes the effect's context (`EffectCtx`, below)
    // Models
    async fn generate(&self, frame: &FrameCtx, cx: &EffectCtx, req: ModelRequest) -> Result<ModelResponse, HostError>;
    async fn embed(&self, frame: &FrameCtx, cx: &EffectCtx, texts: Vec<Labelled<String>>, model: Option<ModelRef>) -> Result<Vec<Vec<f32>>, HostError>;
    fn count_tokens(&self, frame: &FrameCtx, what: Countable) -> TokenCount;
    fn resolve_model(&self, frame: &FrameCtx, m: &ModelRef) -> Result<ModelCard, HostError>;
    // Tools: every call carries the frame's scope and the arguments' secrecy flag;
    // the engine enforces both before its own checks
    async fn call_tool(&self, frame: &FrameCtx, cx: &EffectCtx, call: ToolInvocation) -> Result<ToolOutcome, HostError>;
    fn tool_class(&self, frame: &FrameCtx, tool: &ToolName) -> ToolClass;
    // Conversations (engine-owned, append-only, any number per agent)
    fn conversation(&self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError>;
    fn conversation_digest(&self, frame: &FrameCtx, conv: ConvId) -> Result<Digest, HostError>;
    async fn chat(&self, frame: &FrameCtx, cx: &EffectCtx, conv: ConvId, req: ChatRequest) -> Result<ModelReply, HostError>;
    async fn safe_point(&self, frame: &FrameCtx, cx: &EffectCtx, conv: ConvId) -> Result<SafePoint, HostError>;
    async fn dispatch(&self, frame: &FrameCtx, cx: &EffectCtx, conv: ConvId, calls: Vec<ToolCall>, concurrent: bool) -> Result<Vec<ToolResult>, HostError>;
    fn append(&self, frame: &FrameCtx, conv: ConvId, msg: NewMessage) -> Result<(), HostError>;
    fn rollback(&self, frame: &FrameCtx, conv: ConvId, mark: MarkId) -> Result<usize, HostError>;
    async fn compact(&self, frame: &FrameCtx, cx: &EffectCtx, conv: ConvId, opts: CompactOpts) -> Result<Compaction, HostError>;
    // Frames, agents, the session
    fn open_frame(&self, parent: &FrameCtx, req: FrameRequest) -> Result<FrameCtx, HostError>;
    fn close_frame(&self, frame: FrameCtx, outcome: FrameOutcome);
    async fn agents(&self, frame: &FrameCtx, cx: &EffectCtx, op: AgentOp) -> Result<AgentResult, HostError>;
    async fn memory(&self, frame: &FrameCtx, cx: &EffectCtx, op: MemoryOp) -> Result<MemoryResult, HostError>;
    async fn session(&self, frame: &FrameCtx, cx: &EffectCtx, op: SessionOp) -> Result<SessionResult, HostError>;
    async fn approve(&self, frame: &FrameCtx, cx: &EffectCtx, req: ApprovalRequest) -> Result<bool, HostError>;
    // Recorded, authority-free effects
    fn now(&self) -> Instant;
    fn seed(&self) -> [u8; 32];
    fn emit(&self, frame: &FrameCtx, event: RatioEvent);
    fn cancelled(&self, frame: &FrameCtx) -> bool;
}
```

Every value crossing the trait carries its secrecy labels (`Labelled<T>`: the arguments of tool calls, model requests, appends and agent operations). They are checked against the sinks of chapter 03 twice, once on each side of the boundary: in Ratio by a guard the interpreter wraps around every host (no host can opt out of it), and in Imperium by the engine's dispatch, from the per-call secrecy flag.

`FrameCtx` carries the frame path (the key by which a host finds its own record of the frame), agent id, attenuated tool set, permission mode, extra rules,
paths, location, `sees`, attended flag, authored tier and budget ledger handle. The tape is a
wrapper (`TapeHost<H>`) that records or replays every input-bearing call by effect key and numbers
the turn's scheduling events (chapter 01); because keys depend on frame paths, frames are built
before the tape (stage order below).

**Effects, cancellation and deadlines.** `EffectCtx` carries the effect's key, a cancellation
token and its **deadline** (chapter 03: the earliest enclosing `with timeout` deadline and the end
of the frame's `wall` budget). A host bounds every call by the deadline. `tool_class` returns a
tool's `ToolClass`: its cancellation class (`abort` or `drain`, chapter 06, `tools`) and, for a
`drain`-class tool, its declared maximum running time, at most 3600 s. When the token fires, the
host ends the call by its class and reports how it ended: `aborted` (acknowledged), `abandoned`
(an `abort` call that had reached a remote server), `drained` (it ran to the end; its result is
discarded) or `withdrawn` (it was still waiting for an approval or a hook, and never ran); the
interpreter records each outcome on the turn sequence. Withdrawal is atomic with the user's
answer. These are **host-contract requirements**: every `drain`-class call must finish within its
bound once started, or the drain fence of chapter 03 could wait without end. `count_tokens` and
the `ModelCard` from `resolve_model` (its maximum output, window and prices) are the inputs of
admission by reservation (chapter 04), and are recorded with each admission. `conversation_digest`
returns the head of a conversation's chained digest (chapter 01). The sketch is not yet fixed:
stage 11a ships the first `HOST_API_VERSION`, after the `RatioHost` spike.

## What it touches in Imperium

Everything in this section is Imperium-side work: it lives in the Imperium repository and is done
there, against a released version of `ratio-lang` (see the repository split below).

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
| Workspace `Cargo.toml` | A dependency on `ratio-lang` at a release tag of the Ratio repository (below); the earlier `crates/imperium-ratio` is removed in favour of it | This work |
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
| `imperium-cli` | `imperium harness list`, `check`, `test` and `replay`; `/harness` in `imperium chat` | This work |
| Desktop (`apps/desktop`) | Harness picker, Agents panel rows for harness agents, authored harness list and approval, live harness view | This work (client stages) |
| Imperium docs | `docs/03`, `05`, `06`, `10`, `12` (items 58, 64 and 65 status), a new `docs/2x-ratio.md` pointing at the specification of the Ratio release in use, the protocol reference | This work, with each stage |

## Where the specification lives (Q12)

The specification lives in this repository (`spec/`), next to the implementation, so the two
change together: a language change is one pull request that edits the chapter, the code and the
conformance files. There is no pinned specification revision. Instead the implementation checks
that it agrees with the text beside it:

- `ratio::SPEC_DRAFT` must equal the draft on the version line of `spec/README.md`;
- every conformance directory names, in `conformance/spec-map.txt`, the chapters and headings it
  tests, and CI fails when one of them does not exist;
- from stage 7, a script generates `conformance/builtins.txt` (every built-in of chapter 06 with
  its signature, its Needs column and, for pure data built-ins, its cost class) and CI fails when
  the checked-in file differs.

Imperium pins the language by pinning a release of this repository (below); the specification of
that release is the one it implements.

## The repository split and the boundary

### Who owns what

| Ratio repository | Imperium repository |
| --- | --- |
| The language: token reader, parser, modules and names, checker, schemas and taint, IR and its hash, interpreter | `RatioHost`: the `Host` trait implemented against the engine seams (conversations, models, dispatch, agents, memory, session, context, tools) |
| The standard library's built-ins (chapter 06) and their behaviour against the `Host` trait | The daemon's harness registry, source roots and their trust, pinning by hash, switching, the `[harness]` and `[ratio]` settings |
| Frames, attenuation, budget ledgers, `sees` | Gates and floors: the safety floor on harness folders, plugin approval, agent-file restrictions, trusted projects |
| The tape (`TapeHost`), scheduling choices, structured concurrency, cancellation | The protocol: methods, events, redaction at the daemon boundary, challenges |
| The `Host` trait and every type that crosses it | The desktop: approval dialogs, harness picker, Agents panel rows, live harness view |
| The test hosts, `ScriptedHost` and `TapeHost`, and the host contract tests | Headless evaluation (`imperium run --harness`) and the parity suite |
| The conformance suite, `builtins.txt` and its drift check, the framing test vectors | Engine tests with the scripted OpenAI-compatible endpoint |
| The built-in packages' source (`std:default`, `std:consul_escalation`, and the rest), in `std/` and embedded in the library | Selecting them, and the parity run that decides the default switch |
| The specification | Imperium's own documentation of the integration (`docs/2x-ratio.md`) |

### The public API

The `ratio` library (package `ratio-lang`) is Ratio's only public API. A host uses it to load and
check packages through a source registry trait the host implements, to run an entry (`@turn`,
`@task`) with a `Host`, to record and replay tapes, and to read IR hashes, closure hashes, source
maps and diagnostics. Everything else, including the crates behind the facade, is internal and
may change in any release.

### Versions

- **Semantic versioning** of `ratio-lang`. Before 1.0, a minor bump (`0.x` to `0.x+1`) may break
  the API and a patch never does. 1.0 follows the stage 24 security review.
- **`ratio::HOST_API_VERSION`** (an integer, from stage 11a) changes whenever the `Host` trait or
  a type crossing it changes. `RatioHost` asserts the version it was written for at compile time,
  so a bump that Imperium has not adapted to fails to build rather than misbehaving.
- **`ratio::IR_VERSION`** (an integer, from stage 6) changes whenever the canonical IR
  serialisation, the IR hash or the effect keys (frame and branch paths included) change. Harness
  hashes from another IR version are not comparable. The prepared form (chapter 01) is not
  covered: it is never serialised or hashed.
- **`ratio::TAPE_VERSION`** (an integer, from stage 11a) changes whenever the tape's file format
  changes (its header, entry encoding or label encoding). The first version covers the format of
  chapter 01: entries for input-bearing effects only, the numbered turn-sequence events, blob
  lines named by BLAKE3 digest, the end-of-turn reply digest, `full` mode's digest-only entries
  and labels stored with every value or blob reference. Compression (`.jsonl.zst`) is a property
  of the file, not a format version. It is separate from `IR_VERSION`: a
  tape is replayable only when both match, because its keys come from the IR and its encoding
  from the tape format.
- **What Imperium records.** Every pin (`harness_changed`) records the `ratio-lang` version and
  `IR_VERSION` with the hashes, and every tape's header records the `ratio-lang` version,
  `IR_VERSION` and `TAPE_VERSION`. After a bump that changes `IR_VERSION`, every pinned package
  must be re-selected. A pinned `std:` package whose hash the new release no longer ships (its
  source changed, or the IR did) is a **pin miss**: at the session's next turn the user gets a
  notice and the session runs `native` until they re-select, which shows the diff as any
  re-selection does. Replay refuses a tape whose `IR_VERSION` or `TAPE_VERSION` differs from the
  running library's, naming both.
- **A version notice for user and plugin pins.** A pinned `user:` or `plugin:` package whose
  recorded `ratio-lang` version differs from the running one (with the same `IR_VERSION`, so the
  pin still holds) keeps running, and each session using it gets one notice naming the package,
  the recorded version and the running one, so the user knows the package now runs on a library
  it was not selected with and can re-check or re-select it. The notice is per session, at the
  first turn that runs the package, and never repeats within that session.
- `ratio::SPEC_DRAFT` names the specification draft and `ratio::STAGE` the stage reached.

### How Imperium pins Ratio

- Releases are tags `vX.Y.Z` on `main`, made only from a commit whose CI is green. The release
  workflow (`.github/workflows/release.yml`) runs on every pushed tag and fails, publishing
  nothing, unless the tagged commit is on `main`, its CI run succeeded, and the workspace version
  equals the tag; otherwise it creates the GitHub release. Only that workflow creates a release:
  the repository ruleset protects `v*` tags (only the maintainer may create them, and nobody may
  move or delete them), so a release cannot be made for a tag by another route. The ruleset is a
  repository setting the maintainer creates, not code (`CONTRIBUTING.md`, "Releases").
- The first release, `v0.1.0`, is tagged after the workspace lands on `main`.
- Imperium depends on a tag: `ratio-lang = { git = "https://github.com/Densetsu152637/Ratio",
  tag = "vX.Y.Z" }`, and its `Cargo.lock` records the commit. Publishing to crates.io, and
  reserving the crate names there, is deferred: it needs the user's account. Until then the
  crates are `publish = false`.
- A version bump in Imperium is its own pull request naming the tag and the stages it brings.
  Before tagging a release that changes `HOST_API_VERSION`, `IR_VERSION` or `TAPE_VERSION`, the
  Imperium bump is opened as a **draft** pull request against the candidate commit (`rev = "…"`)
  and must pass Imperium's CI; it switches to the tag once the tag exists.
- **Local development across both repositories** uses a Cargo patch in Imperium's workspace,
  `[patch."https://github.com/Densetsu152637/Ratio"] ratio-lang = { path = "…" }`, in a local
  commit that is never merged.
- **Imperium's CI checks the source.** A check in Imperium's CI fails when `ratio-lang` resolves
  from anything but a `tag` of this repository: a `path`, a `[patch]` entry or a `branch` fails
  always, and a `rev` fails on any pull request that is not a draft. A tag alone is not enough:
  the check also requires a **GitHub release** for that tag, which only the release workflow
  creates, and only for a commit on `main` whose CI passed, so a tag pushed by any other route
  (or a release that failed its checks) never reaches Imperium's build.
- A language change never starts in Imperium: it is a pull request here first, then a release,
  then a bump there.

### Where cross-repository tests live

- **Here:** everything that needs only the language: unit, property and conformance tests, the
  `ScriptedHost` and `TapeHost` tests, replay determinism, secrecy invariants at the guard, and
  the **host contract tests**, a suite exported by the facade (behind a `host-tests` feature)
  that is generic over any `Host` and checks what every host must do: refuse a child frame whose
  scope is not within its parent's (each dimension of the attenuation row below), refuse labelled
  values at every sink, preserve per-conversation order, honour cancellation, and answer recorded
  effects identically under the tape. It runs here against `ScriptedHost`.
  - **Cancellation cases.** An `abort` call is acknowledged within a bound and its scope returns;
    a remote `abort` call is `abandoned`; every `drain`-class tool in the host's table declares a
    bound of at most 3600 s, and a scripted never-ending drain call is failed at it, after which
    the fence releases; `race` returns before a losing `drain` write finishes, while the enclosing
    harness call, later writes in the frame tree and the turn wait for it; a nested harness frame
    inside a cancelled scope closes at once and its write is re-parented; `defer` effects and
    `tools.command_stop` are not fenced; a `run_command` not classified read-only is not aborted
    by a `race` and is killed when an enclosing `with timeout` fires; an unlisted tool, and an MCP
    tool whose server claims read-only without user configuration, are `drain`; an
    approval-pending call is withdrawn and never runs, and an answer after the withdrawal changes
    no session approval and records no denial; `session.control stop` drains every in-flight
    call; the host receives every deadline.
  - **A host-supplied fixture.** The suite cannot know a host's internals, so the host provides
    a fixture: a constructor for a fresh host in a known state, the names of its sink tools (at
    least one network tool, one command tool and one write tool), and a scripted model endpoint
    the host's model calls reach.
  - **Unguarded sink cases.** The sink and frame cases drive the host directly, through the raw
    `Host` trait without Ratio's guard, so they test the host's own checks; a host that relied on
    the guard would fail them.
- **Here, shared:** the prompt framing vectors (`conformance/framing/`, from stage 12), inputs
  and expected rendered text for every framing rule of chapter 05.
- **In Imperium:** the host contract tests run against `RatioHost`; the framing vectors run
  against the engine's own framing helpers, so the native loop and Ratio frame text identically;
  the engine, daemon, protocol, client, headless-eval and parity tests.

### Security requirements by side

The requirements of chapters 03 and 07 are unchanged; each has an owner. Attenuation and the
other authority checks are enforced on both sides, and the engine's check is the boundary
(chapter 07, T3): a Ratio bug there is caught in Imperium. Secrecy is different. The engine
checks sinks from the labels the interpreter reports, so its check catches a bug in Ratio's guard
but not a bug in **label propagation**: a value whose label the interpreter dropped reaches the
engine unlabelled. Propagation is Ratio's alone, a residual risk covered by the propagation
tests of stages 6 and 11 and the secrecy invariants, and explicitly in the scope of the stage 24
review.

| Requirement | Ratio | Imperium |
| --- | --- | --- |
| No `eval`, no ambient authority; every effect through the `Host` | Owns: the interpreter has no other path to the outside world | Implements the trait; holds all authority |
| Memory safety of the interpreter | Owns: safe Rust, `unsafe_code = "forbid"` in every crate | — |
| Taint typing, sinks, `Brief`, `TrustedPrompt`, monomorphic `trust` and `as_task`, the effect rule for built-ins | Owns: the checker and the conformance suite | Applies `ratio.trust` at selection |
| Secrecy labels on values and their propagation | Owns, alone: no Imperium check can restore a dropped label (residual risk, stage 24 scope) | Labels conversations and engine state; journals labels |
| Secrecy sinks | Enforces at the guard, from the labels | Enforces at dispatch from the per-call flag, which comes from the same labels: it catches a guard bug, not a propagation bug; owns redaction, the per-session HMAC key, and the declassify and `secrets: true` challenges |
| Tape secrecy and replay trust | `TapeHost` records every value's labels and restores them on replay, adding any label the replaying session's source rules set (never removing one); replayed effect results are untrusted exactly as live ones are; effect keys carry no values; replay verifies the turn sequence against the tape alone (ledgers, step totals, cache waits, cancellation outcomes) and every blob against its digest | Redacts tape files (`--ratio-tape`) like events; journals `ratio_effect` and redacts it when streamed; redacts labelled values and the `harness` field's values in hook payloads |
| Attenuation (tools, modes, rules, paths, location, budgets, `sees`, depth) | Computes every frame's scope as the meet of its caller's and its request | **The boundary**: `RatioHost::open_frame` verifies independently that the child scope is within the parent's in every dimension (tool set, mode meet, rules, paths, location, budgets, `sees`, depth) and refuses otherwise. It checks against **Imperium's own record** of the parent's scope, looked up by the parent's frame path, never against the scope in the `FrameCtx` that Ratio passes in, so a Ratio bug that widens a `FrameCtx` cannot widen a child. Dispatch enforces the scope (`CallScope`) before its own checks |
| Extension installs (`tools.install_extension`) | No other path: a program can install only through this built-in, and no frame, `with` override or `ratio_approval` can widen or pre-approve it | Owns: the dispatch floor (every install from a program is agent-origin and asks the user per install, in every mode, never remembered, not skipped by allow rules, auto mode or `ratio_approval`; refused in unattended sessions) and load-time withholding (an agent-origin extension stays off until the user approves it in the desktop app) |
| Interpreter limits (steps, heap, call depth, nesting, interpolation, JSON, strings, collections, branches, tape size) | Owns | Sets configured values within the maxima |
| Session budgets (tokens, cost, wall clock), approvals, unattended refusals, the permission broker | Admission by reservation with the effective `max_tokens` and the reply floor; debits frame ledgers through the host | Owns: `BudgetGate` (including the refusal of a hosted model without an explicitly configured price under any cost cap, from stage 13; the card it gives a `local` model has price 0, which counts as priced), the broker, the unattended rules |
| Cancellation (classes, the drain fence, withdrawal, deadlines) | Owns the scheduler's side: cancellation points, re-parenting and the fence, withdrawal and outcomes on the tape, deadlines in `EffectCtx`, the contract tests (each `drain` tool declares a bound of at most 3600 s and is failed there) | Owns the class table, the read-only command classifier (a conservative allowlist, stage 17; until then no command is read-only), read-only MCP tools from user configuration only, the drain clamps (MCP `timeout_s`, browser tools), atomic withdrawal in the broker, abortable dispatch |
| Source loading: imports without escape or links, source size limits | Owns: the loader, given source roots | Decides which roots exist and their trust |
| Authored harnesses | Owns the authored rules in the checker (no trust sites, untrusted parameters, forced taint) and closure hashes | Owns storage, `write_harness` and `run_harness`, the contained tier, and approvals keyed by closure hash |
| Plugins, project sources, the safety floor on harness folders | — | Owns |
| Prompt framing | Owns the rules (chapter 05) and the vectors | Runs the vectors against the engine's helpers |
| The security review (stage 24) | Reviews this repository | Reviews the engine bindings, gates, clients and redaction |

## Stages

Each stage is one pull request of about 800 changed lines (code and tests; documentation on top)
in the repository named in the **Repo** column: Ratio stages against `main` here, Imperium stages
against Imperium's `experimental`, behind `ratio.enabled = false` until the security review. A
stage marked **Both** is a Ratio pull request and release first, then an Imperium pull request
that bumps the dependency. Every stage keeps its repository's checks green **in CI**. Each stage's
documentation updates ship in the same pull request.

Stages 1 to 12 are Ratio's and merge without anything from Imperium. Stage 11 is two pull
requests, 11a and 11b, to keep each near the size above. Stage 11a also includes a throwaway
`RatioHost` spike in Imperium, built against the candidate commit and never merged, to validate
the `Host` trait against the real engine before the release that fixes it. Of the 28 stages, 26
are committed; 27 is optional and 28 waits on Q9.

| # | Stage | Repo | Delivers | Tests (all in CI) |
| --- | --- | --- | --- | --- |
| 1 | Token reader | Ratio | Lexer with the mode stack (code, strings, nested interpolations), quantity, duration and money literals, contextual keywords, semicolon insertion by innermost delimiter | Token corpus: every literal form, nested interpolations, insertion in every delimiter kind, line-start `with` and `else`, attribute-only lines (no insertion), variant literals across lines |
| 2 | Parser | Ratio | The whole grammar; AST with spans; diagnostics with file, line, column and a caret; error recovery; nesting limits; a canonical printer | Parse corpus for every production and disambiguation rule; printer round trip (for printed text within the source limit) as a property test; nesting-limit tests |
| 3 | Modules and names | Ratio | Source roots, imports by path (no escape, no links), source-qualified names with areas (`authored:session/x`) and trust order, the registry trait (Imperium's daemon registry implements it in stage 18), name resolution, prelude | Resolution corpus; shadowing refused across trust levels; import-trust rules per source |
| 4 | Type checker | Ratio | Records, enums, options, tuples, generics on functions, subtyping (`Never`, `UserText`, `Brief`, `TrustedPrompt`), contextual variants, variant literals, exhaustiveness, turbofish, constant evaluation rules | `conformance/types/`: accepted and rejected programs with diagnostic codes |
| 5 | Schemas and taint | Ratio | Schema derivation and type digests; `Untrusted` mapping, abstract generics, sinks, `Brief` and `TrustedPrompt` inference; monomorphic `trust` and `as_task`; transitive trust sites; guarded-effect lint; static `Secret<T>` where the label is known; `harnesses.check` as a library call | One rejected program per sink and one accepted per allowed flow; a property test that generated programs never produce a `TrustedPrompt` or `task` section from untrusted sources; schema and digest snapshots |
| 6 | IR and core interpreter | Ratio | Lowering with site ids; canonical serialisation and IR hash; `IR_VERSION`; the prepared form (never hashed); the evaluator for the pure language with **run-time secrecy labels** on values and their propagation (including through `Json`), and label summaries; `Send + Sync` values; the persistent representations of chapter 06 (`data`) and charging by cost class; steps, heap and call-depth accounting, with a yield at every 1,000-step checkpoint; `Cancelled` and `Budget` rules; branch restrictions | `conformance/eval/`; hash stability snapshots across platforms, computed from the canonical IR and unchanged when the prepared form changes; label propagation tests for every operator; the `steps` expectation, one file per cost class; a property that no container with a labelled leaf has a clear summary bit (including values whose labels a replay adds and values restored from a `ratio_cache` entry), and that the debug-build sink scan agrees with the bit; a runtime test in which a second scripted session keeps progressing (by a progress counter) while the first runs a 5,000,000-step pure loop on a single-threaded runtime |
| 7 | Data I | Ratio | `text`, `re`; each built-in's cost class; the bounded pool for large built-ins with its per-session in-flight cap | One conformance test per built-in, with each row's failures and label propagation; debug-build work counters stay within a fixed multiple of each built-in's charge; offloadable built-ins return identical values, labels and charges below and above the threshold; a cancelled branch's pool job keeps its place in the cap, its steps and its heap until it finishes, and its heap is accounted before dispatch; a built-in whose input-size charge exceeds the steps left raises `Budget { which: steps }` without running (in stages 7 to 9, including `text.similarity` on large inputs) |
| 8 | Data II | Ratio | `json`, `list`, `map` | As stage 7; a loop of N pushes or sets is charged O(N log N) steps |
| 9 | Data III | Ratio | `math`, `hash`, `diff`, `template`, `vec`, `path`, `url`, `codec` | As stage 7; `diff.lines` charged on line counts plus both inputs' characters, debited before running, so `diff.lines` on large inputs raises `Budget { which: steps }` without running when the steps left cannot pay; float determinism with fixed bit patterns |
| 10 | Frames | Ratio | Frame model and frame paths; configuration evaluation and merge; attenuation (tools, mode meet, rules, paths, location, budgets, `sees`, depth, width counting running branches, attended); the budget ledger with admission by reservation, the effective `max_tokens`, the reply floor and the refusal of unpriced hosted models under a cost cap; the shared `steps` ledger; context modes; `Question` crossing frames; `meta` | Attenuation monotonicity property; ledger tests: concurrent model calls never exceed a token cap, in the frame or in a child frame with a lower cap, and the `max_tokens` sent equals the formula; a request with `max_tokens: 1` is admitted with one output token reserved whatever `min_reply_tokens` says; an admission below the floor waits for a held reservation and is refused when none is held; an unpriced hosted model under a cost cap raises `Budget { which: cost }`, and one with a configured price of 0 is admitted; `sees` refusals, including labelled values entering frames without `secrets` |
| 11a | Host, tape and scheduler | Ratio (spike in Imperium) | The `&self` `Host` trait with `Labelled<T>` values, `EffectCtx` (key, cancellation token, deadline), `tool_class` and `conversation_digest`, and the sink guard; `HOST_API_VERSION` and `TAPE_VERSION`; the host contract tests and their fixture, with the cancellation cases and the finite bound on every `drain`-class tool; the throwaway `RatioHost` spike in Imperium against the candidate commit; `ScriptedHost`; `TapeHost` with effect keys built from frame and branch paths, the turn sequence and verified replay (ledgers, step totals, cancellation outcomes), entries for input-bearing effects only, suppression and the reply digest, blobs and the label union; per-branch random generators; the scheduler: `par`, `par_map`, `race`, `timeout`, width with parked branches and admission in branch-path order, cancellation by class, withdrawal, re-parenting and the drain fence, deadlines | Replay determinism property under random completion orders with budgets just below total demand, the rebuilt ledgers verifying at every sequence number; replay of one fixed tape under permuted resolution orders passes every check; altering any sequence number, refusal, reading, recorded input count, card limit, price, step total, drained or withdrawn entry, or blob raises `Replay`; record-then-replay gives identical event streams; replayed values keep their labels, gain those the replaying session's rules set, and are untrusted; a change only to an output-only effect fails the reply digest, and replay makes no live `session.stream`, `session.emit` or log call; `replay/keys-retry-in-par-map`, `replay/budget-par-exhaustion`, `replay/crafted-tape-steps`, `replay/par-sibling-cancel-points`; `eval/width-nested-fanout` at width 2; running branches never exceed the width and start in branch-path order; tape size regression; real concurrency; every secrecy sink refuses a labelled value at the guard and, in the contract suite, at the unguarded host; the contract suite's frame and cancellation cases; a labelled blob in a headless tape is named by the redaction digest only |
| 11b | Control, `rand`, `time`, `state` | Ratio | `control`, with `par_fold` and single-flight `cached`, `memo` and `once` whose session entries keep labels; `rand`; `time`; `state` | `par_fold` in index order, in completion order (record, then replay under another order), after `Stop` (branches not started make no model call) and with a raise in `step`; a `check-error` for `state.set` in `step` and a `runtime-error Invalid` for `model.chat` in `step` on an outside conversation; eight branches on one `cached` key make one model call; a failing producer promotes the first waiter in branch-path order; crossed keys complete; `eval/width-single-flight` completes; record then replay of cache events under permuted orders; a tape whose wait names another key's producer, a producer with no earlier miss, a cycle or the waiter's ancestor raises `Replay`; a labelled cached value reaching a frame without `secrets` raises `Denied { by: sees }`, and one stored with `scope: session` or `once` is labelled when read back in a later turn; `eval/rand-par-branches` draws identical values under permuted orders with no per-draw tape entries |
| 12 | Model and think | Ratio | Prompt rendering with the framing rules of chapter 05 and the shared framing vectors (`conformance/framing/`); `model.*` and `think.*`, including `model.stream`'s `until` with deltas on the tape and `early_stop` for `think.self_consistency` and `think.best_of`; structured generation and repair; built-in prompt texts in the hash | Scripted-host tests per built-in; prompt rendering snapshots; a scripted stream of five deltas stopped by `until` after the third (the host sees a cancellation, the text is three deltas, and replay repeats the text and the callbacks); a `check-error` for an effectful `until`; self-consistency with `early_stop` and a scripted majority after four samples makes fewer than `n` model calls |
| 13 | Engine host Ia: conversations and models | Imperium | The `ratio-lang` dependency at a release tag; `RatioHost` model calls; the host contract tests and framing vectors run against it; multi-conversation support with handles, forks and rollback; safe points; harness agents and their events; conversation labelling and its journal event; the conversation digest chain; deadlines as request timeouts; `BudgetGate`'s refusal of a hosted model without an explicitly configured price under any cost cap (a `local` model's card has price 0, which counts as priced). **Needs the turn, multi-conversation, prompts and model-request seams** | Engine tests with the scripted OpenAI-compatible endpoint of `crates/imperium-cli/tests/headless_eval.rs`, extended for `response_format`, `tool_choice`, image parts and logprobs; the host contract suite against `RatioHost`, including `open_frame` refusing a child scope wider than its parent in each dimension (tools, mode, rules, paths, location, budgets, `sees`, depth), also when the parent `FrameCtx` passed in has been forged wider than Imperium's record of that frame; under a cost cap, `BudgetGate` refuses an unpriced hosted model with `Budget { which: cost }` and admits a hosted model with a configured price of 0 and a `local` model; over random append and rollback sequences, the conversation digest chain equals a full recomputation |
| 14 | Engine host Ib: dispatch | Imperium | Dispatch on any conversation with `CallScope`, structured outcomes, `tools.refuse`, compaction calls (local-only for labelled conversations), secrecy checks at dispatch from the per-call flag; the class table, abortable dispatch, the drain clamps (MCP `timeout_s`, browser tools) and atomic approve-or-withdraw in the broker. **Needs the dispatch, outcome and compaction seams** | One test per permission path, each the twin of a native-loop test; dispatch refuses labelled arguments even when the host is bypassed; dispatch refuses a call outside the frame's `CallScope` when a test forges a frame wider than its parent; the contract suite's cancellation cases against `RatioHost` |
| 15 | Engine host II: agents | Imperium | `agents.*` gated by tool names; per-task budgets carved from frames; cancelling running subagents; writer worktrees and `agent_merge`; secrecy on briefs and questions | Delegation limits and rules refuse as the engine does; budget carving; cancel tests |
| 16 | Engine host IIIa: memory and session | Imperium | `memory.*` (private entries as a secret source), `session.*` with pseudo-tools, `ratio_approval`, `declassify`, checkpoints, branches, rewinds; the command rule for secret paths (floor-style parsing of command lines) | One engine test per built-in group; pseudo-tool rules apply |
| 17 | Engine host IIIb: context and tools | Imperium | `context.*` extras (pins, artefacts, chunks), every `tools.*` built-in including `browser_screenshot`, `eval.run_tests`; `tools.install_extension` as an agent-origin install; read-only MCP tools from the user's configuration, and the read-only command classifier (a conservative allowlist) | One engine test per built-in group; `tools.install_extension` asks on every install in every mode, with an allow rule present, in auto mode and after an earlier approval, is not covered by `ratio_approval`, leaves the extension withheld until desktop approval, and is refused in an unattended session; an unrecognised or compound command line (`cat a; rm b`, a redirect) is `drain`; a read-only MCP flag from a project's files is ignored |
| 18 | Harness switch and surfaces | Imperium | Registry and loading in the daemon; pinning by hash and re-selection; `[harness]` and `[ratio]` settings; `session.create` and `set_harness`; agent file and `spawn_agent` fields; turn payloads from automation; protocol methods and events with **redaction at the daemon boundary**: the per-session HMAC key, `[secret: <digest>]` substitution in every outgoing event and result, `session.subscribe` `secrets: true` and `declassify_conversation` only through the desktop app's single-use challenge and native dialog (never the CLI, never in unattended sessions, never to paired phones); labels on engine state (plan, aids' state, titles including auto-titles, authored source) journalled, carried and redacted; hooks; CLI | Protocol tests; a changed file does not take effect until re-selected; the model cannot name a package outside `harness.offered`; no labelled value leaves the daemon unredacted to a client that did not ask, never to a paired phone, never in an unattended session; the HMAC key never leaves the daemon; **an agent's command that runs the `imperium` CLI (or connects as any client) to declassify or to subscribe with `secrets: true` is refused**; a labelled plan or title is redacted in every client and phone view |
| 19 | Headless evaluation | Imperium | `--harness`, `--subagent-harness`, tape files with secret redaction and optional zstd compression, output schema v2. **Needs the eval-runs seam** | Headless runs on the scripted endpoint; tape record and replay through the CLI |
| 20 | Floor and plugin gates | Imperium | Safety floor on file-tool paths for harness folders and authored areas; plugin-with-harnesses withheld and hash-pinned; agent-file source restriction; `ratio.trusted_projects` | A write to a harness folder asks every time in every mode; an unapproved plugin's harnesses do not load; an update is withheld again |
| 21 | Authored and offered harnesses | Both | Ratio (the checker): the authored rules (no trust sites, untrusted parameters for `run_harness`, results forced untrusted), the not-offerable rule for harnesses with trusted parameters, closure hashes, `meta.author`. Imperium: `write_harness`, `run_harness` (deep-untrusted arguments for every source), authored areas and their qualified names, the contained tier with the pinned model and provider-side tools off, caller-tier approvals keyed by closure hash and scoped to authority, journal events | A contained harness cannot write, run commands, reach the network, use provider-side tools or see secrets; a route that cannot disable provider tools is refused; a new closure hash or a wider authority asks again; self-use never exceeds the caller |
| 22 | Security-relevant clients | Imperium | The desktop's approval dialogs (authored caller tier with the whole closure, diffs and summary, untrusted rendering; persistence; plugin-harness approval), the clients' handling of redacted values (the daemon side is stage 18), the labelled-conversation banner with its three recovery paths, and the **live harness view** (`harnesses.source`, `harness_site` subscription and throttling, replay stepping) | Client tests against recorded event streams: authored source never rendered as markdown or links; redacted values stay redacted; throttle tests |
| 23 | Built-in packages and parity | Both | Ratio: the packages' source in `std/`, embedded by the facade, and the examples as conformance programs. Imperium: the parity suite. The packages: `std:default`, `std:consul_escalation`, `std:plan_execute_verify`, `std:self_consistency`, `std:tdd_fix`; the examples as conformance programs; the **parity suite** as defined in chapter 08, run with the default configuration | The parity suite; item 64 scenarios (each kind of rejection, per task, within a turn; takeover continues the conversation; independent critic after takeover; plan critique before execution when `update_plan` is alone; a question reaches the dispatcher) |
| 24 | Security review and hardening | Both | One review across both repositories (the ownership table above says which side fixes what). The independent review (chapter 07's gate), **after** stages 13 to 23, so it covers the engine bindings, the gates, authored harnesses, the dialogs, the live view and redaction; fixes; limit tests; parser and checker robustness properties in normal CI, `cargo fuzz` on the nightly schedule; default limits set from measurements of the examples; `ratio.enabled` may default to true only after this stage | The review's regression tests, limit tests, robustness properties |
| 25 | Remaining clients | Imperium | The harness picker, Agents panel rows for harness agents, the authored list | Client tests against recorded event streams |
| 26 | Default switch | Imperium | After one release cycle of opt-in use with the parity suite green, `harness.select` defaults to `std:default` (the user's decision, Q1); the native loop stays selectable for one more release | The parity suite on every pull request |
| 27 (optional) | Resume by replay | Both | Ratio: resuming an entry from a tape. Imperium: the journal and restart wiring. After a daemon restart mid-turn, re-run the entry from the journal's tape to rebuild the interpreter state, then continue live | Restart tests: the resumed turn makes no duplicate tool calls |
| 28 (pending Q9) | Harnesses from code mode | Imperium | Option B of chapter 08 if the user chooses it, after code mode's own security review | As specified with the decision |

## The conformance suite

`conformance/` at the root of this repository holds `.ratio` files grouped by chapter (`syntax`, `names`, `types`,
`taint`, `secrecy`, `eval`, `stdlib/<module>`, `frames`, `replay`, `authored`, `examples`). Each
file starts with expectation lines:

```ratio
// expect: ok                                   (checks and runs; value printed as JSON)
// expect: value {"winner": "42", "agreement": 0.6}
// expect: check-error E0412 at 7:13            (a checker diagnostic code and position)
// expect: runtime-error SchemaMismatch
// expect: steps 1042                           (the interpreter steps the run is charged, exactly)
// fixture: model replies.json                  (scripted model replies for this file)
```

The token reader's expectations (stage 1) are:

```ratio
// expect: lex-ok                               (read without a diagnostic)
// expect: lex-error L0004 at 2:7               (a lexical diagnostic and position; one line each, in order)
// tokens: kw:let ident:x = int:1 ;!            (the tokens, in the notation below; lines join)
// option: max_nesting 2                        (a reader limit: max_source_bytes, max_nesting,
//                                                 max_interpolation_depth or max_diagnostics)
```

The parser's expectations (stage 2), for a file without a `lex-` expectation:

```ratio
// expect: parse-ok                             (read and parsed without a diagnostic)
// expect: parse-error P0001 at 3:5             (a parse diagnostic and position; one line each, in order)
// tree: (module (item (fn f …)))               (the span-free tree dump; lines join)
```

`parse-ok` and `parse-error` take the same `option:` lines as the token reader. `tree:` is the
span-free dump of the parsed tree (`ratio_syntax::dump`, one line, notation in that module's
documentation); several `tree:` lines join with a space, and a file with `tree:` needs a
`parse-ok` or `parse-error` line. The diagnostics a `parse-` expectation lists are all of the
parse's, the token reader's included, in order.

The header is every line at the top of the file that starts with `//` and a space; each must be one of these
directives (a misspelt one fails the file), and the first other line starts the program. Line
numbers count the header. In the `tokens:` notation a keyword is `kw:let`, a contextual keyword
`ctx:task`, an identifier `ident:x`, literals `int:1`, `qty:1500`, `float:0.5`, `dur:500000us`
(microseconds), `money:500000u` (millionths of a dollar), strings `str[ text:"…" interp[ … ]interp
]str` (`str3[` for a long string, `raw:"…"` and `raw3:"…"` for raw ones, `spec:"…"` for a format
spec), `doc:"…"` a documentation comment, `{block`, `{record`, `{prompt`, `{enum` and `{match`
an opening brace and what it opens, `;!` an inserted semicolon, and other punctuation as written.

The lexical diagnostic codes (chapter 02 defines the rules, chapter 07 the limits):

| Code | Diagnostic |
| --- | --- |
| `L0001` | A character that starts no token (including `!` and `&`, and a `$` not starting an amount) |
| `L0002` | A string not closed before the end of its line (short strings) or of the source |
| `L0003` | An unknown or malformed escape, or a `\` at the end of a line |
| `L0004` | A carriage return not followed by a line feed, in code, a comment or a string |
| `L0005` | A lone `}` in string text |
| `L0006` | A line break inside an interpolation of a short string |
| `L0007` | An interpolation not closed before the end of the source |
| `L0008` | Letters after a number that are not a unit, or a misplaced `_` separator |
| `L0009` | A number outside its range (64-bit signed; a duration in microseconds; a finite float) |
| `L0010` | A quantity, duration or amount of money that cannot be exact |
| `L0011` | Interpolations nested deeper than the limit |
| `L0012` | Brackets, blocks and interpolations nested deeper than the limit; reading stops |
| `L0013` | A source larger than the limit; nothing is read |
| `L0014` | A closing delimiter that matches nothing, or an opening one never closed |
| `L0015` | More diagnostics than the limit (100); it marks where the next one was found, and reading stops |
| `L0016` | A `{`, `}` or line break in a format spec |

The parse diagnostic codes (chapter 02 defines the rules; the limit is chapter 07's):

| Code | Diagnostic |
| --- | --- |
| `P0001` | A token the grammar does not allow where it is (unexpected token) |
| `P0002` | Expressions, types and patterns nested deeper than `max_nesting`; parsing stops |
| `P0003` | A `{` the token reader read as one kind (rule 1) where the grammar needs another (wrong brace) |
| `P0004` | The left side of an assignment is not a place |
| `P0005` | A second comparison or range operator in a row (`a < b < c`, `0..1..2`) |
| `P0006` | Attributes, documentation or `pub` where they do not apply (misplaced modifier) |
| `P0007` | More parse diagnostics than the limit (100); it marks where the next one was found, and parsing stops |
| `P0008` | A block head whose `{` is on the next line (head split across lines) |
| `P0009` | A positional argument after a named one |
| `P0010` | Internal error: the parser could not run (no source triggers it; the module is empty) |

The name diagnostic codes (stage 3; chapter 03 defines the rules, chapter 07 the limits; `N0020`
and `N0021` are warnings):

| Code | Diagnostic |
| --- | --- |
| `N0001` | An import path that is not a plain relative path to a `.ratio` file |
| `N0002` | An import path whose `..` leaves the package's source root |
| `N0003` | An import path that is, or passes through, a link |
| `N0004` | No file at an import path |
| `N0005` | No package by that name (a bare name looks in `std` and `user` only) |
| `N0006` | A malformed source-qualified name or version range |
| `N0007` | An import the trust rules refuse |
| `N0008` | A package whose `meta` version is missing or outside the import's range |
| `N0009` | An import cycle |
| `N0010` | A file or package over its size limit |
| `N0011` | More files, packages or registry calls, or a longer import chain, than the limits |
| `N0012` | A second definition of a name in a module |
| `N0013` | An import list naming something the module does not define |
| `N0014` | An import list naming an item that is not `pub` |
| `N0015` | Two paths of a package that differ only in letter case |
| `N0016` | A file the registry could not read, or that is not UTF-8 |
| `N0017` | An imported module with syntax errors |
| `N0018` | An import that binds no name (the file name is not an identifier); `as` is needed |
| `N0019` | `pub` on an import (imports are not re-exported) |
| `N0020` | Warning: a name that shadows a standard module |
| `N0021` | Warning: a name that shadows `trust`, `as_task` or `declassify` |

A single test runner runs every file with the scripted host. In stages 1 and 2 it is the syntax
crate's runner, `crates/ratio-syntax/tests/conformance.rs`, which understands `lex-ok`,
`lex-error`, `tokens:`, `parse-ok`, `parse-error`, `tree:` and `option:` lines and skips the later
stages' expectations; every `L` code has at least one `lex-error` file and every `P` code at least
one `parse-error` file. From stage 3, `conformance/names/` holds **worlds**: one folder per world,
then one per source root (`std/`, `user/`, `project/`, `plugin/<plugin>/`, `authored/session/`,
`authored/env/`), then one per package with its entry file `main.ratio`. The names runner,
`crates/ratio-names/tests/conformance.rs`, loads the one entry file of each world that says
`// expect: resolve-ok`, or lists `// expect: resolve-error N0007 at 3:8 [in <file>]` and
`// expect: resolve-warning …` lines (all of the load's diagnostics, in any order); the syntax
runner treats these as later-stage expectations. Every `N` code has a world, except those only a
host or a limit can trigger (`N0003`, `N0010`, `N0011`, `N0016`), which the crate's tests cover.
`conformance/spec-map.txt` names the chapters each directory tests (above). Three coverage checks
keep the suite honest:

- `builtins.txt` (generated from chapter 06, above) lists every built-in. The
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

- **CI is the gate.** Unit, property and conformance tests run in this repository's CI on Linux,
  Windows and macOS; engine, parity and client tests in Imperium's. Nothing needs a local model,
  a GPU or a network; a local `cargo test` here is an ordinary build.
- **A fake model, never a real one.** The scripted host (in this repository) and the scripted
  OpenAI-compatible endpoint (for engine tests) answer from fixtures; they also check the
  requests they receive, so a test fails when a prompt or schema changes unexpectedly.
- **Twins of existing tests.** Every permission path the native loop is tested on gets a Ratio
  twin, so attenuation and dispatch cannot drift from the engine.
- **Determinism is tested, not assumed.** Replay and completion-order properties run on every
  pull request, including replay of one fixed tape under randomly permuted resolution orders.

## Rollout

1. Stages 1 to 12 land here, released as `ratio-lang` versions that nothing in Imperium selects
   yet (`ratio.enabled = false`, no switch).
2. Stages 13 to 23 make Ratio selectable in headless runs and, with `ratio.enabled = true`, in
   sessions; documentation marks it experimental. Stage 23 ships the built-in packages, so item 64
   becomes usable by selecting `std:consul_escalation`.
3. Stage 24's review decides whether `ratio.enabled` defaults to true.
4. Stage 26 switches the default harness to `std:default` after a release cycle of parity.

Rollback at any point: set `ratio.enabled = false`; sessions using a package fall back to
`native` at their next turn with a notice.
