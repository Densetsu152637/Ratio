//! Secrecy labels (chapter 03, "Secrecy"): a set of sources, carried on every leaf of a value.
//!
//! A [`Labels`] value is a set of up to 32 label sources; any non-empty set means the value
//! is secret. Stage 11 assigns the bits to sources (a secret path's file contents, a private
//! memory entry, a secret answer, a secret command's output); stage 6 only carries, combines
//! and checks them. Labels combine by union and are never removed except by `declassify`.

use std::ops::{BitOr, BitOrAssign};

/// A set of secrecy label sources.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Labels(u32);

impl Labels {
    /// No label: the value is not secret.
    pub const NONE: Labels = Labels(0);

    /// The set with these source bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Labels {
        Labels(bits)
    }

    /// One source, by its index (`0..32`; larger indexes wrap).
    #[must_use]
    pub const fn source(index: u32) -> Labels {
        Labels(1 << (index % 32))
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether the set is empty (the value is not secret).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn union(self, other: Labels) -> Labels {
        Labels(self.0 | other.0)
    }

    /// Whether every source of `other` is in `self`.
    #[must_use]
    pub const fn contains(self, other: Labels) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Labels {
    type Output = Labels;
    fn bitor(self, rhs: Labels) -> Labels {
        self.union(rhs)
    }
}

impl BitOrAssign for Labels {
    fn bitor_assign(&mut self, rhs: Labels) {
        *self = self.union(rhs);
    }
}
