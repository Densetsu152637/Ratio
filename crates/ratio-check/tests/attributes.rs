//! The security attributes of the built-in table, checked against chapter 06
//! (`conformance/builtins.txt`) and against lists reviewed by hand.
//!
//! - `@pure` exactly where chapter 06's Needs column says `pure` (or `as f`, `as callbacks`:
//!   a combinator whose only effects are its callbacks', counted where they are written);
//! - `@tool(...)` exactly the column's `tool:` names (pseudo-tools without their italics), with
//!   the MCP rows' `tool:mcp__<server>__…` written as the pattern `mcp__*`;
//! - `@local` and `@secrecy_sink` exactly the pinned lists below, so adding or removing one fails
//!   here until the change is reviewed against `Traits::local` and chapter 03's sinks;
//! - every parameter type as chapter 06 writes it, but for the listed, documented differences.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// The modules whose table entries later stages complete (their Needs are all `pure`).
const PENDING: &[&str] = &[
    "math", "hash", "diff", "template", "vec", "path", "url", "codec",
];

/// Reviewed against `Traits::local`: nothing a program gives these reaches a model, the network,
/// a command, another agent or persisted state. Most take no data at all (no argument, or only
/// handles, numbers and enums); the rest take only trusted `Str` (which no generic value can
/// become) or are local reads following `tools.read_file`; `rand`, the clock and `retry`'s waits
/// compute on the machine.
const LOCAL: &[&str] = &[
    "trust",
    "as_task",
    "declassify",
    "retry",
    "cancelled",
    "budget_left",
    "spent",
    "model.count_tokens",
    "model.window",
    "model.resolve",
    "model.list",
    "model.current",
    "model.can_reason",
    "tools.read_file",
    "tools.list_dir",
    "tools.glob",
    "tools.grep",
    "tools.command_output",
    "tools.command_stop",
    "tools.mcp_servers",
    "tools.tool_search",
    "tools.offered",
    "tools.spec",
    "tools.has",
    "tools.skills",
    "tools.plugins",
    "tools.load_skill",
    "session.ask",
    "session.request_approval",
    "session.plan",
    "session.state",
    "session.id",
    "session.info",
    "session.turn",
    "session.attended",
    "session.mode",
    "session.folders",
    "session.scratch_folder",
    "session.usage",
    "session.queued",
    "context.current",
    "context.main",
    "context.fork",
    "context.fork_at",
    "context.load",
    "context.messages",
    "context.last",
    "context.excerpt",
    "context.mark",
    "context.snapshot",
    "context.system",
    "context.system_default",
    "context.pending",
    "context.pins",
    "context.labelled",
    "context.tokens",
    "context.window",
    "context.remaining",
    "context.fits",
    "context.breakdown",
    "context.read_artefact",
    "context.chunks",
    "meta.harnesses",
    "meta.has_harness",
    "meta.package",
    "meta.trust_sites",
    "meta.authored",
    "meta.frame",
    "meta.caller",
    "meta.depth",
    "meta.limits",
    "meta.permissions",
    "meta.can",
    "meta.tools",
    "meta.models",
    "meta.engine",
    "meta.features",
    "agents.status",
    "agents.worktree",
    "agents.list",
    "agents.current",
    "agents.named",
    "agents.knowledge",
    "state.get",
    "memory.index",
    "memory.environment",
    "memory.used",
    "eval.task",
    "eval.is_headless",
    "eval.fixture",
    "eval.mode",
    "eval.tape",
    "rand.int",
    "rand.float",
    "rand.bool",
    "rand.choice",
    "rand.sample",
    "rand.shuffle",
    "rand.uuid",
    "time.now",
    "time.since",
    "time.sleep",
    "time.today",
];

/// Chapter 03's secrecy sinks and everything else that sends its arguments off the machine, to
/// another agent, to the person or into persisted or published state. Generic ones
/// (`model.rank`, `think.consensus`) refuse a secret type argument at the call.
const SINKS: &[&str] = &[
    "model.rank",
    "tools.write_file",
    "tools.edit_file",
    "tools.run_command",
    "tools.run_background",
    "tools.web_search",
    "tools.web_fetch",
    "tools.mcp_call",
    "tools.call_tool",
    "tools.browser_open",
    "tools.browser_read",
    "tools.browser_click",
    "tools.browser_type",
    "tools.browser_tabs",
    "tools.browser_switch",
    "tools.browser_close",
    "tools.browser_screenshot",
    "tools.browser_download",
    "tools.preview_open",
    "tools.media_image",
    "tools.media_speak",
    "tools.media_transcribe",
    "tools.mcp_resources",
    "tools.mcp_read_resource",
    "tools.mcp_prompts",
    "tools.mcp_get_prompt",
    "tools.activate_plugin",
    "tools.set_model_card",
    "tools.install_extension",
    "tools.uninstall_extension",
    "tools.suggest_local_model",
    "tools.run_script",
    "session.emit",
    "session.notify",
    "session.stream",
    "session.checkpoint",
    "session.branch",
    "session.start",
    "meta.author",
    "agents.spawn",
    "agents.steer",
    "agents.queue",
    "agents.ask",
    "agents.delegate",
    "agents.map",
    "agents.delegate_chunks",
    "agents.resume",
    "agents.keep_as_knowledge",
    "think.consensus",
    "memory.search",
    "memory.remember",
    "memory.edit",
    "memory.forget",
    "memory.propose",
    "memory.recall",
    "memory.search_sessions",
    "eval.run_tests",
    "eval.metric",
    "eval.score",
    "eval.label",
    "eval.expect",
    "eval.golden",
];

/// Every built-in's kinds of effect ([`ratio_check::Effects::names`]), reviewed: what the
/// guarded-effect lint and `harnesses.check` see. `secrecy_sink` is set by `@secrecy_sink` and,
/// failing closed, by every effectful built-in that is not `@local`; `other` is an effect of no
/// listed kind. A built-in not listed makes no effect.
const EFFECTS: &[(&str, &str)] = &[
    ("trust", "other"),
    ("as_task", "other"),
    ("declassify", "other"),
    ("retry", "other"),
    ("rate_limit", "other secrecy_sink"),
    ("cached", "other secrecy_sink"),
    ("once", "other secrecy_sink"),
    ("cancelled", "other"),
    ("budget_left", "other"),
    ("spent", "other"),
    ("model.generate", "model secrecy_sink"),
    ("model.generate_as", "model secrecy_sink"),
    ("model.classify", "model secrecy_sink"),
    ("model.complete", "model secrecy_sink"),
    ("model.generate_n", "model secrecy_sink"),
    ("model.stream", "model secrecy_sink"),
    ("model.logprobs", "model secrecy_sink"),
    ("model.chat", "model secrecy_sink"),
    ("model.chat_as", "model secrecy_sink"),
    ("model.loop", "any_tool model secrecy_sink"),
    ("model.choose", "model secrecy_sink"),
    ("model.extract", "model secrecy_sink"),
    ("model.summarize", "model secrecy_sink"),
    ("model.rewrite", "model secrecy_sink"),
    ("model.translate", "model secrecy_sink"),
    ("model.judge", "model secrecy_sink"),
    ("model.score", "model secrecy_sink"),
    ("model.compare", "model secrecy_sink"),
    ("model.rank", "model secrecy_sink"),
    ("model.embed", "model secrecy_sink"),
    ("model.count_tokens", "other"),
    ("model.window", "other"),
    ("model.resolve", "other"),
    ("model.list", "other"),
    ("model.current", "other"),
    ("model.can_reason", "other"),
    ("tools.read_file", "other"),
    ("tools.write_file", "write secrecy_sink"),
    ("tools.edit_file", "write secrecy_sink"),
    ("tools.run_command", "command secrecy_sink"),
    ("tools.run_background", "command secrecy_sink"),
    ("tools.web_search", "network secrecy_sink"),
    ("tools.web_fetch", "network secrecy_sink"),
    ("tools.mcp_call", "network secrecy_sink"),
    ("tools.call_tool", "any_tool secrecy_sink"),
    ("tools.dispatch", "any_tool secrecy_sink"),
    ("tools.list_dir", "other"),
    ("tools.glob", "other"),
    ("tools.grep", "other"),
    ("tools.command_output", "other"),
    ("tools.command_stop", "other"),
    ("tools.browser_open", "network secrecy_sink"),
    ("tools.browser_read", "network secrecy_sink"),
    ("tools.browser_click", "network secrecy_sink"),
    ("tools.browser_type", "network secrecy_sink"),
    ("tools.browser_tabs", "network secrecy_sink"),
    ("tools.browser_switch", "network secrecy_sink"),
    ("tools.browser_close", "network secrecy_sink"),
    ("tools.browser_screenshot", "network secrecy_sink"),
    ("tools.browser_download", "write network secrecy_sink"),
    ("tools.preview_open", "command secrecy_sink"),
    ("tools.preview_close", "other secrecy_sink"),
    ("tools.media_image", "write network secrecy_sink"),
    ("tools.media_speak", "write network secrecy_sink"),
    ("tools.media_transcribe", "network secrecy_sink"),
    ("tools.mcp_servers", "other"),
    ("tools.mcp_resources", "network secrecy_sink"),
    ("tools.mcp_read_resource", "network secrecy_sink"),
    ("tools.mcp_prompts", "network secrecy_sink"),
    ("tools.mcp_get_prompt", "network secrecy_sink"),
    ("tools.tool_search", "other"),
    ("tools.refuse", "other secrecy_sink"),
    ("tools.dispatch_all", "any_tool secrecy_sink"),
    ("tools.offered", "other"),
    ("tools.spec", "other"),
    ("tools.has", "other"),
    ("tools.skills", "other"),
    ("tools.plugins", "other"),
    ("tools.load_skill", "other"),
    ("tools.activate_plugin", "write secrecy_sink"),
    ("tools.set_model_card", "write secrecy_sink"),
    ("tools.install_extension", "write network secrecy_sink"),
    ("tools.uninstall_extension", "write secrecy_sink"),
    ("tools.suggest_local_model", "other secrecy_sink"),
    ("tools.run_script", "command secrecy_sink"),
    ("session.ask", "other"),
    ("session.request_approval", "other"),
    ("session.emit", "other secrecy_sink"),
    ("session.notify", "other secrecy_sink"),
    ("session.stream", "other secrecy_sink"),
    ("session.plan", "other"),
    ("session.update_plan", "other secrecy_sink"),
    ("session.state", "other"),
    ("session.update_state", "other secrecy_sink"),
    ("session.checkpoint", "write secrecy_sink"),
    ("session.rewind_files", "write secrecy_sink"),
    ("session.branch", "other secrecy_sink"),
    ("session.start", "delegate secrecy_sink"),
    ("session.id", "other"),
    ("session.info", "other"),
    ("session.turn", "other"),
    ("session.attended", "other"),
    ("session.mode", "other"),
    ("session.folders", "other"),
    ("session.scratch_folder", "other"),
    ("session.usage", "other"),
    ("session.queued", "other"),
    ("context.notice", "other secrecy_sink"),
    ("context.current", "other"),
    ("context.main", "other"),
    ("context.new", "other secrecy_sink"),
    ("context.fork", "other"),
    ("context.fork_at", "other"),
    ("context.rollback", "other secrecy_sink"),
    ("context.append_user", "other secrecy_sink"),
    ("context.append_turn", "other secrecy_sink"),
    ("context.append_task", "other secrecy_sink"),
    ("context.append_assistant", "other secrecy_sink"),
    ("context.load", "other"),
    ("context.messages", "other"),
    ("context.last", "other"),
    ("context.excerpt", "other"),
    ("context.mark", "other"),
    ("context.snapshot", "other"),
    ("context.system", "other"),
    ("context.system_default", "other"),
    ("context.safe_point", "model secrecy_sink"),
    ("context.pending", "other"),
    ("context.compact", "model secrecy_sink"),
    ("context.clear_stale", "other secrecy_sink"),
    ("context.pin", "other secrecy_sink"),
    ("context.unpin", "other secrecy_sink"),
    ("context.pins", "other"),
    ("context.labelled", "other"),
    ("context.tokens", "other"),
    ("context.window", "other"),
    ("context.remaining", "other"),
    ("context.fits", "other"),
    ("context.breakdown", "other"),
    ("context.store", "other secrecy_sink"),
    ("context.read_artefact", "other"),
    ("context.chunks", "other"),
    ("meta.harness", "other secrecy_sink"),
    ("meta.harnesses", "other"),
    ("meta.has_harness", "other"),
    ("meta.package", "other"),
    ("meta.trust_sites", "other"),
    ("meta.author", "write secrecy_sink"),
    ("meta.authored", "other"),
    ("meta.frame", "other"),
    ("meta.caller", "other"),
    ("meta.depth", "other"),
    ("meta.limits", "other"),
    ("meta.permissions", "other"),
    ("meta.can", "other"),
    ("meta.tools", "other"),
    ("meta.models", "other"),
    ("meta.engine", "other"),
    ("meta.features", "other"),
    ("agents.spawn", "delegate secrecy_sink"),
    ("agents.steer", "delegate secrecy_sink"),
    ("agents.queue", "delegate secrecy_sink"),
    ("agents.ask", "delegate secrecy_sink"),
    ("agents.await", "delegate secrecy_sink"),
    ("agents.join", "delegate secrecy_sink"),
    ("agents.delegate", "delegate secrecy_sink"),
    ("agents.map", "delegate secrecy_sink"),
    ("agents.delegate_chunks", "delegate secrecy_sink"),
    ("agents.resume", "delegate secrecy_sink"),
    ("agents.cancel", "delegate secrecy_sink"),
    ("agents.status", "other"),
    ("agents.worktree", "other"),
    ("agents.merge", "write secrecy_sink"),
    ("agents.list", "other"),
    ("agents.current", "other"),
    ("agents.named", "other"),
    ("agents.knowledge", "other"),
    ("agents.keep_as_knowledge", "delegate secrecy_sink"),
    ("agents.retire_knowledge", "delegate secrecy_sink"),
    ("log.debug", "other secrecy_sink"),
    ("log.info", "other secrecy_sink"),
    ("log.warn", "other secrecy_sink"),
    ("log.error", "other secrecy_sink"),
    ("trace.span", "other secrecy_sink"),
    ("trace.event", "other secrecy_sink"),
    ("state.get", "other"),
    ("state.set", "other secrecy_sink"),
    ("state.update", "other secrecy_sink"),
    ("state.reset", "other secrecy_sink"),
    ("think.steps", "model secrecy_sink"),
    ("think.step_back", "model secrecy_sink"),
    ("think.plan", "model secrecy_sink"),
    ("think.replan", "model secrecy_sink"),
    ("think.decompose", "model secrecy_sink"),
    ("think.hypothesize", "model secrecy_sink"),
    ("think.reflect", "model secrecy_sink"),
    ("think.rubric", "model secrecy_sink"),
    ("think.critique", "model secrecy_sink"),
    ("think.revise", "model secrecy_sink"),
    ("think.verify", "model secrecy_sink"),
    ("think.counterexample", "model secrecy_sink"),
    ("think.consensus", "model secrecy_sink"),
    ("think.reconcile", "model secrecy_sink"),
    ("think.confidence", "model secrecy_sink"),
    ("think.clarify", "other secrecy_sink"),
    ("memory.search", "other secrecy_sink"),
    ("memory.read", "other secrecy_sink"),
    ("memory.remember", "write secrecy_sink"),
    ("memory.edit", "write secrecy_sink"),
    ("memory.forget", "write secrecy_sink"),
    ("memory.propose", "other secrecy_sink"),
    ("memory.index", "other"),
    ("memory.environment", "other"),
    ("memory.recall", "other secrecy_sink"),
    ("memory.used", "other"),
    ("memory.search_sessions", "other secrecy_sink"),
    ("eval.run_tests", "command secrecy_sink"),
    ("eval.metric", "other secrecy_sink"),
    ("eval.score", "other secrecy_sink"),
    ("eval.label", "other secrecy_sink"),
    ("eval.expect", "other secrecy_sink"),
    ("eval.timer", "other secrecy_sink"),
    ("eval.task", "other"),
    ("eval.is_headless", "other"),
    ("eval.fixture", "other"),
    ("eval.mode", "other"),
    ("eval.tape", "other"),
    ("eval.golden", "write secrecy_sink"),
    ("rand.int", "other"),
    ("rand.float", "other"),
    ("rand.bool", "other"),
    ("rand.choice", "other"),
    ("rand.sample", "other"),
    ("rand.shuffle", "other"),
    ("rand.uuid", "other"),
    ("time.now", "other"),
    ("time.since", "other"),
    ("time.sleep", "other"),
    ("time.today", "other"),
];

/// Parameter types the table writes differently from chapter 06, each for a documented reason
/// (`src/builtins.rs` and the crate README): `(built-in, parameter, the table's type)`.
const TYPE_DIFFERENCES: &[(&str, &str, &str)] = &[
    // `(fn(..) -> B)?` is an option of a one-element tuple; the table declares `fn(..) -> B = none`.
    (
        "model.stream",
        "until",
        "fn(Untrusted<Str>, Untrusted<Str>) -> Bool",
    ),
    ("model.loop", "until", "fn(ModelReply) -> Bool"),
    // `S` means text in the table; package state and search states use other names.
    ("state.set", "v", "Q"),
    ("state.update", "f", "fn(Q) -> Q"),
    ("think.tree_search", "root", "N"),
    ("think.tree_search", "expand", "fn(N) -> [N]"),
    ("think.tree_search", "score", "fn(N) -> Float"),
    ("think.tree_search", "is_goal", "fn(N) -> Bool"),
    ("think.beam_search", "init", "[N]"),
    ("think.beam_search", "expand", "fn(N) -> [N]"),
    ("think.beam_search", "score", "fn(N) -> Float"),
    ("think.mcts", "root", "N"),
    ("think.mcts", "expand", "fn(N) -> [N]"),
    ("think.mcts", "rollout", "fn(N) -> Float"),
];

/// Combinators chapter 06 writes `as f` that are not `@pure` here: `once` stores its result in
/// the session's journal, like `cached` with `session` scope (chapter 06, caching).
const STORES: &[&str] = &["once"];

fn module(name: &str) -> &str {
    name.split_once('.').map_or("control", |(m, _)| m)
}

/// `(signature, needs)` of every chapter 06 built-in.
fn chapter() -> BTreeMap<String, (String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/builtins.txt");
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0].to_owned(), (f[1].to_owned(), f[2].to_owned()))
        })
        .collect()
}

/// The parameter types of a chapter 06 signature, split at the top level.
fn param_types(signature: &str) -> Vec<(String, String)> {
    let open = signature.find('(').unwrap();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut parts = vec![String::new()];
    for c in signature[open + 1..].chars() {
        let current = parts.last_mut().unwrap();
        if quoted {
            quoted = c != '"';
            current.push(c);
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' | '[' | '{' | '<' => depth += 1,
            ')' if depth == 0 => break,
            ')' | ']' | '}' => depth -= 1,
            '>' if !current.ends_with('-') => depth -= 1,
            ',' if depth == 0 => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, rest) = p.split_once(':').unwrap();
            let ty = rest.split(" = ").next().unwrap().trim();
            (name.trim().to_owned(), ty.to_owned())
        })
        .collect()
}

/// The `tool:` names of a Needs cell, as the table spells them.
fn tools_needed(needs: &str) -> BTreeSet<String> {
    needs
        .split(',')
        .filter_map(|p| p.trim().strip_prefix("tool:"))
        .map(|t| {
            let t = t.trim_matches('*');
            if t.starts_with("mcp__<server>__") {
                "mcp__*".to_owned()
            } else {
                t.to_owned()
            }
        })
        .collect()
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn the_tables_attributes_are_chapter_06s_and_the_reviewed_lists() {
    let spec = chapter();
    let rules = ratio_check::builtin_rules();
    let mut problems = Vec::new();
    let mut local = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    for b in &rules {
        let has = |a: &str| b.attrs.iter().any(|x| x == a);
        if has("local") {
            local.insert(b.name.as_str());
        }
        if has("secrecy_sink") {
            sinks.insert(b.name.as_str());
        }
        let Some((signature, needs)) = spec.get(&b.name) else {
            problems.push(format!("{}: not in chapter 06", b.name));
            continue;
        };
        let combinator = needs.starts_with("as ")
            && !needs.contains("effect")
            && !STORES.contains(&b.name.as_str());
        if has("pure") != (needs == "pure" || combinator) {
            problems.push(format!(
                "{}: @pure is {}, Needs is `{needs}`",
                b.name,
                has("pure")
            ));
        }
        let pure_ok = needs == "pure" || combinator;
        if pure_ok && b.attrs.iter().any(|a| a == "effect" || a == "tool") {
            problems.push(format!("{}: pure in chapter 06 but has effects", b.name));
        }
        if !PENDING.contains(&module(&b.name)) {
            let ours: BTreeSet<String> = b.tools.iter().cloned().collect();
            let want = tools_needed(needs);
            if ours != want {
                problems.push(format!("{}: @tool {ours:?}, Needs has {want:?}", b.name));
            }
            let want_types = param_types(signature);
            for ((name, _), ty) in b.params.iter().zip(&b.param_types) {
                let Some((_, spec_ty)) = want_types.iter().find(|(n, _)| n == name) else {
                    continue;
                };
                let excused = TYPE_DIFFERENCES
                    .iter()
                    .find(|(f, p, _)| *f == b.name && p == name);
                match excused {
                    Some((_, _, table)) => {
                        if squash(ty) != squash(table) {
                            problems.push(format!("{}({name}): {ty}, listed as {table}", b.name));
                        }
                    }
                    None => {
                        if squash(ty) != squash(spec_ty) {
                            problems
                                .push(format!("{}({name}): {ty}, chapter 06 {spec_ty}", b.name));
                        }
                    }
                }
            }
        }
    }
    for b in &rules {
        let ours = b.effects.join(" ");
        let pinned = EFFECTS
            .iter()
            .find(|(n, _)| *n == b.name)
            .map_or("", |(_, e)| *e);
        if ours != pinned {
            problems.push(format!(
                "{}: effects `{ours}`, the reviewed list has `{pinned}`",
                b.name
            ));
        }
    }
    for (name, _) in EFFECTS {
        if !rules.iter().any(|b| b.name == *name) {
            problems.push(format!("EFFECTS lists `{name}`, which is not in the table"));
        }
    }
    let pinned_local: BTreeSet<&str> = LOCAL.iter().copied().collect();
    let pinned_sinks: BTreeSet<&str> = SINKS.iter().copied().collect();
    for n in local.symmetric_difference(&pinned_local) {
        problems.push(format!("@local on `{n}` differs from the reviewed list"));
    }
    for n in sinks.symmetric_difference(&pinned_sinks) {
        problems.push(format!(
            "@secrecy_sink on `{n}` differs from the reviewed list"
        ));
    }
    for (name, param, _) in TYPE_DIFFERENCES {
        let found = rules
            .iter()
            .any(|b| b.name == *name && b.params.iter().any(|(p, _)| p == param));
        if !found {
            problems.push(format!(
                "TYPE_DIFFERENCES lists {name}({param}), which does not exist"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
