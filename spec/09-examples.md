# 09 Examples

Nine complete packages. Each is a whole `.ratio` file that type checks against this
specification; together they use most of the language and a large part of the standard library.
They are also the first entries of the conformance suite (chapter 10), run against the scripted
host.

| # | Package | Shows |
| --- | --- | --- |
| 1 | `std:default` | The native agent loop |
| 2 | `std:plan-execute-verify` | Planning, step execution with acceptance checks, re-planning, verification |
| 3 | `std:self-consistency` | Sampling and voting, escalation on low agreement |
| 4 | `debate` | Two harnesses on different models argue; a third judges |
| 5 | `tree-of-thought` | Search over partial solutions with memoised evaluation |
| 6 | `research` | Retrieval over memory, earlier sessions and the web, reranking, cited answers, verification |
| 7 | `std:consul-escalation` | Roadmap item 64 |
| 8 | `std:tdd-fix` | A test-driven code-fix subagent |
| 9 | `digest` | Map-reduce over an input too large for one context |

## 1. The default agent loop

The complete source of `std:default` is in [chapter 08](08-imperium.md#the-default-agent-loop-in-ratio),
where each line is tied to the native loop's documented behaviour. Every example below that needs
a tool-using loop either calls `model.loop` (the same loop as one built-in) or writes it out to
change one part of it, as example 7 does.

## 2. Plan, execute, verify

The root plans in a read-only frame, executes each step in the main conversation with a bounded
loop, checks each step's acceptance criterion in a separate frame, re-plans at most twice when a
step fails, and verifies the whole plan before answering.

```ratio
meta {
  name: "std:plan-execute-verify",
  version: "1.0.0",
  description: "Plan with read-only tools, execute step by step, check each step, verify, answer",
}

const MAX_REPLANS = 2
const STEP_BUDGET = 20

/// Plans in its own frame: may read and search, never change anything.
harness planner(request: UserText, recent: Transcript) -> Untrusted<Plan>
  with {
    tools: [read_file, grep, glob, list_dir],
    permissions: { mode: plan },
    effort: high,
    budget: { tokens: 60k },
  }
{
  let conv = context.current()
  context.append_user(conv, prompt {
    instruct "Investigate what you need to, then stop. A plan will be asked for next."
    task request
    context recent
  })
  model.loop(conv, max_steps: 12)
  think.plan(request, context: prompt { context context.snapshot(conv) }, max_steps: 8)
}

/// Checks one step's acceptance criterion against the files, in a fresh read-only frame.
harness accept(step: Untrusted<PlanStep>, work: Transcript) -> Untrusted<Judgement>
  with { tools: [read_file, grep, glob], permissions: { mode: plan }, budget: { tokens: 30k } }
{
  let conv = context.current()
  context.append_user(conv, prompt {
    instruct "Check whether the acceptance criterion holds now. Look at the files; do not trust the transcript's claims."
    data step.acceptance ?? step.title as "acceptance criterion"
    data text.show(work) as "what was done"
  })
  model.loop(conv, max_steps: 8)
  model.judge(text.show(context.snapshot(conv)), rubric: "Pass only if the files show the criterion holds.")
}

@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)

  var plan = planner(turn.message, context.excerpt(conv, last_turns: 3))
  session.update_plan(plan)

  var replans = 0
  var i = 0
  while i < plan.steps.len() {
    let step = plan.steps[i]
    if step.status is done or step.status is skipped {
      i += 1
      continue
    }
    plan = mark(plan, i, StepStatus.in_progress)
    session.update_plan(plan)

    let start = context.mark(conv)
    context.append_user(conv, prompt {
      instruct "Carry out this step of the plan, and only this step. Say when it is done."
      task step.title
    })
    model.loop(conv, max_steps: STEP_BUDGET)
    let verdict = accept(step, context.excerpt(conv, from: start))

    if verdict.verdict is pass {
      plan = mark(plan, i, StepStatus.done)
      session.update_plan(plan)
      i += 1
    } else if replans < MAX_REPLANS {
      replans += 1
      plan = think.replan(plan, step.title, verdict.reasons)
      session.update_plan(plan)
    } else {
      plan = mark(plan, i, StepStatus.blocked)
      session.update_plan(plan)
      break
    }
  }

  let done = plan.steps.count(|s| s.status is done)
  eval.metric("steps_done", math.to_float(done))
  eval.metric("replans", math.to_float(replans))
  context.notice(conv, "Plan finished: {done} of {plan.steps.len()} steps done. Report to the user now: what was done, what was not, and what was not verified.",
    data: text.show(plan))
  let final = model.chat(conv, tools: [])
  { text: final.text, status: ReplyStatus.done }
}

fn mark(p: Untrusted<Plan>, i: Int, status: StepStatus) -> Untrusted<Plan> {
  { ..p, steps: p.steps.map_indexed(|j, s| if j == i { { ..s, status: status } } else { s }) }
}
```

## 3. Self-consistency voting

Samples several worked solutions, votes on the normalised final answers, and when agreement is
low asks a frontier model to adjudicate between the distinct answers.

```ratio
meta {
  name: "std:self-consistency",
  version: "1.0.0",
  description: "Sample several solutions, vote on the answer, adjudicate when they disagree",
}

type Solution = {
  reasoning: Str "Brief working",
  answer: Str "The final answer only: a number without units or separators, or a short phrase",
}
type Adjudication = { answer: Str, why: Str }
type Voted = { answer: Untrusted<Str>, agreement: Float, samples: Int, adjudicated: Bool }

pub harness solve(question: UserText, n: Int = 7) -> Voted
  with { context: none, sampling: { temperature: 0.7 }, budget: { model_calls: 16 } }
{
  let vote = think.self_consistency(n,
    || model.generate_as<Solution>(prompt {
      instruct "Solve the problem. Work briefly in `reasoning`, then give only the final answer."
      task question
      format Solution
    }),
    key: |s| normalise(s.answer))

  if vote.agreement >= 0.5 {
    return { answer: vote.key, agreement: vote.agreement, samples: n, adjudicated: false }
  }

  let candidates = vote.counts.keys().filter(|k| k != "<error>")
  let pick = model.generate_as<Adjudication>(prompt {
    instruct "Several attempts disagree. Decide which candidate answer is correct, solving the problem yourself if none is."
    task question
    data text.join(candidates, "\n") as "candidate answers"
    format Adjudication
  }, opts: { model: model.route(tier: frontier), effort: high, sampling: { temperature: 0.0 } })
  { answer: normalise(pick.answer), agreement: vote.agreement, samples: n, adjudicated: true }
}

fn normalise(a: Untrusted<Str>) -> Untrusted<Str> {
  a.trim().lower().replace(",", "").trim_end_matches_dot()
}

// A local helper, since `text` has no dedicated function for it.
fn trim_end_matches_dot(s: Untrusted<Str>) -> Untrusted<Str> {
  s.strip_suffix(".") ?? s
}

@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)
  let v = solve(turn.message)
  let how = if v.adjudicated { ", adjudicated" } else { "" }
  let out = "{v.answer}\n\n(agreement {v.agreement:.2} over {v.samples} samples{how})"
  context.append_assistant(conv, out)
  { text: out, status: ReplyStatus.done }
}
```

`…trim_end_matches_dot()` calls a user function as a method (chapter 03, modules).

## 4. Debate between two harnesses with a judge

Each side is a harness on a different model with its own system prompt; the judge runs on a
frontier model and sees only the record of the debate.

```ratio
meta { name: "debate", version: "1.0.0", description: "Two models argue a question; a third judges" }

const RUBRIC = "Decide which side argued better on the evidence and reasoning given, not on style. Pass means the 'for' side won, fail means the 'against' side won, unsure means neither did."

fn argue(t: DebateTurn, side: Str) -> Untrusted<Str> {
  model.generate(prompt {
    system "You are a debater arguing {side} the proposition. Be honest: no invented facts, and concede points you cannot answer."
    task t.question
    if t.round > 1 { data text.show(t.record) as "the debate so far" }
    instruct "Round {t.round}: make your strongest case {side}, and answer the other side's latest points."
  })
}

harness pro(t: DebateTurn) -> Untrusted<Str>
  with { model: "consul", context: none, effort: high }
{ argue(t, "for") }

harness con(t: DebateTurn) -> Untrusted<Str>
  with { model: model.route(tier: strong, location: cloud), context: none, effort: high }
{ argue(t, "against") }

harness judge(r: DebateRecord) -> Untrusted<Judgement>
  with { model: model.route(tier: frontier), context: none, effort: max, sampling: { temperature: 0.0 } }
{
  model.judge(text.show(r), rubric: RUBRIC)
}

@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)
  let d = think.debate(turn.message, sides: [pro, con], judge: judge, rounds: 2)
  let side = match d.verdict.verdict { pass => "for", fail => "against", unsure => "neither side" }
  let out = "Verdict: {side} (score {d.verdict.score:.2}).\n\n{d.verdict.reasons}"
  context.append_assistant(conv, out)
  { text: out, status: ReplyStatus.done }
}
```

## 5. Tree-of-thought search

Best-first search over partial solutions. A model proposes next steps, another call rates each
partial solution, and evaluations are memoised so a node is never rated twice.

```ratio
meta { name: "tree-of-thought", version: "1.0.0", description: "Best-first search over partial solutions" }

type Node = { steps: [Str], state: Str "What has been established so far" }
type Proposals = { @len(1, 4) next: [Node] }
enum Rating { sure, likely, unlikely, impossible }
type Evaluation = { rating: Rating, solved: Bool, why: Str }

pub harness search(problem: UserText) -> Untrusted<Str>
  with { context: none, budget: { model_calls: 90, tokens: 300k } }
{
  let rate = memo(|n: Untrusted<Node>| evaluate(problem, n))
  let root: Untrusted<Node> = { steps: [], state: problem }
  let found = think.tree_search(root,
    expand: |n| propose(problem, n),
    score: |n| score_of(rate(n)),
    is_goal: |n| rate(n).solved,
    strategy: best_first, beam: 3, max_nodes: 30, max_depth: 6)
  eval.metric("expanded", math.to_float(found.expanded))
  eval.label("goal_reached", text.show(found.goal_reached))
  if found.goal_reached {
    text.join(found.best.steps, "\n")
  } else {
    "No complete solution found. Most promising partial solution:\n{text.join(found.best.steps, "\n")}"
  }
}

fn propose(problem: UserText, n: Untrusted<Node>) -> [Untrusted<Node>] {
  model.generate_as<Proposals>(prompt {
    instruct "Propose up to four different next steps. Each must follow from the current state."
    task problem
    data text.join(n.steps, "\n") as "steps so far"
    data n.state as "current state"
    format Proposals
  }, opts: { sampling: { temperature: 0.8 } }).next
    .map(|c| { steps: n.steps + c.steps, state: c.state })
}

fn evaluate(problem: UserText, n: Untrusted<Node>) -> Untrusted<Evaluation> {
  model.generate_as<Evaluation>(prompt {
    instruct "Rate how likely this partial solution is to lead to a correct solution, and say whether it already is one."
    task problem
    data text.join(n.steps, "\n") as "partial solution"
    format Evaluation
  }, opts: { sampling: { temperature: 0.0 } })
}

fn score_of(e: Untrusted<Evaluation>) -> Float {
  match e.rating { sure => 1.0, likely => 0.6, unlikely => 0.2, impossible => 0.0 }
}

@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)
  let answer = search(turn.message)
  context.append_assistant(conv, answer)
  { text: answer, status: ReplyStatus.done }
}
```

`rate(n).solved` and `score_of(rate(n))` read trusted fields (a `Bool` and a unit enum) of an
untrusted record, so the search's control flow needs no `trust`.

## 6. Retrieval over memory, earlier sessions and the web

Rewrites the question into queries, retrieves from three sources in parallel, reads the best
candidates, reranks them, answers with numbered citations only, and checks the answer against
the cited sources.

```ratio
meta { name: "research", version: "1.0.0", description: "Cited answers from memory, past sessions and the web" }

type Queries = { @len(1, 4) queries: [Str] }
type Source = { origin: Untrusted<Str>, title: Untrusted<Str>, text: Untrusted<Str> }
type Cited = { answer: Str, citations: [Int], gaps: [Str] "What the sources do not settle" }
type Answer = { answer: Untrusted<Str>, sources: [Source], supported: Bool, gaps: [Untrusted<Str>] }

pub harness research(question: UserText, use_web: Bool = true) -> Answer
  with {
    tools: { need: [memory_search, memory_read, search_sessions], want: [web_search, web_fetch] },
    context: none,
    budget: { tokens: 200k, wall: 4m },
  }
{
  let qs = model.generate_as<Queries>(prompt {
    instruct "Write up to four short search queries that together cover the question."
    task question
    format Queries
  }).queries

  let (mem_hits, past_hits, web_hits) = par {
    qs.flat_map(|q| memory.search(q, limit: 4)) |> list.unique_by(|h| h.name)
    memory.search_sessions(question, limit: 5)
    if use_web and tools.has("web_search") {
      qs |> list.take(2) |> list.flat_map(|q| tools.web_search(q, count: 5)) |> list.unique_by(|h| h.url)
    } else { [] }
  }

  let from_memory = mem_hits |> list.take(4) |> list.map(|h| {
    let e = memory.read(h.name, scope: h.scope)
    { origin: "memory:{h.name}", title: e.description, text: e.body }
  })
  let from_sessions = past_hits |> list.map(|h| { origin: "session:{h.session_id}", title: h.title, text: h.snippet })
  let pages = par_map_settled(web_hits |> list.take(5), limit: 3, |h| {
    let p = tools.web_fetch(h.url, limit: 12000)
    { origin: h.url, title: p.title, text: p.text }
  }) |> list.filter_map(|r| match r { Ok { value } => value, Err { error } => none })

  let candidates = from_memory + from_sessions + pages
  if candidates.is_empty() {
    return { answer: "I found nothing on this in memory, earlier sessions or the web.", sources: [], supported: false, gaps: [] }
  }
  let sources = rerank(question, candidates) |> list.take(6)

  let cited = model.generate_as<Cited>(prompt {
    instruct "Answer from the numbered sources only. Cite every claim with source numbers. Say what the sources do not settle."
    task question
    for (i, s) in sources.enumerate() { data s.text as "[{i + 1}] {s.title}" }
    format Cited
  }, opts: { effort: high })

  let check = think.verify(cited.answer, sources.map(|s| s.text))
  { answer: cited.answer, sources: sources, supported: check.status is supported, gaps: cited.gaps }
}

/// Embedding rerank when an embedding model is configured, else a listwise model ranking.
fn rerank(question: UserText, xs: [Source]) -> [Source] {
  fallback([
    || {
      let vs = model.embed([question] + xs.map(|s| s.title + "\n" + s.text.truncate(2000)))
      vec.top_k(vs[0], vs |> list.drop(1), k: xs.len()) |> list.map(|(i, _)| xs[i])
    },
    || model.rank(xs, show: |s| s.title + "\n" + s.text.truncate(1500), criteria: "relevance to: {question}")
         |> list.map(|r| r.value),
  ])
}

@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)
  let a = research(turn.message)
  let refs = a.sources.map_indexed(|i, s| "[{i + 1}] {s.origin}") |> text.join("\n")
  let caveat = if a.supported { "" } else { "\n\nNote: a check could not confirm every claim against the sources." }
  let out = "{a.answer}{caveat}\n\nSources:\n{refs}"
  context.append_assistant(conv, out)
  { text: out, status: ReplyStatus.done }
}
```

## 7. Consul escalation (roadmap item 64)

The `@task` entry runs a subagent's turn with the native loop's structure, counts rejections, and
switches the rest of the turn to Consul after the second; the `@turn` entry has Consul critique
the root's plan whenever it changes. Chapter 08 maps each requirement of item 64 to this code.

```ratio
meta {
  name: "std:consul-escalation",
  version: "1.0.0",
  description: "Consul takes over a subagent's task after two rejections in one turn, and critiques the root's plans",
}

/// The model table id of Imperium's house model Consul.
const CONSUL = "consul"
/// Rejections, within one turn of one subagent task, before Consul takes over. Not a setting.
const TAKEOVER_AFTER = 2
const RESULT_RUBRIC = "Accept only a result that does what the task asked, says what was verified and what was not, and claims nothing the transcript does not show."
const PLAN_RUBRIC = "A good plan has steps in a workable order, an acceptance check for each step that can actually be checked, and no step that needs information nobody has."
const QUESTION_MARK = "QUESTION:"

@task
pub harness task(task: Task) -> Report
  with { context: main }
{
  let conv = context.main()
  context.append_user(conv, prompt {
    task task.brief
    note "If you cannot continue without an answer from whoever gave you this task, reply with one line starting {QUESTION_MARK} and nothing else."
  })

  var rejections = 0
  var consul = false
  var steps = 0
  let max_steps = session.info().max_steps

  loop {
    if steps >= max_steps {
      break { text: "Stopped at the step limit before the task was finished.", ok: false }
    }
    let opts: GenOpts = if consul { { model: CONSUL } } else { {} }
    let reply = model.chat(conv, opts: opts)
    steps += 1

    if reply.tool_calls.is_empty() {
      // A question goes back up to whoever dispatched the work.
      if let q = reply.text.trim().strip_prefix(QUESTION_MARK) {
        raise question(q.trim())
      }
      // A result: Consul reviews it before it is reported. A refusal is a rejection.
      let review = think.critique(reply.text, rubric: RESULT_RUBRIC, opts: { model: CONSUL })
      if review.pass {
        eval.label("finished_by", if consul { "consul" } else { "subagent" })
        break { text: reply.text, ok: true }
      }
      rejections += 1
      context.notice(conv, "A reviewer did not accept this result. Address the issues below, then report again.",
        data: text.show(review.issues))
    } else {
      // A declined approval, a failing tool and a failing command are each a rejection.
      let results = tools.dispatch_all(conv, reply.tool_calls)
      rejections += results.count(is_rejection)
    }

    if not consul and rejections >= TAKEOVER_AFTER {
      consul = true
      eval.metric("consul_takeovers", 1.0)
      session.notify("Consul is taking over \"{task.title}\" after {rejections} rejections.")
      context.notice(conv, "From here a stronger model continues this task. Read the conversation so far and carry on from where it stands; do not start again.")
    }
  }
}

fn is_rejection(r: ToolResult) -> Bool {
  match r.outcome {
    Denied { by } => by is user,
    failed => true,
    _ => false,
  }
}

/// The root agent: the default loop, plus a Consul critique of every new or changed plan.
@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)
  var steps = 0
  let max_steps = session.info().max_steps
  var last_plan_hash = hash.stable(session.plan())

  loop {
    if steps >= max_steps {
      break { text: "Stopped at the step limit.", status: ReplyStatus.step_limit }
    }
    let reply = model.chat(conv)
    steps += 1
    if reply.tool_calls.is_empty() {
      if context.pending(conv).any(|p| p.kind is steering) { continue }
      break { text: reply.text, status: ReplyStatus.done }
    }
    tools.dispatch_all(conv, reply.tool_calls)

    let plan = session.plan()
    if hash.stable(plan) != last_plan_hash {
      last_plan_hash = hash.stable(plan)
      if let p = plan {
        let review = think.critique(text.show(p), rubric: PLAN_RUBRIC, opts: { model: CONSUL })
        if not review.pass {
          context.notice(conv, "Consul reviewed your plan and found problems. Revise the plan with update_plan before you act on it.",
            data: text.show(review.issues))
        }
      }
    }
  }
}
```

## 8. Test-driven code fix

A subagent harness that will not report success until the project's tests pass, refuses to make
them pass by editing the tests, and records a checkpoint before it changes anything.

```ratio
meta { name: "std:tdd-fix", version: "1.0.0", description: "Fix code until the tests pass, without touching the tests" }

const ATTEMPTS = 3
const EDIT_TOOLS: ToolSet = [read_file, grep, glob, list_dir, edit_file, write_file]

@task
pub harness fix(task: Task) -> Report
  with { context: main, budget: { tokens: 400k, wall: 30m } }
{
  let conv = context.main()
  let command = test_command()
  let before = eval.run_tests(command)
  eval.metric("failing_before", math.to_float(before.failed))
  if before.ok {
    return { text: "All {before.passed} tests already pass; nothing to fix.", ok: true }
  }
  session.checkpoint("before std:tdd-fix")

  context.append_user(conv, prompt {
    instruct "Make the failing tests pass by changing the code under test. Do not edit, delete or skip tests. Change as little as possible, and stop when you believe the fix is complete."
    task task.brief
    data before.raw.truncate(8000) as "test output"
  })

  var last = before
  for attempt in 1..=ATTEMPTS {
    model.loop(conv, max_steps: 25, tools: EDIT_TOOLS)

    let touched = test_files_touched()
    if not touched.is_empty() {
      context.notice(conv, "You changed test files. Revert those changes and fix the code instead.",
        data: text.join(touched, "\n"))
      continue
    }

    let after = eval.run_tests(command)
    eval.metric("failing_after_attempt", math.to_float(after.failed), tags: { attempt: text.show(attempt) })
    if after.ok {
      eval.score("tests", 1.0)
      return { text: "Fixed in {attempt} attempt(s): {after.passed} tests pass (were {before.failed} failing).", ok: true }
    }
    last = after
    context.notice(conv, "The tests still fail. The new output follows; continue from here.",
      data: after.raw.truncate(8000))
  }
  eval.score("tests", math.to_float(last.passed) / math.to_float(math.max(1, last.passed + last.failed)))
  { text: "Could not make the tests pass in {ATTEMPTS} attempts. Last failures:\n{last.raw.truncate(2000)}", ok: false }
}

/// The test command, from the files at the project root.
fn test_command() -> Str {
  let names = tools.list_dir(".").entries.map(|e| e.name)
  if "Cargo.toml" in names { "cargo test --quiet" }
  else if "go.mod" in names { "go test ./..." }
  else if "pyproject.toml" in names or "pytest.ini" in names { "python -m pytest -q" }
  else if "package.json" in names { "npm test --silent" }
  else { raise fail("no-tests", "no known test setup at the project root") }
}

/// Test files changed in the working tree, by git.
fn test_files_touched() -> [Untrusted<Str>] {
  let r = tools.run_command("git diff --name-only")
  r.stdout.lines().filter(|p| re.is_match(r"(^|/)(tests?|__tests__)/|_test\.(go|py)$|\.test\.[jt]sx?$|test_[^/]*\.py$", p))
}
```

## 9. Map-reduce over a large input

An input too large for one context is stored as an artefact, split by the engine's own chunking
rules, summarised in parallel on a small local model, and merged by a stronger one. Unlike
`agents.delegate_chunks`, nothing here becomes a subagent: the chunks are harness work within the
turn.

```ratio
meta { name: "digest", version: "1.0.0", description: "Summarise an input of any size by map-reduce" }

pub harness digest(input: Untrusted<Str>, focus: Str = "the main points, decisions and open problems") -> Untrusted<Str>
  with { context: none, budget: { tokens: 500k } }
{
  let window = model.window(model.route(tier: basic, location: local)) ?? 32768
  let a = context.store(input)
  let parts = context.chunks(a, chunk_tokens: window / 2)

  let notes = par_map(parts, limit: 4, |part| retry(|| model.summarize(part, max_words: 200, focus: focus,
    opts: { model: model.route(tier: basic, location: local) })))
  eval.metric("chunks", math.to_float(parts.len()))

  if notes.len() == 1 { return notes[0] }
  model.generate(prompt {
    instruct "Merge these partial summaries into one summary of the whole input. Keep every distinct point once; keep the order of the original."
    note "Focus on {focus}."
    for (i, n) in notes.enumerate() { data n as "part {i + 1} of {notes.len()}" }
  }, opts: { model: model.route(tier: strong) })
}

@turn
pub harness turn(turn: Turn) -> Reply
  with { context: main }
{
  let conv = context.main()
  context.append_turn(conv, turn)
  let sources = turn.attachments.filter_map(|a| a.text)
  let out = if sources.is_empty() {
    "Attach the document to summarise."
  } else {
    digest(text.join(sources, "\n\n"), focus: turn.message)
  }
  context.append_assistant(conv, out)
  { text: out, status: ReplyStatus.done }
}
```
