//! Loading and resolution beyond the corpus: what only a host or a limit can trigger, which
//! package a name resolved to, the trust rules as a table, and hostile import graphs.

use std::cell::RefCell;
use std::fmt::Write as _;

use ratio_names::prelude::PreludeName;
use ratio_names::{
    Area, Binding, Code, Limits, Loaded, Loader, MemoryRegistry, PackageId, QualifiedName,
    Registry, RegistryError, RelPath, Resolution, Source,
};

fn path(text: &str) -> RelPath {
    RelPath::parse(text).unwrap()
}

/// A registry from `(source, package, path, text)` rows; `main.ratio` is each package's entry.
fn registry(files: &[(Source, &str, &str, &str)]) -> MemoryRegistry {
    let mut registry = MemoryRegistry::new();
    for (source, package, file, text) in files {
        let id = PackageId::new(source.clone(), *package);
        if *file == "main.ratio" {
            registry.set_entry(&id, path(file));
        }
        registry.add_file(&id, path(file), text.as_bytes());
    }
    registry
}

fn load(host: &dyn Registry, builtin: &dyn Registry, name: &str) -> Loaded {
    Loader::new(host, builtin).load(&QualifiedName::parse(name).unwrap())
}

fn codes(loaded: &Loaded) -> Vec<Code> {
    loaded.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn a_bare_name_resolves_to_std_before_user() {
    let builtin = registry(&[(
        Source::Std,
        "debate",
        "main.ratio",
        "pub fn judge() -> Int { 1 }",
    )]);
    let host = registry(&[
        (
            Source::User,
            "debate",
            "main.ratio",
            "pub fn judge() -> Int { 2 }",
        ),
        (Source::User, "app", "main.ratio", "import debate"),
    ]);
    let loaded = load(&host, &builtin, "user:app");
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics);
    let entry = loaded.module(loaded.entry.unwrap());
    let target = loaded.module(entry.imports[&0]);
    assert_eq!(target.package, PackageId::new(Source::Std, "debate"));
}

#[test]
fn the_host_registry_is_never_asked_for_std() {
    // A host that claims a `std` package does not get to supply it.
    let host = registry(&[
        (
            Source::Std,
            "debate",
            "main.ratio",
            "pub fn judge() -> Int { 1 }",
        ),
        (
            Source::User,
            "app",
            "main.ratio",
            "import std:debate\nimport debate as d",
        ),
    ]);
    let loaded = load(&host, &MemoryRegistry::new(), "user:app");
    assert_eq!(
        codes(&loaded),
        [Code::PackageNotFound, Code::PackageNotFound]
    );
}

#[test]
fn the_trust_rules_per_source() {
    let plugin = |p: &str| Source::Plugin(p.to_owned());
    let session = Source::Authored(Area::Session);
    let env = Source::Authored(Area::Env);
    let all = [
        Source::Std,
        Source::User,
        plugin("acme"),
        plugin("other"),
        Source::Project,
        session.clone(),
        env.clone(),
    ];
    let allowed: [(Source, Vec<Source>); 5] = [
        (Source::Std, vec![Source::Std]),
        (
            Source::User,
            vec![
                Source::Std,
                Source::User,
                plugin("acme"),
                plugin("other"),
                Source::Project,
            ],
        ),
        (plugin("acme"), vec![Source::Std, plugin("acme")]),
        (Source::Project, vec![Source::Std, Source::Project]),
        (session.clone(), vec![Source::Std, session]),
    ];
    for (importer, targets) in &allowed {
        for target in &all {
            assert_eq!(
                importer.may_import(target),
                targets.contains(target),
                "{importer} importing {target}"
            );
        }
    }
    assert!(env.may_import(&env) && !env.may_import(&Source::Authored(Area::Session)));
}

#[test]
fn links_are_refused() {
    let mut host = registry(&[(Source::User, "app", "main.ratio", "import \"lib/a.ratio\"")]);
    host.add_link(&PackageId::new(Source::User, "app"), path("lib/a.ratio"));
    let loaded = load(&host, &MemoryRegistry::new(), "user:app");
    assert_eq!(codes(&loaded), [Code::Link]);
}

struct Failing;

impl Registry for Failing {
    fn find(&self, _: &Source, _: &str) -> Result<Option<RelPath>, RegistryError> {
        Ok(Some(path("main.ratio")))
    }
    fn read(&self, _: &PackageId, _: &RelPath, _: usize) -> Result<Vec<u8>, RegistryError> {
        Err(RegistryError::Other("disk on fire".to_owned()))
    }
}

#[test]
fn unreadable_files_and_bad_encodings_are_reported() {
    let loaded = load(&Failing, &MemoryRegistry::new(), "user:app");
    assert_eq!(codes(&loaded), [Code::Unreadable]);
    assert!(loaded.entry.is_none() && !loaded.is_clean());
    assert!(loaded.diagnostics[0].at.is_none());

    let mut host = registry(&[(Source::User, "app", "main.ratio", "import \"bad.ratio\"")]);
    host.add_file(
        &PackageId::new(Source::User, "app"),
        path("bad.ratio"),
        vec![0xff, 0xfe],
    );
    assert_eq!(
        codes(&load(&host, &MemoryRegistry::new(), "user:app")),
        [Code::Unreadable]
    );
}

#[test]
fn files_and_packages_over_their_size_limits_are_refused() {
    let big = format!("// {}\n", "x".repeat(300 * 1024));
    let host = registry(&[
        (Source::User, "app", "main.ratio", "import \"big.ratio\""),
        (Source::User, "app", "big.ratio", &big),
    ]);
    assert_eq!(
        codes(&load(&host, &MemoryRegistry::new(), "user:app")),
        [Code::TooLarge]
    );

    // An authored package holds at most 64 KiB in all.
    let half = format!("// {}\n", "x".repeat(40 * 1024));
    let host = registry(&[
        (
            Source::Authored(Area::Session),
            "vote",
            "main.ratio",
            &format!("import \"more.ratio\"\n{half}"),
        ),
        (Source::Authored(Area::Session), "vote", "more.ratio", &half),
    ]);
    assert_eq!(
        codes(&load(
            &host,
            &MemoryRegistry::new(),
            "authored:session/vote"
        )),
        [Code::TooLarge]
    );
}

/// A chain `main -> f1 -> f2 -> … -> f{n}` inside one package.
fn chain(n: usize) -> MemoryRegistry {
    let id = PackageId::new(Source::User, "app");
    let mut host = MemoryRegistry::new();
    host.set_entry(&id, path("main.ratio"));
    host.add_file(&id, path("main.ratio"), "import \"f1.ratio\"");
    for i in 1..=n {
        let text = if i < n {
            format!("import \"f{}.ratio\"", i + 1)
        } else {
            String::new()
        };
        host.add_file(&id, path(&format!("f{i}.ratio")), text);
    }
    host
}

#[test]
fn import_depth_modules_and_packages_are_bounded() {
    let builtin = MemoryRegistry::new();
    let loaded = load(&chain(10), &builtin, "user:app");
    assert!(loaded.is_clean());

    // The default depth (64) stops a long chain without exhausting the stack.
    let loaded = load(&chain(200), &builtin, "user:app");
    assert_eq!(codes(&loaded), [Code::LimitExceeded]);

    let small = |limits: Limits| {
        let loaded = Loader::new(&chain(10), &builtin)
            .with_limits(limits)
            .load(&QualifiedName::parse("user:app").unwrap());
        codes(&loaded)
    };
    let modules = Limits {
        max_modules: 4,
        ..Limits::default()
    };
    assert_eq!(small(modules), [Code::LimitExceeded]);
    let depth = Limits {
        max_import_depth: 3,
        ..Limits::default()
    };
    assert_eq!(small(depth), [Code::LimitExceeded]);

    let host = registry(&[
        (
            Source::User,
            "app",
            "main.ratio",
            "import user:a\nimport user:b",
        ),
        (Source::User, "a", "main.ratio", ""),
        (Source::User, "b", "main.ratio", ""),
    ]);
    let loaded = Loader::new(&host, &builtin)
        .with_limits(Limits {
            max_packages: 2,
            ..Limits::default()
        })
        .load(&QualifiedName::parse("user:app").unwrap());
    assert_eq!(codes(&loaded), [Code::LimitExceeded]);
}

#[test]
fn a_file_imported_many_times_is_read_once() {
    struct Counting(MemoryRegistry, RefCell<Vec<String>>);
    impl Registry for Counting {
        fn find(&self, s: &Source, n: &str) -> Result<Option<RelPath>, RegistryError> {
            self.0.find(s, n)
        }
        fn read(
            &self,
            p: &PackageId,
            path: &RelPath,
            max: usize,
        ) -> Result<Vec<u8>, RegistryError> {
            // Every path the loader asks for is already normalised.
            assert_eq!(RelPath::parse(path.as_str()).as_ref(), Ok(path));
            self.1.borrow_mut().push(path.to_string());
            self.0.read(p, path, max)
        }
    }
    let mut main = String::new();
    for i in 0..500 {
        writeln!(main, "import \"./x/../lib.ratio\" as l{i}").unwrap();
    }
    let host = Counting(
        registry(&[
            (Source::User, "app", "main.ratio", &main),
            (Source::User, "app", "lib.ratio", "pub fn f() -> Int { 1 }"),
        ]),
        RefCell::new(Vec::new()),
    );
    let loaded = load(&host, &MemoryRegistry::new(), "user:app");
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics.first());
    assert_eq!(loaded.modules.len(), 2);
    assert_eq!(*host.1.borrow(), ["main.ratio", "lib.ratio"]);
}

#[test]
fn module_names_resolve_to_items_then_imports_then_the_prelude() {
    let host = registry(&[
        (
            Source::User,
            "app",
            "main.ratio",
            "import \"lib.ratio\" { open }\nimport \"lib.ratio\" as lib\nfn retry() -> Int { 1 }",
        ),
        (
            Source::User,
            "app",
            "lib.ratio",
            "fn hidden() -> Int { 0 }\npub fn open() -> Int { 1 }",
        ),
    ]);
    let loaded = load(&host, &MemoryRegistry::new(), "user:app");
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics);
    let entry = loaded.module(loaded.entry.unwrap());
    let lib = entry.imports[&0];
    assert_eq!(entry.imports[&1], lib);
    let scope = &entry.scope;
    assert_eq!(
        scope.resolve("open"),
        Some(Resolution::Binding(Binding::Imported {
            import: 0,
            module: Some(lib),
            item: Some(1)
        }))
    );
    assert_eq!(
        scope.resolve("lib"),
        Some(Resolution::Binding(Binding::Module {
            import: 1,
            module: Some(lib)
        }))
    );
    // A module's own `retry` shadows the prelude's.
    assert_eq!(
        scope.resolve("retry"),
        Some(Resolution::Binding(Binding::Item(2)))
    );
    assert_eq!(
        scope.resolve("par_map"),
        Some(Resolution::Prelude(PreludeName::Function("par_map")))
    );
    assert_eq!(
        scope.resolve("text"),
        Some(Resolution::Prelude(PreludeName::Module("text")))
    );
    assert_eq!(scope.resolve("hidden"), None);
    assert_eq!(scope.iter().count(), 3);
}

#[test]
fn qualified_names_read_and_print() {
    for text in [
        "std:debate",
        "user:critics.strict",
        "plugin:acme/review@^1.2",
        "project:kit",
        "authored:vote",
        "authored:env/vote@=1.0.0",
        "debate",
        "std:consul-escalation",
    ] {
        assert_eq!(QualifiedName::parse(text).unwrap().to_string(), text);
    }
    for text in [
        "",
        "std:",
        "std:a/b",
        "plugin:acme",
        "plugin:a.b/c",
        "authored:team/x",
        "who:x",
        "a/b",
        "std:debate@1",
        "std:9x",
        "user:a..b",
    ] {
        assert!(QualifiedName::parse(text).is_err(), "{text}");
    }
}

#[test]
fn version_ranges_apply_to_the_entry_too() {
    let host = registry(&[(
        Source::User,
        "app",
        "main.ratio",
        "meta { version: \"0.3.1\" }",
    )]);
    let builtin = MemoryRegistry::new();
    assert!(load(&host, &builtin, "user:app@^0.3").is_clean());
    let loaded = load(&host, &builtin, "user:app@^0.4");
    assert_eq!(codes(&loaded), [Code::VersionMismatch]);
    assert!(loaded.entry.is_none());

    // A package refused for its version is not resolved: its duplicate `f` goes unreported.
    let host = registry(&[
        (Source::User, "app", "main.ratio", "import user:lib @\"^1\""),
        (
            Source::User,
            "lib",
            "main.ratio",
            "meta { version: \"2.0.0\" }
fn f() { 1 }
fn f() { 2 }",
        ),
    ]);
    assert_eq!(
        codes(&load(&host, &builtin, "user:app")),
        [Code::VersionMismatch]
    );
}
