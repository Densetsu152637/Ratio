//! `ratio`, the command-line tool for Ratio sources.
//!
//! `ratio check` parses, resolves, type-checks and taint-checks packages and prints their
//! diagnostics; `ratio --version` and `ratio help` describe the tool. `ratio lint` (stage L) and
//! `ratio run` are reserved for later stages. The arguments are parsed by hand: the tool has
//! no dependency beyond the `ratio` library.

mod check;
mod fsreg;
mod render;

use std::io::Write as _;
use std::process::ExitCode;

/// Clean, or only warnings.
const CLEAN: u8 = 0;
/// At least one error diagnostic.
const ERRORS: u8 = 1;
/// A usage error, or a path that names no package (I/O).
const USAGE: u8 = 2;

const USAGE_TEXT: &str = "\
ratio: the Ratio language tool

Usage:
  ratio check [--json] <path>...  Check .ratio packages: parse, names, types and taint
  ratio help [<command>]          Show this help, or a command's
  ratio --version                 Show the ratio-lang version, spec draft and stage

Exit status: 0 when clean (warnings allowed), 1 when there are errors, 2 for a usage error
or a path that cannot be read.

Planned: `ratio lint` (stage L) and `ratio run` (a later stage).
";

const CHECK_TEXT: &str = "\
Usage: ratio check [--json] <path>...

Checks each path as a `user:` package and prints its diagnostics (syntax P, names N, types and
taint C) as `file:line:col: severity CODE: message` with the source line and a caret.

A path is a .ratio file, whose folder is the package's source root, or a folder holding a
package, whose entry is main.ratio, else the only .ratio file there with a `meta` block.
Imports by path stay inside the source root; links are refused. Packages named together may
import one another as `user:<name>` (a folder's name, or a file's name without `.ratio`).
No `std:` packages ship yet, so a `std:` import is reported as not found.

Options:
  --json   One JSON object per diagnostic and line: file, line, column, end_line,
           end_column, severity, code, message.
  --       Every later argument is a path.

Exit status: 0 when clean (warnings allowed), 1 when there are errors, 2 for a usage error
or a path that cannot be read.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run(&args))
}

fn run(args: &[String]) -> u8 {
    let Some(first) = args.first() else {
        eprint!("{USAGE_TEXT}");
        return USAGE;
    };
    match first.as_str() {
        "check" => check_command(&args[1..]),
        "help" | "--help" | "-h" => help(args.get(1).map(String::as_str)),
        "--version" | "-V" | "version" => {
            say(&format!(
                "ratio {} (spec draft {}, stage {})\n",
                ratio::VERSION,
                ratio::SPEC_DRAFT,
                ratio::STAGE
            ));
            CLEAN
        }
        "lint" => not_yet("lint", "stage L"),
        "run" => not_yet("run", "a later stage"),
        other => {
            eprintln!("ratio: unknown command `{}`\n", render::clean(other));
            eprint!("{USAGE_TEXT}");
            USAGE
        }
    }
}

fn help(topic: Option<&str>) -> u8 {
    match topic {
        None => {
            say(USAGE_TEXT);
            CLEAN
        }
        Some("check") => {
            say(CHECK_TEXT);
            CLEAN
        }
        Some(cmd @ ("lint" | "run")) => {
            say(&format!(
                "`ratio {cmd}` is not implemented yet ({}).\n",
                if cmd == "lint" {
                    "stage L"
                } else {
                    "a later stage"
                }
            ));
            CLEAN
        }
        Some(other) => {
            eprintln!("ratio: no help for `{}`\n", render::clean(other));
            eprint!("{USAGE_TEXT}");
            USAGE
        }
    }
}

fn not_yet(cmd: &str, when: &str) -> u8 {
    eprintln!("ratio: `ratio {cmd}` is not implemented yet ({when})");
    USAGE
}

fn check_command(args: &[String]) -> u8 {
    let mut json = false;
    let mut paths: Vec<&str> = Vec::new();
    let mut only_paths = false;
    for a in args {
        match a.as_str() {
            _ if only_paths => paths.push(a),
            "--" => only_paths = true,
            "--json" => json = true,
            "-h" | "--help" => {
                say(CHECK_TEXT);
                return CLEAN;
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                eprintln!("ratio check: unknown option `{}`\n", render::clean(flag));
                eprint!("{CHECK_TEXT}");
                return USAGE;
            }
            path => paths.push(path),
        }
    }
    if paths.is_empty() {
        eprintln!("ratio check: no path given\n");
        eprint!("{CHECK_TEXT}");
        return USAGE;
    }
    let mut status = CLEAN;
    let mut targets = Vec::new();
    for p in paths {
        match check::target(p) {
            Ok(t) => targets.push(t),
            Err(problem) => {
                eprintln!(
                    "ratio check: {}: {}",
                    render::clean(&problem.path),
                    render::clean(&problem.reason)
                );
                status = USAGE;
            }
        }
    }
    let (findings, internal) = check::check_all(&targets);
    let mut out = std::io::stdout().lock();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for f in &findings {
        match f.severity {
            check::Severity::Error => errors += 1,
            check::Severity::Warning => warnings += 1,
        }
        let text = if json {
            format!("{}\n", render::json(f))
        } else {
            render::human(f)
        };
        if out.write_all(text.as_bytes()).is_err() {
            // The reader went away (a closed pipe): the status still says what was found.
            break;
        }
    }
    let _ = out.flush();
    for problem in &internal {
        eprintln!(
            "ratio check: internal error in the checker's built-in table: {}",
            render::clean(problem)
        );
    }
    if !json && !targets.is_empty() {
        eprintln!(
            "ratio check: {} package{}: {errors} error{}, {warnings} warning{}",
            targets.len(),
            plural(targets.len()),
            plural(errors),
            plural(warnings)
        );
    }
    if !internal.is_empty() {
        status = USAGE;
    } else if errors > 0 && status == CLEAN {
        status = ERRORS;
    }
    status
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Prints to standard output, ignoring a closed pipe.
fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}
