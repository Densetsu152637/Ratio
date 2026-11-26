//! Constant evaluation rules (chapter 02, "Constants"; chapter 04, the configuration record).
//!
//! A `const` initialiser, a `state` initialiser and a harness's declared configuration may call
//! only pure functions and pure built-ins. A `fn` is pure unless it calls or names an effectful
//! built-in, a harness or a function that is not pure; there is no effect annotation, so purity
//! is computed over the call graph. Recursion is allowed: a cycle of functions is pure when
//! nothing on it makes an effect.
//!
//! Effects are counted where a function value is **made or named**, not where it is called: a
//! closure's body is checked in place, so its calls count for the body that writes it, and
//! naming a declaration counts as using it. Calling a function value (a parameter, a local)
//! then adds nothing, because whatever it can do was already counted where it came from. This
//! is conservative: a constant that only names an effectful function, without calling it, is
//! refused too.
//!
//! While checking, each body keeps an [`EffectLog`]: the effectful calls it makes directly, the
//! functions it calls or refers to, and the constants it reads. Afterwards purity spreads
//! backwards along the call graph from the functions with direct effects (a worklist, linear in
//! the graph), and constants that reach themselves through reads and calls are reported as
//! cycles (Tarjan's algorithm, iterative).

use std::collections::{HashMap, HashSet};

use ratio_syntax::Span;
use ratio_syntax::ast::ItemKind;

use crate::check::Checker;
use crate::diag::Code;
use crate::tables::{ItemRef, Origin};

/// What one body does that matters for purity and constant cycles.
#[derive(Clone, Debug, Default)]
pub(crate) struct EffectLog {
    /// Calls that make effects, with a description.
    pub(crate) direct: Vec<(Span, String)>,
    /// User functions and prompts called or referred to.
    pub(crate) calls: Vec<(ItemRef, Span)>,
    /// Constants read.
    pub(crate) consts: Vec<(ItemRef, Span)>,
}

impl Checker<'_> {
    /// Reports effects in constant expressions and constant cycles.
    pub(crate) fn effects(&mut self) {
        let impure = self.impure_functions();
        let constants: Vec<(ItemRef, bool)> = self
            .logs
            .keys()
            .map(|r| (*r, true))
            .chain(self.config_logs.keys().map(|r| (*r, false)))
            .collect();
        let mut constants: Vec<(ItemRef, bool)> = constants
            .into_iter()
            .filter(|(r, own)| {
                !*own || matches!(self.item(*r).kind, ItemKind::Const(_) | ItemKind::State(_))
            })
            .collect();
        constants.sort();
        for (r, own) in constants {
            let log = if own {
                self.logs.get(&r)
            } else {
                self.config_logs.get(&r)
            };
            let Some(log) = log.cloned() else {
                continue;
            };
            for (span, what) in log.direct {
                self.report(
                    r.origin,
                    Code::EffectInConstant,
                    span,
                    format!("{what}, which a constant expression may not do"),
                );
            }
            for (callee, span) in log.calls {
                if impure.contains(&callee) {
                    let name = crate::check::item_name(&self.item(callee).kind)
                        .map_or_else(String::new, |i| i.name.clone());
                    self.report(
                        r.origin,
                        Code::EffectInConstant,
                        span,
                        format!("`{name}` makes effects, so a constant expression may not use it"),
                    );
                }
            }
        }
        self.constant_cycles();
    }

    /// The user functions and prompts that are not pure.
    fn impure_functions(&self) -> HashSet<ItemRef> {
        let mut callers: HashMap<ItemRef, Vec<ItemRef>> = HashMap::new();
        let mut impure: HashSet<ItemRef> = HashSet::new();
        let mut work = Vec::new();
        for (r, log) in &self.logs {
            if !matches!(self.item(*r).kind, ItemKind::Fn(_) | ItemKind::Prompt(_)) {
                continue;
            }
            for (callee, _) in &log.calls {
                callers.entry(*callee).or_default().push(*r);
            }
            if !log.direct.is_empty() && impure.insert(*r) {
                work.push(*r);
            }
        }
        while let Some(r) = work.pop() {
            for caller in callers.get(&r).into_iter().flatten() {
                if impure.insert(*caller) {
                    work.push(*caller);
                }
            }
        }
        impure
    }

    /// Reports every constant on a cycle of reads and calls (Tarjan's strongly connected
    /// components, with an explicit stack).
    fn constant_cycles(&mut self) {
        let mut nodes: Vec<ItemRef> = self.logs.keys().copied().collect();
        nodes.sort();
        let index_of: HashMap<ItemRef, usize> =
            nodes.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let edges: Vec<Vec<usize>> = nodes
            .iter()
            .map(|r| {
                let log = &self.logs[r];
                log.calls
                    .iter()
                    .chain(log.consts.iter())
                    .filter_map(|(to, _)| index_of.get(to).copied())
                    .collect()
            })
            .collect();
        let n = nodes.len();
        let (mut index, mut low) = (vec![usize::MAX; n], vec![0usize; n]);
        let (mut on_stack, mut stack) = (vec![false; n], Vec::new());
        let mut next = 0usize;
        let mut cyclic: Vec<usize> = Vec::new();
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
                if low[v] == index[v] {
                    let mut component = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    let looped = component.len() > 1 || edges[v].contains(&v);
                    if looped {
                        cyclic.extend(component);
                    }
                }
            }
        }
        cyclic.sort_unstable();
        for i in cyclic {
            let r = nodes[i];
            if let ItemKind::Const(decl) = &self.item(r).kind
                && let Origin::Module(_) = r.origin
            {
                self.report(
                    r.origin,
                    Code::ConstantCycle,
                    decl.name.span,
                    format!("the constant `{}` depends on itself", decl.name.name),
                );
            }
        }
    }
}
