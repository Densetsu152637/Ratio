# 03 Semantics

## Values

| Kind | Types | Notes |
| --- | --- | --- |
| Unit | `()` | The value of statements and of blocks ending in one |
| Scalars | `Bool`, `Int`, `Float`, `Duration`, `Money` | `Int` is 64-bit signed, overflow raises `Invalid`; `Duration` is whole microseconds (a literal is a whole number of its unit, converted to microseconds and range-checked there, chapter 02); `Money` is exact to 1e-6 USD |
| Text | `Str`, `UserText`, `Brief`, `Untrusted<Str>` | Unicode scalar values; indexes and lengths count characters, not bytes. `UserText` and `Brief` are text with a known origin (below) |
| Images | `Image` | `{ mime, width, height }` plus bytes, opaque to programs; from attachments and `tools.browser_screenshot`; always untrusted |
| Options | `T?` | `none` or a `T`; `T??` is `T?` |
| Lists | `[T]` | Persistent vectors; indexing out of range raises `Invalid` (use `list.get` for `T?`) |
| Tuples | `(A, B, …)` | Fixed arity, at most 8 |
| Records | `{ a: A, b?: B }` | Structural; a field declared with `?` may be absent, and reads as `B?` |
| Maps | `Map<K, V>` | Ordered by key; persistent, with O(log n) lookup, update and removal (chapter 06, `data`); `K` is `Str`, `Untrusted<Str>`, `Int`, `Bool` or a unit-only enum. Where a `Map<Str, V>` is expected, a record literal (`{ attempt: "2" }`) builds one |
| Enums | `enum E { A, B { x: Int } }` | Nominal tagged unions; variant payloads are records |
| Functions | `fn(A, B) -> C` | Named functions and closures; harnesses are functions of a special kind (chapter 04) |
| Prompts | `Prompt`, `TrustedPrompt` | Built from sections (chapter 05); `TrustedPrompt` holds trusted text only |
| Dynamic | `Json` | Any JSON value; must be decoded through a schema before typed use |
| Handles | `Conversation`, `Task<T>`, `Process`, `Scratchpad`, `Mark`, `Pin`, `Artefact`, `Checkpoint` | References to things the engine owns; see below |

All values are **immutable**. "Updating" a record makes a new one (`{ ..r, x: 1 }`); `list.push`
returns a new list. Persistent structures make this cheap; chapter 06 (`data`) gives the complexities they must have.

**Handles** are the only references. A handle names something the engine owns and keeps (a
conversation, a subagent task, a background process, a stored artefact). Operations on a handle
are effects. A handle cannot be put in `state` or a cache, encoded to JSON, or returned from a
harness to a caller in a different frame, except `Task<T>` and `Artefact` (which the engine tracks
per session) and `Conversation`, which crosses frames only as a `Transcript` snapshot value.

## Types

### Decision R5: static types with local inference and one dynamic escape

Every binding, parameter, field and return has a type known before the program runs. Function
and harness signatures are written out; local `let` and `var` bindings and closure parameters are
inferred (bidirectional checking: the expected type flows into closures and literals). Generic
type parameters are allowed on functions and type aliases; harnesses are monomorphic, because a
harness signature is a contract recorded in the registry and used to derive schemas. Enums a
program declares are not generic; only built-in enums (`Result<T>`, `Fold<A>`, chapter 06) may
be generic.

Records are structural: two record types with the same fields are the same type. A record value
must have exactly the declared fields (no width subtyping), and a required field may stand
where the same field is declared optional; spread against a declared record type takes only the
fields it declares, so it builds narrower records explicitly. Enums are nominal.

**Subtyping** is deliberately small:

- `T` is a subtype of `T?`.
- `Never`, the type of `raise`, `return`, `break` and `continue`, is a subtype of every type, so
  `else { raise … }` fits any branch type.
- `T` is a subtype of `Untrusted<T>` (trusted data may always be treated as untrusted).
- `UserText` is a subtype of `Str`; `Str` is a subtype of `Brief`; `Brief` is a subtype of
  `Untrusted<Str>`.
- `TrustedPrompt` is a subtype of `Prompt`.
- Lists, options, tuples, maps (in values) and records are covariant in their element types.
  This is sound because values are immutable. An enum's type arguments relate by how its
  payloads use each parameter (`Result<T>` and `Fold<A>` are covariant).
- A function type is contravariant in parameters and covariant in result.

`Json` is the one dynamic type. It comes from `json.parse`, from `tools.call_tool` and MCP
results, and from `meta` introspection. It supports only JSON operations (`json.get`, `json.keys`,
indexing that returns `Json?`); it is turned into a typed value by `json.decode::<T>(j)`, which
validates against `T`'s schema and raises `SchemaMismatch` with every violation listed. So a
dynamic value never reaches typed code unchecked.

Rejected: gradual typing with an `any` type that flows silently into typed code. It would make
schema derivation partial and taint unsound. Rejected: full Hindley-Milner inference. Signatures
are documentation for harness authors and for the registry; inferring them would make errors
appear far from their cause.

### Types are schemas

Every type built only from scalars, text, options, lists, tuples, records, maps, enums and `Json`
is **schema-able**. Its JSON Schema (draft 2020-12) is derived mechanically and is what
structured generation constrains the model to (chapter 05).

| Ratio type | JSON Schema |
| --- | --- |
| `Bool`, `Int`, `Float`, `Str` (and every text type) | `boolean`, `integer`, `number`, `string` |
| `Duration` | `string`, pattern `^[0-9]+(ms\|s\|m\|h)$` |
| `Money` | `string`, pattern `^\$[0-9]+(\.[0-9]{1,6})?$` |
| `()` | `null` |
| `T?` (field), and a field `b?: T` | the field is not `required`, and has `T`'s schema (`null` is not a value of it) |
| `T?` (elsewhere) | `anyOf: [T, null]` |
| `[T]` | `array`, `items: T`; `@len(min, max)` on the field adds `minItems`/`maxItems` |
| `(A, B)` | `array`, `prefixItems: [A, B]`, `items: false` |
| `{ a: A }` | `object`, `properties`, `required`, `additionalProperties: false` |
| `Map<K, V>` | `object`, `additionalProperties: V`; a key type other than text adds `propertyNames` (`^-?[0-9]+$` for `Int`, `false` and `true` for `Bool`, the variant names in `snake_case` for a unit-only enum) |
| unit-only `enum` | `string`, `enum` of the variant names in `snake_case` |
| other `enum` | `oneOf` of objects with a `kind` constant (the variant's name in `snake_case`) and the payload's fields; an enum that contains itself is defined once under `$defs` and referred to by `$ref` |
| `Json` | `{}` |
| `Untrusted<T>`, `Secret<T>` | the schema of `T` (labels are not part of the generated schema) |

Field descriptions come from the trailing string or `///` comment. Attributes written before a
field refine the schema: `@len(min, max)` (whole numbers, on a list), `@range(min, max)` (number
literals, on a number, kept as the decimal literals written), `@pattern(r"…")` (on text) and
`@example(value)` (a literal); one that does not fit its field is a checker error. A
`@pattern` must be in the regular-expression syntax both JSON Schema validators (ECMAScript) and
`re` read: alternation, groups and `(?:…)`, classes, the quantifiers `*`, `+`, `?` and `{m,n}`,
anchors, and `\`-escapes of punctuation and `d`, `w`, `s`, `D`, `W`, `S`, `b`, `B`, `n`, `r`,
`t`; look-around, back-references and named groups are refused. The derived schema is written as
canonical JSON and is part of the IR hash: no whitespace; a schema object's keys in the fixed
order `$ref`, `type`, `const`, `enum`, `anyOf`, `oneOf`, `properties`, `required`,
`additionalProperties`, `propertyNames`, `prefixItems`, `items`, `pattern`, then a field's
`description`, `minItems`, `maxItems`, `minimum`, `maximum`, `pattern`, `examples`, and `$defs`
last at the root; `properties` and `required` in field-name order (by UTF-8 bytes); `enum`
values, `oneOf` cases and `$defs` in declaration and first-met order.

**Type digests.** Every schema-able type also has a **type digest**: `sha256` over its canonical
form *including* trust and secrecy labels (`Str` and `Untrusted<Str>` digest differently). Digests
key persisted state and caches (below), so they key the **structure** of values: the canonical
form leaves out field descriptions and refinements, which change the schema but not which values
the type has. The canonical form is `ratio-type-1;` followed by the type's form, then the
definition of each enum met, numbered from 0 in the order first met (depth first, left to
right):
`bool`, `int`, `float`, `duration`, `money`, `str`, `user_text`, `brief`, `json`, `unit`,
`untrusted(T)`, `secret(T)`, `opt(T)`, `list(T)`, `map(K,V)`, `tuple(A,B,…)`,
`record("a":A,"b"?:B)` (fields in name order by UTF-8 bytes, names as JSON strings), and `#n` for the n-th enum
(with its type arguments substituted), defined as `;#n="Name"{"v1","v2":record(…),…}` with the
variants in declaration order.

## Taint: `Untrusted<T>`

### Decision R6

Imperium's rule is that tool output, web content, file contents and other agents' answers are
**data, never instructions** (`docs/05-tools-and-isolation.md#built-in-tools`,
`docs/21-knowledge-agents.md#safety`). Today that rule is upheld by framing text in tags and
telling the model. Ratio makes it a type rule as well, so a harness author cannot accidentally put
a web page into a system prompt.

**The effect rule.** Every string inside a value an effect returns is untrusted, unless it is an
**engine-generated identifier** (task ids, process ids, tab ids, session ids, checkpoint ids,
artefact ids, call ids the engine assigned, enum-valued statuses). This covers the obvious cases
(model output, tool results, file contents, pages, MCP results, memory entries, reports and
answers) and the easy-to-miss ones: tool-call names and ids a model wrote, a subagent's title,
JSON object keys, CSV headers, URL query keys, MCP tool names, model card texts, labels a model
chose. The standard library's signatures follow the rule, and the conformance suite checks every
built-in's declared return type against it (chapter 10).

**Text with a known origin.**

| Type | Origin | May go |
| --- | --- | --- |
| `Str` | Written in the program, or converted with `trust` | Anywhere |
| `UserText` | The user's own message (`Turn.message`): typed by the user, or the configured starting prompt of an unattended session | Anywhere a `Str` may |
| `Brief` | A task given to this agent: its brief from the dispatching agent (`TaskInput.brief`), or text converted with `as_task(x, because:)`; trusted `Str` is a `Brief` too | `task` prompt sections, and anywhere untrusted text may; never instruction positions |
| `Untrusted<Str>` | Everything else from effects | Anywhere except instruction positions and `task` sections |

`Turn.payloads` holds text that arrived with a turn but was not written by the user: CI logs,
pull-request comments and issue bodies that automation prompts embed. It is untrusted (chapter
01); the user's own words are only `Turn.message`.

**`Untrusted<T>` is deep.** It is defined by structure:

| `T` | `Untrusted<T>` is |
| --- | --- |
| `Str`, `UserText`, `Brief` | the primitive untrusted text type |
| `Bool`, `Int`, `Float`, `Duration`, `Money`, `()` | `T` itself |
| unit-only enum `E` | `E` itself |
| `[A]`, `A?`, `(A, B)`, `Map<K, V>` | `[Untrusted<A>]`, `Untrusted<A>?`, … (map keys follow the same rule) |
| `{ a: A, b: B }` | `{ a: Untrusted<A>, b: Untrusted<B> }` |
| other enum | the same variants with untrusted payloads |
| `Json`, `Image` | itself, already untrusted by construction |
| `Untrusted<A>` | `Untrusted<A>` |
| a type parameter `T` | `Untrusted<T>`, kept abstract until instantiated |

So `Untrusted<Verdict>` where `Verdict = { pass: Bool, reasons: [Str] }` lets the program branch
on `v.pass` directly, while `v.reasons` stays untrusted text.

**Scalars are trusted.** A number, a boolean or a choice from a closed set cannot carry an
instruction to a model. A model can still influence which branch the program takes; see "what the
guarantee covers" below.

**`Json` counts as untrusted.** `json.decode::<T>` on any `Json` returns `Untrusted<T>`.

**Propagation.** Operations on untrusted text give untrusted text: concatenation, interpolation,
`text.*`, `re.*` captures, `json.stringify`, template rendering. Operations that return scalars
return trusted scalars (`text.len`, `re.is_match`, `text.contains`). Concatenating or
interpolating a `Brief` with trusted text only gives a `Brief` (a task the harness words around
the task it was given), so `task "Fix this: {t.brief}"` is a `task` section.

**Generic code is checked abstractly.** Inside a generic function, a value of type parameter `T`
counts as possibly untrusted: `text.show`, interpolation, `json.stringify` and `template.render`
over a `T` (or anything containing one) give `Untrusted<Str>`. A generic function can therefore
never launder untrusted text into a `Str`, whatever it is instantiated with.

**Sinks.** Untrusted text, including `Brief`, may not reach **instruction positions**:

- the `system`, `instruct`, `note`, `example` and `steps` sections of a prompt, and therefore
  any `TrustedPrompt` (chapter 05);
- the `system` field of a harness or call configuration (type `TrustedPrompt`);
- `model.complete`'s text;
- the `summary` of `session.request_approval` and the text of `context.notice`;
- harness and package names given to `meta.harness`, `agents.spawn(package: …)` and imports;
- the `because` of `trust`, `as_task` and `declassify`.

`task` sections accept `Brief` (and its subtypes `Str` and `UserText`) only. Everywhere else
untrusted text is accepted: tool arguments (authority comes from permissions, not from the text),
`data` sections (where it is framed), replies to the user, logs, memory writes (which the user
approves by default), `state` fields, and harness arguments whose parameter type is untrusted.

**Conversion.**

- `trust(x: Untrusted<T>, because: Str) -> T` converts to trusted. `as_task(x: Untrusted<Str>,
  because: Str) -> Brief` converts to task text only (for example, a step of a plan the harness's
  own planner wrote).
- Both are allowed only at **monomorphic** types: `T` may not be or contain a type parameter, so
  a generic helper cannot become a general-purpose launderer. Unless a turbofish gives it,
  `trust`'s `T` is `x`'s type with its trust labels removed (`Untrusted<Verdict>` gives
  `Verdict`), never what the expected type would bind.
- `trust`, `as_task`, `declassify`, `session.ask` and `meta.harness` may only be called, never
  used as function values (chapter 06, "Naming and argument conventions").
- Both are effects that emit a `ratio_trust` event: package, version, call site, kind (`trust` or
  `as_task`), reason, and the value's `hash.stable` digest. The journal records the digest, never
  an excerpt of the text.
- `harnesses.check` lists every trust site of a package **transitively**, through its imports and
  every package it can load with `meta.harness`.
- `ratio.trust` (chapter 08) is `allow` (sites run), `ask` (selecting a package whose transitive
  trust-site list is not empty asks the user to approve that list, pinned to the package hash) or
  `deny` (such packages cannot be selected). The default is `ask`. `std` packages are reviewed
  with the crate and are exempt from `ask`. Authored packages may contain
  no trust sites at all (chapter 07).

**What the guarantee covers.** The taint rules track **explicit flows**: a value that is computed
from untrusted text is untrusted. They do not track **implicit flows** through control flow:
`if model_says_yes { tools.write_file(p, "fixed") }` writes trusted text chosen by an untrusted
branch. That is inherent in letting programs branch on model output, which is their purpose. The
checker's **guarded-effect lint** (`C0039`) warns when a condition derived from an untrusted
value (a model-derived scalar) is the only guard on an effect that writes, runs a command,
delegates, reaches the network or runs a tool a model chose (`tools.call_tool`,
`tools.dispatch`, `model.loop`), so a reviewer sees each such decision; the lint is silenced with
`@allow(model_guard)` on the enclosing item (statements take no attributes, chapter 02).

Rejected: dynamic taint tracking at run time only. It finds the mistake after a turn has spent
tokens, and cannot be listed for review before a package is enabled. Rejected: no taint, framing
only. Framing is still applied (chapter 05), but a type error at load time is cheaper than a
successful injection.

## Secrecy: `Secret<T>`

Taint protects integrity: what may *instruct* a model. It does nothing for confidentiality: an
injected instruction can still make a model put what it read into a web query. Secrecy is a
second, independent label for values that must not leave the machine or be stored outside it.

**A run-time label.** Whether a value is secret usually depends on data the checker cannot see
(which path a `read_file` received, which memory a search returned). Secrecy is therefore a
**label carried on values at run time**: every text and scalar leaf of a value has a secrecy bit,
set by sources and propagated by every operation. The interpreter passes values to the host as
`Labelled<T>`, and the host and the engine's dispatch enforce the sinks below from the labels, so
the enforcement does not depend on the checker. The static type `Secret<T>` is used only where the
label is known at check time (a `session.ask` question declared `secret: true`, the argument of
`declassify`); there, and only there, the checker also refuses a sink at load time. It also
refuses (`C0037`) instantiating a generic function at a type holding a `Secret` when the callee
may reach a secrecy sink (any effect not kept on the machine), whether it is called, named as a
function value, given by a turbofish or returned: inside the callee the type parameter carries no label,
so the sink there could not be refused. Everywhere else a sink violation is a run-time
`Denied { by: secrecy }`.

**Opt-in sources.** Nothing is secret by default: `ratio.secrets.enabled` is `false` and
`ratio.secret_paths` is empty. When enabled, labels come from `tools.read_file`, `tools.grep` and
`tools.list_dir` results for paths matching `ratio.secret_paths` (a suggested preset is the
safety floor's secret patterns: `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `.npmrc`,
`.netrc`, …); memory entries marked `private`; answers to `session.ask` questions asked with
`secret: true`; output of commands matching `ratio.secret_commands`; and any value derived from a
labelled one.

**Propagation.** Operations propagate the label to every result computed from a labelled operand,
including scalars (`text.len(s)` of a secret `s` is secret), because one bit can be the bit that
matters. `Json` carries labels on its leaves: `json.encode` and `json.stringify` of a labelled
value give labelled JSON, and `json.decode` keeps them. `hash.*` of a labelled value is labelled
too; only the redaction digest below is exempt. Branching on a secret `Bool` is allowed; that
implicit flow is outside the guarantee, as for taint. The labels combine: a secret file's contents
are secret and untrusted.

**Conversations are labelled.** Labels attach to **messages**: a message appended with a
labelled value (by `context.append_*`, a tool result from `tools.dispatch`) is labelled, and so is
every model reply produced on a conversation that already holds a labelled message (the model has
read the secret). A conversation is **labelled** while it holds at least one labelled message.
Labels are journalled (`conversation_labelled`, naming the first labelled message and its source)
and survive resume and branching; a compaction summary of labelled messages is labelled, and must
be written by a local model or the compaction is refused. A model call or an outbound tool call
made **on** a labelled conversation is a sink, decided by the rules below as if its whole content
were secret.

**Invariant: replies after a secret are labelled.** Every model reply produced on a conversation
after its first labelled message is itself labelled, at the moment it is appended. Removing or
shrinking messages therefore never unlabels what depends on a secret: `context.clear_stale` and
mid-turn clearing replace a labelled tool result with a stub that **keeps the label**, a
compaction summary of labelled messages is labelled, and `context.rollback` past the first
labelled message leaves an unlabelled conversation only because every labelled reply after it is
removed too. The conformance suite checks the invariant: after any sequence of appends, clears,
compactions and rollbacks, no unlabelled message follows a labelled message that it was produced
after.

**Tool calls and engine state.** Every tool-call argument a model produces on a labelled
conversation is a labelled value, so the sinks below see it as secret. Engine state written from
labelled values carries the label too: the plan set by `update_plan` (`session.update_plan`), the
harness aids' state set by `update_state`, the session title (an auto-title generated from a
labelled conversation, or a title set from labelled text), and authored source written through
`write_harness` or `meta.author`. These labels are journalled with the state, carried into forks,
branches, `fork_at` copies and kept-context blocks (so a kept `<current-plan>` block survives
compaction and stays labelled), and redacted at the daemon boundary like any labelled value, which
includes titles sent to clients and paired phones. Re-injecting labelled engine state into an
unlabelled conversation (a carried plan block, a state block, recalled text) labels that
conversation. A labelled authored package can run only at contained tier, which never holds
secrets, so it cannot run at all until the user declassifies it in the desktop app.

Copies keep labels: `context.fork` of a labelled conversation is labelled; `context.fork_at(mark)`
copies only the messages before the mark, so it is labelled only if one of those is;
`context.excerpt`, `context.last`, `context.snapshot` and every `Transcript` carry the labels of
the messages they copy; a `session.branch` at a turn after the first labelled message is labelled,
one before it is not.

**When a conversation becomes labelled.** The engine shows the user a notice (and the desktop a
banner on the session): "This conversation now holds secret material from <source>; hosted models
and outbound tools are refused on it." Every later refused call raises `Denied { by: secrecy }`
in the program, and in the native loop or `tools.dispatch` becomes a refusal the model reads. The
banner offers the recovery paths, which harness code can also take:

1. **Continue on a local model**: switch the session's model (or the frame's `opts.model`) to a
   model with location `local`; model calls on the labelled conversation are then allowed, and
   outbound tools stay refused.
2. **Declassify**: the user declassifies the conversation in the **desktop app only**
   (`session.control` `declassify_conversation`, which requires a single-use challenge bound to the installed desktop app's identity and answered through its native dialog, the pattern of `plugins.approval_challenge` and `pull_requests.auto_merge_challenge` (`docs/05-tools-and-isolation.md#auto-merge-the-desktop-app-alone-implemented`); there is no
   CLI path, because a process an agent detached could pass the CLI's user check; impossible in an
   unattended session); the labels are removed, journalled with the user's decision.
3. **Fork without the secret**: branch the session (or `context.fork_at` in a harness) at a point
   before the first labelled message; the copy is unlabelled and continues with any model.

A harness can detect the state with `context.labelled(conv)` and choose a path itself, for
example by catching `Denied { by: secrecy }` and retrying on `model.route(location: local)`.

**Sinks.** A labelled value, or a call on a labelled conversation, may not reach:

| Sink | Why |
| --- | --- |
| Network tools: `web_search`, `web_fetch`, the browser tools, MCP tools of remote servers, `media_*` with hosted backends | They send their arguments off the machine |
| A model call whose model is hosted (location `cloud`), in any section | The provider receives it; local models are allowed |
| `tools.run_command` and `tools.run_background` (the command line and its standard input) | A command can upload anything (`curl`, `git push`, `scp`) |
| `tools.write_file` and `tools.edit_file`, unless the target path itself matches `ratio.secret_paths` | Keeps secret content in files that are themselves secret paths, where the command rule below can see them. It does not by itself stop an upload: a command can send a file without Ratio reading it (`curl -d @.env`), which is what the command rule is for |
| `tools.call_tool` and `tools.mcp_call` | They can reach any tool by name, including outbound ones |
| `agents.spawn`, `agents.steer`, `agents.queue` briefs and `agents.ask` questions | The receiving agent may have network tools and runs outside this frame's checks |
| Memory writes, `session.emit`, `eval` metrics, scores and labels | They persist or publish |
| A harness argument or result crossing into a frame whose `sees` lacks `secrets` (chapter 04); such a frame also cannot run an effect whose result would be labelled | Frames see secrets only when granted |
| Logs, traces and every event that leaves the daemon | Replaced by `[secret: <digest>]` (below) |

**Commands that name secret paths.** With secrecy enabled, a `run_command` or `run_background`
call is also a sink when the command line **references** a path matching `ratio.secret_paths`,
whatever the labels of its arguments: the command could read the secret itself and send it on. It
**asks the user every time** (no rule, session allowance or permission mode lifts it, as for the
safety floor) and is **refused in an unattended session**. Matching follows the safety floor's
command parsing (`docs/05-tools-and-isolation.md#permission-modes-implemented`):

- the line is split into commands at `;`, `&&`, `||`, `|`, `&` and newlines, and shells running a
  command string (`sh -c`, `cmd /c`, `powershell -Command`), wrappers (`env`, `nice`, `xargs`,
  `find -exec`, …) and command substitutions are unwrapped, as the floor does;
- every word is unquoted (quotes, backslashes and `^` removed as the floor does) and checked as a
  candidate path: a bare word; the value of `--opt=value` and of a short option glued to its value;
  a word after `@` (`curl -d @file`, response files); and the target of every redirection (`<`,
  `>`, `>>`, `2>`, `<>`, `&>`);
- a candidate is resolved against the working folder and the environment's folders and matched
  against `ratio.secret_paths` (case-insensitively on Windows); a glob word (`*.env`, `secrets/*`)
  matches when it could match a secret path;
- a word the parser cannot read (starting with `$` or a backtick, a brace expansion, an encoded
  PowerShell command) counts as a match, so it asks.

This is **best effort**, a stated residual like the floor's: a script or program that opens a
secret path it was not given on the command line (from a config file, a hard-coded path, a
variable set earlier in the shell, an alias) is not seen.

**Conversion.** `declassify(x, because: Str)` removes the label. It is an effect, monomorphic
only, journalled like `trust` with a digest, listed transitively, and it **always asks** the user
(no rule can allow it); in an unattended session it is refused.

**Redaction digests.** Where a secret would be shown or emitted, it is replaced by
`[secret: <digest>]`, where the digest is an HMAC keyed per session, so equal secrets can be
recognised within a session without the digest revealing a low-entropy secret.

**Tapes.** Effects whose responses carry labels are recorded in the session journal (which stays
on the machine) with their labels, but redacted from tape files written by headless runs;
replaying such a run needs `--ratio-tape-mode lenient` for those effects. Replay never removes a label (chapter 01).

**Parity.** With the default configuration no source is configured, so no value or conversation
is ever labelled and the sink rules never fire; `std:default` under defaults therefore behaves
exactly as the native loop, and the parity suite runs with defaults (chapter 08).

## Evaluation

Evaluation is strict and left to right: arguments before the call, fields in written order,
operands of a binary operator left before right. `and`, `or` and `??` short-circuit.

**Shadowing.** A `let` may shadow an earlier binding in the same or an enclosing scope, including
a parameter (`let n = math.clamp(n, 1, 9)`); this is allowed and draws only a warning (lint
`shadowed_binding`), which the checker does not escalate. Shadowing a standard-library module also
draws a warning.

**Bindings.** `let` binds immutably, with a pattern that must be irrefutable (or the checker asks
for `if let` or `match`). `var` binds a mutable local; assignment is a statement. A closure
captures variables **by value at the moment it is created**, and cannot assign to a captured
`var` (a checker error).

**Functions.** `fn` declarations are pure functions over values unless they call effectful
built-ins or harnesses; there is no effect annotation. Recursion is allowed and bounded by the
call-depth, step and frame-depth limits (chapter 07).

**Options.** `x?.f` is `none` if `x` is `none`; `x ?? d` supplies a default. Reading an absent
optional record field gives `none`.

**`if let` and `while let`.** `if let p = e { … } else { … }` tests a pattern. When `e` has an
option type `T?`, the pattern is matched against the unwrapped `T`, and the condition fails on
`none`: `if let (from, to) = entry.split_once(" -> ") { … }` binds both parts when
`split_once` returns a tuple and takes the `else` branch on `none`. When `e` is not an option, `p`
is matched against `e` directly and may be refutable (`if let Denied { by } = e.kind { … }`). A
pattern of option type itself (`if let none = x`) is matched against `e` without unwrapping.
Bindings are in scope in the first block only.

## Control flow

- `if` without `else` has type `()`. With `else`, both branches must have a common supertype.
- `match` must be exhaustive; the checker names the missing variants or literal cases. Guards
  (`if …`) do not count towards exhaustiveness. A pattern other than a binding, `_` or `none`
  matched against `T?` matches a present value and is checked against `T`; a binding matches
  the option itself.
- `for p in xs { … }` iterates lists, ranges and maps (as `(key, value)` tuples, in key order); its
  value is `()`. `while cond { … }` and `loop { … }` run until `break`; `break v` gives `loop` its
  value. `repeat n { … }` runs the block `n` times.
- Every loop iteration costs steps (chapter 07), so an unbounded loop ends with a `Budget` error
  rather than hanging the session.
- `return` leaves the enclosing function or harness; inside a closure, it leaves the closure.

## Errors

### Decision R7: exceptions with a closed set of kinds

Effects fail often and for reasons a harness must tell apart (the user declined; the test
failed; the model returned garbage; the budget ran out). Ratio uses exceptions so that the common
path stays short, with a **closed** set of kinds so that `catch` arms are exhaustive and every
failure has a known shape.

```ratio
type Error = {
  kind: ErrorKind,
  message: Untrusted<Str>,     // may quote tool or model output
  data: Json,                  // kind-specific details, documented per kind
  site: Site,                  // file, line, column, frame path
}
```

| `ErrorKind` | Raised when | Catchable |
| --- | --- | --- |
| `Denied { by }` | A call was refused: `by` is `rule`, `mode`, `floor`, `user`, `hook`, `profile`, `attenuation`, `unattended`, `secrecy` or `sees` | Yes |
| `ToolFailed` | A tool ran and reported failure (`data`: tool, exit code, output excerpt) | Yes |
| `ModelFailed` | An endpoint error, an empty reply, a refusal, or a reply that could not be parsed as tool calls | Yes |
| `RateLimited` | A usage limit the session's resume policy chose not to wait out | Yes |
| `SchemaMismatch` | Structured output or `json.decode` did not match the schema after repairs (`data`: violations) | Yes |
| `Budget { which }` | `tokens`, `cost`, `steps`, `wall`, `model_calls`, `tool_calls`, `depth`, `call_depth`, `heap`, `width` ran out | Only outside the scope whose budget ran out |
| `Timeout` | A `with timeout` or a `timeout:` argument elapsed | Yes |
| `Cancelled` | The session was stopped for good, a `race` sibling won, a `par` sibling failed, or a `par_fold` stopped | No (observed by `defer`) |
| `Assertion` | `assert` failed | Yes |
| `NotFound` | A named thing does not exist (file, memory, agent, harness, model) | Yes |
| `Invalid` | A bad argument, an out-of-range index, an integer overflow, a malformed regex | Yes, except a branch-restriction violation (below) |
| `Unsupported` | The backend cannot do what was asked (logprobs on an endpoint without them) | Yes |
| `Replay` | A replay run diverged from its tape | No |
| `Question { question, options }` | A callee needs an answer it cannot get (chapter 04) | Yes |
| `User { tag }` | `raise fail("tag", message)` | Yes |

```ratio
let report = try {
  tools.run_command("cargo test -q", check: true)
} catch {
  e if e.kind is ToolFailed => json.decode::<CommandResult>(e.data),
  e if e.kind is Denied => raise fail("no-tests", "running tests was refused"),
}
```

`try { … } catch e { … }` binds the error for a single arm. Uncaught kinds in an arms form
propagate. `result(|| expr)` evaluates `expr` and returns `Result<T>` (`Ok { value }` or
`Err { error }`), which is how fan-out collects per-branch outcomes without stopping at the first
failure.

**Budgets cannot be swallowed where they run out.** A `Budget` error raised because the budget of
a scope ran out (a harness call's, or a `with { budget: … }` block's) cannot be caught inside that
scope, only by the code that opened it.

**Cancellation cannot be swallowed.** `Cancelled` passes through `catch`. `defer { … }` blocks run
on every exit from their scope (normal, error, cancellation) in reverse order; a `defer` block may
make effects that release resources (`command_stop`), and those effects are exempt from
cancellation but bounded by a 10-second grace period. They are also exempt from the drain fence
(below), so a write made in a `defer` block may run concurrently with an outstanding drain:
`defer` blocks are for releasing resources, not for writing.

## Concurrency

### Decision R8: structured concurrency only

Concurrency serves fan-out: several critics, several samples, several searches, several
independent tool calls. Ratio offers it only in forms whose lifetimes nest in the code:

| Form | Value | Semantics |
| --- | --- | --- |
| `par { a; b; c }` | `(A, B, C)` | Runs the expressions concurrently; returns their values in written order. If one raises, the others are cancelled and the error propagates |
| `par_map(xs, f, limit: n)` | `[B]` | Applies `f` to each element with at most `n` running at once (default 4); results by index; fail-fast like `par` |
| `par_map_settled(xs, f, limit: n)` | `[Result<B>]` | As `par_map` but never fails fast; every element gets a result |
| `race { a; b }` | common supertype | Returns the first to finish successfully; cancels the rest; if all fail, raises the failure of the **first branch in written order** |
| `par_fold(xs, f, init: a, step: s)` | `Folded<A>` | Runs `f` over `xs` as `par_map` does and delivers each outcome, with its index, to `step` one at a time; stops early, cancelling the rest, when `step` returns `Stop` (chapter 06, `control`) |
| `with timeout d { … } else { … }` | block's type | Cancels the block after `d`; runs `else` instead, or raises `Timeout` without one |
| `agents.spawn(…)` | `Task<T>` | An engine subagent; the only thing that can outlive its creating scope (chapter 06, `agents`) |

**Branch restrictions.** Inside a branch of `par`, `race`, `par_map`, `par_map_settled` or
`par_fold`, inside `par_fold`'s `step` (which runs in the caller while branches are live), and in
any function or harness these call, the checker forbids, and the interpreter refuses with
`Invalid`:

- assigning to any `var` declared outside the branch (closures already capture by value; this
  rule also covers `par { … }` blocks, which are not closures);
- `state.set`, `state.update` and `state.reset`;
- appending to, or running a model on, a conversation that was not created inside the branch (for
  `step`, inside that `step` call).

The checker does not yet refuse the first rule (`var`s assigned inside a branch); the interpreter
does, at run time. A violation is a defect of the program, not a recoverable failure: its
`Invalid` is fatal to the evaluation, and no `catch`, `result` or sibling `race` branch can
swallow it.

**Implementation model.** All branches of a frame tree run on the agent's own task as futures; the
interpreter has no threads. Pure evaluation is cooperative, yielding at every 1,000-step
checkpoint (chapter 01); effects are where branches interleave, and the host runs them
concurrently (chapter 10: `Host` takes `&self`).

**Determinism.** Results are combined in program order. The outcomes that depend on timing are
**scheduling choices**, and each is an event on the tape's turn sequence (chapter 01): which
`race` branch won; which `par` branch failed first; whether a `timeout` fired; the cancellation
point of every cancelled branch, whatever cancelled it (a `race` loss, a failing `par` sibling, a
`par_fold` stop, a fired timeout, a steps exhaustion in a sibling), recorded as
`(effect index, step count)` within the branch; how each cancelled call ended (below); timer
readings (`time.now`, `time.since`, `eval.timer`); cache events (`cached`, `memo`, `once`,
chapter 06); `par_fold`'s deliveries in completion order; and budget admissions, refusals and
readings (chapter 04). In replay, cancelled branches replay their recorded effects and are
cancelled at the recorded point, so the same effects are consumed in the same order, and replay
checks every event against the tape alone.

**Width and admission.** At most `limits.width` branches **run** at once per frame tree
(default 16). A branch runs while it is evaluating or awaiting a host effect (a model or tool call,
`agents.await`, `time.sleep`, `rate_limit`). A branch that waits for anything else, its own child
branches or a single-flight cache producer (chapter 06, `control`), is **parked** and releases its
slot; when its wait ends it takes a slot back ahead of other waiters. Every slot holder therefore
awaits only evaluation or host effects, which complete without any other branch's help, so nested
fan-out never deadlocks on the width. Branches waiting for a slot start in lexicographic order of
their branch paths (chapter 01): program order, depth first. The `limit:` of `par([…])`,
`par_map`, `par_map_settled` and `par_fold` is a per-construct cap inside the width, and the
parallel built-ins (`model.generate_n`, `think.self_consistency`, `think.best_of`, `think.debate`,
`think.beam_search`) count against the width the same way. Admission changes only when branches
start, never keys or results, and is not recorded. Every branch draws on the same budgets as its
frame; a scoped budget block inside each branch gives each its own share (chapter 06, `control`).

**Cancellation** reaches a branch at its next effect boundary or at its next 1,000 steps,
whichever is first. A model request in flight is aborted. A background process the branch started
is stopped unless it was handed out of the scope. A tool call in flight ends by its tool's
**cancellation class** (chapter 06, `tools`):

- an `abort`-class call is stopped at once and the host acknowledges it; a call that had already
  reached a remote server (`web_*`, MCP) is recorded `abandoned` rather than `aborted`, because the
  remote side may still act on it;
- a `drain`-class call runs to completion (or to its deadline) and its result is discarded;
- a call still waiting for an approval or a `tool.before` hook is **withdrawn**, whatever its
  class. The host resolves a withdrawal racing the user's answer atomically: either the answer is
  taken first (the call is approved, starts, and is then cancelled by its class) or the withdrawal
  is (the call never runs). An answer that arrives for a withdrawn prompt is discarded entirely,
  including any "allow for this session" choice in it, and is never remembered as a denial.

None of these outcomes is returned to the program; each is an event on the turn sequence with the
call's key (and, for a drained call, the digest of its discarded response), and replay verifies
that the same calls end the same way.

**Deadlines.** Every host effect carries a deadline: the earliest of the enclosing `with timeout`
deadlines and the end of the frame's `wall` budget. The host bounds the call by it (request
timeouts for models and MCP calls; `run_command`'s effective timeout is the smaller of its
argument and the time left), so a command is killed when an enclosing timeout fires, exactly as
its own timeout would kill it. A response caused by a deadline (`Timeout`, or `timed_out: true`
from `run_command`) is recorded like any other. Replay ignores deadlines.

**The drain fence.** An **outstanding drain** is a `drain`-class call that was running when its
scope was cancelled and that the cancelled scope left running; a `drain`-class call in flight in a
live scope is an ordinary call. A cancelled scope completes as soon as its `abort` calls have
acknowledged; it does not wait for its outstanding drains. They are **re-parented** to the nearest
enclosing frame that is not itself being cancelled (or to the turn, when every frame is), an event
on the turn sequence; their reservations already sit on that frame's ledgers and its ancestors',
so only ownership moves, and harness frames inside the cancelled scope close at once. The owning
frame's **drain fence** then holds back, until its outstanding drains have finished:

- the frame's own close (its harness call cannot return);
- every later `drain`-class call anywhere in the same frame tree, which waits before it starts, so
  a new write never races a stale one;
- the completion of the turn.

Each outstanding drain keeps its reservation until it finishes and is then debited its actual
charge, so cancellation never yields free or unbounded spend. Effects in `defer` blocks and
`tools.command_stop` never wait behind the fence: they release resources, and stopping a stuck
process must not queue behind the call it would end. The fence cannot deadlock: every outstanding
drain is already running in the host (a call waiting for approval was withdrawn, not drained) and
finishes within its finite bound (chapter 10, host contract) without any action by the program,
and the calls the fence holds back have not started.

**Session stop** is not cancellation: `session.control stop` drains every in-flight tool call
whatever its class and holds the interpreter future (chapter 01).

Rejected: unstructured `spawn`/`await` of local tasks. Lifetimes escape the code, cancellation
and budgets become guesswork, and a forgotten task keeps spending after the turn ends.

## Modules and imports

- A **package** is its entry file and the files it imports. A file is a module; its `pub` items
  are visible to importers, everything else is private.
- `import "./lib/critics.ratio" as critics` imports a file by path, relative to the importing
  file. The path must stay inside the package's **source root** (the folder its entry file was
  found in); `..` that leaves it, absolute paths, and symbolic links are refused.
- An import binds its alias; without an alias or a list, the last word of its name or its file
  name; `{ a, b }` binds those public items, and the module only with an alias. `pub` does not
  apply to imports. A path's components hold only ASCII letters, digits, `_`, `-` and `.`, none
  ending in a dot or naming a device (`con`); paths differing only in case are refused. A package
  name is ASCII identifiers (letters, digits and `_`; not keywords) joined by `.`, so every
  package can be named in an import, and a plugin name one such identifier; each is at most 128 bytes and never
  a device name.
- `import std:debate`, `import user:critics.strict`, `import plugin:acme/review` import a
  registered package by **source-qualified name**; `@"^1.2"` pins a semver range (`^`, `~` or
  `=` with one to three numbers, `=` needing three), checked against the package's `meta`
  `version`. A bare name (`import debate`) resolves to `std`, then `user`: a lower-trust package
  never shadows a higher-trust one, and a plugin, project or authored package is reached only by
  its qualified name.
- The standard library modules (`model`, `think`, `tools`, `agents`, `memory`, `context`,
  `session`, `state`, `data` and its submodules, `control`, `eval`, `meta`) are in scope in every
  file without an import. The `data` submodules (`text`, `list`, `map`, `json`, …) are also in
  scope by their short names. Method syntax `x.f(a)` resolves to the module of `x`'s type first,
  then to a function `f` in scope whose first parameter accepts `x`.
- Import cycles are errors. Name resolution is lexical: a local binding shadows an import, which
  shadows the prelude; the checker warns on shadowing a standard module or `trust`, `as_task`
  or `declassify`.
- **Trust follows source.** A package may import only the standard library, built-in (`std`)
  packages, files in its own source root, and registered packages as follows: a user package may import user, plugin and project packages by qualified
  name; a plugin package only `std` and its own plugin's; a project package only `std` and its own
  project's; an authored package only `std` and authored packages of the same area.

## Persistent state

A package may declare one `state`:

```ratio
state: { rejections: Map<Str, Int>, plan: Untrusted<Plan>? } = { rejections: {}, plan: none }
```

- The type must be schema-able and contain no handles. The initialiser is a constant expression.
- `state.get()` returns the current value; `state.set(v)`, `state.update(|s| …)` and
  `state.reset()` replace it. Changes are visible at once in the turn and are written when the
  turn ends: one `ratio_state` event with the JSON and the type digest, if the value changed.
- State belongs to **one agent**: the session root's for a `@turn` entry; each subagent's own for
  a `@task` entry. A frame reads it only if its `sees` includes `state` (chapter 04).
- At most 64 KiB of JSON. A larger value raises `Invalid` at `state.set`.
- On resume after a restart, the newest `ratio_state` event is read back. A branch or a rewind of
  the conversation keeps the state as of its cut point.
- **Type changes.** When the stored type digest equals the declared type's, the value is used.
  Otherwise the JSON is decoded as `Untrusted<T>` of the declared type: if `T` contains no trusted
  text (every text position is already untrusted), that value is used; otherwise the state is
  reset to the initialiser with a notice. Stored untrusted text never comes back as trusted.

The same digest rule keys caches: `cached` and `once` entries are keyed by `(package, key, type
digest)`, so a value cached as `Untrusted<Str>` is never returned where a `Str` is expected.
Caches keep secrecy labels too: a session-scoped entry (`ratio_cache`) stores the labels of every
leaf with the value and a hit restores them, so a value cached while labelled comes back labelled
in a later turn or a resumed session; and a hit that would deliver a labelled value to a frame
whose `sees` lacks `secrets` raises `Denied { by: sees }` (chapter 04).

## Tests

`test "name" { … }` blocks are compiled only for `imperium harness test` and `harnesses.check`
with `test: true`. They run against a scripted host (chapter 10) in which `model.*`, tools and
agents answer from fixtures declared with `eval.fixture` and `eval.script_model`. Tests never run
in a session.
