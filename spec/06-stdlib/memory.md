# `memory`: remembered facts and earlier sessions

Imperium's memory (`docs/17-memory.md`) has three scopes: **global** (facts about the user, in
every environment), **environment** (the environment's `Memory.md`), and **session** (working
memory: the plan and notes, kept in the journal). The built-ins below reach the first two through
the engine's memory tools; the session scope is Ratio's own `state` (chapter 03), scratchpads
(chapter 05) and the session plan (`session.plan`).

Everything read from memory is untrusted: a memory may have been written from tool output, and
memories persist across sessions, which makes them a place injected text could wait. Writes follow
the scope's write policy (`memory.write.global` and `memory.write.environment`, `ask` by default),
are refused in plan mode, and are allowed only in frames of the root agent's turn: subagents read
and propose, as in Imperium.

`enum MemoryScope { global, environment }`; `enum MemoryType { user, feedback, project, reference, lesson }`.

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `memory.search` | `(query: Untrusted<Str>, scope: MemoryScope? = none, type: MemoryType? = none, limit: Int = 8) -> Untrusted<[MemoryHit]>` | Ranked matches, `{ name, scope, type, description, snippet, score }`, by the engine's memory search (term-based, plus vectors when an embedding model is set) | — | `tool:memory_search` |
| `memory.read` | `(name: Untrusted<Str>, scope: MemoryScope? = none) -> Untrusted<MemoryEntry>` | A whole entry, `{ name, description, type, scope, created, source, confidence, body }`; without `scope`, the environment's section of that name before the global entry | `NotFound` | `tool:memory_read` |
| `memory.remember` | `(entry: NewMemory) -> ()` | Saves or replaces a memory, `NewMemory { name, description, type, scope, body, confidence: confirmed \| inferred }`. Bodies that look like secrets are refused by the engine | `Denied`, `Invalid` (limits, secret-like text) | `memory.write`, `tool:memory_remember` |
| `memory.edit` | `(name: Untrusted<Str>, old: Untrusted<Str>, new: Untrusted<Str>, scope: MemoryScope? = none) -> ()` | Replaces exactly one occurrence in a memory's body, as `memory_edit`; refused before the user is asked when it cannot apply | `NotFound`, `ToolFailed` (no match, several), `Denied` | `memory.write`, `tool:memory_edit` |
| `memory.forget` | `(name: Untrusted<Str>, scope: MemoryScope? = none) -> ()` | Moves a memory to the trash or archive (recoverable), as `memory_forget` | `NotFound`, `Denied` | `memory.write`, `tool:memory_forget` |
| `memory.propose` | `(entry: NewMemory) -> ()` | From a subagent or any frame without write rights: attaches the proposed memory to the agent's report and emits `memory_proposed`, for the root or the user to save. Available when Imperium's memory proposals are built (`docs/17-memory.md#interfaces`); until then it adds the proposal to the report text | — | `effect` |
| `memory.index` | `(scope: MemoryScope = global) -> Untrusted<[IndexLine]>` | The index lines agents see in their instructions, `{ name, description, type }` | `Denied` | `sees:memory_index` |
| `memory.environment` | `() -> Untrusted<Str>` | The environment's `Memory.md` text, as the instructions carry it (up to 24,000 characters) | `NotFound` (no environment), `Denied` | `sees:memory_environment` |
| `memory.recall` | `(text: Untrusted<Str>, limit: Int = 5) -> Untrusted<[MemoryHit]>` | The query automatic recall makes before a turn, within `memory.max_recall_tokens`, for harnesses that place recalled memories themselves | — | `tool:memory_search` |
| `memory.links` | `(entry: Untrusted<MemoryEntry>) -> [Untrusted<Str>]` | The `[[name]]` links in an entry's body, in order, without duplicates | — | `pure` |
| `memory.used` | `() -> [Untrusted<Str>]` | Names of the memories used in this turn so far (what `memory_used` events report) | `Denied` | `sees:memory_index` |
| `memory.search_sessions` | `(query: Untrusted<Str>, archived: Bool = false, limit: Int = 10) -> Untrusted<[SessionHit]>` | Searches earlier sessions' journals, as `search_sessions` (`docs/17-memory.md#session-search-implemented`); `{ session_id, title, at, kind, snippet, score }` | — | `tool:search_sessions` |
