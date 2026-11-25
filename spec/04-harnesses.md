# 04 Harnesses as functions

## Decision R9: a harness call is a function call with its own frame

A harness is declared like a function and called like one. What makes it a harness is that the
call opens a **frame**: a bounded context with its own model settings, tools, permissions,
budget, view of the session (`sees`) and, when it needs one, conversation. Reasoning methods
compose because a frame is just a call: a debate is two harness calls and a judge call; a tree
search is a recursive harness; self-critique is a harness that calls a critic harness.

Alternatives considered:

| Option | Why not |
| --- | --- |
| Every harness call is an engine subagent (`spawn_agent`) | Subagents are heavyweight by design (task ids, kept conversations, Agents panel, `await_tasks`, today depth 1); a vote over five samples would be five subagents. Subagents remain available through `agents.spawn` when that is what is wanted |
| Harnesses as plain functions sharing the caller's conversation and settings | No isolation: a critic would see the draft author's reasoning and be anchored by it; overrides would leak into the caller; budgets could not be bounded per method |
| Message-passing actors | Expressive but unstructured; cycles and lifetimes would need the rules Imperium's knowledge agents avoid on purpose (`docs/21-knowledge-agents.md#decisions`, K3) |

## Declaring a harness

```ratio
/// A strict reviewer that only reads.
pub harness reviewer(diff: Untrusted<Str>, focus: Str = "correctness") -> Untrusted<Review>
  with {
    model: model.route(tier: strong, tags: ["code"]),
    effort: high,
    sampling: { temperature: 0.2 },
    tools: [read_file, grep, glob],
    permissions: { mode: plan },
    budget: { tokens: 80k, wall: 5m },
    context: fresh,
    system: reviewer_system(focus),
  }
{
  let conv = context.current()
  conv.append_user(prompt { instruct "Review this diff."; data diff as "diff" })
  model.loop(conv, max_steps: 12)
  model.chat_as::<Review>(conv, instructions: prompt { instruct "Write the review now." })
}
```

Parameters are typed, may have defaults, and are passed positionally or by name. The return type
is required. A harness may be `pub` (importable) or private to its file.

### The configuration record

Every field is optional. A field left out is inherited from the caller's frame (or, for an entry,
from the session or the spawning parent, chapter 08), except `sees` and `context`, whose defaults
are given below.

| Field | Type | Meaning |
| --- | --- | --- |
| `model` | `ModelRef` | A model table id (`"consul"`), a spec (`"llamacpp:qwen3-4b"`), `model.route(tier:, location:, tags:)` for the router to pick (`docs/04-models-and-routing.md#routing`), `inherit`, or `session` (the session's current model) |
| `effort` | `Effort` | `low`, `medium`, `high`, `max`, sent only to models that can reason (`docs/04-models-and-routing.md#reasoning-effort`) |
| `thinking` | `Thinking` | Effort plus an optional token budget for backends that take one (chapter 05) |
| `sampling` | `Sampling` | Defaults for model calls in the frame (chapter 05) |
| `tools` | `ToolSet` | A list of tool names, all required; or `{ need: [...], want: [...] }`, where `want` tools are dropped with a log line when the caller lacks them; tool names may be patterns such as `"mcp__github__*"` |
| `permissions` | `PermissionRequest` | `mode`, extra `deny` and `ask` rules, `paths` (folders the file tools are confined to), `location` (`local` keeps every model call on this machine) |
| `budget` | `Budget` | `tokens`, `cost`, `wall`, `steps`, `model_calls`, `tool_calls`; each a cap |
| `limits` | `Limits` | `depth`, `width`, `heap` |
| `context` | `ContextMode` | `fresh` (default), `fork`, `none`, or `main` (entries only) |
| `sees` | `Sees` | What the frame may read of the session besides its arguments (below); default empty for `fresh` and `none` frames |
| `system` | `TrustedPrompt` | The system prompt of the frame's conversation; trusted text only |
| `title` | `Str` | Shown on the frame's events, in the transcript and in the Agents panel; defaults to the harness name |
| `attended` | `Bool` | `false` makes the frame treat the session as unattended (no questions to the user); `true` is refused if the caller is unattended |

Tool names in `tools` are resolved by the checker against the tool universe it knows (Imperium's
built-in tools and the `mcp__<server>__<tool>` form); an unknown literal name is a warning at
load time (an MCP server may connect later) and resolves at call time.

**Where configuration is evaluated.** The declared `with { … }` of a harness is evaluated at each
call, after the arguments, in the harness's own lexical scope with its parameters bound (so
`system: reviewer_system(focus)` can use `focus`). It must be effect-free: the checker refuses
calls to effectful built-ins and harnesses in it. A call's override `f(x) with { … }` is evaluated
in the **caller's** frame, after the arguments and before the declared configuration; it may
make effects, charged to the caller. Overrides win over declarations field by field.

## Calling a harness

```ratio
let review = reviewer(diff)                                       // its own settings
let quick  = reviewer(diff, focus: "style") with { effort: low, budget: { tokens: 10k } }
```

A call:

1. Evaluates the arguments, then the call's overrides, in the caller's frame.
2. Evaluates the declared configuration with the parameters bound.
3. **Opens the callee's frame** by combining, field by field, the caller's frame, the declared
   configuration and the overrides, then attenuating the result against the caller (below). If a
   required tool, model location, permission or `sees` item cannot be had, the call raises
   `Denied { by: attenuation }` before anything runs.
4. Runs the body. Model and tool calls in the body are charged to the callee's frame and, through
   it, to every ancestor.
5. Closes the frame: a `fresh` or `fork` conversation is discarded (it remains in the journal as
   the harness agent's events), unspent budget returns to the caller, deferred blocks run.
6. Returns the value, or propagates the error.

Harnesses are values. Their type is `harness(A, B) -> R`, which is a subtype of `fn(A, B) -> R`,
so a harness can be passed wherever a function is expected (`think.debate(sides: [pro, con], …)`).
Overrides with `with { … }` are allowed only on calls whose callee has a harness type.

## What a callee sees

**Isolation by default.** A callee sees its arguments, its own system prompt, its own
conversation (empty with `context: fresh`), the tools and permissions its frame allows, and the
parts of the session its `sees` grants. It does **not** see the caller's conversation,
scratchpads, local variables, or the results of the caller's earlier tool calls, except as
arguments. A critic that is to judge a draft gets the draft, not the reasoning that produced it.

**`sees`.** Ambient reads, the built-ins that read session state without a tool call, are gated
by the frame's `sees` capability, a record of booleans:

| `sees` item | Gates |
| --- | --- |
| `turn` | `session.turn()` (the message and payloads) |
| `attachments` | the turn's attachments, through `session.turn()` |
| `plan` | `session.plan()`, `session.state()` |
| `memory_environment` | `memory.environment()` |
| `memory_index` | `memory.index()`, `memory.used()` |
| `agents` | `agents.list()`, `agents.status()` reports, `agents.knowledge()` cards |
| `system_default` | `context.system_default()` |
| `state` | `state.get()` (writes need `state` and an entry's own frame or its `fork` frames) |
| `secrets` | receiving `Secret<T>` values as arguments or results (chapter 03) |

Defaults: an entry's own frame sees everything; a `fork` frame inherits its caller's `sees`
(it already holds the caller's conversation); a `fresh` or `none` frame sees nothing unless its
configuration asks. A request is attenuated like every other dimension (`sees ⊆ caller's`). A
gated read outside `sees` raises `Denied { by: sees }`. Memory search and read, being tools, are
governed by the tool set instead.

**Context modes.**

| `context` | The callee's conversation | Use |
| --- | --- | --- |
| `fresh` | New and empty, with the frame's `system` prompt; created on first use | The default |
| `fork` | A copy of the caller's conversation as it is at the call; the callee appends to its copy, which is discarded at return | Continuing someone else's work, or reusing a local model's cached prefix |
| `none` | No conversation; `model.chat` and `context.current()` raise `Invalid`; single-shot `model.generate*` calls still work | Pure transformations and judges |
| `main` | The agent's main conversation: the session's root conversation for a `@turn` entry, the subagent's kept conversation for a `@task` entry | Only an entry's own frame; refused elsewhere |

Explicit passing is the other way to share context: `context.excerpt(conv, last_turns: 4)` gives
a `Transcript` value (an immutable snapshot of messages) that a callee takes as an argument and
puts in a `context` prompt section. A transcript's message texts are `Untrusted`, except user
messages from the session's own user, which keep their `UserText` status.

There is no shared mutable conversation between frames. Two writers on one conversation would
break the pairing of tool calls and results and make prompts depend on timing.

## Decision R10: attenuation only

A callee can never have more authority or more resources than its caller. The rule is applied
per dimension, at frame opening, and again by the engine at every effect.

| Dimension | Callee gets | Notes |
| --- | --- | --- |
| Tools | Requested ∩ caller's | A missing required tool raises `Denied { by: attenuation }`; a missing `want` tool is dropped |
| Permission mode | `meet(requested, caller's)` | Order: `plan` < `ask` < `default` < `auto`, `accept_edits` < `full`; `meet(auto, accept_edits)` is `default` |
| Rules | Caller's rules plus the callee's `deny` and `ask` rules | A callee cannot add `allow` rules; deny still wins over ask over allow |
| Paths | Requested ∩ caller's folders and grants | Only narrows |
| Model location | `local` if either is `local` | A caller confined to this machine cannot reach a hosted model through a callee |
| Model choice | Any model the frame's location allows | Cost is bounded by the budget, not by forbidding models |
| Effort, sampling | As requested | Bounded by budget |
| Budgets | `min(requested, caller's remaining)` per budget kind | Spending debits every ancestor; unspent returns |
| `sees` | Requested ∩ caller's | Defaults above |
| Depth | Caller's depth + 1 ≤ `limits.depth` | Default limit 8 |
| Width | ≤ caller's width | Concurrent branches in the frame tree |
| Attended | Caller's and requested | An unattended frame cannot ask the user anything. Frames of a subagent are never attended: a subagent asks its dispatcher by raising `Question` |
| Memory writes | Only frames of the root agent's turn, and only if the caller has them | Subagents never write memory (`docs/17-memory.md#editing-and-forgetting-implemented`) |
| Subagents | A subagent spawned from a frame is bounded by the frame: its tools, mode, rules, paths, location and `sees` are at most the frame's, and its budget is carved from the frame's | Chapter 06, `agents.spawn` |
| Approvals already given | Shared | "Allow for this session" approvals are the session's; a callee does not ask again for what the session already allowed, but its own `ask` rules still ask |

The engine enforces the same bounds independently: each effect carries the frame's tool set,
mode, extra rules, paths, location and secrecy flag, and the engine's dispatch refuses anything
outside them (chapter 08, chapter 10). So a bug in the interpreter's attenuation cannot widen
authority; it can only refuse more.

**Budgets in detail.** Each frame has a budget for each kind (`tokens`, `cost`, `wall`, `steps`,
`model_calls`, `tool_calls`). A request is a cap, not a reservation: a callee with
`budget: { tokens: 50k }` may spend at most 50k tokens and at most what its caller has left. Every
spend is checked against the frame and all its ancestors, up to the session's own budgets
(Imperium's `BudgetGate`, in `imperium-core`), before the effect runs; the effect is not started
when it could not be paid for. When a frame's budget runs out, it raises `Budget { which }`, which
only its caller can catch (chapter 03); the caller can then use a partial result or fall back.

## Recursion and depth

Harnesses may call themselves and each other recursively. Each call adds one to the frame
depth; beyond `limits.depth` (default 8, at most 32) the call raises `Budget { which: depth }`.
Plain function calls are bounded separately by the call-depth limit (chapter 07). Recursion is
also bounded by steps, tokens and wall clock, so deep recursion over cheap pure code and shallow
recursion over expensive model calls both end.

Engine subagent depth is separate: `agents.spawn` follows Imperium's own limits (today depth 1:
subagents cannot delegate, `docs/03-agent-engine.md#reactive-delegation`; the configured maximum
later). A subagent's `@task` entry starts at frame depth 0 within its own agent, but cannot spawn
further subagents while the engine forbids it.

## Harness agents

A frame whose conversation is not `none` and that calls a model is registered with the engine as
a **harness agent**:

- Its id is the caller agent's id plus a suffix (`root.h3`, `t2.h1`), unique in the session.
- It has a title (the configuration's `title`, else the harness name), a parent (the agent whose
  turn it runs in), its package and harness name, its frame path, its model, and the usage it
  spent.
- It appears in the desktop's **Agents panel** while it runs, nested under its parent, with its
  title, model, depth, spend and status, and stays listed (as finished) until the turn ends. The
  panel's entry links to the live harness view (chapter 08).
- Its events (`text_delta` is suppressed by default; `model_call_completed`, `context_breakdown`,
  `tool_started`, `tool_finished`, `notice`) carry its id.
- On a local model it is the slot holder for its calls, so its cached prompt is its own and does
  not evict the caller's (`docs/04-models-and-routing.md#runtimes`).
- It is ephemeral: it is not resumable, not offered to `resume_agent`, and its conversation is not
  kept beyond the journal's events.

Frames with `context: none` make single-shot calls under their caller's agent id with a `frame`
field naming the harness; the Agents panel shows them as rows of their parent's frame stack.

## Questions crossing frames

A callee that cannot continue without an answer raises `Question { question, options }`, built
with `question(text, options)` (chapter 06, `control`); `think.clarify` raises it in an
unattended frame. Both fields are untrusted text.

1. **Between frames.** The caller may catch it, decide from what it knows, and call again with the
   decision as an argument; or let it propagate.
2. **At a `@turn` entry in an attended session.** The engine asks the user through Imperium's
   question prompt (`questions_asked`). The question and options are shown **wrapped**: under a
   line naming the harness that asks, as quoted text, never as the prompt's own wording; options
   are displayed as quoted choices with no recommendation. The user's typed or chosen answer is
   the user's own act and becomes `UserText`, appended to the main conversation with the question
   as a framed note; the turn then ends with status `question` answered, and the next turn begins
   with it. A blank answer or the prompt's 10-minute timeout **fails** the question: the turn ends
   with `ReplyStatus.question`, the reply states the open question (quoted), and no option is ever
   taken as the user's answer.
3. **At a `@turn` entry in an unattended session.** No prompt is shown; the turn ends with
   `ReplyStatus.question` and the quoted question, as Imperium's unattended runners report open
   questions.
4. **At a `@task` entry.** The subagent's turn ends with `Report { ok: false, question }`; the
   engine routes it to the dispatcher as Imperium escalates questions up the tree
   (`docs/03-agent-engine.md#reactive-delegation`). A native-loop dispatcher reads it framed as
   data from the subagent; a Ratio dispatcher gets `TaskReport.question` as `Untrusted<Str>`. The
   dispatcher answers by resuming the subagent (`agents.resume` / `resume_agent`), whose next turn
   receives the answer as its `Task.brief` with `follow_up: true`. This is the hand-back path
   roadmap item 64 needs (chapter 08).

## Versioning

A harness is identified by its package's source-qualified name and version
(`meta { version: "1.4.0" }`, semver) and by its IR hash. Imports by name may pin a range;
`meta.harness::<H>("std:debate@^1")` resolves by name at run time against the registry, under the
same trust rules as imports (chapter 03). A package's public harness signatures are its API: the
registry warns when a newer version of a package changes a public signature without a major
version bump. A package selected as a session or agent harness is **pinned by IR hash** at
selection (chapter 08): editing its files changes nothing until the user selects it again.
