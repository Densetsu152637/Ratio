# `model`: calling models

Every model call goes through the engine's `ChatClient` for the chosen model
(`docs/03-agent-engine.md#agent-loop`), is charged to the frame's budgets, and emits
`model_call_completed` and (for calls on a conversation) `context_breakdown`. The model is the
frame's unless `opts.model` names another; routing and location rules apply as in chapter 04.

## Generation

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.generate` | `(p: Prompt, opts: GenOpts = {}) -> Untrusted<Str>` | Renders `p` (chapter 05) into a request of its own, without touching any conversation, and returns the reply text | `ModelFailed`, `Timeout`, `Invalid` (empty prompt) | `model ×1` |
| `model.generate_as` | `<T>(p: Prompt, opts: GenOpts = {}) -> Untrusted<T>` | Structured generation against `T`'s schema, using the strongest mechanism the backend has, then validation; repairs up to `opts.repairs` (default 2) | `SchemaMismatch`, `ModelFailed` | `model ×1..1+repairs` |
| `model.generate_n` | `(p: Prompt, n: Int, opts: GenOpts = {}) -> [Untrusted<Str>]` | `n` independent samples (the backend's `n` when it supports it, else `n` calls run in parallel within the frame's width); results in sample order. Temperature defaults to 0.8 when the frame sets none | `ModelFailed` (any sample), `Invalid` (`n` < 1 or > 64) | `model ×n` |
| `model.complete` | `(text: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Str>` | Raw text completion without a chat template, for endpoints with a completions route (local llama.cpp, vLLM) | `Unsupported`, `ModelFailed` | `model ×1` |
| `model.stream` | `(p: Prompt, on_text: fn(Untrusted<Str>) -> (), opts: GenOpts = {}) -> Untrusted<Str>` | As `generate`, calling `on_text` with each text delta as it arrives (in order, on the frame's task); returns the whole text. `on_text` may not make effects other than `session.stream` and logging | `ModelFailed`, `Invalid` (effectful callback) | `model ×1` |
| `model.logprobs` | `(p: Prompt, opts: GenOpts = {}) -> Untrusted<TokenText>` | Generates with token log probabilities: `{ text, tokens: [{ token, logprob, top: [{ token, logprob }] }] }` | `Unsupported` (no logprobs on this backend) | `model ×1` |

## Conversations and the agent loop

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.chat` | `(conv: Conversation, tools: ToolSet? = none, opts: GenOpts = {}) -> ModelReply` | Runs a safe point on `conv` (steering, background notices, due compaction), sends the conversation with the frame's system prompt and the offered tools (`tools`, default the frame's offered set, always intersected with the frame's tool set), appends the assistant message to `conv`, and returns it. On the session conversation it streams `text_delta` to clients | `ModelFailed`, `Invalid` (`conv` has unanswered tool calls, or belongs to another frame) | `model ×1` |
| `model.loop` | `(conv: Conversation, max_steps: Int = 50, tools: ToolSet? = none, until: fn(ModelReply) -> Bool = never, concurrent: Bool = true, opts: GenOpts = {}) -> LoopResult` | The tool-using agent loop: `model.chat`; if the reply has no tool calls, stop; else `tools.dispatch_all` the calls (concurrently when `concurrent` and the engine allows) and repeat; stop early when `until` holds after a step. Returns `{ text, steps, stopped: done \| max_steps \| until, last: ModelReply }` | `ModelFailed` | `model ×1..max_steps`, plus each dispatched tool's own permission |

`model.loop` is the native loop's body in one built-in; `std:default` (chapter 08) shows it
written out in Ratio, which is how a harness changes one part of it.

## Language tasks

Each of these uses a built-in prompt (replaceable with `instructions:`) and a fixed output schema.

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.classify` | `<E>(input: Untrusted<Str>, labels: E, opts: GenOpts = {}) -> Classified<E>` | Constrains the reply to one variant of the unit-only enum `E` (or one of a `[Str]` list, then `E` is `Str`); returns `{ label, confidence? }`, where confidence is the label's probability when the backend gives logprobs, else none | `SchemaMismatch`, `ModelFailed` | `model ×1` |
| `model.extract` | `<T>(input: Untrusted<Str>, what: Str? = none, opts: GenOpts = {}) -> Untrusted<T>` | Extracts a `T` from the input; `what` describes what to look for. Fields the input does not support must be absent (optional) rather than invented; the prompt says so | `SchemaMismatch`, `ModelFailed` | `model ×1..3` |
| `model.summarize` | `(input: Untrusted<Str>, max_words: Int = 150, focus: Str? = none, opts: GenOpts = {}) -> Untrusted<Str>` | A summary within `max_words` (checked; one retry if over by more than 20 %); `focus` says what to keep | `ModelFailed` | `model ×1..2` |
| `model.rewrite` | `(input: Untrusted<Str>, instruction: Str, opts: GenOpts = {}) -> Untrusted<Str>` | Rewrites the input as instructed (tone, length, format) | `ModelFailed` | `model ×1` |
| `model.translate` | `(input: Untrusted<Str>, to: Str, opts: GenOpts = {}) -> Untrusted<Str>` | Translates into the language named by `to` (a name or BCP 47 tag) | `ModelFailed` | `model ×1` |

## Judging, comparing, ranking

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.judge` | `(candidate: Untrusted<Str>, rubric: Str, opts: GenOpts = {}) -> Untrusted<Judgement>` | `{ verdict: pass \| fail \| unsure, score: Float (0..1), reasons }` against the rubric. The candidate is a `data` section; the rubric is an instruction, so it must be trusted | `SchemaMismatch`, `ModelFailed` | `model ×1` |
| `model.score` | `(candidate: Untrusted<Str>, criteria: [Criterion], opts: GenOpts = {}) -> Untrusted<Scores>` | Scores each `Criterion { name, description, weight }` from 0 to 1 with a reason; `total` is the weighted mean | `SchemaMismatch`, `ModelFailed` | `model ×1` |
| `model.compare` | `(a: Untrusted<Str>, b: Untrusted<Str>, criteria: Str, swap: Bool = true, opts: GenOpts = {}) -> Untrusted<Preference>` | `{ winner: a \| b \| tie, margin: Float, reasons }`. With `swap`, asks twice with the order reversed and reports `tie` when the two answers disagree (position bias control) | `SchemaMismatch`, `ModelFailed` | `model ×1..2` |
| `model.rank` | `<T>(items: [T], show: fn(T) -> Untrusted<Str>, criteria: Str, method: RankMethod = listwise, opts: GenOpts = {}) -> [Scored<T>]` | Orders items best first. `pointwise` scores each item alone (n calls); `listwise` ranks all in one call (up to 20 items, else falls back to pointwise); `pairwise` runs a comparison sort with `model.compare` (about n log n calls, capped by budget) | `SchemaMismatch`, `ModelFailed`, `Invalid` (empty) | `model ×1..n log n` |

## Embeddings and tokens

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.embed` | `(texts: [Untrusted<Str>], model: ModelRef? = none) -> [Vector]` | Embeds each text with `model`, else `memory.embedding_model` (`docs/17-memory.md#configuration`), in batches of 16 through `POST /embeddings`. `Vector` is `[Float]` normalised to unit length | `Unsupported` (no embedding model), `ModelFailed` | `model ×⌈n/16⌉` |
| `model.count_tokens` | `(x: Countable, model: ModelRef? = none) -> TokenCount` | Counts a `Str`, `Prompt` or `Conversation` for a model: `{ tokens, method: exact \| calibrated \| estimated }`, by the same machinery as context breakdowns (`docs/03-agent-engine.md#context-management`). Never loads a model | — | `engine` |
| `model.window` | `(model: ModelRef? = none) -> Int?` | The model's context window as Imperium knows it (the server's, the model table's, or a local model's planned window); `none` when unknown | — | `engine` |
| `model.render` | `(p: Prompt, model: ModelRef? = none) -> Transcript` | The messages a prompt would send, after framing, without sending them | `Invalid` | `pure` |
| `model.prompt_hash` | `(p: Prompt) -> Str` | `sha256:` of the canonical rendered prompt; equal prompts hash equal | — | `pure` |

## Choosing models

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `model.route` | `(tier: Tier? = none, location: Location? = none, tags: [Str] = [], min_context: Int? = none) -> ModelRef` | Builds a routed reference; nothing is resolved until a call uses it | — | `pure` |
| `model.resolve` | `(m: ModelRef) -> ModelCard` | Asks the router which model the reference would use now (the same choice `spawn_agent` would make), without loading it. `ModelCard`: `{ id, spec, location, tier, tags, reasoning, window?, prices?, local, fits_now?, card_text? }` | `NotFound`, `Denied { by: attenuation }` (location) | `engine` |
| `model.list` | `(query: Str? = none, location: Location? = none, limit: Int = 20) -> [ModelCard]` | The models this frame may use, best match to `query` first, as `list_models` returns them (`docs/04-models-and-routing.md#model-awareness-and-model-cards-implemented`) | — | `engine` |
| `model.current` | `() -> ModelCard` | The frame's model, resolved | — | `engine` |
| `model.can_reason` | `(m: ModelRef? = none) -> Bool` | Whether effort is sent to this model (Imperium's four-step rule) | `NotFound` | `engine` |
