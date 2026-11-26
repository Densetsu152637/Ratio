//! Exhaustiveness (chapter 03, "Control flow": `match` must be exhaustive; guards do not
//! count).
//!
//! The algorithm is Maranget's usefulness check ("Warnings for pattern matching", 2007) with
//! witnesses: a match is exhaustive when a wildcard row is not useful against its arms. Each
//! column is split by the constructors of its type: `true`/`false`; `none` and a present value
//! (a pattern of the inner type matches a present option value); an enum's variants; the single
//! constructor of a tuple, record or `()`; list lengths `0..=L` and "longer than `L`", where `L`
//! is the longest fixed length or prefix plus suffix in the column; and, for the infinite types
//! (numbers, text), only the literals that appear, so those need a wildcard. `Never` has no
//! constructor, so a match on it needs no arm.
//!
//! The check is exponential in the worst case (or-patterns multiply rows, and so do nested
//! constructors), so it is **bounded**: every row it builds costs fuel from a budget of
//! [`MATCH_FUEL`] units per match, its recursion is at most a fixed depth, and a match over the
//! bound is refused (`C0023`) rather than accepted unchecked.

use std::collections::HashMap;

use crate::types::{Ty, TypeId, Types};

/// The work budget of one exhaustiveness check, in rows (and row cells) built.
pub(crate) const MATCH_FUEL: usize = 200_000;

/// A literal in a pattern.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LitKey {
    Int(i64),
    Float(u64),
    Duration(i64),
    Money(i64),
    Str(String),
}

/// A constructor of a value, as patterns test for it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ctor {
    Bool(bool),
    None,
    /// A present option value (one field: the value).
    Some,
    /// An enum variant by index (one field, the payload record, when it has one).
    Variant(usize),
    /// The one constructor of a tuple, record or `()`.
    Single,
    Lit(LitKey),
    /// A list of exactly this length.
    Exact(usize),
    /// A list longer than the column's longest fixed length: `prefix` first and `suffix` last
    /// elements.
    Long {
        prefix: usize,
        suffix: usize,
    },
}

/// A pattern reduced to what exhaustiveness needs.
#[derive(Clone, Debug)]
pub(crate) enum DPat {
    Wild,
    Ctor(Ctor, Vec<DPat>),
    /// A list pattern with a rest: the patterns before and after `..`.
    Slice(Vec<DPat>, Vec<DPat>),
    Or(Vec<DPat>),
}

/// The answer of a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Exhaustive,
    /// Not exhaustive: a value no arm matches, written as a pattern.
    Missing(String),
    TooComplex,
}

struct TooComplex;

type Step<T> = Result<T, TooComplex>;

/// The constructors of a column's type.
enum Space {
    /// A finite list of constructors with their fields' types.
    Finite(Vec<(Ctor, Vec<TypeId>)>),
    /// A list type: constructors come from the patterns.
    List(TypeId),
    /// Infinitely many constructors (numbers, text), or a type without constructors to split
    /// on (an error): only a wildcard covers it.
    Open,
}

pub(crate) struct Exhaust<'t> {
    types: &'t mut Types,
    fuel: usize,
    depth: usize,
    max_depth: usize,
}

impl<'t> Exhaust<'t> {
    pub(crate) fn new(types: &'t mut Types, max_depth: usize) -> Self {
        Exhaust {
            types,
            fuel: MATCH_FUEL,
            depth: 0,
            max_depth,
        }
    }

    /// Whether the arm patterns `arms` (matching values of type `ty`) cover every value.
    pub(crate) fn check(&mut self, arms: Vec<DPat>, ty: TypeId) -> Outcome {
        let rows = arms.into_iter().map(|p| vec![p]).collect();
        match self.useful(rows, &[ty]) {
            Ok(None) => Outcome::Exhaustive,
            Ok(Some(mut witness)) => Outcome::Missing(witness.pop().unwrap_or_default()),
            Err(TooComplex) => Outcome::TooComplex,
        }
    }

    /// Spends from this match's budget and from the check's.
    fn spend(&mut self, units: usize) -> Step<()> {
        let global = u64::try_from(units).unwrap_or(u64::MAX);
        if self.fuel < units || self.types.fuel < global {
            self.fuel = 0;
            return Err(TooComplex);
        }
        self.fuel -= units;
        self.types.fuel -= global;
        Ok(())
    }

    /// Whether a row of wildcards is useful against `rows`: `Some` with a witness (one pattern
    /// per column, in reverse order) when it is.
    fn useful(&mut self, rows: Vec<Vec<DPat>>, tys: &[TypeId]) -> Step<Option<Vec<String>>> {
        self.spend(1 + rows.len())?;
        let Some((&ty, rest)) = tys.split_first() else {
            return Ok(if rows.is_empty() {
                Some(Vec::new())
            } else {
                None
            });
        };
        if self.depth >= self.max_depth {
            return Err(TooComplex);
        }
        self.depth += 1;
        let answer = self.useful_column(rows, ty, rest);
        self.depth -= 1;
        answer
    }

    fn useful_column(
        &mut self,
        rows: Vec<Vec<DPat>>,
        ty: TypeId,
        rest: &[TypeId],
    ) -> Step<Option<Vec<String>>> {
        let rows = self.expand_or(rows)?;
        let ctors = match self.space(ty) {
            Space::Finite(ctors) => Some(ctors),
            Space::List(elem) => Self::list_ctors(&rows, elem),
            Space::Open => None,
        };
        // The rows by the constructor they start with; rows starting with a wildcard match
        // every constructor. (Grouping keeps specialisation linear in the rows it keeps.)
        let mut groups: HashMap<&Ctor, Vec<usize>> = HashMap::new();
        let mut wild: Vec<usize> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            match row.first() {
                Some(DPat::Ctor(c, _)) => groups.entry(c).or_default().push(i),
                Some(DPat::Slice(..)) => {}
                _ => wild.push(i),
            }
        }
        let complete = ctors.as_ref().is_some_and(|all| {
            (all.len() == 1 && matches!(all[0].0, Ctor::Single))
                || all.iter().all(|(c, _)| groups.contains_key(c))
                || all.iter().any(|(c, _)| matches!(c, Ctor::Exact(_)))
        });
        if complete && let Some(all) = ctors {
            for (ctor, fields) in all {
                let picked: Vec<usize> = if matches!(ctor, Ctor::Exact(_) | Ctor::Long { .. }) {
                    // List patterns with a rest match several lengths: scan every row.
                    self.spend(rows.len())?;
                    (0..rows.len()).collect()
                } else {
                    let mut picked = groups.get(&ctor).cloned().unwrap_or_default();
                    picked.extend(wild.iter().copied());
                    picked.sort_unstable();
                    self.spend(1)?;
                    picked
                };
                let sub = self.specialize(&rows, &picked, &ctor, fields.len())?;
                let tys: Vec<TypeId> = fields.iter().chain(rest.iter()).copied().collect();
                if let Some(mut witness) = self.useful(sub, &tys)? {
                    let at = witness.len() - fields.len();
                    let mut parts = witness.split_off(at);
                    parts.reverse();
                    witness.push(self.render(ty, &ctor, &parts));
                    return Ok(Some(witness));
                }
            }
            return Ok(None);
        }
        let mut default = Vec::new();
        for row in &rows {
            if matches!(row.first(), Some(DPat::Wild)) {
                self.spend(row.len())?;
                default.push(row[1..].to_vec());
            }
        }
        let Some(mut witness) = self.useful(default, rest)? else {
            return Ok(None);
        };
        let missing = ctors
            .and_then(|all| {
                all.into_iter()
                    .find(|(c, _)| !groups.contains_key(c))
                    .map(|(c, fields)| {
                        let wild = vec!["_".to_owned(); fields.len()];
                        self.render(ty, &c, &wild)
                    })
            })
            .unwrap_or_else(|| "_".to_owned());
        witness.push(missing);
        Ok(Some(witness))
    }

    /// Splits rows whose first pattern is an or-pattern into one row per alternative.
    fn expand_or(&mut self, rows: Vec<Vec<DPat>>) -> Step<Vec<Vec<DPat>>> {
        if !rows.iter().any(|r| matches!(r.first(), Some(DPat::Or(_)))) {
            return Ok(rows);
        }
        let mut out = Vec::with_capacity(rows.len());
        let mut work: Vec<Vec<DPat>> = rows.into_iter().rev().collect();
        while let Some(mut row) = work.pop() {
            if let Some(DPat::Or(alts)) = row.first_mut() {
                let alts = std::mem::take(alts);
                for alt in alts.into_iter().rev() {
                    self.spend(row.len())?;
                    let mut new = row.clone();
                    new[0] = alt;
                    work.push(new);
                }
                continue;
            }
            out.push(row);
        }
        Ok(out)
    }

    /// The rows that match constructor `ctor` (of `arity` fields), with the first column
    /// replaced by the constructor's fields.
    fn specialize(
        &mut self,
        rows: &[Vec<DPat>],
        picked: &[usize],
        ctor: &Ctor,
        arity: usize,
    ) -> Step<Vec<Vec<DPat>>> {
        let mut out = Vec::new();
        for row in picked.iter().map(|i| &rows[*i]) {
            let Some((head, tail)) = row.split_first() else {
                continue;
            };
            let fields: Vec<DPat> = match head {
                DPat::Wild | DPat::Or(_) => vec![DPat::Wild; arity],
                DPat::Ctor(c, subs) if c == ctor => {
                    let mut subs = subs.clone();
                    subs.resize(arity, DPat::Wild);
                    subs
                }
                DPat::Ctor(..) => continue,
                DPat::Slice(prefix, suffix) => match *ctor {
                    Ctor::Exact(n) if prefix.len() + suffix.len() <= n => {
                        let mut f = prefix.clone();
                        f.resize(n - suffix.len(), DPat::Wild);
                        f.extend(suffix.iter().cloned());
                        f
                    }
                    Ctor::Long {
                        prefix: p,
                        suffix: s,
                    } => {
                        let mut f = prefix.clone();
                        f.resize(p + s - suffix.len(), DPat::Wild);
                        f.extend(suffix.iter().cloned());
                        f
                    }
                    _ => continue,
                },
            };
            self.spend(fields.len() + tail.len() + 1)?;
            let mut new = fields;
            new.extend(tail.iter().cloned());
            out.push(new);
        }
        Ok(out)
    }

    fn space(&mut self, ty: TypeId) -> Space {
        match self.types.get(ty).clone() {
            Ty::Never => Space::Finite(Vec::new()),
            Ty::Bool => Space::Finite(vec![
                (Ctor::Bool(true), Vec::new()),
                (Ctor::Bool(false), Vec::new()),
            ]),
            Ty::Unit => Space::Finite(vec![(Ctor::Single, Vec::new())]),
            Ty::Option(inner) => {
                Space::Finite(vec![(Ctor::None, Vec::new()), (Ctor::Some, vec![inner])])
            }
            Ty::Tuple(items) => Space::Finite(vec![(Ctor::Single, items.to_vec())]),
            Ty::Record(fields) => Space::Finite(vec![(
                Ctor::Single,
                fields.iter().map(|f| field_type(self.types, f)).collect(),
            )]),
            Ty::Enum(..) | Ty::Untrusted(_) => {
                let Some(id) = self.types.as_enum(ty) else {
                    return Space::Open;
                };
                let n = self.types.enum_def(id).variants.len();
                let mut ctors = Vec::with_capacity(n);
                for i in 0..n {
                    let fields = self.types.payload(ty, i).into_iter().collect();
                    ctors.push((Ctor::Variant(i), fields));
                }
                Space::Finite(ctors)
            }
            Ty::List(elem) => Space::List(elem),
            _ => Space::Open,
        }
    }

    /// The list constructors the column's patterns call for, or `None` when it has only
    /// wildcards.
    fn list_ctors(rows: &[Vec<DPat>], elem: TypeId) -> Option<Vec<(Ctor, Vec<TypeId>)>> {
        let (mut longest, mut prefix, mut suffix, mut any) = (0usize, 0usize, 0usize, false);
        for row in rows {
            match row.first() {
                Some(DPat::Ctor(Ctor::Exact(n), _)) => {
                    longest = longest.max(*n);
                    any = true;
                }
                Some(DPat::Slice(p, s)) => {
                    prefix = prefix.max(p.len());
                    suffix = suffix.max(s.len());
                    any = true;
                }
                _ => {}
            }
        }
        if !any {
            return None;
        }
        let longest = longest.max(prefix + suffix);
        let mut ctors: Vec<(Ctor, Vec<TypeId>)> = (0..=longest)
            .map(|n| (Ctor::Exact(n), vec![elem; n]))
            .collect();
        ctors.push((Ctor::Long { prefix, suffix }, vec![elem; prefix + suffix]));
        Some(ctors)
    }

    /// A witness for constructor `ctor` of `ty` with field witnesses `fields`.
    fn render(&mut self, ty: TypeId, ctor: &Ctor, fields: &[String]) -> String {
        let text = match ctor {
            Ctor::Bool(b) => b.to_string(),
            Ctor::None => "none".to_owned(),
            Ctor::Some => fields.first().cloned().unwrap_or_else(|| "_".to_owned()),
            Ctor::Variant(i) => {
                let name = self
                    .types
                    .as_enum(ty)
                    .map(|id| self.types.enum_def(id).variants[*i].name.clone())
                    .unwrap_or_default();
                // `none` alone would read as an absent option value.
                let name = if name == "none" {
                    let id = self.types.as_enum(ty);
                    let owner = id.map(|id| self.types.enum_def(id).name.clone());
                    format!("{}.none", owner.unwrap_or_default())
                } else {
                    name
                };
                match fields.first() {
                    Some(payload) => format!("{name} {payload}"),
                    None => name,
                }
            }
            Ctor::Single => match self.types.get(ty) {
                Ty::Unit => "()".to_owned(),
                Ty::Record(fs) => {
                    let named: Vec<String> = fs
                        .iter()
                        .zip(fields)
                        .filter(|(_, w)| *w != "_")
                        .map(|(f, w)| format!("{}: {w}", f.name))
                        .collect();
                    if named.is_empty() {
                        "{ .. }".to_owned()
                    } else {
                        format!("{{ {}, .. }}", named.join(", "))
                    }
                }
                _ => format!("({})", fields.join(", ")),
            },
            Ctor::Lit(_) => "_".to_owned(),
            Ctor::Exact(_) => format!("[{}]", fields.join(", ")),
            Ctor::Long { prefix, .. } => {
                let (before, after) = fields.split_at((*prefix).min(fields.len()));
                let mut parts: Vec<&str> = before.iter().map(String::as_str).collect();
                parts.push("..");
                parts.extend(after.iter().map(String::as_str));
                format!("[{}]", parts.join(", "))
            }
        };
        if text.len() > 200 {
            let mut cut = 197;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &text[..cut])
        } else {
            text
        }
    }
}

/// The type a record field's pattern is matched against: an optional field reads as `T?`.
fn field_type(types: &mut Types, f: &crate::types::Field) -> TypeId {
    if f.optional { types.option(f.ty) } else { f.ty }
}
