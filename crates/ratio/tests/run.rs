//! A host's path through the facade: load, check, lower, prepare, run.

use std::sync::Arc;

#[test]
fn a_host_runs_a_pure_function_through_the_facade() {
    let package = ratio::PackageId::new(
        ratio::Source::User,
        ratio::names::PackageName::parse("app").unwrap(),
    );
    let main = ratio::RelPath::parse("main.ratio").unwrap();
    let mut registry = ratio::MemoryRegistry::new();
    registry.set_entry(&package, main.clone());
    registry.add_file(
        &package,
        main,
        b"fn twice(s: Str) -> Str { s + s }".to_vec(),
    );
    let builtin = ratio::MemoryRegistry::new();
    let loaded = ratio::Loader::new(&registry, &builtin)
        .load(&ratio::QualifiedName::parse("user:app").unwrap());
    let checked = ratio::check(&loaded);
    let program = ratio::lower(&loaded, &checked).unwrap();
    assert_eq!(ratio::IR_VERSION, 1);
    assert_eq!(ratio::STAGE, 6);
    assert_eq!(program.packages[0].hash_hex().len(), 64);
    let prepared = ratio::prepare(&program).unwrap();
    let entry = prepared.find("main.ratio", "twice").unwrap();
    let secret = ratio::Labels::source(0);
    let arg = ratio::Value::text("ab").with_labels(secret);
    let eval = ratio::evaluate(
        Arc::clone(&prepared),
        entry,
        vec![arg],
        ratio::Limits::default(),
        ratio::CancelToken::new(),
    )
    .unwrap();
    let ratio::Outcome::Returned(v) = ratio::block_on(eval).outcome else {
        panic!("raised")
    };
    assert_eq!(v.as_text(), Some("abab"));
    assert_eq!(v.labels(), secret);
}
