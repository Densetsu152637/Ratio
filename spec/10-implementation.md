# 10 Implementation plan

## Decision R15: one crate behind a host trait

The interpreter is a new crate, `crates/imperium-ratio`, in the Imperium workspace. It depends on
no other Imperium crate except `imperium-protocol` (for the shared tool schemas and framing
helpers, if they live there; otherwise the helpers move there, see dependencies). It defines a
`Host` trait for every effect; `imperium-engine` depends on `imperium-ratio` and implements
`Host`. Dependency direction:

```text
imperium-protocol ◀── imperium-ratio ◀── imperium-engine ◀── imperium-core ◀── imperium-cli
                         (language)       (RatioHost,         (registry,        (flags,
                                           harness switch)     protocol)         commands)
```

This keeps the language testable without a daemon (a scripted host implements the trait), keeps
the engine the only place that holds authority, and lets the research session change the engine
without touching the language.

Rejected: putting the interpreter inside `imperium-engine`. The engine is large and its agent
files are owned by another session; the language would be untestable without the whole agent.
Rejected: a separate process (chapter 01).

### The `Host` trait (sketch)

```rust
#[async_trait]
pub trait Host: Send {
    // Models
    async fn generate(&mut self, frame: &FrameCtx, req: ModelRequest) -> Result<ModelResponse, HostError>;
    async fn embed(&mut self, frame: &FrameCtx, texts: Vec<String>, model: Option<ModelRef>) -> Result<Vec<Vec<f32>>, HostError>;
    fn count_tokens(&self, frame: &FrameCtx, what: Countable) -> TokenCount;
    fn resolve_model(&self, frame: &FrameCtx, m: &ModelRef) -> Result<ModelCard, HostError>;
    // Tools: every call carries the frame's scope; the engine enforces it before its own checks
    async fn call_tool(&mut self, frame: &FrameCtx, call: ToolInvocation) -> Result<ToolOutcome, HostError>;
    // Conversations (engine-owned, append-only)
    fn conversation(&mut self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError>;
    async fn chat(&mut self, frame: &FrameCtx, conv: ConvId, req: ChatRequest) -> Result<ModelReply, HostError>;
    async fn safe_point(&mut self, frame: &FrameCtx, conv: ConvId) -> Result<SafePoint, HostError>;
    async fn dispatch(&mut self, frame: &FrameCtx, conv: ConvId, calls: Vec<ToolCall>, concurrent: bool) -> Result<Vec<ToolResult>, HostError>;
    fn append(&mut self, frame: &FrameCtx, conv: ConvId, msg: NewMessage) -> Result<(), HostError>;
    async fn compact(&mut self, frame: &FrameCtx, conv: ConvId, opts: CompactOpts) -> Result<Compaction, HostError>;
    // Frames, agents, the session
    fn open_frame(&mut self, parent: &FrameCtx, req: FrameRequest) -> Result<FrameCtx, HostError>;
    fn close_frame(&mut self, frame: FrameCtx, outcome: FrameOutcome);
    async fn agents(&mut self, frame: &FrameCtx, op: AgentOp) -> Result<AgentResult, HostError>;
    async fn memory(&mut self, frame: &FrameCtx, op: MemoryOp) -> Result<MemoryResult, HostError>;
    async fn session(&mut self, frame: &FrameCtx, op: SessionOp) -> Result<SessionResult, HostError>;
    // Recorded, authority-free effects
    fn now(&mut self) -> Instant;
    fn seed(&mut self) -> [u8; 32];
    fn emit(&mut self, frame: &FrameCtx, event: RatioEvent);
    fn cancelled(&self, frame: &FrameCtx) -> bool;
}
```

`FrameCtx` carries the frame path, agent id, attenuated tool set, permission mode, extra rules,
paths, location, attended flag and budget ledger handle. The tape is a wrapper (`TapeHost<H>`)
that records or replays every call by effect key, so record and replay need nothing from the
engine.

## What it touches in Imperium

| Area | Change | Owner |
| --- | --- | --- |
| `crates/imperium-ratio` (new) | The language: parser, checker, IR, interpreter, standard library, tape, scripted host, built-in packages, conformance suite | This work |
| `imperium-engine` `agent/ratio_host.rs` (new) | `RatioHost`: `Host` over the agent's chat clients, tool dispatch, conversations, compactor, agent records and events | This work |
| `imperium-engine` `agent/turn.rs` | A seam where a turn is driven by a `TurnDriver` (native loop or Ratio entry), chosen from the agent's harness; safe points exposed as a callable step | **Research session: dependency to coordinate** |
| `imperium-engine` `agent/tools.rs` | `run_tool` takes a `CallScope` (frame tool set, mode meet, extra deny and ask rules, paths, location) checked first; `ToolOutcome` exposes `denied { by }` and `blocked` distinctly; tool-call provenance (`harness` field) for hooks and events | **Research session: dependency to coordinate** |
| `imperium-engine` `agent/compaction.rs`, `compact.rs` | Callable compaction for any engine-owned conversation (not only the root's), a `focus` line, and pinned ranges carried verbatim like kept context | **Research session: dependency to coordinate** |
| `imperium-engine` `agent/prompts.rs` | `system_default(role)` exposed as a function; the framing helpers (`HARNESS_TAGS` defusing) callable for prompt sections | **Research session: dependency to coordinate** |
| `imperium-models` `chat.rs` | Request fields: `response_format` / JSON Schema, llama.cpp `json_schema`, vLLM guided JSON; sampling (`top_k`, `min_p`, `seed`, `stop`, penalties, `n`, `logprobs`); a completions route; capability flags per endpoint | **Research session: dependency to coordinate** |
| `imperium-memory` | A recall call with caller-supplied text (`memory.recall`) and proposals (`memory.propose`) when the protocol's `memory.approve` lands | **Research session: dependency to coordinate** |
| `imperium-core` `eval*` | `--harness`, `--subagent-harness`, `--ratio-tape`, output schema v2 (`harness`, `ratio`) | **Research session: dependency to coordinate** |
| `imperium-engine` agent state, delegation | Harness agents (ephemeral records, ids, slot holders); `spawn_agent` `harness`, `prompt`, `workflow`; per-agent harness on the record | This work, reviewed with the research session |
| `imperium-policy` | Permission-mode `meet`; harness source folders added to the safety floor's protected paths | This work |
| `imperium-protocol` | Events, methods, `SessionSummary.harness`, `spawn_agent` schema fields (status `planned` until built), a protocol minor bump; the Emperor repository is told of the schema change | This work |
| `imperium-config` | `[harness] select`, `offered`; `[ratio]` | This work |
| `imperium-core` (daemon) | Harness registry and loading, `harnesses.list`, `harnesses.check`, `set_harness`, journal replay of `ratio_*` events | This work |
| Hooks (`imperium-extensions`) | `harness.call`, `harness.return`; `harness` field in tool payloads | This work |
| `imperium-cli` | `imperium harness list|check|test`, `/harness` in `imperium chat` | This work |
| Imperium docs | `docs/03`, `05`, `06`, `10`, `12` (items 58 and 64 status), a new `docs/2x-ratio.md` pointing at this specification, the protocol reference | This work, with each stage |

## Stages

Each stage is one pull request of about 800 changed lines (code and tests; documentation on top)
against `experimental`, behind `ratio.enabled = false` until stage 12. Every stage keeps
`cargo fmt`, the workspace's checks and the tool-schema drift check green **in CI**: no stage
depends on a local build. Each stage's documentation updates ship in the same pull request.

| # | Stage | Delivers | Tests (all in CI) |
| --- | --- | --- | --- |
| 1 | Syntax | Crate skeleton; token reader with semicolon insertion; parser for the whole grammar; AST with spans; diagnostics with file, line, column and a caret; a canonical printer | Parse corpus (`conformance/syntax/`): every grammar production, every disambiguation rule, error recovery; printer round trip (print then parse gives the same AST) as a property test |
| 2 | Names and types | Modules, imports by path (source-root confinement, no links), the registry trait, name resolution, prelude, the type checker (records, enums, options, tuples, generics on functions, subtyping, contextual variants, exhaustiveness, `Never`), schema derivation with attributes | `conformance/types/`: accepted and rejected programs with expected diagnostics; schema snapshots for every schema-able type form |
| 3 | Taint and prompts | `Untrusted<T>` mapping and normalisation, propagation, sinks, `trust` sites; prompt sections typed; `harnesses.check` as a library call returning diagnostics, tools used and trust sites | `conformance/taint/`: one rejected program per sink, one accepted per allowed flow; a property test that no program accepted by the checker can produce a `system` section from an untrusted source (generated programs over a small grammar) |
| 4 | IR and core interpreter | Lowering to Ratio Core IR with site ids; canonical serialisation and the IR hash; tree-walking evaluator for the pure language (persistent values, closures by value, `var`, loops, `match`, `try`/`catch`, `defer`, `raise`); step and heap accounting; `Cancelled` and `Budget` propagation rules | `conformance/eval/`: programs with expected values or errors; hash stability snapshots (the same source gives the same hash across runs and platforms; any change to the IR format is a test failure until the snapshot is updated deliberately) |
| 5 | Data library I | `text`, `re`, `json`, `list`, `map`, `math` (pure) | One conformance test per built-in, edge cases from each row's "fails with"; float determinism tests with fixed expected bit patterns |
| 6 | Effects, concurrency, tape | The `Host` trait; `ScriptedHost` (models, tools, agents answered from fixtures); `TapeHost` record and replay with effect keys and request digests; `par`, `par_map`, `race`, `timeout`, cancellation; `rand`, `time`; `control` (retry, fallback, rate limit, caches, logging, tracing, assertions) | Replay determinism property: for random completion orders of scripted effects, results and tapes are identical; record-then-replay gives identical event streams; divergence tests; cancellation reaches every branch |
| 7 | Data library II and frames | `hash`, `diff`, `template`, `vec`, `path`, `url`, `codec`; frames and harness calls: configuration merge, attenuation (tools, mode meet, rules, paths, location, budgets, depth, width, attended), the budget ledger, context modes, `Question` crossing frames; `meta` | Attenuation monotonicity as a property test (for random caller and request configurations, every dimension of the callee is at most the caller's); budget ledger tests (spend debits all ancestors, refunds on return, uncatchable inside its scope) |
| 8 | Model and think | Prompt rendering with the engine's framing rules (vendored test vectors until stage 9 links the real helpers); `model.*` and `think.*` over the host, structured generation (native schema, grammar, validate-and-repair), voting, refinement, debate, the three search helpers, confidence; built-in prompt texts and their hashing | Scripted-host tests per built-in, including repair loops, schema failures, ties, and search order determinism; prompt rendering snapshots |
| 9 | Engine host I | `RatioHost`: model calls through chat clients, conversations and safe points, `tools.dispatch` with `CallScope`, compaction calls, harness agents and their events. **Needs the turn, tools, compaction, prompts and chat dependencies above** | Engine tests with the scripted OpenAI-compatible endpoint of `crates/imperium-cli/tests/headless_eval.rs`, extended for `response_format` and logprobs: one per permission path (rule deny, mode refuse, floor ask, hook block, attenuation refuse), each the twin of an existing native-loop test |
| 10 | Engine host II | `agents.*`, `memory.*`, `session.*`, `context.*` extras (pins, artefacts, chunks), `tools.*` for every tool, `eval.run_tests`; `ratio_approval` | One engine test per built-in group against the scripted endpoint and fake tools; the knowledge-agent and delegation limits refuse as the engine does |
| 11 | Harness switch and surfaces | Registry and loading in the daemon; `[harness]` and `[ratio]` settings; `session.create` and `set_harness`; agent file and `spawn_agent` fields; protocol methods and events; hooks; CLI commands; headless flags, tape files and output schema v2. **Needs the `eval*` dependency** | Protocol tests; journal replay of `ratio_state`, `harness_changed`; headless runs with `--harness` on the scripted endpoint; a project harness in an untrusted project is not loaded; the model cannot name a package outside `harness.offered` |
| 12 | Built-in packages and parity | `std:default`, `std:consul-escalation`, `std:plan-execute-verify`, `std:self-consistency`, `std:tdd-fix`; the examples of chapter 09 as conformance programs; **differential tests**: every recorded headless task of the eval suites run with `native` and with `std:default` against the same scripted endpoint must give byte-identical request bodies, the same tool calls, the same events (ignoring ids and times) and the same final reply | The differential suite; item 64 scenario tests (two rejections of each kind, per task, within a turn; takeover continues the same conversation; a Consul question reaches the dispatcher) |
| 13 | Security review and hardening | An independent review (chapter 07's gate); fixes; resource-limit tests (steps, heap, strings, nesting, regex size, approvals per turn); a parser and checker robustness property test (random input never panics, always ends within a step budget) in normal CI, and a `cargo fuzz` job on the nightly schedule; `ratio.enabled` may default to true only after this stage | Limit tests, robustness properties, the review's regression tests |
| 14 (optional) | Resume by replay | After a daemon restart mid-turn, re-run the entry from the journal's tape to rebuild the interpreter state, then continue live | Restart tests with the scripted endpoint: the resumed turn makes no duplicate tool calls |

## The conformance suite

`crates/imperium-ratio/conformance/` holds `.ratio` files grouped by chapter (`syntax`, `types`,
`taint`, `eval`, `stdlib/<module>`, `frames`, `replay`, `examples`). Each file starts with
expectation lines:

```ratio
// expect: ok                                   (checks and runs; value printed as JSON)
// expect: value {"winner": "42", "agreement": 0.6}
// expect: check-error E0412 at 7:13            (a checker diagnostic code and position)
// expect: runtime-error SchemaMismatch
// fixture: model replies.json                  (scripted model replies for this file)
```

A single test runner (`tests/conformance.rs`) runs them all with the scripted host. Two coverage
checks keep the suite honest:

- `conformance/builtins.txt` lists every built-in of chapter 06 (generated from the tables of this
  specification). The runner fails if a listed built-in has no conformance file using it, or if
  the interpreter registers a built-in the list does not have.
- Every diagnostic code the checker can emit has at least one `check-error` file.

The suite is versioned with the specification: a change to the language changes this
specification, the suite and the implementation in one pull request.

## Testing principles

- **Entirely in CI.** Unit, property, conformance, engine and differential tests run in the
  workspace's CI jobs; nothing needs a local model, a GPU, a network, or a local build.
- **A fake model, never a real one.** The scripted host (inside the crate) and the scripted
  OpenAI-compatible endpoint (for engine tests) answer from fixtures; they also check the
  requests they receive, so a test fails when a prompt or schema changes unexpectedly.
- **Twins of existing tests.** Every permission path the native loop is tested on gets a Ratio
  twin, so attenuation and dispatch cannot drift from the engine.
- **Determinism is tested, not assumed.** Replay and completion-order properties run on every
  pull request.

## Rollout

1. Stages 1 to 8 land with nothing selectable (`ratio.enabled = false`, no switch).
2. Stages 9 to 11 make Ratio selectable in headless runs and, with `ratio.enabled = true`, in
   sessions; documentation marks it experimental.
3. Stage 12 ships the built-in packages; item 64 becomes usable by selecting
   `std:consul-escalation`.
4. Stage 13's review decides whether `ratio.enabled` defaults to true. Whether `std:default`
   replaces the native loop as the default harness is a separate, later decision (R14).

Rollback at any point: set `ratio.enabled = false`; sessions using a package fall back to
`native` at their next turn with a notice.
