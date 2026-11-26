//! Stage 5 beyond the corpus: schema and digest snapshots (stable across platforms: the bytes
//! are fixed here), trust sites listed transitively, the package summary and `harnesses.check`
//! through a registry, and hostile inputs to the analyses.

#![allow(
    clippy::format_push_string,
    reason = "the test programs are built by appending"
)]

use std::time::{Duration, Instant};

use ratio_check::{Checked, Code, Effects, SchemaError, TrustKind, TypeId};
use ratio_names::{
    Loaded, Loader, MemoryRegistry, ModuleId, PackageId, PackageName, QualifiedName, RelPath,
    Source,
};
use ratio_syntax::ast::ItemKind;

fn registry_with(files: &[(&str, &str, &str)]) -> MemoryRegistry {
    let mut registry = MemoryRegistry::new();
    for (package, path, source) in files {
        let id = PackageId::new(Source::User, PackageName::parse(package).unwrap());
        let rel = RelPath::parse(path).unwrap();
        if *path == "main.ratio" {
            registry.set_entry(&id, rel.clone());
        }
        registry.add_file(&id, rel, source.as_bytes().to_vec());
    }
    registry
}

fn load(source: &str) -> Loaded {
    let registry = registry_with(&[("app", "main.ratio", source)]);
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap());
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics);
    loaded
}

fn check(source: &str) -> (Loaded, Checked) {
    let loaded = load(source);
    let checked = ratio_check::check(&loaded);
    assert!(
        checked.internal_errors().is_empty(),
        "{:?}",
        checked.internal_errors()
    );
    (loaded, checked)
}

/// The type a top-level `type` alias of the entry module names.
fn alias(loaded: &Loaded, checked: &Checked, name: &str) -> TypeId {
    let module = &loaded.modules[0];
    let index = module
        .ast
        .items
        .iter()
        .position(|i| match &i.kind {
            ItemKind::Type(d) => d.name.name == name,
            ItemKind::Enum(d) => d.name.name == name,
            _ => false,
        })
        .unwrap_or_else(|| panic!("no alias {name}"));
    checked.tables[0].items[index].unwrap_or_else(|| panic!("{name} is not lowered"))
}

const SCHEMAS: &str = r#"
enum Severity { low, medium, high }
enum Finding { Bug { line: Int, why: Str }, StyleNote { note: Str }, Clean }
enum Tree { Leaf { value: Int }, Node { kids: [Tree] } }
type Plain = { a: Int, b?: Str, c: Bool? }
type Report = {
  /// What the reviewer concluded.
  verdict: Bool,
  @len(0, 20) findings: [Finding],
  @range(0, 1.5) confidence: Float "How sure",
  @pattern(r"^[a-z]+$") tag: Str,
  @example("high") severity: Severity,
  counts: Map<Str, Int>,
  by_level: Map<Severity, [Str]>,
  span?: (Int, Int),
  raw: Json,
  took: Duration,
  cost: Money,
  nothing: (),
}
type Web = Untrusted<Report>
type Text = Str
type WebText = Untrusted<Str>
type Labelled = Secret<Untrusted<Str>>
type Briefed = { brief: Brief, user: UserText }
type Results = Result<[Int]>
fn f() -> Int { 1 }
"#;

#[test]
fn schemas_are_derived_canonically() {
    let (loaded, mut checked) = check(SCHEMAS);
    assert!(checked.is_clean(), "{:?}", checked.diagnostics);
    let cases = [
        (
            "Plain",
            r#"{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"string"},"c":{"type":"boolean"}},"required":["a"],"additionalProperties":false}"#,
        ),
        (
            "Report",
            concat!(
                r#"{"type":"object","properties":{"by_level":{"type":"object","additionalProperties":{"type":"array","items":{"type":"string"}},"propertyNames":{"enum":["low","medium","high"]}},"#,
                r#""confidence":{"type":"number","description":"How sure","minimum":0,"maximum":1.5},"#,
                r#""cost":{"type":"string","pattern":"^\\$[0-9]+(\\.[0-9]{1,6})?$"},"#,
                r#""counts":{"type":"object","additionalProperties":{"type":"integer"}},"#,
                r#""findings":{"type":"array","items":{"oneOf":[{"type":"object","properties":{"kind":{"const":"bug"},"line":{"type":"integer"},"why":{"type":"string"}},"required":["kind","line","why"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"style_note"},"note":{"type":"string"}},"required":["kind","note"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"clean"}},"required":["kind"],"additionalProperties":false}]},"minItems":0,"maxItems":20},"#,
                r#""nothing":{"type":"null"},"raw":{},"#,
                r#""severity":{"type":"string","enum":["low","medium","high"],"examples":["high"]},"#,
                r#""span":{"type":"array","prefixItems":[{"type":"integer"},{"type":"integer"}],"items":false},"#,
                r#""tag":{"type":"string","pattern":"^[a-z]+$"},"#,
                r#""took":{"type":"string","pattern":"^[0-9]+(ms|s|m|h)$"},"#,
                r#""verdict":{"type":"boolean","description":"What the reviewer concluded."}},"#,
                r#""required":["by_level","confidence","cost","counts","findings","nothing","raw","severity","tag","took","verdict"],"additionalProperties":false}"#
            ),
        ),
        (
            "Tree",
            concat!(
                r##"{"$ref":"#/$defs/Tree","$defs":{"Tree":{"oneOf":["##,
                r##"{"type":"object","properties":{"kind":{"const":"leaf"},"value":{"type":"integer"}},"required":["kind","value"],"additionalProperties":false},"##,
                r##"{"type":"object","properties":{"kind":{"const":"node"},"kids":{"type":"array","items":{"$ref":"#/$defs/Tree"}}},"required":["kind","kids"],"additionalProperties":false}]}}}"##
            ),
        ),
        ("WebText", r#"{"type":"string"}"#),
        ("Labelled", r#"{"type":"string"}"#),
        (
            "Results",
            concat!(
                r#"{"oneOf":[{"type":"object","properties":{"kind":{"const":"ok"},"value":{"type":"array","items":{"type":"integer"}}},"required":["kind","value"],"additionalProperties":false},"#,
                r#"{"type":"object","properties":{"kind":{"const":"err"},"error":"#
            ),
        ),
    ];
    for (name, want) in cases {
        let t = alias(&loaded, &checked, name);
        let got = checked.schema(t).unwrap();
        if name == "Results" {
            // `Error` holds a `Site` and an `ErrorKind`: only the start is fixed here.
            assert!(got.starts_with(want), "{name}: {got}");
        } else {
            assert_eq!(got, want, "{name}");
        }
    }
    // The untrusted view of a record has the record's schema: labels are not in the schema.
    let web = alias(&loaded, &checked, "Web");
    let report = alias(&loaded, &checked, "Report");
    assert_eq!(checked.schema(web), checked.schema(report));
}

#[test]
fn digests_keep_labels_and_are_fixed() {
    let (loaded, mut checked) = check(SCHEMAS);
    let forms = [
        ("Text", "ratio-type-1;str"),
        ("WebText", "ratio-type-1;untrusted(str)"),
        ("Labelled", "ratio-type-1;secret(untrusted(str))"),
        (
            "Plain",
            r#"ratio-type-1;record("a":int,"b"?:str,"c":opt(bool))"#,
        ),
        (
            "Briefed",
            r#"ratio-type-1;record("brief":brief,"user":user_text)"#,
        ),
        (
            "Tree",
            r#"ratio-type-1;#0;#0="Tree"{"Leaf":record("value":int),"Node":record("kids":list(#0))}"#,
        ),
    ];
    for (name, want) in forms {
        let t = alias(&loaded, &checked, name);
        assert_eq!(
            ratio_check::canonical_form(&mut checked.types, t).unwrap(),
            want,
            "{name}"
        );
    }
    let digest = |checked: &mut Checked, name: &str| {
        let t = alias(&loaded, checked, name);
        checked.digest(t).unwrap().hex()
    };
    // Snapshots: sha256 of the forms above, the same on every platform.
    assert_eq!(
        digest(&mut checked, "Text"),
        ratio_check::hex(&ratio_check::sha256(b"ratio-type-1;str"))
    );
    let text = digest(&mut checked, "Text");
    let web = digest(&mut checked, "WebText");
    let report = digest(&mut checked, "Report");
    let web_report = digest(&mut checked, "Web");
    assert_ne!(text, web, "Str and Untrusted<Str> digest differently");
    assert_ne!(
        report, web_report,
        "a record and its untrusted view digest differently"
    );
    assert_eq!(
        text,
        "8e2173abf9e5c7b99aeb95703848035fd507e7469faaac50875ba4e372fef626"
    );
    assert_eq!(
        web,
        "8265957ce5ebac759bcad42d68e8f200145d745bdbf05eb2b42dd14bcb220dbb"
    );
    assert_eq!(
        report,
        "49f67452e113fd89c053cf95fdf04e7c2643a9d5a126c5e785314c4ad6f13dfd"
    );
    // Descriptions and refinements change the schema, not the digest.
    let (loaded2, mut other) = check(
        "type Report = { verdict: Bool, findings: [Int] }\ntype Same = { @len(1, 2) findings: [Int], verdict: Bool \"why\" }\n",
    );
    let a = alias(&loaded2, &other, "Report");
    let b = alias(&loaded2, &other, "Same");
    assert_eq!(a, b, "records are structural");
    assert_eq!(other.digest(a).unwrap(), other.digest(b).unwrap());
}

#[test]
fn types_without_schemas_are_refused() {
    let (loaded, mut checked) = check(
        "type F = { f: fn(Int) -> Int }\ntype H = { c: Conversation }\ntype P = [Prompt]\ntype G = Map<Int, Image>\n",
    );
    for name in ["F", "H", "P", "G"] {
        let t = alias(&loaded, &checked, name);
        assert!(
            matches!(checked.schema(t), Err(SchemaError::NotSchemaable(_))),
            "{name}"
        );
        assert!(checked.digest(t).is_err(), "{name}");
    }
}

#[test]
fn a_doubling_type_is_refused_quickly_not_written_out() {
    let mut source = String::from("type T0 = { a: Int, b: Str }\n");
    for i in 1..=80 {
        source.push_str(&format!("type T{i} = {{ l: T{}, r: T{} }}\n", i - 1, i - 1));
    }
    let (loaded, mut checked) = check(&source);
    assert!(checked.is_clean(), "{:?}", checked.diagnostics);
    let t = alias(&loaded, &checked, "T80");
    let start = Instant::now();
    assert_eq!(checked.schema(t), Err(SchemaError::TooLarge));
    assert_eq!(checked.digest(t), Err(SchemaError::TooLarge));
    assert!(start.elapsed() < Duration::from_secs(5));
}

const SITES: &str = r#"
fn helper(x: Untrusted<Str>) -> Str {
  trust(x, because: "a helper")
}

fn uses_helper(x: Untrusted<Str>) -> Str {
  helper(x)
}

fn plan_step(step: Untrusted<Str>) -> Brief {
  as_task(step, because: "our planner")
}

pub harness entry(x: Untrusted<Str>, path: Untrusted<Str>) -> Str {
  let w = tools.write_file(path, x)
  uses_helper(x)
}

pub harness takes_text(rules: Str) -> Untrusted<Str> {
  model.generate(prompt { instruct rules })
}

pub harness offered(page: Untrusted<Str>, n: Int) -> Untrusted<Str> {
  let p = agents.spawn(page, "std:tdd_fix")
  page
}

@task
harness task_entry(t: TaskInput) -> Untrusted<Str> {
  model.generate(prompt { task t.brief })
}

fn untouched() -> Int { 1 }
"#;

#[test]
fn trust_sites_are_listed_transitively() {
    let (loaded, mut checked) = check(SITES);
    assert!(checked.is_clean(), "{:?}", checked.diagnostics);
    let kinds: Vec<(TrustKind, Option<&str>)> = checked
        .trust_sites
        .iter()
        .map(|s| (s.kind, s.reason.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (TrustKind::Trust, Some("a helper")),
            (TrustKind::AsTask, Some("our planner")),
        ]
    );
    assert_eq!(checked.trust_sites[0].ty, TypeId::STR);
    assert_eq!(checked.trust_sites[1].ty, TypeId::BRIEF);
    let index = |name: &str| {
        loaded.modules[0]
            .ast
            .items
            .iter()
            .position(|i| match &i.kind {
                ItemKind::Fn(d) => d.name.name == name,
                ItemKind::Harness(d) => d.name.name == name,
                _ => false,
            })
            .unwrap()
    };
    let sites = |checked: &Checked, name: &str| {
        checked
            .summary(ModuleId(0), index(name))
            .unwrap()
            .trust_sites
            .to_vec()
    };
    assert_eq!(sites(&checked, "helper"), vec![0]);
    assert_eq!(sites(&checked, "uses_helper"), vec![0], "through a call");
    assert_eq!(sites(&checked, "entry"), vec![0], "through two calls");
    assert_eq!(sites(&checked, "plan_step"), vec![1]);
    assert_eq!(sites(&checked, "untouched"), Vec::<usize>::new());
    let entry = checked.summary(ModuleId(0), index("entry")).unwrap();
    assert!(entry.effects.contains(Effects::WRITE));
    assert_eq!(entry.tools, vec!["write_file".to_owned()]);

    let report = checked.harness_check(&loaded);
    assert!(report.clean);
    assert_eq!(report.trust_sites.len(), 2);
    assert_eq!(report.trust_sites[0].ty, "Str");
    assert_eq!(report.trust_sites[0].line, 3);
    assert!(report.effects.contains(Effects::WRITE));
    assert!(report.effects.contains(Effects::DELEGATE));
    assert_eq!(
        report.describe(),
        "writes; delegates; tools: spawn_agent, write_file"
    );
    let runnable: Vec<(&str, bool)> = report
        .harnesses
        .iter()
        .map(|h| (h.name.as_str(), h.runnable_by_models.is_ok()))
        .collect();
    assert_eq!(
        runnable,
        vec![
            ("entry", true),
            ("takes_text", false),
            ("offered", true),
            ("task_entry", false),
        ]
    );
    let takes_text = &report.harnesses[1];
    assert!(
        takes_text
            .runnable_by_models
            .as_ref()
            .unwrap_err()
            .contains("trusted text"),
        "{:?}",
        takes_text.runnable_by_models
    );
    assert_eq!(
        report.harnesses[2].params[1].2.as_deref(),
        Ok(r#"{"type":"integer"}"#)
    );
}

#[test]
fn harnesses_check_follows_loads_through_the_registry() {
    let main = r#"
pub harness run(x: Untrusted<Str>) -> Untrusted<Str> {
  let h = meta.harness::<harness(Untrusted<Str>) -> Untrusted<Str>>("user:critics.strict")
  h(x)
}
"#;
    let critics = r#"
pub harness strict(x: Untrusted<Str>) -> Untrusted<Str> {
  let s = trust(x, because: "the critic's own rules")
  x
}
"#;
    let registry = registry_with(&[
        ("app", "main.ratio", main),
        ("critics", "main.ratio", critics),
    ]);
    let builtin = MemoryRegistry::new();
    let done = ratio_check::harnesses_check(
        &registry,
        &builtin,
        &QualifiedName::parse("user:app").unwrap(),
    );
    assert!(done.summary.clean, "{:?}", done.checked.diagnostics);
    assert_eq!(done.summary.trust_sites, Vec::new());
    assert_eq!(done.loaded_packages, vec!["user:critics".to_owned()]);
    assert_eq!(done.trust_sites.len(), 1);
    assert_eq!(done.trust_sites[0].package, "user:critics");
    assert!(done.complete);
    // Calling a loaded harness may do anything.
    assert!(done.summary.effects.contains(Effects::ALL));

    // A computed name cannot be followed: the list is incomplete, never silently shorter.
    let dynamic = r#"
pub harness run(x: Untrusted<Str>, which: Str) -> Untrusted<Str> {
  let h = meta.harness::<harness(Untrusted<Str>) -> Untrusted<Str>>("user:{which}")
  h(x)
}
"#;
    let registry = registry_with(&[("app", "main.ratio", dynamic)]);
    let done = ratio_check::harnesses_check(
        &registry,
        &builtin,
        &QualifiedName::parse("user:app").unwrap(),
    );
    assert!(done.summary.clean);
    assert!(!done.complete);
    // A name the registry does not have makes it incomplete too.
    let registry = registry_with(&[("app", "main.ratio", main)]);
    let done = ratio_check::harnesses_check(
        &registry,
        &builtin,
        &QualifiedName::parse("user:app").unwrap(),
    );
    assert!(!done.complete);
}

#[test]
fn the_lint_and_the_analysis_stay_linear_on_hostile_bodies() {
    // Many `var`s that become derived one after another inside a loop: the rounds are
    // bounded, and past them every local counts as derived.
    let mut body = String::from("fn f(p: Prompt) -> () {\n  let says = model.generate(p)\n");
    for i in 0..300 {
        body.push_str(&format!("  var v{i} = false\n"));
    }
    body.push_str("  loop {\n");
    for i in (1..300).rev() {
        body.push_str(&format!("    v{i} = v{}\n", i - 1));
    }
    body.push_str("    v0 = text.contains(says, \"go\")\n");
    body.push_str("    if v299 {\n      let _ = tools.write_file(\"out\", \"x\")\n    }\n  }\n}\n");
    // Many items calling each other in a chain, each with a trust site.
    for i in 0..2000 {
        body.push_str(&format!(
            "fn g{i}(x: Untrusted<Str>) -> Str {{\n  {}\n}}\n",
            if i == 0 {
                "trust(x, because: \"base\")".to_owned()
            } else {
                format!("let y = trust(x, because: \"step\")\n  g{}(x)", i - 1)
            }
        ));
    }
    let start = Instant::now();
    let (_, checked) = check(&body);
    assert!(start.elapsed() < Duration::from_secs(20));
    let codes: Vec<Code> = checked.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![Code::ModelGuard], "{:?}", checked.diagnostics);
    assert_eq!(checked.trust_sites.len(), 2000);
    // The last of the chain reaches every site, or the analysis said it gave up.
    let last = checked.summary(ModuleId(0), 2000).unwrap();
    assert!(checked.approximate || last.trust_sites.len() == 2000);
}

/// Every attribute-driven rule of a built-in must hold when the built-in is used as a function
/// value: each annotated built-in is either refused as a value (`C0036`) or its attributes are
/// all of the kinds whose rules hold for values too.
#[test]
fn every_annotated_built_in_is_refused_as_a_value_or_covered() {
    // Rules enforced only at a direct call: the built-in must be refused as a value.
    const CALL_ONLY: &[&str] = &["audited", "loads", "secret_answers"];
    // Rules that hold for values: effects, tools and purity count where a value is named;
    // `@user` and `@local` only relax the lint and the sink summary; `@schema(T)` is checked
    // at the type a value is named with; a secrecy sink without type parameters refuses
    // secrets through its parameter types.
    const VALUE_SAFE: &[&str] = &["pure", "effect", "tool", "user", "local", "schema"];
    let rules = ratio_check::builtin_rules();
    assert!(rules.len() > 40, "{}", rules.len());
    let mut refused = 0;
    for b in &rules {
        let call_only = b.attrs.iter().any(|a| CALL_ONLY.contains(&a.as_str()))
            || (b.generic && b.attrs.iter().any(|a| a == "secrecy_sink"));
        for a in &b.attrs {
            assert!(
                CALL_ONLY.contains(&a.as_str())
                    || VALUE_SAFE.contains(&a.as_str())
                    || a == "secrecy_sink",
                "`@{a}` on `{}` is not classified for function values",
                b.name
            );
        }
        assert_eq!(
            call_only,
            b.refused_as_value.is_some(),
            "`{}` ({:?})",
            b.name,
            b.attrs
        );
        if call_only {
            refused += 1;
            let (_, checked) = check(&format!(
                "fn f() -> Int {{\n  let v = {}\n  1\n}}\n",
                b.name
            ));
            let codes: Vec<(Code, u32, u32)> = checked
                .diagnostics
                .iter()
                .map(|d| (d.code, d.span.line, d.span.col))
                .collect();
            assert_eq!(codes, vec![(Code::NotMonomorphic, 2, 11)], "{}", b.name);
        }
    }
    assert_eq!(
        refused, 5,
        "trust, as_task, declassify, session.ask, meta.harness"
    );
    // The `@schema(T)` rule at a value's type.
    let (_, checked) = check("fn f() -> Int {\n  let e: fn(Image) -> Json = json.encode\n  1\n}\n");
    let codes: Vec<Code> = checked.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![Code::NotSchemaable]);
    // A value whose expected type does not fit its instantiation is refused where it is
    // named, never typed as an error and let through (`fn(Json) -> Str` would launder).
    let (_, checked) = check(
        "fn f(j: Json) -> TrustedPrompt {\n  let d: fn(Json) -> Str = json.decode\n  prompt { system d(j) }\n}\n",
    );
    let codes: Vec<Code> = checked.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![Code::Mismatch]);
}

#[test]
fn a_dotted_load_name_takes_the_longest_package_and_ambiguity_is_incomplete() {
    let readings: Vec<String> = ratio_check::package_readings("user:a.b.c@^1")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        readings,
        vec!["user:a.b@^1".to_owned(), "user:a@^1".to_owned()]
    );
    // A bare name: the source first (`std`, then `user`), then the longest prefix.
    let readings: Vec<String> = ratio_check::package_readings("a.b.c")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(readings, vec!["std:a.b", "std:a", "user:a.b", "user:a"]);
    let main = r#"
pub harness run(x: Untrusted<Str>) -> Untrusted<Str> {
  let h = meta.harness::<harness(Untrusted<Str>) -> Untrusted<Str>>("user:a.b.c")
  h(x)
}
"#;
    let a = "pub harness b(x: Untrusted<Str>) -> Untrusted<Str> {\n  let s = trust(x, because: \"a\")\n  x\n}\n";
    let ab = "pub harness c(x: Untrusted<Str>) -> Untrusted<Str> {\n  x\n}\n";
    let builtin = MemoryRegistry::new();
    let name = QualifiedName::parse("user:app").unwrap();
    // Only package `a` exists (harness `b.c`): it is followed, and the list is complete.
    let registry = registry_with(&[("app", "main.ratio", main), ("a", "main.ratio", a)]);
    let done = ratio_check::harnesses_check(&registry, &builtin, &name);
    assert!(done.complete);
    assert_eq!(done.trust_sites.len(), 1);
    // `a.b` exists too: the longest reading (`a.b`, harness `c`) wins, as at run time, and
    // the name is ambiguous, so the list is incomplete.
    let registry = registry_with(&[
        ("app", "main.ratio", main),
        ("a", "main.ratio", a),
        ("a.b", "main.ratio", ab),
    ]);
    let done = ratio_check::harnesses_check(&registry, &builtin, &name);
    assert!(!done.complete);
    assert_eq!(done.loaded_packages, vec!["user:a.b".to_owned()]);
    assert_eq!(done.trust_sites.len(), 0);
}

#[test]
fn recursive_enum_definitions_never_share_a_name() {
    let (loaded, mut checked) = check(
        "enum A_2 { L, N { k: [A_2] } }\nenum A { L, N { k: [A] } }\ntype R = { x: A_2, y: A }\n",
    );
    let t = alias(&loaded, &checked, "R");
    let got = checked.schema(t).unwrap();
    assert_eq!(
        got,
        concat!(
            r##"{"type":"object","properties":{"x":{"$ref":"#/$defs/A_2"},"y":{"$ref":"#/$defs/A"}},"required":["x","y"],"additionalProperties":false,"$defs":{"##,
            r##""A_2":{"oneOf":[{"type":"object","properties":{"kind":{"const":"l"}},"required":["kind"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"n"},"k":{"type":"array","items":{"$ref":"#/$defs/A_2"}}},"required":["kind","k"],"additionalProperties":false}]},"##,
            r##""A":{"oneOf":[{"type":"object","properties":{"kind":{"const":"l"}},"required":["kind"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"n"},"k":{"type":"array","items":{"$ref":"#/$defs/A"}}},"required":["kind","k"],"additionalProperties":false}]}}}"##
        )
    );
    // The reverse order of declaration: `A` then `A_2` again get distinct names.
    let (loaded, mut checked) = check(
        "enum A { L, N { k: [A] } }\nenum B { L, N { k: [B] } }\ntype R = { x: A, y: A2 }\ntype A2 = A_2\nenum A_2 { L, N { k: [A_2] } }\n",
    );
    let t = alias(&loaded, &checked, "R");
    let got = checked.schema(t).unwrap();
    assert!(
        got.contains(r#""$defs":{"A":"#) && got.contains(r#","A_2":"#),
        "{got}"
    );
}

#[test]
fn regular_expressions_in_patterns_are_checked() {
    for (pattern, ok) in [
        ("^[a-z]+$", true),
        (r"^(?:ab|c){2,3}\d*$", true),
        (r"[\]]x", true),
        ("(", false),
        ("a)", false),
        ("[a-", false),
        ("*a", false),
        ("a{3,1}", false),
        ("(?=a)", false),
        (r"\1", false),
        ("a\\", false),
    ] {
        assert_eq!(
            ratio_check::regex_problem(pattern).is_none(),
            ok,
            "{pattern}: {:?}",
            ratio_check::regex_problem(pattern)
        );
    }
}

#[test]
fn a_load_whose_name_a_closure_computes_is_incomplete() {
    let main = r#"
type H = harness(Untrusted<Str>) -> Untrusted<Str>

pub harness run(x: Untrusted<Str>) -> Untrusted<Str> {
  let load = |name: Str| meta.harness::<H>(name)
  let h = load("user:critics.strict")
  h(x)
}
"#;
    let critics = "pub harness strict(x: Untrusted<Str>) -> Untrusted<Str> {\n  let s = trust(x, because: \"rules\")\n  x\n}\n";
    let registry = registry_with(&[
        ("app", "main.ratio", main),
        ("critics", "main.ratio", critics),
    ]);
    let builtin = MemoryRegistry::new();
    let done = ratio_check::harnesses_check(
        &registry,
        &builtin,
        &QualifiedName::parse("user:app").unwrap(),
    );
    assert!(done.summary.clean, "{:?}", done.checked.diagnostics);
    assert_eq!(done.summary.loads, vec![None]);
    assert!(
        !done.complete,
        "the load's name is computed, so its sites are unknown"
    );
}
