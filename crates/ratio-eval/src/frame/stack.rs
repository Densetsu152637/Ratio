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

#[derive(Debug)]
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

#[derive(Debug)]
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
    ) -> Result<(), OpenError> {
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
        let node = self.ledgers.open(parent_node, scope.budget, scope.width);
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

    /// A concurrent construct starts in the top frame (its first branch, index 0).
    pub(crate) fn enter_branch(&mut self, site: u32) {
        let top = self.top_mut();
        let occ = top.next_occ(site);
        top.branch.push(BranchEl { site, occ, idx: 0 });
    }

    /// The construct moves on to branch `idx`.
    pub(crate) fn set_branch(&mut self, idx: u32) {
        if let Some(b) = self.top_mut().branch.last_mut() {
            b.idx = idx;
        }
    }

    /// The construct ends.
    pub(crate) fn leave_branch(&mut self) {
        self.top_mut().branch.pop();
    }

    /// The top frame's branch path, rendered (empty outside concurrent constructs).
    pub(crate) fn branch_path(&self) -> String {
        let mut s = self.top().branch_prefix();
        s.pop();
        s
    }
}
