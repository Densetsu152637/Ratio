# `agents`: subagents and knowledge agents

These built-ins drive Imperium's own delegation (`docs/03-agent-engine.md#reactive-delegation`,
`docs/21-knowledge-agents.md`). A subagent is an engine agent with a task id, its own events and
agent record, and a kept conversation; it is not a harness call (chapter 04). Use a harness call
for a step of reasoning inside a turn; use a subagent for work the user should see as delegated,
that may run long, write in its own worktree, be resumed later, or be kept as a knowledge agent.

`Task<Report>` is a handle the engine tracks. Unlike every other handle it may outlive the scope
that created it: a task not joined when the turn ends keeps running as an ordinary subagent and
reports through `task_finished`, exactly as one the model spawned.

The delegation limits are Imperium's: today only the root delegates, subagents are depth 1, and
`delegate_chunks` is root only. A built-in that the engine does not allow in the current agent
raises `Denied { by: profile }`.

## Spawning and collecting

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `agents.spawn` | `(brief: Untrusted<Str>, harness: Str? = none, agent: Str? = none, model: ModelRef? = none, tier: Tier? = none, location: Location? = none, tags: [Str] = [], effort: Effort? = none, access: Access = read, knowledge: Str? = none, title: Str? = none, single_call: Bool = false, budget: Budget? = none) -> Task<Report>` | Starts a subagent as `spawn_agent` does, returning at once. `harness` names a registered package whose `@task` entry runs it (roadmap item 58; unset inherits the spawning agent's package, chapter 08); `agent` names an agent file; `budget` is carved from the frame's (chapter 04). The subagent's permissions are bounded by the frame's, never the session's alone | `Denied`, `NotFound` (harness, agent, model), `Invalid` (`single_call` with `agent`) | `delegate` |
| `agents.await` | `(tasks: [Task<Report>], timeout: Duration? = none) -> [TaskReport]` | Waits without model calls until every task reports, as `await_tasks`; `{ task, ok, report: Untrusted<Str>, question?, usage, status }` per task, in the order given. With `timeout`, tasks still running are returned with `status: running` | — | `delegate` |
| `agents.join` | `(task: Task<Report>, timeout: Duration? = none) -> TaskReport` | `await` of one task | `Timeout` | `delegate` |
| `agents.delegate` | `(brief: Untrusted<Str>, harness: Str? = none, opts: SpawnOpts = {}) -> TaskReport` | `spawn` then `join`, for a single delegated piece of work | as `spawn` | `delegate` |
| `agents.map` | `(briefs: [Untrusted<Str>], harness: Str? = none, limit: Int = 4, opts: SpawnOpts = {}) -> [TaskReport]` | Spawns one subagent per brief with at most `limit` running at once (and never more than the engine's concurrency limit), and returns all reports in brief order | as `spawn` | `delegate` |
| `agents.delegate_chunks` | `(artefact: Artefact, chunk_tokens: Int, brief: Untrusted<Str>, reduce: Untrusted<Str>? = none) -> [Task<Report>]` | Splits a stored large input and starts one subagent per chunk, as `delegate_chunks` (`docs/03-agent-engine.md#large-inputs-and-delegate_chunks`); with `reduce`, the last task is the reducer | `NotFound` (artefact), `Invalid` (too many chunks), `Denied` | `delegate` |

## Controlling running and finished subagents

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `agents.steer` | `(task: Task<Report>, text: Untrusted<Str>) -> ()` | Puts a message into the subagent's running turn at its next safe point (`steer_agent`) | `Invalid` (not running), `NotFound` | `delegate` |
| `agents.queue` | `(task: Task<Report>, text: Untrusted<Str>) -> ()` | Queues a further turn for a running subagent (`queue_agent`) | `Invalid`, `NotFound` | `delegate` |
| `agents.resume` | `(task: Task<Report>, text: Untrusted<Str>) -> Task<Report>` | Gives an idle, failed, interrupted or paused subagent a follow-up with its kept conversation (`resume_agent`) | `Invalid` (running, not collected), `NotFound` | `delegate` |
| `agents.cancel` | `(task: Task<Report>) -> ()` | Ends a paused subagent (`resume_agent` with `cancel: true`), or cancels a running one at its next safe point | `NotFound` | `delegate` |
| `agents.status` | `(task: Task<Report>) -> AgentSummary` | The agent record: `{ id, title, model, status: running \| idle \| failed \| interrupted \| paused, turns, usage, cost, latest_report? }` | `NotFound` | `engine` |
| `agents.list` | `() -> [AgentSummary]` | Every agent of the session, root first, as `list_agents` (including tasks the model spawned) | — | `engine` |
| `agents.current` | `() -> AgentInfo` | The agent this frame runs in: `{ id, role: root \| subagent \| harness \| knowledge_fork, parent?, title }` | — | `engine` |
| `agents.named` | `() -> [AgentFileInfo]` | The agent files the session can use, `{ name, description, origin, tools?, model?, harness? }` (`docs/06-extensibility.md#named-agents-implemented`) | — | `engine` |

## Knowledge agents

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `agents.ask` | `(agent: Str, question: Untrusted<Str>, timeout: Duration = 300s) -> Untrusted<Str>` | Asks a knowledge agent (by id or title) in an ephemeral fork, as `ask_agent`; the answer is untrusted and arrives already framed by the engine | `NotFound`, `Timeout`, `Denied` (a knowledge fork may not ask) | `delegate` |
| `agents.knowledge` | `(query: Str? = none) -> [KnowledgeCard]` | The session's knowledge agents' cards, best match first (`list_knowledge_agents`) | — | `engine` |
| `agents.keep_as_knowledge` | `(task: Task<Report>, topic: Str) -> KnowledgeCard` | Promotes an idle, read-only subagent (`keep_as_knowledge`) | `Invalid` (not promotable, with the reason), `Denied` | `delegate` |
| `agents.retire_knowledge` | `(task: Task<Report>) -> ()` | Retires a knowledge agent | `NotFound` | `delegate` |
