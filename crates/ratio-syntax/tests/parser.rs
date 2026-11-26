//! Parser tests that are not conformance files: the specification's own examples, the nesting
//! limit and the stack, hostile input, error recovery and the rendering of diagnostics.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratio_syntax::ast::ItemKind;
use ratio_syntax::{Code, LexOptions, MAX_SOURCE_BYTES, dump, parse, parse_with, print};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn markdown_files() -> Vec<PathBuf> {
    let spec = repo_root().join("spec");
    let mut out = Vec::new();
    for dir in [spec.clone(), spec.join("06-stdlib")] {
        for entry in fs::read_dir(&dir).expect("spec directory").flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// `(file, line of the opening fence, text)` for every `ratio`-fenced block.
fn ratio_blocks() -> Vec<(String, usize, String)> {
    let mut blocks = Vec::new();
    for path in markdown_files() {
        let text = fs::read_to_string(&path).expect("markdown");
        let name = path
            .strip_prefix(repo_root().join("spec"))
            .unwrap()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let mut current: Option<(usize, String)> = None;
        for (index, line) in text.lines().enumerate() {
            match &mut current {
                None => {
                    if line.trim_end() == "```ratio" {
                        current = Some((index + 1, String::new()));
                    }
                }
                Some((start, body)) => {
                    if line.trim_end() == "```" {
                        blocks.push((name.clone(), *start, std::mem::take(body)));
                        current = None;
                    } else {
                        body.push_str(line);
                        body.push('\n');
                    }
                }
            }
        }
    }
    blocks
}

// ---- the specification's examples ----

/// Blocks that are statements, not a whole file: they are parsed inside `fn fragment() { … }`.
/// Listed so that a block that stops being a fragment is noticed rather than silently wrapped.
const FRAGMENTS: &[(&str, usize)] = &[
    ("03-semantics.md", 432),
    ("04-harnesses.md", 80),
    ("05-thinking.md", 71),
];

#[test]
fn every_ratio_block_in_the_specification_parses() {
    let blocks = ratio_blocks();
    assert!(blocks.len() >= 20, "found only {} blocks", blocks.len());
    let mut problems = Vec::new();
    let mut fragments_seen = 0;
    for (file, line, text) in &blocks {
        let parsed = parse(text);
        if FRAGMENTS.contains(&(file.as_str(), *line)) {
            fragments_seen += 1;
            assert!(
                !parsed.is_clean(),
                "{file}:{line} parses as a file now; remove it from FRAGMENTS"
            );
            let wrapped = format!("fn fragment() {{\n{text}}}\n");
            for d in &parse(&wrapped).diagnostics {
                problems.push(d.render(&format!("{file}:{line} (as a fragment)"), &wrapped));
            }
        } else {
            for d in &parsed.diagnostics {
                problems.push(d.render(&format!("{file}:{line}"), text));
            }
        }
    }
    assert_eq!(
        fragments_seen,
        FRAGMENTS.len(),
        "a listed fragment was not found"
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

// ---- the nesting limit and the stack ----

fn options(max_nesting: usize) -> LexOptions {
    LexOptions {
        max_nesting,
        ..LexOptions::default()
    }
}

fn codes(source: &str, options: &LexOptions) -> Vec<&'static str> {
    parse_with(source, options)
        .diagnostics
        .iter()
        .map(|d| d.code.as_str())
        .collect()
}

/// `(name, nodes per level, source with n levels)`.
type Shape = (&'static str, usize, fn(usize) -> String);

/// Shapes of source that nest by repetition. A tree whose deepest chain of expressions, types and
/// patterns is deeper than `max_nesting` is refused. Most shapes add one node per repetition and
/// the innermost literal is a node too, so `n` repetitions are accepted exactly when
/// `n < max_nesting`; a call adds two (the call and its argument), so `2n < max_nesting`.
fn last_clean(per_level: usize, max: usize) -> usize {
    (max - 1) / per_level
}

fn shapes() -> Vec<Shape> {
    vec![
        ("parentheses", 1, |n| {
            format!("const X = {}1{}\n", "(".repeat(n), ")".repeat(n))
        }),
        ("lists", 1, |n| {
            format!("const X = {}1{}\n", "[".repeat(n), "]".repeat(n))
        }),
        ("negations", 1, |n| {
            format!("const X = {}1\n", "-".repeat(n))
        }),
        ("nots", 1, |n| {
            format!("const X = {}true\n", "not ".repeat(n))
        }),
        ("additions", 1, |n| {
            format!("const X = 1{}\n", " + 1".repeat(n))
        }),
        ("calls", 2, |n| {
            format!("const X = {}1{}\n", "f(".repeat(n), ")".repeat(n))
        }),
        ("closures", 1, |n| {
            format!("const X = {}1\n", "|x| ".repeat(n))
        }),
        ("list types", 1, |n| {
            format!("type X = {}Int{}\n", "[".repeat(n), "]".repeat(n))
        }),
        ("option types", 1, |n| {
            format!("type X = Int{}\n", "?".repeat(n))
        }),
        ("tuple patterns", 2, |n| {
            format!(
                "fn f() {{ let {}1{} = 1 }}\n",
                "(".repeat(n),
                ",)".repeat(n)
            )
        }),
    ]
}

#[test]
fn nesting_is_refused_exactly_at_the_limit() {
    for max in [4, 16, 128, 256] {
        for (name, per_level, make) in shapes() {
            let options = options(max);
            let clean = last_clean(per_level, max);
            assert_eq!(
                codes(&make(clean), &options),
                Vec::<&str>::new(),
                "{name}: {clean} levels with max_nesting {max}"
            );
            assert_eq!(
                codes(&make(clean + 1), &options),
                ["P0002"],
                "{name}: {} levels with max_nesting {max}",
                clean + 1
            );
            // Far past the limit the parse still ends with exactly one diagnostic (the reader may
            // be first to refuse the brackets).
            let far = codes(&make(max + 1), &options);
            assert!(
                far == ["P0002"] || far == ["L0012"],
                "{name}: {far:?} at {} levels",
                max + 1
            );
        }
    }
}

#[test]
fn the_default_limit_is_128_and_the_largest_is_256() {
    let default = LexOptions::default();
    assert_eq!(default.max_nesting, 128);
    for (name, per_level, make) in shapes() {
        let clean = last_clean(per_level, 128);
        assert!(codes(&make(clean), &default).is_empty(), "{name}: {clean}");
        assert_eq!(codes(&make(clean + 1), &default), ["P0002"], "{name}");
        // `parse` uses the defaults.
        assert!(parse(&make(clean)).is_clean(), "{name}: parse");
        let refused: Vec<Code> = parse(&make(clean + 1))
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect();
        assert_eq!(refused, [Code::ParseNestingTooDeep], "{name}: parse");
        // A larger request is held to 256.
        let huge = options(10_000);
        let clean = last_clean(per_level, 256);
        assert!(codes(&make(clean), &huge).is_empty(), "{name}: {clean}");
        assert_eq!(codes(&make(clean + 1), &huge), ["P0002"], "{name}");
    }
}

#[test]
fn nested_blocks_are_bounded_by_the_reader_first() {
    // Blocks count against the reader's nesting; the function body is the first level.
    let make = |n: usize| format!("fn f() {}1{}\n", "{ ".repeat(n), " }".repeat(n));
    for max in [4, 16, 128] {
        assert_eq!(codes(&make(max), &options(max)), Vec::<&str>::new());
        assert_eq!(codes(&make(max + 1), &options(max)), ["L0012"]);
    }
}

#[test]
fn past_the_limit_the_parse_stops_with_one_diagnostic_and_keeps_what_it_read() {
    let source = format!(
        "const A = 1\nconst B = {}1{}\n",
        "-(".repeat(150),
        ")".repeat(150)
    );
    let parsed = parse_with(&source, &options(256));
    let found: Vec<Code> = parsed.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(found, [Code::ParseNestingTooDeep]);
    assert!(
        !parsed.module.items.is_empty(),
        "the items read before the limit are kept"
    );
}

#[test]
fn the_deepest_allowed_source_parses_on_a_small_stack() {
    // The parser runs on a thread of its own with a large stack, so the caller's stack does not
    // matter: parse the deepest allowed sources from a thread with only 512 KiB, in a debug build
    // where frames are large. The tree is dropped on this thread too.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let options = options(256);
            for (name, per_level, make) in shapes() {
                let source = make(last_clean(per_level, 256));
                let parsed = parse_with(&source, &options);
                assert!(parsed.is_clean(), "{name}: {:?}", parsed.diagnostics);
            }
        })
        .expect("a thread");
    handle.join().expect("no stack overflow, no panic");
}

/// The deepest source of a block shape that parses cleanly at `max_nesting` 256.
fn deepest(open: &str, close: &str, inner: &str) -> String {
    let make = |n: usize| {
        format!(
            "fn f() {{ {}{inner}{} }}\n",
            open.repeat(n),
            close.repeat(n)
        )
    };
    let n = (1..=256)
        .rev()
        .find(|n| parse_with(&make(*n), &options(256)).is_clean())
        .expect("some depth parses");
    assert!(n >= 60, "only {n} levels of `{open}` parse");
    make(n)
}

#[test]
fn printing_and_dumping_the_deepest_trees_work_on_a_small_stack() {
    // `print` and `dump` run on the same big stack as the parser, so the caller's 512 KiB do not
    // matter, in a debug build where frames are large.
    let mut sources: Vec<(String, String)> = shapes()
        .into_iter()
        .map(|(name, per_level, make)| (name.to_owned(), make(last_clean(per_level, 256))))
        .collect();
    for (name, open, close, inner) in [
        ("ifs", "if a { ", " }", "1"),
        ("fors", "for x in xs { ", " }", "1"),
        ("tries", "try { ", " } catch e { 1 }", "1"),
        ("matches", "match a { _ => ", " }", "1"),
        ("pars", "par { ", " }", "1"),
        ("let blocks", "let b = { ", " }", "1"),
        ("parens", "f(", ")", "1"),
        ("closures", "g(|x| ", ")", "1"),
    ] {
        sources.push((name.to_owned(), deepest(open, close, inner)));
    }
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            for (name, source) in sources {
                let parsed = parse_with(&source, &options(256));
                assert!(parsed.is_clean(), "{name}: {:?}", parsed.diagnostics);
                let text = print(&parsed.module);
                assert!(!text.contains("<too deep>"), "{name}");
                assert!(!dump(&parsed.module).contains("(too-deep)"), "{name}");
            }
        })
        .expect("a thread");
    handle.join().expect("no stack overflow, no panic");
}

#[test]
fn the_round_trip_holds_only_within_the_source_limit() {
    // Printing indents by depth, so the text of a deep tree can outgrow `MAX_SOURCE_BYTES` even
    // though the source it came from was small: it then does not read back (`L0013`).
    let make = |n: usize| format!("fn f() {{ {}1{} }}\n", "if a { ".repeat(n), " }".repeat(n));
    let deep = make(255);
    let parsed = parse_with(&deep, &options(256));
    assert!(parsed.is_clean(), "{:?}", parsed.diagnostics);
    let text = print(&parsed.module);
    assert!(text.len() > MAX_SOURCE_BYTES, "{} bytes", text.len());
    let again: Vec<Code> = parse_with(&text, &options(256))
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect();
    assert_eq!(again, [Code::SourceTooLarge]);
    // Within the limit the text reads back as the same tree.
    let shallow = parse(&make(60));
    let text = print(&shallow.module);
    assert!(text.len() <= MAX_SOURCE_BYTES);
    assert_eq!(dump(&parse(&text).module), dump(&shallow.module));
}

#[test]
fn deep_mixed_nesting_parses() {
    // Many kinds of construct in one chain, to reach the widest frames of the parser.
    let mut open = String::new();
    let mut close = String::new();
    for i in 0..60 {
        match i % 3 {
            0 => {
                open.push_str("match a { _ if (");
                close.insert_str(0, ") => x }");
            }
            1 => {
                open.push_str("{ k: [");
                close.insert_str(0, "] }");
            }
            _ => open.push_str("|(a, [b, ..c])| "),
        }
    }
    let source = format!("const X = {open}1{close}\n");
    let parsed = parse_with(&source, &options(256));
    assert!(parsed.is_clean(), "{:?}", parsed.diagnostics);
}

// ---- hostile input ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap_or(1)).unwrap_or(0)
    }
}

/// A source's diagnostics are bounded, and each lies inside the source.
fn assert_sane(source: &str, label: &str) {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.len() <= 101,
        "{label}: {} diagnostics",
        parsed.diagnostics.len()
    );
    for d in &parsed.diagnostics {
        assert!(
            d.span.start <= d.span.end && d.span.end <= source.len(),
            "{label}: span {:?} outside the source",
            d.span
        );
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..300 {
        let len = rng.below(400);
        let bytes: Vec<u8> = (0..len)
            .map(|_| u8::try_from(rng.next() >> 56).unwrap_or(0))
            .collect();
        let source = String::from_utf8_lossy(&bytes).into_owned();
        assert_sane(&source, &format!("round {round}"));
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "the table of words")]
fn random_token_soup_never_panics() {
    const WORDS: &[&str] = &[
        "fn",
        "harness",
        "prompt",
        "type",
        "enum",
        "const",
        "import",
        "state",
        "test",
        "pub",
        "meta",
        "let",
        "var",
        "if",
        "else",
        "for",
        "in",
        "while",
        "loop",
        "repeat",
        "try",
        "catch",
        "match",
        "par",
        "race",
        "with",
        "timeout",
        "do",
        "defer",
        "return",
        "break",
        "continue",
        "raise",
        "and",
        "or",
        "not",
        "is",
        "as",
        "none",
        "true",
        "false",
        "x",
        "Y",
        "_",
        "1",
        "2.5",
        "3s",
        "$1",
        "\"s\"",
        "\"a{b}c\"",
        "r\"raw\"",
        "\"\"\"l\n\"\"\"",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        ",",
        ";",
        ":",
        "::",
        ".",
        "..",
        "..=",
        "?.",
        "?",
        "??",
        "|",
        "||",
        "|>",
        "=",
        "==",
        "!=",
        "<",
        "<=",
        ">",
        ">=",
        "=>",
        "->",
        "+",
        "-",
        "*",
        "/",
        "%",
        "+=",
        "@",
        "\n",
        "\n",
        "\n",
        "/// doc\n",
        "// c\n",
        "system",
        "task",
        "data",
        "context",
    ];
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for round in 0..2_000 {
        let count = rng.below(60);
        let mut source = String::new();
        for _ in 0..count {
            source.push_str(WORDS[rng.below(WORDS.len())]);
            source.push(' ');
        }
        assert_sane(&source, &format!("soup {round}: {source:?}"));
    }
}

#[test]
fn hostile_shapes_are_handled() {
    let big = 200_000;
    let inputs: Vec<(&str, String)> = vec![
        ("open parens", "(".repeat(big)),
        ("close parens", ")".repeat(big)),
        ("open braces", "{".repeat(big)),
        ("close braces", "}".repeat(big)),
        ("minuses", format!("const X = {}1", "-".repeat(big))),
        ("additions", format!("const X = 1{}", "+1".repeat(big))),
        ("commas", format!("const X = [{}]", ",".repeat(big))),
        ("semicolons", ";".repeat(big)),
        ("at signs", "@ ".repeat(big)),
        ("keywords", "fn ".repeat(big)),
        ("if else", "if x { } else ".repeat(big / 10)),
        ("long identifier", "a".repeat(big)),
        ("long string", format!("const X = \"{}\"", "x".repeat(big))),
        ("lets", "let ".repeat(big)),
        ("pipes", format!("const X = a{}", " |> f".repeat(big / 5))),
    ];
    for (name, source) in inputs {
        assert_sane(&source, name);
    }
}

fn best_of(runs: usize, source: &str) -> Duration {
    (0..runs)
        .map(|_| {
            let start = Instant::now();
            let parsed = parse(source);
            std::hint::black_box(&parsed);
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

#[test]
fn parsing_time_grows_linearly() {
    // For each shape, four times the input takes about four times as long. The bound is generous
    // (a debug build on a loaded machine is noisy) but a quadratic parser would take sixteen.
    type Maker = fn(usize) -> String;
    let shapes: Vec<(&str, Maker)> = vec![
        ("statements", |n| {
            format!("fn f() {{\n{}}}\n", "let a = f(1, 2)\n".repeat(n))
        }),
        ("items", |n| "const X = 1\n".repeat(n)),
        ("list elements", |n| {
            format!("const X = [{}]\n", "1, ".repeat(n))
        }),
        ("record fields", |n| {
            let fields: String = (0..n).fold(String::new(), |mut acc, i| {
                let _ = write!(acc, "f{i}: 1, ");
                acc
            });
            format!("const X = {{{fields}}}\n")
        }),
        ("arguments", |n| {
            format!("const X = f({})\n", "1, ".repeat(n))
        }),
        ("match arms", |n| {
            format!("const X = match a {{ {}}}\n", "1 => 2, ".repeat(n))
        }),
        ("errors", |n| "fn f() { ) ] 1 2 } \n".repeat(n)),
        ("broken items", |n| "let\n".repeat(n)),
        ("strings", |n| {
            format!("const X = \"{}\"\n", "a{b}c".repeat(n))
        }),
    ];
    for (name, make) in shapes {
        let small = make(3_000);
        let large = make(12_000);
        let t_small = best_of(5, &small);
        let t_large = best_of(5, &large);
        // Below a couple of milliseconds the timer says little; compare against that floor.
        let floor = Duration::from_millis(2);
        let ratio = t_large.as_secs_f64() / t_small.max(floor).as_secs_f64();
        assert!(
            ratio < 12.0,
            "{name}: 4x input took {ratio:.1}x as long ({t_small:?} -> {t_large:?})"
        );
    }
}

// ---- error recovery ----

#[test]
fn independent_errors_are_each_reported_once() {
    // One broken function per error shape, each followed by a good one: four lines per pair.
    let broken = [
        ("let a = (1 2)", Code::UnexpectedToken),
        ("let a = 1 < 2 < 3", Code::NonAssociative),
        ("f(a: 1, 2)", Code::PositionalAfterNamed),
        ("f(x) = 1", Code::NotAPlace),
        ("let a = [1 2]", Code::UnexpectedToken),
        ("let a = match x { 1 2 }", Code::UnexpectedToken),
        ("let = 1", Code::UnexpectedToken),
        ("let a = ]", Code::UnbalancedDelimiter),
        ("let a = 0..1..2", Code::NonAssociative),
        ("@turn", Code::MisplacedModifier),
    ];
    let mut source = String::new();
    for (index, (body, _)) in broken.iter().enumerate() {
        writeln!(source, "fn broken_{index}() {{\n  {body}\n  1\n}}").unwrap();
    }
    let parsed = parse(&source);
    for (index, (body, code)) in broken.iter().enumerate() {
        let first_line = 4 * index + 1;
        let here: Vec<&ratio_syntax::Diagnostic> = parsed
            .diagnostics
            .iter()
            .filter(|d| {
                let line = usize::try_from(d.span.line).unwrap();
                (first_line..first_line + 4).contains(&line)
            })
            .collect();
        assert_eq!(here.len(), 1, "`{body}`: {here:?}");
        assert_eq!(here[0].code, *code, "`{body}`: {here:?}");
    }
    assert_eq!(parsed.diagnostics.len(), broken.len());
    // The good function bodies after each error were still read.
    assert_eq!(parsed.module.items.len(), broken.len());
}

#[test]
fn the_good_items_between_errors_are_kept() {
    let source = "fn a() { 1 }\nconst X = (1 2)\nfn b() { 2 }\nlet z = 1\nfn c() { 3 }\n";
    let parsed = parse(source);
    let names: Vec<String> = parsed
        .module
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Fn(f) => Some(f.name.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["a", "b", "c"], "{:?}", parsed.diagnostics);
}

#[test]
fn a_parse_with_too_many_errors_ends_with_the_marker() {
    let source = "fn f() {\n".to_owned() + &"  let a = (1 2)\n".repeat(200) + "}\n";
    let parsed = parse(&source);
    assert_eq!(parsed.diagnostics.len(), 101);
    assert_eq!(
        parsed.diagnostics.last().map(|d| d.code),
        Some(Code::ParseTooManyErrors)
    );
    assert!(
        parsed.diagnostics[..100]
            .iter()
            .all(|d| d.code == Code::UnexpectedToken)
    );
}

// ---- rendering ----

fn first_render(file: &str, source: &str) -> String {
    let parsed = parse(source);
    parsed
        .diagnostics
        .first()
        .unwrap_or_else(|| panic!("no diagnostic for {source:?}"))
        .render(file, source)
}

#[test]
fn render_shows_file_position_line_and_caret() {
    let source = "fn f() {\n  let a = (1 2)\n}\n";
    assert_eq!(
        first_render("harness.ratio", source),
        "error[P0001]: expected `,` or `)`, found a number\n \
          --> harness.ratio:2:14\n  \
           |\n\
         2 |   let a = (1 2)\n  \
           |              ^\n"
    );
}

#[test]
fn render_copies_tabs_so_the_caret_lines_up() {
    let source = "fn f() {\n\tlet a = (1 2)\n}\n";
    assert_eq!(
        first_render("tabs.ratio", source),
        "error[P0001]: expected `,` or `)`, found a number\n \
          --> tabs.ratio:2:13\n  \
           |\n\
         2 | \tlet a = (1 2)\n  \
           | \t           ^\n"
    );
}

#[test]
fn render_drops_the_carriage_return() {
    let source = "fn f() {\r\n  let a = (1 2)\r\n}\r\n";
    let text = first_render("crlf.ratio", source);
    assert!(!text.contains('\r'), "{text:?}");
    assert!(text.contains("--> crlf.ratio:2:14\n"), "{text:?}");
    assert!(
        text.ends_with("2 |   let a = (1 2)\n  |              ^\n"),
        "{text:?}"
    );
}

#[test]
fn render_counts_characters_not_bytes() {
    let source = "fn f() {\n  let a = (\"\u{1f600}\" 2)\n}\n";
    assert_eq!(
        first_render("emoji.ratio", source),
        "error[P0001]: expected `,` or `)`, found a number\n \
          --> emoji.ratio:2:16\n  \
           |\n\
         2 |   let a = (\"\u{1f600}\" 2)\n  \
           |                ^\n"
    );
}

#[test]
fn render_covers_a_span_on_its_first_line_only() {
    // The target of the assignment spans two lines; the carets stop at the end of the first.
    let source = "fn f() {\n  g(\n    1,\n  ) = 2\n}\n";
    assert_eq!(
        first_render("multi.ratio", source),
        "error[P0004]: only a name, a field (`x.a`) or an element (`x[i]`) can be assigned\n \
          --> multi.ratio:2:3\n  \
           |\n\
         2 |   g(\n  \
           |   ^^\n"
    );
}

#[test]
fn render_underlines_a_whole_token_and_numbers_wide_gutters() {
    let mut source = String::new();
    for _ in 0..9 {
        source.push('\n');
    }
    source.push_str("const X = 1 < 2 < 3\n");
    assert_eq!(
        first_render("wide.ratio", &source),
        "error[P0005]: comparisons do not chain: write `a < b and b < c`, or add parentheses\n  \
          --> wide.ratio:10:17\n   \
            |\n\
         10 | const X = 1 < 2 < 3\n   \
            |                 ^\n"
    );
    let source = "\u{feff}let x = 1\n";
    assert!(first_render("bom.ratio", source).contains("1 | let x = 1\n  | ^^^\n"));
}

#[test]
fn render_does_not_fail_on_odd_spans() {
    // A diagnostic at the end of the source, in an empty source, and past a multi-byte character.
    for source in ["", "fn", "fn f() {", "\"\u{1f600}", "\u{e9}"] {
        let parsed = parse(source);
        for d in &parsed.diagnostics {
            let text = d.render("odd.ratio", source);
            assert!(text.starts_with(&format!("error[{}]", d.code)), "{text}");
        }
    }
}

// ---- the corpus covers every kind of tree node ----

/// The variants of `pub enum name { … }` in `ast.rs`, without `Error` (which only a failed parse
/// builds).
fn ast_variants(ast: &str, name: &str) -> Vec<String> {
    let header = format!("pub enum {name} {{");
    let start = ast
        .find(&header)
        .unwrap_or_else(|| panic!("no enum {name}"));
    let mut out = Vec::new();
    for line in ast[start + header.len()..].lines().skip(1) {
        if line == "}" {
            break;
        }
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if ident != "Error" {
                out.push(ident);
            }
        }
    }
    assert!(!out.is_empty(), "enum {name} has no variants?");
    out
}

/// `(enum, prefixes)`: the Debug text of a node of this variant contains `prefix` followed by the
/// variant's name, for one of the prefixes. The prefixes name the field or struct that holds the
/// enum, which tells apart variants that several enums share (`Expr`, `Tuple`, `Record`, …).
const KINDS: &[(&str, &[&str])] = &[
    (
        "ItemKind",
        &["public: false, kind: ", "public: true, kind: "],
    ),
    ("ImportPath", &["path: "]),
    ("TypeKind", &["Type { kind: "]),
    ("StmtKind", &["Stmt { kind: "]),
    ("ExprKind", &["Expr { kind: "]),
    ("Lit", &["Lit("]),
    ("StrPart", &[""]),
    ("FieldInit", &["fields: [", ", "]),
    ("UnaryOp", &["Unary { op: "]),
    ("BinaryOp", &["Binary { op: "]),
    ("Condition", &["cond: "]),
    ("Else", &["else_: Some("]),
    ("Catch", &["catch: "]),
    ("PatternKind", &["Pattern { kind: "]),
    ("ListPattern", &[""]),
    ("SectionKind", &["Section { kind: "]),
];

fn contains_name(text: &str, prefix: &str, variant: &str) -> bool {
    let needle = format!("{prefix}{variant}");
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let at = from + found;
        let before_ok =
            !prefix.is_empty() || !text[..at].ends_with(|c: char| c.is_ascii_alphanumeric());
        let after = text[at + needle.len()..].chars().next();
        if before_ok && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// The parse-ok files of the corpus, and the AST ones parse into, joined as Debug text.
fn corpus_debug_text() -> String {
    let mut text = String::new();
    let mut count = 0;
    let mut stack = vec![repo_root().join("conformance")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("conformance").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "ratio") {
                let source = fs::read_to_string(&path).expect("a corpus file");
                if source.lines().any(|l| l == "// expect: parse-ok") {
                    let parsed = parse(&source);
                    assert!(parsed.is_clean(), "{}", path.display());
                    writeln!(text, "{:?}", parsed.module).unwrap();
                    count += 1;
                }
            }
        }
    }
    assert!(count >= 20, "only {count} parse-ok files");
    text
}

#[test]
fn the_parse_ok_corpus_builds_every_kind_of_node() {
    let ast = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ast.rs"))
        .expect("ast.rs");
    let text = corpus_debug_text();
    let mut missing = Vec::new();
    for (name, prefixes) in KINDS {
        for variant in ast_variants(&ast, name) {
            if !prefixes.iter().any(|p| contains_name(&text, p, &variant)) {
                missing.push(format!("{name}::{variant}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "no parse-ok corpus file builds: {}",
        missing.join(", ")
    );
}

#[test]
fn every_kind_enum_of_the_tree_is_in_the_coverage_table() {
    // A new enum in ast.rs has to be added to KINDS, or listed here as one with no variants worth
    // covering.
    let ast = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ast.rs"))
        .expect("ast.rs");
    let known: Vec<&str> = KINDS.iter().map(|(n, _)| *n).collect();
    let others = ["AssignOp"];
    for line in ast.lines() {
        if let Some(rest) = line.strip_prefix("pub enum ") {
            let name: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            assert!(
                known.contains(&name.as_str()) || others.contains(&name.as_str()),
                "enum {name} in ast.rs is not in the coverage table"
            );
        }
    }
}

#[test]
fn the_corpus_uses_every_assignment_operator() {
    let text = corpus_debug_text();
    for op in ["Set", "Add", "Sub", "Mul", "Div"] {
        assert!(
            text.contains(&format!("op: {op}, value:")),
            "no assignment with {op}"
        );
    }
}

#[test]
fn the_reader_gives_fn_heads_and_closure_arms_their_braces() {
    // `a: fn() {}` and `a: fn() -> T { x }` are function expressions, `a: fn() -> T` a type.
    for src in [
        "fn f() { let r = { a: fn() -> T { x } } }",
        "fn f() { g(a: fn() {}) }",
        "fn f() { let r = {a: fn() {}} }",
        "fn f() { g(a: fn() -> { b: Int } { { b: 1 } }) }",
        "fn f() { match q { a => |x, y| match n { _ => 1 }, } }",
        "fn f(a: fn() -> T, b: { c: fn() -> Int }) { let g: fn() -> T = h\ng }",
        "type F = fn(A) -> B\ntype G = Int",
        "type F = fn() -> { a: Int }\ntype G = Int",
        "prompt p() { format fn() -> Int\n steps \"x\" }",
        "type X = Int??\ntype Y = List<Int>\nfn f() {}",
    ] {
        let parsed = parse(src);
        assert!(parsed.is_clean(), "{src}: {:?}", parsed.diagnostics);
    }
}
