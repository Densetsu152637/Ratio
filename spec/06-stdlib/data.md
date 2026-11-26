# `data`: values and text

The data modules are pure unless a row says otherwise: they cost interpreter steps, by the cost
class below, and nothing else. They are in scope
by their short names (`text.trim(s)`, `list.map(xs, f)`), and every function whose first
parameter is a value of the module's type can be called as a method (`s.trim()`, `xs.map(f)`).

**Taint in signatures.** `S` stands for `Str` or `Untrusted<Str>`: a function given untrusted
text returns untrusted text (chapter 03), and a function returning only scalars returns trusted
scalars. `T`, `K`, `V` are type parameters.

**Needs** is `pure` for every row except where the Fails column also says `effect`. `Num` stands
for any of `Int`, `Float`, `Duration` and `Money` where the operation is meaningful (the checker
rejects, say, `Money * Money`).

| Submodule | Covers | Built-ins |
| --- | --- | --- |
| [`text`](#text) | Strings | 45 |
| [`re`](#re) | Regular expressions | 9 |
| [`json`](#json) | JSON, schemas, pointers, patches | 14 |
| [`list`](#list) | Lists | 47 |
| [`map`](#map) | Ordered maps | 15 |
| [`math`](#math) | Numbers and statistics | 27 |
| [`rand`](#rand) | Seeded, recorded randomness | 7 |
| [`time`](#time) | Clock, durations, dates | 12 |
| [`hash`](#hash) | Digests | 4 |
| [`diff`](#diff) | Diffs and patches | 5 |
| [`template`](#template) | Logic-less templates | 2 |
| [`vec`](#vec) | Embedding vectors | 7 |
| [`path`](#path) | Path strings | 8 |
| [`url`](#url) | URLs | 5 |
| [`codec`](#codec) | Base64, hex, CSV | 6 |

## Cost classes and representations

Each pure built-in has a **cost class** and is charged `1 + ⌈w / 64⌉` steps, where `w` is the
class's work over the size n of its inputs (characters of text, elements of lists, entries of
maps, nodes of JSON) and the size m of its output. Callbacks charge their own steps as any code
does.

| Class | Work `w` | Built-ins |
| --- | --- | --- |
| `const` | 1 | `text.len`, `text.is_empty`, `list.len`, `list.is_empty`, `list.first`, `list.last`, `map.len`, `map.is_empty`, `json.type_of`; every `math` built-in on scalars (`abs`, `sign`, `min`, `max`, `clamp`, `floor`, `ceil`, `round`, `trunc`, `to_float`, `sqrt`, `pow`, `exp`, `ln`, `log2`, `log10`, `is_nan`); `time.add`, `time.diff`, `time.date`, `time.duration`, `time.seconds`, `time.show` |
| `log` | ⌈log2(n + 1)⌉ | `list.push`, `list.get`, `list.insert`, `list.remove_at`, `list.slice`, `list.take`, `list.drop`, `list.concat`, `map.get`, `map.has`, `map.set`, `map.remove`, `map.update` |
| `nlogn` | n · ⌈log2(n + 1)⌉ | `list.sort`, `list.sort_by`, `list.unique`, `list.unique_by`, `list.group_by`, `map.from`, `map.count_by`, `math.median`, `math.percentile`, `math.mode`, `vec.top_k` |
| `product` | n₁ · n₂, the sizes of its two inputs: lines for `diff.lines` and `diff.unified`, words for `diff.words`, characters for `text.similarity`, nodes for `json.diff` | `text.similarity`, `diff.lines`, `diff.words`, `diff.unified`, `json.diff` |
| `output` | m | `list.range`, `list.repeat`, `text.repeat`, `text.pad_start`, `text.pad_end` |
| `linear` | n + m | Every other pure built-in of this page (for example `list.map`, `list.filter`, `text.split`, `text.contains`, `text.replace`, `json.parse`, `hash.stable`, `template.render`) |

**Charged before running.** The part of a charge that follows from the arguments (the class's
work over the input sizes, and for `output` built-ins the output size the arguments fix) is debited
before the built-in runs; when it exceeds the steps left, the built-in raises
`Budget { which: steps }` without running, so a `product` built-in over large inputs never runs
unpaid. A `linear` built-in's output part is debited when it returns.

Text concatenation and string interpolation are charged `linear` in the result; build long text
with `text.join` or a prompt rather than by repeated concatenation.

**Charges never depend on representation state.** An implementation may update a uniquely owned
value in place, but uniqueness depends on how long sibling branches hold references, which is
timing; charges depend only on sizes, so they are deterministic (chapter 07).

**Representations.** The charges rest on these, which every implementation must provide, with its
work per built-in within a constant factor of the built-in's class:

- lists are persistent vectors with O(log n) push, get, set, slice and concatenation (a relaxed
  radix-balanced tree);
- maps are persistent ordered maps with O(log n) get, set and remove and in-order iteration;
- records have a fixed layout with O(1) field access (their types are static);
- `Str` is immutable UTF-8 that caches its character count and an all-ASCII flag, so `text.len`
  is O(1) and character-indexed slicing of ASCII text needs no scan;
- every container keeps a **label summary**: a bit set whenever any leaf inside it may carry a
  secrecy label, maintained on construction. The bit may be set when no leaf is labelled (after the
  labelled part was sliced away, say) but is never clear when one is, so a clear bit lets the sink
  guard and the `sees` check skip the value in O(1), and a set bit falls back to scanning the
  leaves. Debug builds also scan at every sink and assert that the scan agrees with the bit. Leaf
  labels (chapter 03) keep their meaning; the summary is representation only.

## `text`

Lengths and indexes count Unicode scalar values. Case mapping is Unicode's default, locale-free.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `text.len` | `(s: S) -> Int` | Number of characters | — |
| `text.is_empty` | `(s: S) -> Bool` | `len == 0` | — |
| `text.lower` | `(s: S) -> S` | Lower case | — |
| `text.upper` | `(s: S) -> S` | Upper case | — |
| `text.trim` | `(s: S) -> S` | Removes leading and trailing whitespace | — |
| `text.trim_start` | `(s: S) -> S` | Removes leading whitespace | — |
| `text.trim_end` | `(s: S) -> S` | Removes trailing whitespace | — |
| `text.contains` | `(s: S, part: S) -> Bool` | Substring test | — |
| `text.starts_with` | `(s: S, prefix: S) -> Bool` | Prefix test | — |
| `text.ends_with` | `(s: S, suffix: S) -> Bool` | Suffix test | — |
| `text.find` | `(s: S, part: S, from: Int = 0) -> Int?` | Index of the first occurrence at or after `from` | — |
| `text.rfind` | `(s: S, part: S) -> Int?` | Index of the last occurrence | — |
| `text.count` | `(s: S, part: S) -> Int` | Non-overlapping occurrences | `Invalid` (empty `part`) |
| `text.slice` | `(s: S, start: Int, end: Int? = none) -> S` | Characters `start..end`; negative indexes count from the end; clamped to the string | — |
| `text.truncate` | `(s: S, max: Int, marker: Str = "…") -> S` | At most `max` characters including the marker, cut at a character boundary | `Invalid` (`max` < marker length) |
| `text.split` | `(s: S, sep: S, limit: Int? = none) -> [S]` | Splits on every `sep` (at most `limit` parts) | `Invalid` (empty `sep`) |
| `text.split_once` | `(s: S, sep: S) -> (S, S)?` | Splits at the first `sep` | — |
| `text.lines` | `(s: S) -> [S]` | Splits on `\n` and `\r\n`; no trailing empty line | — |
| `text.words` | `(s: S) -> [S]` | Splits on Unicode whitespace, dropping empties | — |
| `text.chars` | `(s: S) -> [S]` | One string per character | — |
| `text.join` | `(parts: [S], sep: S = "") -> S` | Joins with a separator | — |
| `text.replace` | `(s: S, from: S, to: S) -> S` | Replaces every occurrence | `Invalid` (empty `from`) |
| `text.replace_first` | `(s: S, from: S, to: S) -> S` | Replaces the first occurrence | `Invalid` (empty `from`) |
| `text.repeat` | `(s: S, n: Int) -> S` | `n` copies (result at most 16 MiB) | `Invalid` |
| `text.reverse` | `(s: S) -> S` | Characters in reverse order | — |
| `text.pad_start` | `(s: S, width: Int, fill: Str = " ") -> S` | Left pad to `width` characters | — |
| `text.pad_end` | `(s: S, width: Int, fill: Str = " ") -> S` | Right pad to `width` characters | — |
| `text.before` | `(s: S, sep: S) -> S` | Text before the first `sep`, or all of `s` | — |
| `text.after` | `(s: S, sep: S) -> S` | Text after the first `sep`, or empty | — |
| `text.before_last` | `(s: S, sep: S) -> S` | Text before the last `sep`, or all of `s` | — |
| `text.after_last` | `(s: S, sep: S) -> S` | Text after the last `sep`, or empty | — |
| `text.strip_prefix` | `(s: S, prefix: S) -> S?` | `s` without the prefix, or `none` | — |
| `text.strip_suffix` | `(s: S, suffix: S) -> S?` | `s` without the suffix, or `none` | — |
| `text.trim_end_matches` | `(s: S, suffix: S) -> S` | `s` with every trailing repetition of `suffix` removed (`"1.00."` without `"."` is `"1.00"`) | `Invalid` (empty `suffix`) |
| `text.dedent` | `(s: S) -> S` | Removes the common leading whitespace of non-blank lines | — |
| `text.indent` | `(s: S, prefix: Str = "  ") -> S` | Prefixes every non-blank line | — |
| `text.wrap` | `(s: S, width: Int = 80) -> S` | Re-flows paragraphs to `width`, never splitting words | — |
| `text.number_lines` | `(s: S, start: Int = 1) -> S` | Prefixes each line with its number and a tab, as `read_file` shows files | — |
| `text.show` | `<T>(v: T) -> S` | A human-readable rendering: text as is, scalars in literal form, records and lists as compact JSON; untrusted if `v` holds untrusted text | — |
| `text.parse_int` | `(s: S) -> Int?` | A decimal integer, surrounding whitespace allowed | — |
| `text.parse_float` | `(s: S) -> Float?` | A decimal or exponent float | — |
| `text.similarity` | `(a: S, b: S) -> Float` | 1 minus normalised Levenshtein distance, 0..1 | — |
| `text.extract_json` | `(s: S) -> Json?` | The first complete JSON object or array in the text (code fences tolerated); used by repair loops | — |
| `text.code_blocks` | `(s: S, lang: Str? = none) -> [CodeBlock]` | Fenced code blocks in Markdown, `{ lang?, code: S }`, optionally only one language | — |
| `text.quote` | `(s: S, label: Str = "data") -> Untrusted<Str>` | The text framed and defused exactly as a `data` section would render it, for building framed text by hand; the result is always untrusted | — |

## `re`

Rust `regex` syntax, which runs in time linear in the input, so no pattern can hang the
interpreter. Patterns are at most 4 KiB and are compiled once per call site.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `re.is_match` | `(pattern: Str, s: S) -> Bool` | Whether the pattern matches anywhere | `Invalid` (bad pattern) |
| `re.find` | `(pattern: Str, s: S) -> Match?` | The first match, `{ text: S, start, end }` | `Invalid` |
| `re.find_all` | `(pattern: Str, s: S, limit: Int = 1000) -> [Match]` | Non-overlapping matches in order | `Invalid` |
| `re.captures` | `(pattern: Str, s: S) -> Captures?` | Groups of the first match: `{ groups: [S?], named: Map<Str, S> }` | `Invalid` |
| `re.captures_all` | `(pattern: Str, s: S, limit: Int = 1000) -> [Captures]` | Groups of every match | `Invalid` |
| `re.replace` | `(pattern: Str, s: S, with: S) -> S` | Replaces the first match; `$1` and `${name}` refer to groups | `Invalid` |
| `re.replace_all` | `(pattern: Str, s: S, with: S) -> S` | Replaces every match | `Invalid` |
| `re.split` | `(pattern: Str, s: S) -> [S]` | Splits on matches | `Invalid` |
| `re.escape` | `(s: Str) -> Str` | The text as a literal pattern | — |

## `json`

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `json.parse` | `(s: S) -> Json` | Parses JSON text (at most 16 MiB, nesting at most 128) | `Invalid` (with line and column) |
| `json.stringify` | `<T>(v: T, pretty: Bool = false) -> S` | Encodes any schema-able value (keys in declared or key order); untrusted if `v` holds untrusted text | `Invalid` (handles, functions) |
| `json.decode` | `<T>(j: Json) -> Untrusted<T>` | Validates against `T`'s schema and converts | `SchemaMismatch` (every violation) |
| `json.encode` | `<T>(v: T) -> Json` | A schema-able value as `Json`; secrecy labels stay on the leaves (chapter 03) | `Invalid` |
| `json.schema` | `<T>() -> Json` | `T`'s derived JSON Schema (chapter 03) | — |
| `json.validate` | `(j: Json, schema: Json) -> [Violation]` | Validates against any JSON Schema (draft 2020-12, no remote references); `{ pointer, message }` per violation | `Invalid` (bad schema) |
| `json.get` | `(j: Json, pointer: Str) -> Json?` | RFC 6901 pointer lookup | `Invalid` (bad pointer) |
| `json.set` | `(j: Json, pointer: Str, v: Json) -> Json` | A copy with the value at the pointer replaced or added | `Invalid` |
| `json.keys` | `(j: Json) -> [Untrusted<Str>]` | An object's keys in document order; empty for non-objects. Keys come from the data and are untrusted | — |
| `json.type_of` | `(j: Json) -> JsonType` | `null`, `bool`, `number`, `string`, `array`, `object` | — |
| `json.merge` | `(a: Json, b: Json) -> Json` | RFC 7396 merge patch of `b` onto `a` | — |
| `json.patch` | `(j: Json, ops: Json) -> Json` | Applies an RFC 6902 patch | `Invalid` (failed `test`, bad path) |
| `json.diff` | `(a: Json, b: Json) -> Json` | An RFC 6902 patch turning `a` into `b` | — |
| `json.equal` | `(a: Json, b: Json) -> Bool` | Semantic equality (object key order ignored, numbers compared by value) | — |

## `list`

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `list.len` | `<T>(xs: [T]) -> Int` | Length | — |
| `list.is_empty` | `<T>(xs: [T]) -> Bool` | `len == 0` | — |
| `list.get` | `<T>(xs: [T], i: Int) -> T?` | Element `i` (negative counts from the end) | — |
| `list.first` | `<T>(xs: [T]) -> T?` | First element | — |
| `list.last` | `<T>(xs: [T]) -> T?` | Last element | — |
| `list.push` | `<T>(xs: [T], x: T) -> [T]` | A new list with `x` appended | — |
| `list.insert` | `<T>(xs: [T], i: Int, x: T) -> [T]` | A new list with `x` at `i` | `Invalid` (out of range) |
| `list.remove_at` | `<T>(xs: [T], i: Int) -> [T]` | A new list without element `i` | `Invalid` |
| `list.concat` | `<T>(xs: [T], ys: [T]) -> [T]` | `xs` then `ys` (also `xs + ys`) | — |
| `list.flatten` | `<T>(xss: [[T]]) -> [T]` | One level of nesting removed | — |
| `list.range` | `(start: Int, end: Int, step: Int = 1) -> [Int]` | `start..end` by `step` (at most 1,000,000 elements) | `Invalid` (`step` 0) |
| `list.repeat` | `<T>(x: T, n: Int) -> [T]` | `n` copies | `Invalid` |
| `list.map` | `<T, U>(xs: [T], f: fn(T) -> U) -> [U]` | Applies `f` in order (sequentially; see `par_map` for concurrency) | Errors of `f` |
| `list.map_indexed` | `<T, U>(xs: [T], f: fn(Int, T) -> U) -> [U]` | With indexes | Errors of `f` |
| `list.filter` | `<T>(xs: [T], f: fn(T) -> Bool) -> [T]` | Elements for which `f` holds | Errors of `f` |
| `list.filter_map` | `<T, U>(xs: [T], f: fn(T) -> U?) -> [U]` | Maps and drops `none` | Errors of `f` |
| `list.flat_map` | `<T, U>(xs: [T], f: fn(T) -> [U]) -> [U]` | Maps and flattens | Errors of `f` |
| `list.fold` | `<T, A>(xs: [T], init: A, f: fn(A, T) -> A) -> A` | Left fold | Errors of `f` |
| `list.reduce` | `<T>(xs: [T], f: fn(T, T) -> T) -> T?` | Fold from the first element; `none` when empty | Errors of `f` |
| `list.find` | `<T>(xs: [T], f: fn(T) -> Bool) -> T?` | First element for which `f` holds | Errors of `f` |
| `list.find_index` | `<T>(xs: [T], f: fn(T) -> Bool) -> Int?` | Its index | Errors of `f` |
| `list.index_of` | `<T>(xs: [T], x: T) -> Int?` | Index of the first element equal to `x` | — |
| `list.contains` | `<T>(xs: [T], x: T) -> Bool` | Membership (also `x in xs`) | — |
| `list.any` | `<T>(xs: [T], f: fn(T) -> Bool) -> Bool` | Whether `f` holds for some element (stops early) | Errors of `f` |
| `list.all` | `<T>(xs: [T], f: fn(T) -> Bool) -> Bool` | Whether it holds for all (stops early) | Errors of `f` |
| `list.count` | `<T>(xs: [T], f: fn(T) -> Bool) -> Int` | How many satisfy `f` | Errors of `f` |
| `list.sum` | `(xs: [Num]) -> Num` | Sum of `Int`, `Float`, `Duration` or `Money` | `Invalid` (overflow) |
| `list.min` | `<T>(xs: [T]) -> T?` | Smallest by natural order | — |
| `list.max` | `<T>(xs: [T]) -> T?` | Largest by natural order | — |
| `list.min_by` | `<T, K>(xs: [T], key: fn(T) -> K) -> T?` | Smallest by key (first on ties) | Errors of `key` |
| `list.max_by` | `<T, K>(xs: [T], key: fn(T) -> K) -> T?` | Largest by key (first on ties) | Errors of `key` |
| `list.sort` | `<T>(xs: [T]) -> [T]` | Stable sort by natural order (`NaN` last) | — |
| `list.sort_by` | `<T, K>(xs: [T], key: fn(T) -> K, descending: Bool = false) -> [T]` | Stable sort by key | Errors of `key` |
| `list.reverse` | `<T>(xs: [T]) -> [T]` | Reversed | — |
| `list.unique` | `<T>(xs: [T]) -> [T]` | First occurrence of each value, in order | — |
| `list.unique_by` | `<T, K>(xs: [T], key: fn(T) -> K) -> [T]` | First occurrence of each key | Errors of `key` |
| `list.group_by` | `<T, K>(xs: [T], key: fn(T) -> K) -> Map<K, [T]>` | Groups by key, order kept within groups | Errors of `key` |
| `list.partition` | `<T>(xs: [T], f: fn(T) -> Bool) -> ([T], [T])` | Those that satisfy `f`, and the rest | Errors of `f` |
| `list.chunk` | `<T>(xs: [T], size: Int) -> [[T]]` | Consecutive groups of `size` (last may be shorter) | `Invalid` (`size` < 1) |
| `list.window` | `<T>(xs: [T], size: Int) -> [[T]]` | Sliding windows | `Invalid` |
| `list.zip` | `<A, B>(xs: [A], ys: [B]) -> [(A, B)]` | Pairs up to the shorter length | — |
| `list.unzip` | `<A, B>(ps: [(A, B)]) -> ([A], [B])` | Splits pairs | — |
| `list.enumerate` | `<T>(xs: [T]) -> [(Int, T)]` | With indexes | — |
| `list.take` | `<T>(xs: [T], n: Int) -> [T]` | First `n` | — |
| `list.drop` | `<T>(xs: [T], n: Int) -> [T]` | All but the first `n` | — |
| `list.take_while` | `<T>(xs: [T], f: fn(T) -> Bool) -> [T]` | Longest prefix satisfying `f` | Errors of `f` |
| `list.slice` | `<T>(xs: [T], start: Int, end: Int? = none) -> [T]` | Elements `start..end`, negative from the end, clamped | — |

## `map`

Keys are `Str`, `Int`, `Bool` or unit-only enums; iteration is in key order. An empty record
literal `{}` is an empty map where a map is expected.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `map.from` | `<K, V>(pairs: [(K, V)]) -> Map<K, V>` | Builds a map; later pairs win | — |
| `map.len` | `<K, V>(m: Map<K, V>) -> Int` | Number of keys | — |
| `map.is_empty` | `<K, V>(m: Map<K, V>) -> Bool` | No keys | — |
| `map.get` | `<K, V>(m: Map<K, V>, k: K) -> V?` | Value for a key (also `m[k]`, which raises `NotFound`) | — |
| `map.has` | `<K, V>(m: Map<K, V>, k: K) -> Bool` | Key present (also `k in m`) | — |
| `map.set` | `<K, V>(m: Map<K, V>, k: K, v: V) -> Map<K, V>` | A copy with the key set | — |
| `map.remove` | `<K, V>(m: Map<K, V>, k: K) -> Map<K, V>` | A copy without the key | — |
| `map.update` | `<K, V>(m: Map<K, V>, k: K, f: fn(V) -> V, default: V) -> Map<K, V>` | Applies `f` to the value, or to `default` when absent | Errors of `f` |
| `map.keys` | `<K, V>(m: Map<K, V>) -> [K]` | Keys in order | — |
| `map.values` | `<K, V>(m: Map<K, V>) -> [V]` | Values in key order | — |
| `map.entries` | `<K, V>(m: Map<K, V>) -> [(K, V)]` | Pairs in key order | — |
| `map.merge` | `<K, V>(a: Map<K, V>, b: Map<K, V>) -> Map<K, V>` | Union; `b` wins on shared keys | — |
| `map.map_values` | `<K, V, W>(m: Map<K, V>, f: fn(V) -> W) -> Map<K, W>` | Transforms values | Errors of `f` |
| `map.filter` | `<K, V>(m: Map<K, V>, f: fn(K, V) -> Bool) -> Map<K, V>` | Keeps entries for which `f` holds | Errors of `f` |
| `map.count_by` | `<T, K>(xs: [T], key: fn(T) -> K) -> Map<K, Int>` | How many elements per key | Errors of `key` |

## `math`

IEEE 754 binary64 throughout, no fused operations, so results are identical on every machine.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `math.abs` | `(x: Num) -> Num` | Absolute value | `Invalid` (`Int` minimum) |
| `math.sign` | `(x: Num) -> Int` | -1, 0 or 1 | — |
| `math.min` | `(a: Num, b: Num) -> Num` | Smaller | — |
| `math.max` | `(a: Num, b: Num) -> Num` | Larger | — |
| `math.clamp` | `(x: Num, lo: Num, hi: Num) -> Num` | `x` limited to `lo..=hi` | `Invalid` (`lo` > `hi`) |
| `math.floor` | `(x: Float) -> Int` | Rounds down | `Invalid` (out of range, `NaN`) |
| `math.ceil` | `(x: Float) -> Int` | Rounds up | `Invalid` |
| `math.round` | `(x: Float, digits: Int = 0) -> Float` | Half away from zero, to `digits` decimals | — |
| `math.trunc` | `(x: Float) -> Int` | Toward zero | `Invalid` |
| `math.to_float` | `(x: Int) -> Float` | Exact conversion where representable, else nearest | — |
| `math.sqrt` | `(x: Float) -> Float` | Square root (`NaN` for negatives) | — |
| `math.pow` | `(x: Num, y: Num) -> Num` | Power; `Int` to a non-negative `Int` stays `Int` | `Invalid` (overflow) |
| `math.exp` | `(x: Float) -> Float` | e to the `x` | — |
| `math.ln` | `(x: Float) -> Float` | Natural logarithm | — |
| `math.log2` | `(x: Float) -> Float` | Base-2 logarithm | — |
| `math.log10` | `(x: Float) -> Float` | Base-10 logarithm | — |
| `math.is_nan` | `(x: Float) -> Bool` | Whether `x` is `NaN` | — |
| `math.mean` | `(xs: [Num]) -> Float?` | Arithmetic mean | — |
| `math.median` | `(xs: [Num]) -> Float?` | Median (mean of the middle two for even lengths) | — |
| `math.mode` | `<T>(xs: [T]) -> T?` | Most frequent value, first on ties | — |
| `math.variance` | `(xs: [Num], sample: Bool = true) -> Float?` | Variance | — |
| `math.stdev` | `(xs: [Num], sample: Bool = true) -> Float?` | Standard deviation | — |
| `math.percentile` | `(xs: [Num], p: Float) -> Float?` | Linear-interpolated percentile, `p` in 0..100 | `Invalid` (`p` out of range) |
| `math.softmax` | `(xs: [Float], temperature: Float = 1.0) -> [Float]` | Softmax, computed stably | `Invalid` (temperature ≤ 0) |
| `math.argmax` | `(xs: [Num]) -> Int?` | Index of the largest (first on ties) | — |
| `math.argmin` | `(xs: [Num]) -> Int?` | Index of the smallest (first on ties) | — |
| `math.entropy` | `(ps: [Float]) -> Float` | Shannon entropy in bits of a distribution (normalised first) | `Invalid` (negative) |

## `rand`

Every `rand` built-in is an **effect**: it draws from its branch's own generator (ChaCha20, seeded
with `H(turn seed, frame path, branch path)`, chapter 01). Draws are a function of the turn's
recorded seed and the drawing branch's position alone, so they need no tape entries, a replayed run
draws the same values, and two live runs with the same model answers draw the same values whatever
the order in which concurrent branches run. Sequential code has one branch path and is unaffected.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `rand.int` | `(lo: Int, hi: Int) -> Int` | Uniform in `lo..hi` (excluding `hi`) | `Invalid` (empty range); `effect` |
| `rand.float` | `() -> Float` | Uniform in `[0, 1)` | `effect` |
| `rand.bool` | `(p: Float = 0.5) -> Bool` | `true` with probability `p` | `Invalid`; `effect` |
| `rand.choice` | `<T>(xs: [T]) -> T` | One element | `Invalid` (empty); `effect` |
| `rand.sample` | `<T>(xs: [T], k: Int) -> [T]` | `k` distinct elements, in random order | `Invalid` (`k` > len); `effect` |
| `rand.shuffle` | `<T>(xs: [T]) -> [T]` | A random permutation | `effect` |
| `rand.uuid` | `() -> Str` | A version 4 UUID from the generator | `effect` |

## `time`

`Instant` is a UTC timestamp in microseconds. Reading the clock is an **effect** (recorded).

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `time.now` | `() -> Instant` | The current time | `effect` |
| `time.since` | `(t: Instant) -> Duration` | Time elapsed since `t` | `effect` |
| `time.sleep` | `(d: Duration) -> ()` | Waits; counts against `wall`; returns at once in replay | `effect` |
| `time.today` | `(tz: Str = "local") -> Date` | Today's date in a time zone (IANA name or `local`) | `Invalid` (zone); `effect` |
| `time.add` | `(t: Instant, d: Duration) -> Instant` | `t + d` | `Invalid` (overflow) |
| `time.diff` | `(a: Instant, b: Instant) -> Duration` | `a - b` (may be negative) | — |
| `time.format` | `(t: Instant, fmt: Str = "%Y-%m-%dT%H:%M:%SZ", tz: Str = "UTC") -> Str` | `strftime`-style formatting | `Invalid` |
| `time.parse` | `(s: S, fmt: Str = "%Y-%m-%dT%H:%M:%S%z") -> Instant?` | Parses a timestamp | `Invalid` (bad format) |
| `time.date` | `(t: Instant, tz: Str = "UTC") -> Date` | `{ year, month, day, hour, minute, second, weekday }` | `Invalid` |
| `time.duration` | `(seconds: Float) -> Duration` | A duration from seconds | `Invalid` (negative, overflow) |
| `time.seconds` | `(d: Duration) -> Float` | A duration in seconds | — |
| `time.show` | `(d: Duration) -> Str` | `1h 4m 3s` style | — |

## `hash`

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `hash.sha256` | `(s: S) -> Str` | Lower-case hex SHA-256 of the UTF-8 bytes | — |
| `hash.blake3` | `(s: S) -> Str` | Lower-case hex BLAKE3 | — |
| `hash.stable` | `<T>(v: T) -> Str` | `sha256:` over the value's canonical JSON (sorted keys, no spaces); equal values hash equal across runs and machines | `Invalid` (handles, functions) |
| `hash.short` | `<T>(v: T, n: Int = 8) -> Str` | The first `n` hex digits of `stable` | — |

## `diff`

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `diff.lines` | `(a: S, b: S) -> Diff` | A line diff (Myers), `[{ op: equal \| insert \| delete, text: S }]` | — |
| `diff.words` | `(a: S, b: S) -> Diff` | A word diff | — |
| `diff.unified` | `(a: S, b: S, context: Int = 3, from: Str = "a", to: Str = "b") -> S` | A unified diff | — |
| `diff.apply` | `(text: S, patch: S) -> S` | Applies a unified diff exactly (no fuzz); untrusted if either is | `Invalid` (hunk does not apply, with the hunk) |
| `diff.stats` | `(d: Diff) -> DiffStats` | `{ added, removed, unchanged }` lines | — |

## `template`

Logic-less templates (a Mustache subset: `{{name}}`, `{{a.b}}`, `{{#list}}…{{/list}}`,
`{{^empty}}…{{/empty}}`), for text that must be filled from data, not for prompts (use prompt
sections). No partials, no lambdas, no HTML escaping.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `template.render` | `<T>(tpl: Str, vars: T) -> S` | Fills the template from a record or map; untrusted if any filled value is | `Invalid` (syntax, missing name) |
| `template.names` | `(tpl: Str) -> [Str]` | The names the template refers to | `Invalid` |

## `vec`

`Vector` is `[Float]`. Mismatched dimensions raise `Invalid`.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `vec.dot` | `(a: Vector, b: Vector) -> Float` | Dot product | `Invalid` |
| `vec.norm` | `(a: Vector) -> Float` | Euclidean length | — |
| `vec.normalize` | `(a: Vector) -> Vector` | Unit length | `Invalid` (zero vector) |
| `vec.cosine` | `(a: Vector, b: Vector) -> Float` | Cosine similarity | `Invalid` |
| `vec.add` | `(a: Vector, b: Vector) -> Vector` | Element-wise sum | `Invalid` |
| `vec.mean` | `(vs: [Vector]) -> Vector?` | Element-wise mean | `Invalid` |
| `vec.top_k` | `(query: Vector, vs: [Vector], k: Int = 5) -> [(Int, Float)]` | Indexes and cosine scores of the `k` nearest, best first (stable on ties) | `Invalid` |

## `path`

Lexical operations on path strings. They never touch the file system; the engine resolves paths.
Both `/` and `\` are separators on input; output uses `/`.

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `path.join` | `(parts: [S]) -> S` | Joins with `/`, collapsing repeated separators | — |
| `path.parent` | `(p: S) -> S?` | The path without its last component | — |
| `path.name` | `(p: S) -> S` | The last component | — |
| `path.stem` | `(p: S) -> S` | The last component without its extension | — |
| `path.ext` | `(p: S) -> S?` | The extension without the dot | — |
| `path.normalize` | `(p: S) -> S` | Removes `.` and resolves `..` lexically | — |
| `path.is_absolute` | `(p: S) -> Bool` | Starts with `/`, a drive letter or a UNC prefix | — |
| `path.relative_to` | `(p: S, base: S) -> S?` | `p` relative to `base`, lexically; `none` when not under it | — |

## `url`

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `url.parse` | `(s: S) -> Url?` | `{ scheme, host, port?, path, query: Map<S, S>, fragment? }` (query keys come from the URL, so they carry its trust), parsed as the WHATWG parser does (the one `web_fetch` uses) | — |
| `url.host` | `(s: S) -> S?` | The host, lower case, ASCII form | — |
| `url.with_query` | `(s: S, params: Map<Str, S>) -> S` | Adds or replaces query parameters, encoded | `Invalid` (not a URL) |
| `url.encode` | `(s: S) -> S` | Percent-encodes a component | — |
| `url.decode` | `(s: S) -> S?` | Decodes a component; `none` for invalid UTF-8 | — |

## `codec`

| Built-in | Signature | Semantics | Fails with |
| --- | --- | --- | --- |
| `codec.base64_encode` | `(s: S) -> S` | Standard Base64 of the UTF-8 bytes | — |
| `codec.base64_decode` | `(s: S) -> S?` | Decodes Base64 to UTF-8 text; `none` when not valid or not UTF-8 | — |
| `codec.hex_encode` | `(s: S) -> S` | Lower-case hex of the UTF-8 bytes | — |
| `codec.hex_decode` | `(s: S) -> S?` | Decodes hex to UTF-8 text | — |
| `codec.csv_parse` | `(s: S, header: Bool = true, delimiter: Str = ",") -> [Map<S, S>]` | RFC 4180 CSV into rows (keys from the header, which carry the text's trust; else `c1`, `c2`, …) | `Invalid` (ragged rows, with the line) |
| `codec.csv_write` | `(rows: [Map<Str, S>], delimiter: Str = ",") -> S` | Rows to CSV, header from the first row's keys | `Invalid` (missing keys) |
