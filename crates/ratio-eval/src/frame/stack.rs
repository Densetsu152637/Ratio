//! The evaluation's stack of open frames: the session's frame at the bottom, then each
//! entry, harness call and `with` block, with the frame paths, ledger nodes and branch paths
//! the machine needs (chapter 01, "The effect tape"; chapter 04).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use ratio_ir::Loc;

use super::config::Settings;
use super::ledger::{Ledgers, NodeId};
use super::scope::{
    BudgetKind, FrameKind, Refusal, Request, Scope, attenuate, check_within, min_cap,
};

/// What the host grants an evaluation: the session's scope, and the name its frame paths
/// start with (`turn` for a `@turn` entry, `task` for a `@task` entry).
#[derive(Clone, Debug)]
pub struct Session {
    pub scope: Scope,
    pub root: Arc<str>,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            scope: Scope::session(),
            root: Arc::from("turn"),
        }
    }
}

/// The frame an effect is made in, as the host receives it with every effect: its path, its
/// scope after attenuation, its ledger node (for admissions) and its settings. A host checks
/// the scope against its own record of the frame, never trusting it to be narrow.
#[derive(Clone, Debug)]
pub struct FrameCtx {
    /// The frame path (`turn/critic@s41#0/c12#0.b2/judge@s30#1`).
    pub path: Arc<str>,
    /// The caller's frame path; `None` for the session's own frame.
    pub parent: Option<Arc<str>>,
    pub kind: FrameKind,
    /// The harness's name (the root name for the session's frame, `with` for a block).
    pub harness: Arc<str>,
    pub scope: Arc<Scope>,
    pub ledgers: Ledgers,
    pub node: NodeId,
    pub settings: Arc<Settings>,
    /// What opened the frame, from the program's IR, for the host to verify against what it
    /// loaded: the kind alone is Ratio's assertion, the origin is checkable.
    pub origin: FrameOrigin,
}

/// What opened a frame (consistent with [`FrameCtx::kind`]; the interpreter refuses to open a
/// frame whose origin and kind disagree).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FrameOrigin {
    /// An entry's own frame, or the session's.
    Entry,
    /// A `with { … } { … }` block at this site: the IR hash of its package and the node index.
    /// A host refuses a block whose package it never loaded.
    Block { site: ([u8; 32], u32) },
    /// A harness call entering `key` (`<module path>::<name>`) of `package`, whose IR hash is
    /// `entry_hash`. A host refuses a harness it never loaded, by package, hash and key.
    Harness {
        package: Arc<str>,
        entry_hash: [u8; 32],
        key: Arc<str>,
    },
}

impl FrameOrigin {
    /// Whether the origin fits `kind`.
    #[must_use]
    pub fn fits(&self, kind: FrameKind) -> bool {
        matches!(
            (self, kind),
            (FrameOrigin::Entry, FrameKind::Entry)
                | (FrameOrigin::Block { .. }, FrameKind::Block)
                | (FrameOrigin::Harness { .. }, FrameKind::Harness)
        )
    }
}

/// One element of a branch path: `c<construct site>#<occurrence>.b<branch index>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BranchEl {
    site: u32,
    occ: u32,
    idx: u32,
}

/// Who called a harness frame.
#[derive(Clone, Debug)]
pub(crate) struct Caller {
    pub harness: Arc<str>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub ctx: FrameCtx,
    branch: Vec<BranchEl>,
    occ: HashMap<(Vec<BranchEl>, u32), u32>,
    /// The frame (an index into the stack) whose `limits.depth` binds; 0 is the turn's.
    depth_scope: u32,
    pub caller: Option<Caller>,
}

impl Frame {
    fn next_occ(&mut self, site: u32) -> u32 {
        let n = self.occ.entry((self.branch.clone(), site)).or_insert(0);
        let occ = *n;
        *n = n.saturating_add(1);
        occ
    }

    fn branch_prefix(&self) -> String {
        let mut out = String::new();
        for b in &self.branch {
            let _ = write!(out, "c{}#{}.b{}/", b.site, b.occ, b.idx);
        }
        out
    }
}

/// A frame could not open: the refusal, and the frame whose limit it was (0: the turn's).
#[derive(Debug)]
pub(crate) struct OpenError {
    pub refusal: Refusal,
    pub scope: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct FrameStack {
    frames: Vec<Frame>,
    ledgers: Ledgers,
    /// The machine's step count already debited on the ledgers.
    flushed: u64,
    /// The absolute step count the top frame may reach, and the frame whose cap that is.
    pub step_limit: u64,
    pub step_scope: u32,
}

impl FrameStack {
    /// The session's frame, with the run's step limit as its `steps` cap.
    pub(crate) fn new(session: &Session, steps: u64) -> FrameStack {
        let mut scope = session.scope.clone();
        scope.budget.steps = min_cap(scope.budget.steps, Some(steps));
        let (ledgers, node) = Ledgers::new(scope.budget, scope.width);
        let ctx = FrameCtx {
            path: Arc::clone(&session.root),
            parent: None,
            kind: FrameKind::Entry,
            harness: Arc::clone(&session.root),
            scope: Arc::new(scope),
            ledgers: ledgers.clone(),
            node,
            settings: Arc::new(Settings::default()),
            origin: FrameOrigin::Entry,
        };
        let mut s = FrameStack {
            frames: vec![Frame {
                ctx,
                branch: Vec::new(),
                occ: HashMap::new(),
                depth_scope: 0,
                caller: None,
            }],
            ledgers,
            flushed: 0,
            step_limit: 0,
            step_scope: 0,
        };
        s.limits(0);
        s
    }

    pub(crate) fn top(&self) -> &Frame {
        // The session's frame is never popped.
        &self.frames[self.frames.len() - 1]
    }

    fn top_mut(&mut self) -> &mut Frame {
        let i = self.frames.len() - 1;
        &mut self.frames[i]
    }

    /// The top frame's index (0: the session's).
    pub(crate) fn index(&self) -> u32 {
        u32::try_from(self.frames.len() - 1).unwrap_or(u32::MAX)
    }

    /// The session's scope (the bottom frame's).
    pub(crate) fn session_scope(&self) -> &Scope {
        &self.frames[0].ctx.scope
    }

    /// Debits the steps charged since the last flush to the top frame's ledgers. The machine
    /// has already kept them within [`FrameStack::step_limit`].
    pub(crate) fn flush(&mut self, steps: u64) {
        let delta = steps.saturating_sub(self.flushed);
        if delta > 0 {
            self.ledgers.record_steps(self.top().ctx.node, delta);
        }
        self.flushed = steps;
    }

    /// Recomputes the top frame's step limit from its ledgers.
    fn limits(&mut self, steps: u64) {
        let node = self.top().ctx.node;
        let room = self.ledgers.remaining(node).steps;
        self.step_limit = room.map_or(u64::MAX, |r| steps.saturating_add(r));
        self.step_scope = self
            .ledgers
            .binding(node, BudgetKind::Steps)
            .and_then(|n| self.frames.iter().position(|f| f.ctx.node == n))
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(0);
    }

    /// The comparisons opening a frame with `request` makes over the top frame, every pass
    /// counted (one step each): for tools, each required tool met with the parent's, the
    /// request's own cover filter, the pairwise meet and the cover filter over its result, and
    /// `check_within`; for paths, the request's cover filter, both directions of the meet, the
    /// cover filter over the result and `check_within`; the rules copied and compared; and a
    /// constant for the other dimensions.
    pub(crate) fn open_work(&self, request: &Request) -> u64 {
        let s = &self.top().ctx.scope;
        let n = |x: usize| x as u64;
        let (pt, pp) = (n(s.tools.len()), n(s.paths.len()));
        let (need, rt) = request.tools.as_ref().map_or((0, 0), |t| {
            (n(t.need.len()), n(t.need.len() + t.want.len()))
        });
        let rp = request.paths.as_ref().map_or(0, |p| n(p.len()));
        let rules = n(s.rules.allow.len() + s.rules.ask.len() + s.rules.deny.len())
            .saturating_add(n(request.deny.len() + request.ask.len()));
        let mul = u64::saturating_mul;
        let square = |x: u64| x.saturating_mul(x);
        let (ct, cp) = (rt.saturating_add(pt), rp.saturating_add(pp));
        let tools = [
            mul(need, pt),
            square(rt),
            mul(rt, pt),
            square(ct),
            mul(ct, pt),
        ];
        let paths = [square(rp), mul(2, mul(rp, pp)), square(cp), mul(cp, pp)];
        tools
            .into_iter()
            .chain(paths)
            .fold(mul(3, rules).saturating_add(32), u64::saturating_add)
    }

    /// Opens a frame of `kind` over the top frame with `request`; `site` is the call site
    /// (or the block's) and `name` the harness's.
    #[allow(
        clippy::too_many_arguments,
        reason = "what a frame opens with, each from a different source"
    )]
    pub(crate) fn open(
        &mut self,
        steps: u64,
        kind: FrameKind,
        request: &Request,
        settings: Settings,
        name: &Arc<str>,
        site: u32,
        loc: Loc,
        origin: FrameOrigin,
    ) -> Result<(), OpenError> {
        if !origin.fits(kind) {
            return Err(OpenError {
                refusal: Refusal {
                    depth: false,
                    dimension: super::scope::Dimension::Tools,
                    message: "a frame whose origin does not fit its kind (an interpreter bug)"
                        .to_owned(),
                },
                scope: 0,
            });
        }
        self.flush(steps);
        let index = u32::try_from(self.frames.len()).unwrap_or(u32::MAX);
        let parent = self.top();
        let room = self.ledgers.remaining(parent.ctx.node);
        let parent_scope = Arc::clone(&parent.ctx.scope);
        let depth_scope = parent.depth_scope;
        let refuse = |refusal| OpenError {
            refusal,
            scope: depth_scope,
        };
        let scope = attenuate(&parent_scope, &room, request, kind).map_err(refuse)?;
        // Defence in depth: what attenuation computed must pass the host's own check.
        if let Err(d) = check_within(&parent_scope, &scope, kind) {
            return Err(refuse(Refusal {
                depth: false,
                dimension: d,
                message: format!("attenuation widened `{}` (an interpreter bug)", d.name()),
            }));
        }
        let depth_scope = match request.depth {
            Some(d) if d < parent_scope.max_depth => index,
            _ => depth_scope,
        };
        let parent_path = Arc::clone(&parent.ctx.path);
        let parent_settings = Arc::clone(&parent.ctx.settings);
        let caller_name = Arc::clone(&parent.ctx.harness);
        let parent_node = parent.ctx.node;
        let top = self.top_mut();
        let path: Arc<str> = if kind == FrameKind::Entry {
            Arc::clone(&parent_path)
        } else {
            let occ = top.next_occ(site);
            Arc::from(format!(
                "{parent_path}/{}{name}@s{site}#{occ}",
                top.branch_prefix()
            ))
        };
        let Some(node) = self.ledgers.open(parent_node, scope.budget, scope.width) else {
            return Err(refuse(Refusal {
                depth: false,
                dimension: super::scope::Dimension::Budget(BudgetKind::Steps),
                message: "the caller's ledger is closed".to_owned(),
            }));
        };
        // Every setting left out is inherited from the caller, except a harness's title,
        // which defaults to its name (chapter 04).
        let settings = match kind {
            FrameKind::Entry | FrameKind::Harness => settings.over(&Settings {
                title: None,
                ..(*parent_settings).clone()
            }),
            FrameKind::Block => settings.over(&parent_settings),
        };
        self.frames.push(Frame {
            ctx: FrameCtx {
                path,
                parent: (kind != FrameKind::Entry).then_some(parent_path),
                kind,
                harness: Arc::clone(name),
                scope: Arc::new(scope),
                ledgers: self.ledgers.clone(),
                node,
                settings: Arc::new(settings),
                origin,
            },
            branch: Vec::new(),
            occ: HashMap::new(),
            depth_scope,
            caller: (kind == FrameKind::Harness).then_some(Caller {
                harness: caller_name,
                loc,
            }),
        });
        self.limits(steps);
        Ok(())
    }

    /// Closes the top frame (never the session's) and returns it.
    pub(crate) fn close(&mut self, steps: u64) -> Option<Frame> {
        if self.frames.len() < 2 {
            return None;
        }
        self.flush(steps);
        let f = self.frames.pop()?;
        self.ledgers.close(f.ctx.node);
        self.limits(steps);
        Some(f)
    }

    /// The next occurrence of `site` in the top frame and branch path (an effect, a harness
    /// call or a concurrent construct).
    pub(crate) fn next_occ(&mut self, site: u32) -> u32 {
        self.top_mut().next_occ(site)
    }

    /// The frames of branch `idx` of the construct at `site` (occurrence `occ`) in the top
    /// frame: the same frames, the branch element pushed onto the top frame's branch path,
    /// and its own step count (from 0). The occurrence counts below are not copied: the
    /// branch's paths are its own, so none of them can be counted yet.
    pub(crate) fn child(&self, site: u32, occ: u32, idx: u32) -> FrameStack {
        let mut frames: Vec<Frame> = self
            .frames
            .iter()
            .map(|f| Frame {
                ctx: f.ctx.clone(),
                branch: f.branch.clone(),
                occ: HashMap::new(),
                depth_scope: f.depth_scope,
                caller: f.caller.clone(),
            })
            .collect();
        if let Some(top) = frames.last_mut() {
            top.branch.push(BranchEl { site, occ, idx });
        }
        let mut s = FrameStack {
            frames,
            ledgers: self.ledgers.clone(),
            flushed: 0,
            step_limit: 0,
            step_scope: 0,
        };
        s.limits(0);
        s
    }

    /// Debits the steps since the last flush and recomputes the top frame's step limit from
    /// the ledgers (which concurrent branches debit too).
    pub(crate) fn refresh(&mut self, steps: u64) {
        self.flush(steps);
        self.limits(steps);
    }

    /// Adds a finished branch's steps (already debited on the ledgers by the branch).
    pub(crate) fn absorb(&mut self, steps: u64, branch_steps: u64) {
        self.flushed = self.flushed.saturating_add(branch_steps);
        self.limits(steps);
    }

    /// The session's frame's ledger node.
    pub(crate) fn session_node(&self) -> NodeId {
        self.frames[0].ctx.node
    }

    /// The path of the open frame whose ledger node is `node`.
    pub(crate) fn path_of(&self, node: NodeId) -> Option<Arc<str>> {
        self.frames
            .iter()
            .find(|f| f.ctx.node == node)
            .map(|f| Arc::clone(&f.ctx.path))
    }

    /// The ledger nodes of the open frames, bottom first.
    pub(crate) fn nodes(&self) -> Vec<NodeId> {
        self.frames.iter().map(|f| f.ctx.node).collect()
    }

    /// The stack index of the frame whose ledger node is `node` (0: the turn's).
    pub(crate) fn index_of(&self, node: NodeId) -> u32 {
        self.frames
            .iter()
            .position(|f| f.ctx.node == node)
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(0)
    }

    /// The frame below the top one (the caller of the top frame), if any.
    pub(crate) fn below_top(&self) -> Option<&FrameCtx> {
        let n = self.frames.len();
        (n >= 2).then(|| &self.frames[n - 2].ctx)
    }

    /// The top frame's branch path, rendered (empty outside concurrent constructs).
    pub(crate) fn branch_path(&self) -> String {
        let mut s = self.top().branch_prefix();
        s.pop();
        s
    }
}
