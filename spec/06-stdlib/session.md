# `session`: the user, the plan, checkpoints and branches

These built-ins reach the person and the session around the agent. Interaction with the person
is possible only in attended frames (chapter 04); an unattended session (schedules, auto-fix,
project agents) has nobody to answer, so Imperium answers its questions with recommendations and
never shows it prompts that wait on a reply, and Ratio does the same.

## The person

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `session.ask` | `(questions: [Question]) -> [UserText]` | One batch of questions, each `{ question: Str, options: [Str], recommended: Str, secret?: Bool }` (`Question`; an absent `secret` is `false`) (`secret` answers are `Secret<UserText>`, chapter 03), through `ask_user` (`docs/05-tools-and-isolation.md#ask_user`): the agent waits with no model calls, a blank answer or a 10-minute timeout takes the recommendation. Unattended: the recommendations at once. Answers are the user's own text and trusted | `Invalid` (empty batch, untrusted question text) | `user` (recommendations when unattended) |
| `session.request_approval` | `(summary: Str, detail: Untrusted<Str>? = none) -> Bool` | Asks the person to approve an action the harness is about to take that no tool approval covers (for example, handing work to a paid frontier model). Shown as a permission request for the pseudo-tool `ratio_approval` with the harness's name; rules on `ratio_approval` apply (`allow` skips the question, `deny` refuses). `summary` is trusted harness text, `detail` is framed. Unattended: `false`, with a notice. A `false` is remembered for the turn: the same summary is refused again without asking | — | `user` |
| `session.notify` | `(text: Untrusted<Str>, level: NoticeLevel = info) -> ()` | Shows a notice to the session's clients (a `notice` event); the model does not see it | — | `effect` |
| `session.stream` | `(text: Untrusted<Str>) -> ()` | Streams text to the person as part of the turn's reply (`text_delta`) while the harness works, for progress a harness wants seen | `Invalid` (not in the session's root agent) | `effect` |

## Plan and state

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `session.plan` | `() -> Untrusted<Plan>?` | The agent's latest plan (the newest `update_plan`, or the block carried through compaction) | `Denied` | `sees:plan` |
| `session.update_plan` | `(plan: Untrusted<Plan>) -> ()` | Sets the agent's plan as `update_plan` does: `plan_updated` event, live checklist in the clients, carried through compaction as `<current-plan>`. A plan built from labelled values is labelled, journalled so, and redacted in clients (chapter 03) | `SchemaMismatch` | `tool:update_plan` |
| `session.state` | `() -> Untrusted<AidState>` | The harness aids' session state (`{ entities, goals, constraints }`, `docs/03-agent-engine.md#harness-aids-for-small-models`) | `Denied` | `sees:plan` |
| `session.update_state` | `(change: AidStateChange) -> Untrusted<AidState>` | Changes it as `update_state` does (bounded, angle brackets stripped) | `Invalid` | `tool:update_state` |

The harness aids' state is the engine's, shared with the native loop's aids. A package's own
`state` (chapter 03) is separate and typed.

## Checkpoints, rewinds, branches and new sessions

These are **tool-gated**: each is decided as an Imperium tool (`start_session`) or a pseudo-tool
(`session_checkpoint`, `session_branch`, `session_rewind_files`), so a frame can use one only if
its tool set includes it, and the user's rules and permission mode apply. Rejected: making them
entry-only. That would forbid a well-scoped helper harness from checkpointing before a risky step,
while tool gating already lets an entry withhold them from every callee.

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `session.checkpoint` | `(label: Str? = none) -> Checkpoint` | Records a git checkpoint of the session's files now, as the engine does before turns that change files (`docs/03-agent-engine.md#persistence`), linked to the journal's length; for harnesses that want a restore point before a risky step | `Unsupported` (no git, remote environment), `Denied` | `tool:`*`session_checkpoint`* |
| `session.rewind_files` | `(cp: Checkpoint) -> ()` | Puts the session's files back to a checkpoint. Discards work, so it **always asks**, every time, like the safety floor; unattended frames are refused | `Denied`, `NotFound` | `user`, `tool:`*`session_rewind_files`* |
| `session.branch` | `(at_turn: Int? = none, title: Str? = none) -> SessionRef` | Branches the session at the end of an earlier turn (default the last completed one) into a new session, as `session.branch`; this session is unchanged | `Invalid` (point compacted away), `Denied` | `tool:`*`session_branch`* |
| `session.start` | `(prompt: Str, environment: Str? = none, package: Str? = none, model: ModelRef? = none) -> SessionRef` | Starts a new session, as the `start_session` tool (refused in plan mode, since the new session would not be). `package` selects its harness package (chapter 08) | `Denied`, `NotFound` | `tool:start_session` |

## About the session

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `session.id` | `() -> Str` | The session id | — | `engine` |
| `session.info` | `() -> SessionInfo` | `{ id, title, environment, folders: [Str], model, effort, mode, attended, package, turn_index, max_steps }`, where `package` is the harness package the session runs, where `max_steps` is `agents.max_steps` (or a headless run's `max_steps` flag) | — | `engine` |
| `session.turn` | `() -> Turn` | The current turn's input (the `@turn` entry's argument); attachments are empty unless `sees.attachments` | `Denied` | `sees:turn` |
| `session.attended` | `() -> Bool` | Whether this frame may talk to the person | — | `engine` |
| `session.mode` | `() -> PermissionMode` | The frame's effective permission mode after attenuation | — | `engine` |
| `session.folders` | `() -> [Str]` | The environment's folders, first the working folder | — | `engine` |
| `session.scratch_folder` | `() -> Str?` | The session's scratch folder, where it has one | — | `engine` |
| `session.usage` | `() -> Spent` | Tokens, cost and calls spent by the whole session so far, and by this turn | — | `engine` |
| `session.queued` | `() -> [UserText]` | User messages waiting in the queue for later turns (read only; they run as their own turns) | — | `engine` |
| `session.emit` | `(name: Str, data: Json) -> ()` | A custom `ratio_event { harness, name, data }` for clients, mods and the telemetry API; at most 16 KiB of data | `Invalid` (too large) | `effect` |

## `state`: package state

The package's declared `state` (chapter 03). Reads need `sees.state`; writes also need an entry's
own frame or a `fork` frame of it, and are refused inside concurrent branches.

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `state.get` | `() -> State` | The current value of the package's state (`State` is the type it is declared with, fixed at each call: never inferred or given) | `Denied`, `Invalid` (no `state` declared) | `sees:state` |
| `state.set` | `(v: State) -> ()` | Replaces it; written at the end of the turn | `Invalid` (over 64 KiB, inside a concurrent branch) | `sees:state`, `entry` |
| `state.update` | `(f: fn(State) -> State) -> State` | Applies `f` and stores the result, which it returns | as `set`; errors of `f` | `sees:state`, `entry` |
| `state.reset` | `() -> ()` | Back to the declared initialiser | as `set` | `sees:state`, `entry` |
