//! What the machine hands the scheduler at a concurrent construct, and what it gets back.
//!
//! `par { … }`, `race { … }`, `par([…])`, `race([…])`, `par_map` and `par_map_settled` suspend
//! the machine with [`crate::Suspend::Branches`]: the scheduler starts each branch as a machine
//! of its own ([`crate::Machine::spawn`]), runs them concurrently within the width, cancels
//! them as the construct requires, and answers the construct with [`Joined`]
//! ([`crate::Machine::join`]). `with timeout` and `timeout(d, f)` stay in their machine: a
//! timeout scope ([`crate::Machine::timeouts`]) the scheduler fires by cancelling it.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use ratio_ir::Loc;

use crate::error::RtError;
use crate::frame::{FrameCtx, NodeId};
use crate::value::Value;

/// How a construct combines its branches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ForkKind {
    /// `par`, `par_map`: every branch's value, in order; the first failure cancels the rest.
    All,
    /// `par_map_settled`: every branch's `Result`, in order; nothing is cancelled.
    Settled,
    /// `race`: the first success; the rest are cancelled; when all fail, the failure of the
    /// first branch in written order.
    Race,
}

/// A concurrent construct, as the scheduler sees it.
#[derive(Clone, Debug)]
pub struct ForkInfo {
    pub kind: ForkKind,
    /// The number of branches.
    pub count: u32,
    /// The construct's own cap on its running branches (`limit:`).
    pub limit: Option<u32>,
    /// The construct's site (node index) and occurrence: its branch path element is
    /// `c<site>#<occurrence>.b<index>`.
    pub site: u32,
    pub occurrence: u32,
    /// The frame the construct runs in (its ledgers take the branches' slots).
    pub frame: FrameCtx,
    pub loc: Loc,
}

/// What a construct's branches produced, as the scheduler decided it.
#[derive(Debug)]
pub enum Joined {
    /// Every branch returned (for `All`), in branch order.
    Values(Vec<Value>),
    /// Every branch's outcome (for `Settled`), in branch order.
    Settled(Vec<Result<Value, RtError>>),
    /// A branch won the race with this value.
    Won(Value),
    /// The construct raised this error (a failing `par` branch, every `race` branch failing,
    /// or a cancellation of the construct itself).
    Failed(RtError),
}

/// Outstanding drains by owning frame (chapter 03, "The drain fence"): a frame with any may
/// not close. Shared between the scheduler, which counts them, and the machines, which check
/// before closing a frame.
#[derive(Debug, Default)]
pub struct Fence {
    held: Mutex<HashMap<NodeId, u32>>,
}

impl Fence {
    /// One more outstanding drain owned by `node`.
    pub fn hold(&self, node: NodeId) {
        let mut h = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        *h.entry(node).or_insert(0) += 1;
    }

    /// An outstanding drain owned by `node` finished (or moved away).
    pub fn release(&self, node: NodeId) {
        let mut h = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = h.get_mut(&node) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                h.remove(&node);
            }
        }
    }

    /// Whether `node` owns an outstanding drain.
    #[must_use]
    pub fn holds(&self, node: NodeId) -> bool {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&node)
    }

    /// The outstanding drains in the whole frame tree.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .sum()
    }
}

/// An active timeout scope of a machine: its id (unique within the machine), the duration
/// it was given and the frame it was entered in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeoutScope {
    pub id: u32,
    /// Microseconds.
    pub micros: i64,
    pub frame: NodeId,
}
