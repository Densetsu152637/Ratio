//! The conformance runner (chapter 10, "The conformance suite").
//!
//! Every `.ratio` file under `conformance/` starts with expectation lines. This stage runs the
//! ones about reading tokens:
//!
//! ```text
//! // expect: lex-ok                       (no diagnostics)
//! // expect: lex-error L0001 at 7:13      (one line per diagnostic, in order)
//! // tokens: kw:let ident:x = int:1 ;!    (the tokens, in the notation of TokenKind::render;
//! //                                       several `tokens:` lines join with a space)
//! // option: max_interpolation_depth 1    (a LexOptions field)
//! ```
//!
//! Stage 2 adds the parser's:
//!
//! ```text
//! // expect: parse-ok                     (no diagnostic from the reader or the parser)
//! // expect: parse-error P0001 at 3:9     (one line per diagnostic, in order; the reader's
//! //                                       diagnostics come first in the list when there are any)
//! ```
//!
//! A file with a parse expectation may also have `tree:` lines: the span-free dump of the tree
//! (`ratio_syntax::dump`, one line) split over any number of lines, which join with a space.
//!
//! ```text
//! // tree: (module - [(item [] [] - (fn f [] [] - (block [(expr (bin + (int 1) (int 2)))])))])
//! ```
//!
//! Files whose expectations are `ok`, `value`, `check-error`, `runtime-error`, `fixture` or, from
//! stage 3, `resolve-ok`, `resolve-error` and `resolve-warning` (run by `ratio-names`'s own
//! runner) belong to later stages; the runner skips their checks, but each must still parse without a diagnostic
//! (except `check-error` and `runtime-error` files that say otherwise with a parse expectation). The header ends at the first line that
//! does not start with `// `; every header line must be one of these directives, so a misspelt
//! one (`// token: …`) fails the file instead of being ignored.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ratio_syntax::{Code, LexOptions, dump, lex_with, parse_with};

#[derive(Default)]
struct Case {
    lex_ok: bool,
    errors: Vec<(String, u32, u32)>,
    parse_ok: bool,
    parse_errors: Vec<(String, u32, u32)>,
    tokens: Option<String>,
    tree: Option<String>,
    options: LexOptions,
    other_stage: bool,
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ratio") {
            out.push(path);
        }
    }
}

fn position(path: &Path, position: &str) -> Result<(u32, u32), String> {
    let (l, c) = position
        .split_once(':')
        .ok_or_else(|| format!("{}: bad position {position}", path.display()))?;
    let l = l
        .parse()
        .map_err(|_| format!("{}: bad line", path.display()))?;
    let c = c
        .parse()
        .map_err(|_| format!("{}: bad column", path.display()))?;
    Ok((l, c))
}

fn parse(path: &Path, text: &str) -> Result<Case, String> {
    let mut case = Case::default();
    let mut token_lines: Vec<&str> = Vec::new();
    let mut tree_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(expect) = rest.strip_prefix("expect: ") {
            let words: Vec<&str> = expect.split_whitespace().collect();
            match words.as_slice() {
                ["lex-ok"] => case.lex_ok = true,
                ["lex-error", code, "at", at] => {
                    let (l, c) = position(path, at)?;
                    case.errors.push(((*code).to_owned(), l, c));
                }
                ["parse-ok"] => case.parse_ok = true,
                ["parse-error", code, "at", at] => {
                    let (l, c) = position(path, at)?;
                    case.parse_errors.push(((*code).to_owned(), l, c));
                }
                [
                    "ok" | "value" | "check-error" | "runtime-error" | "resolve-ok"
                    | "resolve-error" | "resolve-warning",
                    ..,
                ] => case.other_stage = true,
                _ => {
                    return Err(format!(
                        "{}: unknown expectation `{expect}`",
                        path.display()
                    ));
                }
            }
        } else if let Some(tokens) = rest.strip_prefix("tokens: ") {
            token_lines.push(tokens.trim());
        } else if let Some(tree) = rest.strip_prefix("tree: ") {
            tree_lines.push(tree.trim());
        } else if rest.starts_with("fixture: ") {
            case.other_stage = true;
        } else if let Some(option) = rest.strip_prefix("option: ") {
            let option = option.trim();
            let (name, value) = option
                .split_once(' ')
                .ok_or_else(|| format!("{}: bad option `{option}`", path.display()))?;
            let value: usize = value
                .trim()
                .parse()
                .map_err(|_| format!("{}: bad option value `{value}`", path.display()))?;
            match name {
                "max_source_bytes" => case.options.max_source_bytes = value,
                "max_interpolation_depth" => case.options.max_interpolation_depth = value,
                "max_nesting" => case.options.max_nesting = value,
                "max_diagnostics" => case.options.max_diagnostics = value,
                _ => return Err(format!("{}: unknown option `{name}`", path.display())),
            }
        } else {
            return Err(format!(
                "{}: unknown header directive `{line}` (expect:, tokens:, tree:, option: or fixture:)",
                path.display()
            ));
        }
    }
    if !token_lines.is_empty() {
        case.tokens = Some(token_lines.join(" "));
    }
    if !tree_lines.is_empty() {
        case.tree = Some(tree_lines.join(" "));
    }
    Ok(case)
}

/// Runs the parser on one file and compares it with the file's `parse-ok` or `parse-error`
/// lines. The diagnostics compared are all of `parse_with`'s, the token reader's included.
fn check_parse(name: &str, text: &str, case: &Case, failures: &mut Vec<String>) {
    let parsed = parse_with(text, &case.options);
    let got: Vec<(String, u32, u32)> = parsed
        .diagnostics
        .iter()
        .map(|d| (d.code.as_str().to_owned(), d.span.line, d.span.col))
        .collect();
    if got != case.parse_errors {
        failures.push(format!(
            "{name}: parse diagnostics differ
  expected {:?}
  got      {:?}
  messages {:?}",
            case.parse_errors,
            got,
            parsed
                .diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
        ));
    }
    if let Some(expected) = &case.tree {
        let got = dump(&parsed.module);
        if got != *expected {
            failures.push(format!(
                "{name}: tree differs
  expected {expected}
  got      {got}"
            ));
        }
    }
}

/// Runs the token reader on one file and compares it with the file's `lex-*` and `tokens:` lines.
fn check_lex(name: &str, text: &str, case: &Case, failures: &mut Vec<String>) {
    let lexed = lex_with(text, &case.options);
    if let Err(problem) = support::check_invariants(text, &lexed) {
        failures.push(format!("{name}: invariant broken: {problem}"));
    }
    if let Err(problem) = support::check_insertions(&lexed) {
        failures.push(format!("{name}: insertion property broken: {problem}"));
    }
    let got: Vec<(String, u32, u32)> = lexed
        .diagnostics
        .iter()
        .map(|d| (d.code.as_str().to_owned(), d.span.line, d.span.col))
        .collect();
    if got != case.errors {
        failures.push(format!(
            "{name}: diagnostics differ\n  expected {:?}\n  got      {:?}\n  messages {:?}",
            case.errors,
            got,
            lexed
                .diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
        ));
    }
    if let Some(expected) = &case.tokens {
        let rendered = support::render(&lexed);
        if rendered != *expected {
            failures.push(format!(
                "{name}: tokens differ\n  expected {expected}\n  got      {rendered}"
            ));
        }
    }
}

#[test]
fn the_conformance_files_pass() {
    let root = support::conformance_root();
    let mut paths = Vec::new();
    files(&root, &mut paths);
    paths.sort();
    assert!(
        paths.len() >= 30,
        "expected the stage 1 corpus, found {} files",
        paths.len()
    );

    let mut failures = Vec::new();
    let mut run = 0usize;
    let mut skipped = 0usize;
    let mut parse_run = 0usize;
    let mut codes_seen: BTreeSet<String> = BTreeSet::new();
    for path in &paths {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let case = match parse(path, &text) {
            Ok(case) => case,
            Err(problem) => {
                failures.push(problem);
                continue;
            }
        };
        let has_parse = case.parse_ok || !case.parse_errors.is_empty();
        if case.tree.is_some() && !has_parse {
            failures.push(format!(
                "{name}: a `tree:` needs a parse-ok or parse-error expectation"
            ));
        }
        let has_lex = case.lex_ok || !case.errors.is_empty() || case.tokens.is_some();
        if has_parse {
            parse_run += 1;
            check_parse(&name, &text, &case, &mut failures);
            for (code, _, _) in &case.parse_errors {
                if Code::from_code(code).is_some_and(|c| !c.is_lexical()) {
                    codes_seen.insert(code.clone());
                }
            }
        } else if case.other_stage {
            // Later stages' files are programs: they have to read and parse cleanly.
            let parsed = parse_with(&text, &case.options);
            if !parsed.is_clean() {
                failures.push(format!(
                    "{name}: a later-stage file must parse cleanly, got {:?}",
                    parsed.diagnostics
                ));
            }
        }
        if !has_lex {
            if !has_parse && !case.other_stage {
                failures.push(format!(
                    "{name}: no expectation (lex-*, parse-* or later stage)"
                ));
            }
            skipped += 1;
            continue;
        }
        if !case.lex_ok && case.errors.is_empty() && case.tokens.is_none() {
            continue;
        }
        run += 1;
        check_lex(&name, &text, &case, &mut failures);
        for (code, _, _) in &case.errors {
            codes_seen.insert(code.clone());
        }
    }
    assert!(
        failures.is_empty(),
        "{} failure(s)\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(run >= 30, "only {run} lexer files ran ({skipped} skipped)");

    assert!(parse_run >= 20, "only {parse_run} parser files ran");

    // Every lexical code has a lex-error file and every parser code a parse-error file.
    for code in Code::ALL {
        // `P0010` is the parser's internal error (it could not run): no source triggers it.
        if code == Code::Internal {
            continue;
        }
        assert!(
            codes_seen.contains(code.as_str()),
            "no conformance file expects {code}"
        );
    }
}

#[test]
fn the_builtins_drift_check_is_still_a_stub() {
    // Chapter 10 generates `builtins.txt` from chapter 06 of the specification and fails CI when the
    // checked-in file differs. No stage before 7 has built-ins, so the file holds comments only;
    // when the generator lands, this test becomes the drift check.
    let text =
        fs::read_to_string(support::conformance_root().join("builtins.txt")).expect("builtins.txt");
    assert!(
        text.lines()
            .all(|line| line.trim().is_empty() || line.starts_with('#')),
        "builtins.txt has entries but no generator checks them yet"
    );
}
