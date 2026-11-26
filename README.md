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

Specification draft 0.14. Stages 1 to 7 of the [implementation plan](spec/10-implementation.md)
are built: the token reader, the parser, modules and names, the type checker, schemas and
taint, the IR with the core interpreter (the pure language, with run-time secrecy labels;
effects arrive with stage 11's host), and the first data built-ins (`text`, and `re` on a
linear-time engine) with their cost classes and the bounded pool large built-ins run on. Stage C
makes the checker complete for harness authors: every built-in outside the stage 8 and 9 data
modules has a checked signature, and the `ratio check` command checks packages on disk. Ratio is a language in its own right: this repository holds its specification,
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
| [`crates/ratio-names/`](crates/ratio-names/README.md) | Package and library `ratio-names`: modules and names (stage 3): sources, imports, source-qualified names, the `Registry` trait and name resolution |
| [`crates/ratio-check/`](crates/ratio-check/README.md) | Package and library `ratio-check`: the type checker (stage 4) with schemas and taint (stage 5): types, subtyping, inference, exhaustiveness, constants, sinks, trust sites, secrets, schemas, type digests and `harnesses.check`, and the side tables later stages read |
| [`crates/ratio-ir/`](crates/ratio-ir/README.md) | Package and library `ratio-ir`: the Core IR (stage 6): lowering, site ids, the canonical form and the IR hash |
| [`crates/ratio-cli/`](crates/ratio-cli/README.md) | Package `ratio-cli`, binary `ratio`: `ratio check` parses, resolves, type-checks and taint-checks packages on disk (stage C) |
| [`crates/ratio-eval/`](crates/ratio-eval/README.md) | Package and library `ratio-eval`: the core interpreter (stage 6): the prepared form, labelled values, the evaluator and its limits; the `text` and `re` built-ins, the regex engine and the offload pool (stage 7) |
| `std/` | The built-in packages' sources (`std:default` and the rest), embedded by the `ratio` library; it arrives with stage 23 |
| `conformance/` | The conformance corpus, by chapter (`syntax/`, `names/`, `types/`, `taint/`, `eval/` and `data/` so far); `spec-map.txt` names the chapters each directory tests; `builtins.txt` lists every built-in of chapter 06 with its signature, Needs and cost class, generated from the chapter (regenerate with `RATIO_BLESS=1 cargo test -p ratio-lang --test spec builtins`) |
| `scripts/` | The checks CI runs: `check-rust.sh`, and `ci/check-links.py` and `ci/check-license.py` |
| `.github/workflows/` | `ci.yml` (every pull request and `main`) and `release.yml` (every `v*` tag) |

The facade's package is `ratio-lang` because a bare `ratio` may be taken on crates.io; code still
reads `ratio::lex`. Later stages add crates behind the facade (the IR and interpreter,
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

## Checking harnesses with `ratio check`

Install the command from a checkout (it lands in Cargo's `bin` folder, normally on `PATH`):

```sh
cargo install --path crates/ratio-cli --locked
ratio check path/to/harness.ratio            # one file; its folder is the source root
ratio check path/to/package/ other.ratio     # a folder: its main.ratio, else its one `meta` file
ratio check --json harness.ratio             # one JSON object per diagnostic
```

Or run it without installing: `cargo run -p ratio-cli -- check harness.ratio`. Each diagnostic
is `file:line:col: severity CODE: message` with the source line and a caret; the exit status is
0 when clean (warnings allowed), 1 with errors and 2 for a usage or I/O error. Packages named
together may import one another as `user:<name>`; the `std:` packages arrive with stage 23, so
until then a `std:` import is reported as not found. Built-ins of the stage 8 and 9 data
modules (`json`, `list`, `map`, `math` to `codec`) are reported `C0034` until those stages land.
[crates/ratio-cli/README.md](crates/ratio-cli/README.md) has the details.

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
