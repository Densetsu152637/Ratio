# Plan: Ratio-side implementation roadmap

Revision: 2026-11-26, end of week (stages 1-8 and 10 merged, spec draft 0.16; 11a in PR #13; C and 9
converged; coordinator: the "Imperium Implementation" session sets order and priorities)

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
  when stage 11a's `Host` trait is on `main`, and whenever the user creates a release tag.

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
- Status: done
- Outcome: PR #3 squash-merged as 0794aba; CI 8/8 green; security critic converged in 3 rounds (8 findings accepted).
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
- Status: done
- Outcome: review converged after 4 security-critic rounds; committed with stage 2S.
- Handoff notes:

## Stage 2S: Specification update for accepted proposals (spec-only PR, draft 0.9)

- Objective: write the accepted proposals into the spec. Syntax they add is specified here and parsed
  by a named follow-up in the stage that first needs it (normally stage 3's PR, before resolution),
  so stage 2 is not reopened.
- Depends on: stage 2 merged, stage 2R decided.
- Owned paths: `spec/**`, `docs/design/performance-concurrency-review.md`.
- Acceptance checks: G4; docs CI job green; each accepted proposal names its implementing stage.
- Status: done (PR #4, squash-merged as 21ab549)
- Handoff notes:
  - Stage 2R converged after 4 security-critic rounds (23 findings accepted, 0 rejected); the review is
    `docs/design/performance-concurrency-review.md` (sha256 prefix 5908123c). Accepted PC-1..PC-9,
    deferred PC-10..PC-15; stage 11 splits into 11a and 11b (see its Decisions table).
  - Imperium's answers (2026-11-26, from its code at origin/experimental) to fold in as Host
    requirements:
    - unpriced models are admitted under a cost cap today; refusing them is new. Specify the refusal
      unless the model is configured with an explicit price (a price of 0 counts as priced).
    - drain-class calls need a finite bound: `run_command` is clamped 1–3600 s; MCP `timeout_s` has no
      upper clamp; browser tools have no per-call bound. Specify a cap (at most 3600 s) on every
      drain-class call; Imperium adds the clamps.
    - no read-only command classifier exists. Specify it as a conservative allowlist owned by
      Imperium (unknown means not read-only); until it exists, every command is not read-only.

## Stage 3: Modules and names

- Depends on: 2, 2S.
- Owned paths: a new `crates/ratio-names` (decided at stage start), `crates/ratio/**`,
  `conformance/names/**`, and the draft 0.10 errata in `spec/**`.
- Acceptance checks: resolution corpus; shadowing refused across trust levels; import-trust rules per
  source; imports cannot escape the root or follow links.
- Status: done (PR #5, cac8ab5; spec draft 0.10)

## Stage 4: Type checker

- Depends on: 3. Owned paths: a new checker crate, `conformance/types/**`.
- Acceptance checks: accepted and rejected programs with diagnostic codes in `conformance/types/`.
- Status: done (PR #6, c834361; spec draft 0.11)

## Stage 5: Schemas and taint (security review)

- Depends on: 4. Owned paths: checker crate, `conformance/taint/**`.
- Acceptance checks: one rejected program per sink and one accepted per allowed flow; property that
  generated programs never produce a `TrustedPrompt` or `task` section from untrusted sources; schema
  and digest snapshots; static `Secret<T>` labels where known.
- Status: done (PR #7, squash-merged as f2f11a9; spec draft 0.12)

## Stage 6: IR and core interpreter (security review)

- Depends on: 5. Owned paths: new IR and interpreter crates, `conformance/eval/**`.
- Acceptance checks: `conformance/eval/`; IR hash stability snapshots across platforms; run-time label
  propagation tests for every operator; branch restrictions; steps, heap and call-depth limits.
- Status: done (PR #8, edc3dad, released as v0.1.0)

## Stages 7, 8, 9: Data I, II, III

- Depends on: 6 (7, then 8, then 9). Owned paths: built-in modules, `conformance/data/**`.
- Acceptance checks: one conformance test per built-in with each row's failures and label
  propagation; stage 9 adds float determinism with fixed bit patterns.
- Status: 7 done (PR #9, 4837678); 8 done (PR #12, dfb3d8f; spec draft 0.16); 9 security review
  converged (3 rounds), branch feat/wt-s9-data3 at ed6ad7a, takes the next free draft after C

## Stage 10: Frames (security review)

- Depends on: 9. Owned paths: frame model in the interpreter, `conformance/frames/**`.
- Acceptance checks: attenuation monotonicity property; ledger tests; `sees` refusals including
  labelled values entering frames without `secrets`.
- Status: done (PR #10, c665deb)

## Stage C: checker-complete and the `ratio check` CLI

- Requested by the user so the "Imperium Harnesses" session can write harnesses with full
  static checking before the run time exists.
- Depends on: 7 merged (built on stage 7's head; merges `origin/main` when stage 7 lands).
  Runs beside stages 8, 9 and 10, which add their own modules' signatures.
- Owned paths: the checker signatures of `agents`, `context`, `control` (with `log`, `trace`,
  `state`), `eval`, `memory`, `meta`, `model`, `session`, `think`, `tools`, and of `rand` and
  `time` (their run time is stage 11b), in `crates/ratio-check/src/builtins.rs` (one block per
  module) and the prelude types they need; `crates/ratio-check/tests/signatures.rs`; a new
  `crates/ratio-cli/**` (binary `ratio`); its conformance files in `conformance/types/` and
  `conformance/taint/`.
- Acceptance checks: every chapter 06 built-in outside `json`, `list`, `map` (stage 8) and
  `math` to `codec` (stage 9) has a signature, proved against `conformance/builtins.txt`; one
  accepted conformance file per new module, and rejected files for secrets at each new class of
  sink, untrusted text at trusted parameters, and installs and harness loads in
  `harnesses.check` and the guarded-effect lint; the chapter 09 examples check through
  `ratio check` but for the stage 8 and 9 built-ins and listed errata; `ratio check [--json]`
  with exit statuses 0, 1 and 2, `ratio --version` and `ratio help`, tested on the built
  binary; `bash scripts/check-rust.sh` and `scripts/ci/check-links.py` clean.
- Status: security review converged (4 rounds); branch feat/wt-sC-checker; takes the next free
  draft after 11a, with the 11a review notes and the Consul-as-harness erratum (example 7).

## Stage 11: Host, tape and concurrency (security review)

Split into two PRs (draft 0.9, chapter 10 rows 11a and 11b; design PC-1, PC-2, PC-3, PC-7 in 11a,
PC-4 and PC-9 in 11b).

### Stage 11a: Host, tape and scheduler

- Depends on: 10. Owned paths: `Host` trait (with `EffectCtx`, `tool_class`,
  `conversation_digest`), `ScriptedHost`, `TapeHost`, the scheduler, contract suite.
- Acceptance checks: replay determinism under random completion orders with sequenced, verified
  ledgers, and replay of one tape under permuted resolution orders; effect keys with branch paths;
  record-then-replay gives identical event streams; replayed values keep (and may gain) labels and
  are untrusted; divergence tests (tape mutations, reply digest, tampered blobs); width with parked
  branches and branch-path admission; cancellation classes, withdrawal, re-parenting and the drain
  fence; the host-contract requirement that every `drain`-class tool declares a bound of at most
  3600 s and is failed there; real concurrency; every secrecy sink refuses a labelled value at the
  guard and at the unguarded host; the contract suite's frame cases refuse a wider child scope in
  every dimension (the suite Imperium's `RatioHost::open_frame` must also pass, stage 13); the
  throwaway Imperium `RatioHost` spike's result against the candidate commit is recorded in the PR
  before merge (the spike code is never merged).
- Status: in review: PR #13 (draft 0.17), security review converged (3 rounds plus final check);
  merges after green CI and the Imperium spike result is recorded. On merge: message "Imperium
  Implementation" (G6).

### Stage 11b: Control, rand, time, state

- Depends on: 11a. Owned paths: `control` (with `par_fold` and single-flight caches), `rand`,
  `time`, `state`, their conformance files.
- Acceptance checks: chapter 10 row 11b: `par_fold` in index and completion order, after `Stop` and
  on a raise; single-flight caches (one producer, promotion, no deadlock, verified waits in
  replay); labels kept by session caches and the `sees` refusal of labelled hits; per-branch `rand`
  draws identical under permuted orders with no tape entries.
- Status: pending.

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

## Loops, vectorised operations and functional syntax (design accepted)

Design: [loops-and-vector-ops.md](../design/loops-and-vector-ops.md), revision 4. Requested by
the user: compatible and optimised loops and vectorised operations, extra functional syntax,
`Result` and monadic chains, and a Haskell–Java style. The user adopted comprehensions, `_`
placeholders, `|> .method` stages, `scan` and `zip_with`. Each slice takes the next free spec
draft when it merges, one at a time, and the files each slice owns are listed in the design.

- **P0** (equivalence errata and the `conformance/equiv/` corpus): after stages 8, 9 and 10.
- **P1** (built-in speed-ups in `plist.rs` and `builtins/`): after 8 and 9, alongside 11a.
- **D4** (the `maybe` and `either` modules, `scan`, `zip_with` and the internal
  `list.comprehend`): after 8, 9, 10, C, P0 and P1.
- **P2/P2b** (interpreter speed-ups LV-1 to LV-5 and LV-8): after 11a; never open beside 11b.
- **F** (syntax: comprehensions, `_`, `|> .method`, `then`/`compose`, `where`, postfix `?`,
  `do` chains, C0039 rules): after D4, C and 10, before stage L.
- **Q2** (`if`, `if let`, `while let` and `match` values carry their condition's labels, by the
  rule in the design; adopted by the user as hardening, not a guarantee): after 11a; never open
  beside P2.
- Status: design accepted (critic converged after four revisions); slices pending.

## Stage L: `.ratio` text linter

- Placement: after the Ratio-side language stages (1–12 and Ratio's parts of 21 and 23). Stages 24
  and 27 are not blocked by it, and it does not gate the language-ready notification.
- Entry criterion: starts only after the **language-ready** notification has been sent to the
  "Imperium Implementation" and "Imperium Harnesses" sessions (language-ready = stages 1–12 and
  Ratio's parts of 21 and 23 on `main`), recorded in this plan's outcome for that stage.
- Goal: a `ratio lint` checker for style and suspicious-pattern rules over `.ratio` source, with
  its own conformance directory (mapped in `conformance/spec-map.txt`) and stable lint codes.
- Scope: to be detailed when the stage starts.
- Status: pending
