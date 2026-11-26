//! Subtyping and common supertypes (chapter 03, "Subtyping").
//!
//! The relation is the small one chapter 03 lists: `T <: T?`, `Never <: T`, `T <: Untrusted<T>`,
//! `UserText <: Str <: Brief <: Untrusted<Str>`, `TrustedPrompt <: Prompt`, a harness is a
//! function, lists, options, tuples, records, map values, tasks and enum arguments are
//! covariant, and a function type is contravariant in its parameters and covariant in its
//! result. A record value has exactly its declared fields (no width subtyping); a required field
//! may stand where an optional one is declared.
//!
//! Inference variables of a generic call are bound here, locally: `A <: ?T` binds `?T` to `A`,
//! or widens it to a common supertype when it is already bound; `?T <: B` binds `?T` to `B`
//! when it is still unbound. This is bidirectional local inference, not unification: each call
//! is solved on its own, so the work is linear in the types involved. Results between types
//! without variables are memoised, and every step spends the table's fuel, so no input makes
//! the relation exponential.

use std::collections::HashMap;

use crate::types::{Field, Ty, TypeId, Types, VarId};

/// The position of a text type in the chain `UserText <: Str <: Brief <: Untrusted<Str>`.
fn text_rank(ty: &Ty) -> Option<u8> {
    match ty {
        Ty::UserText => Some(0),
        Ty::Str => Some(1),
        Ty::Brief => Some(2),
        Ty::UntrustedStr => Some(3),
        _ => None,
    }
}

/// Memoised answers for pairs of types without inference variables.
#[derive(Clone, Debug, Default)]
pub(crate) struct Memo {
    sub: HashMap<(TypeId, TypeId), bool>,
}

impl Types {
    /// Whether `a` is a subtype of `b`, binding inference variables as it goes.
    pub(crate) fn subtype(&mut self, memo: &mut Memo, a: TypeId, b: TypeId) -> bool {
        if a == b || a == TypeId::ERROR || b == TypeId::ERROR || a == TypeId::NEVER {
            return true;
        }
        if !self.burn() {
            return true;
        }
        let plain = !self.has_vars(a) && !self.has_vars(b);
        if plain && let Some(&known) = memo.sub.get(&(a, b)) {
            return known;
        }
        let answer = self.subtype_step(memo, a, b);
        if plain {
            memo.sub.insert((a, b), answer);
        }
        answer
    }

    fn subtype_step(&mut self, memo: &mut Memo, a: TypeId, b: TypeId) -> bool {
        let (ta, tb) = (self.get(a).clone(), self.get(b).clone());
        if let Ty::Var(v) = ta {
            if let Some(bound) = self.vars[v.0 as usize].binding {
                return self.subtype(memo, bound, b);
            }
            self.bind_var(v, b);
            return true;
        }
        if let Ty::Var(v) = tb {
            return match self.vars[v.0 as usize].binding {
                None => {
                    self.bind_var(v, a);
                    true
                }
                Some(bound) => {
                    if self.subtype(memo, a, bound) {
                        return true;
                    }
                    match self.join(memo, a, bound) {
                        Some(wider) => {
                            self.bind_var(v, wider);
                            true
                        }
                        None => false,
                    }
                }
            };
        }
        match (&ta, &tb) {
            (Ty::Option(x), Ty::Option(y))
            | (Ty::Untrusted(x), Ty::Untrusted(y))
            | (Ty::List(x), Ty::List(y))
            | (Ty::Task(x), Ty::Task(y))
            | (Ty::Secret(x), Ty::Secret(y)) => self.subtype(memo, *x, *y),
            (_, Ty::Option(y) | Ty::Untrusted(y)) => self.subtype(memo, a, *y),
            (Ty::TrustedPrompt, Ty::Prompt) => true,
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                self.subtype(memo, *k1, *k2)
                    && self.subtype(memo, *k2, *k1)
                    && self.subtype(memo, *v1, *v2)
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| self.subtype(memo, *x, *y))
            }
            (Ty::Record(xs), Ty::Record(ys)) => {
                xs.len() == ys.len()
                    && xs.iter().zip(ys.iter()).all(|(x, y)| {
                        x.name == y.name
                            && (!x.optional || y.optional)
                            && self.subtype(memo, x.ty, y.ty)
                    })
            }
            (Ty::Enum(e1, xs), Ty::Enum(e2, ys)) => {
                e1 == e2
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| self.subtype(memo, *x, *y))
            }
            (Ty::Fn(p1, r1) | Ty::Harness(p1, r1), Ty::Fn(p2, r2))
            | (Ty::Harness(p1, r1), Ty::Harness(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| self.subtype(memo, *y, *x))
                    && self.subtype(memo, *r1, *r2)
            }
            _ => match (text_rank(&ta), text_rank(&tb)) {
                (Some(x), Some(y)) => x <= y,
                _ => false,
            },
        }
    }

    /// The least common supertype of `a` and `b`, if they have one.
    pub(crate) fn join(&mut self, memo: &mut Memo, a: TypeId, b: TypeId) -> Option<TypeId> {
        if self.subtype_quiet(memo, a, b) {
            return Some(b);
        }
        if self.subtype_quiet(memo, b, a) {
            return Some(a);
        }
        if !self.burn() {
            return Some(TypeId::ERROR);
        }
        let (ta, tb) = (self.get(a).clone(), self.get(b).clone());
        match (&ta, &tb) {
            (Ty::Option(x), _) => {
                let inner = self.join(memo, *x, b)?;
                Some(self.option(inner))
            }
            (_, Ty::Option(y)) => {
                let inner = self.join(memo, a, *y)?;
                Some(self.option(inner))
            }
            (Ty::List(x), Ty::List(y)) => {
                let inner = self.join(memo, *x, *y)?;
                Some(self.list(inner))
            }
            (Ty::Task(x), Ty::Task(y)) => {
                let inner = self.join(memo, *x, *y)?;
                Some(self.task(inner))
            }
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) if k1 == k2 => {
                let v = self.join(memo, *v1, *v2)?;
                Some(self.map(*k1, v))
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                let mut ts = Vec::with_capacity(xs.len());
                for (x, y) in xs.iter().zip(ys.iter()) {
                    ts.push(self.join(memo, *x, *y)?);
                }
                Some(self.tuple(ts))
            }
            (Ty::Record(xs), Ty::Record(ys)) if xs.len() == ys.len() => {
                let mut fields = Vec::with_capacity(xs.len());
                for (x, y) in xs.iter().zip(ys.iter()) {
                    if x.name != y.name {
                        return None;
                    }
                    fields.push(Field {
                        name: x.name.clone(),
                        ty: self.join(memo, x.ty, y.ty)?,
                        optional: x.optional || y.optional,
                    });
                }
                Some(self.record(fields))
            }
            (Ty::Enum(e1, xs), Ty::Enum(e2, ys)) if e1 == e2 => {
                let mut args = Vec::with_capacity(xs.len());
                for (x, y) in xs.iter().zip(ys.iter()) {
                    args.push(self.join(memo, *x, *y)?);
                }
                Some(self.enum_type(*e1, args))
            }
            (Ty::Untrusted(x), _) => {
                let inner = self.join(memo, *x, b)?;
                Some(self.untrusted(inner))
            }
            (_, Ty::Untrusted(y)) => {
                let inner = self.join(memo, a, *y)?;
                Some(self.untrusted(inner))
            }
            _ => None,
        }
    }

    /// [`Types::subtype`] that leaves inference variables as they were when it fails.
    pub(crate) fn subtype_quiet(&mut self, memo: &mut Memo, a: TypeId, b: TypeId) -> bool {
        if !self.has_vars(a) && !self.has_vars(b) {
            return self.subtype(memo, a, b);
        }
        let mark = self.trail.len();
        self.quiet += 1;
        let ok = self.subtype(memo, a, b);
        self.quiet -= 1;
        if !ok {
            self.undo(mark);
        } else if self.quiet == 0 {
            self.trail.clear();
        }
        ok
    }

    /// Binds `v` to `t`, remembering the old binding while a quiet relation may undo it.
    pub(crate) fn bind_var(&mut self, v: VarId, t: TypeId) {
        let state = &mut self.vars[v.0 as usize];
        if self.quiet > 0 {
            self.trail.push((v, state.binding));
        }
        state.binding = Some(t);
    }

    /// Undoes the bindings made since the trail had `mark` entries.
    pub(crate) fn undo(&mut self, mark: usize) {
        while self.trail.len() > mark {
            if let Some((v, old)) = self.trail.pop() {
                self.vars[v.0 as usize].binding = old;
            }
        }
    }
}
