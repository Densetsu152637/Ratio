# ratio-syntax

Ratio's syntax: the token reader (stage 1 of the [implementation plan](../../spec/10-implementation.md)),
and the parser from stage 2. Hosts do not depend on this crate; they use the
[`ratio` facade](../ratio/README.md), which re-exports what they need.

Ported from Imperium pull request 314, where stage 1 first landed as `crates/imperium-ratio`; the
behaviour is unchanged.

## Stage 1: the token reader

`ratio_syntax::lex(source)` returns tokens with spans (byte range plus 1-based line and column
at both ends) and diagnostics. It implements chapter 02's lexical structure:

- comments and `///` documentation comments (kept as tokens; `////` starts one too), identifiers,
  the 36 reserved words, the contextual keywords in their one position each, and members after
  `.`, `?.` and `::`;
- integers (decimal, hex, `_` separators; leading zeros allowed, so `007` is 7), quantities
  (`20k`, `1.5M`), floats, durations (`500ms`, `30s`, `5m`, `2h`; whole numbers, stored and
  range-checked in microseconds) and money (`$0.50`, exact to a millionth);
- strings with a mode stack: short, long (dedented), raw and raw long strings, escapes, nested
  interpolation with format specs;
- semicolon insertion at line ends by the three conditions of chapter 02, using the kind of the
  innermost `{` (block, record, prompt body, enum body, match arms) that the reader works out from
  the keyword before it and rule 1 of chapter 02 (including a variant literal after a type name);
  a line holding only an attribute (`@turn`, `@allow(…)`) never ends a statement;
- the limits of chapter 07's table that apply while reading: source size (`LexOptions`, 256 KiB,
  64 KiB for an authored harness, never more than `MAX_SOURCE_BYTES`), interpolation nesting (4,
  at most 8) and bracket nesting (128, at most 256; reading stops at the first nesting error);
- recovery: errors never panic, the stream always ends in `Eof`, strings and interpolations stay
  balanced, and at most 100 diagnostics are reported (`max_diagnostics`, never more than
  `MAX_DIAGNOSTICS`); when there are more, a final `L0015` marks where the next one was found and
  reading stops. Chapter 10 lists the lexical codes, `L0001` to `L0016` (`Code`).

## Tests

- `tests/conformance.rs` runs the corpus in `conformance/` at the repository root, in chapter 10's
  format (`// expect:` lines). It understands `lex-ok`, `lex-error CODE at LINE:COL`, `tokens:`
  (the expected tokens in `TokenKind::render` notation) and `option:` (a `LexOptions` field), and
  skips files whose expectations belong to later stages (`ok`, `value`, `check-error`,
  `runtime-error`, `fixture`). Every diagnostic code must have a `lex-error` file, and an unknown
  header directive (a misspelt `// token:`) fails its file.
- `tests/properties.rs` reads fixed-seed random inputs, mutated conformance programs and hostile
  shapes: the reader never panics, is deterministic, and its spans round-trip (ordered, in
  bounds, on character boundaries, correct line and column, only trivia between tokens in a clean
  read, a number or punctuation token read alone from its slice is the same token), and an
  inserted `;` appears only inside a block, a prompt body or at the top level, after a token that
  can end a statement. Hostile shapes must each read within a generous time bound. The crate has
  no dependencies, so the generator is a small xorshift rather than `proptest`.
- `tests/lexer.rs` covers single behaviours the corpus does not express well.
- `conformance/builtins.txt` is a stub: chapter 10 generates it from chapter 06, and the
  generator and its drift check land with stage 7.
