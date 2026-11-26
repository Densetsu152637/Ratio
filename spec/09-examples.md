# 09 Examples

Nine complete packages. Each is a whole `.ratio` file, re-checked by hand against chapters 02 to
06 of draft 0.3 (argument order, turbofish type arguments, record and block disambiguation and
parenthesised heads, semicolon insertion, constants, contextual variants, taint and `Brief`
sinks, `sees`, secrecy, tool gating). They are
also the first entries of the conformance suite (chapter 10), where the checker and the scripted
host will confirm them mechanically.

**Which harnesses models can run.** `run_harness` reaches only public, non-entry harnesses whose
parameters hold no trusted text (chapter 07). Example 3's `solve` is offered (it is in `std:*`),
so it takes its question as `Untrusted<Str>` and puts it in a `data` section. The public harnesses
of examples 5 (`search`), 6 (`research`) and 9 (`digest`) take `UserText` or `Str` parameters on
purpose, because their entries pass them the user's own words or the harness's own constants;
they are callable from Ratio code but not offerable to models, and `harnesses.check` says so.

| # | Package | Shows |
| --- | --- | --- |
| 1 | `std:default` | The native agent loop |
| 2 | `std:plan_execute_verify` | Planning, step execution with acceptance checks, re-planning, verification |
| 3 | `std:self_consistency` | Sampling and voting, escalation on low agreement |
| 4 | `user:debate` | Two harnesses on different models argue; a third judges |
| 5 | `user:tree_of_thought` | Search over partial solutions with memoised evaluation |
| 6 | `user:research` | Retrieval over memory, earlier sessions and the web, reranking, cited answers, verification |
| 7 | `std:consul_escalation` | Roadmap item 64 |
| 8 | `std:tdd_fix` | A test-driven code-fix subagent |
| 9 | `user:digest` | Map-reduce over an input too large for one context |

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
  name: "std:plan_execute_verify",
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
      task as_task(step.title, because: "a step of the plan this harness's planner wrote")
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

The step title reaches the `task` section only through `as_task`, which is journalled with its
reason; the acceptance criterion and the transcript go to the checker as framed data.

## 3. Self-consistency voting

Samples several worked solutions, votes on the normalised final answers, and when agreement is
low asks a frontier model to adjudicate between the distinct answers.

```ratio
meta {
  name: "std:self_consistency",
  version: "1.0.0",
  description: "Sample several solutions, vote on the answer, adjudicate when they disagree",
}

type Solution = {
  reasoning: Str "Brief working",
  answer: Str "The final answer only: a number without units or separators, or a short phrase",
}
type Adjudication = { answer: Str, why: Str }
type Voted = { answer: Untrusted<Str>, agreement: Float, samples: Int, adjudicated: Bool }

/// Offered to models through run_harness (it is in std:*), so its parameters are untrusted:
/// the problem arrives as data, never as an instruction.
/// Budget: n samples with up to two repairs each, plus one adjudication with repairs
/// (3n + 3 model calls at most); n is clamped to 9 so 32 always covers it.
pub harness solve(question: Untrusted<Str>, n: Int = 7) -> Voted
  with { context: none, sampling: { temperature: 0.7 }, budget: { model_calls: 32 } }
{
  let n = math.clamp(n, 1, 9)
  let vote = think.self_consistency(n,
    || model.generate_as::<Solution>(prompt {
      instruct "Solve the problem given as data. Work briefly in `reasoning`, then give only the final answer."
      data question as "problem"
      format Solution
    }),
    key: |s| normalise(s.answer))

  if vote.agreement >= 0.5 {
    return { answer: vote.key, agreement: vote.agreement, samples: n, adjudicated: false }
  }

  let candidates = vote.counts.keys()
  let pick = model.generate_as::<Adjudication>(prompt {
    instruct "Several attempts disagree. Decide which candidate answer to the problem given as data is correct, solving it yourself if none is."
    data question as "problem"
    data text.join(candidates, "\n") as "candidate answers"
    format Adjudication
  }, opts: { model: model.route(tier: frontier), effort: high, sampling: { temperature: 0.0 } })
  { answer: normalise(pick.answer), agreement: vote.agreement, samples: n, adjudicated: true }
}

fn normalise(a: Untrusted<Str>) -> Untrusted<Str> {
  a.trim().lower().replace(",", "").trim_end_matches(".")
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

## 4. Debate between two harnesses with a judge

Each side is a harness on a different model with its own system prompt; the judge runs on a
frontier model and sees only the record of the debate.

```ratio
meta { name: "debate", version: "1.0.0", description: "Two models argue a question; a third judges" }

const RUBRIC = "Decide which side argued better on the evidence and reasoning given, not on style. Pass means the 'for' side won, fail means the 'against' side won, unsure means neither did."

fn argue(t: DebateTurn, side: Str) -> Untrusted<Str> {
  model.generate(prompt {
    system "You are a debater arguing {side} the proposition. Be honest: no invented facts, and concede points you cannot answer."
    data t.question as "proposition"
    if t.round > 1 { data text.show(t.record) as "the debate so far" }
    instruct "Round {t.round}: make your strongest case {side} the proposition, and answer the other side's latest points."
  })
}

harness pro(t: DebateTurn) -> Untrusted<Str>
  with { model: "consul", context: none, effort: high }
{
  argue(t, "for")
}

harness con(t: DebateTurn) -> Untrusted<Str>
  with { model: model.route(tier: strong, location: cloud), context: none, effort: high }
{
  argue(t, "against")
}

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
  let side = match d.verdict.verdict {
    pass => "for",
    fail => "against",
    unsure => "neither side",
  }
  let out = "Verdict: {side} (score {d.verdict.score:.2}).\n\n{d.verdict.reasons}"
  context.append_assistant(conv, out)
  { text: out, status: ReplyStatus.done }
}
```

`fail` in the `match` is the `Judgement` variant: contextual variants are not blocked by the
prelude's `fail` function (chapter 02, rule 10). The proposition reaches the debaters as data,
since `DebateTurn.question` is untrusted.

## 5. Tree-of-thought search

Best-first search over partial solutions. A model proposes next steps, another call rates each
partial solution, and evaluations are memoised so a node is never rated twice.

```ratio
meta { name: "tree_of_thought", version: "1.0.0", description: "Best-first search over partial solutions" }

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
  model.generate_as::<Proposals>(prompt {
    instruct "Propose up to four different next steps. Each must follow from the current state."
    task problem
    data text.join(n.steps, "\n") as "steps so far"
    data n.state as "current state"
    format Proposals
  }, opts: { sampling: { temperature: 0.8 } }).next
    .map(|c| { steps: n.steps + c.steps, state: c.state })
}

fn evaluate(problem: UserText, n: Untrusted<Node>) -> Untrusted<Evaluation> {
  model.generate_as::<Evaluation>(prompt {
    instruct "Rate how likely this partial solution is to lead to a correct solution, and say whether it already is one."
    task problem
    data text.join(n.steps, "\n") as "partial solution"
    format Evaluation
  }, opts: { sampling: { temperature: 0.0 } })
}

fn score_of(e: Untrusted<Evaluation>) -> Float {
  match e.rating {
    sure => 1.0,
    likely => 0.6,
    unlikely => 0.2,
    impossible => 0.0,
  }
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
untrusted record, so the search's control flow needs no `trust`. Memo hits are recorded on the
tape, so a replay rates the same nodes.

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
  let qs = model.generate_as::<Queries>(prompt {
    instruct "Write up to four short search queries that together cover the question."
    task question
    format Queries
  }).queries

  let (mem_hits, past_hits, web_hits) = par {
    qs.flat_map(|q| memory.search(q, limit: 4)) |> list.unique_by(|h| h.name)
    memory.search_sessions(question, limit: 5)
    if use_web and tools.has("web_search") {
      qs |> list.take(2) |> list.flat_map(|q| tools.web_search(q, count: 5)) |> list.unique_by(|h| h.url)
    } else {
      []
    }
  }

  let from_memory = mem_hits |> list.take(4) |> list.filter_map(|h| match result(|| memory.read(h.name, scope: h.scope)) {
    Ok { value } => { origin: "memory:{h.name}", title: value.description, text: value.body },
    Err { error } => none,
  })
  let from_sessions = past_hits |> list.map(|h| { origin: "session:{h.session_id}", title: h.title, text: h.snippet })
  let pages = par_map_settled(web_hits |> list.take(5), |h| {
    let p = tools.web_fetch(h.url, limit: 12000)
    { origin: h.url, title: p.title, text: p.text }
  }, limit: 3)
    |> list.filter_map(|r| match r {
      Ok { value } => value,
      Err { error } => none,
    })

  let candidates = from_memory + from_sessions + pages
  if candidates.is_empty() {
    return { answer: "I found nothing on this in memory, earlier sessions or the web.", sources: [], supported: false, gaps: [] }
  }
  let sources = rerank(question, candidates) |> list.take(6)

  let cited = model.generate_as::<Cited>(prompt {
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

`research` is a `none`-context frame with an empty `sees`: it reads nothing ambient, only what its
tools return. With secrecy enabled (it is off by default), a memory marked private is a secret
source; since `sees.secrets` is not granted, that `memory.read` raises `Denied { by: secrecy }`
at run time, `result` turns it into `Err`, and the entry is skipped. A harness that did hold the
secret (with `sees: { secrets: true }`) would find any web search on text derived from it refused
at run time by the host and at dispatch; the checker cannot see that label, because which memory
is private is only known when it is read.

## 7. Consul escalation (roadmap item 64)

The `@task` entry runs a subagent's turn with the native loop's structure, counts rejections, and
switches the rest of the turn to Consul after the second; after the takeover, Consul's own results
are reviewed by a different model. The `@turn` entry has Consul critique the root's plan whenever
it changes, before execution when the plan arrives alone. Chapter 08 maps each requirement of
item 64 to this code.

```ratio
meta {
  name: "std:consul_escalation",
  version: "1.0.0",
  description: "Consul takes over a subagent's task after two rejections in one turn, and critiques the root's plans",
}

/// The model table id of Imperium's house model Consul.
const CONSUL: ModelRef = "consul"
/// Reviews Consul's own results after a takeover: a model does not grade its own work.
const INDEPENDENT_CRITIC: ModelRef = model.route(tier: frontier)
/// Rejections, within one turn of one subagent task, before Consul takes over. Not a setting.
const TAKEOVER_AFTER = 2
const RESULT_RUBRIC = "Accept only a result that does what the task asked, says what was verified and what was not, and claims nothing the transcript does not show."
const PLAN_RUBRIC = "A good plan has steps in a workable order, an acceptance check for each step that can actually be checked, and no step that needs information nobody has."
const QUESTION_MARK = "QUESTION:"

@task
pub harness task(task: TaskInput) -> Report
  with { context: main }
{
  let conv = context.main()
  context.append_task(conv, task)
  context.notice(conv, "If you cannot continue without an answer from whoever gave you this task, reply with one line starting {QUESTION_MARK} and nothing else.")

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
      // A result is reviewed before it is reported; a refusal is a rejection.
      let critic: ModelRef = if consul { INDEPENDENT_CRITIC } else { CONSUL }
      let review = think.critique(reply.text, RESULT_RUBRIC, opts: { model: critic })
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
      if context.pending(conv).any(|p| p.kind is steering) {
        continue
      }
      break { text: reply.text, status: ReplyStatus.done }
    }

    let calls = reply.tool_calls
    let plan_alone = calls.len() == 1 and calls[0].name == "update_plan"
    tools.dispatch_all(conv, calls)

    let plan = session.plan()
    let h = hash.stable(plan)
    if h != last_plan_hash {
      last_plan_hash = h
      if let p = plan {
        let review = think.critique(text.show(p), PLAN_RUBRIC, opts: { model: CONSUL })
        if not review.pass {
          let advice = if plan_alone {
            "Consul reviewed your new plan before any step ran and found problems. Revise the plan with update_plan before acting on it."
          } else {
            "Consul reviewed your plan after the other calls sent with it had already run, and found problems. Check those calls' results, then revise the plan with update_plan."
          }
          context.notice(conv, advice, data: text.show(review.issues))
        }
      }
    }
  }
}
```

The rubric is passed positionally (`think.critique(work, rubric, opts:)`), so positional
arguments come before the named `opts`. `session.notify` interpolates `task.title`, which is
untrusted; notifications accept untrusted text and show it as plain text.

## 8. Test-driven code fix

A subagent harness that will not report success until the project's tests pass, refuses to make
them pass by editing the tests, and records a checkpoint before it changes anything.

```ratio
meta { name: "std:tdd_fix", version: "1.0.0", description: "Fix code until the tests pass, without touching the tests" }

const ATTEMPTS = 3
const EDIT_TOOLS: ToolSet = [read_file, grep, glob, list_dir, edit_file, write_file]
const TEST_PATH = r"(^|/)(tests?|__tests__|spec)/|_test\.(go|py|rs)$|\.(test|spec)\.[cm]?[jt]sx?$|(^|/)test_[^/]*\.py$"

@task
pub harness fix(task: TaskInput) -> Report
  with { context: main, budget: { tokens: 400k, wall: 30m } }
{
  let conv = context.main()
  context.append_task(conv, task)
  let command = test_command()
  let before = eval.run_tests(command)
  eval.metric("failing_before", math.to_float(before.failed))
  if before.ok {
    return { text: "All {before.passed} tests already pass; nothing to fix.", ok: true }
  }
  session.checkpoint("before std:tdd_fix")

  context.append_user(conv, prompt {
    instruct "Make the failing tests pass by changing the code under test. Do not edit, delete or skip tests. Change as little as possible, and stop when you believe the fix is complete."
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

/// The test command, from the files at the project root, checking that a test setup exists
/// rather than guessing from the language alone.
fn test_command() -> Str {
  let names = tools.list_dir(".").entries.map(|e| e.name)
  if "Cargo.toml" in names {
    return "cargo test --quiet"
  }
  if "go.mod" in names {
    return "go test ./..."
  }
  if "package.json" in names {
    let pkg = json.parse(tools.read_file("package.json").text)
    if json.get(pkg, "/scripts/test") != none {
      return "npm test --silent"
    }
  }
  let pytest_config = "pyproject.toml" in names and tools.read_file("pyproject.toml").text.contains("pytest")
  if "pytest.ini" in names or "conftest.py" in names or pytest_config {
    return "python -m pytest -q"
  }
  raise fail("no-tests", "no recognised test setup at the project root")
}

/// Test files changed, added, deleted or renamed in the working tree since the last commit, by git.
fn test_files_touched() -> [Untrusted<Str>] {
  let r = tools.run_command("git status --porcelain=v1 --untracked-files=all")
  r.stdout.lines()
    |> list.flat_map(|l| porcelain_paths(l.slice(3)))
    |> list.filter(|p| re.is_match(TEST_PATH, p))
}

/// The paths of one porcelain entry: both sides of a rename, else the one path; quotes removed.
fn porcelain_paths(entry: Untrusted<Str>) -> [Untrusted<Str>] {
  let parts = if let (from, to) = entry.split_once(" -> ") { [from, to] } else { [entry] }
  parts.map(|p| unquote(p.trim()))
}

fn unquote(p: Untrusted<Str>) -> Untrusted<Str> {
  let opened = p.strip_prefix("\"") ?? p
  opened.strip_suffix("\"") ?? opened
}
```

`text.split_once(" -> ")` separates a rename's two paths, and the entry is kept whole when there
is no arrow; both sides of a rename are checked, so renaming a test file away from the test
pattern is caught too, and deleted test files (a `D` in the second status column) are listed like changed ones. Git
quotes paths with unusual characters; `unquote` removes the quotes (escapes inside such a path
are left as they are, which only makes the pattern miss exotic names, never match wrongly). The subagent runs `session.checkpoint`, a pseudo-tool its entry frame
has; a frame without `session_checkpoint` in its tools would get `Denied`.

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

  let notes = par_map(parts, |part| retry(|| model.summarize(part, max_words: 200, focus: focus,
    opts: { model: model.route(tier: basic, location: local) })), limit: 4)
  eval.metric("chunks", math.to_float(parts.len()))

  if notes.len() == 1 {
    return notes[0]
  }
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
