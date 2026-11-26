# `meta`: introspection and loading harnesses

## The registry

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `meta.harnesses` | `(query: Str? = none) -> [HarnessInfo]` | Registered packages this package may load (chapter 03, trust follows source): `{ name (source-qualified), version, source: std \| user \| plugin \| project \| authored, description: Untrusted<Str>, entries: [turn \| task], public: [Signature], hash, trust_sites: Int, offered: Bool }`, best match first | — | `meta` |
| `meta.harness` | `<H>(name: Str) -> H` | Loads a public harness of a registered package by source-qualified `name` (`std:debate.judge`, with an optional `@range`; a bare name resolves to `std`, then `user`, chapter 03; the source is chosen first, then the longest prefix of the name that names a registered package in it is the package and the rest the harness; an ambiguous name makes `harnesses.check`'s listing incomplete, chapter 04), checking that its signature is the harness type `H` the caller expects (`meta.harness::<harness(Str) -> Untrusted<Str>>("user:critics.strict")`). An authored harness loads only under the authored rules of chapter 07, and its result type is mapped to `Untrusted<R>` whatever it declares. `name` must be trusted text (chapter 03, sinks). The package is parsed and checked once per session and cached by hash. It may only be called, never used as a function value (README, conventions), and `harnesses.check` follows the names it is called with | `NotFound`, `Invalid` (signature differs, with both), `Denied` (trust rules) | `meta` |
| `meta.has_harness` | `(name: Str) -> Bool` | Whether `meta.harness` would find it (ignoring the signature) | — | `meta` |
| `meta.signature` | `<H>(h: H) -> Signature` | A harness's parameters and result, with their JSON Schemas: `{ name, params: [{ name, type, schema, default? }], returns: { type, schema } }` | — | `pure` |
| `meta.package` | `() -> PackageInfo` | This package: `{ name, version, hash, source, path }` | — | `meta` |
| `meta.trust_sites` | `() -> [Site]` | Every `trust`, `as_task` and `declassify` site of this package and, transitively, of everything it imports or can load (what `harnesses.check` lists) | — | `meta` |
| `meta.author` | `(name: Str, source: Untrusted<Str>, description: Untrusted<Str>) -> AuthoredVersion` | Writes a new version of an **authored** harness package to the session's authored area (chapter 07): checks it (parse, types, taint, no trust sites), pins it by IR hash and journals it; returns `{ name: "authored:session/<name>", version: Int, closure_hash, diagnostics, diffs_from_previous }`. A package with errors is stored but not loadable; harnesses with trusted parameter types are marked not runnable by models | `Invalid` (too large, bad name), `Denied` | `tool:`*`write_harness`* |
| `meta.authored` | `() -> [AuthoredVersion]` | The authored packages of this session and environment, every version, newest first, with who wrote each (agent id) and its approval status | — | `meta` |

## The frame

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `meta.frame` | `() -> FrameInfo` | `{ name, title, depth, path, model, effort, mode, location, attended, context: fresh \| fork \| none \| main, tools: Int }` (`name` is the harness's name; `harness` is a keyword) | — | `engine` |
| `meta.caller` | `() -> CallerInfo?` | The calling harness and call site, `{ name, site }`; `none` in an entry | — | `engine` |
| `meta.depth` | `() -> Int` | The frame depth (0 in an entry) | — | `engine` |
| `meta.limits` | `() -> FrameLimits` | The frame's caps (budget, depth, width, heap) after attenuation | — | `engine` |
| `meta.site` | `() -> Site` | The current source position and frame path | — | `pure` |
| `meta.permissions` | `() -> PermissionView` | `{ mode, location, paths: [Str], extra_rules: [Rule], tools: [Str] }` for this frame | — | `engine` |
| `meta.can` | `(tool: Str, arguments: Json = {}) -> Decision` | What the engine would decide for this call now, without running it: `run`, `ask` or `refuse`, with the reason (rule, mode, floor, profile, attenuation). Hooks and auto mode's classifier are not consulted, so `run` may still become `ask` or `blocked` at dispatch | — | `engine` |

## The environment

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `meta.tools` | `() -> [ToolInfo]` | The frame's tool universe: `{ name, source: builtin \| mcp \| plugin \| provider, offered: Bool, deferred: Bool, asks_by_default: Bool }`; descriptions of MCP and plugin tools are untrusted | — | `engine` |
| `meta.models` | `() -> [ModelCard]` | Every model in the table and the local model folders, with `allowed: Bool` for this frame's location | — | `engine` |
| `meta.engine` | `() -> EngineInfo` | `{ imperium_version, protocol_version, ratio_version, os }` | — | `engine` |
| `meta.ratio_version` | `() -> Str` | The language and standard library version (semver) | — | `pure` |
| `meta.features` | `() -> [Str]` | Optional engine features available in this session: `web_search`, `browser`, `media`, `knowledge_agents`, `tool_loading`, `code_mode`, `harness_agents`, `structured_output`, `logprobs`, … | — | `engine` |

## Model-facing harness tools

These are Imperium tools a **model** calls (defined in `imperium-protocol` and exported to
`schemas/tools/`), not Ratio built-ins, so they are not counted in the module's built-ins. They are
specified here because they are the model's side of `meta.harness` and `meta.author`.

| Tool | Parameters | Semantics | Refused when |
| --- | --- | --- | --- |
| `write_harness` | `name`, `source`, `description` | As `meta.author`: stores a new version of `authored:session/<name>`, checks it, pins it by closure hash, journals it (chapter 07). Runs without asking; a pseudo-tool for rules | The frame lacks the tool; size or count limits; a rule denies it |
| `run_harness` | `name` (qualified or bare), `arguments` (a JSON object) | Runs a `pub`, non-entry harness of a package in `harness.offered` or the session's authored areas as a child frame of the calling agent. **Every argument is decoded through the parameter schema as deep-`Untrusted`, whatever the harness's source.** A harness whose parameter types contain `Str`, `UserText`, `Brief` or `TrustedPrompt` anywhere is **not offerable**: it is refused at check time (reported "not runnable by models") and at offer time (absent from the tool's list of harnesses; a call by name is refused). Authored harnesses run at the tiers of chapter 07. The result is returned as the tool result (JSON of the declared result type), which the model reads as data | Not offered; trusted parameter types; arguments do not match the schema; contained tier impossible on the route; approval declined |
