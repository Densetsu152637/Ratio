# ratio-ir

The Ratio Core IR: the first half of stage 6 of the
[implementation plan](../../spec/10-implementation.md) (chapter 01, decision R2). Hosts do not
depend on this crate; the [`ratio` facade](../ratio/README.md) re-exports it as `ratio::ir`, with
`ratio::lower` and `ratio::IR_VERSION`.

## What it does

`lower(&loaded, &checked)` turns a cleanly checked load into a `Program`: every module, its
items (functions, harnesses, prompts, constants, `state`), and the enums and types the nodes
refer to. Only a clean check lowers (`LowerError::NotClean`); a node the checker's side tables
do not describe is an internal error, never a guess. The walk runs on a large stack with its own
depth bound, as the checker does.

The IR is a tree of expressions in which:

- every name is resolved: locals to per-function slots (captures first named by the closure),
  items to `ItemId`s, built-ins by the name a program writes (`list.push`);
- every effect is an explicit `ExprKind::Effect` node (a call of a built-in without `@pure` in
  the checker's table: `trust`, `model.generate`, `tools.*`, …);
- every loop is an explicit loop node (`For`, `While`, `WhileLet`, `Loop`, `Repeat`);
- the sugar is gone: `|>` and method calls are calls with their arguments in written order and
  an explicit parameter order (`ArgFrom`), interpolation is `Concat` of `Show` parts (with the
  format spec), `?.` is `OptChain`, record literals that build maps are `Map`, model names are
  `ModelRef.Named` variants.

`par`, `race`, `with timeout`, `with` and call overrides stay explicit nodes: their branch and
site ids are what effect keys will name (stage 11). A harness's declared configuration is
lowered in the harness's own frame, after its parameters, since it is evaluated with them
bound (chapter 04). Every program holds the prelude enums the interpreter builds values of
(`RUNTIME_ENUMS`) and those `meta`'s frame readers build (`FRAME_ENUMS`, stage 10); the
canonical form names only the enums a package refers to, so neither changes an IR hash.

## The canonical form and the hash

`canon::canonical_text(&program, package)` is the package's canonical serialisation: UTF-8
S-expressions, one item per line, modules in path order, items in source order. Every
cross-reference is written by what it names (`(item "user:app" "main.ratio" "score")`,
`(builtin "list.push")`); types and enums by numbers local to the package, defined once in a
trailing `defs` section in order of first reference; floats as their 16 hexadecimal IEEE 754
digits. Nothing depends on load order, the platform, pointer width or hash-map order, and source
positions are left out, so layout and comments do not change the hash.

Writing the form numbers the package's expressions in pre-order (`Expr::id`): with the package
hash, an expression's number is its **site id**. `Package::hash` is `sha256` over the form, which
starts with `ratio-ir <IR_VERSION>` and `stdlib <STDLIB_VERSION>`; built-in prompt texts join it
in stage 12. The interpreter's prepared form (`ratio-eval`) is built from the IR and never
hashed, so `IR_VERSION` does not cover it.

`tests/hash.rs` holds a hash snapshot that CI checks on Linux, Windows and macOS, and checks that
layout does not change the hash, that meaning does, and that every expression has its own site.
