# ratio-check

Ratio's type checker, with schemas and taint: stages 4 and 5 of the
[implementation plan](../../spec/10-implementation.md).
Hosts do not depend on this crate; they use the [`ratio` facade](../ratio/README.md), which
re-exports it as `ratio::checker` and its entry point as `ratio::check`.

## What it does

`check(&loaded)` takes a package loaded by [`ratio-names`](../ratio-names/README.md) (stage 3)
and checks every module that parsed cleanly. It returns `Checked`: the diagnostics (the `C`
codes below) and, per module, the side tables later stages read. For stage 6's lowering it also names
each enum's declaration (`Checked::enum_items`, by `EnumId`) and each built-in reference
(`Checked::builtin_name`: `list.push`, or a bare `fail`).

```rust
let loaded = ratio::Loader::new(&registry, &builtin).load(&name);
let checked = ratio::check(&loaded);
if checked.is_clean() { /* stage 5 and 6 read checked.tables */ }
```

The rules are chapter 03's ("Types", "Evaluation", "Control flow") and chapter 02's
(disambiguation rules 3, 6 and 10, "Constants"):

- **Types.** Scalars, the text types, `Json`, `Image`, prompts, handles, `T?` (`T??` is `T?`),
  lists, tuples of at most 8, records (structural, fields in name order, `b?: B` reads as `B?`),
  maps (keys `Str`, `Untrusted<Str>`, `Int`, `Bool` or a unit-only enum), nominal enums whose
  payloads are records, function and harness types. Functions and type aliases may be
  generic (`Scored<T>`); harnesses are not, and neither are enums a program declares
  (`C0033`): only the built-in `Result<T>` is. An enum's type arguments relate by the variance
  its payloads give each parameter (computed as a fixpoint; `Result<T>` is covariant), never
  covariantly by default.
- **Subtyping** is chapter 03's list: `T <: T?`, `Never <: T`, `T <: Untrusted<T>`,
  `UserText <: Str <: Brief <: Untrusted<Str>`, `TrustedPrompt <: Prompt`, a harness is a
  function, covariant lists, options, tuples, records, map values and tasks, enum arguments by
  their variance, contravariant function parameters. No width subtyping: a record value has exactly its
  declared fields (a required field may stand where an optional one is declared), and a spread
  against a declared type builds the narrower record (`let n: { x: Int } = { ..point }`).
- **Bidirectional local inference.** Each expression is checked with the type expected where
  it stands, when there is one: literals, `none`, closures, records, lists, contextual
  variants and generic calls use it. A generic call binds its type parameters from the
  turbofish, else from the arguments that are not closures, then from the expected type, then
  from the closures, whose parameter types come from what is known by then. A parameter still
  open is an error (`C0014`), never a guess. Once the type arguments are final, every argument
  is checked again against its parameter's final type, so an argument that widened a type
  argument cannot launder an earlier one (`apply(id, web)` with `id: fn(Str) -> Str` and
  `web: Untrusted<Str>` is refused), and binding a variable to a type that contains it fails
  (occurs check). `let` and `var` without an annotation take their
  value's type. There is no global unification.
- **Names.** Locals (block scopes, shadowing warned as `shadowed_binding`), then the module's
  items and imports, then the prelude: the standard modules, `control`'s functions, the
  primitive types and the shared types of chapter 06. `x.f(a)` calls `f` from the module of
  `x`'s type (`text`, `list`, `map`, `json`, `math`), else a function `f` in scope whose first
  parameter accepts `x`; `x |> f(a)` is `f(x, a)`.
- **Contextual variants and tools** (rule 10): where the expected type is an enum, a bare name
  that is not a local and names one of its unit variants is that variant; where it is a
  `ToolName` or `ToolSet`, a bare name is a tool. A string literal where a `ModelRef` is
  expected builds `Named { id }`, and where a `ToolName` is expected names a tool pattern.
  `ContextMode`'s variant `none` is a keyword, so the prelude cannot declare it: the checker
  adds it. Where a `ContextMode` is expected, including the optional field
  `context?: ContextMode` (`with { context: none }`), `none` is that variant; where a
  `ContextMode?` is expected (`let c: ContextMode? = none`), it is the absent value, and a
  pattern names the variant as `ContextMode.none`.
- **Variant literals** (rule 3): `Name { … }` resolves to the expected enum's variant, else to
  the one enum in scope (the module's, then the prelude's) that has it; several is `C0008`.
- **Patterns.** In `match`, `catch`, `if let` and `while let` arms a bare name that is a variant
  of the matched enum is a variant pattern (rule 6). A pattern other than a binding, `_` or
  `none` matched against `T?` matches a present value. `if let p = e` with `e: T?` matches `p`
  against `T`. `let`, `for` and closure parameters need irrefutable patterns (`C0010`).
- **Exhaustiveness** of `match` (guards do not count): Maranget's usefulness algorithm with
  witnesses, so the error names a missing case (`Denied { by: rule, .. }`, `none`, `[_]`).
  It is exponential in the worst case, so it is bounded: every row it builds costs fuel from
  both a budget of 200,000 units per match and the check's own budget, its recursion depth is
  bounded, and a match over the bound is refused (`C0023`) rather than accepted unchecked;
  after one refusal the remaining matches are not checked (the package is refused already). Rows are grouped by their first
  constructor, so a match of thousands of arms over an enum of thousands of variants stays
  linear.
- **Constants** (chapter 02, "Constants"; chapter 04, "Where configuration is evaluated"). A
  `const` initialiser, a `state` initialiser and a harness's declared configuration may call
  only pure functions and pure built-ins (`C0016`). A `fn` is pure unless it calls or names an
  effectful built-in, a harness or an impure function: purity spreads backwards over the call
  graph. Effects are counted where a function value is made or named (a closure's body is
  checked in place), so calling a function value adds nothing and `twice(|x| x * 2, 1)` is a
  constant expression. Constants that reach themselves through reads and calls are `C0017`
  (Tarjan's algorithm). An unannotated constant is typed on first use.
- **Prompts** are `TrustedPrompt` when no section is `data`, `image` or `context`, no section
  holds text that may be untrusted (untrusted text, a `Brief`, `Json`, a type parameter) and
  no included prompt is untrusted; else `Prompt`. A string literal interpolating such a value
  is `Untrusted<Str>`.
- **`Untrusted<T>`** is mapped by structure when the type is built (chapter 03's table: a
  record of untrusted fields, `Untrusted<Int>` is `Int`, a unit-only enum is itself), so field
  access and subtyping work through it.

## Stage 5: schemas and taint

Taint is part of the type relation, so most sinks are trusted positions the relation already
guards; stage 5 makes them explicit, adds the conversions and the analyses, and lives in this
crate rather than a second one because its rules change which programs type (the sections,
`trust`'s inference, interpolation, secrets) and report through the same diagnostics. Chapter 03
("Taint", "Secrecy", "Types are schemas"), chapter 05 (sections) and chapter 07:

- **Sinks.** The `system`, `instruct`, `note` and `example` sections and `steps` take trusted
  text only (`system` also a `TrustedPrompt`); a `task` section takes a `Brief` (and so `Str`
  and `UserText`). Every other sink is a parameter or field of trusted type: `model.complete`'s
  text, `session.request_approval`'s summary, `context.notice`'s text, the names given to
  `meta.harness` and `agents.spawn`, the `because` of the conversions, a harness's or call's
  `system: TrustedPrompt`. A value that would fit but for its trust labels is `C0035`, one that
  would fit but for a secrecy label `C0037`; any other misfit stays `C0004`. Classification
  never accepts anything the relation refuses.
- **`Brief` and `TrustedPrompt` inference.** A string literal is as trusted as the least
  trusted value it interpolates: untrusted text (or a type parameter, `Json`, an image) gives
  `Untrusted<Str>`, a `Brief` with only trusted text otherwise a `Brief`, else `Str`
  (concatenation already joins the same way). A prompt is a `TrustedPrompt` when it has no
  `data`, `image` or `context` section, no `Brief` task and no included untrusted prompt; a
  section expression is checked expecting `Str`, so a generic call in it is instantiated at the
  most trusted type.
- **Abstract generics.** A type parameter counts as possibly untrusted (stage 4's taint flag),
  so `"{x}"` and `text.show(x)` over a `T` are `Untrusted<Str>`; `Untrusted<T>` stays abstract.
- **Conversions.** `trust(x, because:)`, `as_task(x, because:)` and `declassify(x, because:)`
  are audited trust sites. `trust`'s `T` is the turbofish's, else `x`'s type with its trust
  labels removed (`Untrusted<Verdict>` gives `Verdict`), never what inference from the expected
  type would bind. Each converts only at a monomorphic type, and may only be called directly:
  a type parameter in `T`, or naming one as a value, is `C0036`.
- **Built-ins as values.** Every rule a built-in's attributes impose holds for a function value
  too. Those enforced only where it is called (`@audited`, `@loads`, `@secret_answers`, and
  `@secrecy_sink` on a generic built-in) make the built-in callable only: naming it as a value
  is `C0036` (so `let load: fn(Str) -> H = meta.harness` cannot load a harness no list names,
  and `session.ask` cannot lose `secret: true`). The others hold for values: effects, tools and
  purity count where a value is named, `@schema(T)` is checked at the type a value is named
  with, and a non-generic secrecy sink refuses secrets by its parameter types.
  `tests/taint.rs` enumerates the table and checks each annotated built-in is one or the other.
  A generic value whose expected type does not fit its instantiation is refused where it is
  named (`C0004`): stage 4 closed its open parameters to the error type first, which let
  `let d: fn(Json) -> Str = json.decode` through as a laundering of `Json` into `Str`. Every call is recorded
  (`Checked::trust_sites`: kind, position, enclosing item, type, literal reason).
- **Transitive trust sites and the summary.** The call graph has an edge from an item to every
  declaration its source names (as for purity, effects count where a function value is made or
  named; calling a harness *value* counts as every kind of effect). Over its strongly connected
  components, callees first, each item gets the trust sites and `meta.harness` loads it
  reaches and the kinds of effect (write, command, network, delegate, any tool of the frame,
  model, other) and tools it may use (`Checked::summary`). The site sets are bounded
  (`MAX_SITE_WORK` entries); past that every item lists every site, a superset, and
  `Checked::approximate` says so.
- **Static `Secret<T>`.** Only where the label is known: `session.ask`'s answers are
  `[Secret<UserText>]` unless the questions are a list written out whose every question is a
  record written out without `secret: true`; a declared `Secret<T>` parameter; `declassify`'s
  argument. A result computed from a statically secret argument is secret (interpolation and
  calls; not `()`, prompts, or `declassify`), and branching on a `Secret<Bool>` is allowed.
  `Secret<T>` relates only to `Secret<U>`, so a secret reaches no unlabelled position (`C0037`):
  the built-ins marked `@secrecy_sink` refuse one even for a generic parameter, and a generic
  function that **may reach a secrecy sink** cannot be instantiated at a type holding a secret,
  whether called, named at an expected function type, given by a turbofish or returned (its
  body cannot see the label). "May reach a secrecy sink" is a summary bit (`Effects::SINK`)
  propagated like the effects: set by `@secrecy_sink` built-ins and, failing closed, by every
  effectful built-in not marked `@local` (a model may be hosted; an effect of unknown kind may
  send its arguments anywhere) and by calling a harness value. Everything else is the run-time
  labels' job (stage 6).
- **Guarded-effect lint** (`C0039`, a warning; [`src/lint.rs`](src/lint.rs) states the rules):
  an effect that writes, runs a command, delegates, reaches the network or runs a model-chosen
  tool, every one of whose guards in its item is derived from model or tool output only.
  Derivation over-approximates (values of untrusted type, effect results other than the
  user's answers, locals assigned under derived guards, closure parameters, caught errors); a
  trusted conjunct (`if v.pass and approved`) is a guard of its own. Each item is walked until
  its derived locals settle, at most `MAX_ROUNDS` times, then every local counts as derived.
  `@allow(model_guard)` on the item silences it (statements take no attributes). `declassify`
  is `@user` (the person approves it), so its result is as derived as its argument: the
  person approves removing the label, not where the value came from.
- **Schemas and digests** ([`src/schema.rs`](src/schema.rs) documents the canonical forms).
  `schema(t)` is the JSON Schema (draft 2020-12) of chapter 03's table, with the labels dropped
  and the record declarations' descriptions (`"…"` or `///`) and refinements (`@len`,
  `@range`, `@pattern`, `@example`, checked against their fields: `C0038`) added, written as
  canonical JSON (fixed key order, no whitespace); an enum that contains itself goes under
  `$defs`, by its name or the first `Name_2`, `Name_3`, … still free. A `@pattern` must be in
  the subset of regular-expression syntax both JSON Schema validators (ECMAScript) and `re`
  read (no look-around, back-references or named groups), else `C0038`; `@range` keeps the
  decimal literals as written. `digest(t)` is `sha256` (written out in [`src/sha256.rs`](src/sha256.rs), with the
  standard's vectors) over a canonical form that keeps the labels (`Str` and `Untrusted<Str>`
  differ) and leaves out descriptions and refinements: it keys the structure of values (state,
  caches), which a changed description or `@len` does not change. Records are structural, so equal
  records share the metadata of the first declaration lowered. A `state` type, a `format`
  type and the type arguments marked `@schema(T)` (`json.decode`, `json.encode`,
  `model.generate_as`) must be schema-able (`C0038`).
- **`harnesses.check`** ([`src/report.rs`](src/report.rs)). `Checked::harness_check(&loaded)`
  summarises a package: effects and tools ("any tool of the frame" when it runs tools a model
  chooses), every trust site with its position, the `meta.harness` names it loads, and per
  harness its parameter and result schemas and whether a model may run it (`pub`, not an
  entry, every parameter schema-able and free of trusted text; otherwise "not runnable by
  models" with the reason). `harnesses_check(&registry, &builtin, &name)` loads and checks
  through a registry and follows literal `meta.harness` names to list the sites of everything
  the package can load, at most `MAX_LOADED_PACKAGES` packages. A name is a package name, `.`
  and the harness, and package names hold dots, so it follows the run-time rule: the longest
  prefix that names a registered package wins (`package_readings` lists the candidates). A
  computed name, a name no reading resolves, or one that two readings resolve makes it
  `complete: false`. `test` blocks against a scripted host need the interpreter.

What stage 6 must carry at run time: the trust sites (kind, reason, site) to emit `ratio_trust`
events with the value's digest; secrecy labels on every value, since the checker's static
`Secret<T>` sees only what it can prove; the type digests for `state` and caches; and the
schemas for structured generation and decoding.

## Architecture: interned types and span-keyed side tables

- **Types are interned** in one table (`Types`): a type is a copyable `TypeId`, equal types
  have equal ids, and the constructors normalise. Each node records its depth and whether it
  holds inference variables, type parameters or possibly-untrusted text, so those questions
  cost nothing. Enum declarations and type parameters live in the same table.
- **The tree is not copied.** The parser's tree has no node ids, and every expression of a
  cleanly parsed module has its own span (a parent's span strictly contains its children's), so
  the checker leaves **side tables keyed by span** (`Key { start, end }`, per module):
  `types` (every expression; a shorthand field `{ x }` under its name's span), `res` (what a
  name, member, variant or `is` test resolved to), `calls` (callee, inferred type arguments,
  and which argument, receiver or default fills each parameter), `coercions`, `patterns`
  (bindings and variant tests) and `locals` (every binding, by `LocalId`, with its type and
  mutability), plus each item's type.
- **Why**: stage 5 needs every expression's type (with its untrusted structure) at the
  positions it checks, and stage 6 lowers the same tree to IR, needing resolutions, local slots,
  instantiations and argument order at each call site. Both walk the parser's tree and look
  nodes up, so neither depends on a second tree shape, and site ids in stage 6 can be derived
  from the same spans.

## Built-in signatures

The shared types (`Turn`, `ModelRef`, `GenOpts`, `Error`, `ErrorKind`, `Result<T>`,
`HarnessConfig`, …) are declared in Ratio in [`src/std/prelude.ratio`](src/std/prelude.ratio);
shapes the specification does not spell out (`Site`, `PermissionRequest`) are marked
provisional there. Built-in functions are a declarative table in
[`src/builtins.rs`](src/builtins.rs): one Ratio source per standard module, `fn` declarations
with empty bodies, `@pure` where chapter 06's Needs column is `pure`. A declaration without
`@pure` counts as effectful (of an unknown kind), so one added carelessly fails closed. Stage 5's
attributes say the rest: `@audited(trust | as_task | declassify)`, `@effect(write | command |
network | delegate | any_tool | model)`, `@tool(name)`, `@secrecy_sink`, `@schema(T)`, `@user`
(the result is the person's answer), `@secret_answers`, `@loads` and `@local` (its effects keep
its arguments on the machine: not a secrecy sink). A name that is a Ratio
keyword is written with a trailing `_` (`meta.harness_`, `agents.spawn`'s `harness_`), which
the checker drops. The type-parameter names `S`
(any text, normalised to `Str` or `Untrusted<Str>`) and `Num` (`Int`, `Float`, `Duration`,
`Money`) carry chapter 06's bounds.

**Scope cut**: the table is deliberately partial. It holds what the conformance programs use
(`control`, `list`, `text`, `map`, `json`, `math`, `model`, `tools`, `session`, `context`,
`meta`, `agents`), a few dozen of chapter 06's 478 built-ins; stages 7 to 12 extend it module by module. A standard name it lacks is
reported with its own code, `C0034` ("no signature yet"), so it is never confused with a name
that does not exist (`C0001`).

## Bounds

The checker runs on model-written source, so no input makes it panic, overflow its stack or
work without bound:

| Bound | Value | When exceeded |
| --- | --- | --- |
| Type depth (also bounds generic instantiation depth) | 128 | `C0022` |
| Recursion through expressions, patterns and types | 512 (12 on the caller's stack) | `C0022` |
| Aliases, constants or prompts resolved on demand inside each other | 64 | `C0022` |
| Work on types (relations, substitutions, normalisations) per check | 20,000,000 units | `C0022` |
| Exhaustiveness work per `match` (or irrefutable pattern), also charged to the check's budget | 200,000 units | `C0023` |
| Trust-site and load set entries over the call graph | 4,000,000 | Every item lists every site (`approximate`) |
| Rounds of the guarded-effect lint per item | 8 | Every local of the item counts as derived |
| Schema or canonical form size, nesting; work per query | 1 MiB, 256 levels; 2,000,000 units | `SchemaError::TooLarge` (`C0038` where one is required) |
| Packages `harnesses_check` loads through `meta.harness` | 32 | `complete: false` |

Out of budget, relations fail closed (they do not hold), and the check reports `C0022` even
when the budget ran out after the last expression.

Relations between types without inference variables are memoised, substitution and the
`Untrusted` mapping are memoised per node, record fields are found by binary search, and enum
variants and parameters by name through maps, so the work is linear in the program, not in the
size its types would have written out (`tests/check.rs` checks types of 2^100 leaves, 2,500
fields, variants, arms and statements, and 300-long chains). The checker runs on a thread with
a 64 MiB stack, as the parser does; if none can be started it runs on the caller's stack and
refuses nesting beyond 12.

## Diagnostic codes

| Code | Diagnostic |
| --- | --- |
| `C0001` | A name that is no local, item, import or prelude name; a type or module used as a value |
| `C0002` | A type name that resolves to no type |
| `C0003` | Type arguments that do not fit a type (wrong number, or some on a type that takes none) |
| `C0004` | A value whose type is not a subtype of the type expected where it stands |
| `C0005` | No such field, module member or method |
| `C0006` | A record literal or pattern whose fields do not match the record type (missing, extra or repeated) |
| `C0007` | An unknown variant, or a variant literal whose fields do not match its payload |
| `C0008` | A bare variant name that several enums in scope define |
| `C0009` | A `match` that is not exhaustive (the message names a missing case) |
| `C0010` | A refutable pattern where an irrefutable one is needed (`let`, `for`, parameters) |
| `C0011` | A pattern that cannot match a value of its type |
| `C0012` | Arguments that do not fit the parameters (too many, too few, unknown or repeated names) |
| `C0013` | A call of something that is not a function or harness |
| `C0014` | A type parameter or closure parameter whose type cannot be inferred |
| `C0015` | Type arguments that do not fit a function's type parameters or their bounds |
| `C0016` | An effect in a constant expression (`const`, `state`, harness configuration) |
| `C0017` | A constant whose value depends on itself |
| `C0018` | A type alias that refers to itself |
| `C0019` | An assignment to something that is not a `var`, or to a `var` a closure captured |
| `C0020` | Branches or elements with no common supertype |
| `C0021` | `return`, `break` or `continue` outside a function or loop, or `break` with a value outside `loop` |
| `C0022` | A type deeper than the limit, nesting or on-demand chains over the limit, or the work budget spent |
| `C0023` | A `match` too large to check for exhaustiveness within the bound |
| `C0024` | A harness without a return type |
| `C0025` | A call override (`with { … }`) on a call whose callee is not a harness |
| `C0026` | An operator applied to operands it does not take |
| `C0027` | A map key type that is not `Str`, `Untrusted<Str>`, `Int`, `Bool` or a unit-only enum |
| `C0028` | A tuple with more than eight elements |
| `C0029` | A member of another module that is not `pub` |
| `C0030` | A name declared twice (variant, field, parameter, type parameter) or bound twice in a pattern |
| `C0031` | Warning: a `let` that shadows an earlier binding (lint `shadowed_binding`) |
| `C0032` | Warning: a local binding that shadows a standard module |
| `C0033` | Type parameters on an enum a program declares (only functions and type aliases are generic) |
| `C0034` | A standard built-in the checker has no signature for yet (stages 7 to 12 add them) |
| `C0035` | Untrusted text, a `Brief` or an untrusted prompt where trusted text is required: an instruction position, a `task` section (a `Brief` only), any trusted type |
| `C0036` | `trust`, `as_task` or `declassify` at a type that is or holds a type parameter, or not called directly |
| `C0037` | A statically secret value where an unlabelled one is required, at a secrecy sink, or for a generic parameter of a function that may send it on |
| `C0038` | A type that must be schema-able and is not, or a schema attribute that does not fit its field |
| `C0039` | Warning: a model-derived condition is the only guard on a write, command, delegation, network effect or model-chosen tool (lint `model_guard`) |

## Tests

- `tests/conformance.rs` runs `conformance/types/` and `conformance/taint/` (one refused
  program per sink, one accepted per allowed flow, trust sites, secrets, schemas, the lint): each `.ratio` file is a package of one
  file, each folder a package whose `main.ratio` imports the others by path. The entry's header
  says `check-ok`, or lists `check-error CODE at LINE:COL [in FILE]` and `check-warning …`
  lines, all of the checker's diagnostics in any order. Every code has a file.
- `tests/taint.rs`: schema and digest snapshots (fixed bytes, so the same on every platform),
  types without schemas, a doubling type refused quickly, trust sites listed transitively,
  the package summary, `harnesses_check` following loads through a registry (and incomplete for
  a computed or missing name), and the lint and analysis on hostile bodies.
- `tests/taint_property.rs`: a fixed-seed generator of well-typed programs mixing untrusted
  sources, joins, generics, closures, `var`s and the conversions, with ground truth computed by
  data provenance; accepted programs never put untrusted text in an instruction position, a
  `task` section or a `TrustedPrompt`, safe programs of the exactly typed fragment are
  accepted, and coverage is asserted.
- `tests/check.rs` covers the side tables (types, resolutions, call instantiations, coercions)
  and hostile input: deep expressions, doubling aliases, long alias, constant and
  instantiation chains, wide declarations and bodies, an exponential match, and the
  caller's-stack fallback on a 512 KiB thread.

Stages 4 and 5 are each larger than chapter 10's 800-line guideline: the checker is one cohesive pass
over the whole expression language, and taint is part of its relation and its diagnostics, so
splitting either would leave a stage that checks part of it.
