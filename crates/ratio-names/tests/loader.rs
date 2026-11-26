//! Loading and resolution beyond the corpus: what only a host or a limit can trigger, which
//! package a name resolved to, the trust rules as a table, and hostile import graphs.

use std::cell::RefCell;
use std::fmt::Write as _;

use ratio_names::prelude::PreludeName;
use ratio_names::{
    Area, Binding, Code, Limits, Loaded, Loader, MemoryRegistry, PackageId, PackageName,
    PluginName, QualifiedName, Registry, RegistryError, RelPath, Resolution, Source,
};

fn id(source: Source, name: &str) -> PackageId {
    PackageId::new(source, PackageName::parse(name).unwrap())
}

fn path(text: &str) -> RelPath {
    RelPath::parse(text).unwrap()
}

/// A registry from `(source, package, path, text)` rows; `main.ratio` is each package's entry.
fn registry(files: &[(Source, &str, &str, &str)]) -> MemoryRegistry {
    let mut registry = MemoryRegistry::new();
    for (source, package, file, text) in files {
        let id = id(source.clone(), package);
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
    let entry = loaded.module(loaded.entry.unwrap()).unwrap();
    let target = loaded.module(entry.imports[&0]).unwrap();
    assert_eq!(target.package, id(Source::Std, "debate"));
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
    let plugin = |p: &str| Source::Plugin(PluginName::parse(p).unwrap());
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
    host.add_link(&id(Source::User, "app"), path("lib/a.ratio"));
    let loaded = load(&host, &MemoryRegistry::new(), "user:app");
    assert_eq!(codes(&loaded), [Code::Link]);
}

struct Failing;

impl Registry for Failing {
    fn find(&self, _: &Source, _: &PackageName) -> Result<Option<RelPath>, RegistryError> {
        Ok(Some(path("main.ratio")))
    }
    fn read(&self, _: &PackageId, _: &RelPath, _: usize) -> Result<Vec<u8>, RegistryError> {
        Err(RegistryError::Other)
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
        &id(Source::User, "app"),
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
    let id = id(Source::User, "app");
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
        fn find(&self, s: &Source, n: &PackageName) -> Result<Option<RelPath>, RegistryError> {
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
    let entry = loaded.module(loaded.entry.unwrap()).unwrap();
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

/// A registry that records every call it gets.
struct Recording(MemoryRegistry, RefCell<usize>);

impl Registry for Recording {
    fn find(&self, s: &Source, n: &PackageName) -> Result<Option<RelPath>, RegistryError> {
        *self.1.borrow_mut() += 1;
        self.0.find(s, n)
    }
    fn read(&self, p: &PackageId, path: &RelPath, max: usize) -> Result<Vec<u8>, RegistryError> {
        *self.1.borrow_mut() += 1;
        self.0.read(p, path, max)
    }
}

#[test]
fn failed_lookups_and_reads_are_asked_once() {
    let mut main = String::new();
    for i in 0..300 {
        writeln!(main, "import \"missing.ratio\" as m{i}").unwrap();
        writeln!(main, "import \"link.ratio\" as l{i}").unwrap();
        writeln!(main, "import user:absent as u{i}").unwrap();
        writeln!(main, "import nowhere as n{i}").unwrap();
    }
    let mut inner = registry(&[(Source::User, "app", "main.ratio", &main)]);
    inner.add_link(&id(Source::User, "app"), path("link.ratio"));
    let host = Recording(inner, RefCell::new(0));
    let builtin = Recording(MemoryRegistry::new(), RefCell::new(0));
    let loaded = load(&host, &builtin, "user:app");
    assert_eq!(loaded.diagnostics.len(), 1200);
    // The entry's find and read, `missing.ratio`, `link.ratio`, `user:absent`, then `nowhere`
    // in user (std is the built-in registry's one call).
    assert_eq!(*host.1.borrow(), 6);
    assert_eq!(*builtin.1.borrow(), 1);
}

#[test]
fn registry_calls_are_capped() {
    let mut main = String::new();
    for i in 0..50 {
        writeln!(main, "import \"m{i}.ratio\"").unwrap();
    }
    let host = registry(&[(Source::User, "app", "main.ratio", &main)]);
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&host, &builtin)
        .with_limits(Limits {
            max_registry_calls: 12,
            ..Limits::default()
        })
        .load(&QualifiedName::parse("user:app").unwrap());
    let codes = codes(&loaded);
    assert_eq!(
        codes.iter().filter(|c| **c == Code::FileNotFound).count(),
        10
    );
    assert_eq!(
        codes.iter().filter(|c| **c == Code::LimitExceeded).count(),
        40
    );
}

#[test]
fn an_answer_longer_than_asked_for_fails_closed() {
    struct Ignoring;
    impl Registry for Ignoring {
        fn find(&self, _: &Source, _: &PackageName) -> Result<Option<RelPath>, RegistryError> {
            Ok(Some(path("main.ratio")))
        }
        fn read(&self, _: &PackageId, _: &RelPath, _: usize) -> Result<Vec<u8>, RegistryError> {
            Ok(vec![b' '; 300 * 1024])
        }
    }
    let loaded = load(&Ignoring, &MemoryRegistry::new(), "user:app");
    assert_eq!(codes(&loaded), [Code::TooLarge]);
    assert!(loaded.modules.is_empty());
}

#[test]
fn the_depth_limit_counts_modules_in_the_chain() {
    let builtin = MemoryRegistry::new();
    // `chain(n)` holds n + 1 modules: 64 load, 65 do not.
    assert!(load(&chain(63), &builtin, "user:app").is_clean());
    assert_eq!(
        codes(&load(&chain(64), &builtin, "user:app")),
        [Code::LimitExceeded]
    );
}

#[test]
fn names_outside_ascii_and_device_names_are_refused() {
    for text in [
        "user:con",
        "user:critics.NUL",
        "plugin:aux/review",
        "user:\u{3c3}",
        "user:\u{3c2}",
        "user:caf\u{e9}",
        "user:cafe\u{301}",
        "user:a\u{200b}b",
        "user:a\u{202e}b",
        "user:critic~1",
        "plugin:\u{430}cme/review",
        "plugin:a.b/review",
    ] {
        assert!(QualifiedName::parse(text).is_err(), "{text:?}");
    }
    // The same names written in an import are refused there too (the token reader already
    // refuses identifiers outside ASCII).
    let host = registry(&[(
        Source::User,
        "app",
        "main.ratio",
        "import user:con as a\nimport plugin:nul/review as b",
    )]);
    let loaded = load(&host, &MemoryRegistry::new(), "user:app");
    assert_eq!(codes(&loaded), [Code::InvalidName, Code::InvalidName]);
}

#[test]
fn an_authored_name_without_an_area_stays_in_the_importers_area() {
    let host = registry(&[
        (
            Source::Authored(Area::Env),
            "kept",
            "main.ratio",
            "import authored:helper",
        ),
        (Source::Authored(Area::Session), "helper", "main.ratio", ""),
        (Source::Authored(Area::Env), "helper", "main.ratio", ""),
        (
            Source::Authored(Area::Session),
            "vote",
            "main.ratio",
            "import authored:only_env",
        ),
        (Source::Authored(Area::Env), "only_env", "main.ratio", ""),
    ]);
    let builtin = MemoryRegistry::new();
    let loaded = load(&host, &builtin, "authored:env/kept");
    assert!(loaded.is_clean(), "{:?}", loaded.diagnostics);
    let entry = loaded.module(loaded.entry.unwrap()).unwrap();
    let target = loaded.module(entry.imports[&0]).unwrap();
    assert_eq!(target.package, id(Source::Authored(Area::Env), "helper"));
    // A session package does not reach into the environment's area by a bare authored name.
    let loaded = load(&host, &builtin, "authored:session/vote");
    assert_eq!(codes(&loaded), [Code::PackageNotFound]);
}
