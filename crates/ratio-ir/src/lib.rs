//! The Ratio Core IR: stage 6 of the implementation plan (chapter 10), the first half.
//!
//! [`lower`] turns a cleanly checked load (stage 3's [`ratio_names::Loaded`] and stages 4 and
//! 5's [`ratio_check::Checked`]) into a [`Program`]: the Core IR of chapter 01 (decision R2), a
//! tree of expressions in which every name is resolved, every effect is an explicit
//! [`ExprKind::Effect`] node, closures name their captures and the surface sugar is gone. The
//! [`canon`] module writes each package's **canonical serialisation**, which numbers every
//! expression (its **site id**, with the package hash) and whose `sha256` is the package's
//! **IR hash** ([`Package::hash`]).
//!
//! The IR is what is hashed and what effect keys name; the interpreter (`ratio-eval`) runs a
//! separate **prepared form** built from it, which is never serialised or hashed, so making
//! the interpreter faster never changes a hash.

#![allow(
    clippy::many_single_char_names,
    reason = "interpreter code names values, patterns and nodes briefly (v, p, n, k)"
)]

pub mod canon;
mod ir;
mod lower;
mod stack;

pub use ir::*;
pub use lower::{FRAME_ENUMS, LowerError, RUNTIME_ENUMS, lower};

/// The version of the canonical IR serialisation, the IR hash and the effect keys (chapter 10,
/// "Versions"). Hashes from another version are not comparable. The prepared form is not
/// covered: it is never serialised or hashed.
pub const IR_VERSION: u32 = 1;

/// The version of the built-in functions' semantics that the hash covers ("plus the standard
/// library's version", chapter 01): bumped whenever a built-in changes what it computes.
pub const STDLIB_VERSION: u32 = 1;

impl Package {
    /// The IR hash in lower-case hexadecimal.
    #[must_use]
    pub fn hash_hex(&self) -> String {
        ratio_check::hex(&self.hash)
    }
}
