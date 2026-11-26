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
    "json", "list", "map", "math", "hash", "diff", "template", "vec", "path", "url", "codec",
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
