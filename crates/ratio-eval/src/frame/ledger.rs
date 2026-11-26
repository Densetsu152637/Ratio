//! The budget ledgers of a frame tree (chapter 04, "Budgets in detail" and "Admission by
//! bounded reservation"; the performance review's PC-1 and PC-2).
//!
//! Every frame has a ledger node under its caller's; the root is the session's. An effect is
//! **admitted** by reserving its admission charge on its frame's node and every ancestor's at
//! once, and only when every one can cover it, so concurrent effects never overspend any cap,
//! a child frame's lower cap included, under any interleaving. Completion replaces the
//! reservation with the actual charge; unspent budget is simply never debited, so it stays
//! with the caller. All of the tree sits behind one lock: a reservation is atomic across the
//! whole path.
//!
//! A model call reserves one `model_calls`, its counted input tokens and its **effective
//! `max_tokens`**, the smallest of the requested `max_tokens`, the model's maximum output (its
//! card's, or its window minus the input) and the tokens and cost every ledger on the path can
//! still cover after the input. The effective `max_tokens` may not fall below the **reply
//! floor**, the smallest of `min_reply_tokens`, the requested `max_tokens` and the model's
//! maximum output (never below one token). Below it, or when the input itself cannot be
//! covered, the admission **waits** while a reservation is held on the ledger whose cap binds
//! (only a release there can make room) and is refused once none is. A wait carries the
//! release epoch read under the lock: [`Ledgers::wait_for_release`] blocks, and
//! [`Ledgers::register_waker`] wakes an async waiter, as soon as any release moves the epoch,
//! so no release between the admission and the wait is lost. A hosted model without prices is
//! refused under any cost cap on the path; a `local` model, or a price of 0, counts as priced.
//!
//! **Completion and overshoot.** [`Ledgers::complete`] debits the actual charge on the whole
//! path, even beyond what was reserved, and returns the overshoot: a provider that reports
//! more than the reservation (an input miscounted, say) is charged in full, so later
//! admissions see it; the host records it (stage 11a). A reservation belongs to one tree and
//! is given back once: completing it on another tree, or twice, is an error.
//!
//! Every sum is checked or saturating towards "spent more": an overflow can refuse more, never
//! widen a budget. The `steps` ledger is the same tree's: branches of a frame share its node,
//! and the branch that would pass a cap is refused. A closed node is freed once nothing is
//! held on it and it has no open children, so a loop of `with` blocks keeps the tree small;
//! a [`NodeId`] carries a generation, so a stale id never names a reused node.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::Waker;

use super::scope::{BudgetKind, Caps};

/// A frame's ledger node: its slot and the slot's generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

impl NodeId {
    /// The node's slot (slots of freed nodes are reused, under a new generation).
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }
}

/// Amounts on a ledger: what is spent, or held by reservations. Cost is in micro-units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Amounts {
    pub tokens: u64,
    pub cost: u64,
    pub steps: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
}

impl Amounts {
    fn get(&self, k: BudgetKind) -> u64 {
        match k {
            BudgetKind::Tokens => self.tokens,
            BudgetKind::Cost => self.cost,
            BudgetKind::Steps => self.steps,
            BudgetKind::ModelCalls => self.model_calls,
            BudgetKind::ToolCalls => self.tool_calls,
            BudgetKind::Wall => 0,
        }
    }

    /// The sum, saturating: a ledger that overflows counts as full.
    fn plus(self, o: Amounts) -> Amounts {
        Amounts {
            tokens: self.tokens.saturating_add(o.tokens),
            cost: self.cost.saturating_add(o.cost),
            steps: self.steps.saturating_add(o.steps),
            model_calls: self.model_calls.saturating_add(o.model_calls),
            tool_calls: self.tool_calls.saturating_add(o.tool_calls),
        }
    }

    fn minus(self, o: Amounts) -> Amounts {
        Amounts {
            tokens: self.tokens.saturating_sub(o.tokens),
            cost: self.cost.saturating_sub(o.cost),
            steps: self.steps.saturating_sub(o.steps),
            model_calls: self.model_calls.saturating_sub(o.model_calls),
            tool_calls: self.tool_calls.saturating_sub(o.tool_calls),
        }
    }
}

/// A model's prices, in micro-units of money per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prices {
    pub input: u64,
    pub output: u64,
}

/// What an admission reads from a model's card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Card {
    /// The most output tokens the model gives.
    pub max_output: Option<u64>,
    /// Its context window, for a maximum output when the card gives none.
    pub window: Option<u64>,
    /// `None`: no price is configured.
    pub prices: Option<Prices>,
    /// Location `local`: priced at zero.
    pub local: bool,
}

/// The default reply floor (`ratio.limits.min_reply_tokens`, chapter 07).
pub const MIN_REPLY_TOKENS: u64 = 256;

/// A model call to admit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCall {
    /// The counted input tokens (`Host::count_tokens`).
    pub input: u64,
    /// The `max_tokens` the program asked for.
    pub max_tokens: Option<u64>,
    pub card: Card,
    /// `ratio.limits.min_reply_tokens`.
    pub min_reply: u64,
}

/// A refusal: the budget kind, and the node whose cap could not cover the charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub which: BudgetKind,
    pub node: NodeId,
}

/// A held reservation. Give it back with [`Ledgers::complete`] or [`Ledgers::cancel`].
#[must_use = "a reservation holds budget until it is completed or cancelled"]
#[derive(Debug, PartialEq, Eq)]
pub struct Reservation {
    tree: u64,
    id: u64,
    /// What is held on every ledger of the path.
    pub held: Amounts,
    /// For a model call, the effective `max_tokens` to send (`None`: nothing bounds it).
    pub max_tokens: Option<u64>,
}

/// Why a reservation could not be given back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerError {
    /// It belongs to another tree.
    WrongTree,
    /// It was already completed or cancelled.
    Released,
}

/// The outcome of a model call's admission.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Admitted(Reservation),
    /// Below the floor (or the input uncovered) while reservations are held on `node`, the
    /// ledger whose cap binds: retry once the release epoch differs from `epoch`
    /// ([`Ledgers::wait_for_release`], [`Ledgers::register_waker`]).
    Wait {
        epoch: u64,
        node: NodeId,
    },
    Refused(Refused),
}

#[derive(Debug)]
struct Node {
    generation: u32,
    parent: Option<u32>,
    caps: Caps,
    width: u32,
    spent: Amounts,
    reserved: Amounts,
    /// Reservations held on this node.
    held: u32,
    /// Open or not yet freed children.
    children: u32,
    running: u32,
    open: bool,
}

#[derive(Debug)]
struct Res {
    path: Vec<u32>,
    held: Amounts,
}

#[derive(Debug, Default)]
struct Tree {
    nodes: Vec<Option<Node>>,
    gens: Vec<u32>,
    free: Vec<u32>,
    reservations: HashMap<u64, Res>,
    next: u64,
    /// Bumped at every release, so a waiting admission knows when to retry.
    releases: u64,
    wakers: Vec<Waker>,
}

#[derive(Debug)]
struct Shared {
    id: u64,
    tree: Mutex<Tree>,
    released: Condvar,
}

/// The ledgers of one frame tree, shared by every branch and frame in it (and with the
/// host, which admits effects through it).
#[derive(Clone, Debug)]
pub struct Ledgers(Arc<Shared>);

/// Tree ids, so a reservation is never given back to another tree.
static TREES: AtomicU64 = AtomicU64::new(1);

/// Units per million, for prices.
const MILLION: u128 = 1_000_000;

/// The cost of `tokens` at `price` per million, rounded up.
fn cost_of(tokens: u64, price: u64) -> u64 {
    let c = (u128::from(tokens) * u128::from(price)).div_ceil(MILLION);
    u64::try_from(c).unwrap_or(u64::MAX)
}

/// The most tokens `budget` pays for at `price` per million (`None`: unbounded).
fn tokens_for(budget: u64, price: u64) -> Option<u64> {
    if price == 0 {
        return None;
    }
    let t = u128::from(budget) * MILLION / u128::from(price);
    Some(u64::try_from(t).unwrap_or(u64::MAX))
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    super::scope::min_cap(a, b)
}

impl Tree {
    fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes
            .get(id.index as usize)?
            .as_ref()
            .filter(|n| n.generation == id.generation)
    }

    fn node(&self, i: u32) -> &Node {
        self.nodes[i as usize]
            .as_ref()
            .expect("a node on a live path")
    }

    fn node_mut(&mut self, i: u32) -> &mut Node {
        self.nodes[i as usize]
            .as_mut()
            .expect("a node on a live path")
    }

    fn id(&self, i: u32) -> NodeId {
        NodeId {
            index: i,
            generation: self.node(i).generation,
        }
    }

    /// The path from `node` up to the root (empty for a freed or stale id).
    fn path(&self, node: NodeId) -> Vec<u32> {
        let mut out = Vec::new();
        if self.get(node).is_none() {
            return out;
        }
        let mut at = Some(node.index);
        while let Some(i) = at {
            out.push(i);
            at = self.node(i).parent;
        }
        out
    }

    /// The path, when every node on it is open.
    fn open_path(&self, node: NodeId) -> Option<Vec<u32>> {
        let path = self.path(node);
        (!path.is_empty() && path.iter().all(|&i| self.node(i).open)).then_some(path)
    }

    /// What `i` can still cover of `k`: its cap less what is spent and held.
    fn avail(&self, i: u32, k: BudgetKind) -> Option<u64> {
        let n = self.node(i);
        n.caps.get(k).map(|cap| {
            cap.saturating_sub(n.spent.get(k))
                .saturating_sub(n.reserved.get(k))
        })
    }

    fn reserve(
        &mut self,
        tree: u64,
        path: Vec<u32>,
        held: Amounts,
        max_tokens: Option<u64>,
    ) -> Reservation {
        for &i in &path {
            let n = self.node_mut(i);
            n.reserved = n.reserved.plus(held);
            n.held = n.held.saturating_add(1);
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.reservations.insert(id, Res { path, held });
        Reservation {
            tree,
            id,
            held,
            max_tokens,
        }
    }

    /// Releases a reservation, debiting `actual` on its path; the overshoot beyond what was
    /// held.
    fn release(&mut self, r: &Reservation, actual: Amounts) -> Result<Amounts, LedgerError> {
        let res = self
            .reservations
            .remove(&r.id)
            .ok_or(LedgerError::Released)?;
        for &i in &res.path {
            let n = self.node_mut(i);
            n.reserved = n.reserved.minus(res.held);
            n.spent = n.spent.plus(actual);
            n.held = n.held.saturating_sub(1);
        }
        for &i in &res.path {
            self.try_free(i);
        }
        self.releases = self.releases.wrapping_add(1);
        Ok(actual.minus(res.held))
    }

    /// Frees `i`, and then its ancestors, while each is closed, holds nothing and has no
    /// children.
    fn try_free(&mut self, mut i: u32) {
        loop {
            let Some(n) = self.nodes.get(i as usize).and_then(Option::as_ref) else {
                return;
            };
            if n.open || n.held > 0 || n.children > 0 {
                return;
            }
            let parent = n.parent;
            self.nodes[i as usize] = None;
            self.gens[i as usize] = self.gens[i as usize].wrapping_add(1);
            self.free.push(i);
            let Some(p) = parent else {
                return;
            };
            let pn = self.node_mut(p);
            pn.children = pn.children.saturating_sub(1);
            i = p;
        }
    }

    /// The first node of `path` that cannot cover one more of `k`.
    fn short_of_one(&self, path: &[u32], k: BudgetKind) -> Option<u32> {
        path.iter().copied().find(|&i| self.avail(i, k) == Some(0))
    }

    fn insert(&mut self, mut node: Node) -> NodeId {
        let index = if let Some(i) = self.free.pop() {
            i
        } else {
            self.nodes.push(None);
            self.gens.push(0);
            u32::try_from(self.nodes.len() - 1).unwrap_or(u32::MAX)
        };
        let generation = self.gens[index as usize];
        node.generation = generation;
        self.nodes[index as usize] = Some(node);
        NodeId { index, generation }
    }
}

fn fresh(parent: Option<u32>, caps: Caps, width: u32) -> Node {
    Node {
        generation: 0,
        parent,
        caps,
        width,
        spent: Amounts::default(),
        reserved: Amounts::default(),
        held: 0,
        children: 0,
        running: 0,
        open: true,
    }
}

impl Ledgers {
    /// A tree whose root is the session's ledger, with these caps and width.
    #[must_use]
    pub fn new(caps: Caps, width: u32) -> (Ledgers, NodeId) {
        let mut tree = Tree::default();
        let root = tree.insert(fresh(None, caps, width));
        let shared = Shared {
            id: TREES.fetch_add(1, Ordering::Relaxed),
            tree: Mutex::new(tree),
            released: Condvar::new(),
        };
        (Ledgers(Arc::new(shared)), root)
    }

    /// The lock. A panic while it was held can only have happened between whole updates
    /// (every update here is a sequence of saturating writes), so the tree is used as is.
    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.0
            .tree
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Opens a child frame's node; `None` when the parent is closed or gone.
    #[must_use]
    pub fn open(&self, parent: NodeId, caps: Caps, width: u32) -> Option<NodeId> {
        let mut t = self.tree();
        t.open_path(parent)?;
        t.node_mut(parent.index).children += 1;
        Some(t.insert(fresh(Some(parent.index), caps, width)))
    }

    /// Closes a frame's node. Reservations still held on it (a drained call) stay held on
    /// it and its ancestors until they complete; nothing new is admitted on it or below it.
    /// The node is freed once nothing is held on it and it has no children.
    pub fn close(&self, node: NodeId) {
        let mut t = self.tree();
        if t.get(node).is_some() {
            t.node_mut(node.index).open = false;
            t.try_free(node.index);
        }
    }

    /// How many node slots the tree has (live and free).
    #[must_use]
    pub fn slots(&self) -> usize {
        self.tree().nodes.len()
    }

    /// What `node` may still spend of each kind: the least over it and its ancestors.
    #[must_use]
    pub fn remaining(&self, node: NodeId) -> Caps {
        let t = self.tree();
        let mut out = Caps::NONE;
        for i in t.path(node) {
            for k in BudgetKind::ALL {
                out.set(k, min_opt(out.get(k), t.avail(i, k)));
            }
        }
        out
    }

    /// What `node` has spent (its own and its children's completed charges).
    #[must_use]
    pub fn spent(&self, node: NodeId) -> Amounts {
        self.tree()
            .get(node)
            .map_or_else(Amounts::default, |n| n.spent)
    }

    /// What reservations hold on `node`.
    #[must_use]
    pub fn reserved(&self, node: NodeId) -> Amounts {
        self.tree()
            .get(node)
            .map_or_else(Amounts::default, |n| n.reserved)
    }

    /// How many reservations are held on `node`.
    #[must_use]
    pub fn held_on(&self, node: NodeId) -> u32 {
        self.tree().get(node).map_or(0, |n| n.held)
    }

    /// `node`'s caps.
    #[must_use]
    pub fn caps(&self, node: NodeId) -> Caps {
        self.tree().get(node).map_or(Caps::NONE, |n| n.caps)
    }

    /// How many reservations the tree holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.tree().reservations.len()
    }

    /// The release epoch: bumped at every release.
    #[must_use]
    pub fn releases(&self) -> u64 {
        self.tree().releases
    }

    /// Blocks **the calling thread** until a release moves the epoch past `epoch` (the one a
    /// [`Admission::Wait`] gave), then returns the new epoch. A release between the admission
    /// and this call is not lost: the epoch already differs. For hosts and tests on threads
    /// of their own; the interpreter's scheduler (stage 11a) must not block its thread here,
    /// and uses [`Ledgers::register_waker`] instead.
    pub fn wait_for_release(&self, epoch: u64) -> u64 {
        let mut t = self.tree();
        while t.releases == epoch {
            t = self
                .0
                .released
                .wait(t)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        t.releases
    }

    /// Registers `waker` to be woken at the next release, for an async waiter: `false` (and
    /// nothing registered) when the epoch already differs from `epoch`, so retry at once. This
    /// is what the stage 11a scheduler uses on the interpreter's thread. Wakers are woken after
    /// the ledger's lock is released, so a waker may touch the ledgers.
    #[must_use]
    pub fn register_waker(&self, epoch: u64, waker: &Waker) -> bool {
        let mut t = self.tree();
        if t.releases != epoch {
            return false;
        }
        if !t.wakers.iter().any(|w| w.will_wake(waker)) {
            t.wakers.push(waker.clone());
        }
        true
    }

    /// Admits a tool call: one `tool_calls` on every ledger of the path.
    ///
    /// # Errors
    ///
    /// `tool_calls` when a ledger on the path has none left (or the frame is closed).
    pub fn admit_tool(&self, node: NodeId) -> Result<Reservation, Refused> {
        let mut t = self.tree();
        let refuse = |n: NodeId| Refused {
            which: BudgetKind::ToolCalls,
            node: n,
        };
        let path = t.open_path(node).ok_or(refuse(node))?;
        if let Some(i) = t.short_of_one(&path, BudgetKind::ToolCalls) {
            return Err(refuse(t.id(i)));
        }
        let held = Amounts {
            tool_calls: 1,
            ..Amounts::default()
        };
        Ok(t.reserve(self.0.id, path, held, None))
    }

    /// Admits a model call by reservation (see the module documentation).
    #[must_use]
    pub fn admit_model(&self, node: NodeId, call: &ModelCall) -> Admission {
        let mut t = self.tree();
        let Some(path) = t.open_path(node) else {
            return Admission::Refused(Refused {
                which: BudgetKind::ModelCalls,
                node,
            });
        };
        let prices = if call.card.local {
            Some(Prices::default())
        } else {
            call.card.prices
        };
        let prices = if let Some(p) = prices {
            p
        } else {
            // A hosted model without prices: refused under any cost cap on the path.
            if let Some(&i) = path.iter().find(|&&i| t.node(i).caps.cost.is_some()) {
                return Admission::Refused(Refused {
                    which: BudgetKind::Cost,
                    node: t.id(i),
                });
            }
            Prices::default()
        };
        if let Some(i) = t.short_of_one(&path, BudgetKind::ModelCalls) {
            return Admission::Refused(Refused {
                which: BudgetKind::ModelCalls,
                node: t.id(i),
            });
        }
        let in_cost = cost_of(call.input, prices.input);
        // The output every ledger can cover after the input, the ledger that binds it, and
        // the first ledger that cannot cover the input itself.
        let mut room: Option<u64> = None;
        let mut short: Option<(BudgetKind, u32)> = None;
        let mut binding: Option<(BudgetKind, u32)> = None;
        for &i in &path {
            if let Some(avail) = t.avail(i, BudgetKind::Tokens) {
                match avail.checked_sub(call.input) {
                    Some(out) => {
                        if room.is_none_or(|r| out < r) {
                            room = Some(out);
                            binding = Some((BudgetKind::Tokens, i));
                        }
                    }
                    None => {
                        short.get_or_insert((BudgetKind::Tokens, i));
                    }
                }
            }
            if let Some(avail) = t.avail(i, BudgetKind::Cost) {
                match avail.checked_sub(in_cost) {
                    Some(left) => {
                        if let Some(out) = tokens_for(left, prices.output)
                            && room.is_none_or(|r| out < r)
                        {
                            room = Some(out);
                            binding = Some((BudgetKind::Cost, i));
                        }
                    }
                    None => {
                        short.get_or_insert((BudgetKind::Cost, i));
                    }
                }
            }
        }
        let model_max = call
            .card
            .max_output
            .or_else(|| call.card.window.map(|w| w.saturating_sub(call.input)));
        let effective = min_opt(min_opt(call.max_tokens, model_max), room);
        let floor = call
            .min_reply
            .min(call.max_tokens.unwrap_or(u64::MAX))
            .min(model_max.unwrap_or(u64::MAX))
            .max(1);
        let below = effective.is_some_and(|e| e < floor);
        if short.is_some() || below {
            // Only a release on the ledger that binds can make room there.
            let (which, i) = short
                .or(binding)
                .unwrap_or((BudgetKind::Tokens, node.index));
            let at = t.id(i);
            if t.node(i).held > 0 {
                return Admission::Wait {
                    epoch: t.releases,
                    node: at,
                };
            }
            return Admission::Refused(Refused { which, node: at });
        }
        let out = effective.unwrap_or(0);
        let held = Amounts {
            tokens: call.input.saturating_add(out),
            cost: in_cost.saturating_add(cost_of(out, prices.output)),
            model_calls: 1,
            ..Amounts::default()
        };
        Admission::Admitted(t.reserve(self.0.id, path, held, effective))
    }

    /// Completes an effect: its reservation is replaced by `actual` on every ledger of its
    /// path, even beyond what was held; returns the overshoot (`actual` less what was held).
    ///
    /// # Errors
    ///
    /// The reservation belongs to another tree, or was already given back.
    pub fn complete(&self, r: &Reservation, actual: Amounts) -> Result<Amounts, LedgerError> {
        if r.tree != self.0.id {
            return Err(LedgerError::WrongTree);
        }
        // The wakers are woken after the lock is dropped: a waker may poll a future that
        // takes the lock again.
        let (out, wakers) = {
            let mut t = self.tree();
            let out = t.release(r, actual);
            let wakers = if out.is_ok() {
                std::mem::take(&mut t.wakers)
            } else {
                Vec::new()
            };
            (out, wakers)
        };
        self.0.released.notify_all();
        for w in wakers {
            w.wake();
        }
        out
    }

    /// Gives a reservation back without a charge (the effect never started).
    ///
    /// # Errors
    ///
    /// As [`Ledgers::complete`].
    pub fn cancel(&self, r: &Reservation) -> Result<(), LedgerError> {
        self.complete(r, Amounts::default()).map(|_| ())
    }

    /// Debits `n` steps on `node` and its ancestors.
    ///
    /// # Errors
    ///
    /// `steps` when a ledger on the path cannot cover them; nothing is debited then.
    pub fn debit_steps(&self, node: NodeId, n: u64) -> Result<(), Refused> {
        let mut t = self.tree();
        let path = t.path(node);
        if let Some(&i) = path
            .iter()
            .find(|&&i| t.avail(i, BudgetKind::Steps).is_some_and(|a| a < n))
        {
            return Err(Refused {
                which: BudgetKind::Steps,
                node: t.id(i),
            });
        }
        for i in path {
            let x = t.node_mut(i);
            x.spent.steps = x.spent.steps.saturating_add(n);
        }
        Ok(())
    }

    /// Records `n` steps on `node` and its ancestors without checking: for a caller that
    /// already kept them within [`Ledgers::remaining`] (the sequential machine).
    pub fn record_steps(&self, node: NodeId, n: u64) {
        let mut t = self.tree();
        for i in t.path(node) {
            let x = t.node_mut(i);
            x.spent.steps = x.spent.steps.saturating_add(n);
        }
    }

    /// The node on `node`'s path with the least left of `k` (the cap that binds), if any is
    /// capped; of equal ones, the outermost.
    #[must_use]
    pub fn binding(&self, node: NodeId, k: BudgetKind) -> Option<NodeId> {
        let t = self.tree();
        let mut best: Option<(u64, u32)> = None;
        for i in t.path(node) {
            if let Some(a) = t.avail(i, k)
                && best.is_none_or(|(b, _)| a <= b)
            {
                best = Some((a, i));
            }
        }
        best.map(|(_, i)| t.id(i))
    }

    /// Takes a running slot for a branch of `node`'s frame: `false` when `node` or an
    /// ancestor already runs as many branches as its width (the branch waits; a parked
    /// branch holds no slot).
    #[must_use]
    pub fn acquire_slot(&self, node: NodeId) -> bool {
        let mut t = self.tree();
        let path = t.path(node);
        if path.is_empty()
            || path.iter().any(|&i| {
                let n = t.node(i);
                n.running >= n.width
            })
        {
            return false;
        }
        for i in path {
            let x = t.node_mut(i);
            x.running = x.running.saturating_add(1);
        }
        true
    }

    /// Gives a running slot back (the branch finished or parked).
    pub fn release_slot(&self, node: NodeId) {
        let mut t = self.tree();
        for i in t.path(node) {
            let x = t.node_mut(i);
            x.running = x.running.saturating_sub(1);
        }
    }

    /// The branches running in `node`'s frame and below.
    #[must_use]
    pub fn running(&self, node: NodeId) -> u32 {
        self.tree().get(node).map_or(0, |n| n.running)
    }
}
