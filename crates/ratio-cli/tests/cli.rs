//! The `ratio` binary, run as a person or a tool would run it: exit statuses, the two output
//! forms, package discovery, the registry's refusals, the conformance corpora (whose headers
//! say what must be reported) and the example harnesses of chapter 09.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ratio(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ratio"))
        .args(args)
        .current_dir(repo())
        .output()
        .expect("the ratio binary runs")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A fresh scratch folder for one test, under Cargo's temporary directory for this target.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("ratio-cli")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// One diagnostic of `--json` output.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Diag {
    file: String,
    line: u32,
    column: u32,
    severity: String,
    code: String,
    message: String,
}

/// The value of `"key":` in one JSON object line written by `ratio check --json`.
fn field(line: &str, key: &str) -> Option<String> {
    let start = line.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &line[start..];
    if let Some(body) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    } else {
        Some(rest.chars().take_while(char::is_ascii_digit).collect())
    }
}

fn diags(o: &Output) -> Vec<Diag> {
    stdout(o)
        .lines()
        .map(|l| {
            assert!(
                l.starts_with('{') && l.ends_with('}'),
                "not a JSON object: {l}"
            );
            Diag {
                file: field(l, "file").unwrap(),
                line: field(l, "line").map_or(0, |n| n.parse().unwrap()),
                column: field(l, "column").map_or(0, |n| n.parse().unwrap()),
                severity: field(l, "severity").unwrap(),
                code: field(l, "code").unwrap(),
                message: field(l, "message").unwrap(),
            }
        })
        .collect()
}

#[test]
fn version_and_help_describe_the_tool() {
    let o = ratio(&["--version"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        stdout(&o),
        format!(
            "ratio {} (spec draft {}, stage {})\n",
            ratio::VERSION,
            ratio::SPEC_DRAFT,
            ratio::STAGE
        )
    );
    let o = ratio(&["help"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("ratio check [--json] <path>..."));
    assert!(stdout(&o).contains("ratio lint"));
    let o = ratio(&["help", "check"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("--json"));
}

#[test]
fn usage_errors_exit_with_two() {
    for args in [
        &[][..],
        &["frobnicate"],
        &["check"],
        &["check", "--colour"],
        &["lint", "x.ratio"],
        &["run", "x.ratio"],
        &["help", "frobnicate"],
    ] {
        let o = ratio(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(stdout(&o).is_empty(), "{args:?}");
    }
    let o = ratio(&["check", "conformance/no-such-file.ratio"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("no-such-file.ratio: no such file or folder"));
    let o = ratio(&["check", "README.md"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("is not a `.ratio` file"));
}

#[test]
fn a_clean_file_exits_with_zero_and_prints_nothing() {
    let o = ratio(&["check", "conformance/taint/ok-module-tools.ratio"]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    assert_eq!(stdout(&o), "");
    assert!(stderr(&o).contains("1 package: 0 errors, 0 warnings"));
    // Warnings alone are clean.
    let o = ratio(&["check", "conformance/taint/warn-model-guard-stdlib.ratio"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains(": warning C0039: "));
}

#[test]
fn errors_print_position_code_message_and_a_caret() {
    let o = ratio(&["check", "conformance/types/err-state-undeclared.ratio"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stdout(&o),
        "conformance/types/err-state-undeclared.ratio:7:11: error C0001: `state.get` needs a \
         `state` declaration in the package, and it has none\n\
         7 |   let s = state.get()\n  |           ^^^^^^^^^^^\n"
    );
    let o = ratio(&[
        "check",
        "--json",
        "conformance/types/err-state-undeclared.ratio",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stdout(&o),
        "{\"file\":\"conformance/types/err-state-undeclared.ratio\",\"line\":7,\"column\":11,\
         \"end_line\":7,\"end_column\":22,\"severity\":\"error\",\"code\":\"C0001\",\
         \"message\":\"`state.get` needs a `state` declaration in the package, and it has none\"}\n"
    );
}

/// The `// expect:` header of a conformance entry, as `(file, code, line, col)`.
fn expected(dir: &str, entry: &Path) -> Vec<(String, String, u32, u32)> {
    let main = if entry.is_dir() {
        entry.join("main.ratio")
    } else {
        entry.to_path_buf()
    };
    let shown = |file: &str| {
        let name = entry.file_name().unwrap().to_string_lossy();
        if entry.is_dir() {
            format!("conformance/{dir}/{name}/{file}")
        } else {
            format!("conformance/{dir}/{name}")
        }
    };
    let text = fs::read_to_string(&main).unwrap();
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// expect: ") else {
            if line.starts_with("// ") {
                continue;
            }
            break;
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let (code, at, file) = match words.as_slice() {
            [_, code, "at", at] => (*code, *at, "main.ratio"),
            [_, code, "at", at, "in", file] => (*code, *at, *file),
            _ => continue,
        };
        let (l, c) = at.split_once(':').unwrap();
        out.push((
            shown(file),
            code.to_owned(),
            l.parse().unwrap(),
            c.parse().unwrap(),
        ));
    }
    out
}

#[test]
fn the_cli_reports_what_the_checker_corpora_expect() {
    for dir in ["types", "taint"] {
        let root = repo().join("conformance").join(dir);
        let mut entries: Vec<PathBuf> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() || p.extension().is_some_and(|e| e == "ratio"))
            .collect();
        entries.sort();
        let mut want = Vec::new();
        let mut args = vec!["check".to_owned(), "--json".to_owned()];
        for e in &entries {
            want.extend(expected(dir, e));
            args.push(format!(
                "conformance/{dir}/{}",
                e.file_name().unwrap().to_string_lossy()
            ));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = ratio(&args);
        // The headers list the checker's diagnostics; the loader's warnings (`N`) are extra.
        let mut got: Vec<(String, String, u32, u32)> = diags(&o)
            .into_iter()
            .filter(|d| !(d.code.starts_with('N') && d.severity == "warning"))
            .map(|d| (d.file, d.code, d.line, d.column))
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "conformance/{dir}: {}", stderr(&o));
        let errors = want.iter().any(|(_, code, _, _)| {
            ratio::checker::Code::from_code(code).is_none_or(|c| !c.is_warning())
        });
        assert_eq!(o.status.code(), Some(i32::from(errors)));
    }
}

#[test]
fn a_folder_is_a_package_by_its_main_or_its_one_meta_file() {
    let dir = scratch("folders");
    let pkg = dir.join("helper");
    fs::create_dir_all(pkg.join("lib")).unwrap();
    fs::write(
        pkg.join("entry.ratio"),
        "meta { name: \"helper\", version: \"1.0.0\", description: \"d\" }\n\
         import \"./lib/util.ratio\" as util\n\
         pub fn twice(x: Int) -> Int { util.add(x, x) }\n",
    )
    .unwrap();
    fs::write(
        pkg.join("lib/util.ratio"),
        "pub fn add(a: Int, b: Int) -> Int { a + b }\n",
    )
    .unwrap();
    fs::write(pkg.join("notes.ratio"), "fn unused() -> Int { 1 }\n").unwrap();
    let path = pkg.to_string_lossy().into_owned();
    let o = ratio(&["check", &path]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));

    // Two candidates and no main.ratio: the person must name one.
    fs::write(
        pkg.join("other.ratio"),
        "meta { name: \"other\", version: \"1.0.0\", description: \"d\" }\n",
    )
    .unwrap();
    let o = ratio(&["check", &path]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("several .ratio files with a `meta` block"));
    let entry = pkg.join("entry.ratio").to_string_lossy().into_owned();
    let o = ratio(&["check", &entry]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));

    // A path import may not leave the source root.
    fs::write(pkg.join("main.ratio"), "import \"../outside.ratio\" as o\n").unwrap();
    fs::write(dir.join("outside.ratio"), "pub fn f() -> Int { 1 }\n").unwrap();
    let o = ratio(&["check", "--json", &path]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        diags(&o).iter().all(|d| d.code.starts_with('N')),
        "{}",
        stdout(&o)
    );
}

#[test]
fn packages_named_together_import_each_other_and_std_is_not_found() {
    let dir = scratch("together");
    fs::create_dir_all(dir.join("critics")).unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(
        dir.join("critics/main.ratio"),
        "pub fn strict(x: Untrusted<Str>) -> Untrusted<Str> { x }\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/main.ratio"),
        "import user:critics\npub fn run(x: Untrusted<Str>) -> Untrusted<Str> { critics.strict(x) }\n",
    )
    .unwrap();
    let app = dir.join("app").to_string_lossy().into_owned();
    let critics = dir.join("critics").to_string_lossy().into_owned();
    let o = ratio(&["check", &app, &critics]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    // Alone, the import is not found: a diagnostic, not an I/O error.
    let o = ratio(&["check", "--json", &app]);
    assert_eq!(o.status.code(), Some(1));
    assert!(diags(&o).iter().any(|d| d.code.starts_with('N')));

    fs::write(
        dir.join("app/main.ratio"),
        "import std:debate\nfn f() -> Int { 1 }\n",
    )
    .unwrap();
    let o = ratio(&["check", "--json", &app]);
    assert_eq!(o.status.code(), Some(1));
    let found = diags(&o);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].code.starts_with('N') && found[0].line == 1,
        "{found:?}"
    );
}

#[cfg(unix)]
#[test]
fn links_are_refused() {
    use std::os::unix::fs::symlink;
    let dir = scratch("links");
    let pkg = dir.join("pkg");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(dir.join("secret.ratio"), "pub fn f() -> Int { 1 }\n").unwrap();
    symlink(dir.join("secret.ratio"), pkg.join("lib.ratio")).unwrap();
    fs::write(
        pkg.join("main.ratio"),
        "import \"./lib.ratio\" as lib\nfn g() -> Int { lib.f() }\n",
    )
    .unwrap();
    let path = pkg.to_string_lossy().into_owned();
    let o = ratio(&["check", "--json", &path]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        diags(&o)
            .iter()
            .any(|d| d.code.starts_with('N') && d.message.contains("link")),
        "{}",
        stdout(&o)
    );
    // A hard link is a link too.
    fs::remove_file(pkg.join("lib.ratio")).unwrap();
    fs::hard_link(dir.join("secret.ratio"), pkg.join("lib.ratio")).unwrap();
    let o = ratio(&["check", "--json", &path]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    // A package root that is a link is refused before anything is read.
    symlink(&pkg, dir.join("alias")).unwrap();
    let alias = dir.join("alias").to_string_lossy().into_owned();
    let o = ratio(&["check", &alias]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn a_file_over_the_size_limit_is_refused_without_reading_it() {
    let dir = scratch("large");
    let max = ratio::names::Limits::default().max_file_bytes;
    let mut text = String::from("fn f() -> Int { 1 }\n");
    text.push_str(&"// padding\n".repeat(max / 10));
    fs::write(dir.join("big.ratio"), &text).unwrap();
    let path = dir.join("big.ratio").to_string_lossy().into_owned();
    let o = ratio(&["check", "--json", &path]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        diags(&o).iter().all(|d| d.code.starts_with('N')),
        "{}",
        stdout(&o)
    );
}

/// The diagnostics chapter 09's examples are known to give, each a spec erratum or a warning
/// a reviewer is meant to see: `(example, line in the example, code)`. Examples are numbered by
/// their order in `spec/09-examples.md`.
const EXAMPLE_FINDINGS: &[(usize, u32, &str, &str)] = &[
    // `std:plan_execute_verify`: the loop runs what a model-written plan says (by design).
    (
        1,
        69,
        "C0039",
        "model.loop guarded by a model-written plan's step status",
    ),
    (
        1,
        70,
        "C0039",
        "a harness call guarded by a model-written plan's step status",
    ),
    // `std:self_consistency`: `let n = math.clamp(n, 1, 9)` shadows the parameter.
    (2, 21, "C0031", "a shadowed binding"),
    // `tree_of_thought`: `eval.label`'s value is `Str`, `text.show` gives `Untrusted<Str>`.
    (
        4,
        19,
        "C0035",
        "erratum: eval.label(key, text.show(found.goal_reached))",
    ),
    // `std:consul_escalation`: dispatching the calls the model made (the agent loop itself).
    (
        6,
        55,
        "C0039",
        "tools.dispatch_all of a model's calls under a model-derived guard",
    ),
    // `std:tdd_fix`: `eval.metric`'s tags are `Map<Str, Str>`, `text.show` is untrusted.
    (
        7,
        38,
        "C0035",
        "erratum: tags: { attempt: text.show(attempt) }",
    ),
];

/// Whether a diagnostic of an example comes from a built-in a later stage adds (stage 8's
/// `json`, `list`, `map`; stage 9's `math` to `codec`): a missing signature, a missing list
/// method, or a closure whose parameter type would have come from one.
fn pending(d: &Diag, list_methods: &BTreeSet<String>) -> bool {
    const LATER: &[&str] = &[
        "json", "list", "map", "math", "hash", "diff", "template", "vec", "path", "url", "codec",
    ];
    match d.code.as_str() {
        "C0034" => LATER.iter().any(|m| d.message.contains(&format!("`{m}."))),
        "C0005" => list_methods
            .iter()
            .any(|m| d.message.ends_with(&format!("has no method `{m}`"))),
        _ => false,
    }
}

#[test]
fn the_examples_of_chapter_09_check_but_for_later_stages_and_listed_errata() {
    let chapter = fs::read_to_string(repo().join("spec/09-examples.md")).unwrap();
    let dir = scratch("examples");
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in chapter.lines() {
        match &mut current {
            None if line.starts_with("```ratio") => current = Some(String::new()),
            Some(text) if line.starts_with("```") => {
                blocks.push(std::mem::take(text));
                current = None;
            }
            Some(text) => {
                text.push_str(line);
                text.push('\n');
            }
            None => {}
        }
    }
    assert_eq!(blocks.len(), 8, "chapter 09 has eight example packages");
    let mut args = vec!["check".to_owned(), "--json".to_owned()];
    for (i, b) in blocks.iter().enumerate() {
        let path = dir.join(format!("example-{}.ratio", i + 1));
        fs::write(&path, b).unwrap();
        args.push(path.to_string_lossy().into_owned());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = ratio(&args);
    // The `list` built-ins whose signatures stage 8 adds (methods on lists resolve to them).
    let signed: BTreeSet<String> = ratio::checker::builtin_rules()
        .into_iter()
        .map(|b| b.name)
        .collect();
    let list_methods: BTreeSet<String> =
        fs::read_to_string(repo().join("conformance/builtins.txt"))
            .unwrap()
            .lines()
            .filter_map(|l| l.split('\t').next())
            .filter(|n| n.starts_with("list.") && !signed.contains(*n))
            .map(|n| n.trim_start_matches("list.").to_owned())
            .collect();
    let all = diags(&run);
    let number = |d: &Diag| -> usize {
        let name = Path::new(&d.file)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        name.trim_start_matches("example-").parse().unwrap()
    };
    let waiting: BTreeSet<usize> = all
        .iter()
        .filter(|d| pending(d, &list_methods))
        .map(number)
        .collect();
    let mut unexplained = Vec::new();
    let mut listed: BTreeSet<(usize, u32, &str)> = BTreeSet::new();
    for d in &all {
        let example = number(d);
        if pending(d, &list_methods) {
            continue;
        }
        // A closure whose parameter type a pending built-in would have given.
        if d.code == "C0014" && waiting.contains(&example) {
            continue;
        }
        match EXAMPLE_FINDINGS
            .iter()
            .find(|(e, line, code, _)| *e == example && *line == d.line && *code == d.code)
        {
            Some((e, line, code, _)) => {
                listed.insert((*e, *line, *code));
            }
            None => unexplained.push(format!(
                "example {example} line {}: {} {}",
                d.line, d.code, d.message
            )),
        }
    }
    assert!(unexplained.is_empty(), "{}", unexplained.join("\n"));
    for (e, l, c, why) in EXAMPLE_FINDINGS {
        assert!(
            listed.contains(&(*e, *l, *c)),
            "example {e} line {l} no longer reports {c} ({why}): update the list"
        );
    }
}

/// Whether `s` holds a character that would control a terminal or reorder text: a C0 control
/// other than tab and newline, DEL, a C1 control, or a bidirectional override or isolate.
fn hostile(s: &str) -> bool {
    s.chars().any(|c| {
        (c < ' ' && c != '\t' && c != '\n')
            || ('\u{7f}'..='\u{9f}').contains(&c)
            || ('\u{202a}'..='\u{202e}').contains(&c)
            || ('\u{2066}'..='\u{2069}').contains(&c)
    })
}

#[test]
fn control_characters_in_a_package_never_reach_the_terminal() {
    let dir = scratch("controls");
    // In a source line shown under a diagnostic: OSC 52 (write the clipboard) and a bidi override.
    fs::write(
        dir.join("line.ratio"),
        "fn f() -> Str { 1 } // \u{1b}]52;c;aGk=\u{7} \u{202e}\u{9b}2J\n",
    )
    .unwrap();
    // In an import path, which the loader quotes.
    fs::write(
        dir.join("import.ratio"),
        "import \"./\u{1b}[2J\u{1b}]0;owned\u{7}.ratio\" as x\nfn g() -> Int { 1 }\n",
    )
    .unwrap();
    for name in ["line.ratio", "import.ratio"] {
        let path = dir.join(name).to_string_lossy().into_owned();
        let o = ratio(&["check", &path]);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", stdout(&o));
        assert!(!hostile(&stdout(&o)), "{name}: {:?}", stdout(&o));
        assert!(!hostile(&stderr(&o)), "{name}: {:?}", stderr(&o));
        assert!(stdout(&o).contains(r"\u{1b}"), "{name}: {:?}", stdout(&o));
        // JSON escapes its own.
        let o = ratio(&["check", "--json", &path]);
        assert!(!stdout(&o).chars().any(|c| c < ' ' && c != '\n'), "{name}");
    }
    // In a path on the command line, echoed on standard error.
    let o = ratio(&["check", "missing\u{1b}]8;;http://x\u{7}.ratio"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!hostile(&stderr(&o)), "{:?}", stderr(&o));
    let o = ratio(&["frobnicate\u{1b}[2J"]);
    assert!(!hostile(&stderr(&o)), "{:?}", stderr(&o));
}

/// A file name holding an escape sequence (Unix allows it; Windows does not).
#[cfg(unix)]
#[test]
fn control_characters_in_a_file_name_never_reach_the_terminal() {
    let dir = scratch("control-name");
    let file = dir.join("a\u{1b}[2J\u{1b}]52;c;aGk=\u{7}.ratio");
    fs::write(&file, "fn f() -> Str { 1 }\n").unwrap();
    let path = file.to_string_lossy().into_owned();
    let o = ratio(&["check", &path]);
    assert!(!hostile(&stdout(&o)), "{:?}", stdout(&o));
    assert!(!hostile(&stderr(&o)), "{:?}", stderr(&o));
}

/// Junctions (and, where the account may make them, symbolic links) are refused on Windows: a
/// junction inside the package, and a package root that is one. `mklink /J` needs no privilege;
/// if `cmd` cannot make one the case is skipped with a note (check by hand: make a junction
/// `pkg\lib` to a folder outside and import `./lib/x.ratio`; `ratio check pkg` must report a link).
#[cfg(windows)]
#[test]
fn junctions_and_reparse_points_are_refused() {
    let dir = scratch("junctions");
    let outside = dir.join("outside");
    let pkg = dir.join("pkg");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&pkg).unwrap();
    fs::write(outside.join("x.ratio"), "pub fn f() -> Int { 1 }\n").unwrap();
    fs::write(
        pkg.join("main.ratio"),
        "import \"./lib/x.ratio\" as x\nfn g() -> Int { x.f() }\n",
    )
    .unwrap();
    let junction = |link: &Path, target: &Path| {
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !junction(&pkg.join("lib"), &outside) {
        eprintln!("skipped: `mklink /J` is not available here");
        return;
    }
    let path = pkg.to_string_lossy().into_owned();
    let o = ratio(&["check", "--json", &path]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        diags(&o)
            .iter()
            .any(|d| d.code.starts_with('N') && d.message.contains("link")),
        "{}",
        stdout(&o)
    );
    // A package root that is a junction is refused before anything is read.
    assert!(junction(&dir.join("alias"), &pkg));
    let alias = dir.join("alias").to_string_lossy().into_owned();
    let o = ratio(&["check", &alias]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    // A file symbolic link, where this account may make one.
    if std::os::windows::fs::symlink_file(outside.join("x.ratio"), pkg.join("y.ratio")).is_ok() {
        fs::write(
            pkg.join("main.ratio"),
            "import \"./y.ratio\" as y\nfn g() -> Int { y.f() }\n",
        )
        .unwrap();
        let o = ratio(&["check", "--json", &path]);
        assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    }
}
