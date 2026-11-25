//! The specification lives in this repository (`spec/`), so instead of pinning a revision the
//! implementation checks that it agrees with the text next to it:
//!
//! - [`ratio::SPEC_DRAFT`] is the draft named on the version line of `spec/README.md`;
//! - every conformance directory names, in `conformance/spec-map.txt`, the chapters and headings
//!   it tests, and each of those exists.

use std::collections::BTreeSet;
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
