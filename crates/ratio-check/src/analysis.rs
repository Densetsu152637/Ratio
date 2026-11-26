//! What each declaration may do and which trust sites it reaches, after the types are checked
//! (stage 5): the summary `harnesses.check` gives (chapter 08) and the trust-site lists of
//! chapter 03 ("Conversion": listed **transitively**).
//!
//! The call graph has an edge from an item to every declaration its source names (a call, a
//! function value, a constant): effects are counted where a function value is made or named,
//! as for purity (`effects`), so calling a function value adds nothing, except that calling a
//! *harness* value (a parameter, a `meta.harness` result) may do anything a harness can, and
//! counts as every kind of effect. Over the graph, each item's effect kinds, tools and trust
//! sites are the union of its own and its callees' (a worklist over the reverse graph, so the
//! work is linear in the graph times the few effect kinds and tools). Trust-site sets are
//! bounded: past [`MAX_SITE_WORK`] set insertions every item is said to reach every site of the
//! package, a superset, which is the safe direction for a list a reviewer approves.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use ratio_names::ModuleId;
use ratio_syntax::Span;
use ratio_syntax::ast::ItemKind;

use crate::builtins::TrustKind;
use crate::check::{Checker, ItemSig};
use crate::tables::{ItemRef, Key, Origin, Res};
use crate::types::TypeId;

/// The kinds of effect chapter 07's summary distinguishes, as a set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Effects(u8);

impl Effects {
    pub const NONE: Effects = Effects(0);
    /// Writes files or other persistent state the user owns.
    pub const WRITE: Effects = Effects(1);
    /// Runs a command.
    pub const COMMAND: Effects = Effects(2);
    /// Reaches the network.
    pub const NETWORK: Effects = Effects(4);
    /// Delegates to another agent.
    pub const DELEGATE: Effects = Effects(8);
    /// Runs tools a model chooses (`tools.call_tool`, `tools.dispatch`, `model.loop`): "any
    /// tool of the frame".
    pub const ANY_TOOL: Effects = Effects(16);
    /// Calls a model.
    pub const MODEL: Effects = Effects(32);
    /// An effect of another kind (reads, engine state, the session).
    pub const OTHER: Effects = Effects(64);
    /// What a harness value may do when called: anything.
    pub const ALL: Effects = Effects(127);
    /// The kinds the guarded-effect lint watches (chapter 03: writes, commands, delegation,
    /// the network, and any tool).
    pub const GUARDED: Effects = Effects(1 | 2 | 4 | 8 | 16);

    const NAMES: [(Effects, &'static str); 7] = [
        (Effects::WRITE, "write"),
        (Effects::COMMAND, "command"),
        (Effects::NETWORK, "network"),
        (Effects::DELEGATE, "delegate"),
        (Effects::ANY_TOOL, "any_tool"),
        (Effects::MODEL, "model"),
        (Effects::OTHER, "other"),
    ];

    /// The kind called `name` in the built-in table's `@effect(name)`.
    #[must_use]
    pub fn named(name: &str) -> Option<Effects> {
        Effects::NAMES
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(e, _)| *e)
    }

    #[must_use]
    pub const fn union(self, other: Effects) -> Effects {
        Effects(self.0 | other.0)
    }

    #[must_use]
    pub const fn intersects(self, other: Effects) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn contains(self, other: Effects) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The names of the kinds in the set, in a fixed order.
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        Effects::NAMES
            .iter()
            .filter(|(e, _)| self.contains(*e))
            .map(|(_, n)| *n)
            .collect()
    }
}

/// A call of `trust`, `as_task` or `declassify` in a checked module.
#[derive(Clone, Debug)]
pub struct TrustSite {
    pub kind: TrustKind,
    pub module: ModuleId,
    /// The call.
    pub span: Span,
    /// The item it is written in, by index in its module.
    pub item: usize,
    /// The type it converts to (`T` of `trust` and `declassify`; `Brief` for `as_task`).
    pub ty: TypeId,
    /// The `because` text, when it is a string literal without interpolations.
    pub reason: Option<String>,
}

/// A call of `meta.harness`: the name it loads, when written as a literal.
#[derive(Clone, Debug)]
pub struct Load {
    pub module: ModuleId,
    pub span: Span,
    pub item: usize,
    /// `None` when the name is computed, so what it loads cannot be known before it runs.
    pub name: Option<String>,
}

/// What one item of a checked module may do, its own and through everything it names.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub effects: Effects,
    /// The tools it may run by name, sorted (`mcp__*` for any MCP tool).
    pub tools: Vec<String>,
    /// The trust sites it reaches, by index into [`crate::Checked::trust_sites`], sorted.
    pub trust_sites: Arc<[usize]>,
    /// The `meta.harness` loads it reaches, by index into [`crate::Checked::loads`], sorted.
    pub loads: Arc<[usize]>,
}

/// The entries of trust-site and load sets the analysis builds before it gives up and lists
/// every site for every item.
pub const MAX_SITE_WORK: usize = 4_000_000;

/// What the checker recorded while checking bodies, for the analysis.
#[derive(Clone, Debug, Default)]
pub(crate) struct Records {
    pub(crate) trust_sites: Vec<(ModuleId, Span, TrustKind, TypeId, Option<String>)>,
    pub(crate) loads: Vec<(ModuleId, Span, Option<String>)>,
    /// Calls of harness values.
    pub(crate) harness_calls: Vec<(ModuleId, Span)>,
}

/// The analysis's results.
#[derive(Clone, Debug, Default)]
pub(crate) struct Results {
    pub(crate) trust_sites: Vec<TrustSite>,
    pub(crate) loads: Vec<Load>,
    pub(crate) summaries: HashMap<(ModuleId, usize), Summary>,
    pub(crate) approximate: bool,
}

/// The index of the item of `starts` (sorted item start offsets) whose span holds `at`.
fn enclosing(starts: &[(usize, usize)], at: usize) -> Option<usize> {
    let i = starts.partition_point(|(start, _)| *start <= at);
    starts.get(i.checked_sub(1)?).map(|(_, index)| *index)
}

impl Checker<'_> {
    /// Builds the call graph and each item's summary.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass: nodes, edges, then the fixpoints"
    )]
    pub(crate) fn analyse(&mut self) -> Results {
        let mut out = Results::default();
        // Nodes: every item of every checked module.
        let mut node_of: HashMap<(ModuleId, usize), usize> = HashMap::new();
        let mut nodes: Vec<(ModuleId, usize)> = Vec::new();
        let mut starts: Vec<Vec<(usize, usize)>> = vec![Vec::new(); self.loaded.modules.len()];
        for (m, starts) in starts.iter_mut().enumerate() {
            if !self.checked[m] {
                continue;
            }
            for (index, item) in self.loaded.modules[m].ast.items.iter().enumerate() {
                node_of.insert((ModuleId(m), index), nodes.len());
                nodes.push((ModuleId(m), index));
                starts.push((item.span.start, index));
            }
            starts.sort_unstable();
        }
        let n = nodes.len();
        let mut effects = vec![Effects::NONE; n];
        let mut tools: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
        let mut sites: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        let mut loads: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (m, table) in self.tables.iter().enumerate() {
            if !self.checked[m] {
                continue;
            }
            let mut named: Vec<(Key, ItemRef)> = table
                .res
                .iter()
                .filter_map(|(k, r)| match r {
                    Res::Item(r) => Some((*k, *r)),
                    _ => None,
                })
                .collect();
            named.sort_unstable();
            for (key, r) in named {
                let Some(index) = starts.get(m).and_then(|s| enclosing(s, key.start)) else {
                    continue;
                };
                let Some(&from) = node_of.get(&(ModuleId(m), index)) else {
                    continue;
                };
                match r.origin {
                    Origin::Module(other) => {
                        if let Some(&to) = node_of.get(&(other, r.index)) {
                            edges[from].push(to);
                        }
                    }
                    Origin::Std(_) => {
                        if let ItemSig::Fn(sig) = self.sig(r) {
                            effects[from] = effects[from].union(sig.traits.effects);
                            tools[from].extend(sig.traits.tools.iter().cloned());
                        }
                    }
                }
            }
        }
        let mut records = std::mem::take(&mut self.records);
        // In module and source order, each call once.
        records
            .trust_sites
            .sort_by_key(|(m, span, ..)| (*m, span.start, span.end));
        records
            .trust_sites
            .dedup_by_key(|(m, span, ..)| (*m, span.start, span.end));
        records
            .loads
            .sort_by_key(|(m, span, _)| (*m, span.start, span.end));
        records
            .loads
            .dedup_by_key(|(m, span, _)| (*m, span.start, span.end));
        let node = |module: ModuleId, at: usize| {
            enclosing(starts.get(module.0)?, at)
                .and_then(|index| node_of.get(&(module, index)).map(|n| (*n, index)))
        };
        for (module, span) in &records.harness_calls {
            if let Some((from, _)) = node(*module, span.start) {
                effects[from] = Effects::ALL;
            }
        }
        for (module, span, kind, ty, reason) in records.trust_sites {
            let Some((from, item)) = node(module, span.start) else {
                continue;
            };
            sites[from].insert(out.trust_sites.len());
            out.trust_sites.push(TrustSite {
                kind,
                module,
                span,
                item,
                ty,
                reason,
            });
        }
        for (module, span, name) in records.loads {
            let Some((from, item)) = node(module, span.start) else {
                continue;
            };
            loads[from].insert(out.loads.len());
            out.loads.push(Load {
                module,
                span,
                item,
                name,
            });
        }
        for e in &mut edges {
            e.sort_unstable();
            e.dedup();
        }
        let final_sites: Vec<Arc<[usize]>>;
        let final_loads: Vec<Arc<[usize]>>;
        let mut callers: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (from, to) in edges.iter().enumerate() {
            for t in to {
                callers[*t].push(from);
            }
        }
        // Effects and tools: few kinds, so each item's set grows a bounded number of times.
        let mut work: Vec<usize> = (0..n).collect();
        while let Some(i) = work.pop() {
            for &c in &callers[i] {
                let before = (effects[c], tools[c].len());
                effects[c] = effects[c].union(effects[i]);
                let add: Vec<String> = tools[i].difference(&tools[c]).cloned().collect();
                tools[c].extend(add);
                if (effects[c], tools[c].len()) != before {
                    work.push(c);
                }
            }
        }
        // Trust sites and loads: over the strongly connected components of the call graph,
        // callees first, each component's set is its members' own and its callees' sets,
        // within a work bound.
        let (comp_sites, comp_loads, comp_of) = components(&edges, &sites, &loads, &mut out);
        if out.approximate {
            let all_sites: Arc<[usize]> = (0..out.trust_sites.len()).collect();
            let all_loads: Arc<[usize]> = (0..out.loads.len()).collect();
            final_sites = vec![all_sites; n];
            final_loads = vec![all_loads; n];
        } else {
            final_sites = comp_of.iter().map(|c| comp_sites[*c].clone()).collect();
            final_loads = comp_of.iter().map(|c| comp_loads[*c].clone()).collect();
        }
        for (i, (module, index)) in nodes.iter().enumerate() {
            let item = &self.loaded.modules[module.0].ast.items[*index];
            if matches!(
                item.kind,
                ItemKind::Import(_) | ItemKind::Type(_) | ItemKind::Enum(_)
            ) {
                continue;
            }
            out.summaries.insert(
                (*module, *index),
                Summary {
                    effects: effects[i],
                    tools: tools[i].iter().cloned().collect(),
                    trust_sites: final_sites[i].clone(),
                    loads: final_loads[i].clone(),
                },
            );
        }
        out
    }
}

/// Per strongly connected component of the call graph (Tarjan's algorithm, iterative, which
/// emits each component after every component it reaches): the sites and loads it reaches,
/// and each node's component. Past [`MAX_SITE_WORK`] entries, `out.approximate` is set.
#[allow(clippy::type_complexity, reason = "three parallel results")]
fn components(
    edges: &[Vec<usize>],
    sites: &[BTreeSet<usize>],
    loads: &[BTreeSet<usize>],
    out: &mut Results,
) -> (Vec<Arc<[usize]>>, Vec<Arc<[usize]>>, Vec<usize>) {
    let n = edges.len();
    let (mut index, mut low) = (vec![usize::MAX; n], vec![0usize; n]);
    let (mut on_stack, mut stack) = (vec![false; n], Vec::new());
    let mut comp_of = vec![usize::MAX; n];
    let mut comp_sites: Vec<Arc<[usize]>> = Vec::new();
    let mut comp_loads: Vec<Arc<[usize]>> = Vec::new();
    let mut next = 0usize;
    let mut spent = 0usize;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut edge)) = frames.last_mut() {
            if let Some(&w) = edges[v].get(*edge) {
                *edge += 1;
                if index[w] == usize::MAX {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    frames.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] != index[v] {
                continue;
            }
            let id = comp_sites.len();
            let mut members = Vec::new();
            while let Some(w) = stack.pop() {
                on_stack[w] = false;
                comp_of[w] = id;
                members.push(w);
                if w == v {
                    break;
                }
            }
            let (mut s, mut l): (BTreeSet<usize>, BTreeSet<usize>) =
                (BTreeSet::new(), BTreeSet::new());
            for &m in &members {
                s.extend(sites[m].iter().copied());
                l.extend(loads[m].iter().copied());
                for &w in &edges[m] {
                    let c = comp_of[w];
                    if c != id && c != usize::MAX && !out.approximate {
                        spent += comp_sites[c].len() + comp_loads[c].len() + 1;
                        if spent > MAX_SITE_WORK {
                            out.approximate = true;
                        }
                        s.extend(comp_sites[c].iter().copied());
                        l.extend(comp_loads[c].iter().copied());
                    }
                }
            }
            if out.approximate {
                s.clear();
                l.clear();
            }
            comp_sites.push(s.into_iter().collect());
            comp_loads.push(l.into_iter().collect());
        }
    }
    (comp_sites, comp_loads, comp_of)
}
