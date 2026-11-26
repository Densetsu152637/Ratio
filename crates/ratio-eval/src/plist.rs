//! Persistent lists (chapter 06, `data`, "Representations"): O(log n) push, get, set, slice
//! and concatenation.
//!
//! A list is a height-balanced binary tree (an AVL rope) whose leaves hold up to [`CHUNK`]
//! elements. Every node records its length, height and the [`Meta`] of its subtree (label
//! summary, weight, depth), so the summary of any list, and of every slice or concatenation,
//! is maintained in O(log n) as it is built. Updates copy the path to the changed leaf and
//! share everything else. A relaxed radix-balanced tree, which the chapter names, has the same
//! bounds; this one is simpler and its work per operation is within a constant factor of the
//! `log` cost class.

use std::sync::Arc;

use crate::heap::{Charge, Heap, cost};
use crate::value::{Fault, MAX_ELEMS, Meta, Value};

/// The most elements in one leaf.
pub(crate) const CHUNK: usize = 32;

#[derive(Debug)]
pub(crate) struct Node {
    kind: Kind,
    len: u64,
    height: u8,
    meta: Meta,
    _charge: Charge,
}

#[derive(Debug)]
enum Kind {
    Leaf(Box<[Value]>),
    Branch(Arc<Node>, Arc<Node>),
}

/// A persistent list.
#[derive(Clone, Debug, Default)]
pub struct List {
    root: Option<Arc<Node>>,
}

/// A node's meta covers its elements only (weight: the sum of theirs), so a list's meta is
/// the same whatever the shape of its tree; [`List::meta`] adds the list itself.
fn leaf(heap: &Heap, items: Vec<Value>) -> Result<Arc<Node>, Fault> {
    let mut meta = Meta::of(&items);
    meta.weight -= 1;
    let charge = heap.alloc(cost::LIST_NODE + cost::SLOT * items.len() as u64)?;
    Ok(Arc::new(Node {
        len: items.len() as u64,
        height: 1,
        meta: meta.check()?,
        kind: Kind::Leaf(items.into_boxed_slice()),
        _charge: charge,
    }))
}

fn branch(heap: &Heap, l: Arc<Node>, r: Arc<Node>) -> Result<Arc<Node>, Fault> {
    let charge = heap.alloc(cost::LIST_NODE)?;
    Ok(Arc::new(Node {
        len: l.len + r.len,
        height: l.height.max(r.height).saturating_add(1),
        meta: Meta::join(l.meta, r.meta),
        kind: Kind::Branch(l, r),
        _charge: charge,
    }))
}

fn children(n: &Node) -> Option<(&Arc<Node>, &Arc<Node>)> {
    match &n.kind {
        Kind::Branch(l, r) => Some((l, r)),
        Kind::Leaf(_) => None,
    }
}

/// `l` and `r` under one node, rotated so the heights stay within one of each other (given
/// that they differ by at most two).
fn balance(heap: &Heap, l: Arc<Node>, r: Arc<Node>) -> Result<Arc<Node>, Fault> {
    if l.height > r.height.saturating_add(1) {
        let (ll, lr) = children(&l).expect("a node of height > 2 is a branch");
        if ll.height >= lr.height {
            let right = branch(heap, Arc::clone(lr), r)?;
            return branch(heap, Arc::clone(ll), right);
        }
        let (lrl, lrr) = children(lr).expect("taller than its sibling, so a branch");
        let left = branch(heap, Arc::clone(ll), Arc::clone(lrl))?;
        let right = branch(heap, Arc::clone(lrr), r)?;
        return branch(heap, left, right);
    }
    if r.height > l.height.saturating_add(1) {
        let (rl, rr) = children(&r).expect("a node of height > 2 is a branch");
        if rr.height >= rl.height {
            let left = branch(heap, l, Arc::clone(rl))?;
            return branch(heap, left, Arc::clone(rr));
        }
        let (rll, rlr) = children(rl).expect("taller than its sibling, so a branch");
        let left = branch(heap, l, Arc::clone(rll))?;
        let right = branch(heap, Arc::clone(rlr), Arc::clone(rr))?;
        return branch(heap, left, right);
    }
    branch(heap, l, r)
}

/// The concatenation of two trees, balanced, in O(|height difference|).
fn join(heap: &Heap, a: Arc<Node>, b: Arc<Node>) -> Result<Arc<Node>, Fault> {
    if a.height > b.height.saturating_add(1) {
        let (al, ar) = children(&a).expect("taller, so a branch");
        let right = join(heap, Arc::clone(ar), b)?;
        return balance(heap, Arc::clone(al), right);
    }
    if b.height > a.height.saturating_add(1) {
        let (bl, br) = children(&b).expect("taller, so a branch");
        let left = join(heap, a, Arc::clone(bl))?;
        return balance(heap, left, Arc::clone(br));
    }
    if let (Kind::Leaf(x), Kind::Leaf(y)) = (&a.kind, &b.kind)
        && x.len() + y.len() <= CHUNK
    {
        let mut items = Vec::with_capacity(x.len() + y.len());
        items.extend(x.iter().cloned());
        items.extend(y.iter().cloned());
        return leaf(heap, items);
    }
    branch(heap, a, b)
}

fn join_opt(
    heap: &Heap,
    a: Option<Arc<Node>>,
    b: Option<Arc<Node>>,
) -> Result<Option<Arc<Node>>, Fault> {
    Ok(match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(join(heap, a, b)?),
    })
}

type Split = (Option<Arc<Node>>, Option<Arc<Node>>);

/// The first `i` elements and the rest.
fn split(heap: &Heap, n: &Arc<Node>, i: u64) -> Result<Split, Fault> {
    if i == 0 {
        return Ok((None, Some(Arc::clone(n))));
    }
    if i >= n.len {
        return Ok((Some(Arc::clone(n)), None));
    }
    match &n.kind {
        Kind::Leaf(items) => {
            let i = usize::try_from(i).unwrap_or(items.len());
            Ok((
                Some(leaf(heap, items[..i].to_vec())?),
                Some(leaf(heap, items[i..].to_vec())?),
            ))
        }
        Kind::Branch(l, r) => {
            if i <= l.len {
                let (ll, lr) = split(heap, l, i)?;
                Ok((ll, join_opt(heap, lr, Some(Arc::clone(r)))?))
            } else {
                let (rl, rr) = split(heap, r, i - l.len)?;
                Ok((join_opt(heap, Some(Arc::clone(l)), rl)?, rr))
            }
        }
    }
}

/// `n` with `v` appended to its rightmost leaf, when that leaf has room.
fn into_last(heap: &Heap, n: &Arc<Node>, v: &Value) -> Result<Option<Arc<Node>>, Fault> {
    match &n.kind {
        Kind::Leaf(items) if items.len() < CHUNK => {
            let mut new = Vec::with_capacity(items.len() + 1);
            new.extend(items.iter().cloned());
            new.push(v.clone());
            Ok(Some(leaf(heap, new)?))
        }
        Kind::Leaf(_) => Ok(None),
        Kind::Branch(l, r) => match into_last(heap, r, v)? {
            Some(r) => Ok(Some(branch(heap, Arc::clone(l), r)?)),
            None => Ok(None),
        },
    }
}

/// A balanced tree over the leaves, built bottom-up.
fn build(heap: &Heap, mut level: Vec<Arc<Node>>) -> Result<Option<Arc<Node>>, Fault> {
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut it = level.into_iter();
        while let Some(a) = it.next() {
            match it.next() {
                Some(b) => next.push(branch(heap, a, b)?),
                None => next.push(a),
            }
        }
        level = next;
    }
    Ok(level.pop())
}

impl Node {
    /// Moves the node's children onto the drop work-list.
    pub(crate) fn take_children(&mut self, stack: &mut Vec<crate::drop::Garbage>) {
        match std::mem::replace(&mut self.kind, Kind::Leaf(Box::default())) {
            Kind::Leaf(items) => {
                stack.extend(
                    items
                        .into_vec()
                        .into_iter()
                        .map(crate::drop::Garbage::Value),
                );
            }
            Kind::Branch(l, r) => {
                stack.push(crate::drop::Garbage::Node(l));
                stack.push(crate::drop::Garbage::Node(r));
            }
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let nested = match &self.kind {
            Kind::Leaf(items) => items.iter().any(|v| v.meta().is_some()),
            Kind::Branch(..) => true,
        };
        if nested {
            let mut stack = Vec::new();
            self.take_children(&mut stack);
            crate::drop::dispose(stack);
        }
    }
}

impl List {
    /// The tree, for the drop work-list.
    pub(crate) fn into_root(self) -> Option<Arc<Node>> {
        self.root
    }

    #[must_use]
    pub fn empty() -> List {
        List { root: None }
    }

    /// A list of `items`, in O(n).
    ///
    /// # Errors
    ///
    /// Over the heap, element or depth limits.
    pub fn from_vec(heap: &Heap, items: Vec<Value>) -> Result<List, Fault> {
        if items.len() as u64 > MAX_ELEMS {
            return Err(Fault::TooLarge("a list of more than 1,000,000 elements"));
        }
        let mut leaves = Vec::with_capacity(items.len().div_ceil(CHUNK));
        let mut it = items.into_iter().peekable();
        while it.peek().is_some() {
            let chunk: Vec<Value> = it.by_ref().take(CHUNK).collect();
            leaves.push(leaf(heap, chunk)?);
        }
        Ok(List {
            root: build(heap, leaves)?,
        })
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.root.as_ref().map_or(0, |n| n.len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub(crate) fn meta(&self) -> Meta {
        match &self.root {
            Some(n) => Meta {
                weight: n.meta.weight.saturating_add(1),
                ..n.meta
            },
            None => Meta {
                summary: crate::labels::Labels::NONE,
                weight: 1,
                depth: 1,
            },
        }
    }

    /// The height of the tree (for the balance tests).
    #[doc(hidden)]
    #[must_use]
    pub fn height(&self) -> u8 {
        self.root.as_ref().map_or(0, |n| n.height)
    }

    /// Whether every node is balanced and records its subtree correctly (for tests).
    #[doc(hidden)]
    #[must_use]
    pub fn well_formed(&self) -> bool {
        fn check(n: &Node) -> bool {
            match &n.kind {
                Kind::Leaf(items) => {
                    !items.is_empty()
                        && items.len() <= CHUNK
                        && n.len == items.len() as u64
                        && n.height == 1
                }
                Kind::Branch(l, r) => {
                    l.height.abs_diff(r.height) <= 1
                        && n.height == l.height.max(r.height) + 1
                        && n.len == l.len + r.len
                        && n.meta.summary.contains(l.meta.summary | r.meta.summary)
                        && check(l)
                        && check(r)
                }
            }
        }
        self.root.as_deref().is_none_or(check)
    }

    /// Element `i`.
    #[must_use]
    pub fn get(&self, mut i: u64) -> Option<&Value> {
        let mut n = self.root.as_deref()?;
        if i >= n.len {
            return None;
        }
        loop {
            match &n.kind {
                Kind::Leaf(items) => return items.get(usize::try_from(i).ok()?),
                Kind::Branch(l, r) => {
                    if i < l.len {
                        n = l;
                    } else {
                        i -= l.len;
                        n = r;
                    }
                }
            }
        }
    }

    /// The list with `v` appended.
    ///
    /// # Errors
    ///
    /// Over the heap, element or depth limits.
    pub fn push(&self, heap: &Heap, v: Value) -> Result<List, Fault> {
        if self.len() >= MAX_ELEMS {
            return Err(Fault::TooLarge("a list of more than 1,000,000 elements"));
        }
        let Some(root) = &self.root else {
            return Ok(List {
                root: Some(leaf(heap, vec![v])?),
            });
        };
        // Append to the rightmost leaf while it has room; otherwise join a new leaf.
        if let Some(root) = into_last(heap, root, &v)? {
            return Ok(List { root: Some(root) });
        }
        let single = leaf(heap, vec![v])?;
        Ok(List {
            root: Some(join(heap, Arc::clone(root), single)?),
        })
    }

    /// The concatenation of two lists.
    ///
    /// # Errors
    ///
    /// Over the heap or element limits.
    pub fn concat(&self, heap: &Heap, other: &List) -> Result<List, Fault> {
        if self.len().saturating_add(other.len()) > MAX_ELEMS {
            return Err(Fault::TooLarge("a list of more than 1,000,000 elements"));
        }
        Ok(List {
            root: join_opt(heap, self.root.clone(), other.root.clone())?,
        })
    }

    /// Elements `start..end` (clamped to the list).
    ///
    /// # Errors
    ///
    /// Over the heap limit.
    pub fn slice(&self, heap: &Heap, start: u64, end: u64) -> Result<List, Fault> {
        let end = end.min(self.len());
        let start = start.min(end);
        let Some(root) = &self.root else {
            return Ok(List::empty());
        };
        let (head, _) = split(heap, root, end)?;
        let Some(head) = head else {
            return Ok(List::empty());
        };
        let (_, tail) = split(heap, &head, start)?;
        Ok(List { root: tail })
    }

    /// The list with element `i` replaced.
    ///
    /// # Errors
    ///
    /// `i` out of range, or over the heap or depth limits.
    pub fn set(&self, heap: &Heap, i: u64, v: Value) -> Result<List, Fault> {
        fn go(heap: &Heap, n: &Arc<Node>, i: u64, v: Value) -> Result<Arc<Node>, Fault> {
            match &n.kind {
                Kind::Leaf(items) => {
                    let mut new = items.to_vec();
                    let slot = usize::try_from(i)
                        .ok()
                        .and_then(|i| new.get_mut(i))
                        .ok_or_else(|| Fault::Invalid("index out of range".to_owned()))?;
                    *slot = v;
                    leaf(heap, new)
                }
                Kind::Branch(l, r) => {
                    if i < l.len {
                        branch(heap, go(heap, l, i, v)?, Arc::clone(r))
                    } else {
                        branch(heap, Arc::clone(l), go(heap, r, i - l.len, v)?)
                    }
                }
            }
        }
        match &self.root {
            Some(root) if i < root.len => Ok(List {
                root: Some(go(heap, root, i, v)?),
            }),
            _ => Err(Fault::Invalid("index out of range".to_owned())),
        }
    }

    /// Calls `f` on every element in order.
    pub(crate) fn for_each(&self, f: &mut dyn FnMut(&Value)) {
        let mut stack: Vec<&Node> = Vec::new();
        if let Some(r) = &self.root {
            stack.push(r);
        }
        while let Some(n) = stack.pop() {
            match &n.kind {
                Kind::Leaf(items) => items.iter().for_each(&mut *f),
                Kind::Branch(l, r) => {
                    stack.push(r);
                    stack.push(l);
                }
            }
        }
    }

    /// The elements, in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<Value> {
        let mut out = Vec::with_capacity(usize::try_from(self.len()).unwrap_or(0));
        self.for_each(&mut |v| out.push(v.clone()));
        out
    }

    /// An owned cursor over the elements (for loops, which hold it across steps).
    pub(crate) fn cursor(&self) -> Cursor {
        Cursor {
            stack: self.root.iter().cloned().collect(),
            leaf: None,
        }
    }
}

/// An owned in-order cursor over a list.
#[derive(Debug)]
pub(crate) struct Cursor {
    stack: Vec<Arc<Node>>,
    leaf: Option<(Arc<Node>, usize)>,
}

impl Cursor {
    pub(crate) fn next(&mut self) -> Option<Value> {
        loop {
            if let Some((n, i)) = &mut self.leaf {
                if let Kind::Leaf(items) = &n.kind
                    && let Some(v) = items.get(*i)
                {
                    *i += 1;
                    return Some(v.clone());
                }
                self.leaf = None;
            }
            let n = self.stack.pop()?;
            match &n.kind {
                Kind::Leaf(_) => self.leaf = Some((n, 0)),
                Kind::Branch(l, r) => {
                    self.stack.push(Arc::clone(r));
                    self.stack.push(Arc::clone(l));
                }
            }
        }
    }
}
