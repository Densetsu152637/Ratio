//! Compiles a parsed pattern to a Thompson NFA program for the Pike VM ([`super::vm`]).
//!
//! The program is bounded: its **weight** (one per instruction, a literal's variants, a set's
//! [`Set::weight`]) may not pass [`MAX_PROGRAM`], checked as each instruction is emitted, so a
//! pattern such as `(a{1000}){1000}` is refused before it allocates much, as Rust's `regex`
//! refuses a program over its size limit.

use super::class::Set;
use super::parse::{Look, Node, Parsed};

/// The most weight a compiled program may have.
pub(crate) const MAX_PROGRAM: u64 = 10_000;

/// One instruction.
#[derive(Debug)]
pub(crate) enum Inst {
    /// One of these characters.
    Chars(Box<[char]>),
    /// A character of the set with this index.
    Set(u32),
    /// Any character.
    Any,
    /// Any character but `\n`.
    AnyButNewline,
    Look(Look),
    /// Continue at both, preferring the first.
    Split(u32, u32),
    Jmp(u32),
    /// Record the position in this slot.
    Save(u32),
    Match,
}

/// A compiled program: instruction 0 starts it.
#[derive(Debug)]
pub(crate) struct Program {
    pub insts: Vec<Inst>,
    pub sets: Vec<Set>,
    /// `2 · (groups + 1)`: a start and an end per group, group 0 the whole match.
    pub slots: u32,
    /// The weight (instructions, literal variants and set weights).
    pub weight: u64,
    /// Whether every match must start at the start of the text (the pattern begins with `\A`).
    pub anchored: bool,
}

struct Compiler {
    insts: Vec<Inst>,
    sets: Vec<Set>,
    weight: u64,
}

type R<T> = Result<T, String>;

pub(crate) fn compile(parsed: &Parsed) -> R<Program> {
    let mut c = Compiler {
        insts: Vec::new(),
        sets: Vec::new(),
        weight: 0,
    };
    c.emit(Inst::Save(0), 1)?;
    c.node(&parsed.node)?;
    c.emit(Inst::Save(1), 1)?;
    c.emit(Inst::Match, 1)?;
    Ok(Program {
        anchored: starts_anchored(&parsed.node),
        insts: c.insts,
        sets: c.sets,
        slots: 2 * (parsed.groups + 1),
        weight: c.weight,
    })
}

fn too_large() -> String {
    format!("the pattern compiles to a program larger than the limit of {MAX_PROGRAM} instructions")
}

/// Whether every path through the node begins with `\A`.
fn starts_anchored(node: &Node) -> bool {
    match node {
        Node::Look(Look::StartText) => true,
        Node::Concat(v) => v.first().is_some_and(starts_anchored),
        Node::Group { node, .. } => starts_anchored(node),
        Node::Alt(v) => !v.is_empty() && v.iter().all(starts_anchored),
        _ => false,
    }
}

impl Compiler {
    fn pc(&self) -> u32 {
        u32::try_from(self.insts.len()).unwrap_or(u32::MAX)
    }

    fn emit(&mut self, inst: Inst, weight: u64) -> R<u32> {
        self.weight += weight;
        if self.weight > MAX_PROGRAM {
            return Err(too_large());
        }
        let pc = self.pc();
        self.insts.push(inst);
        Ok(pc)
    }

    fn patch(&mut self, at: u32, inst: Inst) {
        if let Some(slot) = self.insts.get_mut(at as usize) {
            *slot = inst;
        }
    }

    fn node(&mut self, node: &Node) -> R<()> {
        match node {
            Node::Empty => {}
            Node::Chars(cs) => {
                self.emit(Inst::Chars(cs.clone().into_boxed_slice()), cs.len() as u64)?;
            }
            Node::Set(s) => {
                let weight = 1 + s.weight();
                // Checked before the set is copied.
                if self.weight + weight > MAX_PROGRAM {
                    return Err(too_large());
                }
                let index = u32::try_from(self.sets.len()).unwrap_or(u32::MAX);
                self.sets.push(s.clone());
                self.emit(Inst::Set(index), weight)?;
            }
            Node::Dot { newline } => {
                self.emit(
                    if *newline {
                        Inst::Any
                    } else {
                        Inst::AnyButNewline
                    },
                    1,
                )?;
            }
            Node::Look(l) => {
                self.emit(Inst::Look(*l), 1)?;
            }
            Node::Group { index, node } => match index {
                Some(i) => {
                    self.emit(Inst::Save(2 * i), 1)?;
                    self.node(node)?;
                    self.emit(Inst::Save(2 * i + 1), 1)?;
                }
                None => self.node(node)?,
            },
            Node::Concat(v) => {
                for n in v {
                    self.node(n)?;
                }
            }
            Node::Alt(v) => self.alt(v)?,
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => self.repeat(node, *min, *max, *greedy)?,
        }
        Ok(())
    }

    /// `a | b | c`: `split(a, next); a; jmp end; next: split(b, next2); b; jmp end; c; end:`.
    fn alt(&mut self, v: &[Node]) -> R<()> {
        let mut jumps = Vec::new();
        for (i, n) in v.iter().enumerate() {
            if i + 1 == v.len() {
                self.node(n)?;
            } else {
                let split = self.emit(Inst::Split(0, 0), 1)?;
                self.node(n)?;
                jumps.push(self.emit(Inst::Jmp(0), 1)?);
                let next = self.pc();
                self.patch(split, Inst::Split(split + 1, next));
            }
        }
        let end = self.pc();
        for j in jumps {
            self.patch(j, Inst::Jmp(end));
        }
        Ok(())
    }

    fn split(&self, body: u32, out: u32, greedy: bool) -> Inst {
        if greedy {
            Inst::Split(body, out)
        } else {
            Inst::Split(out, body)
        }
    }

    /// `e{min,max}`: `min` copies, then `max - min` optional copies (or a loop when unbounded).
    fn repeat(&mut self, node: &Node, min: u32, max: Option<u32>, greedy: bool) -> R<()> {
        for _ in 0..min {
            self.node(node)?;
        }
        match max {
            None => {
                // L: split(body, out); body; jmp L; out:
                let l = self.emit(Inst::Split(0, 0), 1)?;
                self.node(node)?;
                self.emit(Inst::Jmp(l), 1)?;
                let out = self.pc();
                let s = self.split(l + 1, out, greedy);
                self.patch(l, s);
            }
            Some(max) => {
                let mut splits = Vec::new();
                for _ in min..max {
                    splits.push(self.emit(Inst::Split(0, 0), 1)?);
                    self.node(node)?;
                }
                let out = self.pc();
                for s in splits {
                    let inst = self.split(s + 1, out, greedy);
                    self.patch(s, inst);
                }
            }
        }
        Ok(())
    }
}
