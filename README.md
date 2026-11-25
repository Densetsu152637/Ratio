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

Specification draft 0.6. Stage 1 of the [implementation plan](spec/10-implementation.md), the token
reader, is built. Ratio is a language in its own right: this repository holds its specification,
its implementation, its conformance suite and its releases. Imperium is a host: it depends on a
release of the `ratio` library and implements its `Host` trait against the agent engine
([chapter 10](spec/10-implementation.md#the-repository-split-and-the-boundary) says which stages
belong to which repository).

## Repository layout

| Path | Contents |
| --- | --- |
| [`spec/`](spec/README.md) | The specification; start at [spec/README.md](spec/README.md) |
| [`crates/ratio/`](crates/ratio/README.md) | Package `ratio-lang`, library `ratio`: the public facade, the only crate a host depends on |
| [`crates/ratio-syntax/`](crates/ratio-syntax/README.md) | Package and library `ratio-syntax`: the token reader (stage 1), and the parser from stage 2 |
| `conformance/` | The conformance corpus, by chapter (`syntax/` so far); `spec-map.txt` names the chapters each directory tests; `builtins.txt` is the built-in list (a stub until stage 7) |
| `scripts/` | The checks CI runs: `check-rust.sh`, and `ci/check-links.py` and `ci/check-license.py` |
| `.github/workflows/ci.yml` | CI |

The facade's package is `ratio-lang` because a bare `ratio` may be taken on crates.io; code still
reads `ratio::lex`. Later stages add crates behind the facade (a checker, the IR and interpreter,
the standard library); they are implementation details, and only `ratio` is public API.

## Building and testing

You need Rust 1.89 or newer (the minimum supported version, which matches Imperium's);
`rust-toolchain.toml` selects the stable toolchain with `rustfmt` and `clippy`. The crates have no
dependencies outside this repository.

```sh
cargo build --workspace
cargo test --workspace            # unit, property and conformance tests
bash scripts/check-rust.sh        # what CI runs: fmt --check, clippy -D warnings, tests
```

The conformance runner alone is `cargo test -p ratio-syntax --test conformance`. CI also checks
the minimum Rust version, the Markdown (markdownlint and relative links), the licence, and the
workflow files; [CONTRIBUTING.md](CONTRIBUTING.md) lists the checks and the conventions.

## Using Ratio from a host

Depend on a release tag (none is tagged yet; the first will be `v0.1.0`):

```toml
[dependencies]
ratio-lang = { git = "https://github.com/Densetsu152637/Ratio", tag = "v0.1.0" }
```

```rust
let lexed = ratio::lex("let x = 1");
assert!(lexed.is_clean());
```

Releases follow semantic versioning; chapter 10 defines the `Host` boundary and the
`HOST_API_VERSION` and `IR_VERSION` constants that accompany it once they exist.

## Licence

MIT; see [LICENSE](LICENSE).
