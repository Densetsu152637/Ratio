# Plan: Ratio-side implementation roadmap

Revision: 2026-11-26 (plan gate round 1 applied; base e2bb2e5, branch `feat/stage-2-parser`)

Goals (each verified by the named check):

- G1: every Ratio-side chapter-10 stage (1–12, and Ratio's part of 21, 23, 24 and optional 27) lands
  on `main` as its own squash-merged PR; `gh pr list --state merged` shows one PR per stage. Plan-only
  stages 2R and 2S are outside this count (2S has its own spec PR).
- G2: before each squash-merge, CI is fully green on the final pushed head (`gh pr checks <n>`), and
  an independent critic review of that head is recorded in the PR description. The review is
  security-focused for stages 2, 3, 5, 6, 9, 10, 11, 12 and the Ratio parts of 21, 23 and 24
  (hostile source, import escape, untrusted input, `path`/`url`/`codec`, secrets, frames or the `Host`
  boundary).
- G3: every rule a stage implements has a conformance file under `conformance/`, every conformance
  directory is mapped in `conformance/spec-map.txt`, and the `conformance` CI job (runner plus the
  `spec` test) passes.
- G4: a design review of performance, parallelism and concurrency exists
  (`docs/design/performance-concurrency-review.md`); every accepted proposal is in the specification
  with a draft bump and an erratum line in `spec/README.md`, and is assigned to a stage below.
- G5: `bash scripts/check-rust.sh` exits 0 on each stage PR's head before it is pushed.
- G6: "Imperium Implementation" is sent a message (recorded in this plan's outcome for the stage)
  when stage 11's `Host` trait is on `main`, and whenever the user creates a release tag.

Scope / non-goals:

- Scope: the Ratio repository only; the language, checker, IR, interpreter, built-ins, frames, the
  `Host` trait, tape and concurrency, test hosts, model and think.
- Non-goals: Imperium code (RatioHost and its independent `open_frame` attenuation check, gates,
  clients); per-install approval of agent-installed extensions and the rule that plugins never
  auto-use harnesses (Imperium stages 17 and 20; Ratio must not add anything that bypasses them);
  Emperor and Consul harness content; creating release tags (the user creates or approves them); the
  tag ruleset (a manual repository setting).

Assumptions:

- One PR per stage, never rebased or force-pushed. Spec ambiguity is decided minimally and recorded as
  an erratum with a draft bump in the same PR.
- Drafts are numbered in merge order: stage 2 takes 0.8, stage 2S takes 0.9, later stages bump as
  they need. Only one open PR bumps the draft at a time.
- Local Rust builds are allowed; `scripts/check-rust.sh` runs before each push.

Relevant paths: `spec/` (chapter 10 is the stage table and wins on conflict with this plan),
`crates/ratio-syntax`, `crates/ratio` (`ratio-lang` facade), `conformance/`, `CONTRIBUTING.md`,
`scripts/check-rust.sh`.

Related docs: `spec/10-implementation.md` (authoritative stage definitions), `CONTRIBUTING.md`
(process). This plan tracks status and the decisions not recorded elsewhere.

## Stage 2: Parser

- Objective: finish chapter 10 row 2 from the WIP at e2bb2e5.
- Depends on: stage 1 (done).
- Owned paths: `crates/ratio-syntax/**`, `crates/ratio/src/lib.rs`, `conformance/syntax/**`,
  `conformance/spec-map.txt`, `spec/02-syntax.md`, `spec/10-implementation.md`, `spec/README.md`.
- Acceptance checks:
  - the conformance runner passes, with corpus files for every production and rules 1–10 and a
    parse-error file for each of P0001–P0009;
  - the printer round-trip property (parse, print, parse, equal span-free dumps; printing is
    idempotent) passes;
  - nesting-limit tests pass, and nesting 256 parses in a debug build without stack overflow;
  - hostile input never panics and parses in linear time;
  - error recovery: a file with several independent errors reports each once;
  - `Diagnostic::render` output with file, line, column and caret is snapshot-tested;
  - every ```` ```ratio ```` block in `spec/` parses (or is marked as a deliberate error example);
  - draft 0.8 errata are in `spec/README.md`, `ratio::SPEC_DRAFT` is `"0.8"`, `ratio::STAGE` is 2;
  - CI green.
- Status: in progress
- Outcome:
- Handoff notes:
  - Already in the WIP: `ast.rs`, `parser.rs` (recursive descent, never yet run on real input),
    `diag.rs` P0001–P0009 and `Diagnostic::render`.
  - Remaining work:
    1. facade re-export of `parse`;
    2. `printer.rs`;
    3. a span-free `Module` s-expression dump;
    4. runner directives `parse-ok`, `parse-error CODE at L:C` and `tree:`;
    5. fix `conformance/syntax/later-stage-example.ratio` (a top-level `let` is not an item);
    6. fix `tests/conformance.rs` so only `Code::is_lexical()` codes need lex-error files;
    7. the tests listed in the acceptance checks;
    8. the docs (chapter 10 expectation kinds and P table, the READMEs).
  - Errata (a)–(e) to record:
    - (a) a P diagnostic series;
    - (b) parser nesting counts tree depth: one level per nested expression, type or pattern, per
      prefix and per chain operator, limited by `max_nesting`;
    - (c) the parser takes the token reader's brace kind as the rule-1 decision;
    - (d) `record_pattern` allows `{ .. }` and a trailing comma:
      `"{" [ ( field_pattern {"," field_pattern} ["," ".."] | ".." ) [","] ] "}"`;
    - (e) the grammar is applied strictly: trailing commas only where allowed, jumps are not
      operands, `()` is not an expression;
    - (f) `>` can end a statement inside a type declaration or a format section (the gap below).
  - Gap to fix: insertion condition 2 excludes `>`, so `type T = List<Int>` followed by a newline
    fails, as does `format Map<K,V>` in a prompt. Decision: inside a type declaration or a format
    section, `>` can end a statement. This needs a lexer change, a `check_insertions` update and a
    corpus file.
  - The five stage-1b review nits:
    1. ch02 says a head split across lines is unsupported (P0008);
    2. ch10: `open_frame` looks up the parent by frame path, consistently with the `FrameCtx` sketch;
    3. ch10: the release is created only by the release workflow, given the tag ruleset (cite
       `CONTRIBUTING.md`);
    4. the insertion property also asserts condition 3 and the attribute rule;
    5. a comment that `angles` also tracks comparison `<`.

## Stage 2R: Performance and concurrency design review (plan-only)

- Objective: ranked proposals for performance, parallelism and concurrency, each with spec and stage
  impact; root accepts or rejects each and records why.
- Depends on: none.
- Owned paths: `docs/design/performance-concurrency-review.md`.
- Acceptance checks: the review file exists with a decision per proposal; G4.
- Status: in progress
- Outcome:
- Handoff notes:

## Stage 2S: Specification update for accepted proposals (spec-only PR, draft 0.9)

- Objective: write the accepted proposals into the spec. Syntax they add is specified here and parsed
  by a named follow-up in the stage that first needs it (normally stage 3's PR, before resolution),
  so stage 2 is not reopened.
- Depends on: stage 2 merged, stage 2R decided.
- Owned paths: `spec/**`, `docs/design/performance-concurrency-review.md`.
- Acceptance checks: G4; docs CI job green; each accepted proposal names its implementing stage.
- Status: pending

## Stage 3: Modules and names

- Depends on: 2, 2S.
- Owned paths: `crates/ratio-syntax` or a new `crates/ratio-names` (decided at stage start),
  `conformance/names/**`.
- Acceptance checks: resolution corpus; shadowing refused across trust levels; import-trust rules per
  source; imports cannot escape the root or follow links.
- Status: pending

## Stage 4: Type checker

- Depends on: 3. Owned paths: a new checker crate, `conformance/types/**`.
- Acceptance checks: accepted and rejected programs with diagnostic codes in `conformance/types/`.
- Status: pending

## Stage 5: Schemas and taint (security review)

- Depends on: 4. Owned paths: checker crate, `conformance/taint/**`.
- Acceptance checks: one rejected program per sink and one accepted per allowed flow; property that
  generated programs never produce a `TrustedPrompt` or `task` section from untrusted sources; schema
  and digest snapshots; static `Secret<T>` labels where known.
- Status: pending

## Stage 6: IR and core interpreter (security review)

- Depends on: 5. Owned paths: new IR and interpreter crates, `conformance/eval/**`.
- Acceptance checks: `conformance/eval/`; IR hash stability snapshots across platforms; run-time label
  propagation tests for every operator; branch restrictions; steps, heap and call-depth limits.
- Status: pending

## Stages 7, 8, 9: Data I, II, III

- Depends on: 6 (7, then 8, then 9). Owned paths: built-in modules, `conformance/data/**`.
- Acceptance checks: one conformance test per built-in with each row's failures and label
  propagation; stage 9 adds float determinism with fixed bit patterns.
- Status: pending

## Stage 10: Frames (security review)

- Depends on: 9. Owned paths: frame model in the interpreter, `conformance/frames/**`.
- Acceptance checks: attenuation monotonicity property; ledger tests; `sees` refusals including
  labelled values entering frames without `secrets`.
- Status: pending

## Stage 11: Host, tape and concurrency (security review)

- Depends on: 10. Owned paths: `Host` trait, `ScriptedHost`, `TapeHost`, contract suite.
- Acceptance checks: replay determinism under random completion orders; record-then-replay gives
  identical event streams; replayed values keep labels and are untrusted; divergence tests; real
  concurrency; every secrecy sink refuses a labelled value at the guard and at the unguarded host;
  the contract suite's frame cases refuse a wider child scope in every dimension (the suite Imperium's
  `RatioHost::open_frame` must also pass, stage 13); the throwaway Imperium `RatioHost` spike's result
  against the candidate commit is recorded in the PR before merge (the spike code is never merged).
- Status: pending. On merge: message "Imperium Implementation" (G6).

## Stage 12: Model and think (security review)

- Depends on: 11. Owned paths: prompt rendering, `model.*`, `think.*`, `conformance/framing/**`.
- Acceptance checks: scripted-host tests per built-in; prompt rendering snapshots; framing vectors.
- Status: pending

## Ratio parts of the shared stages

- 21 (authored and offered harnesses, security review): depends on 12. Checker rules for authored
  harnesses, not-offerable rule, closure hashes, `meta.author`. Ratio side does not block on Imperium.
- 23 (built-in packages): depends on 12 and 21. `std/` sources embedded by the facade; examples as
  conformance programs. Imperium's parity suite is theirs.
- 24 (security review and hardening): blocked until Imperium stages 13–23 exist. Parser and checker
  robustness properties, fuzzing schedule, limit tests.
- 27 (optional, resume by replay): depends on 11.
