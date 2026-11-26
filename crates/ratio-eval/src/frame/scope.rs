//! A frame's authority and resources ([`Scope`]), what a configuration asks for ([`Request`]),
//! and the two pure functions of attenuation (chapter 04, decision R10):
//!
//! - [`attenuate`] computes a child frame's scope as the meet of its caller's scope and its
//!   request, refusing what cannot be had (`Denied { by: attenuation }`, or
//!   `Budget { which: depth }` past the depth limit);
//! - [`check_within`] verifies, in every dimension, that one scope is within another. It is the
//!   check a host repeats independently when a frame opens (Imperium's `RatioHost::open_frame`,
//!   against its own record of the parent), so a bug in [`attenuate`] or a forged parent can
//!   only refuse more. It depends on nothing but the two scopes.
//!
//! Every dimension is a lattice and the child is the meet, so attenuation is monotone: a child
//! is never wider than its parent in any dimension, and a chain of frames narrows down the
//! chain. Nothing here allocates without bound or can overflow: caps are `u64` with `None` for
//! "uncapped", combined by `min`.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

/// A permission mode. The order is `plan < ask < default < auto, accept_edits < full`;
/// `auto` and `accept_edits` are incomparable and meet at `default`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Plan,
    Ask,
    Default,
    Auto,
    AcceptEdits,
    Full,
}

impl Mode {
    pub const ALL: [Mode; 6] = [
        Mode::Plan,
        Mode::Ask,
        Mode::Default,
        Mode::Auto,
        Mode::AcceptEdits,
        Mode::Full,
    ];

    /// The name a program writes (`PermissionMode`'s variant).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Ask => "ask",
            Mode::Default => "default",
            Mode::Auto => "auto",
            Mode::AcceptEdits => "accept_edits",
            Mode::Full => "full",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }

    const fn rank(self) -> u8 {
        match self {
            Mode::Plan => 0,
            Mode::Ask => 1,
            Mode::Default => 2,
            Mode::Auto | Mode::AcceptEdits => 3,
            Mode::Full => 4,
        }
    }

    /// Whether `self` allows at most what `other` allows.
    #[must_use]
    pub const fn within(self, other: Mode) -> bool {
        self as u8 == other as u8 || self.rank() < other.rank()
    }

    /// The greatest mode within both.
    #[must_use]
    pub const fn meet(self, other: Mode) -> Mode {
        if self.within(other) {
            self
        } else if other.within(self) {
            other
        } else {
            Mode::Default
        }
    }
}

/// Where a frame's model calls may go. `local` (this machine only) is the narrowest, then
/// `cloud` (hosted models only), then `any`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    Local,
    Cloud,
    Any,
}

impl Location {
    pub const ALL: [Location; 3] = [Location::Local, Location::Cloud, Location::Any];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Location::Local => "local",
            Location::Cloud => "cloud",
            Location::Any => "any",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Location> {
        Location::ALL.into_iter().find(|l| l.name() == name)
    }

    #[must_use]
    pub const fn within(self, other: Location) -> bool {
        self as u8 <= other as u8
    }

    /// `local` if either is `local` (chapter 04).
    #[must_use]
    pub const fn meet(self, other: Location) -> Location {
        if self.within(other) { self } else { other }
    }
}

/// A tool name or a prefix pattern (`"mcp__github__*"`; `"*"` is every tool).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ToolPat {
    Name(Arc<str>),
    /// Every tool whose name starts with the prefix.
    Prefix(Arc<str>),
}

impl ToolPat {
    /// Parses a tool name or pattern: ASCII letters, digits, `_`, `-`, `.` and `:`, with at
    /// most one `*`, at the end. `None` for anything else (a pattern this module could not
    /// intersect exactly is refused rather than approximated).
    #[must_use]
    pub fn parse(text: &str) -> Option<ToolPat> {
        let (body, prefix) = match text.strip_suffix('*') {
            Some(body) => (body, true),
            None => (text, false),
        };
        let ok = body.len() <= 256
            && body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
        if !ok || (!prefix && body.is_empty()) {
            return None;
        }
        Some(if prefix {
            ToolPat::Prefix(Arc::from(body))
        } else {
            ToolPat::Name(Arc::from(body))
        })
    }

    /// Whether every tool `other` names is named by `self`.
    #[must_use]
    pub fn covers(&self, other: &ToolPat) -> bool {
        match (self, other) {
            (ToolPat::Name(a), ToolPat::Name(b)) => a == b,
            (ToolPat::Name(_), ToolPat::Prefix(_)) => false,
            (ToolPat::Prefix(p), ToolPat::Name(n) | ToolPat::Prefix(n)) => n.starts_with(&**p),
        }
    }

    /// The tools both name, when any: one pattern, since prefixes nest or are disjoint.
    fn meet(&self, other: &ToolPat) -> Option<ToolPat> {
        if self.covers(other) {
            Some(other.clone())
        } else if other.covers(self) {
            Some(self.clone())
        } else {
            None
        }
    }
}

impl fmt::Display for ToolPat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolPat::Name(n) => f.write_str(n),
            ToolPat::Prefix(p) => write!(f, "{p}*"),
        }
    }
}

/// A set of tools: names and prefix patterns, kept without a pattern another covers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ToolSet(BTreeSet<ToolPat>);

impl ToolSet {
    /// Every tool.
    #[must_use]
    pub fn all() -> ToolSet {
        ToolSet::of([ToolPat::Prefix(Arc::from(""))])
    }

    /// No tool.
    #[must_use]
    pub fn none() -> ToolSet {
        ToolSet::default()
    }

    /// The set of these patterns, without one another covers. Duplicates are dropped first,
    /// so the work is quadratic in the distinct patterns only.
    #[must_use]
    pub fn of(pats: impl IntoIterator<Item = ToolPat>) -> ToolSet {
        let all: Vec<ToolPat> = pats
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let out = all
            .iter()
            .filter(|p| !all.iter().any(|q| q != *p && q.covers(p)))
            .cloned()
            .collect();
        ToolSet(out)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ToolPat> {
        self.0.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the set names every tool `pat` names.
    #[must_use]
    pub fn covers(&self, pat: &ToolPat) -> bool {
        self.0.iter().any(|p| p.covers(pat))
    }

    /// Whether the set names the tool `name`.
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        self.covers(&ToolPat::Name(Arc::from(name)))
    }

    /// The tools both sets name.
    #[must_use]
    pub fn meet(&self, other: &ToolSet) -> ToolSet {
        ToolSet::of(
            self.0
                .iter()
                .flat_map(|a| other.0.iter().filter_map(move |b| a.meet(b))),
        )
    }

    /// Whether every tool this set names is named by `parent`.
    #[must_use]
    pub fn within(&self, parent: &ToolSet) -> bool {
        self.0.iter().all(|p| parent.covers(p))
    }
}

/// A request for tools: `need` tools are required, `want` tools are dropped when the caller
/// lacks them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRequest {
    pub need: ToolSet,
    pub want: ToolSet,
}

/// Permission rules. A callee adds `deny` and `ask` rules and can never add `allow` rules; the
/// engine decides deny over ask over allow. Rules are opaque text to Ratio.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rules {
    pub allow: BTreeSet<Arc<str>>,
    pub ask: BTreeSet<Arc<str>>,
    pub deny: BTreeSet<Arc<str>>,
}

impl Rules {
    /// Whether `self` restricts at least as much as `parent`: no allow rule the parent lacks,
    /// every ask and deny rule the parent has.
    #[must_use]
    pub fn within(&self, parent: &Rules) -> bool {
        self.allow.is_subset(&parent.allow)
            && self.ask.is_superset(&parent.ask)
            && self.deny.is_superset(&parent.deny)
    }
}

/// The root an absolute folder hangs from. Different roots never contain each other.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Root {
    /// `/` (a POSIX root, or a Windows path from the current drive's root).
    Posix,
    /// A Windows drive, by its letter in upper case (`C:` and `c:` are one root).
    Drive(char),
    /// A Windows UNC share, `//server/share`, lower-cased (UNC names ignore case).
    Unc(Arc<str>, Arc<str>),
}

/// An absolute folder: a root and normalised components (`/` and `\` separate them).
/// Relative folders, `.`, the empty path, `..`, drive-relative paths (`C:foo`) and components
/// holding `:`, `*` or `?` are refused, so a folder never names anything outside itself by
/// its text, and the engine (which resolves paths itself) can only refuse more.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Folder {
    root: Root,
    parts: Vec<Arc<str>>,
}

/// A name a folder may hold: not empty, `.` or `..`, and without `:`, `*`, `?` or NUL.
fn plain_component(c: &str) -> bool {
    !c.is_empty() && c != "." && c != ".." && !c.contains([':', '*', '?', '\0'])
}

impl Folder {
    /// The absolute folder `text` names, or `None` (see [`Folder`]); at most 4096 bytes.
    #[must_use]
    pub fn parse(text: &str) -> Option<Folder> {
        if text.len() > 4096 {
            return None;
        }
        let sep = ['/', '\\'];
        let (root, rest) = if let Some(unc) = text
            .strip_prefix("//")
            .or_else(|| text.strip_prefix("\\\\"))
            .or_else(|| text.strip_prefix("/\\"))
            .or_else(|| text.strip_prefix("\\/"))
        {
            let mut it = unc.splitn(3, sep);
            let server = it.next()?;
            let share = it.next()?;
            if !plain_component(server) || !plain_component(share) {
                return None;
            }
            let root = Root::Unc(
                Arc::from(server.to_lowercase()),
                Arc::from(share.to_lowercase()),
            );
            (root, it.next().unwrap_or(""))
        } else if let Some(rest) = text.strip_prefix(sep) {
            (Root::Posix, rest)
        } else {
            let mut chars = text.chars();
            let letter = chars.next().filter(char::is_ascii_alphabetic)?;
            let rest = chars.as_str().strip_prefix(':')?;
            if !(rest.is_empty() || rest.starts_with(sep)) {
                return None;
            }
            (Root::Drive(letter.to_ascii_uppercase()), rest)
        };
        let mut parts = Vec::new();
        for c in rest.split(sep) {
            match c {
                "" | "." => {}
                c if plain_component(c) => parts.push(Arc::from(c)),
                _ => return None,
            }
        }
        Some(Folder { root, parts })
    }

    /// Whether `other` is this folder or inside it: the same root, and this folder's
    /// components a prefix of its components (so `/a` does not contain `/ab`).
    #[must_use]
    pub fn contains(&self, other: &Folder) -> bool {
        self.root == other.root
            && other.parts.len() >= self.parts.len()
            && other.parts[..self.parts.len()] == self.parts[..]
    }
}

impl fmt::Display for Folder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.root {
            Root::Posix => {}
            Root::Drive(d) => write!(f, "{d}:")?,
            Root::Unc(server, share) => write!(f, "//{server}/{share}")?,
        }
        for c in &self.parts {
            write!(f, "/{c}")?;
        }
        if self.parts.is_empty() && !matches!(self.root, Root::Unc(..)) {
            f.write_str("/")?;
        }
        Ok(())
    }
}

/// The folders the file tools are confined to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Paths {
    /// No confinement beyond the session's own grants.
    Any,
    /// Only these folders (none: no file access).
    Within(BTreeSet<Folder>),
}

impl Paths {
    fn of(folders: impl IntoIterator<Item = Folder>) -> Paths {
        let all: Vec<Folder> = folders
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let out = all
            .iter()
            .filter(|f| !all.iter().any(|g| g != *f && g.contains(f)))
            .cloned()
            .collect();
        Paths::Within(out)
    }

    /// How many folders (0 for `Any`).
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Paths::Any => 0,
            Paths::Within(f) => f.len(),
        }
    }

    /// Whether no folder is listed (`Any` included).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The folders within both.
    #[must_use]
    pub fn meet(&self, other: &Paths) -> Paths {
        match (self, other) {
            (Paths::Any, p) | (p, Paths::Any) => p.clone(),
            (Paths::Within(a), Paths::Within(b)) => Paths::of(
                a.iter()
                    .filter(|r| b.iter().any(|c| c.contains(r)))
                    .chain(b.iter().filter(|c| a.iter().any(|r| r.contains(c))))
                    .cloned(),
            ),
        }
    }

    #[must_use]
    pub fn within(&self, parent: &Paths) -> bool {
        match (self, parent) {
            (_, Paths::Any) => true,
            (Paths::Any, Paths::Within(_)) => false,
            (Paths::Within(c), Paths::Within(p)) => {
                c.iter().all(|f| p.iter().any(|g| g.contains(f)))
            }
        }
    }
}

/// A budget kind a frame caps (chapter 04): the ledger kinds, and `wall`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BudgetKind {
    Tokens,
    Cost,
    Wall,
    Steps,
    ModelCalls,
    ToolCalls,
}

impl BudgetKind {
    pub const ALL: [BudgetKind; 6] = [
        BudgetKind::Tokens,
        BudgetKind::Cost,
        BudgetKind::Wall,
        BudgetKind::Steps,
        BudgetKind::ModelCalls,
        BudgetKind::ToolCalls,
    ];

    /// The `BudgetKind` variant a refusal names.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BudgetKind::Tokens => "tokens",
            BudgetKind::Cost => "cost",
            BudgetKind::Wall => "wall",
            BudgetKind::Steps => "steps",
            BudgetKind::ModelCalls => "model_calls",
            BudgetKind::ToolCalls => "tool_calls",
        }
    }
}

/// A cap per budget kind; `None` is uncapped. Cost is in micro-units of money, wall clock in
/// microseconds (the interpreter's `Money` and `Duration`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Caps {
    pub tokens: Option<u64>,
    pub cost: Option<u64>,
    pub wall: Option<u64>,
    pub steps: Option<u64>,
    pub model_calls: Option<u64>,
    pub tool_calls: Option<u64>,
}

impl Caps {
    /// No cap at all.
    pub const NONE: Caps = Caps {
        tokens: None,
        cost: None,
        wall: None,
        steps: None,
        model_calls: None,
        tool_calls: None,
    };

    #[must_use]
    pub const fn get(&self, k: BudgetKind) -> Option<u64> {
        match k {
            BudgetKind::Tokens => self.tokens,
            BudgetKind::Cost => self.cost,
            BudgetKind::Wall => self.wall,
            BudgetKind::Steps => self.steps,
            BudgetKind::ModelCalls => self.model_calls,
            BudgetKind::ToolCalls => self.tool_calls,
        }
    }

    pub fn set(&mut self, k: BudgetKind, v: Option<u64>) {
        let slot = match k {
            BudgetKind::Tokens => &mut self.tokens,
            BudgetKind::Cost => &mut self.cost,
            BudgetKind::Wall => &mut self.wall,
            BudgetKind::Steps => &mut self.steps,
            BudgetKind::ModelCalls => &mut self.model_calls,
            BudgetKind::ToolCalls => &mut self.tool_calls,
        };
        *slot = v;
    }

    /// The smaller cap of each kind.
    #[must_use]
    pub fn meet(&self, other: &Caps) -> Caps {
        let mut out = Caps::NONE;
        for k in BudgetKind::ALL {
            out.set(k, min_cap(self.get(k), other.get(k)));
        }
        out
    }

    /// The first kind in which `self` allows more than `parent`, if any.
    #[must_use]
    pub fn wider_than(&self, parent: &Caps) -> Option<BudgetKind> {
        BudgetKind::ALL
            .into_iter()
            .find(|&k| match (self.get(k), parent.get(k)) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(c), Some(p)) => c > p,
            })
    }
}

/// The smaller of two caps, `None` being uncapped.
#[must_use]
pub const fn min_cap(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// An item of `sees` (chapter 04, "What a callee sees").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeesItem {
    Turn,
    Attachments,
    Plan,
    MemoryEnvironment,
    MemoryIndex,
    Agents,
    SystemDefault,
    State,
    Secrets,
}

impl SeesItem {
    pub const ALL: [SeesItem; 9] = [
        SeesItem::Turn,
        SeesItem::Attachments,
        SeesItem::Plan,
        SeesItem::MemoryEnvironment,
        SeesItem::MemoryIndex,
        SeesItem::Agents,
        SeesItem::SystemDefault,
        SeesItem::State,
        SeesItem::Secrets,
    ];

    /// The `Sees` field's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SeesItem::Turn => "turn",
            SeesItem::Attachments => "attachments",
            SeesItem::Plan => "plan",
            SeesItem::MemoryEnvironment => "memory_environment",
            SeesItem::MemoryIndex => "memory_index",
            SeesItem::Agents => "agents",
            SeesItem::SystemDefault => "system_default",
            SeesItem::State => "state",
            SeesItem::Secrets => "secrets",
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of `sees` items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Sees(u16);

impl Sees {
    pub const NONE: Sees = Sees(0);
    pub const ALL: Sees = Sees((1 << 9) - 1);

    #[must_use]
    pub const fn has(self, item: SeesItem) -> bool {
        self.0 & item.bit() != 0
    }

    #[must_use]
    pub const fn with(self, item: SeesItem) -> Sees {
        Sees(self.0 | item.bit())
    }

    #[must_use]
    pub const fn without(self, item: SeesItem) -> Sees {
        Sees(self.0 & !item.bit())
    }

    #[must_use]
    pub const fn meet(self, other: Sees) -> Sees {
        Sees(self.0 & other.0)
    }

    #[must_use]
    pub const fn union(self, other: Sees) -> Sees {
        Sees((self.0 | other.0) & Sees::ALL.0)
    }

    #[must_use]
    pub const fn within(self, other: Sees) -> bool {
        self.0 & !other.0 == 0
    }

    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn from_bits(bits: u16) -> Sees {
        Sees(bits & Sees::ALL.0)
    }
}

/// What a `sees` field of a configuration sets: items written `true`, items written `false`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeesPatch {
    pub set: Sees,
    pub clear: Sees,
}

/// A frame's conversation (chapter 04, "Context modes").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    Fresh,
    Fork,
    None,
    Main,
}

impl Context {
    pub const ALL: [Context; 4] = [Context::Fresh, Context::Fork, Context::None, Context::Main];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Context::Fresh => "fresh",
            Context::Fork => "fork",
            Context::None => "none",
            Context::Main => "main",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Context> {
        Context::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// What opens a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameKind {
    /// An entry's own frame (`@turn`, `@task`), opened from the session's scope.
    Entry,
    /// A harness call: one deeper than its caller.
    Harness,
    /// A `with { … } { … }` block: a scope at its frame's depth.
    Block,
}

/// The most nested harness calls any frame may allow (chapter 07).
pub const MAX_DEPTH: u32 = 32;
/// The default frame-depth limit.
pub const DEFAULT_DEPTH: u32 = 8;
/// The most running branches any frame may allow (chapter 07).
pub const MAX_WIDTH: u32 = 64;
/// The default width.
pub const DEFAULT_WIDTH: u32 = 16;

/// A frame's authority and resources: every dimension attenuation narrows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub tools: ToolSet,
    pub mode: Mode,
    pub rules: Rules,
    pub paths: Paths,
    pub location: Location,
    /// The caps of this frame's ledgers (see [`crate::frame::Ledgers`]).
    pub budget: Caps,
    pub sees: Sees,
    /// The frame's depth: 0 for an entry and the session, one more per harness call.
    pub depth: u32,
    /// `limits.depth`: the deepest frame below this one.
    pub max_depth: u32,
    /// `limits.width`: running branches in this frame's tree.
    pub width: u32,
    /// `limits.heap`, in bytes; `None` is the run's own limit.
    pub heap: Option<u64>,
    pub attended: bool,
    /// Whether frames may write memory (the root agent's turn only).
    pub memory_writes: bool,
    pub context: Context,
}

impl Scope {
    /// The widest scope: what a host grants a session it does not restrict. A host passes
    /// its own session scope instead (its tools, mode, rules, grants, location and budgets).
    #[must_use]
    pub fn session() -> Scope {
        Scope {
            tools: ToolSet::all(),
            mode: Mode::Full,
            rules: Rules::default(),
            paths: Paths::Any,
            location: Location::Any,
            budget: Caps::NONE,
            sees: Sees::ALL,
            depth: 0,
            max_depth: DEFAULT_DEPTH,
            width: DEFAULT_WIDTH,
            heap: None,
            attended: true,
            memory_writes: true,
            context: Context::Main,
        }
    }
}

/// What a frame's configuration asks for, after the declared configuration and the call's
/// overrides are merged ([`Request::merge`]). `None` fields are inherited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub tools: Option<ToolRequest>,
    pub mode: Option<Mode>,
    pub deny: Vec<Arc<str>>,
    pub ask: Vec<Arc<str>>,
    pub paths: Option<Vec<Folder>>,
    pub location: Option<Location>,
    pub budget: Caps,
    pub depth: Option<u32>,
    pub width: Option<u32>,
    pub heap: Option<u64>,
    pub context: Option<Context>,
    pub sees: SeesPatch,
    pub attended: Option<bool>,
}

impl Request {
    /// The declared configuration with the call's overrides: overrides win field by field
    /// (and, within `budget`, `limits`, `sees` and `permissions`, sub-field by sub-field);
    /// `deny` and `ask` rules of both apply.
    #[must_use]
    pub fn merge(declared: Request, overrides: Request) -> Request {
        let mut budget = declared.budget;
        for k in BudgetKind::ALL {
            if let Some(v) = overrides.budget.get(k) {
                budget.set(k, Some(v));
            }
        }
        let mut deny = declared.deny;
        deny.extend(overrides.deny);
        let mut ask = declared.ask;
        ask.extend(overrides.ask);
        let set = Sees((declared.sees.set.0 & !overrides.sees.clear.0) | overrides.sees.set.0);
        let clear = Sees((declared.sees.clear.0 & !overrides.sees.set.0) | overrides.sees.clear.0);
        Request {
            tools: overrides.tools.or(declared.tools),
            mode: overrides.mode.or(declared.mode),
            deny,
            ask,
            paths: overrides.paths.or(declared.paths),
            location: overrides.location.or(declared.location),
            budget,
            depth: overrides.depth.or(declared.depth),
            width: overrides.width.or(declared.width),
            heap: overrides.heap.or(declared.heap),
            context: overrides.context.or(declared.context),
            sees: SeesPatch { set, clear },
            attended: overrides.attended.or(declared.attended),
        }
    }
}

/// A dimension of a scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    Tools,
    Mode,
    Rules,
    Paths,
    Location,
    Budget(BudgetKind),
    Sees,
    Depth,
    Width,
    Heap,
    Attended,
    MemoryWrites,
    Context,
}

impl Dimension {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Dimension::Tools => "tools",
            Dimension::Mode => "mode",
            Dimension::Rules => "rules",
            Dimension::Paths => "paths",
            Dimension::Location => "location",
            Dimension::Budget(k) => k.name(),
            Dimension::Sees => "sees",
            Dimension::Depth => "depth",
            Dimension::Width => "width",
            Dimension::Heap => "heap",
            Dimension::Attended => "attended",
            Dimension::MemoryWrites => "memory_writes",
            Dimension::Context => "context",
        }
    }
}

/// Why a frame could not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// `true`: `Budget { which: depth }` (the depth limit); `false`:
    /// `Denied { by: attenuation }`.
    pub depth: bool,
    pub dimension: Dimension,
    pub message: String,
}

fn denied(dimension: Dimension, message: String) -> Refusal {
    Refusal {
        depth: false,
        dimension,
        message,
    }
}

/// The scope of a frame of `kind` opened under `parent` with `request` (chapter 04, R10):
/// each dimension is the meet of the parent's and the request, and each budget cap is also
/// at most `room`, what the parent's ledgers have left.
///
/// # Errors
///
/// `Budget { which: depth }` when a harness call would pass the parent's depth limit;
/// `Denied { by: attenuation }` when a required tool, a `cloud` location, a `sees` item, an
/// attended frame or the main conversation cannot be had.
pub fn attenuate(
    parent: &Scope,
    room: &Caps,
    request: &Request,
    kind: FrameKind,
) -> Result<Scope, Refusal> {
    let depth = match kind {
        FrameKind::Harness => parent.depth.saturating_add(1),
        FrameKind::Entry | FrameKind::Block => parent.depth,
    };
    if kind == FrameKind::Harness && depth > parent.max_depth {
        return Err(Refusal {
            depth: true,
            dimension: Dimension::Depth,
            message: format!(
                "a harness call at frame depth {depth} passes the depth limit {}",
                parent.max_depth
            ),
        });
    }
    let tools = match &request.tools {
        None => parent.tools.clone(),
        Some(req) => {
            for need in req.need.iter() {
                if parent.tools.meet(&ToolSet::of([need.clone()])).is_empty() {
                    return Err(denied(
                        Dimension::Tools,
                        format!("the caller does not have the tool `{need}`"),
                    ));
                }
            }
            let asked = ToolSet::of(req.need.iter().chain(req.want.iter()).cloned());
            parent.tools.meet(&asked)
        }
    };
    let mode = request.mode.map_or(parent.mode, |m| m.meet(parent.mode));
    let mut rules = parent.rules.clone();
    rules.deny.extend(request.deny.iter().cloned());
    rules.ask.extend(request.ask.iter().cloned());
    let paths = match &request.paths {
        None => parent.paths.clone(),
        Some(folders) => Paths::of(folders.iter().cloned()).meet(&parent.paths),
    };
    let location = match request.location {
        Some(Location::Cloud) if parent.location == Location::Local => {
            return Err(denied(
                Dimension::Location,
                "the caller is confined to local models".to_owned(),
            ));
        }
        Some(l) => l.meet(parent.location),
        None => parent.location,
    };
    let budget = request.budget.meet(&parent.budget).meet(room);
    let context = request.context.unwrap_or(match kind {
        FrameKind::Harness | FrameKind::Entry => Context::Fresh,
        FrameKind::Block => parent.context,
    });
    if context == Context::Main && !main_allowed(parent, kind) {
        return Err(denied(
            Dimension::Context,
            "only an entry's own frame may use the main conversation".to_owned(),
        ));
    }
    let base = match (kind, context) {
        (FrameKind::Entry | FrameKind::Block, _) | (FrameKind::Harness, Context::Fork) => {
            parent.sees
        }
        (FrameKind::Harness, _) => Sees::NONE,
    };
    if let Some(item) = SeesItem::ALL
        .into_iter()
        .find(|&i| request.sees.set.has(i) && !parent.sees.has(i))
    {
        return Err(denied(
            Dimension::Sees,
            format!("the caller does not see `{}`", item.name()),
        ));
    }
    let sees = Sees(base.union(request.sees.set).0 & !request.sees.clear.0).meet(parent.sees);
    if request.attended == Some(true) && !parent.attended {
        return Err(denied(
            Dimension::Attended,
            "an attended frame was asked for in an unattended caller".to_owned(),
        ));
    }
    let attended = parent.attended && request.attended.unwrap_or(true);
    let max_depth = request
        .depth
        .map_or(parent.max_depth, |d| d.min(parent.max_depth))
        .min(MAX_DEPTH);
    let width = request
        .width
        .map_or(parent.width, |w| w.min(parent.width))
        .min(MAX_WIDTH);
    let heap = match (request.heap, parent.heap) {
        (Some(r), Some(p)) => Some(r.min(p)),
        (r, p) => r.or(p),
    };
    Ok(Scope {
        tools,
        mode,
        rules,
        paths,
        location,
        budget,
        sees,
        depth,
        max_depth,
        width,
        heap,
        attended,
        memory_writes: parent.memory_writes,
        context,
    })
}

fn main_allowed(parent: &Scope, kind: FrameKind) -> bool {
    match kind {
        FrameKind::Entry => parent.context == Context::Main,
        FrameKind::Block => parent.context == Context::Main,
        FrameKind::Harness => false,
    }
}

/// Whether `child`, a frame of `kind`, is within `parent` in every dimension: the check a host
/// repeats when a frame opens. Pass as `parent` the host's own record of the parent frame
/// (with its budget caps lowered to what its ledgers have left, for the tighter check), never
/// a scope the interpreter supplied. Take `kind`, and the depth `parent.depth` the check
/// compares against, from that record too, never from the `FrameCtx` the interpreter passes:
/// a forged `kind` (`Block` for a harness call) or depth would skip the depth check.
///
/// # Errors
///
/// The first dimension in which `child` is wider than `parent`, or out of place (a harness
/// frame not one deeper than its caller, or past its caller's depth limit; the main
/// conversation outside an entry's own frame).
pub fn check_within(parent: &Scope, child: &Scope, kind: FrameKind) -> Result<(), Dimension> {
    let depth_ok = match kind {
        FrameKind::Harness => {
            parent.depth.checked_add(1) == Some(child.depth) && child.depth <= parent.max_depth
        }
        FrameKind::Entry | FrameKind::Block => child.depth == parent.depth,
    };
    let checks = [
        (child.tools.within(&parent.tools), Dimension::Tools),
        (child.mode.within(parent.mode), Dimension::Mode),
        (child.rules.within(&parent.rules), Dimension::Rules),
        (child.paths.within(&parent.paths), Dimension::Paths),
        (child.location.within(parent.location), Dimension::Location),
        (child.sees.within(parent.sees), Dimension::Sees),
        (
            depth_ok && child.max_depth <= parent.max_depth && child.max_depth <= MAX_DEPTH,
            Dimension::Depth,
        ),
        (
            child.width <= parent.width && child.width <= MAX_WIDTH,
            Dimension::Width,
        ),
        (
            match (child.heap, parent.heap) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(c), Some(p)) => c <= p,
            },
            Dimension::Heap,
        ),
        (!child.attended || parent.attended, Dimension::Attended),
        (
            !child.memory_writes || parent.memory_writes,
            Dimension::MemoryWrites,
        ),
        (
            child.context != Context::Main || main_allowed(parent, kind),
            Dimension::Context,
        ),
    ];
    if let Some((_, d)) = checks.iter().find(|(ok, _)| !ok) {
        return Err(*d);
    }
    if let Some(k) = child.budget.wider_than(&parent.budget) {
        return Err(Dimension::Budget(k));
    }
    Ok(())
}
