# `tools`: Imperium's tools

There is one built-in per Imperium tool. Each one builds the tool call the model would have made
and hands it to the engine's dispatch, so it is decided and run exactly as that call would be
(`docs/05-tools-and-isolation.md#permissions`): the frame's tool set, the permission profile, the
rules, the safety floor, the permission mode, `tool.before` and `tool.after` hooks (which may
block or rewrite it), approval, and the `tool_started` and `tool_finished` events. The arguments
are those of the tool's schema (`crates/imperium-tools/src/invocation.rs`,
`crates/imperium-protocol/schemas/tools/*.json`); Ratio adds only typed results and named
optional parameters.

A call the engine refuses raises `Denied { by }`; a tool that runs and reports failure raises
`ToolFailed` with the tool's message as untrusted text. The exception is `tools.dispatch`, which
runs calls a **model** chose and, like the native loop, turns every outcome into a tool result for
the model instead of raising.

Secret-labelled arguments (chapter 03) are refused at the sinks listed there (`run_command`, network tools, `call_tool`, `mcp_call`, non-secret writes) with `Denied { by: secrecy }`, by the host and again at dispatch.

A tool that is not offered in the session (web search turned off, no browser, no media backend, a
remote runtime without `glob`) raises `NotFound` naming the tool.

## Files

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.read_file` | `(path: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> FileText` | Reads numbered lines (offset is 1-based); without `limit`, at most 2,000 lines and the read budget sized from the frame's context window; `more` and `next_offset` say how to continue. `text` is the content without line numbers | `NotFound`, `Denied`, `ToolFailed` (binary, folder) | `tool:read_file` |
| `tools.write_file` | `(path: Untrusted<Str>, content: Untrusted<Str>) -> Written` | Creates or replaces a file; `{ path, bytes, created }` | `Denied`, `ToolFailed` | `tool:write_file` |
| `tools.edit_file` | `(path: Untrusted<Str>, old: Untrusted<Str>, new: Untrusted<Str>) -> Written` | Replaces exactly one occurrence of `old`; it must match exactly and be unique | `ToolFailed` (no match, several), `Denied` | `tool:edit_file` |
| `tools.list_dir` | `(path: Untrusted<Str>, offset: Int = 0) -> Untrusted<Listing>` | Up to 500 entries, `{ entries: [{ name, dir: Bool }], more, next_offset? }` | `NotFound`, `Denied` | `tool:list_dir` |
| `tools.glob` | `(pattern: Str, path: Untrusted<Str>? = none, max_results: Int = 100, offset: Int = 0, include_ignored: Bool = false) -> Untrusted<Globbed>` | Files by name, sorted by path; `{ paths, more, next_offset?, hidden_by_rules: Int }` | `Denied`, `Invalid` (bad pattern) | `tool:glob` |
| `tools.grep` | `(pattern: Str, path: Untrusted<Str>? = none, glob: Str? = none, case_insensitive: Bool = false, context: Int = 0, max_results: Int = 100, offset: Int = 0, include_ignored: Bool = false) -> Untrusted<Grepped>` | Regular-expression search; the engine's `path:line:text` output parsed into `{ matches: [{ path, line, text, before: [Str], after: [Str] }], more, next_offset?, hidden_by_rules, raw }` | `Denied`, `Invalid` (bad regex) | `tool:grep` |

## Commands

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.run_command` | `(command: Untrusted<Str>, timeout: Duration = 120s, check: Bool = false) -> Untrusted<CommandResult>` | Runs a shell line in the working folder under the session's isolation tier; `{ exit_code, stdout, stderr, timed_out }`. A non-zero exit is a normal result unless `check`, which raises `ToolFailed` | `Denied`, `Timeout` (only with `check`), `ToolFailed` (with `check`) | `tool:run_command` |
| `tools.run_background` | `(command: Untrusted<Str>) -> Process` | Starts the command in the background (`run_command` with `background: true`) and returns its handle at once. The frame that started it owns it; it is stopped when that frame's scope ends unless returned or stored in a handle-holding value passed out | `Denied`, `ToolFailed` (eight already running) | `tool:run_command` |
| `tools.command_output` | `(p: Process, since: Int? = none) -> Untrusted<ProcessOutput>` | Status (`running`, `exited { code }`, `stopped`) and output since the last read or byte `since`, at most 30,000 bytes; `next_since` continues | `NotFound` | `tool:command_output` |
| `tools.command_stop` | `(p: Process) -> ()` | Stops the process and everything it started; waits up to 10 s | `NotFound` | `tool:command_stop` |

## Web and browser

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.web_search` | `(query: Untrusted<Str>, count: Int = 10) -> Untrusted<[SearchHit]>` | `{ title, url, snippet }` per result from the configured provider. Searching never asks in Imperium; the query leaves the machine (see chapter 07, exfiltration) | `NotFound` (search off), `ToolFailed` | `tool:web_search` |
| `tools.web_fetch` | `(url: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> Untrusted<Page>` | A page as text, `{ url, final_url, title, text, more, next_offset? }`, under the domain, redirect and `robots.txt` rules and the cache | `Denied`, `ToolFailed` (refused by robots, non-text) | `tool:web_fetch` |
| `tools.browser_open` | `(url: Untrusted<Str>, new_tab: Bool = false) -> Untrusted<PageView>` | Opens a page in the agent's browser; `{ tab, url, title, text }` | `Denied`, `ToolFailed` | `tool:browser_open` |
| `tools.browser_read` | `() -> Untrusted<PageView>` | The active tab's text | `ToolFailed` (no browser) | `tool:browser_read` |
| `tools.browser_click` | `(selector: Untrusted<Str>) -> Untrusted<PageView>` | Clicks the element; asks unless allowed for the session | `Denied`, `ToolFailed` (no element) | `tool:browser_click` |
| `tools.browser_type` | `(selector: Untrusted<Str>, text: Untrusted<Str>, submit: Bool = false) -> Untrusted<PageView>` | Types into the element, optionally submitting | `Denied`, `ToolFailed` | `tool:browser_type` |
| `tools.browser_tabs` | `() -> Untrusted<[TabInfo]>` | `{ id, title, url, active }` per tab | `ToolFailed` | `tool:browser_tabs` |
| `tools.browser_switch` | `(tab: Str) -> Untrusted<PageView>` | Makes a tab active and returns its text | `Denied`, `NotFound` | `tool:browser_switch` |
| `tools.browser_close` | `(tab: Str) -> ()` | Closes a tab (not the last) | `NotFound`, `Invalid` (last tab) | `tool:browser_close` |
| `tools.browser_screenshot` | `() -> Image` | A screenshot of the active tab, for `image` prompt sections on models that take images. Imperium today shows screenshots to the user only; a model-facing screenshot is an engine dependency (chapter 10) | `ToolFailed` (no browser), `NotFound` (not yet available) | `tool:browser_screenshot` |
| `tools.browser_download` | `(url: Untrusted<Str>, filename: Str? = none) -> Untrusted<Downloaded>` | Saves a download the page offered into the session's downloads folder; always asks, every time; `{ path, bytes, final_url, renamed: Bool }` | `Denied`, `ToolFailed` | `tool:browser_download` |
| `tools.preview_open` | `(target: PreviewTarget) -> Preview` | Shows a loopback page in the desktop's Preview panel: `PreviewTarget` is `Url { url }` or `Command { command, port }` (judged as `run_command`) | `Denied`, `ToolFailed` | `tool:preview_open` |
| `tools.preview_close` | `() -> ()` | Closes the preview and stops its server | — | `tool:preview_close` |

## Media

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.media_image` | `(prompt: Untrusted<Str>, width: Int? = none, height: Int? = none, name: Str? = none) -> MediaFile` | Generates an image into the project's media folder; `{ path }` | `NotFound` (no backend), `ToolFailed` | `tool:media_image` |
| `tools.media_speak` | `(text: Untrusted<Str>, voice: Str? = none, name: Str? = none) -> MediaFile` | Text to speech into the media folder | `NotFound`, `ToolFailed` | `tool:media_speak` |
| `tools.media_transcribe` | `(path: Untrusted<Str>) -> Untrusted<Str>` | Transcribes an audio file in the project | `NotFound`, `ToolFailed` | `tool:media_transcribe` |

## MCP

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.mcp_servers` | `() -> [McpServerInfo]` | The session's connected MCP servers, `{ name: Str, tools: [Untrusted<Str>], resources: Bool, prompts: Bool }`: the server name comes from the user's configuration; tool names and descriptions come from the server and are untrusted | — | `engine` |
| `tools.mcp_call` | `(server: Str, tool: Str, arguments: Json) -> Untrusted<McpResult>` | Calls `mcp__<server>__<tool>`; `{ text, content: [Json], is_error }`. A tool-reported error raises `ToolFailed` | `NotFound`, `Denied`, `ToolFailed`, `Timeout` | `tool:mcp__<server>__<tool>` |
| `tools.mcp_resources` | `(server: Str, cursor: Str? = none) -> Untrusted<McpListing>` | One page of the server's resources and templates | `NotFound`, `Denied` | `tool:mcp__<server>__resources_list` |
| `tools.mcp_read_resource` | `(server: Str, uri: Untrusted<Str>) -> Untrusted<Str>` | A resource's text, framed by the engine as untrusted, at most 60,000 characters | `NotFound`, `Denied` | `tool:mcp__<server>__resources_read` |
| `tools.mcp_prompts` | `(server: Str) -> Untrusted<McpListing>` | The server's prompts | `NotFound`, `Denied` | `tool:mcp__<server>__prompts_list` |
| `tools.mcp_get_prompt` | `(server: Str, name: Str, arguments: Map<Str, Str> = {}) -> Transcript` | A prompt's messages, all untrusted, for use in a `context` or `data` section | `NotFound`, `Denied` | `tool:mcp__<server>__prompts_get` |

MCP sampling requests (a server asking the session's model for a reply) are handled by the engine
as today and are not visible to Ratio.

## Discovery and dispatch

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.tool_search` | `(query: Str? = none, names: [Str] = []) -> [ToolSchema]` | Schemas of tools in the frame's universe matching the query or names, as item 61's `tool_search` returns them; descriptions of MCP and plugin tools are untrusted | — | `tool:tool_search` |
| `tools.call_tool` | `(name: Str, arguments: Json) -> Untrusted<ToolResult>` | Runs any tool of the frame's universe by name through the same checks, as item 61's `call_tool` | `NotFound`, `Denied`, `ToolFailed`, `Invalid` (arguments do not match the schema) | the named tool's |
| `tools.dispatch` | `(conv: Conversation, call: ToolCall) -> ToolResult` | Runs one call **a model made** on `conv`, exactly as the native loop does, and appends the paired result message to `conv`. Never raises for tool outcomes: `ToolResult.outcome` is `ok`, `failed` (the tool reported an error, or a command exited non-zero), `Denied { by }` or `Blocked { hook }`, and the text is what the model is shown. A call to a tool outside the frame's set gets the engine's refusal text | `Invalid` (call not pending on `conv`) | the called tool's |
| `tools.refuse` | `(conv: Conversation, call: ToolCall, reason: Str) -> ToolResult` | Answers a pending tool call **without running it**: appends a result with `outcome: Refused { reason }` and the text "Not run: <reason>" (framed as the harness's own note), so the pairing invariant holds and the model learns why. For harnesses that veto calls (a policy, a budget, a reviewer) before dispatch | `Invalid` (call not pending on `conv`) | `engine` |
| `tools.dispatch_all` | `(conv: Conversation, calls: [ToolCall], concurrent: Bool = true) -> [ToolResult]` | Dispatches every pending call of the last assistant message; independent calls run concurrently when `concurrent` and the engine allows it; results are appended in call order | as `dispatch` | the called tools' |
| `tools.offered` | `() -> [ToolSchema]` | The tools a `model.chat` in this frame offers by default (the frame's set, after profiles, agent files and item 61's loading mode) | — | `engine` |
| `tools.spec` | `(name: Str) -> ToolSchema?` | One tool's schema, if it is in the frame's universe | — | `engine` |
| `tools.has` | `(name: Str) -> Bool` | Whether the frame may call the tool at all (the engine may still ask or refuse a particular call; see `meta.can`) | — | `engine` |
| `tools.narrow` | `(set: ToolSet, names: [Str]) -> ToolSet` | The intersection of a set with names or patterns, for building `tools:` overrides | — | `pure` |

## Skills, plugins and the app's configuration

| Built-in | Signature | Semantics | Fails with | Needs |
| --- | --- | --- | --- | --- |
| `tools.skills` | `() -> [SkillInfo]` | The skills the session can load, `{ name, description: Untrusted<Str>, origin: builtin \| user \| plugin \| project, loaded: Bool }`, as the skill index lists them | — | `engine` |
| `tools.plugins` | `() -> [PluginInfo]` | Installed plugins, `{ name, origin, enabled: Bool, active: Bool, withheld: Bool, has_harnesses: Bool, description: Untrusted<Str> }` | — | `engine` |
| `tools.load_skill` | `(name: Str) -> Untrusted<Str>` | Loads a skill's instructions as `load_skill` does; the text is untrusted for Ratio's purposes (a program decides where it goes; the native loop puts it in the conversation) | `NotFound` | `tool:load_skill` |
| `tools.activate_plugin` | `(name: Str) -> ()` | Activates a plugin for the session, as `activate_plugin` (never starts withheld code) | `NotFound`, `Denied` | `tool:activate_plugin` |
| `tools.set_model_card` | `(model: Str, card: Str?) -> ()` | Sets or clears a model's card, journalled and undoable | `NotFound`, `Denied` | `tool:set_model_card` |
| `tools.install_extension` | `(kind: ExtensionKind, source: Str, path: Str? = none, revision: Str? = none) -> ()` | Installs a plugin, skill or agent file as the tool does (code-running plugins stay off until the user approves them in the desktop app) | `Denied`, `ToolFailed` | `tool:install_extension` |
| `tools.uninstall_extension` | `(kind: ExtensionKind, name: Str) -> ()` | Removes one; asks when an agent did not install it | `NotFound`, `Denied` | `tool:uninstall_extension` |
| `tools.suggest_local_model` | `(task: Str, needs: [Str], context_tokens: Int? = none) -> Untrusted<Str>` | Suggests a local model to download for a task (daemon sessions) | `NotFound` | `tool:suggest_local_model` |
| `tools.run_script` | `(code: Untrusted<Str>) -> Untrusted<ScriptResult>` | Runs a program in Imperium's code mode (roadmap item 65) when it is built and enabled; the program runs in the confined Monty worker, never in Ratio. `{ output, summary, error? }` | `NotFound` (code mode off), `ToolFailed` | `tool:run_script` |
