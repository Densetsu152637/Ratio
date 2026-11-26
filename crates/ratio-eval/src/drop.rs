//! Iterative dropping: freeing a value never recurses.
//!
//! A value nests at most [`crate::MAX_DEPTH`] deep, but each list or map level adds its tree's
//! height, and a naive drop recurses through all of it, which overflows a small stack. Every
//! container payload therefore drops through this work-list: its `Drop` moves its children out
//! into a stack and frees them one by one, taking the children of each payload it owns alone
//! (`Arc::into_inner`) before letting it go, so every nested drop finds its children already
//! gone and the Rust stack stays constant whatever the shape.

use std::sync::Arc;

use crate::plist::Node;
use crate::pmap::MNode;
use crate::value::{Json, Repr, Value};

pub(crate) enum Garbage {
    Value(Value),
    Node(Arc<Node>),
    MNode(Arc<MNode>),
}

/// Frees everything in `stack` without recursing.
pub(crate) fn dispose(mut stack: Vec<Garbage>) {
    while let Some(g) = stack.pop() {
        match g {
            Garbage::Value(v) => take_value(v, &mut stack),
            Garbage::Node(n) => {
                if let Some(mut node) = Arc::into_inner(n) {
                    node.take_children(&mut stack);
                }
            }
            Garbage::MNode(n) => {
                if let Some(mut node) = Arc::into_inner(n) {
                    node.take_children(&mut stack);
                }
            }
        }
    }
}

/// Moves the children of `v`'s payload onto `stack` when `v` holds the last reference.
fn take_value(v: Value, stack: &mut Vec<Garbage>) {
    match v.repr {
        Repr::Tuple(a) => {
            if let Some(mut a) = Arc::into_inner(a) {
                take_values(&mut a.items, stack);
            }
        }
        Repr::Record(r) => {
            if let Some(mut r) = Arc::into_inner(r) {
                take_values(&mut r.items, stack);
            }
        }
        Repr::List(l) => {
            if let Some(root) = l.into_root() {
                stack.push(Garbage::Node(root));
            }
        }
        Repr::Map(m) => {
            if let Some(root) = m.into_root() {
                stack.push(Garbage::MNode(root));
            }
        }
        Repr::VariantP(vd) => {
            if let Some(mut vd) = Arc::into_inner(vd) {
                stack.push(Garbage::Value(std::mem::take(&mut vd.payload)));
            }
        }
        Repr::Closure(c) => {
            if let Some(mut c) = Arc::into_inner(c) {
                let captured = std::mem::take(&mut c.captured);
                stack.extend(
                    captured
                        .into_vec()
                        .into_iter()
                        .map(|(_, v)| Garbage::Value(v)),
                );
            }
        }
        Repr::Json(j) => {
            if let Some(mut j) = Arc::into_inner(j) {
                take_json(&mut j.node, stack);
            }
        }
        Repr::Prompt(p) => {
            if let Some(mut p) = Arc::into_inner(p) {
                for s in &mut p.sections {
                    take_values(&mut s.values, stack);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn take_values(items: &mut Box<[Value]>, stack: &mut Vec<Garbage>) {
    let items = std::mem::take(items);
    stack.extend(items.into_vec().into_iter().map(Garbage::Value));
}

pub(crate) fn take_json(node: &mut Json, stack: &mut Vec<Garbage>) {
    match std::mem::replace(node, Json::Null) {
        Json::Array(l) => {
            if let Some(root) = l.into_root() {
                stack.push(Garbage::Node(root));
            }
        }
        Json::Object(entries) => {
            for (k, v) in entries.into_vec() {
                stack.push(Garbage::Value(k));
                stack.push(Garbage::Value(v));
            }
        }
        _ => {}
    }
}

/// The `Drop` of a payload whose children are values.
pub(crate) fn drop_values(items: &mut Box<[Value]>) {
    if items.iter().any(|v| v.meta().is_some()) {
        let mut stack = Vec::new();
        take_values(items, &mut stack);
        dispose(stack);
    }
}
