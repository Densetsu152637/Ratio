//! The checker's API (the side tables later stages read) and its behaviour on hostile input:
//! deep trees, large types built from small sources, long chains and wide declarations must be
//! checked (or refused with a diagnostic) quickly and without a panic or a stack overflow.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use ratio_check::{ArgSource, Checked, Code, Res, Ty, TypeId};
use ratio_names::{
    Loaded, Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source,
};

fn load(source: &str) -> Loaded {
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let main = RelPath::parse("main.ratio").unwrap();
    let mut registry = MemoryRegistry::new();
    registry.set_entry(&package, main.clone());
    registry.add_file(&package, main, source.as_bytes().to_vec());
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap());
    assert!(
        loaded.is_clean(),
        "the source must load cleanly: {:?} {:?}",
        loaded.diagnostics,
        loaded.modules.iter().map(|m| &m.syntax).collect::<Vec<_>>()
    );
    loaded
}

/// Checks `source` within `limit`, and returns the result with its codes.
fn check_within(source: &str, limit: Duration) -> (Checked, Vec<Code>) {
    let loaded = load(source);
    let start = Instant::now();
    let checked = ratio_check::check(&loaded);
    let took = start.elapsed();
    assert!(took < limit, "checking took {took:?}");
    assert!(
        checked.internal_errors().is_empty(),
        "{:?}",
        checked.internal_errors()
    );
    let codes = checked.diagnostics.iter().map(|d| d.code).collect();
    (checked, codes)
}

fn check(source: &str) -> (Checked, Vec<Code>) {
    check_within(source, Duration::from_secs(20))
}

/// The byte span of the first occurrence of `needle` in `source`.
fn span_of(source: &str, needle: &str) -> (usize, usize) {
    let start = source.find(needle).unwrap();
    (start, start + needle.len())
}

#[test]
fn every_expression_has_a_type_and_calls_record_their_instantiation() {
    let source =
        "fn f(xs: [Int]) -> [Str] {\n  let n = list.len(xs)\n  xs.map(|x| text.upper(\"a\"))\n}\n";
    let (checked, codes) = check(source);
    assert!(codes.is_empty(), "{:?}", checked.diagnostics);
    let table = &checked.tables[0];
    let types = &checked.types;
    let (start, end) = span_of(source, "list.len(xs)");
    let key = ratio_check::Key { start, end };
    assert_eq!(table.types[&key], TypeId::INT);
    let call = &table.calls[&key];
    assert_eq!(call.args, vec![ArgSource::Arg(0)]);
    assert_eq!(call.type_args, vec![TypeId::INT]);
    let callee = call.callee.expect("a declared callee");
    assert_eq!(
        checked.std_names()[match callee.origin {
            ratio_check::Origin::Std(s) => s,
            ratio_check::Origin::Module(_) => panic!("a built-in"),
        }],
        "list"
    );
    // The method call `xs.map(…)`: the receiver fills the first parameter.
    let (start, end) = span_of(source, "xs.map(|x| text.upper(\"a\"))");
    let call = &table.calls[&ratio_check::Key { start, end }];
    assert_eq!(call.args, vec![ArgSource::Receiver, ArgSource::Arg(0)]);
    assert_eq!(call.type_args, vec![TypeId::INT, TypeId::STR]);
    // `n` is a local, and the name `xs` in the body resolves to the parameter.
    assert!(
        table
            .locals
            .iter()
            .any(|l| l.name == "n" && l.ty == TypeId::INT)
    );
    let (start, end) = span_of(source, "xs)");
    let res = &table.res[&ratio_check::Key {
        start,
        end: end - 1,
    }];
    assert!(matches!(res, Res::Local(_)));
    let list_of_str = table.types[&ratio_check::Key {
        start: source.find("xs.map").unwrap(),
        end: source.rfind(')').unwrap() + 1,
    }];
    assert!(matches!(types.get(list_of_str), Ty::List(t) if *t == TypeId::STR));
}

#[test]
fn contextual_variants_and_coercions_are_recorded() {
    let source = "fn f() -> GenOpts {\n  { effort: high, model: \"consul\" }\n}\n";
    let (checked, codes) = check(source);
    assert!(codes.is_empty(), "{:?}", checked.diagnostics);
    let table = &checked.tables[0];
    let (start, end) = span_of(source, "high");
    assert!(matches!(
        table.res[&ratio_check::Key { start, end }],
        Res::Variant(..)
    ));
    let (start, end) = span_of(source, "\"consul\"");
    assert_eq!(
        table.coercions[&ratio_check::Key { start, end }],
        ratio_check::Coercion::ModelName
    );
}

#[test]
fn deep_expressions_do_not_overflow_the_stack() {
    let depth = 120;
    let source = format!(
        "fn f(c: Bool) -> Int {{\n  {}1{}\n}}\n",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    let (_, codes) = check(&source);
    assert!(codes.is_empty(), "{codes:?}");
    let ifs = format!(
        "fn f(c: Bool) -> Int {{\n  {}1{}\n}}\n",
        "if c { ".repeat(60),
        " } else { 0 }".repeat(60)
    );
    let (_, codes) = check(&ifs);
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn types_that_double_at_every_alias_stay_cheap() {
    // Each alias is a pair of the previous one: the type written out has 2^100 leaves, but
    // interned it has 100 nodes, and relating two such types is memoised.
    let mut source = String::from("type A0 = Str\ntype B0 = UserText\n");
    for i in 1..=100 {
        let _ = write!(
            source,
            "type A{i} = (A{p}, A{p})\ntype B{i} = (B{p}, B{p})\n",
            p = i - 1
        );
    }
    source.push_str("fn widen(b: B100) -> A100 {\n  b\n}\n");
    source.push_str("fn narrow(a: A100) -> B100 {\n  a\n}\n");
    source.push_str("fn taint(a: A100) -> Untrusted<A100> {\n  a\n}\n");
    let (checked, codes) = check_within(&source, Duration::from_secs(5));
    assert_eq!(codes, vec![Code::Mismatch], "{:?}", checked.diagnostics);
    assert!(checked.diagnostics[0].message.len() < 400);
}

#[test]
fn long_alias_and_constant_chains_are_bounded() {
    let mut aliases = String::new();
    for i in 0..300 {
        let _ = writeln!(aliases, "type T{i} = T{}", i + 1);
    }
    aliases.push_str("type T300 = Int\n");
    let (_, codes) = check(&aliases);
    assert!(codes.contains(&Code::TooComplex), "{codes:?}");

    let mut consts = String::new();
    for i in 0..300 {
        let _ = writeln!(consts, "const C{i} = C{} + 1", i + 1);
    }
    consts.push_str("const C300 = 0\n");
    let (_, codes) = check(&consts);
    assert!(codes.contains(&Code::TooComplex), "{codes:?}");

    // In dependency order, the same chains are fine.
    let mut forward = String::from("type T0 = Int\nconst C0 = 0\n");
    for i in 1..300 {
        let _ = write!(
            forward,
            "type T{i} = T{p}\nconst C{i} = C{p} + 1\n",
            p = i - 1
        );
    }
    let (_, codes) = check(&forward);
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn generic_instantiation_depth_is_bounded() {
    let mut source =
        String::from("fn wrap<T>(x: T) -> [T] {\n  [x]\n}\n\nfn f() -> Int {\n  let a0 = 1\n");
    for i in 1..=200 {
        let _ = writeln!(source, "  let a{i} = wrap(a{})", i - 1);
    }
    source.push_str("  0\n}\n");
    let (_, codes) = check(&source);
    assert!(codes.contains(&Code::TooComplex), "{codes:?}");
}

#[test]
fn wide_declarations_and_long_bodies_check_in_linear_time() {
    let n = 2500;
    let fields: Vec<String> = (0..n).map(|i| format!("f{i}: Int")).collect();
    let values: Vec<String> = (0..n).map(|i| format!("f{i}: {i}")).collect();
    let variants: Vec<String> = (0..n).map(|i| format!("v{i}")).collect();
    let arms: Vec<String> = (0..n).map(|i| format!("    v{i} => {i},")).collect();
    let mut source = format!(
        "type Wide = {{ {} }}\nenum Many {{ {} }}\nfn make() -> Wide {{\n  {{ {} }}\n}}\n",
        fields.join(", "),
        variants.join(", "),
        values.join(", ")
    );
    let _ = write!(
        source,
        "fn pick(m: Many) -> Int {{\n  match m {{\n{}\n  }}\n}}\n",
        arms.join("\n")
    );
    source.push_str("fn body() -> Int {\n  let x0 = 0\n");
    for i in 1..n {
        let _ = writeln!(source, "  let x{i} = x{} + 1", i - 1);
    }
    source.push_str("  0\n}\n");
    let (checked, codes) = check_within(&source, Duration::from_secs(10));
    assert!(
        codes.is_empty(),
        "{:?}",
        &checked.diagnostics[..codes.len().min(3)]
    );
}

#[test]
fn exhaustiveness_on_a_huge_or_pattern_is_refused_not_hung() {
    let bools = ["true | false"; 8].join(", ");
    let source = format!(
        "fn f(t: ((Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool), (Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool), (Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool))) -> Int {{\n  match t {{\n    (({bools}), ({bools}), ({bools})) => 1,\n  }}\n}}\n"
    );
    let (_, codes) = check_within(&source, Duration::from_secs(5));
    assert_eq!(codes, vec![Code::MatchTooComplex]);
}

#[test]
fn every_code_is_documented_and_round_trips() {
    for code in Code::ALL {
        assert_eq!(Code::from_code(code.as_str()), Some(*code));
    }
}

#[test]
fn on_a_small_stack_the_fallback_refuses_depth_instead_of_overflowing() {
    // As the syntax crate's fallback test does: a 512 KiB thread, the smallest stack in use.
    let deep = format!(
        "type T = {}Int{}\nfn f(c: Bool) -> Int {{\n  {}1{}\n}}\n",
        "[".repeat(100),
        "]".repeat(100),
        "(".repeat(100),
        ")".repeat(100)
    );
    let shallow =
        "fn f(x: Int) -> Int {\n  match (x, x) {\n    (1, y) => y,\n    _ => 0,\n  }\n}\n";
    let (deep, shallow) = (load(&deep), load(shallow));
    let codes = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let deep = ratio_check::fallback::check(&deep);
            let shallow = ratio_check::fallback::check(&shallow);
            (
                deep.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
                shallow
                    .diagnostics
                    .iter()
                    .map(|d| d.code)
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(codes.0.contains(&Code::TooComplex), "{:?}", codes.0);
    assert!(codes.1.is_empty(), "{:?}", codes.1);
}

#[test]
fn many_exponential_matches_are_refused_once_and_quickly() {
    // Each match alone is over its bound; after the first is refused the rest are skipped, and
    // all of them draw on the check's one budget, so the package costs one bound, not 300.
    let bools = ["true | false"; 8].join(", ");
    let b8 = "(Bool, Bool, Bool, Bool, Bool, Bool, Bool, Bool)";
    let mut source = String::new();
    for i in 0..300 {
        let _ = writeln!(
            source,
            "fn f{i}(t: ({b8}, {b8}, {b8})) -> Int {{\n  match t {{\n    (({bools}), ({bools}), ({bools})) => 1,\n  }}\n}}"
        );
    }
    let (_, codes) = check_within(&source, Duration::from_secs(5));
    assert_eq!(codes, vec![Code::MatchTooComplex]);
}

#[test]
fn a_budget_spent_by_the_last_relation_still_refuses_the_package() {
    // The body's last relation (`UserText <: Str`, the result against the declared type)
    // comes after its last expression was checked. Find the smallest budget that checks
    // cleanly; one unit less runs out exactly there, and the package must still be refused
    // with `C0022`, reported once at the module.
    let loaded = load("fn f(x: UserText) -> Str {\n  x\n}\n");
    let clean = (0..100_000u64)
        .find(|fuel| ratio_check::budget::check(&loaded, *fuel).is_clean())
        .expect("some budget checks cleanly");
    assert!(clean > 0);
    let short = ratio_check::budget::check(&loaded, clean - 1);
    let too_complex: Vec<_> = short
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::TooComplex)
        .collect();
    assert_eq!(too_complex.len(), 1, "{:?}", short.diagnostics);
    assert_eq!(
        too_complex[0].span.start, 0,
        "reported at the module, after the passes"
    );
    assert!(!short.is_clean());
}

#[test]
fn none_is_the_context_mode_variant_only_where_context_mode_is_expected() {
    let source = "fn cfg() -> HarnessConfig {\n  { context: none }\n}\n\nfn absent() -> ContextMode? {\n  let c: ContextMode? = none\n  c\n}\n";
    let (checked, codes) = check(source);
    assert!(codes.is_empty(), "{:?}", checked.diagnostics);
    let table = &checked.tables[0];
    let first = source.find("none").unwrap();
    let key = ratio_check::Key {
        start: first,
        end: first + 4,
    };
    match table.res.get(&key) {
        Some(Res::Variant(id, index)) => {
            let def = checked.types.enum_def(*id);
            assert_eq!(def.name, "ContextMode");
            assert_eq!(def.variants[*index].name, "none");
        }
        other => panic!("`context: none` resolved to {other:?}"),
    }
    let second = source.rfind("= none").unwrap() + 2;
    let key = ratio_check::Key {
        start: second,
        end: second + 4,
    };
    assert!(
        !table.res.contains_key(&key),
        "`none` for a `ContextMode?` is absence"
    );
    let ty = table.types[&key];
    assert!(
        matches!(checked.types.get(ty), Ty::Option(inner) if *inner == TypeId::NEVER),
        "{}",
        checked.types.display(ty)
    );
}
