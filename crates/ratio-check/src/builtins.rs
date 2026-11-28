//! The built-in names the checker knows: the primitive types, the shared types of chapter 06
//! (declared in Ratio, `std/prelude.ratio`), and a declarative table of built-in function
//! signatures, one Ratio source per standard module.
//!
//! The table is written in Ratio itself: each module is a list of `fn` declarations with empty
//! bodies, `@pure` on those whose Needs column is `pure`. A function without `@pure` counts as
//! making effects, so a signature added without the attribute fails closed (it is refused in
//! constants, and counts as an effect of an unknown kind). Two type-parameter names are bounds,
//! as in chapter 06: `S` stands for any text (`Str` or `Untrusted<Str>`) and `Num` for `Int`,
//! `Float`, `Duration` or `Money`; a third, `Countable`, for text, a `Prompt` or a
//! `Conversation` (`model.count_tokens`, chapter 06's `Countable`). Default values are written for the reader only; the checker
//! uses only whether a parameter has one.
//!
//! Further attributes say what stage 5 needs to know ([`Traits`]): `@audited(kind)` marks the
//! three trust conversions, `@effect(kind)` and `@tool(name)` what a built-in does (for the
//! guarded-effect lint and `harnesses.check`'s summary), `@secrecy_sink` the built-ins no
//! statically secret value may reach, `@schema(T)` type parameters that must be schema-able,
//! `@user` built-ins whose result is the person's answer, `@secret_answers` `session.ask`,
//! `@loads` `meta.harness`, `@state` the `state` built-ins (whose type parameter is the package's
//! declared `state` type, never inferred or given), and `@local` effectful built-ins that keep
//! their arguments on the machine (not secrecy sinks; every `@local` entry is reviewed against
//! the rule on `Traits::local`). A name that is a keyword in Ratio (`harness`, `loop`,
//! `prompt`) is written with a trailing `_` (`harness_`), which the checker drops, since a
//! declaration cannot be called by a keyword. A parameter so written can be passed only by
//! position, as chapter 06 says.
//!
//! An optional callback is `fn(A) -> B = none` (chapter 06's convention): it may be passed or
//! left out, not passed as `none`.
//!
//! A type parameter that only parameters left to their defaults mention (`log.info("done")`'s
//! `data: T?`) is `()` at that call: no value comes from it and none goes to it.
//!
//! The table holds, from stage 7, every `text` and `re` built-in and, from stage C, every
//! built-in of `agents`, `context`, `control` (with `log`, `trace` and `state`), `eval`,
//! `memory`, `meta`, `model`, `session`, `think`, `tools`, `rand` and `time` (stage 10's frames
//! block below adds `question` and `meta`'s frame readers), and, from stage 8, every built-in of
//! `json`, `list` and `map`. Stage 9 completes `math` to `codec`; `conformance/builtins.txt`
//! (generated from chapter 06) is the list, and `tests/signatures.rs` checks the table against
//! it.

use crate::analysis::Effects;
use crate::types::{Bound, Handle, TypeId};

/// The shared types, in scope in every file behind the file's own names.
pub(crate) const PRELUDE: &str = include_str!("std/prelude.ratio");

/// The standard modules the checker has signatures for: the module's name and its source.
/// `control` holds the functions in scope without a prefix.
pub(crate) const MODULES: &[(&str, &str)] = &[
    (
        "control",
        r#"
@pure fn fail(tag: Str, message: Untrusted<Str>, data: Json = null) -> Error {}
@pure fn result<T>(f: fn() -> T) -> Result<T> {}
@pure fn assert(cond: Bool, message: Str = "assertion failed") -> () {}
@pure fn assert_eq<T>(a: T, b: T, message: Str? = none) -> () {}
@audited(trust) @local fn trust<T>(x: Untrusted<T>, because: Str) -> T {}
@audited(as_task) @local fn as_task(x: Untrusted<Str>, because: Str) -> Brief {}
@audited(declassify) @user @local fn declassify<T>(x: Secret<T>, because: Str) -> T {}
@local fn retry<T>(f: fn() -> T, attempts: Int = 3, backoff: Backoff = Backoff.exponential(1s, 2.0, 30s), on: [ErrorKind] = [ModelFailed, RateLimited, Timeout]) -> T {}
@pure fn timeout<T>(d: Duration, f: fn() -> T) -> T {}
@pure fn race<T>(fs: [fn() -> T]) -> T {}
@pure fn fallback<T>(fs: [fn() -> T], on: [ErrorKind] = [ModelFailed, RateLimited, Timeout, SchemaMismatch, Unsupported]) -> T {}
@pure fn par<T>(fs: [fn() -> T], limit: Int? = none) -> [T] {}
@pure fn par_map<T, U>(xs: [T], f: fn(T) -> U, limit: Int = 4) -> [U] {}
@pure fn par_map_settled<T, U>(xs: [T], f: fn(T) -> U, limit: Int = 4) -> [Result<U>] {}
@pure fn par_fold<T, U, A>(xs: [T], f: fn(T) -> U, init: A, step: fn(A, Int, Result<U>) -> Fold<A>, limit: Int = 4, order: FoldOrder = index) -> Folded<A> {}
fn rate_limit(key: Str, n: Int, per: Duration) -> () {}
fn cached<T>(key: Str, f: fn() -> T, scope: CacheScope = turn, ttl: Duration? = none) -> T {}
@pure fn memo<A, R>(f: fn(A) -> R) -> fn(A) -> R {}
fn once<T>(key: Str, f: fn() -> T) -> T {}
@pure fn ensure<T>(v: T?, message: Str) -> T {}
@local fn cancelled() -> Bool {}
@local fn budget_left() -> Budget {}
@local fn spent() -> Spent {}
"#,
    ),
    (
        "list",
        r"
@pure fn len<T>(xs: [T]) -> Int {}
@pure fn get<T>(xs: [T], i: Int) -> T? {}
@pure fn first<T>(xs: [T]) -> T? {}
@pure fn push<T>(xs: [T], x: T) -> [T] {}
@pure fn map<T, U>(xs: [T], f: fn(T) -> U) -> [U] {}
@pure fn filter<T>(xs: [T], f: fn(T) -> Bool) -> [T] {}
@pure fn filter_map<T, U>(xs: [T], f: fn(T) -> U?) -> [U] {}
@pure fn fold<T, A>(xs: [T], init: A, f: fn(A, T) -> A) -> A {}
@pure fn contains<T>(xs: [T], x: T) -> Bool {}
@pure fn sort_by<T, K>(xs: [T], key: fn(T) -> K, descending: Bool = false) -> [T] {}
@pure fn take<T>(xs: [T], n: Int) -> [T] {}
@pure fn sum<Num>(xs: [Num]) -> Num {}
// stage 8: the rest of `list` (chapter 06, data)
@pure fn is_empty<T>(xs: [T]) -> Bool {}
@pure fn last<T>(xs: [T]) -> T? {}
@pure fn insert<T>(xs: [T], i: Int, x: T) -> [T] {}
@pure fn remove_at<T>(xs: [T], i: Int) -> [T] {}
@pure fn concat<T>(xs: [T], ys: [T]) -> [T] {}
@pure fn flatten<T>(xss: [[T]]) -> [T] {}
@pure fn range(start: Int, end: Int, step: Int = 1) -> [Int] {}
@pure fn repeat_<T>(x: T, n: Int) -> [T] {}
@pure fn map_indexed<T, U>(xs: [T], f: fn(Int, T) -> U) -> [U] {}
@pure fn flat_map<T, U>(xs: [T], f: fn(T) -> [U]) -> [U] {}
@pure fn reduce<T>(xs: [T], f: fn(T, T) -> T) -> T? {}
@pure fn find<T>(xs: [T], f: fn(T) -> Bool) -> T? {}
@pure fn find_index<T>(xs: [T], f: fn(T) -> Bool) -> Int? {}
@pure fn index_of<T>(xs: [T], x: T) -> Int? {}
@pure fn any<T>(xs: [T], f: fn(T) -> Bool) -> Bool {}
@pure fn all<T>(xs: [T], f: fn(T) -> Bool) -> Bool {}
@pure fn count<T>(xs: [T], f: fn(T) -> Bool) -> Int {}
@pure fn min<T>(xs: [T]) -> T? {}
@pure fn max<T>(xs: [T]) -> T? {}
@pure fn min_by<T, K>(xs: [T], key: fn(T) -> K) -> T? {}
@pure fn max_by<T, K>(xs: [T], key: fn(T) -> K) -> T? {}
@pure fn sort<T>(xs: [T]) -> [T] {}
@pure fn reverse<T>(xs: [T]) -> [T] {}
@pure fn unique<T>(xs: [T]) -> [T] {}
@pure fn unique_by<T, K>(xs: [T], key: fn(T) -> K) -> [T] {}
@pure fn group_by<T, K>(xs: [T], key: fn(T) -> K) -> Map<K, [T]> {}
@pure fn partition<T>(xs: [T], f: fn(T) -> Bool) -> ([T], [T]) {}
@pure fn chunk<T>(xs: [T], size: Int) -> [[T]] {}
@pure fn window<T>(xs: [T], size: Int) -> [[T]] {}
@pure fn zip<A, B>(xs: [A], ys: [B]) -> [(A, B)] {}
@pure fn unzip<A, B>(ps: [(A, B)]) -> ([A], [B]) {}
@pure fn enumerate<T>(xs: [T]) -> [(Int, T)] {}
@pure fn drop<T>(xs: [T], n: Int) -> [T] {}
@pure fn take_while<T>(xs: [T], f: fn(T) -> Bool) -> [T] {}
@pure fn slice<T>(xs: [T], start: Int, end: Int? = none) -> [T] {}
",
    ),
    (
        "text",
        r#"
@pure fn len<S>(s: S) -> Int {}
@pure fn is_empty<S>(s: S) -> Bool {}
@pure fn lower<S>(s: S) -> S {}
@pure fn upper<S>(s: S) -> S {}
@pure fn trim<S>(s: S) -> S {}
@pure fn trim_start<S>(s: S) -> S {}
@pure fn trim_end<S>(s: S) -> S {}
@pure fn contains<S>(s: S, part: S) -> Bool {}
@pure fn starts_with<S>(s: S, prefix: S) -> Bool {}
@pure fn ends_with<S>(s: S, suffix: S) -> Bool {}
@pure fn find<S>(s: S, part: S, from: Int = 0) -> Int? {}
@pure fn rfind<S>(s: S, part: S) -> Int? {}
@pure fn count<S>(s: S, part: S) -> Int {}
@pure fn slice<S>(s: S, start: Int, end: Int? = none) -> S {}
@pure fn truncate<S>(s: S, max: Int, marker: Str = "…") -> S {}
@pure fn split<S>(s: S, sep: S, limit: Int? = none) -> [S] {}
@pure fn split_once<S>(s: S, sep: S) -> (S, S)? {}
@pure fn lines<S>(s: S) -> [S] {}
@pure fn words<S>(s: S) -> [S] {}
@pure fn chars<S>(s: S) -> [S] {}
@pure fn join<S>(parts: [S], sep: S = "") -> S {}
@pure fn replace<S>(s: S, from: S, to: S) -> S {}
@pure fn replace_first<S>(s: S, from: S, to: S) -> S {}
@pure fn repeat_<S>(s: S, n: Int) -> S {}
@pure fn reverse<S>(s: S) -> S {}
@pure fn pad_start<S>(s: S, width: Int, fill: Str = " ") -> S {}
@pure fn pad_end<S>(s: S, width: Int, fill: Str = " ") -> S {}
@pure fn before<S>(s: S, sep: S) -> S {}
@pure fn after<S>(s: S, sep: S) -> S {}
@pure fn before_last<S>(s: S, sep: S) -> S {}
@pure fn after_last<S>(s: S, sep: S) -> S {}
@pure fn strip_prefix<S>(s: S, prefix: S) -> S? {}
@pure fn strip_suffix<S>(s: S, suffix: S) -> S? {}
@pure fn trim_end_matches<S>(s: S, suffix: S) -> S {}
@pure fn dedent<S>(s: S) -> S {}
@pure fn indent<S>(s: S, prefix: Str = "  ") -> S {}
@pure fn wrap<S>(s: S, width: Int = 80) -> S {}
@pure fn number_lines<S>(s: S, start: Int = 1) -> S {}
@pure fn show<T>(v: T) -> Untrusted<Str> {}
@pure fn parse_int<S>(s: S) -> Int? {}
@pure fn parse_float<S>(s: S) -> Float? {}
@pure fn similarity<S>(a: S, b: S) -> Float {}
@pure fn extract_json<S>(s: S) -> Json? {}
@pure fn code_blocks<S>(s: S, lang: Str? = none) -> [CodeBlock<S>] {}
@pure fn quote<S>(s: S, label: Str = "data") -> Untrusted<Str> {}
"#,
    ),
    (
        "re",
        r"
@pure fn is_match<S>(pattern: Str, s: S) -> Bool {}
@pure fn find<S>(pattern: Str, s: S) -> Match<S>? {}
@pure fn find_all<S>(pattern: Str, s: S, limit: Int = 1000) -> [Match<S>] {}
@pure fn captures<S>(pattern: Str, s: S) -> Captures<S>? {}
@pure fn captures_all<S>(pattern: Str, s: S, limit: Int = 1000) -> [Captures<S>] {}
@pure fn replace<S>(pattern: Str, s: S, with_: S) -> S {}
@pure fn replace_all<S>(pattern: Str, s: S, with_: S) -> S {}
@pure fn split<S>(pattern: Str, s: S) -> [S] {}
@pure fn escape(s: Str) -> Str {}
",
    ),
    (
        "map",
        r"
@pure fn get<K, V>(m: Map<K, V>, k: K) -> V? {}
@pure fn set<K, V>(m: Map<K, V>, k: K, v: V) -> Map<K, V> {}
@pure fn keys<K, V>(m: Map<K, V>) -> [K] {}
// stage 8: the rest of `map` (chapter 06, data)
@pure fn from<K, V>(pairs: [(K, V)]) -> Map<K, V> {}
@pure fn len<K, V>(m: Map<K, V>) -> Int {}
@pure fn is_empty<K, V>(m: Map<K, V>) -> Bool {}
@pure fn has<K, V>(m: Map<K, V>, k: K) -> Bool {}
@pure fn remove<K, V>(m: Map<K, V>, k: K) -> Map<K, V> {}
@pure fn update<K, V>(m: Map<K, V>, k: K, f: fn(V) -> V, default: V) -> Map<K, V> {}
@pure fn values<K, V>(m: Map<K, V>) -> [V] {}
@pure fn entries<K, V>(m: Map<K, V>) -> [(K, V)] {}
@pure fn merge<K, V>(a: Map<K, V>, b: Map<K, V>) -> Map<K, V> {}
@pure fn map_values<K, V, W>(m: Map<K, V>, f: fn(V) -> W) -> Map<K, W> {}
@pure fn filter<K, V>(m: Map<K, V>, f: fn(K, V) -> Bool) -> Map<K, V> {}
@pure fn count_by<T, K>(xs: [T], key: fn(T) -> K) -> Map<K, Int> {}
",
    ),
    (
        "json",
        r"
@pure fn parse<S>(s: S) -> Json {}
@pure @schema(T) fn decode<T>(j: Json) -> Untrusted<T> {}
@pure fn get(j: Json, pointer: Str) -> Json? {}
@pure @schema(T) fn encode<T>(v: T) -> Json {}
// stage 8: the rest of `json` (chapter 06, data)
@pure @schema(T) fn stringify<T>(v: T, pretty: Bool = false) -> Untrusted<Str> {}
@pure @schema(T) fn schema<T>() -> Json {}
@pure fn validate(j: Json, schema: Json) -> [Violation] {}
@pure fn set(j: Json, pointer: Str, v: Json) -> Json {}
@pure fn keys(j: Json) -> [Untrusted<Str>] {}
@pure fn type_of(j: Json) -> JsonType {}
@pure fn merge(a: Json, b: Json) -> Json {}
@pure fn patch(j: Json, ops: Json) -> Json {}
@pure fn diff(a: Json, b: Json) -> Json {}
@pure fn equal(a: Json, b: Json) -> Bool {}
",
    ),
    (
        "math",
        r"
@pure fn abs<Num>(x: Num) -> Num {}
@pure fn min<Num>(a: Num, b: Num) -> Num {}
@pure fn max<Num>(a: Num, b: Num) -> Num {}
",
    ),
    (
        "model",
        r"
@effect(model) fn generate(p: Prompt, opts: GenOpts = {}) -> Untrusted<Str> {}
@effect(model) @schema(T) fn generate_as<T>(p: Prompt, opts: GenOpts = {}) -> Untrusted<T> {}
@effect(model) fn classify<E>(input: Untrusted<Str>, opts: GenOpts = {}) -> Classified<Untrusted<E>> {}
@effect(model) fn complete(text: Str, opts: GenOpts = {}) -> Untrusted<Str> {}
@pure fn route(tier: Tier? = none, location: Location? = none, tags: [Str] = [], min_context: Int? = none) -> ModelRef {}
@effect(model) fn generate_n(p: Prompt, n: Int, opts: GenOpts = {}) -> [Untrusted<Str>] {}
@effect(model) fn stream(p: Prompt, on_text: fn(Untrusted<Str>) -> (), until: fn(Untrusted<Str>, Untrusted<Str>) -> Bool = none, opts: GenOpts = {}) -> Untrusted<Streamed> {}
@effect(model) fn logprobs(p: Prompt, opts: GenOpts = {}) -> Untrusted<TokenText> {}
@effect(model) fn chat(conv: Conversation, tools: ToolSet? = none, opts: GenOpts = {}) -> ModelReply {}
@effect(model) @schema(T) fn chat_as<T>(conv: Conversation, instructions: Prompt? = none, opts: GenOpts = {}) -> Untrusted<T> {}
@effect(model) @effect(any_tool) fn loop_(conv: Conversation, max_steps: Int = 50, tools: ToolSet? = none, until: fn(ModelReply) -> Bool = none, concurrent: Bool = true, opts: GenOpts = {}) -> LoopResult {}
@effect(model) fn choose(input: Untrusted<Str>, options: [Str], opts: GenOpts = {}) -> Chosen {}
@effect(model) @schema(T) fn extract<T>(input: Untrusted<Str>, what: Str? = none, opts: GenOpts = {}) -> Untrusted<T> {}
@effect(model) fn summarize(input: Untrusted<Str>, max_words: Int = 150, focus: Str? = none, opts: GenOpts = {}) -> Untrusted<Str> {}
@effect(model) fn rewrite(input: Untrusted<Str>, instruction: Str, opts: GenOpts = {}) -> Untrusted<Str> {}
@effect(model) fn translate(input: Untrusted<Str>, to: Str, opts: GenOpts = {}) -> Untrusted<Str> {}
@effect(model) fn judge(candidate: Untrusted<Str>, rubric: Str, opts: GenOpts = {}) -> Untrusted<Judgement> {}
@effect(model) fn score(candidate: Untrusted<Str>, criteria: [Criterion], opts: GenOpts = {}) -> Untrusted<Scores> {}
@effect(model) fn compare(a: Untrusted<Str>, b: Untrusted<Str>, criteria: Str, swap: Bool = true, opts: GenOpts = {}) -> Untrusted<Preference> {}
@effect(model) @secrecy_sink fn rank<T>(items: [T], show: fn(T) -> Untrusted<Str>, criteria: Str, method: RankMethod = listwise, opts: GenOpts = {}) -> [Scored<T>] {}
@effect(model) fn embed(texts: [Untrusted<Str>], model: ModelRef? = none) -> [Vector] {}
@local fn count_tokens<Countable>(x: Countable, model: ModelRef? = none) -> TokenCount {}
@local fn window(model: ModelRef? = none) -> Int? {}
@pure fn render(p: Prompt, model: ModelRef? = none) -> Transcript {}
@pure fn prompt_hash(p: Prompt) -> Str {}
@local fn resolve(m: ModelRef) -> ModelCard {}
@tool(list_models) @local fn list(query: Str? = none, location: Location? = none, limit: Int = 20) -> [ModelCard] {}
@local fn current() -> ModelCard {}
@local fn can_reason(m: ModelRef? = none) -> Bool {}
",
    ),
    (
        "tools",
        r#"
@tool(read_file) @local fn read_file(path: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> FileText {}
@effect(write) @tool(write_file) @secrecy_sink fn write_file(path: Untrusted<Str>, content: Untrusted<Str>) -> Written {}
@effect(write) @tool(edit_file) @secrecy_sink fn edit_file(path: Untrusted<Str>, old: Untrusted<Str>, new: Untrusted<Str>) -> Written {}
@effect(command) @tool(run_command) @secrecy_sink fn run_command(command: Untrusted<Str>, timeout: Duration = 120s, check: Bool = false) -> Untrusted<CommandResult> {}
@effect(command) @tool(run_command) @secrecy_sink fn run_background(command: Untrusted<Str>) -> Process {}
@effect(network) @tool(web_search) @secrecy_sink fn web_search(query: Untrusted<Str>, count: Int = 10) -> Untrusted<[SearchHit]> {}
@effect(network) @tool(web_fetch) @secrecy_sink fn web_fetch(url: Untrusted<Str>, offset: Int? = none, limit: Int? = none) -> Untrusted<Page> {}
@effect(network) @tool("mcp__*") @secrecy_sink fn mcp_call(server: Str, tool: Str, arguments: Json) -> Untrusted<McpResult> {}
@effect(any_tool) @secrecy_sink fn call_tool(name: Str, arguments: Json) -> Untrusted<ToolResult> {}
@effect(any_tool) fn dispatch(conv: Conversation, call: ToolCall) -> ToolResult {}
@tool(list_dir) @local fn list_dir(path: Untrusted<Str>, offset: Int = 0) -> Untrusted<Listing> {}
@tool(glob) @local fn glob(pattern: Str, path: Untrusted<Str>? = none, max_results: Int = 100, offset: Int = 0, include_ignored: Bool = false) -> Untrusted<Globbed> {}
@tool(grep) @local fn grep(pattern: Str, path: Untrusted<Str>? = none, glob: Str? = none, case_insensitive: Bool = false, context: Int = 0, max_results: Int = 100, offset: Int = 0, include_ignored: Bool = false) -> Untrusted<Grepped> {}
@tool(command_output) @local fn command_output(p: Process, since: Int? = none) -> Untrusted<ProcessOutput> {}
@tool(command_stop) @local fn command_stop(p: Process) -> () {}
@effect(network) @tool(browser_open) @secrecy_sink fn browser_open(url: Untrusted<Str>, new_tab: Bool = false) -> Untrusted<PageView> {}
@effect(network) @tool(browser_read) @secrecy_sink fn browser_read() -> Untrusted<PageView> {}
@effect(network) @tool(browser_click) @secrecy_sink fn browser_click(selector: Untrusted<Str>) -> Untrusted<PageView> {}
@effect(network) @tool(browser_type) @secrecy_sink fn browser_type(selector: Untrusted<Str>, text: Untrusted<Str>, submit: Bool = false) -> Untrusted<PageView> {}
@effect(network) @tool(browser_tabs) @secrecy_sink fn browser_tabs() -> Untrusted<[TabInfo]> {}
@effect(network) @tool(browser_switch) @secrecy_sink fn browser_switch(tab: Untrusted<Str>) -> Untrusted<PageView> {}
@effect(network) @tool(browser_close) @secrecy_sink fn browser_close(tab: Untrusted<Str>) -> () {}
@effect(network) @tool(browser_screenshot) @secrecy_sink fn browser_screenshot() -> Image {}
@effect(network) @effect(write) @tool(browser_download) @secrecy_sink fn browser_download(url: Untrusted<Str>, filename: Str? = none) -> Untrusted<Downloaded> {}
@effect(command) @tool(preview_open) @secrecy_sink fn preview_open(target: PreviewTarget) -> Preview {}
@tool(preview_close) fn preview_close() -> () {}
@effect(network) @effect(write) @tool(media_image) @secrecy_sink fn media_image(prompt_: Untrusted<Str>, width: Int? = none, height: Int? = none, name: Str? = none) -> MediaFile {}
@effect(network) @effect(write) @tool(media_speak) @secrecy_sink fn media_speak(text: Untrusted<Str>, voice: Str? = none, name: Str? = none) -> MediaFile {}
@effect(network) @tool(media_transcribe) @secrecy_sink fn media_transcribe(path: Untrusted<Str>) -> Untrusted<Str> {}
@local fn mcp_servers() -> [McpServerInfo] {}
@effect(network) @tool("mcp__*") @secrecy_sink fn mcp_resources(server: Str, cursor: Str? = none) -> Untrusted<McpListing> {}
@effect(network) @tool("mcp__*") @secrecy_sink fn mcp_read_resource(server: Str, uri: Untrusted<Str>) -> Untrusted<Str> {}
@effect(network) @tool("mcp__*") @secrecy_sink fn mcp_prompts(server: Str) -> Untrusted<McpListing> {}
@effect(network) @tool("mcp__*") @secrecy_sink fn mcp_get_prompt(server: Str, name: Str, arguments: Map<Str, Str> = {}) -> Transcript {}
@tool(tool_search) @local fn tool_search(query: Str? = none, names: [Str] = []) -> [ToolSchema] {}
fn refuse(conv: Conversation, call: ToolCall, reason: Str) -> ToolResult {}
@effect(any_tool) fn dispatch_all(conv: Conversation, calls: [ToolCall], concurrent: Bool = true) -> [ToolResult] {}
@local fn offered() -> [ToolSchema] {}
@local fn spec(name: Str) -> ToolSchema? {}
@local fn has(name: Str) -> Bool {}
@pure fn narrow(set: ToolSet, names: [Str]) -> ToolSet {}
@local fn skills() -> [SkillInfo] {}
@local fn plugins() -> [PluginInfo] {}
@tool(load_skill) @local fn load_skill(name: Str) -> Untrusted<Str> {}
@effect(write) @tool(activate_plugin) @secrecy_sink fn activate_plugin(name: Str) -> () {}
@effect(write) @tool(set_model_card) @secrecy_sink fn set_model_card(model: Str, card: Str?) -> () {}
@effect(write) @effect(network) @tool(install_extension) @secrecy_sink fn install_extension(kind: ExtensionKind, source: Str, path: Str? = none, revision: Str? = none) -> () {}
@effect(write) @tool(uninstall_extension) @secrecy_sink fn uninstall_extension(kind: ExtensionKind, name: Str) -> () {}
@tool(suggest_local_model) @secrecy_sink fn suggest_local_model(task: Str, needs: [Str], context_tokens: Int? = none) -> Untrusted<Str> {}
@effect(command) @tool(run_script) @secrecy_sink fn run_script(code: Untrusted<Str>) -> Untrusted<ScriptResult> {}
"#,
    ),
    (
        "session",
        r"
@user @secret_answers @local fn ask(questions: [Question]) -> [UserText] {}
@user @local fn request_approval(summary: Str, detail: Untrusted<Str>? = none) -> Bool {}
@secrecy_sink fn emit(name: Str, data: Json) -> () {}
@secrecy_sink fn notify(text: Untrusted<Str>, level: NoticeLevel = info) -> () {}
@secrecy_sink fn stream(text: Untrusted<Str>) -> () {}
@local fn plan() -> Untrusted<Plan>? {}
@tool(update_plan) fn update_plan(plan: Untrusted<Plan>) -> () {}
@local fn state() -> Untrusted<AidState> {}
@tool(update_state) fn update_state(change: AidStateChange) -> Untrusted<AidState> {}
@effect(write) @tool(session_checkpoint) @secrecy_sink fn checkpoint(label: Str? = none) -> Checkpoint {}
@user @effect(write) @tool(session_rewind_files) fn rewind_files(cp: Checkpoint) -> () {}
@tool(session_branch) @secrecy_sink fn branch(at_turn: Int? = none, title: Str? = none) -> SessionRef {}
@effect(delegate) @tool(start_session) @secrecy_sink fn start(prompt_: Str, environment: Str? = none, package: Str? = none, model: ModelRef? = none) -> SessionRef {}
@local fn id() -> Str {}
@local fn info() -> SessionInfo {}
@local fn turn() -> Turn {}
@local fn attended() -> Bool {}
@local fn mode() -> PermissionMode {}
@local fn folders() -> [Str] {}
@local fn scratch_folder() -> Str? {}
@local fn usage() -> Spent {}
@local fn queued() -> [UserText] {}
",
    ),
    (
        "context",
        r"
fn notice(conv: Conversation, text: Str, data: Untrusted<Str>? = none) -> () {}
@local fn current() -> Conversation {}
@local fn main() -> Conversation {}
fn new(system: TrustedPrompt? = none) -> Conversation {}
@local fn fork(conv: Conversation) -> Conversation {}
@local fn fork_at(conv: Conversation, mark: Mark) -> Conversation {}
fn rollback(conv: Conversation, mark: Mark) -> Int {}
fn append_user(conv: Conversation, content: Prompt) -> () {}
fn append_turn(conv: Conversation, turn: Turn) -> () {}
fn append_task(conv: Conversation, task: TaskInput) -> () {}
fn append_assistant(conv: Conversation, text: Untrusted<Str>) -> () {}
@tool(context_load) @local fn load(reference: Str) -> Untrusted<Str> {}
@local fn messages(conv: Conversation) -> Transcript {}
@local fn last(conv: Conversation, n: Int) -> Transcript {}
@local fn excerpt(conv: Conversation, from: Mark? = none, last_turns: Int? = none, roles: [Role] = [user, assistant, tool]) -> Transcript {}
@local fn mark(conv: Conversation) -> Mark {}
@local fn snapshot(conv: Conversation) -> Transcript {}
@local fn system(conv: Conversation) -> Prompt {}
@local fn system_default(role: AgentRole = root) -> TrustedPrompt {}
@effect(model) fn safe_point(conv: Conversation) -> SafePoint {}
@local fn pending(conv: Conversation) -> [Pending] {}
@effect(model) fn compact(conv: Conversation, keep_turns: Int = 2, focus: Str? = none) -> Compaction {}
fn clear_stale(conv: Conversation, older_than_turns: Int = 2) -> Int {}
fn pin(conv: Conversation, from: Mark, to: Mark? = none, reason: Str) -> Pin {}
fn unpin(pin: Pin) -> () {}
@local fn pins(conv: Conversation) -> [PinInfo] {}
@local fn labelled(conv: Conversation) -> Bool {}
@local fn tokens(conv: Conversation) -> TokenCount {}
@local fn window(conv: Conversation? = none) -> Int? {}
@local fn remaining(conv: Conversation) -> Int? {}
@local fn fits(conv: Conversation, extra: Prompt? = none) -> Bool {}
@local fn breakdown(conv: Conversation) -> Breakdown? {}
fn store(text: Untrusted<Str>, name: Str? = none) -> Artefact {}
@local fn read_artefact(a: Artefact, offset: Int = 0, limit: Int = 32000) -> Untrusted<Str> {}
@local fn chunks(a: Artefact, chunk_tokens: Int) -> [Untrusted<Str>] {}
",
    ),
    (
        "meta",
        r"
@loads fn harness_<H>(name: Str) -> H {}
@local fn harnesses(query: Str? = none) -> [HarnessInfo] {}
@local fn has_harness(name: Str) -> Bool {}
@pure fn signature<H>(h: H) -> Signature {}
@local fn package() -> PackageInfo {}
@local fn trust_sites() -> [Site] {}
@effect(write) @tool(write_harness) @secrecy_sink fn author(name: Str, source: Untrusted<Str>, description: Untrusted<Str>) -> AuthoredVersion {}
@local fn authored() -> [AuthoredVersion] {}
@pure fn site() -> Site {}
@local fn can(tool: Str, arguments: Json = {}) -> Decision {}
@local fn tools() -> [ToolInfo] {}
@local fn models() -> [ModelCard] {}
@local fn engine() -> EngineInfo {}
@pure fn ratio_version() -> Str {}
@local fn features() -> [Str] {}
",
    ),
    (
        "agents",
        r"
@effect(delegate) @tool(spawn_agent) @secrecy_sink fn spawn(brief: Untrusted<Str>, package: Str? = none, agent: Str? = none, model: ModelRef? = none, tier: Tier? = none, location: Location? = none, tags: [Str] = [], effort: Effort? = none, access: Access = read, knowledge: Str? = none, title: Str? = none, single_call: Bool = false, budget: Budget? = none) -> Task<Report> {}
@effect(delegate) @tool(steer_agent) @secrecy_sink fn steer(task: Task<Report>, text: Untrusted<Str>) -> () {}
@effect(delegate) @tool(queue_agent) @secrecy_sink fn queue(task: Task<Report>, text: Untrusted<Str>) -> () {}
@effect(delegate) @tool(ask_agent) @secrecy_sink fn ask(agent: Str, question: Untrusted<Str>, timeout: Duration = 300s) -> Untrusted<Str> {}
@effect(delegate) @tool(await_tasks) fn await(tasks: [Task<Report>], timeout: Duration? = none) -> [TaskReport] {}
@effect(delegate) @tool(await_tasks) fn join(task: Task<Report>, timeout: Duration? = none) -> TaskReport {}
@effect(delegate) @tool(spawn_agent) @secrecy_sink fn delegate(brief: Untrusted<Str>, package: Str? = none, opts: SpawnOpts = {}) -> TaskReport {}
@effect(delegate) @tool(spawn_agent) @secrecy_sink fn map(briefs: [Untrusted<Str>], package: Str? = none, limit: Int = 4, opts: SpawnOpts = {}) -> [TaskReport] {}
@effect(delegate) @tool(delegate_chunks) @secrecy_sink fn delegate_chunks(artefact: Artefact, chunk_tokens: Int, brief: Untrusted<Str>, reduce: Untrusted<Str>? = none) -> [Task<Report>] {}
@effect(delegate) @tool(resume_agent) @secrecy_sink fn resume(task: Task<Report>, text: Untrusted<Str>) -> Task<Report> {}
@effect(delegate) @tool(resume_agent) fn cancel(task: Task<Report>) -> () {}
@local fn status(task: Task<Report>) -> AgentSummary {}
@local fn worktree(task: Task<Report>) -> WorktreeInfo? {}
@effect(write) @tool(agent_merge) fn merge(task: Task<Report>) -> MergeResult {}
@local fn list() -> [AgentSummary] {}
@local fn current() -> AgentInfo {}
@local fn named() -> [AgentFileInfo] {}
@tool(list_knowledge_agents) @local fn knowledge(query: Str? = none) -> [KnowledgeCard] {}
@effect(delegate) @tool(keep_as_knowledge) @secrecy_sink fn keep_as_knowledge(task: Task<Report>, topic: Str) -> KnowledgeCard {}
@effect(delegate) @tool(keep_as_knowledge) fn retire_knowledge(task: Task<Report>) -> () {}
",
    ),
    (
        "log",
        r"
@schema(T) fn debug<T>(message: Untrusted<Str>, data: T? = none) -> () {}
@schema(T) fn info<T>(message: Untrusted<Str>, data: T? = none) -> () {}
@schema(T) fn warn<T>(message: Untrusted<Str>, data: T? = none) -> () {}
@schema(T) fn error<T>(message: Untrusted<Str>, data: T? = none) -> () {}
",
    ),
    (
        "trace",
        r"
fn span<T>(name: Str, f: fn() -> T) -> T {}
@schema(T) fn event<T>(name: Str, data: T) -> () {}
",
    ),
    (
        "state",
        r"
@state @local fn get<State>() -> State {}
@state fn set<State>(v: State) -> () {}
@state fn update<State>(f: fn(State) -> State) -> State {}
@state fn reset() -> () {}
",
    ),
    (
        "think",
        r"
@effect(model) @schema(A) fn steps<A>(p: Prompt, steps: [Str], visible: Bool = true, opts: GenOpts = {}) -> Stepped<A> {}
@pure fn scratchpad() -> Scratchpad {}
@pure fn note(pad: Scratchpad, text: Untrusted<Str>) -> () {}
@pure fn notes(pad: Scratchpad, last: Int? = none) -> Untrusted<Str> {}
@effect(model) fn step_back(question: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Str> {}
@pure fn until<T>(init: T, step: fn(T) -> T, stop: Stop, score: fn(T) -> Float = none) -> Iterated<T> {}
@effect(model) fn plan(goal: Untrusted<Str>, context: Prompt? = none, max_steps: Int = 8, opts: GenOpts = {}) -> Untrusted<Plan> {}
@effect(model) fn replan(plan: Untrusted<Plan>, step: Untrusted<Str>, reason: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Plan> {}
@effect(model) fn decompose(task: Untrusted<Str>, max_parts: Int = 6, opts: GenOpts = {}) -> Untrusted<[Subtask]> {}
@effect(model) fn hypothesize(observations: Untrusted<Str>, n: Int = 3, opts: GenOpts = {}) -> Untrusted<[Hypothesis]> {}
@effect(model) fn reflect(trace: Untrusted<Str>, outcome: Str, opts: GenOpts = {}) -> Untrusted<Reflection> {}
@effect(model) fn rubric(task: Untrusted<Str>, n: Int = 5, opts: GenOpts = {}) -> Untrusted<[Criterion]> {}
@effect(model) fn critique(work: Untrusted<Str>, rubric: Str, opts: GenOpts = {}) -> Untrusted<Critique> {}
@effect(model) fn revise(work: Untrusted<Str>, critique: Untrusted<Critique>, opts: GenOpts = {}) -> Untrusted<Str> {}
@pure fn refine<T>(draft: T, critique: fn(T) -> Untrusted<Critique>, revise: fn(T, Untrusted<Critique>) -> T, stop: Stop = { max_rounds: 3 }) -> Refined<T> {}
@effect(model) fn verify(claim: Untrusted<Str>, evidence: [Untrusted<Str>], opts: GenOpts = {}) -> Untrusted<Verification> {}
@pure fn check<T>(value: T, checks: [Check<T>]) -> CheckReport {}
@effect(model) fn counterexample(claim: Untrusted<Str>, context: Prompt? = none, opts: GenOpts = {}) -> Untrusted<Counterexample?> {}
@pure fn vote<T>(candidates: [T], key: fn(T) -> Untrusted<Str>) -> Vote<T> {}
@pure fn self_consistency<T>(n: Int, sample: fn() -> T, key: fn(T) -> Untrusted<Str>, parallel: Bool = true, early_stop: Bool = false) -> Vote<T> {}
@pure fn best_of<T>(n: Int, sample: fn() -> T, score: fn(T) -> Float, parallel: Bool = true, early_stop: Bool = false, good_enough: Float? = none) -> Scored<T> {}
@effect(model) @secrecy_sink fn consensus<T>(answers: [T], method: ConsensusMethod = majority, show: fn(T) -> Untrusted<Str> = text.show, opts: GenOpts = {}) -> Consensus<T> {}
@effect(model) fn reconcile(answers: [Untrusted<Str>], question: Untrusted<Str>, opts: GenOpts = {}) -> Untrusted<Str> {}
@pure fn debate(question: Untrusted<Str>, sides: [fn(DebateTurn) -> Untrusted<Str>], judge: fn(DebateRecord) -> Untrusted<Judgement>, rounds: Int = 2) -> DebateResult {}
@pure fn tree_search<N>(root: N, expand: fn(N) -> [N], score: fn(N) -> Float, is_goal: fn(N) -> Bool, strategy: SearchStrategy = best_first, beam: Int? = none, max_nodes: Int = 64, max_depth: Int = 6) -> SearchResult<N> {}
@pure fn beam_search<N>(init: [N], expand: fn(N) -> [N], score: fn(N) -> Float, width: Int = 3, depth: Int = 4) -> [Scored<N>] {}
@pure fn mcts<N>(root: N, expand: fn(N) -> [N], rollout: fn(N) -> Float, iterations: Int = 32, exploration: Float = 1.4) -> SearchResult<N> {}
@effect(model) fn confidence(question: Untrusted<Str>, answer: Untrusted<Str>, method: ConfidenceMethod = self_report, n: Int = 5, opts: GenOpts = {}) -> Confidence {}
@pure fn uncertainty(keys: [Untrusted<Str>]) -> Float {}
@user fn clarify(missing_parameter: Str, options: [Untrusted<Str>], question: Str? = none, confidence: Float? = none) -> UserText? {}
",
    ),
    (
        "memory",
        r"
@tool(memory_search) @secrecy_sink fn search(query: Untrusted<Str>, scope: MemoryScope? = none, kind: MemoryType? = none, limit: Int = 8) -> Untrusted<[MemoryHit]> {}
@tool(memory_read) fn read(name: Untrusted<Str>, scope: MemoryScope? = none) -> Untrusted<MemoryEntry> {}
@effect(write) @tool(memory_remember) @secrecy_sink fn remember(entry: NewMemory) -> () {}
@effect(write) @tool(memory_edit) @secrecy_sink fn edit(name: Untrusted<Str>, old: Untrusted<Str>, new: Untrusted<Str>, scope: MemoryScope? = none) -> () {}
@effect(write) @tool(memory_forget) @secrecy_sink fn forget(name: Untrusted<Str>, scope: MemoryScope? = none) -> () {}
@secrecy_sink fn propose(entry: NewMemory) -> () {}
@local fn index(scope: MemoryScope = global) -> Untrusted<[IndexLine]> {}
@local fn environment() -> Untrusted<Str> {}
@tool(memory_search) @secrecy_sink fn recall(text: Untrusted<Str>, limit: Int = 5) -> Untrusted<[MemoryHit]> {}
@pure fn links(entry: Untrusted<MemoryEntry>) -> [Untrusted<Str>] {}
@local fn used() -> [Untrusted<Str>] {}
@tool(search_sessions) @secrecy_sink fn search_sessions(query: Untrusted<Str>, archived: Bool = false, limit: Int = 10) -> Untrusted<[SessionHit]> {}
",
    ),
    (
        "eval",
        r"
@effect(command) @tool(run_command) @secrecy_sink fn run_tests(command: Str, format: TestFormat = auto, timeout: Duration = 600s) -> Untrusted<TestReport> {}
@secrecy_sink fn metric(name: Str, value: Float, tags: Map<Str, Str> = {}) -> () {}
@secrecy_sink fn score(name: Str, value: Float, reason: Untrusted<Str>? = none) -> () {}
@secrecy_sink fn label(key: Str, value: Str) -> () {}
@secrecy_sink fn expect(cond: Bool, name: Str) -> Bool {}
fn timer<T>(name: Str, f: fn() -> T) -> T {}
@local fn task() -> EvalTask? {}
@local fn is_headless() -> Bool {}
@local fn fixture(name: Str) -> Json {}
@local fn mode() -> TapeMode {}
@local fn tape() -> TapeInfo? {}
@effect(write) @secrecy_sink fn golden(name: Str, value: Untrusted<Str>) -> Bool {}
@pure fn script_model(replies: [ScriptedReply]) -> () {}
@pure fn script_tool(name: Str, replies: [ScriptedToolReply]) -> () {}
",
    ),
    (
        "rand",
        r"
@local fn int(lo: Int, hi: Int) -> Int {}
@local fn float() -> Float {}
@local fn bool(p: Float = 0.5) -> Bool {}
@local fn choice<T>(xs: [T]) -> T {}
@local fn sample<T>(xs: [T], k: Int) -> [T] {}
@local fn shuffle<T>(xs: [T]) -> [T] {}
@local fn uuid() -> Str {}
",
    ),
    (
        "time",
        r#"
@local fn now() -> Instant {}
@local fn since(t: Instant) -> Duration {}
@local fn sleep(d: Duration) -> () {}
@local fn today(tz: Str = "local") -> Date {}
@pure fn add(t: Instant, d: Duration) -> Instant {}
@pure fn diff(a: Instant, b: Instant) -> Duration {}
@pure fn format(t: Instant, fmt: Str = "%Y-%m-%dT%H:%M:%SZ", tz: Str = "UTC") -> Str {}
@pure fn parse<S>(s: S, fmt: Str = "%Y-%m-%dT%H:%M:%S%z") -> Instant? {}
@pure fn date(t: Instant, tz: Str = "UTC") -> Date {}
@pure fn duration(seconds: Float) -> Duration {}
@pure fn seconds(d: Duration) -> Float {}
@pure fn show(d: Duration) -> Str {}
"#,
    ),
];

/// The conversions chapter 03 audits ("Conversion"): each call is a trust site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrustKind {
    /// `trust(x, because:)`: untrusted to trusted.
    Trust,
    /// `as_task(x, because:)`: untrusted text to a `Brief`.
    AsTask,
    /// `declassify(x, because:)`: removes a secrecy label.
    Declassify,
}

impl TrustKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            TrustKind::Trust => "trust",
            TrustKind::AsTask => "as_task",
            TrustKind::Declassify => "declassify",
        }
    }
}

/// What a built-in's attributes in the table say about it, beyond its signature. User
/// declarations have the default (nothing).
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "independent attributes")]
pub(crate) struct Traits {
    /// `@audited(kind)`: a trust site.
    pub(crate) audited: Option<TrustKind>,
    /// `@user`: its result is the person's answer, not model or tool output (for the
    /// guarded-effect lint).
    pub(crate) user: bool,
    /// `@effect(kind)`: the kinds of effect it makes; a built-in without `@pure` or
    /// `@effect` makes effects of an unknown kind ([`Effects::OTHER`]).
    pub(crate) effects: Effects,
    /// `@tool(name)`: the tools it runs.
    pub(crate) tools: Vec<String>,
    /// `@secrecy_sink`: no argument may hold a statically secret value (chapter 03's secrecy
    /// sinks), whatever its parameter's type.
    pub(crate) secrecy_sink: bool,
    /// `@schema(T)`: these type parameters must be schema-able.
    pub(crate) schema: Vec<String>,
    /// `@secret_answers`: `session.ask`, whose answers are `Secret` unless every question is
    /// written out without `secret: true`.
    pub(crate) secret_answers: bool,
    /// `@loads`: `meta.harness`, which loads a harness by name.
    pub(crate) loads: bool,
    /// `@local`: its effects keep its arguments on the machine, so it is no secrecy sink.
    ///
    /// Review every use of `@local` in the table against this: a `@local` built-in must not
    /// hand any argument (or anything computed from one) to a model, the network, a command,
    /// another agent, or persisted or published state (files, memory, events, the journal
    /// beyond a digest). Marking such a built-in `@local` would let a statically secret value
    /// reach it through a generic function unrefused.
    pub(crate) local: bool,
    /// `@state`: `state.get`, `set`, `update` and `reset`, whose type parameter is the package's
    /// declared `state` type, fixed at each call (chapter 03, "Persistent state").
    pub(crate) state: bool,
}

impl Traits {
    /// The traits the attributes of a built-in declaration give; an attribute the table does
    /// not define (a bug in the table) is pushed to `internal`.
    pub(crate) fn of(attrs: &[ratio_syntax::ast::Attribute], internal: &mut Vec<String>) -> Self {
        let mut t = Traits::default();
        let mut pure = false;
        for a in attrs {
            let words: Vec<String> = a
                .args
                .iter()
                .flatten()
                .filter_map(|arg| word(&arg.value))
                .collect();
            match (a.name.name.as_str(), words.as_slice()) {
                ("pure", []) => pure = true,
                ("user", []) => t.user = true,
                ("secrecy_sink", []) => t.secrecy_sink = true,
                ("secret_answers", []) => t.secret_answers = true,
                ("loads", []) => t.loads = true,
                ("local", []) => t.local = true,
                ("state", []) => t.state = true,
                ("audited", [kind]) => {
                    t.audited = match kind.as_str() {
                        "trust" => Some(TrustKind::Trust),
                        "as_task" => Some(TrustKind::AsTask),
                        "declassify" => Some(TrustKind::Declassify),
                        _ => {
                            internal.push(format!("unknown audited kind `{kind}`"));
                            None
                        }
                    };
                }
                ("effect", [kind]) => match Effects::named(kind) {
                    Some(e) => t.effects = t.effects.union(e),
                    None => internal.push(format!("unknown effect kind `{kind}`")),
                },
                ("tool", [name]) => t.tools.push(name.clone()),
                ("schema", names) if !names.is_empty() => t.schema.extend(names.iter().cloned()),
                (other, _) => internal.push(format!("unknown built-in attribute `@{other}`")),
            }
        }
        if !pure && t.effects.is_empty() {
            t.effects = Effects::OTHER;
        }
        if t.secrecy_sink || (!pure && !t.local) {
            t.effects = t.effects.union(Effects::SINK);
        }
        t
    }

    /// Why the built-in may not be used as a function value, if it may not: every rule its
    /// attributes impose must also hold for a value, and these are enforced only where it is
    /// called (the call's arguments, its literal name, its written questions, its type
    /// arguments at a generic secrecy sink). `None` when every rule also holds for a value:
    /// effects, tools and purity count where a value is named, `@user` only relaxes the lint,
    /// a secrecy sink without type parameters refuses secrets by its parameter types, and
    /// `@schema(T)` is checked where the value is named.
    pub(crate) fn value_refusal(&self, generic: bool) -> Option<&'static str> {
        if self.audited.is_some() {
            Some("each call is an audited trust site at one known type")
        } else if self.loads {
            Some("each load must be listed by the name it is called with")
        } else if self.state {
            Some("its type is the package's declared `state` type, fixed at each call")
        } else if self.secret_answers {
            Some("its answers are labelled from the questions written at the call")
        } else if self.secrecy_sink && generic {
            Some("it is a secrecy sink with type parameters, checked at each call")
        } else {
            None
        }
    }
}

/// An attribute argument as a word: a name, or a string literal without interpolations.
fn word(e: &ratio_syntax::ast::Expr) -> Option<String> {
    use ratio_syntax::ast::{ExprKind, Lit, StrPart};
    match &e.kind {
        ExprKind::Name(id) => Some(id.name.clone()),
        ExprKind::Lit(Lit::Str(s)) => {
            let mut out = String::new();
            for part in &s.parts {
                match part {
                    StrPart::Text(x) => out.push_str(x),
                    StrPart::Interp { .. } => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// A built-in's name as the table spells it, without the trailing `_` of a keyword.
pub(crate) fn builtin_name(name: &str) -> &str {
    name.strip_suffix('_').unwrap_or(name)
}

/// The module a method call on a value of a type looks in first (chapter 03, "Modules and
/// imports": `x.f(a)` resolves to the module of `x`'s type first).
pub(crate) fn method_module(types: &crate::types::Types, t: TypeId) -> Option<&'static str> {
    use crate::types::Ty;
    match types.get(t) {
        Ty::Str | Ty::UserText | Ty::Brief | Ty::UntrustedStr => Some("text"),
        Ty::List(_) => Some("list"),
        Ty::Map(..) => Some("map"),
        Ty::Json => Some("json"),
        Ty::Int | Ty::Float | Ty::Duration | Ty::Money => Some("math"),
        _ => None,
    }
}

/// The bound a built-in's type parameter has by its name.
pub(crate) fn bound(name: &str) -> Bound {
    match name {
        "S" => Bound::Text,
        "Num" => Bound::Num,
        "Countable" => Bound::Countable,
        _ => Bound::None,
    }
}

/// A primitive type name: what it is and how many type arguments it takes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Primitive {
    Plain(TypeId),
    Handle(Handle),
    /// `Map<K, V>`.
    Map,
    /// `Untrusted<T>`.
    Untrusted,
    /// `Secret<T>`.
    Secret,
    /// `Task<T>`, the handle (the shared record is `TaskInput`).
    Task,
}

impl Primitive {
    pub(crate) fn arity(self) -> usize {
        match self {
            Primitive::Plain(_) | Primitive::Handle(_) => 0,
            Primitive::Untrusted | Primitive::Secret | Primitive::Task => 1,
            Primitive::Map => 2,
        }
    }
}

/// The primitive type called `name`.
pub(crate) fn primitive(name: &str) -> Option<Primitive> {
    let plain = match name {
        "Never" => TypeId::NEVER,
        "Bool" => TypeId::BOOL,
        "Int" => TypeId::INT,
        "Float" => TypeId::FLOAT,
        "Duration" => TypeId::DURATION,
        "Money" => TypeId::MONEY,
        "Str" => TypeId::STR,
        "UserText" => TypeId::USER_TEXT,
        "Brief" => TypeId::BRIEF,
        "Json" => TypeId::JSON,
        "Image" => TypeId::IMAGE,
        "Prompt" => TypeId::PROMPT,
        "TrustedPrompt" => TypeId::TRUSTED_PROMPT,
        "Transcript" => TypeId::TRANSCRIPT,
        "ToolName" => TypeId::TOOL_NAME,
        "ToolSet" => TypeId::TOOL_SET,
        "Map" => return Some(Primitive::Map),
        "Untrusted" => return Some(Primitive::Untrusted),
        "Secret" => return Some(Primitive::Secret),
        "Task" => return Some(Primitive::Task),
        _ => {
            return Handle::ALL
                .iter()
                .find(|h| h.name() == name)
                .map(|h| Primitive::Handle(*h));
        }
    };
    Some(Primitive::Plain(plain))
}

// ---- frames (stage 10) ----
//
// The frame built-ins and the shared types `meta` reports with. Entries here are merged into the
// module of the same name above (and these types into the prelude), so this block can grow
// without touching the tables other stages extend.

/// The shared types of the frame built-ins (chapter 06, `meta`). Their shapes are provisional:
/// chapter 06 names them without spelling out every field.
pub(crate) const FRAME_PRELUDE: &str = r"
type FrameInfo = {
  name: Str, title: Str, depth: Int, path: Str, model: ModelRef, effort?: Effort,
  mode: PermissionMode, location: Location, attended: Bool, context: ContextMode, tools: Int,
}
type CallerInfo = { name: Str, site: Site }
type FrameLimits = { budget: Budget, depth: Int, width: Int, heap: Int }
type PermissionView = { mode: PermissionMode, location: Location, paths?: [Str], extra_rules: [Rule], tools: [Str] }
type Rule = { action: RuleAction, rule: Str }
enum RuleAction { deny, ask }
";

/// The frame built-ins, by module. `meta`'s readers answer from the frame alone (`@local`: they
/// send nothing anywhere); they are not `@pure`, since what they return depends on the frame.
pub(crate) const FRAME_MODULES: &[(&str, &str)] = &[
    (
        "control",
        r"
@pure fn question(text: Untrusted<Str>, options: [Untrusted<Str>] = []) -> Error {}
",
    ),
    (
        "meta",
        r"
@local fn frame() -> FrameInfo {}
@local fn caller() -> CallerInfo? {}
@local fn depth() -> Int {}
@local fn limits() -> FrameLimits {}
@local fn permissions() -> PermissionView {}
",
    ),
];
