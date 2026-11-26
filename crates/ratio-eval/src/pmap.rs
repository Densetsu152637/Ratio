//! Persistent ordered maps (chapter 06, `data`, "Representations"): O(log n) get, set and
//! remove, and in-order iteration.
//!
//! A map is an AVL tree of entries, ordered by key (text by code point, integers, `false`
//! before `true`, unit variants by declaration order). Updates copy the path to the changed
//! entry. Every node records the [`Meta`] of its subtree, so the label summary is maintained
//! as the map is built; a node's meta covers its entries only, so a map's meta does not depend
//! on the shape of its tree.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::heap::{Charge, Heap, cost};
use crate::labels::Labels;
use crate::value::{Fault, MAX_ELEMS, Meta, Repr, Value};

#[derive(Debug)]
pub(crate) struct MNode {
    key: Value,
    value: Value,
    left: Option<Arc<MNode>>,
    right: Option<Arc<MNode>>,
    height: u8,
    len: u64,
    meta: Meta,
    _charge: Charge,
}

/// A persistent ordered map.
#[derive(Clone, Debug, Default)]
pub struct Map {
    root: Option<Arc<MNode>>,
}

fn rank(v: &Value) -> u8 {
    match v.repr {
        Repr::Bool(_) => 0,
        Repr::Int(_) => 1,
        Repr::Str(_) => 2,
        Repr::Variant(..) => 3,
        _ => 4,
    }
}

/// The order of map keys.
pub(crate) fn key_cmp(a: &Value, b: &Value) -> Ordering {
    match (&a.repr, &b.repr) {
        (Repr::Str(x), Repr::Str(y)) => x.as_str().cmp(y.as_str()),
        (Repr::Int(x), Repr::Int(y)) => x.cmp(y),
        (Repr::Bool(x), Repr::Bool(y)) => x.cmp(y),
        (Repr::Variant(e1, i1, _), Repr::Variant(e2, i2, _)) => (e1, i1).cmp(&(e2, i2)),
        // Keys the checker accepts beyond the four kinds (variants with payloads, tuples):
        // the natural order, which is total on them.
        _ if rank(a) == rank(b) => crate::ops::cmp_natural(a, b),
        _ => rank(a).cmp(&rank(b)),
    }
}

fn height(n: Option<&Arc<MNode>>) -> u8 {
    n.map_or(0, |n| n.height)
}

fn mk(
    heap: &Heap,
    key: Value,
    value: Value,
    left: Option<Arc<MNode>>,
    right: Option<Arc<MNode>>,
) -> Result<Arc<MNode>, Fault> {
    let mut meta = Meta {
        summary: Labels::NONE,
        weight: 0,
        depth: 1,
    };
    meta.add(&key);
    meta.add(&value);
    for side in [&left, &right].into_iter().flatten() {
        meta = Meta::join(meta, side.meta);
    }
    let charge = heap.alloc(cost::MAP_NODE)?;
    Ok(Arc::new(MNode {
        height: height(left.as_ref())
            .max(height(right.as_ref()))
            .saturating_add(1),
        len: 1 + left.as_ref().map_or(0, |n| n.len) + right.as_ref().map_or(0, |n| n.len),
        meta: meta.check()?,
        key,
        value,
        left,
        right,
        _charge: charge,
    }))
}

/// A node for `key` over `left` and `right`, rotated so that the heights stay balanced (given
/// that they differ by at most two).
fn bal(
    heap: &Heap,
    key: Value,
    value: Value,
    left: Option<Arc<MNode>>,
    right: Option<Arc<MNode>>,
) -> Result<Arc<MNode>, Fault> {
    let (hl, hr) = (height(left.as_ref()), height(right.as_ref()));
    if hl > hr.saturating_add(1) {
        let l = left.expect("taller, so present");
        if height(l.left.as_ref()) >= height(l.right.as_ref()) {
            let new_right = mk(heap, key, value, l.right.clone(), right)?;
            return mk(
                heap,
                l.key.clone(),
                l.value.clone(),
                l.left.clone(),
                Some(new_right),
            );
        }
        let lr = l
            .right
            .clone()
            .expect("taller than its sibling, so present");
        let new_left = mk(
            heap,
            l.key.clone(),
            l.value.clone(),
            l.left.clone(),
            lr.left.clone(),
        )?;
        let new_right = mk(heap, key, value, lr.right.clone(), right)?;
        return mk(
            heap,
            lr.key.clone(),
            lr.value.clone(),
            Some(new_left),
            Some(new_right),
        );
    }
    if hr > hl.saturating_add(1) {
        let r = right.expect("taller, so present");
        if height(r.right.as_ref()) >= height(r.left.as_ref()) {
            let new_left = mk(heap, key, value, left, r.left.clone())?;
            return mk(
                heap,
                r.key.clone(),
                r.value.clone(),
                Some(new_left),
                r.right.clone(),
            );
        }
        let rl = r.left.clone().expect("taller than its sibling, so present");
        let new_left = mk(heap, key, value, left, rl.left.clone())?;
        let new_right = mk(
            heap,
            r.key.clone(),
            r.value.clone(),
            rl.right.clone(),
            r.right.clone(),
        )?;
        return mk(
            heap,
            rl.key.clone(),
            rl.value.clone(),
            Some(new_left),
            Some(new_right),
        );
    }
    mk(heap, key, value, left, right)
}

fn insert(
    heap: &Heap,
    n: Option<&Arc<MNode>>,
    key: Value,
    value: Value,
) -> Result<Arc<MNode>, Fault> {
    let Some(n) = n else {
        return mk(heap, key, value, None, None);
    };
    match key_cmp(&key, &n.key) {
        Ordering::Less => {
            let l = insert(heap, n.left.as_ref(), key, value)?;
            bal(
                heap,
                n.key.clone(),
                n.value.clone(),
                Some(l),
                n.right.clone(),
            )
        }
        Ordering::Greater => {
            let r = insert(heap, n.right.as_ref(), key, value)?;
            bal(
                heap,
                n.key.clone(),
                n.value.clone(),
                n.left.clone(),
                Some(r),
            )
        }
        // The key keeps the labels of both writes.
        Ordering::Equal => {
            let key = key.with_labels(n.key.labels);
            mk(heap, key, value, n.left.clone(), n.right.clone())
        }
    }
}

type Entry = (Value, Value);

fn remove_min(heap: &Heap, n: &Arc<MNode>) -> Result<(Entry, Option<Arc<MNode>>), Fault> {
    match &n.left {
        None => Ok(((n.key.clone(), n.value.clone()), n.right.clone())),
        Some(l) => {
            let (min, rest) = remove_min(heap, l)?;
            Ok((
                min,
                Some(bal(
                    heap,
                    n.key.clone(),
                    n.value.clone(),
                    rest,
                    n.right.clone(),
                )?),
            ))
        }
    }
}

/// `None` when the key is absent; else the new subtree (itself `None` when empty).
#[allow(
    clippy::option_option,
    reason = "absent key, or a subtree that may be empty"
)]
fn remove(
    heap: &Heap,
    n: Option<&Arc<MNode>>,
    key: &Value,
) -> Result<Option<Option<Arc<MNode>>>, Fault> {
    let Some(n) = n else {
        return Ok(None);
    };
    match key_cmp(key, &n.key) {
        Ordering::Less => match remove(heap, n.left.as_ref(), key)? {
            None => Ok(None),
            Some(l) => Ok(Some(Some(bal(
                heap,
                n.key.clone(),
                n.value.clone(),
                l,
                n.right.clone(),
            )?))),
        },
        Ordering::Greater => match remove(heap, n.right.as_ref(), key)? {
            None => Ok(None),
            Some(r) => Ok(Some(Some(bal(
                heap,
                n.key.clone(),
                n.value.clone(),
                n.left.clone(),
                r,
            )?))),
        },
        Ordering::Equal => match (&n.left, &n.right) {
            (None, r) => Ok(Some(r.clone())),
            (l, None) => Ok(Some(l.clone())),
            (Some(_), Some(r)) => {
                let ((k, v), rest) = remove_min(heap, r)?;
                Ok(Some(Some(bal(heap, k, v, n.left.clone(), rest)?)))
            }
        },
    }
}

impl MNode {
    /// Moves the node's entry and subtrees onto the drop work-list.
    pub(crate) fn take_children(&mut self, stack: &mut Vec<crate::drop::Garbage>) {
        stack.push(crate::drop::Garbage::Value(std::mem::take(&mut self.key)));
        stack.push(crate::drop::Garbage::Value(std::mem::take(&mut self.value)));
        for side in [self.left.take(), self.right.take()].into_iter().flatten() {
            stack.push(crate::drop::Garbage::MNode(side));
        }
    }
}

impl Drop for MNode {
    fn drop(&mut self) {
        if self.left.is_some()
            || self.right.is_some()
            || self.value.meta().is_some()
            || self.key.meta().is_some()
        {
            let mut stack = Vec::new();
            self.take_children(&mut stack);
            crate::drop::dispose(stack);
        }
    }
}

impl Map {
    /// The tree, for the drop work-list.
    pub(crate) fn into_root(self) -> Option<Arc<MNode>> {
        self.root
    }

    #[must_use]
    pub fn empty() -> Map {
        Map { root: None }
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
                summary: Labels::NONE,
                weight: 1,
                depth: 1,
            },
        }
    }

    /// The entry for `key`: the stored key (with its labels) and value.
    #[must_use]
    pub fn get_entry(&self, key: &Value) -> Option<(&Value, &Value)> {
        let mut n = self.root.as_deref();
        while let Some(node) = n {
            match key_cmp(key, &node.key) {
                Ordering::Less => n = node.left.as_deref(),
                Ordering::Greater => n = node.right.as_deref(),
                Ordering::Equal => return Some((&node.key, &node.value)),
            }
        }
        None
    }

    /// The map with `key` set to `value`.
    ///
    /// # Errors
    ///
    /// Over the heap, element or depth limits.
    pub fn insert(&self, heap: &Heap, key: Value, value: Value) -> Result<Map, Fault> {
        if self.len() >= MAX_ELEMS && self.get_entry(&key).is_none() {
            return Err(Fault::TooLarge("a map of more than 1,000,000 entries"));
        }
        Ok(Map {
            root: Some(insert(heap, self.root.as_ref(), key, value)?),
        })
    }

    /// The map without `key` (the same map when absent).
    ///
    /// # Errors
    ///
    /// Over the heap limit.
    pub fn remove(&self, heap: &Heap, key: &Value) -> Result<Map, Fault> {
        Ok(match remove(heap, self.root.as_ref(), key)? {
            Some(root) => Map { root },
            None => self.clone(),
        })
    }

    /// Calls `f` on every entry in key order.
    pub(crate) fn for_each(&self, f: &mut dyn FnMut(&Value, &Value)) {
        let mut stack: Vec<&MNode> = Vec::new();
        let mut n = self.root.as_deref();
        loop {
            while let Some(node) = n {
                stack.push(node);
                n = node.left.as_deref();
            }
            let Some(node) = stack.pop() else {
                return;
            };
            f(&node.key, &node.value);
            n = node.right.as_deref();
        }
    }

    /// Whether the tree is balanced, ordered and records its subtrees correctly (for tests).
    #[doc(hidden)]
    #[must_use]
    pub fn well_formed(&self) -> bool {
        fn check(n: &MNode) -> bool {
            let (hl, hr) = (height(n.left.as_ref()), height(n.right.as_ref()));
            let ordered = n
                .left
                .as_ref()
                .is_none_or(|l| key_cmp(&l.key, &n.key) == Ordering::Less)
                && n.right
                    .as_ref()
                    .is_none_or(|r| key_cmp(&r.key, &n.key) == Ordering::Greater);
            let mut inner = n.key.quick() | n.value.quick();
            for side in [&n.left, &n.right].into_iter().flatten() {
                inner |= side.meta.summary;
            }
            hl.abs_diff(hr) <= 1
                && n.height == hl.max(hr) + 1
                && ordered
                && n.meta.summary.contains(inner)
                && n.left.as_deref().is_none_or(check)
                && n.right.as_deref().is_none_or(check)
        }
        self.root.as_deref().is_none_or(check)
    }

    /// An owned cursor over the entries in key order.
    pub(crate) fn cursor(&self) -> MapCursor {
        let mut c = MapCursor { stack: Vec::new() };
        c.descend(self.root.clone());
        c
    }
}

/// An owned in-order cursor over a map.
#[derive(Debug)]
pub(crate) struct MapCursor {
    stack: Vec<Arc<MNode>>,
}

impl MapCursor {
    fn descend(&mut self, mut n: Option<Arc<MNode>>) {
        while let Some(node) = n.take() {
            n.clone_from(&node.left);
            self.stack.push(node);
        }
    }

    pub(crate) fn next(&mut self) -> Option<(Value, Value)> {
        let node = self.stack.pop()?;
        self.descend(node.right.clone());
        Some((node.key.clone(), node.value.clone()))
    }
}
