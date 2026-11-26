//! The specification lives in this repository (`spec/`), so instead of pinning a revision the
//! implementation checks that it agrees with the text next to it:
//!
//! - [`ratio::SPEC_DRAFT`] is the draft named on the version line of `spec/README.md`;
//! - every conformance directory names, in `conformance/spec-map.txt`, the chapters and headings
//!   it tests, and each of those exists.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// GitHub's heading anchors: lower case, punctuation dropped, spaces to hyphens.
fn anchor(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

fn anchors(chapter: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut fenced = false;
    for line in chapter.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            found.insert(anchor(&line[hashes..]));
        }
    }
    found
}

#[test]
fn the_spec_draft_matches_the_specification() {
    let readme = fs::read_to_string(repo().join("spec/README.md")).expect("spec/README.md");
    let line = readme
        .lines()
        .find(|line| line.starts_with("Version: draft "))
        .expect("a `Version: draft` line in spec/README.md");
    let draft = line["Version: draft ".len()..]
        .split_whitespace()
        .next()
        .expect("a draft number");
    assert_eq!(draft, ratio::SPEC_DRAFT);
}

#[test]
fn every_conformance_directory_references_existing_spec_chapters() {
    let conformance = repo().join("conformance");
    let map = fs::read_to_string(conformance.join("spec-map.txt")).expect("spec-map.txt");
    let mut mapped = BTreeSet::new();
    let mut problems = Vec::new();
    for line in map.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let dir = words.next().expect("a directory");
        mapped.insert(dir.to_owned());
        let refs: Vec<&str> = words.collect();
        if refs.is_empty() {
            problems.push(format!("{dir}: no spec references"));
        }
        for reference in refs {
            let Some((file, fragment)) = reference.split_once('#') else {
                problems.push(format!("{dir}: `{reference}` has no #heading"));
                continue;
            };
            let Ok(chapter) = fs::read_to_string(repo().join("spec").join(file)) else {
                problems.push(format!("{dir}: no chapter spec/{file}"));
                continue;
            };
            if !anchors(&chapter).contains(fragment) {
                problems.push(format!("{dir}: spec/{file} has no heading #{fragment}"));
            }
        }
    }
    for entry in fs::read_dir(&conformance).expect("conformance/").flatten() {
        let path = entry.path();
        let holds_programs = path.is_dir()
            && fs::read_dir(&path)
                .expect("a conformance directory")
                .flatten()
                .any(|f| f.path().extension().is_some_and(|e| e == "ratio"));
        let name = entry.file_name().to_string_lossy().into_owned();
        if holds_programs && !mapped.contains(&name) {
            problems.push(format!("conformance/{name} is not in spec-map.txt"));
        }
        if !path.is_dir() && mapped.contains(&name) {
            problems.push(format!(
                "spec-map.txt names {name}, which is not a directory"
            ));
        }
    }
    for dir in &mapped {
        if !conformance.join(dir).is_dir() {
            problems.push(format!(
                "spec-map.txt names conformance/{dir}, which does not exist"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_anchor_rule_matches_github() {
    assert_eq!(
        anchor(" Strings and interpolation"),
        "strings-and-interpolation"
    );
    assert_eq!(
        anchor(" Decision R15: `ratio` behind a host trait"),
        "decision-r15-ratio-behind-a-host-trait"
    );
}

// ---- conformance/builtins.txt ----

/// The cells of a Markdown table row, split on unescaped `|`, trimmed, `\|` unescaped.
fn cells(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line.trim().trim_start_matches('|').chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cur).trim().to_owned()),
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// The text between the first pair of backticks, or the whole cell.
fn code(cell: &str) -> String {
    let mut parts = cell.split('`');
    match (parts.next(), parts.next()) {
        (Some(_), Some(inner)) => inner.to_owned(),
        _ => cell.to_owned(),
    }
}

/// The backticked words of a cell.
fn codes(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The cost class of each pure data built-in named in chapter 06's cost-class table; any
/// other pure `data` built-in is `linear`.
fn cost_classes(data: &str) -> BTreeMap<String, String> {
    let mut classes = BTreeMap::new();
    let start = data
        .find("## Cost classes and representations")
        .expect("the cost-class section");
    for line in data[start..]
        .lines()
        .skip_while(|l| !l.starts_with("| Class"))
        .skip(2)
    {
        if !line.starts_with('|') {
            break;
        }
        let row = cells(line);
        let class = code(&row[0]);
        for name in codes(&row[2]) {
            let full = match name.split_once('.') {
                Some((m, f)) if ident(m) && ident(f) => name.clone(),
                // The `math` built-ins on scalars are listed by their short names.
                None if ident(&name) && name != "math" => format!("math.{name}"),
                _ => continue,
            };
            classes.insert(full, class.clone());
        }
    }
    classes
}

/// `conformance/builtins.txt`, generated from the built-in tables of chapter 06.
fn generate_builtins() -> String {
    let dir = repo().join("spec/06-stdlib");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("spec/06-stdlib")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    let mut out = String::from(
        "# Every built-in of chapter 06 (spec/06-stdlib/), generated from its tables by\n\
         # crates/ratio/tests/spec.rs; CI fails when this file differs. Do not edit it: change the\n\
         # chapter, then regenerate with\n\
         #   RATIO_BLESS=1 cargo test -p ratio-lang --test spec builtins\n\
         #\n\
         # name<TAB>signature<TAB>needs<TAB>cost class (pure `data` built-ins; `-` otherwise)\n",
    );
    for path in files {
        let chapter = fs::read_to_string(&path).expect("a chapter");
        let is_data = path.file_name().is_some_and(|f| f == "data.md");
        let classes = if is_data {
            cost_classes(&chapter)
        } else {
            BTreeMap::new()
        };
        let mut lines = chapter.lines();
        while let Some(line) = lines.next() {
            if !line.starts_with("| Built-in |") {
                continue;
            }
            let header = cells(line);
            let col = |name: &str| header.iter().position(|h| h == name);
            let (sig, fails, needs) = (col("Signature"), col("Fails with"), col("Needs"));
            lines.next(); // the separator
            for row in lines.by_ref().take_while(|l| l.starts_with('|')) {
                let row = cells(row);
                let name = code(&row[0]);
                let signature = sig.map_or_else(String::new, |i| code(&row[i]));
                let needs = match needs {
                    Some(i) => row[i].replace('`', ""),
                    // `data`: pure unless the Fails column says `effect`.
                    None if fails.is_some_and(|i| row[i].contains("`effect`")) => {
                        "effect".to_owned()
                    }
                    None => "pure".to_owned(),
                };
                let class = if is_data && needs == "pure" {
                    classes
                        .get(&name)
                        .cloned()
                        .unwrap_or_else(|| "linear".to_owned())
                } else {
                    "-".to_owned()
                };
                writeln!(out, "{name}\t{signature}\t{needs}\t{class}").unwrap();
            }
        }
    }
    out
}

#[test]
fn builtins_txt_is_generated_from_chapter_06() {
    let generated = generate_builtins();
    let path = repo().join("conformance/builtins.txt");
    if std::env::var_os("RATIO_BLESS").is_some() {
        fs::write(&path, &generated).expect("write builtins.txt");
    }
    let checked_in = fs::read_to_string(&path)
        .expect("conformance/builtins.txt")
        .replace("\r\n", "\n");
    assert!(
        checked_in == generated,
        "conformance/builtins.txt differs from chapter 06; regenerate it with \
         `RATIO_BLESS=1 cargo test -p ratio-lang --test spec builtins`"
    );
    assert!(
        generated.lines().filter(|l| !l.starts_with('#')).count() > 300,
        "too few built-ins: the table format may have changed"
    );
}

#[test]
fn implemented_data_built_ins_have_their_chapter_06_class() {
    let generated = generate_builtins();
    let spec: BTreeMap<&str, &str> = generated
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some((*f.first()?, *f.get(3)?))
        })
        .collect();
    let implemented = ratio::interp::builtins::implemented();
    // Every implemented `data` built-in (stage 6's and stage 7's) has its class; every `text`
    // and `re` built-in of the chapter is implemented (stage 7).
    let data = |n: &str| {
        ["text.", "re.", "list.", "map.", "json.", "math."]
            .iter()
            .any(|m| n.starts_with(m))
    };
    let ours = |n: &str| n.starts_with("text.") || n.starts_with("re.");
    let mut problems = Vec::new();
    for (name, class) in &implemented {
        if !data(name) {
            continue;
        }
        match spec.get(name).copied() {
            Some(c) if c == class.name() => {}
            Some(c) => problems.push(format!(
                "{name}: implemented as {}, expected {c}",
                class.name()
            )),
            None => problems.push(format!("{name}: not in chapter 06")),
        }
    }
    for name in spec.keys().filter(|n| ours(n)) {
        if !implemented.iter().any(|(n, _)| n == name) {
            problems.push(format!("{name}: in chapter 06 but not implemented"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
