# ratio (package `ratio-lang`)

The public facade of Ratio, the programming language for agent harnesses. A host such as
Imperium depends on this crate alone and reaches the language through the `ratio` library; the
crates behind it are implementation details.

Today it re-exports the token reader (stage 1) and the parser (stage 2) of
[`ratio-syntax`](../ratio-syntax/README.md), modules and names (stage 3) of
[`ratio-names`](../ratio-names/README.md) as `ratio::names`, and the type checker (stage 4) of
[`ratio-check`](../ratio-check/README.md) as `ratio::checker`. A host implements
`ratio::Registry` over its package sources, loads a package with `ratio::Loader`, and checks it
with `ratio::check(&loaded)`, which returns `ratio::Checked`: the `C` diagnostics and the side
tables (types, resolutions, call instantiations) the later stages read. Stage 5, schemas and
taint, is part of that check (its sinks, trust sites, static secrets and guarded-effect lint
report through the same diagnostics), and `ratio::harnesses_check(&registry, &builtin, &name)`
is chapter 08's `harnesses.check` as a library call: the package's summary (writes, commands,
network, delegation, tools), every trust site it can reach through imports and `meta.harness`
loads, and which harnesses a model may run. `Checked::schema` and `Checked::digest` give a
type's JSON Schema and type digest. Stage 6 adds the Core IR of
[`ratio-ir`](../ratio-ir/README.md) as `ratio::ir` (`ratio::lower`, the IR hash of each package,
`ratio::IR_VERSION`) and the core interpreter of [`ratio-eval`](../ratio-eval/README.md) as
`ratio::interp` (`ratio::prepare`, `ratio::evaluate`, `ratio::Value` with its secrecy `Labels`,
`ratio::Limits`). Stage 7 adds the `text` and `re` built-ins and the offload pool
(`ratio::interp::Pool`, `Evaluation::offload_to`). It also names the specification draft
(`SPEC_DRAFT`), the stage reached (`STAGE`, 7) and its own version (`VERSION`, which
`ratio --version` prints). The `Host` trait and the rest of the standard library
join it in later stages; chapter 10 of the
[specification](../../spec/10-implementation.md#the-public-api) defines the API and its versions.

`tests/spec.rs` keeps the implementation and the specification beside it in step: `SPEC_DRAFT`
must match `spec/README.md`; every conformance directory must name, in
`conformance/spec-map.txt`, chapters and headings that exist; `conformance/builtins.txt` must
equal what it generates from chapter 06's built-in tables (name, signature, Needs, and the cost
class of each pure `data` built-in); and each implemented `text` and `re` built-in must have its
chapter 06 class (the `re` built-ins are charged `product`, a proposed erratum). After changing
chapter 06, regenerate the file with:

```sh
RATIO_BLESS=1 cargo test -p ratio-lang --test spec builtins
```
