# Contributing to Ratio

## Language changes start in the specification

A change to the language edits `spec/` and, in the same pull request, the implementation and the
conformance files that test it. A change that only touches the specification bumps its draft
(`spec/README.md`'s version line and `ratio::SPEC_DRAFT`, which a test keeps equal) and records
what changed at the top of `spec/README.md`.

## Building

- Rust 1.89 or newer; `rust-toolchain.toml` picks stable with `rustfmt` and `clippy`.
- Edition 2024, resolver 3. Every crate inherits the workspace's lints: `unsafe_code = "forbid"`,
  and `clippy::all` plus `clippy::pedantic` as warnings, which CI turns into errors. Silence a
  pedantic lint only locally, with `#[allow(…, reason = "…")]`.
- The crates have no dependencies outside the workspace. Adding one needs a reason in the pull
  request.

## Checks

CI (`.github/workflows/ci.yml`) runs on every pull request and on `main`:

| Job | What it runs |
| --- | --- |
| `rust` (Linux, Windows, macOS) | `scripts/check-rust.sh`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` |
| `conformance` | The conformance runner (`cargo test -p ratio-syntax --test conformance`) and the check that the corpus and the specification agree (`cargo test -p ratio-lang --test spec`) |
| `msrv` | `cargo +1.89 check --workspace --all-targets --locked` |
| `docs` | markdownlint over every Markdown file (`.markdownlint-cli2.jsonc`), and `scripts/ci/check-links.py` for relative links and heading anchors |
| `license` | `scripts/ci/check-license.py`: `LICENSE` is MIT and every crate declares MIT |
| `workflows` | actionlint |

Run `bash scripts/check-rust.sh` before pushing; the Python checks need Python 3.11 or newer.

## Conformance files

`conformance/<chapter>/*.ratio` start with expectation lines (`// expect: …`, chapter 10, "The
conformance suite"). A new directory needs a line in `conformance/spec-map.txt` naming the
chapters and headings it tests; the `spec` test fails otherwise. Files are byte-exact inputs, so
`.gitattributes` marks `*.ratio` as not text: Git never rewrites their line endings.

## Releases

Releases are tags `vX.Y.Z` on `main`, made only from a commit whose CI is green, with the
workspace version bumped to match. Imperium depends on a tag; chapter 10 ("How Imperium pins
Ratio") describes how a release that changes the `Host` API or the IR reaches it.
