# ratio-check

Ratio's type checker: stage 4 of the [implementation plan](../../spec/10-implementation.md).
Hosts do not depend on this crate; they use the [`ratio` facade](../ratio/README.md), which
re-exports it as `ratio::checker` and its entry point as `ratio::check`.

## What it does

`check(&loaded)` takes a package loaded by [`ratio-names`](../ratio-names/README.md) (stage 3)
and checks every module that parsed cleanly. It returns `Checked`: the diagnostics (the `C`
codes below) and, per module, the side tables later stages read.

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

Taint proper is stage 5: sinks, where `Brief` and `TrustedPrompt` may go, `trust` and
`as_task`, schemas and type digests, and static `Secret<T>` (which this stage only parses and
relates covariantly).

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
`@pure` counts as effectful, so one added carelessly fails closed. The type-parameter names `S`
(any text, normalised to `Str` or `Untrusted<Str>`) and `Num` (`Int`, `Float`, `Duration`,
`Money`) carry chapter 06's bounds.

**Scope cut**: the table is deliberately partial. It holds what the conformance programs use
(`control`, `list`, `text`, `map`, `json`, `math`, `model`, `tools`), a few dozen of chapter
06's 478 built-ins; stages 7 to 12 extend it module by module. A standard name it lacks is
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

## Tests

- `tests/conformance.rs` runs `conformance/types/`: each `.ratio` file is a package of one
  file, each folder a package whose `main.ratio` imports the others by path. The entry's header
  says `check-ok`, or lists `check-error CODE at LINE:COL [in FILE]` and `check-warning …`
  lines, all of the checker's diagnostics in any order. Every code has a file.
- `tests/check.rs` covers the side tables (types, resolutions, call instantiations, coercions)
  and hostile input: deep expressions, doubling aliases, long alias, constant and
  instantiation chains, wide declarations and bodies, an exponential match, and the
  caller's-stack fallback on a 512 KiB thread.

This stage is larger than chapter 10's 800-line guideline: the checker is one cohesive pass
over the whole expression language, and splitting it would leave a stage that checks part of
it.
