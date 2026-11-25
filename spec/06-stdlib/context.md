# `context`: conversations and the context window

A `Conversation` is a handle to an append-only message log the engine owns: the session's root
conversation, a subagent's kept conversation, or a harness frame's own (chapter 04). The engine
keeps Imperium's invariants on every conversation, whoever writes to it:

- every tool call is followed by its result before any other message (steering and notices wait
  for a safe point);
- roles alternate as the chat templates require (the engine merges or wraps as it does today);
- harness tag names in untrusted text are defused;
- the conversation is counted against its model's window, and compacted by the engine's own
  compactor (`docs/03-agent-engine.md#switching-models-and-compaction`).

Only the frame that owns a conversation may append to it or run a model on it; other frames get
`Transcript` snapshots. Transcript texts are untrusted except the session user's own messages.

## Conversations

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `context.current` | `() -> Conversation` | The frame's conversation, created on first use for `context: fresh` | `Invalid` (`context: none`) | `engine` |
| `context.main` | `() -> Conversation` | The agent's main conversation (the session's root conversation in a `@turn` entry, the subagent's kept conversation in a `@task` entry); only in an entry's own frame | `Invalid` (elsewhere) | `engine` |
| `context.new` | `(system: Prompt? = none) -> Conversation` | An extra conversation owned by this frame (for a sub-dialogue that must not mix with the frame's own), with its own system prompt | `Invalid` (untrusted system text) | `engine` |
| `context.fork` | `(conv: Conversation) -> Conversation` | A copy owned by this frame; appends to the copy do not touch the original; on a local model the shared prefix is reused from cache | — | `engine` |
| `context.append_user` | `(conv: Conversation, content: Prompt) -> ()` | Appends a user message rendered from a prompt (sections other than `system` and `context`) | `Invalid` (tool calls pending, `system` section) | `engine` |
| `context.append_turn` | `(conv: Conversation, turn: Turn) -> ()` | Appends the user message the native loop would build for this turn: the workflow manifest, recalled memories, the harness aids' state, plan and hint blocks, attachments and the user's text, in the engine's order and tags | `Invalid` | `engine` |
| `context.append_assistant` | `(conv: Conversation, text: Untrusted<Str>) -> ()` | Appends an assistant message without a model call (a prefilled or synthesised reply) | `Invalid` (tool calls pending) | `engine` |
| `context.notice` | `(conv: Conversation, text: Str, data: Untrusted<Str>? = none) -> ()` | Appends a harness notice: trusted `text` from the harness, with optional framed `data`. Rendered the way the engine renders its own notices (a user-role message the model is told comes from the harness) | `Invalid` | `engine` |
| `context.load` | `(reference: Str) -> Untrusted<Str>` | Loads a workflow companion or reference, as the `context_load` tool; kept verbatim through compaction like any `context_load` result | `NotFound`, `Denied` | `tool:context_load` |

## Reading and snapshots

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `context.messages` | `(conv: Conversation) -> Transcript` | All messages after the system prompt, as a snapshot | — | `engine` |
| `context.last` | `(conv: Conversation, n: Int) -> Transcript` | The last `n` messages, extended backwards so no tool result is separated from its call | — | `engine` |
| `context.excerpt` | `(conv: Conversation, from: Mark? = none, last_turns: Int? = none, roles: [Role] = [user, assistant, tool]) -> Transcript` | Messages from a mark, or the last `n` user turns, filtered by role, with tool results kept with their calls | `Invalid` (mark of another conversation) | `engine` |
| `context.mark` | `(conv: Conversation) -> Mark` | A position at the current end, for later excerpts and pins | — | `engine` |
| `context.snapshot` | `(conv: Conversation) -> Transcript` | The whole conversation including its system prompt, as a value that may be returned, logged or passed to other frames | — | `engine` |
| `context.system` | `(conv: Conversation) -> Prompt` | The conversation's system prompt | — | `engine` |
| `context.system_default` | `(role: AgentRole = root) -> Prompt` | The system prompt the engine assembles for a root or subagent: base instruction, policy layers, project instructions, skills index, memory instructions, citation rules, the harness aids' paragraph, grants, scratch folder and the context-budget line (`docs/03-agent-engine.md#context-management`) | — | `engine` |

## Compaction and pinning

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `context.safe_point` | `(conv: Conversation) -> SafePoint` | Takes waiting steering messages and background notices into `conv` and runs mid-turn compaction when it is due, exactly where the native loop does before a model call; `{ steered: [Str], notices: Int, compacted: Bool, paused_agents: [Str] }`. `model.chat` calls it implicitly | `Invalid` (tool calls pending) | `engine` |
| `context.pending` | `(conv: Conversation) -> [Pending]` | What the next safe point would take, without taking it: `{ kind: steering \| background_notice, text: Untrusted<Str> }` | — | `engine` |
| `context.compact` | `(conv: Conversation, keep_turns: Int = 2, focus: Str? = none) -> Compaction` | Compacts now with the engine's compactor (as `session.control compact`): older messages become a `<conversation-summary>`; kept context (manifest, loaded skills and references, the plan and state blocks) is carried verbatim. `focus` adds a line to the summarising prompt on what to keep. Returns `{ tokens_before, tokens_after, summarised, model }` | `Unsupported` (unknown window), `ModelFailed` | `model ×1..6` |
| `context.clear_stale` | `(conv: Conversation, older_than_turns: Int = 2) -> Int` | Replaces old results of repeatable read-only tools with stubs, as `harness.clear_stale_results` does before a compaction; returns how many | — | `engine` |
| `context.pin` | `(conv: Conversation, from: Mark, to: Mark? = none, reason: Str) -> Pin` | Marks messages to be carried verbatim through compaction, like the kept-context block, within the pin budget (a quarter of the window); a pin over budget is refused | `Invalid` (over budget) | `engine` |
| `context.unpin` | `(pin: Pin) -> ()` | Removes a pin; the messages become ordinary again | `NotFound` | `engine` |
| `context.pins` | `(conv: Conversation) -> [PinInfo]` | The pins, `{ reason, messages, tokens }` | — | `engine` |

## Sizes and budgets

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `context.tokens` | `(conv: Conversation) -> TokenCount` | The conversation's size for its model, with the counting method (`exact`, `calibrated`, `estimated`) | — | `engine` |
| `context.window` | `(conv: Conversation? = none) -> Int?` | The context window of the conversation's model, or the frame's model | — | `engine` |
| `context.remaining` | `(conv: Conversation) -> Int?` | Window minus the conversation's size minus the reply reserve (15 % of the window), when the window is known | — | `engine` |
| `context.fits` | `(conv: Conversation, extra: Prompt? = none) -> Bool` | Whether the conversation, plus `extra`, stays under the compaction threshold (`context.auto_compact_at`) | — | `engine` |
| `context.breakdown` | `(conv: Conversation) -> Breakdown?` | The latest `context_breakdown` of the conversation's last model call: categories, tokens, window, method | — | `engine` |

## Large inputs

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `context.store` | `(text: Untrusted<Str>, name: Str? = none) -> Artefact` | Stores text as a session artefact (as the engine does for inputs over 75 % of the window) and returns its handle: `{ id, chars, lines, outline }`; for `agents.delegate_chunks` and paged reading | — | `engine` |
| `context.read_artefact` | `(a: Artefact, offset: Int = 0, limit: Int = 32000) -> Untrusted<Str>` | Reads characters of a stored artefact | `NotFound` (lost at a daemon restart) | `engine` |
| `context.chunks` | `(a: Artefact, chunk_tokens: Int) -> [Untrusted<Str>]` | The chunks `delegate_chunks` would make, by the same boundary rules (file starts, headings and paragraphs, then token windows), for harnesses that process chunks themselves | `Invalid` (more than 64 chunks) | `engine` |
