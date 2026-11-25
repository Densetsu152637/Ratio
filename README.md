# Ratio

Ratio is a small programming language for **agent harnesses**. A Ratio program says how an agent
behaves: how its model is prompted to think, how it processes what it reads, and how its tool
use, memory and control flow work. It is designed for [Imperium](https://github.com/Densetsu152637/Imperium-AI),
whose daemon runs Ratio programs as selectable harnesses.

The central idea is that **harnesses are callable like functions**. One harness calls another
with typed arguments and gets a typed result back, with its own model, prompt, tools and budget,
and never more authority than its caller. Reasoning methods such as decomposition, debate,
verification loops, tree search, self-critique and retrieval pipelines are then ordinary
compositions of harness calls.

```ratio
harness answer(question: Str) -> Str {
  let votes = think.self_consistency(5, || model.generate(prompt {
    instruct "Answer the question. End with one line: ANSWER: <answer>."
    task question
  }), key: |a| text.after_last(a, "ANSWER:").trim())
  votes.winner
}
```

## Status

Design specification only; no implementation yet. The interpreter is planned as the Rust crate
`imperium-ratio` inside the Imperium workspace (see the
[implementation plan](spec/10-implementation.md)).

## Specification

Start at [spec/README.md](spec/README.md).

## Licence

MIT; see [LICENSE](LICENSE).
