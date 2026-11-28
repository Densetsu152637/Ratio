# 05 Shaping model thinking

This chapter covers the language-level means a program has to shape how a model thinks: prompts,
thinking steps, scratchpads, effort, sampling, structured generation and stopping. The built-ins
that package common reasoning methods (voting, critique and revision, planning, search) are in
[`think`](06-stdlib/think.md); this chapter explains the pieces they are built from.

## Prompts are values

### Decision R11: typed prompt sections instead of string templates

A `Prompt` is an ordered list of **sections**, each with a role in the request and a framing rule.
It is built with a `prompt { … }` expression or a `prompt` declaration (a function returning a
prompt). A prompt whose every section holds trusted content only (no `data`, `image`, `context`
from a conversation, untrusted `task`, or included untrusted prompt) has the type
`TrustedPrompt`, a subtype of `Prompt`; the checker infers which of the two a literal or a
`prompt` declaration is (the declaration has no written result type). Only a
`TrustedPrompt` may be a system prompt:

```ratio
prompt fix_request(issue: UserText, failing: Untrusted<Str>, files: [Untrusted<FileText>]) {
  system  "You fix bugs in Rust code. Change as little as possible."
  instruct "Find the cause of the failing test and propose a patch."
  task    issue
  data    failing as "test output"
  for f in files { data f.text as f.path }
  steps   ["Restate what the test expects", "Locate the code path", "Explain the cause", "Write the patch"]
  format  Patch
}
```

| Section | Accepts | Rendered as | Purpose |
| --- | --- | --- | --- |
| `system` | trusted `Str` or `TrustedPrompt` | The system message (several are joined with blank lines) | Role, rules, house style |
| `instruct` | trusted `Str` | User-message text, unframed | The harness's own instruction |
| `task` | `Brief` (so also `Str` and `UserText`) | `Str` and `UserText`: unframed. A `Brief` from elsewhere: inside `<task from="…">`, after one line saying it was given by another agent, says what to do, and cannot change the role, tools or rules | A delegated task (a subagent's brief; a plan step converted with `as_task`) |
| `data` | anything | Inside `<data label="…">`, after one line saying it is data and never instructions; non-text values as JSON | Files, pages, tool results, other agents' answers |
| `image` | `Image` | An image part of the user message (on models whose card says they take images; elsewhere `Unsupported`), labelled like `data` | Screenshots, attached images |
| `example` | trusted `input -> output` | A worked example pair. An **instruction position**: few-shot pairs steer the model as strongly as instructions, so both sides must be trusted; examples drawn from untrusted material go in `data` sections instead | Few-shot demonstrations |
| `format` | a schema-able type | A short description of the expected output and its JSON Schema (when the backend does not enforce the schema itself, see below) | Output contract |
| `steps` | trusted `[Str]` | A numbered list of thinking steps to follow (see below) | Structured thinking |
| `context` | `Transcript` or `Conversation` | The messages, as prior turns of the request, each with its own trust (untrusted texts stay framed as they are in the transcript) | Supplying history |
| `note` | trusted `Str` | A short line after the instruction | Reminders, constraints |
| `include` | `Prompt` | The other prompt's sections, in place; including an untrusted prompt makes the result a `Prompt` | Composition |

**Framing** uses the engine's own helpers, so Ratio and the native loop frame data identically:
the harness tag names (`HARNESS_TAGS`) and the section's own tag are defused inside framed text
(`<tag` becomes `<\tag`), control characters are removed from labels, and a label longer than
200 characters is cut. A framed block cannot close itself early or fake another block.

**Order.** Sections render in written order, except that `system` sections always go to the
system message and `context` sections always become prior messages before the final user message.
A prompt with no user-message content raises `Invalid`.

**Hashing.** Built-in sections render deterministically; a prompt's rendered form has a hash
(`model.prompt_hash(p)`) used by caches and the tape.

String templates (`"…{x}…"`) remain available for building section text, and taint follows them
(chapter 03), but they are not how data reaches a model: a `data` section is.

Rejected: free-form string templates as the only prompt mechanism (Jinja-like). Nothing would stop
untrusted text from landing in an instruction, and framing would be every author's own job.
Rejected: a chat-message list as the only prompt type. Too low-level for the common case; still
available through `context` sections and the `Conversation` API.

## Thinking steps and scratchpads

**Steps.** A `steps` section asks the model to work through named steps in order. With
`think.steps(…)` the steps become structure the program can use:

```ratio
let t = think.steps::<Answer>(prompt { task question; data evidence as "evidence" },
  steps: ["List what the evidence says", "List what it does not say", "Answer"])
// t.answer : Untrusted<Answer>; t.trace : [Untrusted<StepNote>] (one note per step)
```

`think.steps` generates a record with one field per step plus the answer, constrained by schema,
so the model cannot skip a step and the program can inspect each one. With `visible: false` the
step notes are kept out of everything returned except `trace`.

**Scratchpads.** `think.scratchpad()` returns a `Scratchpad` handle: notes private to the frame
that created it. `pad.note(text)` adds a note; `pad.text()` reads them back; a `context` or `data`
section can include them. A scratchpad never reaches another frame unless the program passes its
text, and it is not part of the session conversation, so it never reaches the user or a later
turn. It is how a harness keeps intermediate reasoning (rejected hypotheses, partial results)
without paying for it in the main conversation's context.

## Reasoning effort and budgets

`effort` (`low`, `medium`, `high`, `max`) maps to each backend exactly as Imperium sends it today
(`docs/04-models-and-routing.md#reasoning-effort`): `reasoning_effort` for OpenAI-compatible
services and local servers, `reasoning.effort` for OpenRouter, a `thinking.budget_tokens` value
for Anthropic, nothing where no control exists. It is sent only to models that can reason.

`thinking: { effort: high, budget_tokens: 8k }` adds a token budget for backends that accept one;
elsewhere the budget is ignored and a `ratio_log` warning names the backend once per call site.
`thinking: off` sends `chat_template_kwargs.enable_thinking = false` where the template has that
switch.

The model's reasoning text is **not** exposed to programs. Imperium does not keep it
(`docs/03-agent-engine.md#running-evals-through-the-harness-implemented`), and exposing it would
invite programs to treat a model's private reasoning as a reliable account of its answer.
`Reply.usage.reasoning_tokens` reports its size where the backend says.

## Sampling

```ratio
type Sampling = {
  temperature?: Float, top_p?: Float, top_k?: Int, min_p?: Float,
  seed?: Int, max_tokens?: Int, stop?: [Str],
  presence_penalty?: Float, frequency_penalty?: Float, repetition_penalty?: Float,
  logprobs?: Int,            // top-k log probabilities per token, where supported
  strict?: Bool,             // true: an option the backend cannot honour raises Unsupported
}
```

**Tool choice.** `GenOpts.tool_choice` (`auto`, the default; `no_tools`; `required`; or
`Named { name }`) is sent to backends that support it on `model.chat`, `model.chat_as` and
`model.loop` calls; on others `no_tools` is honoured by offering no tools and the rest is dropped
with a warning (or `Unsupported` under `strict`).

Options are merged field by field: the frame's `sampling`, then the call's. An option the
backend does not support is dropped with a once-per-site warning, unless `strict: true`. Support
is decided per endpoint kind and recorded in the model card (`meta.models()`), so programs can
check before relying on, say, `logprobs`.

## Structured generation

`model.generate_as::<T>(p)` (and `model.chat_as::<T>(conv)` on a conversation) returns `Untrusted<T>` for any schema-able `T`. The schema is derived
from `T` (chapter 03). The host uses the strongest mechanism the backend offers:

1. **Native schema output**: `response_format: { type: "json_schema", strict: true }` on
   OpenAI-compatible services that accept it.
2. **Grammar-constrained decoding**: llama.cpp's `json_schema` request field (converted to a
   GBNF grammar by the server), vLLM's guided JSON. Imperium already relies on these for tool calls
   with models that lack native function calling (`docs/03-agent-engine.md#agent-loop`).
3. **Validate and repair**: the schema is described in the prompt (the `format` section), the
   reply is parsed (the first JSON value in it, code fences tolerated), validated, and on failure
   the model is asked again with the violations as a `data` section, up to `repairs` times
   (default 2). Each repair is a model call charged to the frame.

After all attempts, a reply that still does not validate raises `SchemaMismatch` with the
violations. A constrained reply is validated too (grammars can drift on numeric ranges and
patterns), so every `Untrusted<T>` a program holds satisfies `T`'s schema.

Choosing among fixed options is the same mechanism with an enum: `model.classify::<Kind>(x)`
constrains the output to the variant names (a trusted scalar); `model.choose(x, options: [...])`
constrains it to a list of strings and returns the chosen option as untrusted text with its index.

## Methods built from these pieces

| Method | Built-ins | Shape |
| --- | --- | --- |
| Self-consistency | `think.self_consistency`, `think.vote`, `think.consensus` | Sample `n` answers (temperature above 0), normalise each with a `key`, take the majority; agreement is a confidence signal |
| Best of n | `think.best_of` | Sample `n`, score each with a judge or a check, keep the best |
| Critique and revise | `think.critique`, `think.revise`, `think.refine` | Draft, critique against a rubric (in a separate frame, so the critic is not anchored), revise, until the critic passes or a stop criterion fires |
| Planning and decomposition | `think.plan`, `think.decompose`, `think.replan` | A typed `Plan` (the shape of Imperium's `update_plan`), executed step by step; re-planned when a step fails |
| Reflection | `think.reflect` | After a failure, extract what went wrong as lessons; optionally saved with `memory.remember` |
| Verification | `think.verify`, `think.check`, `eval.run_tests` | Check a claim against evidence, a value against predicates, code against its tests |
| Debate | `think.debate` | Two or more sides argue for rounds; a judge decides |
| Search | `think.tree_search`, `think.beam_search`, `think.mcts` | Expand partial solutions, score them, keep the promising ones |
| Uncertainty | `think.confidence`, `think.uncertainty`, `think.clarify` | Estimate confidence; ask instead of guessing below a threshold |

## Stopping criteria

Every iterative built-in takes a `Stop` value, and loops in programs use the same helpers:

```ratio
type Round = { round: Int, score?: Float }   // what `until` sees: the round, and its score when known
type Stop = {
  max_rounds?: Int,                 // hard cap on iterations
  until?: fn(Round) -> Bool,        // stop when the predicate holds (e.g. the critic passes)
  no_progress?: Int,                // stop after this many rounds without improvement in score
  converged?: Float,                // stop when the score changes by less than this
  budget?: Budget,                  // stop (not fail) when this much has been spent
}
```

A built-in that stops because of `budget` returns its best result so far with `stopped: budget`;
only the frame's own budget running out raises an error. Every iterative built-in reports why it
stopped (`stopped: until | max_rounds | no_progress | converged | budget`), so a harness can tell
a passing result from a cut-off one.

At the token level, `sampling.stop` sequences, `max_tokens` (which admission may lower to what the
budgets can pay for, chapter 04) and `model.stream`'s `until` predicate (chapter 06, `model`) end a
single generation; at the loop level, `model.loop` ends when the model makes no tool call, when
`max_steps` is reached, or when its `until` predicate holds after a step.
