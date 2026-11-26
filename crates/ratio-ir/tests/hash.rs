//! The IR hash: stable across platforms (a snapshot CI checks on Linux, Windows and macOS),
//! blind to layout and comments, sensitive to meaning, and numbering every expression once.

use ratio_ir::{ExprKind, IR_VERSION, Program, canon};
use ratio_names::{Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source};

fn lower(source: &str) -> Program {
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let main = RelPath::parse("main.ratio").unwrap();
    let mut registry = MemoryRegistry::new();
    registry.set_entry(&package, main.clone());
    registry.add_file(&package, main, source.as_bytes().to_vec());
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap());
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics);
    let checked = ratio_check::check(&loaded);
    assert!(
        checked.is_clean(),
        "{:?}",
        checked
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    ratio_ir::lower(&loaded, &checked).unwrap()
}

/// A program touching most of the IR: records, enums, matches, loops, closures, errors,
/// interpolation with a spec, a prompt, a harness, an effect, a JSON type argument.
const PROGRAM: &str = r#"
type Item = { name: Str, n: Int, tag?: Str "a tag" }
enum Verdict { Pass, Fail { why: Str } }

const LIMIT: Int = 3

fn score(items: [Item]) -> Int {
  var total = 0
  for it in items {
    if it.n > LIMIT { continue }
    total += it.n
  }
  total
}

fn verdict(v: Verdict) -> Str {
  match v {
    Pass => "pass",
    Fail { why } => "fail: {why:>8}",
  }
}

fn safe(n: Int) -> Int {
  try { [1, 2][n] } catch e { -1 }
}

fn decode(j: Json) -> Untrusted<Item> {
  json.decode::<Item>(j)
}

fn opened(x: Untrusted<Str>) -> Str {
  trust(x, because: "reviewed")
}

prompt ask(q: Str) {
  instruct "Answer briefly."
  task q
}

fn wrapped(n: Int) -> Result<Int> {
  Ok { value: n }
}

harness helper(n: Int) -> Int {
  let add = |x: Int| x + n
  add(1)
}
"#;

/// The IR hash of `PROGRAM` at `IR_VERSION` 1. It must be the same on every platform; it
/// changes only when the canonical form does (which bumps `IR_VERSION`) or the program does.
const SNAPSHOT: &str = "73e43aeee4222531754631aa73ddb8e322179a53356ec14e04bcc639ca8409b8";

#[test]
fn the_hash_is_stable_across_platforms() {
    assert_eq!(IR_VERSION, 1);
    let p = lower(PROGRAM);
    assert_eq!(p.packages.len(), 1);
    let text = canon::canonical_text(&p, 0).expect("within the depth");
    assert!(
        text.starts_with("ratio-ir 1\nstdlib 1\npackage \"user:app\"\nmodule \"main.ratio\"\n")
    );
    assert_eq!(ratio_check::sha256(text.as_bytes()), p.packages[0].hash);
    assert_eq!(
        p.packages[0].hash_hex(),
        SNAPSHOT,
        "canonical form:\n{text}"
    );
}

#[test]
fn layout_and_comments_do_not_change_the_hash() {
    let a = lower(PROGRAM);
    let spaced = PROGRAM.replace("  total\n", "  // the sum\n\n    total\n");
    let b = lower(&format!("// a header comment\n{spaced}"));
    assert_eq!(a.packages[0].hash, b.packages[0].hash);
}

#[test]
fn meaning_changes_the_hash() {
    let a = lower(PROGRAM);
    for (from, to) in [
        ("const LIMIT: Int = 3", "const LIMIT: Int = 4"),
        ("\"a tag\"", "\"the tag\""),
        ("{why:>8}", "{why:>9}"),
        ("Answer briefly.", "Answer at length."),
        ("x + n", "n + x"),
    ] {
        assert!(PROGRAM.contains(from));
        let b = lower(&PROGRAM.replace(from, to));
        assert_ne!(a.packages[0].hash, b.packages[0].hash, "{from} -> {to}");
    }
}

fn walk(e: &ratio_ir::Expr, ids: &mut Vec<u32>, effects: &mut usize) {
    ids.push(e.id);
    if let ExprKind::Effect { .. } = e.kind {
        *effects += 1;
    }
    // Children through the canonical text's order is enough here: collect every id.
    let mut kids: Vec<&ratio_ir::Expr> = Vec::new();
    collect(e, &mut kids);
    for k in kids {
        walk(k, ids, effects);
    }
}

fn collect<'a>(e: &'a ratio_ir::Expr, out: &mut Vec<&'a ratio_ir::Expr>) {
    use ratio_ir::{Callee, Catch, ListItem, RecordPart, Stmt};
    match &e.kind {
        ExprKind::Block(stmts) => {
            for s in stmts {
                match s {
                    Stmt::Let { value, .. }
                    | Stmt::Var { value, .. }
                    | Stmt::Defer(value)
                    | Stmt::Expr(value) => {
                        out.push(value);
                    }
                    Stmt::Assign { value, .. } => out.push(value),
                }
            }
        }
        ExprKind::Concat(xs) | ExprKind::Tuple(xs) => out.extend(xs),
        ExprKind::List(items) => out.extend(items.iter().map(|i| match i {
            ListItem::One(x) | ListItem::Spread(x) => x,
        })),
        ExprKind::Record { parts, .. } => out.extend(parts.iter().map(|p| match p {
            RecordPart::Field(_, x) | RecordPart::Spread(x) => x,
        })),
        ExprKind::Show { value, .. } | ExprKind::Field { object: value, .. } => out.push(value),
        ExprKind::Call { callee, args, .. } => {
            if let Callee::Value(f) = callee {
                out.push(f);
            }
            out.extend(args);
        }
        ExprKind::Effect { args, .. } => out.extend(args),
        ExprKind::Binary(_, a, b)
        | ExprKind::Index {
            object: a,
            index: b,
        } => {
            out.push(a);
            out.push(b);
        }
        ExprKind::If { cond, then, else_ } => {
            out.push(cond);
            out.push(then);
            out.extend(else_.as_deref());
        }
        ExprKind::Match { scrutinee, arms } => {
            out.push(scrutinee);
            out.extend(arms.iter().map(|a| &a.body));
        }
        ExprKind::For { iter, body, .. } => {
            out.push(iter);
            out.push(body);
        }
        ExprKind::Try { body, catch } => {
            out.push(body);
            if let Catch::Bind(_, b) = catch {
                out.push(b);
            }
        }
        ExprKind::Closure(f) => out.push(&f.body),
        _ => {}
    }
}

#[test]
fn every_expression_has_a_distinct_site() {
    let p = lower(PROGRAM);
    let mut ids = Vec::new();
    let mut effects = 0;
    for item in p.modules[0].items.iter().flatten() {
        match &item.kind {
            ratio_ir::ItemKind::Fn(f) | ratio_ir::ItemKind::Prompt(f) => {
                walk(&f.body, &mut ids, &mut effects);
            }
            ratio_ir::ItemKind::Harness { func, .. } => walk(&func.body, &mut ids, &mut effects),
            ratio_ir::ItemKind::Const { value, .. } => walk(value, &mut ids, &mut effects),
            ratio_ir::ItemKind::State { .. } => {}
        }
    }
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "two expressions share a site id");
    // `trust` is the program's one effect: an explicit node.
    assert_eq!(effects, 1);
}

#[test]
fn only_clean_programs_lower() {
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let main = RelPath::parse("main.ratio").unwrap();
    let mut registry = MemoryRegistry::new();
    registry.set_entry(&package, main.clone());
    registry.add_file(&package, main, b"fn f() -> Int { \"no\" }".to_vec());
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap());
    let checked = ratio_check::check(&loaded);
    assert_eq!(
        ratio_ir::lower(&loaded, &checked).unwrap_err(),
        ratio_ir::LowerError::NotClean
    );
}

#[test]
fn enum_type_parameters_are_in_the_form_in_declared_order() {
    let mut p = lower(PROGRAM);
    let text = canon::canonical_text(&p, 0).unwrap();
    assert!(text.contains("\"Result\" (params \"T\")"), "{text}");
    // `E<A, B>` and `E<B, A>` are different enums: their forms, and so hashes, differ.
    let result = p.enums.iter().position(|e| e.name == "Result").unwrap();
    p.enums[result].params = vec!["A".to_owned(), "B".to_owned()];
    let ab = canon::canonical_text(&p, 0).unwrap();
    p.enums[result].params = vec!["B".to_owned(), "A".to_owned()];
    let ba = canon::canonical_text(&p, 0).unwrap();
    assert_ne!(
        ratio_check::sha256(ab.as_bytes()),
        ratio_check::sha256(ba.as_bytes())
    );
}
