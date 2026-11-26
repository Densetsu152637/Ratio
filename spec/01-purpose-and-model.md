# 01 Purpose and execution model

## What Ratio is for

An Imperium agent is a loop of model calls and tool calls (`docs/03-agent-engine.md#agent-loop`).
Everything that makes one agent behave differently from another, apart from the model's weights,
lives in that loop: the system prompt, which tools are offered, how tool results are fed back,
when to plan, when to delegate, when to check work, when to stop. Today that loop is Rust code in
`imperium-engine`, and changing it means changing the daemon.

Ratio moves that behaviour into programs. A Ratio program is a **harness definition**. It can
reproduce the stock loop line for line, and it can do what the stock loop cannot: run three
critics in parallel and vote, search a tree of partial solutions, hand a struggling subagent to a
stronger model, or check every answer against the test suite before reporting it. Because
harnesses are callable like functions (chapter 04), those methods compose.

Ratio is not a general-purpose language. It has no file, network, process or clock access of its
own; every effect is a request to the host, which is Imperium. People write most harnesses;
models may also write them, through a separate, attenuated and journalled path (authored
harnesses, chapter 07), but a running program never evaluates generated text (R12).

## What a program is

A **harness package** is one `.ratio` file plus the files it imports. It holds declarations only:
types, constants, functions, prompt templates, harnesses, persistent state and tests. Nothing
runs at load time.

A package exposes up to two **entries**, marked with attributes:

| Entry | Signature | Runs |
| --- | --- | --- |
| `@turn` | `harness name(turn: Turn) -> Reply` | Once per user turn of a session (or of an agent that a session runs as its root) |
| `@task` | `harness name(task: TaskInput) -> Report` | Once per turn of a subagent: its first turn (the brief) and every follow-up (`resume_agent`, a queued message) |

A package used as a session harness needs `@turn`; one used for subagents needs `@task`; a
package may have both, and when a parent runs under a package that has a `@task` entry, its
subagents inherit that entry unless they are given another harness (chapter 08). Every other
`harness` in the package, and every harness it imports, is callable from those entries.

```ratio
meta { name: "careful", version: "1.0.0", description: "Plans, acts, then checks" }

@turn
pub harness turn(turn: Turn) -> Reply {
  let plan = planner(turn.message)
  execute(plan, turn)
}
```

## How Imperium runs it

### Decision R2: an interpreter in the daemon, over a typed IR

```text
 .ratio files ─parse─▶ AST ─resolve─▶ typed AST ─check─▶ Ratio Core IR ─▶ interpreter ─▶ Host (engine)
                 │                         │                    │                 │
             syntax errors          type / taint errors     content hash      effects: model,
                                                           (harness version)  tools, agents, ...
```

- **Loading is checking.** A package is parsed, resolved and type checked when it is selected
  (and by `harnesses.check`, chapter 08), never when a turn is already spending tokens. A package
  with an error is not selectable; the error names file, line and column.
- **Ratio Core IR** is the lowered form: a tree of typed expressions in which every effect is an
  explicit node with a stable **site id**, every loop has an explicit counter, closures are
  explicit environments, and sugar (`|>`, string interpolation, `with` blocks, prompt sections,
  `par` blocks) is gone. It is not bytecode; the interpreter walks it.
- **The IR has a content hash.** `sha256` over its canonical serialisation, plus the standard
  library's version and the hashes of the built-in prompt texts it uses. The hash identifies a
  harness version exactly: evals record it, replay checks it, and the registry uses it to tell
  two copies of a version apart (chapter 06, `meta`).
- **A prepared form.** At load, each package's canonical IR is lowered once into an executable
  **prepared form** (names interned, record fields resolved to slots, locals to frame slots,
  built-ins to direct references, site ids carried through). The prepared form is never
  serialised or hashed, so changing how it is built changes no hash, effect key or `IR_VERSION`
  (chapter 10).
- **The interpreter** is an async tree walker in Rust. Pure evaluation runs on the agent's own
  task; each effect is an `await` on the host. It counts steps and heap bytes as it goes
  (chapter 07, limits). At every 1,000-step checkpoint it yields to the async runtime as well as
  checking cancellation, so a long pure stretch never holds a runtime worker that other sessions
  and the agent's own branches need. A pure built-in whose input exceeds an implementation
  threshold (for example 64 KiB of text or 16,384 elements), or whose charge before running
  exceeds one (for example ten checkpoints, as for a heavy pattern over a short text), runs on a
  bounded blocking pool, as does, whenever the steps left exceed that threshold, a built-in that
  debits its later work as it goes (an iteration over matches), while
  its branch awaits it, with the same result, labels and charges, its heap accounted before
  dispatch; built-ins that take callbacks
  never run there, so program code always stays on the agent's task. Values are `Send + Sync`
  (shared by atomic reference counts) for that reason. A pure built-in's charge for its input
  size is debited before it runs, so one the steps left cannot pay for raises
  `Budget { which: steps }` without running (chapter 06, `data`).
- **The host** is a trait the engine implements (chapter 10). Every effect of every built-in goes
  through it, so the interpreter never touches the network, the file system, a process or a
  clock directly.

Alternatives considered, against the same requirements (embeddable in a Rust daemon; no ambient
authority; typed schemas for generation; taint tracking; structured concurrency; exact replay;
harnesses as first-class calls; readable by people who are not language designers):

| Option | Why not |
| --- | --- |
| Keep harnesses in Rust only, behind item 58's switch | Every new reasoning method is a daemon release; users and plugins cannot write one; evals cannot compare methods without rebuilding |
| Declarative workflow files (TOML/YAML graphs of steps) | Cannot express loops with data-dependent exits, tree search, recursion, or voting without growing into a bad language anyway |
| Embed Starlark (`starlark-rust`) | Closest fit: deterministic, hermetic, Python-like. But synchronous evaluation (every model call would block a thread or need a re-entrant bridge), no concurrency, no taint, and its optional types cannot drive generation schemas or be relied on for security |
| Embed Lua or Rhai | Dynamically typed, mutable shared state, no taint, determinism would rest on discipline; sandboxing an embedded Lua is a known source of escapes |
| Reuse Monty, the Python subset of code mode (roadmap item 65) | Monty runs **model-written** code in a confined worker process because that code is hostile by assumption. A harness is the user's or a plugin's code, runs on every turn, needs concurrency, typed schemas, taint and effect-level replay, none of which Monty offers, and a worker round trip per effect would sit on the hot path of every model call |
| WebAssembly components (harnesses written in any language) | Strong isolation, but authors would need a toolchain, programs become opaque binaries (no reading a harness before trusting it), and replay and taint would have to be rebuilt at the component boundary |
| Bytecode VM instead of an IR walker | Harness programs are small and spend almost all their time waiting on models; a VM's speed buys nothing measurable and costs a second representation to keep correct. Revisit if profiling shows the walker above 1 % of turn time |
| A separate interpreter process | Isolation is not needed for code with no ambient authority (chapter 07), and a process boundary would add latency to every effect and a second lifecycle to clean up |

Authored harnesses (chapter 07) are written by models and so are untrusted source, yet they also
run in process. That is acceptable because a harness has no ambient authority: everything it can
do is an engine call checked against its frame, it cannot `trust`, and the interpreter bounds its
steps, heap, nesting and depth. The interpreter's own memory safety (safe Rust, no `unsafe` in the
crate) is the remaining surface. Revisit R2 if harnesses from a public registry become a
requirement, or if the stage 24 review finds the in-process surface too large: then a separate
process, or a WebAssembly build of the interpreter, becomes worth its cost.

### Decision R3: determinism and replay

Evals need a run to be reproducible: the same task, harness, model answers and tool results must
give the same transcript, the same tool calls in the same order of record, and the same final
answer.

**Determinism by construction.** Given the same answers to its effects, a Ratio program computes
the same result, whatever the timing:

- Values are immutable. Closures capture by value, and concurrent branches may not assign outer
  `var`s, write package state or share a conversation (chapter 03), so there are no data races.
- The only sources of variation are effects and scheduling choices. Effects: model calls, tool
  calls, agents, memory, user answers, the clock and timers (`time.now`, `time.since`,
  `eval.timer`), the turn's random seed (every `rand.*` draw follows from it and the drawing
  branch's position, chapter 06), and cache lookups (`cached`, `memo`, `once`). Scheduling
  choices: which `race` branch won, where each cancelled branch was cancelled, which `par` branch
  failed first, whether and where a `timeout` fired, every budget admission, refusal and reading,
  every cache wait and promotion, and the order in which `par_fold` delivers outcomes by
  completion. All of them go through the host and onto the tape.
- Concurrent branches are combined in **program order**, never completion order: `par` returns a
  tuple in written order, `par_map` returns results by index.
- Iteration over maps is in key order; sets do not exist (use `Map<K, Unit>`, ordered).
- Floating-point operations are IEEE 754 binary64 without fused operations or fast-math, so two
  machines give the same bits.

**The effect tape.** Every effect is identified by an **effect key**:

```text
key         = (frame path, branch path, site id, occurrence)
frame path  = one element per frame from the entry: harness name @ call-site id # occurrence of
              that call, each preceded by the branch path the call was made in
branch path = one element per concurrent construct enclosing the effect within its frame, from
              the frame's root: c<construct site id> # occurrence . b<branch index>
site id     = the IR node's stable id (package IR hash + node index)
occurrence  = how many times this site has run in this frame and branch path (loops)
rendered    : turn/critic@s41#0/c12#0.b2/c30#1.b0
```

Occurrences, of effect sites, of harness call sites and of concurrent constructs alike, count per
`(frame, branch path, site)`. A **branch index** is the written position for `par { … }` and
`race { … }`, and the list position for `par([…])`, `race([…])`, `par_map`, `par_map_settled` and
`par_fold`; a built-in that runs callbacks or calls concurrently numbers its branches by the
position of the item each runs (`model.generate_n` and `think.self_consistency` by sample,
`think.best_of` by sample, `think.debate` by side within a round, `think.beam_search` by state
within a level). Effects made by `par_fold`'s `step` carry the element `c<site>#<occurrence>.fold`.
So the second attempt of a `retry` inside a `par_map` branch has the same key whichever branch
finishes first.

The key depends only on the program and the data it computed, never on timing. In **record**
mode the host writes `(key, request digest, response)` to the tape for each **input-bearing**
effect: one whose response the program reads, including a unit-returning effect that can be
refused. Output-only effects that cannot fail (`log.*`, `trace.*`, `session.emit`,
`session.stream`) get no entry. In **replay** mode the host answers each input-bearing effect from
the tape by key, after checking that the request digest (a hash of the canonical request: model,
prompt, sampling, tools; or tool name and arguments) matches. A mismatch is a `Replay` error
naming the key and both digests; with `replay = "lenient"` it falls through to a live call instead
and is listed in the run's output. Output-only effects are **suppressed** in replay: the program
evaluates them, but they are not forwarded to the host, so a replay never streams, emits or logs
to a live session again; the replay's own event stream (for `harnesses.replay` and the live view)
is produced by the tape host and redacted as live events are. The tape ends each turn with a
**reply digest**, the digest of the entry's returned `Reply` or `Report` or of its uncaught error,
and replay compares it, so a divergence in an output-only effect is caught at the next
input-bearing effect or at the end of the turn. A tape recorded with `--ratio-tape-mode full`
also holds a digest-only entry for each output-only effect, which replay checks where it happens.

**The turn sequence.** In record mode the tape host gives every **scheduling event** of a turn one
number from a single turn-global sequence, in the order the events happen:

- **ledger events** (chapter 04): each budget admission with its reservation and the inputs of
  its arithmetic, each refusal, each completion with its actual charge, each `budget_left()` or
  `spent()` reading, and each branch's step total when the branch ends or is cancelled;
- **cancellation events** (chapter 03): each cancelled branch's cancellation point, each
  `aborted`, `abandoned`, `drained` and `withdrawn` call, and each re-parenting of an outstanding
  drain;
- **cache events** (chapter 06, `control`): each miss that starts a producer, each wait, each hit,
  each producer's success, failure or cancellation, and each promotion;
- **ordering events**: which `race` branch won, which `par` branch failed first, whether and where
  a timeout fired, and `par_fold`'s deliveries in completion order.

Replay checks the tape **against the tape only**. It replays the events in sequence order into a
model built from recorded data alone (the ledgers, the table of cache producers, the wait-for
graph) and verifies each event against that model, never against the replaying run's live state.
The replaying run may resolve effects in any order; where the program needs an outcome (a refusal,
a cancellation point, a cache value), it takes the recorded one. A failed check is a `Replay`
divergence. So a crafted tape cannot make the recorded decisions inconsistent with each other or
lift a limit on what the replay really consumes (chapter 07, T13).

**Tape storage.** The tape is a JSON Lines file. In a session it is part of the journal
(`ratio_effect` events, chapter 08); headless runs write and read it with `--ratio-tape`
(chapter 08), and a headless tape file whose name ends in `.jsonl.zst` is zstd-compressed.

- A response body above 1 KiB is stored once per tape, in a blob line named by its BLAKE3 digest,
  and entries refer to it; in the journal, a `ratio_effect` whose body is already journalled (a
  `tool_finished` payload, a conversation message) refers to that event instead of copying it.
  Labels are stored with the reference. Loading a tape verifies every blob against its digest and
  rejects the tape on a mismatch. In a headless tape a labelled body is never named by a plain
  hash, which would reveal a low-entropy secret, only by the per-session redaction digest
  (chapter 03); so are labelled cache keys and `memo` argument hashes of labelled arguments in
  cache events (chapter 06, `control`).
- A chat request's digest is `H(chain head, tools digest, options digest)`, where the chain head
  is the conversation's chained digest `d(k) = H(d(k−1), H(message k with its labels))`, kept by
  the engine per message (so a rollback restores it) and read through the host (chapter 10).
  Digesting a chat request therefore costs O(1), whatever the conversation's length.
- A replayed response carries the union of its recorded labels and the labels the source rules of
  the replaying session set again (chapter 03, secrecy): a `read_file` of a path that now matches
  `ratio.secret_paths` is labelled in replay even if it was not when recorded. Replay can add
  labels, never remove them.

The clock is an effect like any other, so `time.now()` replays exactly. The session's random seed
is drawn once per turn from the operating system and recorded; each branch draws from its own
generator seeded from it and the branch's position (chapter 06, `rand`), so `rand.*` draws need no
tape entries and replay exactly.

What replay does **not** promise: a live model call at temperature above zero gives different
text on different runs. Replay reproduces a recorded run; it does not make sampling
deterministic. `seed` in sampling options (chapter 05) is passed to backends that honour it.

**Relation to the existing seams.** `imperium run --subagent-replay` answers subagents from
recordings (`docs/03-agent-engine.md#subagent-recordings`). The Ratio tape is finer (every model
and tool call, not only subagent reports) and coexists with it: a subagent answered by
`--subagent-replay` is one effect on the tape.

### How a turn maps to execution

Ratio runs **per turn**, not per session. Each user turn is one call of the `@turn` entry; each
subagent turn is one call of the `@task` entry. Nothing in the interpreter's heap survives the
end of a turn. What must persist is kept by the engine (the conversation, plans, agent records)
or in the package's declared `state` (chapter 03), which is journalled.

Why per turn: Imperium sessions survive daemon restarts, branching and rewinding by replaying
journals and restoring saved conversations (`docs/03-agent-engine.md#persistence`). An
interpreter heap cannot be journalled cheaply or safely; a typed state record and the engine's
own conversation can. A long-lived Ratio process would also hold model slots and budgets across
idle time, which Imperium deliberately avoids.

One turn, step by step:

1. The user's message arrives. The engine does what it does for every turn before the agent
   sees it: slash-command expansion, `prompt.submit` hooks, mid-session model switch and
   compaction, checkpoints (`docs/03-agent-engine.md#switching-models-and-compaction`), and the
   input guard for inputs too large for the window (`docs/03-agent-engine.md#large-inputs-and-delegate_chunks`).
   Harness aids that make a model call of their own (the de-reference step) run here, charged to
   the turn's budget. It emits `turn_started`, then builds the `Turn` value: `message` (the user's
   own words, `UserText`), `payloads` (text that came with the turn but was not written by the
   user, such as the CI log, pull-request comments or issue body an automation prompt embeds:
   untrusted), attachments, the turn index, and whether the session was resumed.
2. The engine calls the `@turn` entry with that value inside a fresh **root frame**: the session
   conversation, the session's model and effort, the tools the session offers, the session's
   permission mode and rules, and the session's budgets (chapter 04).
3. The entry runs. Each model call streams `text_delta` events as the native loop's do; each tool
   call goes through the engine's dispatch and emits `tool_started` and `tool_finished`; each
   model call emits `model_call_completed` and `context_breakdown`.
4. **Safe points.** Steering messages and background-command notices
   (`docs/03-agent-engine.md#queueing-and-steering`) are delivered only at safe points. In Ratio,
   every `model.chat` on a conversation is preceded by a safe point for that conversation, and
   `context.safe_point()` makes one explicitly. A safe point also runs mid-turn compaction when it
   is due. Steering never lands between a tool call and its result, because the conversation API
   refuses to add any other message while a tool call is unanswered.
5. The entry returns a `Reply`. Its text is the turn's final reply; the engine saves the
   conversation, appends to `<id>.turns.jsonl`, writes the package state if it changed, and emits
   `turn_completed`. An uncaught error becomes `turn_failed` with the error kind and message.

**Entry obligations.** The main conversation must stay a faithful record of the session, so the
engine checks two things around every entry call:

- Before the first `model.chat` on the main conversation, the entry must have appended the turn
  (`context.append_turn` in a `@turn` entry; `context.append_task` in a `@task` entry). A
  `model.chat` on the main conversation before that raises `Invalid`. If the entry returns without
  ever appending it, the engine appends the turn itself (the user's message as the native loop
  would build it), records a `ratio_log` warning, and continues.
- After the entry returns, if the main conversation does not end with an assistant message whose
  text is the reply's text, the engine appends one, so the next turn sees what the user saw.
  A `Reply` with status `question` is appended the same way.

**Stop and resume.** `session.control stop` cancels at the next effect boundary: a tool call in
flight finishes, a model request in flight is cancelled, and the interpreter future is held, not
dropped. `resume` continues it, re-sending the cancelled model request, exactly as the native
loop does (`docs/03-agent-engine.md#stopping-a-session`). Stop drains every tool call in flight
whatever its cancellation class: the classes of chapter 03 apply only to cancellation inside the
program.

**Daemon restart mid-turn.** As today, the turn the restart cut off is reported failed. A later
stage may instead **resume by replay**: re-run the entry with the recorded tape until the tape
runs out, which rebuilds the interpreter state exactly, then continue live. This is possible
only because of R3; it is listed in chapter 10 as an optional stage.

**Subagents.** A subagent spawned with a Ratio harness is an ordinary engine subagent
(`docs/03-agent-engine.md#reactive-delegation`): it has a task id, events, an agent record and a
kept conversation. Each of its turns is one call of the `@task` entry in the subagent's own root
frame, whose bounds are those the parent's spawn gave it.

**Harness calls inside a turn** are not subagents. They are synchronous calls that run within the
caller's turn and frame tree (chapter 04). When a callee has its own conversation and calls a
model, the engine registers it as a lightweight **harness agent** so its model calls have an agent
id for events, local-model slots and context breakdowns; it is ephemeral and not resumable.
