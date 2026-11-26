# ratio (package `ratio-lang`)

The public facade of Ratio, the programming language for agent harnesses. A host such as
Imperium depends on this crate alone and reaches the language through the `ratio` library; the
crates behind it are implementation details.

Today it re-exports the token reader (stage 1) and the parser (stage 2) of
[`ratio-syntax`](../ratio-syntax/README.md), and modules and names (stage 3) of
[`ratio-names`](../ratio-names/README.md) as `ratio::names`: a host implements `ratio::Registry`
over its package sources and loads a package with `ratio::Loader`. It also names the
specification draft (`SPEC_DRAFT`) and the stage reached (`STAGE`, 3). The `Host` trait,
the checker and the interpreter join it in later stages; chapter 10 of the
[specification](../../spec/10-implementation.md#the-public-api) defines the API and its versions.

`tests/spec.rs` keeps the implementation and the specification beside it in step: `SPEC_DRAFT`
must match `spec/README.md`, and every conformance directory must name, in
`conformance/spec-map.txt`, chapters and headings that exist.
