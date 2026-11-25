# 06 Standard library

The standard library is how a harness does anything: every model call, tool call, delegation,
memory access, user interaction and clock read is a built-in. It is organised in modules that are
always in scope (chapter 03, modules). This page gives the conventions and the shared types; each
module has its own page.

| Module | Page | What it covers | Built-ins |
| --- | --- | --- | --- |
| `model` | [model.md](model.md) | Generation, chat, structured output, classification, extraction, judging, ranking, embeddings, tokens, routing | 27 |
| `think` | [think.md](think.md) | Reasoning methods: steps, plans, critique, revision, verification, voting, debate, search, confidence, clarification | 30 |
| `tools` | [tools.md](tools.md) | One built-in per Imperium tool, plus dispatch of model-chosen calls | 46 |
| `agents` | [agents.md](agents.md) | Subagents, knowledge agents, named agents | 18 |
| `memory` | [memory.md](memory.md) | Memory entries by scope, and session search | 12 |
| `context` | [context.md](context.md) | Conversations, transcripts, pins, compaction, notices, token budgets, safe points, large inputs | 31 |
| `session` | [session.md](session.md) | The user, approvals, notifications, plans, state, checkpoints, branches | 22 |
| `data` | [data.md](data.md) | Text, regex, JSON, lists, maps, math, random, time, hashing, diff, templates, vectors, paths, URLs, codecs (15 submodules) | 212 |
| `control` | [control.md](control.md) | Retry, timeouts, races, fan-out, rate limits, caches, assertions, errors, budgets, logging, tracing | 27 |
| `eval` | [eval.md](eval.md) | Tests, metrics, fixtures, the record and replay seam | 14 |
| `meta` | [meta.md](meta.md) | Introspection of tools, models, harnesses, frames and permissions; loading harnesses by name | 18 |
| **Total** | | | **457** |

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
| `effect` | A recorded effect with no authority (clock, randomness, logging, events). Never asks |
| `engine` | Reads engine state (the frame, the conversation, agent records, the model table). Never asks |
| `model` | One or more model calls (the row says how many). Charged to `tokens`, `cost` and `model_calls`; subject to the frame's model location. Never asks, except that a session budget configured to ask before spending asks as it does for any model call |
| `tool:<name>` | Dispatched as Imperium's tool `<name>` through the engine's own path: permission profile, frame tool set, rules (deny over ask over allow), safety floor, permission mode, `tool.before` and `tool.after` hooks, approval, events. Decided exactly as a model's direct call of that tool would be; reads run without asking unless a rule says otherwise |
| `user` | Talks to the person: only in attended frames; in an unattended frame it raises `Denied { by: unattended }` or, where noted, uses the recommendation |
| `memory.write` | A memory write: only in frames of the root agent's turn, under the scope's `memory.write.<scope>` policy (`ask` by default); refused in plan mode |
| `delegate` | Creates or controls engine subagents; subject to Imperium's delegation limits (today: root only, depth 1) |
| `meta` | Reads the registry or the frame's own description; loading a harness by name is subject to the trust rules of chapter 03 |

Every effect is also charged to `steps`, and every effect that reaches the engine is subject to
the frame's attenuated tool set, mode, rules, paths and location (chapter 04), checked by the
engine itself.

## Shared types

These types appear across modules. Text fields of anything produced outside the program are
`Untrusted` as described in chapter 03.

```ratio
// Turns and tasks (entries, chapter 01)
// UserText: the user's own message; trusted, and a subtype of Str
type Turn = { message: UserText, attachments: [Untrusted<Attachment>], index: Int, resumed: Bool }
type Attachment = { name: Str, kind: Str, text?: Str, path?: Str, bytes: Int }
type Reply = { text: Untrusted<Str>, status: ReplyStatus }
enum ReplyStatus { done, step_limit, question, stopped }
type Task = { brief: Untrusted<Str>, title: Str, access: Access, profile?: AgentProfile, follow_up: Bool }
type Report = { text: Untrusted<Str>, ok: Bool, question?: Untrusted<Str> }
enum Access { read, write }
type AgentProfile = { name: Str, instructions: Str, workflow?: Str }   // from an agent file

// Models
enum ModelRef { session, inherit, Named { id: Str }, Routed { tier?: Tier, location?: Location, tags: [Str] } }
                                       // a Str literal coerces to Named { id }
enum Tier { basic, standard, strong, frontier }
enum Location { local, cloud, any }
enum Effort { low, medium, high, max }
type Thinking = { effort?: Effort, budget_tokens?: Int, off?: Bool }
type Sampling = { /* chapter 05 */ }
type GenOpts = { model?: ModelRef, effort?: Effort, thinking?: Thinking, sampling?: Sampling,
                 repairs?: Int, timeout?: Duration, title?: Str }
type Usage = { prompt_tokens: Int, completion_tokens: Int, cached_tokens: Int,
               reasoning_tokens?: Int, cost?: Money, model: Str, duration: Duration }

// Prompts and conversations (chapter 05, context)
// Prompt, Conversation (handle), Transcript (value), Mark (handle)
type Message = { role: Role, text: Untrusted<Str>, tool_calls: [ToolCall], tool_call_id?: Str }
enum Role { system, user, assistant, tool }
type ToolCall = { id: Str, name: Str, arguments: Json }
type ToolResult = { call_id: Str, name: Str, outcome: Outcome, text: Untrusted<Str> }
enum Outcome { ok, failed, Denied { by: DeniedBy }, Blocked { hook: Str } }
enum DeniedBy { rule, mode, floor, user, hook, profile, attenuation, unattended }
type ModelReply = { text: Untrusted<Str>, tool_calls: [ToolCall], usage: Usage, finish: Finish }
enum Finish { stop, tool_calls, length, content_filter, cancelled }

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
type Scored<T> = { value: T, score: Float, reasons?: Untrusted<Str> }
// Stop: chapter 05
```

`ModelReply` is the reply of one chat model call; `Reply` is what a `@turn` entry returns. A
model reply is not yet the turn's answer.

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

## Built-in prompts

Built-ins in `model` and `think` that make model calls use prompt texts shipped with the standard
library (`std/prompts/*.md` in the crate), one per built-in. They are part of the IR hash, so an
eval records exactly which wording ran. Every such built-in takes `instructions: Prompt?` to
replace its default wording; the output schema stays fixed.
