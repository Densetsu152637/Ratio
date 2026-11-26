//! The Pike VM: a breadth-first simulation of the program over the text, one list of threads
//! per position, each instruction at most once per list. No backtracking: a search takes
//! O(|text| · weight · (1 + slots)) time and O(instructions · slots) memory, whatever the
//! pattern. Threads are kept in priority order and a match cuts the threads after it, which
//! gives the leftmost-first semantics (with lazy repetitions) of Rust's `regex`.
//!
//! Every elementary operation (a thread explored or stepped, a slot copied, a set node tested)
//! adds one to the caller's work counter, which the charge tests compare with the charge.

use super::class::is_word;
use super::compile::{Inst, Program};
use super::parse::Look;

/// An unset slot.
pub(crate) const UNSET: usize = usize::MAX;

/// A list of threads: the instructions reached, in priority order, with the slots each
/// carries (by instruction).
#[derive(Debug)]
struct Threads {
    dense: Vec<u32>,
    sparse: Vec<u32>,
    slots: Vec<usize>,
}

impl Threads {
    fn new(insts: usize, ns: usize) -> Threads {
        Threads {
            dense: Vec::with_capacity(insts),
            sparse: vec![0; insts],
            slots: vec![UNSET; insts * ns],
        }
    }

    fn contains(&self, pc: u32) -> bool {
        let i = self.sparse[pc as usize] as usize;
        i < self.dense.len() && self.dense[i] == pc
    }

    fn insert(&mut self, pc: u32) {
        self.sparse[pc as usize] = u32::try_from(self.dense.len()).unwrap_or(u32::MAX);
        self.dense.push(pc);
    }

    fn clear(&mut self) {
        self.dense.clear();
    }

    fn slots(&self, pc: u32, ns: usize) -> &[usize] {
        let at = pc as usize * ns;
        &self.slots[at..at + ns]
    }

    fn slots_mut(&mut self, pc: u32, ns: usize) -> &mut [usize] {
        let at = pc as usize * ns;
        &mut self.slots[at..at + ns]
    }
}

#[derive(Debug)]
enum Frame {
    Explore(u32),
    Restore(usize, usize),
}

/// A position in the text: its byte offset and the characters on either side.
#[derive(Clone, Copy, Debug)]
struct Pos {
    at: usize,
    prev: Option<char>,
    next: Option<char>,
}

impl Pos {
    fn new(hay: &str, at: usize) -> Pos {
        Pos {
            at,
            prev: hay[..at].chars().next_back(),
            next: hay[at..].chars().next(),
        }
    }
}

fn holds(look: Look, pos: &Pos, len: usize) -> bool {
    match look {
        Look::StartText => pos.at == 0,
        Look::EndText => pos.at == len,
        Look::StartLine => pos.prev.is_none_or(|c| c == '\n'),
        Look::EndLine => pos.next.is_none_or(|c| c == '\n'),
        Look::Word => pos.prev.is_some_and(is_word) != pos.next.is_some_and(is_word),
        Look::NotWord => pos.prev.is_some_and(is_word) == pos.next.is_some_and(is_word),
    }
}

/// The VM's working memory, sized for one program and a number of tracked slots, reused
/// across the searches of one built-in call.
#[derive(Debug)]
pub(crate) struct Vm {
    clist: Threads,
    nlist: Threads,
    stack: Vec<Frame>,
    curr: Vec<usize>,
    ns: usize,
}

/// The bytes of working memory a VM for `insts` instructions tracking `ns` slots holds, as the
/// heap ledger charges it (fixed sizes, platform-independent).
pub(crate) fn scratch_bytes(insts: u64, ns: u64) -> u64 {
    // Two lists (a dense and a sparse index and the slots per instruction) and the stack.
    insts
        .saturating_mul(2 * (4 + 4) + 16)
        .saturating_add(insts.saturating_mul(ns).saturating_mul(2 * 8))
        .saturating_add(ns.saturating_mul(8))
}

impl Vm {
    /// A VM tracking the first `ns` slots (0 when only whether there is a match matters, 2 for
    /// the match's span, the program's slot count for every group).
    pub(crate) fn new(prog: &Program, ns: usize) -> Vm {
        let ns = ns.min(prog.slots as usize);
        let n = prog.insts.len();
        Vm {
            clist: Threads::new(n, ns),
            nlist: Threads::new(n, ns),
            stack: Vec::with_capacity(2 * n),
            curr: vec![UNSET; ns],
            ns,
        }
    }

    /// The leftmost-first match starting at or after byte `start`: its tracked slots (byte
    /// offsets, [`UNSET`] for a group that did not take part), or `None`.
    pub(crate) fn search(
        &mut self,
        prog: &Program,
        hay: &str,
        start: usize,
        work: &mut u64,
    ) -> Option<Vec<usize>> {
        let ns = self.ns;
        let Vm {
            clist,
            nlist,
            stack,
            curr,
            ..
        } = self;
        clist.clear();
        nlist.clear();
        let mut matched: Option<Vec<usize>> = None;
        let mut pos = Pos::new(hay, start);
        loop {
            if matched.is_none() && (!prog.anchored || pos.at == 0) {
                curr.fill(UNSET);
                *work += ns as u64;
                add(clist, stack, curr, prog, ns, 0, &pos, hay.len(), work);
            }
            if clist.dense.is_empty() && (matched.is_some() || prog.anchored) {
                break;
            }
            let next = pos.next;
            let npos = match next {
                Some(c) => Pos::new(hay, pos.at + c.len_utf8()),
                None => pos,
            };
            nlist.clear();
            for i in 0..clist.dense.len() {
                let pc = clist.dense[i];
                *work += 1;
                let step = match &prog.insts[pc as usize] {
                    Inst::Match => {
                        if ns == 0 {
                            return Some(Vec::new());
                        }
                        *work += ns as u64;
                        matched = Some(clist.slots(pc, ns).to_vec());
                        // The threads after this one have lower priority.
                        break;
                    }
                    Inst::Chars(cs) => {
                        *work += cs.len() as u64;
                        next.is_some_and(|c| cs.contains(&c))
                    }
                    Inst::Set(k) => next.is_some_and(|c| prog.sets[*k as usize].contains(c, work)),
                    Inst::Any => next.is_some(),
                    Inst::AnyButNewline => next.is_some_and(|c| c != '\n'),
                    _ => false,
                };
                if step {
                    curr.copy_from_slice(clist.slots(pc, ns));
                    *work += ns as u64;
                    add(nlist, stack, curr, prog, ns, pc + 1, &npos, hay.len(), work);
                }
            }
            if next.is_none() {
                break;
            }
            pos = npos;
            std::mem::swap(clist, nlist);
        }
        matched
    }
}

/// Adds the thread at `pc0` to `list`, following jumps, splits (in priority order), saves and
/// assertions at `pos`; `curr` holds the thread's slots and is restored on return.
#[allow(clippy::too_many_arguments, reason = "the VM's split borrows")]
fn add(
    list: &mut Threads,
    stack: &mut Vec<Frame>,
    curr: &mut [usize],
    prog: &Program,
    ns: usize,
    pc0: u32,
    pos: &Pos,
    len: usize,
    work: &mut u64,
) {
    stack.push(Frame::Explore(pc0));
    while let Some(frame) = stack.pop() {
        let pc = match frame {
            Frame::Restore(slot, old) => {
                curr[slot] = old;
                continue;
            }
            Frame::Explore(pc) => pc,
        };
        if list.contains(pc) {
            continue;
        }
        list.insert(pc);
        *work += 1;
        match &prog.insts[pc as usize] {
            Inst::Jmp(x) => stack.push(Frame::Explore(*x)),
            Inst::Split(x, y) => {
                stack.push(Frame::Explore(*y));
                stack.push(Frame::Explore(*x));
            }
            Inst::Save(slot) => {
                let slot = *slot as usize;
                if slot < ns {
                    stack.push(Frame::Restore(slot, curr[slot]));
                    curr[slot] = pos.at;
                }
                stack.push(Frame::Explore(pc + 1));
            }
            Inst::Look(look) => {
                if holds(*look, pos, len) {
                    stack.push(Frame::Explore(pc + 1));
                }
            }
            _ => {
                list.slots_mut(pc, ns).copy_from_slice(curr);
                *work += ns as u64;
            }
        }
    }
}
