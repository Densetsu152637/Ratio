//! Attenuation (chapter 04, decision R10; stage 10): the monotonicity property over randomly
//! generated parent scopes and requests in every dimension, and the host's independent check
//! ([`check_within`]) refusing a child computed from a forged, wider parent.

use std::collections::BTreeSet;
use std::sync::Arc;

use ratio_eval::frame::{
    BudgetKind, Caps, Context, Dimension, Folder, FrameKind, Location, Mode, Paths, Request, Rules,
    Scope, Sees, SeesItem, SeesPatch, ToolPat, ToolRequest, ToolSet, attenuate, check_within,
};

/// xorshift64*, so a failure reproduces from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn coin(&mut self) -> bool {
        self.below(2) == 0
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[usize::try_from(self.below(xs.len() as u64)).unwrap()]
    }
}

const TOOLS: [&str; 12] = [
    "read_file",
    "grep",
    "glob",
    "web_fetch",
    "web_search",
    "mcp__github__issues",
    "mcp__github__pulls",
    "mcp__slack__post",
    "mcp__*",
    "mcp__github__*",
    "web_*",
    "*",
];
const FOLDERS: [&str; 16] = [
    "/repo",
    "/repo/src",
    "/repo/src/a",
    "/repo/docs",
    "/tmp",
    "/",
    "/a",
    "/ab",
    "C:/Repo",
    "c:\\Repo\\src",
    "C:/repo",
    "D:/",
    "d:/",
    "//server/share/x",
    "\\\\SERVER\\Share",
    "//server/other",
];
const RULES: [&str; 5] = [
    "Bash(rm:*)",
    "WebFetch",
    "Write(/etc/**)",
    "mcp__slack__*",
    "Edit",
];

fn tool_set(r: &mut Rng, max: u64) -> ToolSet {
    let n = r.below(max + 1);
    ToolSet::of((0..n).map(|_| ToolPat::parse(*r.pick(&TOOLS[..])).unwrap()))
}

fn rules(r: &mut Rng) -> BTreeSet<Arc<str>> {
    (0..r.below(3))
        .map(|_| Arc::from(*r.pick(&RULES)))
        .collect()
}

fn caps(r: &mut Rng) -> Caps {
    let mut c = Caps::NONE;
    for k in BudgetKind::ALL {
        if r.coin() {
            c.set(k, Some(r.below(1_000)));
        }
    }
    c
}

fn sees(r: &mut Rng) -> Sees {
    Sees::from_bits(u16::try_from(r.below(1 << 9)).unwrap())
}

fn folders(r: &mut Rng) -> Vec<Folder> {
    (0..r.below(3))
        .map(|_| Folder::parse(*r.pick(&FOLDERS[..])).unwrap())
        .collect()
}

fn scope(r: &mut Rng) -> Scope {
    let depth = u32::try_from(r.below(4)).unwrap();
    Scope {
        tools: tool_set(r, 4),
        mode: *r.pick(&Mode::ALL),
        rules: Rules {
            allow: rules(r),
            ask: rules(r),
            deny: rules(r),
        },
        paths: if r.coin() {
            Paths::Any
        } else {
            Paths::Within(folders(r).into_iter().collect())
        },
        location: *r.pick(&Location::ALL),
        budget: caps(r),
        sees: sees(r),
        depth,
        max_depth: depth + u32::try_from(r.below(4)).unwrap(),
        width: 1 + u32::try_from(r.below(20)).unwrap(),
        heap: r.coin().then(|| r.below(1 << 20)),
        attended: r.coin(),
        memory_writes: r.coin(),
        context: *r.pick(&Context::ALL),
    }
}

fn request(r: &mut Rng) -> Request {
    let mut q = Request::default();
    if r.coin() {
        q.tools = Some(ToolRequest {
            need: tool_set(r, 2),
            want: tool_set(r, 3),
        });
    }
    if r.coin() {
        q.mode = Some(*r.pick(&Mode::ALL));
    }
    q.deny = rules(r).into_iter().collect();
    q.ask = rules(r).into_iter().collect();
    if r.coin() {
        q.paths = Some(folders(r));
    }
    if r.coin() {
        q.location = Some(*r.pick(&Location::ALL));
    }
    q.budget = caps(r);
    if r.coin() {
        q.depth = Some(u32::try_from(r.below(10)).unwrap());
    }
    if r.coin() {
        q.width = Some(1 + u32::try_from(r.below(30)).unwrap());
    }
    if r.coin() {
        q.heap = Some(r.below(1 << 21));
    }
    if r.coin() {
        q.context = Some(*r.pick(&Context::ALL));
    }
    q.sees = SeesPatch {
        set: if r.below(3) == 0 { sees(r) } else { Sees::NONE },
        clear: if r.below(3) == 0 { sees(r) } else { Sees::NONE },
    };
    if r.coin() {
        q.attended = Some(r.coin());
    }
    q
}

const KINDS: [FrameKind; 3] = [FrameKind::Entry, FrameKind::Harness, FrameKind::Block];

/// Every dimension of `child` is within `parent`'s, checked dimension by dimension here
/// (independently of `check_within`).
fn assert_within(parent: &Scope, child: &Scope, seed: u64) {
    assert!(child.tools.within(&parent.tools), "tools ({seed})");
    for p in child.tools.iter() {
        assert!(parent.tools.covers(p), "tool {p} ({seed})");
    }
    assert!(child.mode.within(parent.mode), "mode ({seed})");
    assert!(
        child.rules.allow.is_subset(&parent.rules.allow),
        "allow ({seed})"
    );
    assert!(
        child.rules.ask.is_superset(&parent.rules.ask),
        "ask ({seed})"
    );
    assert!(
        child.rules.deny.is_superset(&parent.rules.deny),
        "deny ({seed})"
    );
    assert!(paths_within(&child.paths, &parent.paths), "paths ({seed})");
    assert!(child.location.within(parent.location), "location ({seed})");
    for k in BudgetKind::ALL {
        if let Some(p) = parent.budget.get(k) {
            assert!(
                child.budget.get(k).is_some_and(|c| c <= p),
                "budget {} ({seed})",
                k.name()
            );
        }
    }
    for i in SeesItem::ALL {
        assert!(!child.sees.has(i) || parent.sees.has(i), "sees ({seed})");
    }
    assert!(child.max_depth <= parent.max_depth, "depth ({seed})");
    assert!(child.width <= parent.width, "width ({seed})");
    if let Some(p) = parent.heap {
        assert!(child.heap.is_some_and(|c| c <= p), "heap ({seed})");
    }
    assert!(!child.attended || parent.attended, "attended ({seed})");
    assert!(
        !child.memory_writes || parent.memory_writes,
        "memory writes ({seed})"
    );
}

#[test]
fn attenuation_is_monotone_in_every_dimension() {
    let mut opened = 0;
    for seed in 1..=20_000u64 {
        let mut r = Rng(seed);
        let parent = scope(&mut r);
        let room = caps(&mut r).meet(&parent.budget);
        let req = request(&mut r);
        let kind = *r.pick(&KINDS);
        let Ok(child) = attenuate(&parent, &room, &req, kind) else {
            continue;
        };
        opened += 1;
        assert_within(&parent, &child, seed);
        assert_eq!(check_within(&parent, &child, kind), Ok(()), "seed {seed}");
        // Budgets are also within what the parent's ledgers had left.
        assert_eq!(child.budget.wider_than(&room), None, "room ({seed})");
        // A callee never gets a tool it did not ask for.
        if let Some(t) = &req.tools {
            let asked = ToolSet::of(t.need.iter().chain(t.want.iter()).cloned());
            assert!(child.tools.within(&asked), "asked tools ({seed})");
        }
        // Monotone in the parent: a narrower parent never gives a wider child.
        let narrower = attenuate(&parent, &Caps::NONE, &request(&mut r), FrameKind::Block);
        if let Ok(narrow) = narrower
            && let Ok(grandchild) = attenuate(&narrow, &Caps::NONE, &req, kind)
        {
            assert_within(&parent, &grandchild, seed);
        }
    }
    assert!(opened > 5_000, "too few frames opened ({opened})");
}

#[test]
fn a_chain_of_frames_only_narrows() {
    for seed in 1..=2_000u64 {
        let mut r = Rng(seed);
        let root = scope(&mut r);
        let mut at = root.clone();
        for _ in 0..8 {
            let kind = *r.pick(&[FrameKind::Harness, FrameKind::Block]);
            match attenuate(&at, &Caps::NONE, &request(&mut r), kind) {
                Ok(child) => {
                    assert_eq!(check_within(&at, &child, kind), Ok(()), "seed {seed}");
                    assert_within(&root, &child, seed);
                    at = child;
                }
                Err(_) => break,
            }
        }
    }
}

/// Widens `child` in one dimension beyond `parent`, if that dimension can be widened.
fn widen(parent: &Scope, child: &mut Scope, dim: usize) -> Option<Dimension> {
    match dim {
        0 => {
            let extra = TOOLS
                .iter()
                .map(|t| ToolPat::parse(t).unwrap())
                .find(|t| !parent.tools.covers(t))?;
            child.tools = ToolSet::of(child.tools.iter().cloned().chain([extra]));
            Some(Dimension::Tools)
        }
        1 => {
            let wider = Mode::ALL.into_iter().find(|m| !m.within(parent.mode))?;
            child.mode = wider;
            Some(Dimension::Mode)
        }
        2 => {
            let d = parent.rules.deny.iter().next()?.clone();
            child.rules.deny.remove(&d);
            Some(Dimension::Rules)
        }
        3 => {
            child.rules.allow.insert(Arc::from("Bash(*)-never-granted"));
            Some(Dimension::Rules)
        }
        4 => {
            if parent.paths == Paths::Any {
                return None;
            }
            child.paths = Paths::Any;
            Some(Dimension::Paths)
        }
        5 => {
            if parent.location == Location::Any {
                return None;
            }
            child.location = Location::Any;
            Some(Dimension::Location)
        }
        6 => {
            let k = BudgetKind::ALL
                .into_iter()
                .find(|&k| parent.budget.get(k).is_some())?;
            child
                .budget
                .set(k, parent.budget.get(k).map(|c| c.saturating_add(1)));
            Some(Dimension::Budget(k))
        }
        7 => {
            let i = SeesItem::ALL.into_iter().find(|&i| !parent.sees.has(i))?;
            child.sees = child.sees.with(i);
            Some(Dimension::Sees)
        }
        8 => {
            child.max_depth = parent.max_depth + 1;
            Some(Dimension::Depth)
        }
        9 => {
            child.width = parent.width + 1;
            Some(Dimension::Width)
        }
        10 => {
            let h = parent.heap?;
            child.heap = Some(h + 1);
            Some(Dimension::Heap)
        }
        11 => {
            if parent.attended {
                return None;
            }
            child.attended = true;
            Some(Dimension::Attended)
        }
        12 => {
            if parent.memory_writes {
                return None;
            }
            child.memory_writes = true;
            Some(Dimension::MemoryWrites)
        }
        _ => {
            child.context = Context::Main;
            Some(Dimension::Context)
        }
    }
}

#[test]
fn the_check_refuses_a_child_widened_in_any_dimension() {
    let mut refused = [0u32; 14];
    for seed in 1..=20_000u64 {
        let mut r = Rng(seed);
        let parent = scope(&mut r);
        let Ok(mut child) = attenuate(&parent, &Caps::NONE, &request(&mut r), FrameKind::Harness)
        else {
            continue;
        };
        let dim = usize::try_from(r.below(14)).unwrap();
        let Some(d) = widen(&parent, &mut child, dim) else {
            continue;
        };
        let got = check_within(&parent, &child, FrameKind::Harness);
        assert!(got.is_err(), "seed {seed}: widened {d:?} passed");
        // Depth is checked with its position; every other dimension is named exactly.
        if !matches!(d, Dimension::Depth) {
            assert_eq!(got, Err(d), "seed {seed}");
        }
        refused[dim] += 1;
    }
    assert!(
        refused.iter().all(|&n| n > 20),
        "every dimension was widened: {refused:?}"
    );
}

#[test]
fn a_child_of_a_forged_wider_parent_fails_the_hosts_check() {
    // Imperium's record of the parent: read-only tools, plan mode, local models, a token cap,
    // no secrets.
    let real = Scope {
        tools: ToolSet::of([ToolPat::parse("read_file").unwrap()]),
        mode: Mode::Plan,
        location: Location::Local,
        budget: Caps {
            tokens: Some(1_000),
            ..Caps::NONE
        },
        sees: Sees::NONE,
        paths: Paths::Within([Folder::parse("/repo/src").unwrap()].into()),
        attended: false,
        ..Scope::session()
    };
    // A buggy interpreter passes the session's scope as the parent instead.
    let forged = Scope::session();
    let req = Request {
        tools: Some(ToolRequest {
            need: ToolSet::of([ToolPat::parse("web_fetch").unwrap()]),
            want: ToolSet::none(),
        }),
        mode: Some(Mode::Full),
        location: Some(Location::Cloud),
        budget: Caps {
            tokens: Some(50_000),
            ..Caps::NONE
        },
        sees: SeesPatch {
            set: Sees::NONE.with(SeesItem::Secrets),
            clear: Sees::NONE,
        },
        paths: Some(vec![Folder::parse("/").unwrap()]),
        ..Request::default()
    };
    let child = attenuate(&forged, &Caps::NONE, &req, FrameKind::Harness).unwrap();
    assert_eq!(
        check_within(&real, &child, FrameKind::Harness),
        Err(Dimension::Tools)
    );
    // Each dimension on its own.
    for (fix, want) in [
        (0, Dimension::Mode),
        (1, Dimension::Paths),
        (2, Dimension::Location),
        (3, Dimension::Sees),
        (4, Dimension::Attended),
        (5, Dimension::Budget(BudgetKind::Tokens)),
    ] {
        let mut c = child.clone();
        c.tools = real.tools.clone();
        if fix > 0 {
            c.mode = real.mode;
        }
        if fix > 1 {
            c.paths = real.paths.clone();
        }
        if fix > 2 {
            c.location = real.location;
        }
        if fix > 3 {
            c.sees = real.sees;
        }
        if fix > 4 {
            c.attended = real.attended;
        }
        assert_eq!(check_within(&real, &c, FrameKind::Harness), Err(want));
    }
    // From the real parent, the same request is refused outright.
    assert!(attenuate(&real, &Caps::NONE, &req, FrameKind::Harness).is_err());
}

#[test]
fn modes_meet_as_the_specification_orders_them() {
    assert_eq!(Mode::Auto.meet(Mode::AcceptEdits), Mode::Default);
    assert_eq!(Mode::Full.meet(Mode::Plan), Mode::Plan);
    assert_eq!(Mode::Ask.meet(Mode::Auto), Mode::Ask);
    for a in Mode::ALL {
        for b in Mode::ALL {
            let m = a.meet(b);
            assert!(m.within(a) && m.within(b), "{a:?} {b:?}");
            assert_eq!(m, b.meet(a));
            // The greatest such mode.
            for c in Mode::ALL {
                if c.within(a) && c.within(b) {
                    assert!(c.within(m), "{a:?} {b:?} {c:?}");
                }
            }
        }
    }
}

#[test]
fn tool_patterns_are_parsed_exactly() {
    assert!(ToolPat::parse("mcp__*__x").is_none());
    assert!(ToolPat::parse("a b").is_none());
    assert!(ToolPat::parse("").is_none());
    assert_eq!(ToolPat::parse("*"), Some(ToolPat::Prefix(Arc::from(""))));
}

/// An oracle for folder containment, independent of `Folder::contains` and `Paths::within`:
/// each folder written out, normalised as text (separators, drive letters and UNC names
/// folded, a trailing `/`), and compared as a string prefix.
fn canon(text: &str) -> String {
    let t = text.replace('\\', "/");
    let mut out = String::new();
    let rest = if let Some(unc) = t.strip_prefix("//") {
        let mut it = unc.splitn(3, '/');
        let server = it.next().unwrap().to_lowercase();
        let share = it.next().unwrap().to_lowercase();
        out = format!("unc:{server}/{share}/");
        it.next().unwrap_or("").to_owned()
    } else if t.len() >= 2 && t.as_bytes()[1] == b':' {
        out = format!("drive:{}/", t[..1].to_uppercase());
        t[2..].to_owned()
    } else {
        out.push_str("posix:/");
        t[1..].to_owned()
    };
    for c in rest.split('/') {
        if !c.is_empty() && c != "." {
            out.push_str(c);
            out.push('/');
        }
    }
    out
}

fn paths_within(child: &Paths, parent: &Paths) -> bool {
    match (child, parent) {
        (_, Paths::Any) => true,
        (Paths::Any, Paths::Within(_)) => false,
        (Paths::Within(c), Paths::Within(p)) => c.iter().all(|f| {
            let f = canon(&f.to_string());
            p.iter().any(|g| f.starts_with(&canon(&g.to_string())))
        }),
    }
}

#[test]
fn the_folder_oracle_agrees_with_the_source_strings() {
    // The oracle read from the strings themselves, before any `Folder` is built.
    for a in FOLDERS {
        for b in FOLDERS {
            let want = canon(b).starts_with(&canon(a));
            let fa = Folder::parse(a).unwrap();
            let fb = Folder::parse(b).unwrap();
            assert_eq!(fa.contains(&fb), want, "{a} contains {b}");
            assert_eq!(canon(&fa.to_string()), canon(a), "{a} round-trips");
        }
    }
}

#[test]
fn folders_must_be_absolute_and_plain() {
    for bad in [
        "",
        ".",
        "./",
        "repo",
        "repo/src",
        "./repo",
        "..",
        "/repo/../etc",
        "C:repo",
        "C:..",
        "C:",
        "d:",
        "//server",
        "//server/",
        "/a/b:c",
        "/a/*",
        "/a/?",
        "\\\\?\\C:\\x",
        "\\\\.\\pipe",
    ] {
        assert!(Folder::parse(bad).is_none(), "`{bad}` was accepted");
    }
    let f = |t: &str| Folder::parse(t).unwrap();
    assert!(f("/a").contains(&f("/a/b")));
    assert!(!f("/a").contains(&f("/ab")), "`/a` does not contain `/ab`");
    assert!(
        f("C:/x").contains(&f("c:\\x\\y")),
        "drive letters ignore case"
    );
    assert!(!f("C:/x").contains(&f("D:/x")));
    assert!(!f("/").contains(&f("C:/x")), "a POSIX root is not a drive");
    assert!(!f("C:/").contains(&f("//server/share")));
    assert!(f("//Server/Share").contains(&f("\\\\server\\share\\docs")));
    assert!(!f("//server/share").contains(&f("//server/other")));
    // P4: a parent confined to "." used to contain every path; "." is refused now, and an
    // absolute request outside the parent's folders is dropped.
    let parent = Scope {
        paths: Paths::Within([f("/repo")].into_iter().collect()),
        ..Scope::session()
    };
    let req = Request {
        paths: Some(vec![f("/etc"), f("C:/Windows"), f("/repo/src")]),
        ..Request::default()
    };
    let child = attenuate(&parent, &Caps::NONE, &req, FrameKind::Harness).unwrap();
    assert_eq!(
        child.paths,
        Paths::Within([f("/repo/src")].into_iter().collect())
    );
    assert_eq!(check_within(&parent, &child, FrameKind::Harness), Ok(()));
    let forged = Scope {
        paths: Paths::Within([f("/etc"), f("C:/Windows")].into_iter().collect()),
        ..child
    };
    assert_eq!(
        check_within(&parent, &forged, FrameKind::Harness),
        Err(Dimension::Paths)
    );
}
