# `meta`: introspection and loading harnesses

## The registry

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `meta.harnesses` | `(query: Str? = none) -> [HarnessInfo]` | Registered packages this package may load (chapter 03, trust follows source): `{ name, version, source: builtin \| user \| plugin \| project, description, entries: [turn \| task], public: [Signature], hash, trust_sites: Int }`, best match first | — | `meta` |
| `meta.harness` | `<H>(name: Str) -> H` | Loads a public harness of a registered package by `name` (`package.harness`, with an optional `@range`), checking that its signature is the harness type `H` the caller expects. `name` must be trusted text (chapter 03, sinks). The package is parsed and checked once per session and cached by hash | `NotFound`, `Invalid` (signature differs, with both), `Denied` (trust rules) | `meta` |
| `meta.has_harness` | `(name: Str) -> Bool` | Whether `meta.harness` would find it (ignoring the signature) | — | `meta` |
| `meta.signature` | `<H>(h: H) -> Signature` | A harness's parameters and result, with their JSON Schemas: `{ name, params: [{ name, type, schema, default? }], returns: { type, schema } }` | — | `pure` |
| `meta.package` | `() -> PackageInfo` | This package: `{ name, version, hash, source, path }` | — | `meta` |
| `meta.trust_sites` | `() -> [Site]` | Every `trust` call site of this package (what `harnesses.check` lists) | — | `meta` |

## The frame

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `meta.frame` | `() -> FrameInfo` | `{ harness, title, depth, path, model, effort, mode, location, attended, context: fresh \| fork \| none \| main, tools: Int }` | — | `engine` |
| `meta.caller` | `() -> CallerInfo?` | The calling harness and call site; `none` in an entry | — | `engine` |
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
