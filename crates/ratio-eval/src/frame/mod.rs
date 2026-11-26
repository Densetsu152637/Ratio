//! Frames (chapter 04; stage 10 of the implementation plan).
//!
//! A harness call, an entry and a `with { … } { … }` block each open a **frame**: a bounded
//! context with its own tools, permissions, budgets, `sees`, limits and conversation mode,
//! never wider than its caller's in any dimension. This module holds:
//!
//! - [`scope`]: the frame's [`Scope`], the [`Request`] a configuration makes, and the two pure
//!   functions of attenuation, [`attenuate`] (the meet) and [`check_within`] (the check a host
//!   repeats against its own record of the parent, as Imperium's `RatioHost::open_frame` does);
//! - [`ledger`]: the budget [`Ledgers`] of a frame tree, with admission by reservation, the
//!   effective `max_tokens`, the reply floor, the refusal of unpriced hosted models under a
//!   cost cap, the shared `steps` ledger and running-branch slots for the width;
//! - the configuration reader (`HarnessConfig` to a [`Request`] and [`Settings`]), the stack of
//!   open frames with their **frame paths** (`turn/critic@s41#0/c12#0.b2/judge@s30#1`), and
//!   `meta`'s readers.
//!
//! The scope and ledger types are pure and depend on no other part of the interpreter, so a
//! host can mirror them; [`FrameCtx`] is what every effect carries to the host.

mod config;
pub mod ledger;
pub(crate) mod meta;
pub mod scope;
pub(crate) mod stack;

pub use config::Settings;
pub use ledger::{
    Admission, Amounts, Card, LedgerError, Ledgers, MIN_REPLY_TOKENS, ModelCall, NodeId, Prices,
    Refused, Reservation,
};
pub use meta::gate;
pub use scope::{
    BudgetKind, Caps, Context, DEFAULT_DEPTH, DEFAULT_WIDTH, Dimension, Folder, FrameKind,
    Location, MAX_DEPTH, MAX_WIDTH, Mode, Paths, Refusal, Request, Rules, Scope, Sees, SeesItem,
    SeesPatch, ToolPat, ToolRequest, ToolSet, attenuate, check_within,
};
pub use stack::{FrameCtx, Session};

pub(crate) use config::{parse_work as config_work, read as read_config};
