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

Specification draft 0.8. Stages 1 to 5 of the [implementation plan](spec/10-implementation.md)
are built: the token reader, the parser, modules and names, the type checker, and schemas and
taint. Ratio is a language in its own right: this repository holds its specification,
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
| [`crates/ratio-names/`](crates/ratio-names/README.md) | Modules and names (stage 3): sources, imports, the registry trait and name resolution |
| [`crates/ratio-check/`](crates/ratio-check/README.md) | The type checker (stage 4) with schemas and taint (stage 5) |
| `std/` | The built-in packages' sources (`std:default` and the rest), embedded by the `ratio` library; it arrives with stage 23 |
| `conformance/` | The conformance corpus, by chapter (`syntax/`, `names/`, `types/`, `taint/`); `spec-map.txt` names the chapters each directory tests; `builtins.txt` is the built-in list (a stub until stage 7) |
| `scripts/` | The checks CI runs: `check-rust.sh`, and `ci/check-links.py` and `ci/check-license.py` |
| `.github/workflows/` | `ci.yml` (every pull request and `main`) and `release.yml` (every `v*` tag) |

The facade's package is `ratio-lang` because a bare `ratio` may be taken on crates.io; code still
reads `ratio::lex`. Later stages add crates behind the facade (a checker, the IR and interpreter,
the standard library); they are implementation details, and only `ratio` is public API.

## Building and testing

You need Rust 1.89 or newer (the minimum supported version, which matches Imperium's);
`rust-toolchain.toml` pins the toolchain CI uses (1.99.1, with `rustfmt` and `clippy`). The crates have no
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

Depend on a release tag. None exists yet: the first, `v0.1.0`, is tagged after the workspace
lands on `main`. The crates are not on crates.io (`publish = false`); publishing, and reserving
the names, is deferred until the user's account is set up for it.

```toml
[dependencies]
ratio-lang = { git = "https://github.com/Densetsu152637/Ratio", tag = "v0.1.0" }
```

```rust
let lexed = ratio::lex("let x = 1");
assert!(lexed.is_clean());
```

For local development against a Ratio checkout, add a patch to Imperium's workspace in a local
commit that is never merged (Imperium's CI fails on it):

```toml
[patch."https://github.com/Densetsu152637/Ratio"]
ratio-lang = { path = "../Ratio/crates/ratio" }
```

Releases follow semantic versioning; chapter 10 defines the `Host` boundary and the
`HOST_API_VERSION`, `IR_VERSION` and `TAPE_VERSION` constants that accompany it once they
exist.

## Licence

MIT; see [LICENSE](LICENSE).
