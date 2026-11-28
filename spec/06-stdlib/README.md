# 06 Standard library

The standard library is how a harness does anything: every model call, tool call, delegation,
memory access, user interaction and clock read is a built-in. It is organised in modules that are
always in scope (chapter 03, modules). This page gives the conventions and the shared types; each
module has its own page.

| Module | Page | What it covers | Built-ins |
| --- | --- | --- | --- |
| `model` | [model.md](model.md) | Generation, chat, structured output, classification, extraction, judging, ranking, embeddings, tokens, routing | 29 |
| `think` | [think.md](think.md) | Reasoning methods: steps, plans, critique, revision, verification, voting, debate, search, confidence, clarification | 30 |
| `tools` | [tools.md](tools.md) | One built-in per Imperium tool, plus dispatch of model-chosen calls | 50 |
| `agents` | [agents.md](agents.md) | Subagents, knowledge agents, named agents | 20 |
| `memory` | [memory.md](memory.md) | Memory entries by scope, and session search | 12 |
| `context` | [context.md](context.md) | Conversations, transcripts, pins, compaction, notices, token budgets, safe points, large inputs | 35 |
| `session` | [session.md](session.md) | The user, approvals, notifications, plans, checkpoints, branches | 22 |
| `state` | [session.md](session.md#state-package-state) | The package's declared state | 4 |
| `data` | [data.md](data.md) | Text, regex, JSON, lists, maps, math, random, time, hashing, diff, templates, vectors, paths, URLs, codecs (15 submodules) | 213 |
| `control` | [control.md](control.md) | Retry, timeouts, races, fan-out, rate limits, caches, assertions, errors, budgets, logging, tracing | 30 |
| `eval` | [eval.md](eval.md) | Tests, metrics, fixtures, the record and replay seam | 14 |
| `meta` | [meta.md](meta.md) | Introspection of tools, models, harnesses, frames and permissions; loading harnesses by name | 20 |
| **Total** | | | **479** |

## How entries are written

Each built-in has one row:

- **Signature.** Parameters in order; `name: T = default` marks an optional parameter that may also
  be passed by name; `<T>` marks a type parameter. Return type after `->`.
- **Semantics.** What it does, precisely enough to implement and test.
- **Fails with.** The error kinds it raises itself (chapter 03), beyond the kinds any effect can
  raise (`Budget`, `Cancelled`, `Replay`), which are not repeated.
- **Needs.** The permission or capability, from the legend below.

## Permission legend

| Needs | Meaning |
| --- | --- |
| `pure` | No effect. Costs interpreter steps only. Never asks, never fails for permission |
| `effect` | An effect with no authority (clock, randomness, logging, events). Never asks. On the tape when the program reads its result (chapter 01); `rand.*` draws follow from the recorded seed |
| `engine` | Reads engine state (the frame, the conversation, agent records, the model table). Never asks |
| `model` | One or more model calls (the row says how many). Charged to `tokens`, `cost` and `model_calls`; subject to the frame's model location. Never asks, except that a session budget configured to ask before spending asks as it does for any model call |
| `tool:<name>` | Dispatched as (or, for a **pseudo-tool** in italics such as `tool:`*`session_checkpoint`*, decided as if it were) Imperium's tool `<name>` through the engine's own path: permission profile, frame tool set, rules (deny over ask over allow), safety floor, permission mode, `tool.before` and `tool.after` hooks, approval, events. Decided exactly as a model's direct call of that tool would be; reads run without asking unless a rule says otherwise |
| `user` | Talks to the person: only in attended frames; in an unattended frame it raises `Denied { by: unattended }` or, where noted, uses the recommendation |
| `memory.write` | A memory write: only in frames of the root agent's turn, under the scope's `memory.write.<scope>` policy (`ask` by default); refused in plan mode |
| `delegate` | Creates or controls engine subagents; always together with the `tool:` name of the corresponding Imperium tool, and subject to Imperium's delegation limits (today: root only, depth 1) |
| `meta` | Reads the registry or the frame's own description; loading a harness by name is subject to the trust rules of chapter 03 |
| `sees:<item>` | An ambient read of session state, allowed only when the frame's `sees` includes the item (chapter 04); otherwise `Denied { by: sees }` |
| `entry` | Only in an entry's own frame (or a `fork` frame of it) |

Every effect is also charged to `steps`, and every effect that reaches the engine is subject to
the frame's attenuated tool set, mode, rules, paths, location and secrecy flag (chapter 04),
checked by the engine itself.

**Pseudo-tools.** Built-ins with authority but no Imperium tool of their own are decided under a
pseudo-tool name, so that frame tool sets, user rules, permission modes and hooks apply to them
exactly as to tools: `ratio_approval`, `session_checkpoint`, `session_branch`,
`session_rewind_files`, `agent_merge`, `write_harness`, `run_harness`. A pseudo-tool is in a frame's
tool set only if listed (or matched by a pattern); entry frames have all of them. Rules match
them by name like any tool (`{ tool = "session_rewind_files", action = "deny" }`).

## Shared types

These types appear across modules. Text fields of anything produced outside the program are
`Untrusted` as described in chapter 03.

```ratio
// Turns and tasks (entries, chapter 01)
// UserText: the user's own words; trusted, a subtype of Str.
// Brief: a task given to this agent; may go in `task` sections (chapter 03).
type Turn = {
  message: UserText,                        // what the user (or the session's configuration) wrote
  payloads: [Untrusted<Payload>],           // text that came with the turn but was not written by the user
  attachments: [Untrusted<Attachment>],
  index: Int, resumed: Bool,
}
type Payload = { kind: PayloadKind, source: Str, text: Untrusted<Str> }
enum PayloadKind { ci_log, pr_comment, issue, review, page, other }
type Attachment = { name: Untrusted<Str>, kind: Str, text?: Untrusted<Str>, image?: Image, path?: Untrusted<Str>, bytes: Int }
type Reply = { text: Untrusted<Str>, status: ReplyStatus }
enum ReplyStatus { done, step_limit, question, stopped }
type TaskInput = { brief: Brief, title: Untrusted<Str>, access: Access, profile?: AgentProfile, follow_up: Bool }
type Report = { text: Untrusted<Str>, ok: Bool, question?: Untrusted<Str> }
type Question = { question: Str, options: [Str], recommended: Str, secret?: Bool }
                                            // session.ask; an absent `secret` is false
enum Access { read, write }
type AgentProfile = {
  name: Str,                                // the agent file's name (engine-validated kebab-case)
  file_instructions?: TrustedPrompt,        // the agent file's body: configuration, trusted
  spawn_prompt?: Untrusted<Str>,            // a per-spawn `prompt` from the dispatching model: untrusted
  workflow?: Str,                           // a workflow companion reference
}

// Models
enum ModelRef { session, inherit, Named { id: Str }, Routed { tier?: Tier, location?: Location, tags: [Str] } }
                                            // a Str literal coerces to Named { id }; a Str constant
                                            // needs an explicit `: ModelRef` annotation
enum Tier { basic, standard, strong, frontier }
enum Location { local, cloud, any }
enum Effort { low, medium, high, max }
type Thinking = { effort?: Effort, budget_tokens?: Int, off?: Bool }
type Sampling = { }  // fields: chapter 05
enum ToolChoice { auto, no_tools, required, Named { name: Str } }
type GenOpts = { model?: ModelRef, effort?: Effort, thinking?: Thinking, sampling?: Sampling,
                 tool_choice?: ToolChoice, repairs?: Int, timeout?: Duration, title?: Str }
type Usage = { prompt_tokens: Int, completion_tokens: Int, cached_tokens: Int,
               reasoning_tokens?: Int, cost?: Money, model: Str, duration: Duration }
type ModelCard = { id: Str, spec: Str, location: Location, tier: Tier, tags: [Str], reasoning: Bool,
                   images: Bool, window?: Int, prices?: Prices, local: Bool, fits_now?: Bool,
                   card_text?: Untrusted<Str> }    // ids and specs come from the user's model table

// Prompts and conversations (chapter 05, context)
// Prompt, TrustedPrompt (values); Conversation, Mark, Pin (handles); Transcript (value); Image (value)
type Message = { role: Role, text: Untrusted<Str>, images: [Image], tool_calls: [ToolCall], tool_call_id?: Untrusted<Str> }
enum Role { system, user, assistant, tool }
type ToolCall = { id: Untrusted<Str>, name: Untrusted<Str>, arguments: Json }   // written by a model
type ToolResult = { call_id: Untrusted<Str>, name: Untrusted<Str>, outcome: Outcome, text: Untrusted<Str> }
enum Outcome { ok, failed, Denied { by: DeniedBy }, Blocked { hook: Str }, Refused { reason: Str } }
enum DeniedBy { rule, mode, floor, user, hook, profile, attenuation, unattended, secrecy, sees }
type ModelReply = { text: Untrusted<Str>, tool_calls: [ToolCall], usage: Usage, finish: Finish }
enum Finish { stop, tool_calls, length, content_filter, cancelled }

// Frames (chapter 04)
type Sees = { turn?: Bool, attachments?: Bool, plan?: Bool, memory_environment?: Bool,
              memory_index?: Bool, agents?: Bool, system_default?: Bool, state?: Bool, secrets?: Bool }

// Tools
// ToolName: a tool's name, checked against the universe; ToolSet: a set of names and patterns
type FileText = { path: Untrusted<Str>, text: Untrusted<Str>, first_line: Int, last_line: Int,
                  total_lines: Int, more: Bool, next_offset?: Int }

// Budgets and limits (chapter 04)
type Budget = { tokens?: Int, cost?: Money, wall?: Duration, steps?: Int,
                model_calls?: Int, tool_calls?: Int }
type Limits = { depth?: Int, width?: Int, heap?: Int }
type Spent = { tokens: Int, cost: Money, wall: Duration, steps: Int, model_calls: Int, tool_calls: Int }

// Plans: the shape of Imperium's update_plan (docs/03-agent-engine.md#dispatch-contract)
type Plan = { goal: Str, steps: [PlanStep], assumptions: [Str], open_questions: [Str] }
type PlanStep = { title: Str, status: StepStatus, depends_on: [Str], owner?: Str, acceptance?: Str }
enum StepStatus { pending, in_progress, done, blocked, skipped }

// Results and stopping
enum Result<T> { Ok { value: T }, Err { error: Error } }
enum JsonType { null, bool, number, string, array, object }          // json.type_of
type Violation = { pointer: Untrusted<Str>, message: Untrusted<Str> }   // json.validate: where, and why
type Scored<T> = { value: T, score: Float, reasons?: Untrusted<Str>, samples?: Int }   // samples: set by think.best_of
enum Fold<A> { Next { acc: A }, Stop { acc: A } }                  // par_fold (control)
enum FoldOrder { index, completion }
type Folded<A> = { value: A, delivered: Int, stopped: Bool }
enum StreamStop { done, until, max_tokens }                        // model.stream
type Streamed = { text: Str, stopped: StreamStop }
// Stop and Round: chapter 05
enum Backoff { fixed { delay: Duration }, exponential { start: Duration, factor: Float, max: Duration } }
                                                                   // retry (control)
enum CacheScope { turn, frame, session }                           // cached (control)
// Countable (model.count_tokens): a bound, not a type: Str or Untrusted<Str>, Prompt, Conversation
```

`ModelReply` is the reply of one chat model call; `Reply` is what a `@turn` entry returns. A
model reply is not yet the turn's answer. Every string in these types that an effect produced is
untrusted unless it is an engine-generated identifier (chapter 03, the effect rule); in
particular, a `ToolCall`'s name and id are what the model wrote, and a plan's text is
`Untrusted<Plan>` whenever it comes from `session.plan()` or a model.

## Naming and argument conventions

- Functions that make model calls take `opts: GenOpts = {}` last; its fields override the frame's
  defaults for that call only.
- Functions that take a collection take it first, so they read well with `|>` and as methods.
- Functions that can return nothing return `T?` (`list.find`, `map.get`); functions whose argument
  must exist raise `NotFound` (`memory.read`, `meta.harness`).
- Paths are `Str`. They are passed to the engine as written and resolved by it under its folder
  and grant rules (`docs/05-tools-and-isolation.md#isolation-tiers`); Ratio never resolves a path
  against the file system itself.
- Every built-in that makes model calls says how many in its row, as `model ×n`.
- A parameter whose name is a reserved keyword (`prompt`, `type`, …) can be passed only by
  position, since a named argument is an identifier (chapter 02): `session.start`'s `prompt`,
  `tools.media_image`'s `prompt`. A parameter added later is not given a keyword's name, and
  neither is a field of a shared type: a record literal cannot write one.
- An optional callback is written `f: fn(A) -> B = none`: it may be passed or left out, not
  passed as `none`. (`(fn(A) -> B)?` would be an option of a one-element tuple, chapter 02.)
- A type parameter named `S` is always the text bound of `data`; other type parameters use
  other names (`N` for the states of `think`'s searches, `State` for the package state).
- Every rule a built-in imposes on its arguments also holds when it is used as a function value.
  A built-in whose rule can be checked only where it is called may only be called, never used as
  a value (the checker refuses naming one otherwise, `C0036`): every **trust site** (its type and
  reason are fixed at the call), every **source or sink** whose rule depends on the call's
  written arguments, and every **generic secrecy sink** (a secret type argument is
  visible only at the call). Today these are `trust`, `as_task` and `declassify` (trust sites),
  `session.ask` (its answers are labelled from the questions written at the call),
  `meta.harness` (each load is listed by the name it is called with), `state.get`, `set`,
  `update` and `reset` (their type is the package's declared state, fixed at the call), and the
  generic secrecy sinks `model.rank` and `think.consensus` (their items reach a model).

## Built-in prompts

Built-ins in `model` and `think` that make model calls use prompt texts shipped with the standard
library (`std/prompts/*.md` in the crate), one per built-in. They are part of the IR hash, so an
eval records exactly which wording ran. To change a built-in's wording, a harness writes it in
Ratio (each is a short program over `model.*`); the rows list no `instructions` parameter yet.
The output schema stays fixed.
