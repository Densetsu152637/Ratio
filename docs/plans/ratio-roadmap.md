# Plan: Ratio-side implementation roadmap

Revision: 2026-11-26 (base e2bb2e5, branch `feat/stage-2-parser`)

Goals (each verified by the named check):

- G1: every Ratio-side stage (1–12 and Ratio's part of 21, 23, 24, 27 in `spec/10-implementation.md`)
  lands on `main` as its own squash-merged PR; `gh pr list --state merged` shows one PR per stage.
- G2: each stage PR has green CI on its head (`gh pr checks <n>` all pass) and an independent critic
  review recorded in the PR description before merge.
- G3: every rule a stage implements has a conformance file under `conformance/`; the `conformance`
  CI job passes.
- G4: a design review of performance, parallelism and concurrency exists
  (`docs/design/performance-concurrency-review.md`), and every accepted proposal is in the
  specification with a draft bump and an erratum line in `spec/README.md`, and is assigned to a stage
  below.
- G5: `bash scripts/check-rust.sh` exits 0 on each stage PR's head before it is pushed.
- G6: "Imperium Implementation" is messaged when stage 11's `Host` trait is on `main`, and at each
  release tag.

Scope / non-goals:

- Scope: the Ratio repository only; the language, checker, IR, interpreter, built-ins, frames, the
  `Host` trait, tape and concurrency, test hosts, model and think.
- Non-goals: Imperium code (RatioHost, gates, clients), Emperor and Consul harness content, creating
  release tags (the user creates or approves them), the tag ruleset (a manual repository setting).

Assumptions:

- One PR per stage, never rebased or force-pushed. Spec ambiguity is decided minimally and recorded as
  an erratum with a draft bump in the same PR.
- Local Rust builds are allowed; `scripts/check-rust.sh` runs before each push.
- Language changes from the design review land as a spec-only PR (draft bump) before the stage that
  implements them, unless a change only touches a stage not yet started, in which case it lands with
  that stage.

Relevant paths: `spec/` (chapter 10 is the stage table and wins on conflict with this plan),
`crates/ratio-syntax`, `crates/ratio` (`ratio-lang` facade), `conformance/`, `CONTRIBUTING.md`,
`scripts/check-rust.sh`.

Related docs: `spec/10-implementation.md` (authoritative stage definitions), `CONTRIBUTING.md`
(process). This plan tracks status only.

## Stage 2: Parser

- Objective: finish the parser per chapter 10 row 2 and the hand-over list (printer, tree dump,
  corpus, runner, round-trip property, nesting and hostile-input tests, the five stage-1b nits, the
  `>` statement-end gap, draft 0.8 errata).
- Depends on: stage 1 (done).
- Owned paths: `crates/ratio-syntax/**`, `crates/ratio/src/lib.rs`, `conformance/syntax/**`,
  `conformance/spec-map.txt`, `spec/02-syntax.md`, `spec/10-implementation.md`, `spec/README.md`.
- Acceptance checks: conformance runner passes with a parse-error file per P code;
  round-trip property passes; nesting 256 parses without stack overflow in debug; CI green.
- Status: in progress
- Outcome:
- Handoff notes: WIP head e2bb2e5 from the previous owner. Parser decisions (a)–(e) from the hand-over
  (P series, nesting by tree depth, reader brace kind for rule 1, `record_pattern` with `..` and a
  trailing comma, strict grammar) become 0.8 errata.

## Stage 2R: Performance and concurrency design review

- Objective: review the language for performance, parallelism and concurrency improvements;
  produce ranked proposals with spec and stage impact; root accepts or rejects each.
- Depends on: none (read-only over the draft 0.7 spec).
- Owned paths: `docs/design/performance-concurrency-review.md`.
- Acceptance checks: G4.
- Status: in progress
- Outcome:
- Handoff notes:

## Stage 2S: Specification update for accepted proposals

- Objective: write accepted proposals into the spec (draft bump), including grammar and parser
  support if syntax changes, with conformance files.
- Depends on: stage 2, stage 2R.
- Status: pending

## Stages 3–12, 21, 23, 24, 27 (Ratio parts)

- Objective: as chapter 10's table, plus accepted proposals assigned to each.
- Depends on: the previous stage in chapter 10 order.
- Status: pending
