//! The budget ledgers of a frame tree (chapter 04, "Budgets in detail" and "Admission by
//! bounded reservation"; the performance review's PC-1 and PC-2).
//!
//! Every frame has a ledger node under its caller's; the root is the session's. An effect is
//! **admitted** by reserving its admission charge on its frame's node and every ancestor's at
//! once, and only when every one can cover it, so concurrent effects never overspend any cap,
//! a child frame's lower cap included, under any interleaving. Completion replaces the
//! reservation with the actual charge; unspent budget is simply never debited, so it stays
//! with the caller. All of the tree sits behind one lock: a reservation is atomic across the
//! whole path, and the only waits are an admission's own retries after a release.
//!
//! A model call reserves one `model_calls`, its counted input tokens and its **effective
//! `max_tokens`**, the smallest of the requested `max_tokens`, the model's maximum output (its
//! card's, or its window minus the input) and the tokens and cost every ledger on the path can
//! still cover after the input. The effective `max_tokens` may not fall below the **reply
//! floor**, the smallest of `min_reply_tokens`, the requested `max_tokens` and the model's
//! maximum output (never below one token): below it the admission waits while any reservation
//! in the tree is held, and is refused once none is. A hosted model without prices is refused
//! under any cost cap on the path; a `local` model, or a price of 0, counts as priced.
//!
//! Every sum is checked or saturating towards "spent more": an overflow can refuse more, never
//! widen a budget. The `steps` ledger is the same tree's: branches of a frame share its node,
//! and the branch that would pass a cap is refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::scope::{BudgetKind, Caps};

/// A frame's ledger node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
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
#[derive(Debug, PartialEq, Eq)]
pub struct Reservation {
    id: u64,
    /// What is held on every ledger of the path.
    pub held: Amounts,
    /// For a model call, the effective `max_tokens` to send (`None`: nothing bounds it).
    pub max_tokens: Option<u64>,
}

/// The outcome of a model call's admission.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Admitted(Reservation),
    /// Below the floor while other reservations are held: retry when one is released.
    Wait,
    Refused(Refused),
}

#[derive(Debug)]
struct Node {
    parent: Option<u32>,
    caps: Caps,
    width: u32,
    spent: Amounts,
    reserved: Amounts,
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
    nodes: Vec<Node>,
    reservations: HashMap<u64, Res>,
    next: u64,
    /// Bumped at every release, so a waiting admission knows when to retry.
    releases: u64,
}

/// The ledgers of one frame tree, shared by every branch and frame in it (and with the
/// host, which admits effects through it).
#[derive(Clone, Debug)]
pub struct Ledgers(Arc<Mutex<Tree>>);

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
    /// The path from `node` up to the root.
    fn path(&self, node: NodeId) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = Some(node.0);
        while let Some(i) = at {
            let Some(n) = self.nodes.get(i as usize) else {
                break;
            };
            out.push(i);
            at = n.parent;
        }
        out
    }

    fn node(&self, i: u32) -> &Node {
        &self.nodes[i as usize]
    }

    /// What `i` can still cover of `k`: its cap less what is spent and held.
    fn avail(&self, i: u32, k: BudgetKind) -> Option<u64> {
        let n = self.node(i);
        n.caps.get(k).map(|cap| {
            cap.saturating_sub(n.spent.get(k))
                .saturating_sub(n.reserved.get(k))
        })
    }

    fn held(&self) -> usize {
        self.reservations.len()
    }

    fn reserve(&mut self, path: Vec<u32>, held: Amounts, max_tokens: Option<u64>) -> Reservation {
        for &i in &path {
            let n = &mut self.nodes[i as usize];
            n.reserved = n.reserved.plus(held);
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.reservations.insert(id, Res { path, held });
        Reservation {
            id,
            held,
            max_tokens,
        }
    }

    /// Releases a reservation, debiting `actual` on its path.
    fn release(&mut self, r: &Reservation, actual: Amounts) -> bool {
        let Some(res) = self.reservations.remove(&r.id) else {
            return false;
        };
        for &i in &res.path {
            let n = &mut self.nodes[i as usize];
            n.reserved = n.reserved.minus(res.held);
            n.spent = n.spent.plus(actual);
        }
        self.releases = self.releases.wrapping_add(1);
        true
    }

    /// The first node of `path` that cannot cover one more of `k`.
    fn short_of_one(&self, path: &[u32], k: BudgetKind) -> Option<u32> {
        path.iter().copied().find(|&i| self.avail(i, k) == Some(0))
    }
}

impl Ledgers {
    /// A tree whose root is the session's ledger, with these caps and width.
    #[must_use]
    pub fn new(caps: Caps, width: u32) -> (Ledgers, NodeId) {
        let tree = Tree {
            nodes: vec![Node {
                parent: None,
                caps,
                width,
                spent: Amounts::default(),
                reserved: Amounts::default(),
                running: 0,
                open: true,
            }],
            ..Tree::default()
        };
        (Ledgers(Arc::new(Mutex::new(tree))), NodeId(0))
    }

    /// The lock. A panic while it was held can only have happened between whole updates
    /// (every update here is a sequence of saturating writes), so the tree is used as is.
    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Opens a child frame's node.
    #[must_use]
    pub fn open(&self, parent: NodeId, caps: Caps, width: u32) -> NodeId {
        let mut t = self.tree();
        let id = u32::try_from(t.nodes.len()).unwrap_or(u32::MAX);
        t.nodes.push(Node {
            parent: Some(parent.0),
            caps,
            width,
            spent: Amounts::default(),
            reserved: Amounts::default(),
            running: 0,
            open: true,
        });
        NodeId(id)
    }

    /// Closes a frame's node. Reservations still held on it (a drained call) stay held on
    /// its ancestors until they complete; nothing new is admitted on it.
    pub fn close(&self, node: NodeId) {
        if let Some(n) = self.tree().nodes.get_mut(node.0 as usize) {
            n.open = false;
        }
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
            .nodes
            .get(node.0 as usize)
            .map_or_else(Amounts::default, |n| n.spent)
    }

    /// What reservations hold on `node`.
    #[must_use]
    pub fn reserved(&self, node: NodeId) -> Amounts {
        self.tree()
            .nodes
            .get(node.0 as usize)
            .map_or_else(Amounts::default, |n| n.reserved)
    }

    /// `node`'s caps.
    #[must_use]
    pub fn caps(&self, node: NodeId) -> Caps {
        self.tree()
            .nodes
            .get(node.0 as usize)
            .map_or(Caps::NONE, |n| n.caps)
    }

    /// How many reservations the tree holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.tree().held()
    }

    /// A counter bumped at every release: a waiting admission retries once it changes.
    #[must_use]
    pub fn releases(&self) -> u64 {
        self.tree().releases
    }

    fn open_path(t: &Tree, node: NodeId) -> Option<Vec<u32>> {
        let path = t.path(node);
        let first = *path.first()?;
        t.node(first).open.then_some(path)
    }

    /// Admits a tool call: one `tool_calls` on every ledger of the path.
    ///
    /// # Errors
    ///
    /// `tool_calls` when a ledger on the path has none left (or the frame is closed).
    pub fn admit_tool(&self, node: NodeId) -> Result<Reservation, Refused> {
        let mut t = self.tree();
        let refuse = |n: u32| Refused {
            which: BudgetKind::ToolCalls,
            node: NodeId(n),
        };
        let path = Self::open_path(&t, node).ok_or(refuse(node.0))?;
        if let Some(i) = t.short_of_one(&path, BudgetKind::ToolCalls) {
            return Err(refuse(i));
        }
        let held = Amounts {
            tool_calls: 1,
            ..Amounts::default()
        };
        Ok(t.reserve(path, held, None))
    }

    /// Admits a model call by reservation (see the module documentation).
    #[must_use]
    pub fn admit_model(&self, node: NodeId, call: &ModelCall) -> Admission {
        let mut t = self.tree();
        let Some(path) = Self::open_path(&t, node) else {
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
                    node: NodeId(i),
                });
            }
            Prices::default()
        };
        if let Some(i) = t.short_of_one(&path, BudgetKind::ModelCalls) {
            return Admission::Refused(Refused {
                which: BudgetKind::ModelCalls,
                node: NodeId(i),
            });
        }
        let in_cost = cost_of(call.input, prices.input);
        // The output every ledger can cover after the input, and the first ledger that cannot
        // cover the input itself.
        let mut room: Option<u64> = None;
        let mut short: Option<Refused> = None;
        let mut binding: Option<Refused> = None;
        for &i in &path {
            if let Some(avail) = t.avail(i, BudgetKind::Tokens) {
                match avail.checked_sub(call.input) {
                    Some(out) => {
                        if room.is_none_or(|r| out < r) {
                            room = Some(out);
                            binding = Some(Refused {
                                which: BudgetKind::Tokens,
                                node: NodeId(i),
                            });
                        }
                    }
                    None => {
                        short.get_or_insert(Refused {
                            which: BudgetKind::Tokens,
                            node: NodeId(i),
                        });
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
                            binding = Some(Refused {
                                which: BudgetKind::Cost,
                                node: NodeId(i),
                            });
                        }
                    }
                    None => {
                        short.get_or_insert(Refused {
                            which: BudgetKind::Cost,
                            node: NodeId(i),
                        });
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
            if t.held() > 0 {
                return Admission::Wait;
            }
            let refusal = short.or(binding).unwrap_or(Refused {
                which: BudgetKind::Tokens,
                node,
            });
            return Admission::Refused(refusal);
        }
        let out = effective.unwrap_or(0);
        let held = Amounts {
            tokens: call.input.saturating_add(out),
            cost: in_cost.saturating_add(cost_of(out, prices.output)),
            model_calls: 1,
            ..Amounts::default()
        };
        Admission::Admitted(t.reserve(path, held, effective))
    }

    /// Completes an effect: its reservation is replaced by `actual` on every ledger of its
    /// path. `false` when the reservation was already given back.
    #[must_use]
    pub fn complete(&self, r: &Reservation, actual: Amounts) -> bool {
        self.tree().release(r, actual)
    }

    /// Gives a reservation back without a charge (the effect never started). `false` when it
    /// was already given back.
    #[must_use]
    pub fn cancel(&self, r: &Reservation) -> bool {
        self.tree().release(r, Amounts::default())
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
                node: NodeId(i),
            });
        }
        for i in path {
            let x = &mut t.nodes[i as usize];
            x.spent.steps = x.spent.steps.saturating_add(n);
        }
        Ok(())
    }

    /// Records `n` steps on `node` and its ancestors without checking: for a caller that
    /// already kept them within [`Ledgers::remaining`] (the sequential machine).
    pub fn record_steps(&self, node: NodeId, n: u64) {
        let mut t = self.tree();
        for i in t.path(node) {
            let x = &mut t.nodes[i as usize];
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
        best.map(|(_, i)| NodeId(i))
    }

    /// Takes a running slot for a branch of `node`'s frame: `false` when `node` or an
    /// ancestor already runs as many branches as its width (the branch waits; a parked
    /// branch holds no slot).
    #[must_use]
    pub fn acquire_slot(&self, node: NodeId) -> bool {
        let mut t = self.tree();
        let path = t.path(node);
        if path.iter().any(|&i| {
            let n = t.node(i);
            n.running >= n.width
        }) {
            return false;
        }
        for i in path {
            let x = &mut t.nodes[i as usize];
            x.running = x.running.saturating_add(1);
        }
        true
    }

    /// Gives a running slot back (the branch finished or parked).
    pub fn release_slot(&self, node: NodeId) {
        let mut t = self.tree();
        for i in t.path(node) {
            let x = &mut t.nodes[i as usize];
            x.running = x.running.saturating_sub(1);
        }
    }

    /// The branches running in `node`'s frame and below.
    #[must_use]
    pub fn running(&self, node: NodeId) -> u32 {
        self.tree()
            .nodes
            .get(node.0 as usize)
            .map_or(0, |n| n.running)
    }
}
