//! The effect tape (chapter 01, "The effect tape" and "The turn sequence"; stage 11a).
//!
//! [`TapeHost`] wraps a host. In **record** mode it forwards every call and writes the tape:
//! an entry for each input-bearing effect (its key, request digest and response, with labels),
//! every scheduling event of the turn numbered on one turn-global sequence, and the turn's
//! reply digest. In **replay** mode it answers input-bearing effects from the tape by key,
//! after checking the request digest; the scheduler takes every scheduling decision from it;
//! and output-only effects are suppressed. Replay checks the tape against the tape alone
//! first: it rebuilds the ledgers from the recorded events in sequence order and verifies
//! each admission, refusal and completion against them, the step totals against the limit,
//! every blob against its digest, and every cancellation outcome against the entries.
//!
//! **Format** ([`TAPE_VERSION`] 1). JSON Lines, uncompressed; compression (`.jsonl.zst`) is a
//! property of the file, which the host applies when it writes or reads it. The first line is
//! the header; then, in recorded order, blob lines, entry lines and event lines; the last line
//! is the end of the turn.
//!
//! ```text
//! {"tape":1,"ratio":"0.1.0","ir":1,"mode":"default","headless":false,"seed":"…","root":"turn","caps":{…},"width":16}
//! {"blob":"<blake3 hex>","data":"<the encoded value>"}            a body above 1 KiB, once
//! {"blob":"r:<redaction digest>","redacted":true}                  a labelled body, headless
//! {"e":"<key>","q":"<request digest>","v":<value>}                 an entry (or "x": error,
//!                                                                  or "b": blob name, "l": labels)
//! {"seq":1,"ev":"admit","key":"…","frame":"turn","held":{…},"max":…,"model":{…}}
//! {"out":"<key>","d":"<digest>"}                                   full mode: an output effect
//! {"end":"<reply digest>"}
//! ```
//!
//! Values are written by [`codec`] with their labels; a replayed value carries the union of
//! its recorded labels and those the replaying host's source rules give it
//! ([`Host::source_labels`]): replay can add labels, never remove them.

mod codec;
mod json;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Instant;

use json::J;

use crate::frame::{Amounts, BudgetKind, Caps, Card, FrameCtx, Ledgers, ModelCall, NodeId, Prices};
use crate::host::{
    ApprovalRequest, ChatRequest, ConvId, ConvSelector, Countable, Digest, EffectCtx, EffectKey,
    FrameOutcome, Host, HostError, HostFuture, Labelled, ModelCard, ModelRef, ModelRequest,
    ModelResponse, Operation, RatioEvent, ToolClass, ToolInvocation, ToolOutcome, Usage,
};
use crate::labels::Labels;
use crate::machine::{EffectRequest, Outcome};
use crate::prepare::Prepared;
use crate::value::Value;

/// The version of the tape format: its header, entry encoding and label encoding
/// (chapter 10, "Versions"). A tape is replayable only when this and `IR_VERSION` match.
pub const TAPE_VERSION: u32 = 1;

/// The most a tape may hold (chapter 07: 64 MiB per turn by default); recording stops beyond.
pub const MAX_TAPE_BYTES: usize = 64 * 1024 * 1024;

/// The most lines a tape may have.
pub const MAX_TAPE_LINES: usize = 1 << 20;

/// A response body above this many bytes is stored once, in a blob line.
pub const BLOB_THRESHOLD: usize = 1024;

/// Lower-case hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn blake3(bytes: &[u8]) -> Digest {
    Digest(crate::builtins::blake3::hash(bytes))
}

/// The digest of a host value, without its labels.
#[must_use]
pub fn value_digest(v: &Value) -> Digest {
    blake3(
        codec::encode(v, false)
            .map(|j| j.text())
            .unwrap_or_default()
            .as_bytes(),
    )
}

/// The plain BLAKE3 digest of a host value's body as the tape would store it (with its labels):
/// what a headless tape must never name a labelled body by.
#[must_use]
pub fn body_digest(v: &Value) -> Digest {
    blake3(
        codec::encode(v, true)
            .map(|j| j.text())
            .unwrap_or_default()
            .as_bytes(),
    )
}

/// HMAC-SHA-256: the redaction digest, keyed per session (chapter 03), so a low-entropy
/// secret is not revealed by its name.
#[must_use]
pub fn redaction_digest(key: &[u8; 32], data: &[u8]) -> Digest {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(key);
    let mut inner = Vec::with_capacity(64 + data.len());
    inner.extend(k.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(data);
    let ih = ratio_check::sha256(&inner);
    let mut outer = Vec::with_capacity(96);
    outer.extend(k.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&ih);
    Digest(ratio_check::sha256(&outer))
}

/// What the program reads of a recorded admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Admitted, with the effective `max_tokens` the request was sent with.
    Admit(Option<u64>),
    Refuse(BudgetKind),
}

/// Amounts on the tape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmountsRec(pub Amounts);

impl From<Amounts> for AmountsRec {
    fn from(a: Amounts) -> Self {
        AmountsRec(a)
    }
}

/// A scheduling event on the turn sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A frame opened, with its ledgers' caps (the replay model is built from these).
    Frame {
        path: String,
        parent: String,
        caps: Caps,
        width: u32,
    },
    /// An admission with its reservation and the inputs of its arithmetic.
    Admit {
        key: String,
        frame: String,
        held: AmountsRec,
        max: Option<u64>,
        model: Option<ModelCall>,
    },
    /// An admission that waited for the reply floor.
    Wait {
        key: String,
    },
    /// A refusal, with the inputs of its arithmetic.
    Refuse {
        key: String,
        frame: String,
        which: String,
        model: Option<ModelCall>,
    },
    Complete {
        key: String,
        actual: AmountsRec,
    },
    /// A branch's step total when it ended or was cancelled.
    Steps {
        branch: String,
        steps: u64,
    },
    /// Where a cancelled branch was cancelled.
    CancelPoint {
        branch: String,
        effects: u64,
        steps: u64,
    },
    /// How a cancelled call ended (`aborted`, `abandoned`, `drained`, `withdrawn`), and for a
    /// drained one the digest of its discarded response.
    Ended {
        key: String,
        outcome: String,
        digest: Option<String>,
    },
    /// An outstanding drain moved to another frame.
    Reparent {
        key: String,
        owner: String,
    },
    RaceWon {
        construct: String,
        index: u32,
    },
    ParFailed {
        construct: String,
        index: u32,
    },
    /// A timeout scope fired, at this point of its branch.
    TimeoutFired {
        branch: String,
        scope: u32,
        effects: u64,
        steps: u64,
    },
    /// A branch that never started because its construct was cancelled or decided.
    Skipped {
        construct: String,
        index: u32,
    },
}

impl Event {
    pub(crate) fn admit(
        key: &EffectKey,
        r: &crate::frame::Reservation,
        model: Option<ModelCall>,
    ) -> Event {
        Event::Admit {
            key: key.to_string(),
            frame: key.frame.to_string(),
            held: r.held.into(),
            max: r.max_tokens,
            model,
        }
    }

    /// An admission recorded under `key` (an effect's key, or `key~i` for the `i`th tool
    /// call one effect makes) in the frame at `frame`.
    pub(crate) fn admit_named(
        key: &str,
        frame: &str,
        r: &crate::frame::Reservation,
        model: Option<ModelCall>,
    ) -> Event {
        Event::Admit {
            key: key.to_owned(),
            frame: frame.to_owned(),
            held: r.held.into(),
            max: r.max_tokens,
            model,
        }
    }

    pub(crate) fn refuse_named(
        key: &str,
        frame: &str,
        which: BudgetKind,
        model: Option<ModelCall>,
    ) -> Event {
        Event::Refuse {
            key: key.to_owned(),
            frame: frame.to_owned(),
            which: which.name().to_owned(),
            model,
        }
    }

    pub(crate) fn refuse(key: &EffectKey, which: BudgetKind, model: Option<ModelCall>) -> Event {
        Event::Refuse {
            key: key.to_string(),
            frame: key.frame.to_string(),
            which: which.name().to_owned(),
            model,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Event::Frame { .. } => "frame",
            Event::Admit { .. } => "admit",
            Event::Wait { .. } => "wait",
            Event::Refuse { .. } => "refuse",
            Event::Complete { .. } => "complete",
            Event::Steps { .. } => "steps",
            Event::CancelPoint { .. } => "cancel",
            Event::Ended { .. } => "ended",
            Event::Reparent { .. } => "reparent",
            Event::RaceWon { .. } => "race_won",
            Event::ParFailed { .. } => "par_failed",
            Event::TimeoutFired { .. } => "timeout",
            Event::Skipped { .. } => "skipped",
        }
    }

    fn to_j(&self, seq: u64) -> J {
        let s = |x: &str| J::Str(x.to_owned());
        let n = |x: u64| J::Int(i64::try_from(x).unwrap_or(i64::MAX));
        let mut m = vec![
            ("seq".to_owned(), n(seq)),
            ("ev".to_owned(), s(self.name())),
        ];
        let mut put = |k: &str, v: J| m.push((k.to_owned(), v));
        match self {
            Event::Frame {
                path,
                parent,
                caps,
                width,
            } => {
                put("path", s(path));
                put("parent", s(parent));
                put("caps", caps_j(caps));
                put("width", n(u64::from(*width)));
            }
            Event::Admit {
                key,
                frame,
                held,
                max,
                model,
            } => {
                put("key", s(key));
                put("frame", s(frame));
                put("held", amounts_j(&held.0));
                put("max", max.map_or(J::Null, n));
                if let Some(c) = model {
                    put("model", model_j(c));
                }
            }
            Event::Wait { key } => put("key", s(key)),
            Event::Refuse {
                key,
                frame,
                which,
                model,
            } => {
                put("key", s(key));
                put("frame", s(frame));
                put("which", s(which));
                if let Some(c) = model {
                    put("model", model_j(c));
                }
            }
            Event::Complete { key, actual } => {
                put("key", s(key));
                put("actual", amounts_j(&actual.0));
            }
            Event::Steps { branch, steps } => {
                put("branch", s(branch));
                put("steps", n(*steps));
            }
            Event::CancelPoint {
                branch,
                effects,
                steps,
            } => {
                put("branch", s(branch));
                put("effects", n(*effects));
                put("steps", n(*steps));
            }
            Event::Ended {
                key,
                outcome,
                digest,
            } => {
                put("key", s(key));
                put("outcome", s(outcome));
                if let Some(d) = digest {
                    put("digest", s(d));
                }
            }
            Event::Reparent { key, owner } => {
                put("key", s(key));
                put("owner", s(owner));
            }
            Event::RaceWon { construct, index } | Event::ParFailed { construct, index } => {
                put("construct", s(construct));
                put("index", n(u64::from(*index)));
            }
            Event::TimeoutFired {
                branch,
                scope,
                effects,
                steps,
            } => {
                put("branch", s(branch));
                put("scope", n(u64::from(*scope)));
                put("effects", n(*effects));
                put("steps", n(*steps));
            }
            Event::Skipped { construct, index } => {
                put("construct", s(construct));
                put("index", n(u64::from(*index)));
            }
        }
        J::Obj(m)
    }

    fn from_j(j: &J) -> Result<Event, String> {
        let bad = || {
            format!(
                "a malformed event: {}",
                j.text().chars().take(200).collect::<String>()
            )
        };
        let s = |k: &str| j.str(k).map(str::to_owned).ok_or_else(bad);
        let u = |k: &str| j.uint(k).ok_or_else(bad);
        let u32_ = |k: &str| u(k).and_then(|x| u32::try_from(x).map_err(|_| bad()));
        Ok(match j.str("ev").ok_or_else(bad)? {
            "frame" => Event::Frame {
                path: s("path")?,
                parent: s("parent")?,
                caps: j.get("caps").map(caps_from).ok_or_else(bad)??,
                width: u32_("width")?,
            },
            "admit" => Event::Admit {
                key: s("key")?,
                frame: s("frame")?,
                held: AmountsRec(j.get("held").map(amounts_from).ok_or_else(bad)??),
                max: match j.get("max") {
                    Some(J::Null) | None => None,
                    Some(_) => Some(u("max")?),
                },
                model: j.get("model").map(model_from).transpose()?,
            },
            "wait" => Event::Wait { key: s("key")? },
            "refuse" => Event::Refuse {
                key: s("key")?,
                frame: s("frame")?,
                which: s("which")?,
                model: j.get("model").map(model_from).transpose()?,
            },
            "complete" => Event::Complete {
                key: s("key")?,
                actual: AmountsRec(j.get("actual").map(amounts_from).ok_or_else(bad)??),
            },
            "steps" => Event::Steps {
                branch: s("branch")?,
                steps: u("steps")?,
            },
            "cancel" => Event::CancelPoint {
                branch: s("branch")?,
                effects: u("effects")?,
                steps: u("steps")?,
            },
            "ended" => Event::Ended {
                key: s("key")?,
                outcome: s("outcome")?,
                digest: j.str("digest").map(str::to_owned),
            },
            "reparent" => Event::Reparent {
                key: s("key")?,
                owner: s("owner")?,
            },
            "race_won" => Event::RaceWon {
                construct: s("construct")?,
                index: u32_("index")?,
            },
            "par_failed" => Event::ParFailed {
                construct: s("construct")?,
                index: u32_("index")?,
            },
            "timeout" => Event::TimeoutFired {
                branch: s("branch")?,
                scope: u32_("scope")?,
                effects: u("effects")?,
                steps: u("steps")?,
            },
            "skipped" => Event::Skipped {
                construct: s("construct")?,
                index: u32_("index")?,
            },
            _ => return Err(bad()),
        })
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut j = self.to_j(0);
        if let J::Obj(m) = &mut j {
            m.remove(0);
        }
        f.write_str(&j.text())
    }
}

fn opt_j(x: Option<u64>) -> J {
    x.map_or(J::Null, |x| J::Int(i64::try_from(x).unwrap_or(i64::MAX)))
}

fn opt_from(j: &J, k: &str) -> Result<Option<u64>, String> {
    match j.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(J::Int(i)) => u64::try_from(*i)
            .map(Some)
            .map_err(|_| "a negative amount".to_owned()),
        Some(_) => Err("a malformed amount".to_owned()),
    }
}

fn caps_j(c: &Caps) -> J {
    J::Obj(
        [
            ("tokens", c.tokens),
            ("cost", c.cost),
            ("wall", c.wall),
            ("steps", c.steps),
            ("model_calls", c.model_calls),
            ("tool_calls", c.tool_calls),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), opt_j(v)))
        .collect(),
    )
}

fn caps_from(j: &J) -> Result<Caps, String> {
    Ok(Caps {
        tokens: opt_from(j, "tokens")?,
        cost: opt_from(j, "cost")?,
        wall: opt_from(j, "wall")?,
        steps: opt_from(j, "steps")?,
        model_calls: opt_from(j, "model_calls")?,
        tool_calls: opt_from(j, "tool_calls")?,
    })
}

fn amounts_j(a: &Amounts) -> J {
    caps_j(&Caps {
        tokens: Some(a.tokens),
        cost: Some(a.cost),
        wall: None,
        steps: Some(a.steps),
        model_calls: Some(a.model_calls),
        tool_calls: Some(a.tool_calls),
    })
}

fn amounts_from(j: &J) -> Result<Amounts, String> {
    let c = caps_from(j)?;
    Ok(Amounts {
        tokens: c.tokens.unwrap_or(0),
        cost: c.cost.unwrap_or(0),
        steps: c.steps.unwrap_or(0),
        model_calls: c.model_calls.unwrap_or(0),
        tool_calls: c.tool_calls.unwrap_or(0),
    })
}

fn model_j(c: &ModelCall) -> J {
    let mut m = vec![
        ("input".to_owned(), opt_j(Some(c.input))),
        ("requested".to_owned(), opt_j(c.max_tokens)),
        ("max_output".to_owned(), opt_j(c.card.max_output)),
        ("window".to_owned(), opt_j(c.card.window)),
        ("local".to_owned(), J::Bool(c.card.local)),
        ("min_reply".to_owned(), opt_j(Some(c.min_reply))),
    ];
    if let Some(p) = c.card.prices {
        m.push(("price_in".to_owned(), opt_j(Some(p.input))));
        m.push(("price_out".to_owned(), opt_j(Some(p.output))));
    }
    J::Obj(m)
}

fn model_from(j: &J) -> Result<ModelCall, String> {
    let prices = match (opt_from(j, "price_in")?, opt_from(j, "price_out")?) {
        (Some(input), Some(output)) => Some(Prices { input, output }),
        _ => None,
    };
    Ok(ModelCall {
        input: opt_from(j, "input")?.unwrap_or(0),
        max_tokens: opt_from(j, "requested")?,
        card: Card {
            max_output: opt_from(j, "max_output")?,
            window: opt_from(j, "window")?,
            prices,
            local: matches!(j.get("local"), Some(J::Bool(true))),
        },
        min_reply: opt_from(j, "min_reply")?.unwrap_or(crate::frame::MIN_REPLY_TOKENS),
    })
}

/// The scheduler's side of the tape: what it records, and what it asks in replay.
/// [`TapeHost`] implements it.
pub trait Tape: Send + Sync {
    /// Whether this is a replay.
    fn replaying(&self) -> bool;
    /// The turn starts, with its seed and the session's frame.
    fn turn_start(&self, seed: [u8; 32], root: &FrameCtx);
    /// Appends a scheduling event to the turn sequence (record mode).
    fn record(&self, ev: Event);
    /// An output-only effect (in `full` mode, a digest-only entry; in replay, checked).
    fn output(&self, ev: &RatioEvent);
    /// An input-bearing effect was answered (record mode: its entry; a drained call's
    /// discarded answer too).
    ///
    /// # Errors
    ///
    /// When the replay diverged.
    fn answered(
        &self,
        req: &EffectRequest,
        result: &Result<Value, HostError>,
    ) -> Result<(), String>;
    /// The recorded admission under `key` (replay): an effect's key, or `key~i`.
    fn admission_named(&self, key: &str) -> Option<Decision>;
    /// The recorded cancellation point of a branch (replay).
    fn cancel_point(&self, branch: &str) -> Option<(u64, u64)>;
    /// Where a branch's timeout scope fired (replay).
    fn timeout_point(&self, branch: &str, scope: u32) -> Option<(u64, u64)>;
    /// Whether branch `index` of a construct never started (replay).
    fn skipped(&self, construct: &str, index: u32) -> bool;
    /// The recorded winner of a race (replay).
    fn winner(&self, construct: &str) -> Option<u32>;
    /// The recorded first failing branch of a `par` (replay).
    fn first_failure(&self, construct: &str) -> Option<u32>;
    /// A branch's recorded step total (replay): the branch is capped there.
    fn step_total(&self, branch: &str) -> Option<u64>;
    /// Checks a branch's step total against the recorded one (replay).
    ///
    /// # Errors
    ///
    /// On a mismatch.
    fn check_steps(&self, branch: &str, steps: u64) -> Result<(), String>;
    /// Checks that a call cancelled at its point has a recorded outcome (replay).
    ///
    /// # Errors
    ///
    /// When it has none.
    fn check_ended(&self, key: &EffectKey) -> Result<(), String>;
    /// A divergence found by the tape host itself.
    fn divergence(&self) -> Option<String>;
    /// The digest of a host value as this tape names it: keyed when the value is labelled
    /// and the tape headless.
    fn value_digest(&self, v: &Value) -> String;
    /// The turn ended: record or compare the reply digest.
    ///
    /// # Errors
    ///
    /// When the replay's reply digest differs from the recorded one.
    fn turn_end(&self, outcome: &Outcome, program: &Prepared) -> Result<(), String>;
}

/// How a turn is recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordMode {
    /// Entries for input-bearing effects only.
    #[default]
    Default,
    /// Also a digest-only entry for each output-only effect, checked where it happens.
    Full,
}

/// Recording options. The session's redaction key is required: every digest over labelled
/// data in a headless tape is keyed with it ([`redaction_digest`]), so a digest cannot be
/// tested against guesses of a low-entropy secret.
#[derive(Clone, Copy, Debug)]
pub struct RecordOptions {
    pub mode: RecordMode,
    /// A headless tape file: labelled bodies are not stored, only named by the redaction
    /// digest, and digests over labelled data are keyed.
    pub headless: bool,
    redaction_key: [u8; 32],
    max_bytes: usize,
}

impl RecordOptions {
    /// Default-mode, non-headless recording, with the session's redaction key (Imperium's
    /// per-session HMAC key).
    ///
    /// # Errors
    ///
    /// An all-zero key, which keys nothing.
    pub fn new(redaction_key: [u8; 32]) -> Result<RecordOptions, TapeError> {
        if redaction_key == [0; 32] {
            return Err(TapeError("an all-zero redaction key".to_owned()));
        }
        Ok(RecordOptions {
            mode: RecordMode::Default,
            headless: false,
            redaction_key,
            max_bytes: MAX_TAPE_BYTES,
        })
    }

    /// A smaller size limit than [`MAX_TAPE_BYTES`] (never a larger one).
    #[must_use]
    pub fn limit(mut self, bytes: usize) -> RecordOptions {
        self.max_bytes = bytes.min(MAX_TAPE_BYTES);
        self
    }

    #[must_use]
    pub fn mode(mut self, mode: RecordMode) -> RecordOptions {
        self.mode = mode;
        self
    }

    #[must_use]
    pub fn headless(mut self, headless: bool) -> RecordOptions {
        self.headless = headless;
        self
    }
}

/// Replay options.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReplayOptions {
    /// The session's redaction key, to check the keyed digests of a headless tape (without
    /// it, a labelled request of a headless tape cannot be matched).
    pub redaction_key: Option<[u8; 32]>,
    /// Fall through to a live call on a request digest mismatch or a missing entry.
    pub lenient: bool,
    /// Resolve effects in an order permuted by this seed (each answer waits a pseudo-random
    /// number of polls), to show replay does not depend on resolution order.
    pub permute: Option<u64>,
}

/// Why a tape could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TapeError(pub String);

impl fmt::Display for TapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the tape cannot be replayed: {}", self.0)
    }
}

impl std::error::Error for TapeError {}

/// An entry: the request digest and the response (or where it is).
#[derive(Clone, Debug)]
struct Entry {
    req: String,
    res: Res,
}

#[derive(Clone, Debug)]
enum Res {
    Value(J),
    Error(J),
    Blob { name: String, labels: Labels },
}

/// A tape as loaded for replay.
#[derive(Debug, Default)]
struct Loaded {
    seed: [u8; 32],
    headless: bool,
    mode: RecordMode,
    root: String,
    root_caps: Caps,
    root_width: u32,
    entries: HashMap<String, Entry>,
    blobs: HashMap<String, Option<String>>,
    events: Vec<Event>,
    admissions: HashMap<String, Decision>,
    cancels: HashMap<String, (u64, u64)>,
    timeouts: HashMap<(String, u32), (u64, u64)>,
    skipped: HashSet<(String, u32)>,
    winners: HashMap<String, u32>,
    failures: HashMap<String, u32>,
    steps: HashMap<String, u64>,
    ended: HashMap<String, (String, Option<String>)>,
    reparented: HashSet<String>,
    frames: HashMap<String, (Caps, u32)>,
    outputs: HashMap<String, String>,
    reply: Option<String>,
}

#[derive(Debug, Default)]
struct Recording {
    /// The size limit was reached: recording stopped, and the tape refuses replay.
    truncated: bool,
    max_bytes: usize,
    seq: u64,
    lines: Vec<String>,
    bytes: usize,
    full: bool,
    blobs: BTreeMap<String, Option<String>>,
    events: Vec<Event>,
    header: Option<String>,
    end: Option<String>,
}

#[derive(Debug, Default)]
struct ReplayState {
    diverged: Option<String>,
    rng: u64,
    outputs_seen: Vec<String>,
}

enum Mode {
    Record(RecordOptions),
    Replay(Box<Loaded>, ReplayOptions),
}

/// A host that records its turn to a tape, or replays one (see the module documentation).
pub struct TapeHost {
    inner: Arc<dyn Host>,
    mode: Mode,
    rec: Mutex<Recording>,
    rep: Mutex<ReplayState>,
}

impl fmt::Debug for TapeHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.replaying() {
            "TapeHost(replay)"
        } else {
            "TapeHost(record)"
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TapeHost {
    /// Records the turn `inner` answers.
    #[must_use]
    pub fn record(inner: Arc<dyn Host>, opts: RecordOptions) -> TapeHost {
        TapeHost {
            inner,
            mode: Mode::Record(opts),
            rec: Mutex::new(Recording {
                full: opts.mode == RecordMode::Full,
                max_bytes: opts.max_bytes,
                ..Recording::default()
            }),
            rep: Mutex::new(ReplayState::default()),
        }
    }

    /// Loads `text` to replay it; `inner` supplies what replay still asks of a live host
    /// (frames, tool classes, source labels, the clock; and live calls in lenient mode).
    ///
    /// # Errors
    ///
    /// A malformed tape, one over a bound, another version, or a blob that does not match
    /// its digest. (A tape that loads but whose decisions are inconsistent is refused when
    /// the evaluation starts, with `Replay`.)
    pub fn replay(
        inner: Arc<dyn Host>,
        text: &str,
        opts: ReplayOptions,
    ) -> Result<TapeHost, TapeError> {
        let loaded = load(text).map_err(TapeError)?;
        let diverged = verify(&loaded).err();
        Ok(TapeHost {
            inner,
            mode: Mode::Replay(Box::new(loaded), opts),
            rec: Mutex::new(Recording::default()),
            rep: Mutex::new(ReplayState {
                diverged,
                rng: opts.permute.unwrap_or(0) | 1,
                outputs_seen: Vec::new(),
            }),
        })
    }

    fn loaded(&self) -> Option<&Loaded> {
        match &self.mode {
            Mode::Replay(l, _) => Some(l),
            Mode::Record(_) => None,
        }
    }

    /// The tape written so far (record mode): the header, blobs and lines, and the end.
    #[must_use]
    pub fn tape(&self) -> String {
        let r = lock(&self.rec);
        let mut out = String::new();
        if let Some(h) = &r.header {
            out.push_str(h);
            out.push('\n');
        }
        if r.truncated {
            // Recording stopped at the size limit: such a tape is refused by replay.
            out.push_str("{\"truncated\":true}\n");
            return out;
        }
        for (name, data) in &r.blobs {
            let j = match data {
                Some(d) => J::Obj(vec![
                    ("blob".to_owned(), J::Str(name.clone())),
                    ("data".to_owned(), J::Str(d.clone())),
                ]),
                None => J::Obj(vec![
                    ("blob".to_owned(), J::Str(name.clone())),
                    ("redacted".to_owned(), J::Bool(true)),
                ]),
            };
            out.push_str(&j.text());
            out.push('\n');
        }
        for l in &r.lines {
            out.push_str(l);
            out.push('\n');
        }
        if let Some(e) = &r.end {
            out.push_str(e);
            out.push('\n');
        }
        out
    }

    /// The turn sequence: recorded (record mode), or the verified recorded one a replay
    /// re-emits for the live view (replay mode), rendered one event per line.
    #[must_use]
    pub fn events(&self) -> Vec<String> {
        match self.loaded() {
            Some(l) => l.events.iter().map(ToString::to_string).collect(),
            None => lock(&self.rec)
                .events
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    /// The output-only effects a replay evaluated (and suppressed), by key.
    #[must_use]
    pub fn suppressed(&self) -> Vec<String> {
        lock(&self.rep).outputs_seen.clone()
    }

    /// Counts `n` more bytes (and a line) against the tape's limits; at the limit recording
    /// stops for good and the tape is marked truncated (chapter 07).
    fn room(r: &mut Recording, n: usize) -> bool {
        if r.truncated
            || r.bytes.saturating_add(n) > r.max_bytes
            || r.lines.len() + r.blobs.len() >= MAX_TAPE_LINES
        {
            r.truncated = true;
            return false;
        }
        r.bytes += n + 1;
        true
    }

    fn push_line(&self, line: String) -> bool {
        let mut r = lock(&self.rec);
        if !TapeHost::room(&mut r, line.len()) {
            return false;
        }
        r.lines.push(line);
        true
    }

    /// Whether recording stopped at the size limit.
    #[must_use]
    pub fn truncated(&self) -> bool {
        lock(&self.rec).truncated
    }

    /// A digest of `text`: keyed with the session's redaction key when it covers labelled
    /// data in a headless tape (`k:` and the HMAC), else plain BLAKE3.
    fn digest(&self, text: &str, labelled: bool) -> String {
        let (headless, key) = match &self.mode {
            Mode::Record(o) => (o.headless, Some(o.redaction_key)),
            Mode::Replay(l, o) => (l.headless, o.redaction_key),
        };
        if headless && labelled {
            match key {
                Some(k) => format!("k:{}", redaction_digest(&k, text.as_bytes()).hex()),
                None => "k:?".to_owned(),
            }
        } else {
            blake3(text.as_bytes()).hex()
        }
    }

    fn diverge(&self, msg: String) {
        let mut r = lock(&self.rep);
        if r.diverged.is_none() {
            r.diverged = Some(msg);
        }
    }

    /// A replayed answer: from the entry for `cx.key`, after checking the request digest.
    fn answer(
        &self,
        cx: &EffectCtx,
        builtin: &str,
        args: &[(Arc<str>, Value)],
        extra: Labels,
    ) -> Option<Result<Value, HostError>> {
        let l = self.loaded()?;
        let key = cx.key.to_string();
        let lenient = matches!(&self.mode, Mode::Replay(_, o) if o.lenient);
        let Some(e) = l.entries.get(&key) else {
            if !lenient {
                self.diverge(format!("the tape has no entry for `{key}`"));
            }
            return None;
        };
        let labelled = args.iter().any(|(_, v)| crate::sink::check(v).is_err());
        let digest = self.digest(&codec::request_text(builtin, args), labelled);
        if digest != e.req {
            if !lenient {
                self.diverge(format!(
                    "the request at `{key}` differs from the tape's (digest {digest}, recorded {})",
                    e.req
                ));
            }
            return None;
        }
        let decoded = match &e.res {
            Res::Value(j) => codec::decode(j).map(Ok),
            Res::Error(j) => codec::decode_error(j).map(Err),
            Res::Blob { name, labels } => match l.blobs.get(name) {
                Some(Some(data)) => json::parse(data).and_then(|j| codec::decode(&j)).map(|v| {
                    let own = v.labels() | *labels;
                    Ok(v.with_labels(own))
                }),
                Some(None) => {
                    if lenient {
                        return None;
                    }
                    Err(format!(
                        "the response at `{key}` was redacted from this headless tape; replay it \
                         leniently"
                    ))
                }
                None => Err(format!("the blob `{name}` is missing")),
            },
        };
        match decoded {
            // Replay keeps the recorded labels and adds the source rules' (never removes).
            Ok(Ok(v)) => {
                let own = v.labels() | extra;
                Some(Ok(v.with_labels(own)))
            }
            Ok(Err(e)) => Some(Err(e)),
            Err(msg) => {
                self.diverge(msg);
                Some(Err(HostError::Invalid {
                    message: "replay diverged".to_owned(),
                }))
            }
        }
    }

    /// The pseudo-random number of polls an answer waits for (permuted replay).
    fn delay(&self) -> u32 {
        if !matches!(&self.mode, Mode::Replay(_, o) if o.permute.is_some()) {
            return 0;
        }
        let mut r = lock(&self.rep);
        let mut x = r.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.rng = x;
        u32::try_from(x % 8).unwrap_or(0)
    }

    fn replayed<T: Send + Unpin + 'static>(
        &self,
        r: Result<T, HostError>,
    ) -> HostFuture<Result<T, HostError>> {
        Box::pin(Delayed {
            polls: self.delay(),
            value: Some(r),
        })
    }
}

/// A value after a number of polls.
struct Delayed<T> {
    polls: u32,
    value: Option<T>,
}

impl<T: Unpin> Future for Delayed<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if self.polls == 0 {
            return match self.value.take() {
                Some(v) => Poll::Ready(v),
                None => Poll::Pending,
            };
        }
        self.polls -= 1;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

fn not_recorded<T: Send + 'static>(what: &str) -> HostFuture<Result<T, HostError>> {
    let e = HostError::Invalid {
        message: format!("replay diverged: no entry for {what}"),
    };
    Box::pin(async move { Err(e) })
}

impl Host for TapeHost {
    fn generate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ModelRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        if self.replaying() {
            let mut args = req.options.clone();
            args.push((Arc::from("p"), req.prompt.clone()));
            args.sort_by(|a, b| a.0.cmp(&b.0));
            return match self.answer(&cx, &req.builtin, &args, Labels::NONE) {
                Some(r) => self.replayed(r.map(|v| ModelResponse {
                    text: Labelled::new(v.as_text().unwrap_or_default().to_owned(), v.labels()),
                    usage: Usage::default(),
                })),
                None if self.lenient() => Arc::clone(&self.inner).generate(frame, cx, req),
                None => not_recorded(&req.builtin),
            };
        }
        Arc::clone(&self.inner).generate(frame, cx, req)
    }

    fn embed(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        texts: Vec<Labelled<String>>,
        model: Option<ModelRef>,
    ) -> HostFuture<Result<Vec<Vec<f32>>, HostError>> {
        Arc::clone(&self.inner).embed(frame, cx, texts, model)
    }

    fn count_tokens(
        self: Arc<Self>,
        frame: FrameCtx,
        what: Countable,
    ) -> HostFuture<Result<u64, HostError>> {
        // In replay the count is the admission's recorded input, never asked again.
        Arc::clone(&self.inner).count_tokens(frame, what)
    }

    fn resolve_model(&self, frame: &FrameCtx, model: &ModelRef) -> Result<ModelCard, HostError> {
        self.inner.resolve_model(frame, model)
    }

    fn call_tool(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        call: ToolInvocation,
    ) -> HostFuture<Result<ToolOutcome, HostError>> {
        if self.replaying() {
            let extra = self.inner.source_labels(&frame, &call.builtin, &call.args);
            return match self.answer(&cx, &call.builtin, &call.args, extra) {
                Some(r) => self.replayed(r.map(|value| ToolOutcome { value })),
                None if self.lenient() => Arc::clone(&self.inner).call_tool(frame, cx, call),
                None => not_recorded(&call.builtin),
            };
        }
        Arc::clone(&self.inner).call_tool(frame, cx, call)
    }

    fn tool_class(&self, frame: &FrameCtx, tool: &str) -> ToolClass {
        self.inner.tool_class(frame, tool)
    }

    fn is_secret_path(&self, frame: &FrameCtx, path: &str) -> bool {
        self.inner.is_secret_path(frame, path)
    }

    fn source_labels(&self, frame: &FrameCtx, builtin: &str, args: &[(Arc<str>, Value)]) -> Labels {
        self.inner.source_labels(frame, builtin, args)
    }

    fn conversation_labels(&self, frame: &FrameCtx, conv: ConvId) -> Result<Labels, HostError> {
        self.inner.conversation_labels(frame, conv)
    }

    fn conversation(&self, frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError> {
        self.inner.conversation(frame, which)
    }

    fn conversation_digest(&self, frame: &FrameCtx, conv: ConvId) -> Result<Digest, HostError> {
        self.inner.conversation_digest(frame, conv)
    }

    fn chat(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        conv: ConvId,
        req: ChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        Arc::clone(&self.inner).chat(frame, cx, conv, req)
    }

    fn open_frame(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError> {
        let caps = child.scope.budget;
        let width = child.scope.width;
        match self.loaded() {
            Some(l) => match l.frames.get(&*child.path) {
                Some((c, w)) if *c == caps && *w == width => {}
                Some(_) => self.diverge(format!(
                    "the frame `{}` opened with other budgets than on the tape",
                    child.path
                )),
                None => self.diverge(format!("the tape has no frame `{}`", child.path)),
            },
            None => self.record(Event::Frame {
                path: child.path.to_string(),
                parent: parent.path.to_string(),
                caps,
                width,
            }),
        }
        self.inner.open_frame(parent, child)
    }

    fn close_frame(&self, frame: &FrameCtx, outcome: FrameOutcome) {
        self.inner.close_frame(frame, outcome);
    }

    fn operate(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        op: Operation,
    ) -> HostFuture<Result<Value, HostError>> {
        // The person's answers (`@user`: `session.ask`) are never taken from a tape: a crafted
        // tape could forge trusted user text. A replay asks live, under its own session's
        // rules (unattended, the host's recommendations).
        if self.replaying() && !crate::host::bridge::from_user(&op.builtin) {
            let extra = self.inner.source_labels(&frame, &op.builtin, &op.args);
            return match self.answer(&cx, &op.builtin, &op.args, extra) {
                Some(r) => self.replayed(r),
                None if self.lenient() => Arc::clone(&self.inner).operate(frame, cx, op),
                None => not_recorded(&op.builtin),
            };
        }
        Arc::clone(&self.inner).operate(frame, cx, op)
    }

    fn approve(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ApprovalRequest,
    ) -> HostFuture<Result<bool, HostError>> {
        // Never answered from the tape, recording or replaying: a user's decision is asked
        // live, under the running session's own rules (refused when unattended), so a tape
        // can never approve anything, least of all a declassification.
        Arc::clone(&self.inner).approve(frame, cx, req)
    }

    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn seed(&self) -> [u8; 32] {
        match self.loaded() {
            Some(l) => l.seed,
            None => self.inner.seed(),
        }
    }

    fn timer(&self, at: Instant) -> HostFuture<()> {
        self.inner.timer(at)
    }

    fn emit(&self, frame: &FrameCtx, event: RatioEvent) -> Result<(), HostError> {
        // Suppressed in replay: a replay never streams, emits or logs to a live session.
        if self.replaying() {
            return Ok(());
        }
        self.inner.emit(frame, event)
    }
}

/// The arguments an approval's request digest covers (the summary and detail).
fn approval_args(req: &ApprovalRequest) -> Vec<(Arc<str>, Value)> {
    let mut a = vec![(
        Arc::from("summary"),
        Value::text(&req.summary.value).with_labels(req.summary.labels),
    )];
    if let Some(d) = &req.detail {
        a.push((Arc::from("detail"), d.clone()));
    }
    a
}

/// The arguments the scheduler's request names, as the host methods see them, for the
/// request digest: a model request's prompt is `p`, an approval's summary and detail.
fn digest_args(req: &EffectRequest, key: &[u8; 32]) -> (String, Vec<(Arc<str>, Value)>) {
    let call = crate::host::bridge::classify(req);
    match call {
        crate::host::bridge::Call::Tool(t, _) => (t.builtin.to_string(), t.args),
        crate::host::bridge::Call::Op(o) => (o.builtin.to_string(), o.args),
        crate::host::bridge::Call::Model(m) => {
            let mut a = m.options;
            a.push((Arc::from("p"), m.prompt));
            a.sort_by(|x, y| x.0.cmp(&y.0));
            (m.builtin.to_string(), a)
        }
        crate::host::bridge::Call::Approve(a) => {
            let b = match a.kind {
                crate::host::ApprovalKind::Declassify => "declassify",
                crate::host::ApprovalKind::Request => "session.request_approval",
            };
            let mut args = approval_args(&a);
            // What was declassified, by a keyed digest only: a tape never names a secret.
            if a.kind == crate::host::ApprovalKind::Declassify
                && let Some(x) = req.args.first().cloned().flatten()
            {
                let text = codec::encode(&x, false)
                    .map(|j| j.text())
                    .unwrap_or_default();
                let d = redaction_digest(key, text.as_bytes()).hex();
                args.push((Arc::from("x"), Value::text(&format!("k:{d}"))));
            }
            (b.to_owned(), args)
        }
        crate::host::bridge::Call::Event(e) => (e.builtin.to_string(), e.args),
        crate::host::bridge::Call::Unknown(b) => (b.to_string(), Vec::new()),
    }
}

impl TapeHost {
    fn lenient(&self) -> bool {
        matches!(&self.mode, Mode::Replay(_, o) if o.lenient)
    }

    fn options(&self) -> RecordOptions {
        match &self.mode {
            Mode::Record(o) => *o,
            Mode::Replay(..) => RecordOptions {
                mode: RecordMode::Default,
                headless: false,
                redaction_key: [0; 32],
                max_bytes: 0,
            },
        }
    }
}

impl Tape for TapeHost {
    fn replaying(&self) -> bool {
        matches!(self.mode, Mode::Replay(..))
    }

    fn turn_start(&self, seed: [u8; 32], root: &FrameCtx) {
        if self.replaying() {
            return;
        }
        let o = self.options();
        let header = J::Obj(vec![
            ("tape".to_owned(), J::Int(i64::from(TAPE_VERSION))),
            (
                "ratio".to_owned(),
                J::Str(env!("CARGO_PKG_VERSION").to_owned()),
            ),
            ("ir".to_owned(), J::Int(i64::from(ratio_ir::IR_VERSION))),
            (
                "mode".to_owned(),
                J::Str(
                    if o.mode == RecordMode::Full {
                        "full"
                    } else {
                        "default"
                    }
                    .to_owned(),
                ),
            ),
            ("headless".to_owned(), J::Bool(o.headless)),
            ("seed".to_owned(), J::Str(hex(&seed))),
            ("root".to_owned(), J::Str(root.path.to_string())),
            ("caps".to_owned(), caps_j(&root.scope.budget)),
            ("width".to_owned(), J::Int(i64::from(root.scope.width))),
        ]);
        lock(&self.rec).header = Some(header.text());
    }

    fn record(&self, ev: Event) {
        if self.replaying() {
            return;
        }
        let mut r = lock(&self.rec);
        if r.truncated {
            return;
        }
        r.seq += 1;
        let line = ev.to_j(r.seq).text();
        // The event is held twice (its line and the event itself): both count.
        if !TapeHost::room(&mut r, line.len().saturating_mul(2)) {
            return;
        }
        r.lines.push(line);
        r.events.push(ev);
    }

    fn output(&self, ev: &RatioEvent) {
        let key = ev.key.to_string();
        let digest = self.digest(&codec::request_text(&ev.builtin, &ev.args), ev.secret);
        match self.loaded() {
            Some(l) => {
                lock(&self.rep).outputs_seen.push(key.clone());
                if l.mode == RecordMode::Full {
                    match l.outputs.get(&key) {
                        Some(d) if *d == digest => {}
                        _ => self.diverge(format!(
                            "the output effect at `{key}` differs from the tape's"
                        )),
                    }
                }
            }
            None => {
                if lock(&self.rec).full {
                    let j = J::Obj(vec![
                        ("out".to_owned(), J::Str(key)),
                        ("d".to_owned(), J::Str(digest)),
                    ]);
                    let _ = self.push_line(j.text());
                }
            }
        }
    }

    fn answered(
        &self,
        req: &EffectRequest,
        result: &Result<Value, HostError>,
    ) -> Result<(), String> {
        if self.replaying() {
            return match lock(&self.rep).diverged.clone() {
                Some(d) => Err(d),
                None => Ok(()),
            };
        }
        let o = self.options();
        let (builtin, args) = digest_args(req, &o.redaction_key);
        let labelled = args.iter().any(|(_, v)| crate::sink::check(v).is_err());
        let q = self.digest(&codec::request_text(&builtin, &args), labelled);
        let key = req.key().to_string();
        let mut m = vec![("e".to_owned(), J::Str(key)), ("q".to_owned(), J::Str(q))];
        match result {
            Ok(v) => {
                let Ok(j) = codec::encode(v, true) else {
                    // Not recordable: the entry is left out, and replay will diverge there.
                    return Ok(());
                };
                let text = j.text();
                let labelled = crate::sink::check(v).is_err();
                if o.headless && labelled {
                    // Never named by a plain hash, never stored.
                    let name = format!(
                        "r:{}",
                        redaction_digest(&o.redaction_key, text.as_bytes()).hex()
                    );
                    let mut r = lock(&self.rec);
                    if !TapeHost::room(&mut r, name.len()) {
                        return Ok(());
                    }
                    r.blobs.insert(name.clone(), None);
                    drop(r);
                    m.push(("b".to_owned(), J::Str(name)));
                    m.push(("l".to_owned(), J::Int(i64::from(v.labels().bits()))));
                } else if text.len() > BLOB_THRESHOLD {
                    let name = blake3(text.as_bytes()).hex();
                    let mut r = lock(&self.rec);
                    if !r.blobs.contains_key(&name) {
                        if !TapeHost::room(&mut r, name.len() + text.len()) {
                            return Ok(());
                        }
                        r.blobs.insert(name.clone(), Some(text));
                    }
                    drop(r);
                    m.push(("b".to_owned(), J::Str(name)));
                    m.push(("l".to_owned(), J::Int(i64::from(v.labels().bits()))));
                } else {
                    m.push(("v".to_owned(), j));
                }
            }
            Err(e) => m.push(("x".to_owned(), codec::encode_error(e))),
        }
        let _ = self.push_line(J::Obj(m).text());
        Ok(())
    }

    fn admission_named(&self, key: &str) -> Option<Decision> {
        self.loaded()?.admissions.get(key).copied()
    }

    fn cancel_point(&self, branch: &str) -> Option<(u64, u64)> {
        self.loaded()?.cancels.get(branch).copied()
    }

    fn timeout_point(&self, branch: &str, scope: u32) -> Option<(u64, u64)> {
        self.loaded()?
            .timeouts
            .get(&(branch.to_owned(), scope))
            .copied()
    }

    fn skipped(&self, construct: &str, index: u32) -> bool {
        self.loaded()
            .is_some_and(|l| l.skipped.contains(&(construct.to_owned(), index)))
    }

    fn winner(&self, construct: &str) -> Option<u32> {
        self.loaded()?.winners.get(construct).copied()
    }

    fn first_failure(&self, construct: &str) -> Option<u32> {
        self.loaded()?.failures.get(construct).copied()
    }

    fn step_total(&self, branch: &str) -> Option<u64> {
        self.loaded()?.steps.get(branch).copied()
    }

    fn check_steps(&self, branch: &str, steps: u64) -> Result<(), String> {
        let Some(l) = self.loaded() else {
            return Ok(());
        };
        match l.steps.get(branch) {
            Some(&s) if s == steps => Ok(()),
            Some(&s) => Err(format!(
                "the branch `{branch}` took {steps} steps; the tape says {s}"
            )),
            None => Err(format!(
                "the tape has no step total for the branch `{branch}`"
            )),
        }
    }

    fn check_ended(&self, key: &EffectKey) -> Result<(), String> {
        let Some(l) = self.loaded() else {
            return Ok(());
        };
        let k = key.to_string();
        if l.ended.contains_key(&k) || l.reparented.contains(&k) {
            Ok(())
        } else {
            Err(format!(
                "the tape has no outcome for the cancelled call `{k}`"
            ))
        }
    }

    fn divergence(&self) -> Option<String> {
        lock(&self.rep).diverged.clone()
    }

    fn value_digest(&self, v: &Value) -> String {
        let text = codec::encode(v, false)
            .map(|j| j.text())
            .unwrap_or_default();
        self.digest(&text, crate::sink::check(v).is_err())
    }

    fn turn_end(&self, outcome: &Outcome, program: &Prepared) -> Result<(), String> {
        let text = match outcome {
            Outcome::Returned(v) => format!("ok:{}", crate::to_json_text(program, v)),
            Outcome::Raised(e) => format!("err:{}", crate::to_json_text(program, e.value())),
        };
        let labelled = match outcome {
            Outcome::Returned(v) => crate::sink::check(v).is_err(),
            Outcome::Raised(e) => crate::sink::check(e.value()).is_err(),
        };
        let digest = self.digest(&text, labelled);
        let Some(l) = self.loaded() else {
            let j = J::Obj(vec![("end".to_owned(), J::Str(digest))]);
            lock(&self.rec).end = Some(j.text());
            return Ok(());
        };
        if let Some(d) = self.divergence() {
            return Err(d);
        }
        match &l.reply {
            Some(r) if *r == digest => Ok(()),
            Some(_) => Err("the turn's reply differs from the tape's (reply digest)".to_owned()),
            None => Err("the tape has no reply digest".to_owned()),
        }
    }
}

// ---- loading and verification ----

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Parses a tape, within the bounds, and checks its blobs.
fn load(text: &str) -> Result<Loaded, String> {
    if text.len() > MAX_TAPE_BYTES {
        return Err(format!("larger than {MAX_TAPE_BYTES} bytes"));
    }
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = json::parse(lines.next().ok_or("empty")?)?;
    match header.int("tape") {
        Some(v) if v == i64::from(TAPE_VERSION) => {}
        v => {
            return Err(format!(
                "tape version {v:?}, but this library reads version {TAPE_VERSION}"
            ));
        }
    }
    match header.int("ir") {
        Some(v) if v == i64::from(ratio_ir::IR_VERSION) => {}
        v => {
            return Err(format!(
                "IR version {v:?}, but this library has IR version {}",
                ratio_ir::IR_VERSION
            ));
        }
    }
    let mut l = Loaded {
        seed: header
            .str("seed")
            .and_then(decode_hex32)
            .ok_or("a header without a seed")?,
        headless: matches!(header.get("headless"), Some(J::Bool(true))),
        mode: if header.str("mode") == Some("full") {
            RecordMode::Full
        } else {
            RecordMode::Default
        },
        root: header.str("root").unwrap_or("turn").to_owned(),
        root_caps: header
            .get("caps")
            .map(caps_from)
            .transpose()?
            .unwrap_or(Caps::NONE),
        root_width: u32::try_from(header.int("width").unwrap_or(16)).map_err(|_| "a bad width")?,
        ..Loaded::default()
    };
    for (n, line) in lines.enumerate() {
        if n >= MAX_TAPE_LINES {
            return Err("too many lines".to_owned());
        }
        let j = json::parse(line).map_err(|e| format!("line {}: {e}", n + 2))?;
        if j.get("truncated").is_some() {
            return Err(
                "recording stopped at the tape's size limit (truncated): the turn cannot be \
                 replayed"
                    .to_owned(),
            );
        }
        if let Some(name) = j.str("blob") {
            let data = j.str("data").map(str::to_owned);
            if let Some(d) = &data
                && blake3(d.as_bytes()).hex() != name
            {
                return Err(format!("the blob `{name}` does not match its digest"));
            }
            if data.is_none() && !name.starts_with("r:") {
                return Err(format!("the blob `{name}` has no data"));
            }
            l.blobs.insert(name.to_owned(), data);
        } else if let Some(key) = j.str("e") {
            let req = j
                .str("q")
                .ok_or("an entry without a request digest")?
                .to_owned();
            let res = if let Some(v) = j.get("v") {
                Res::Value(v.clone())
            } else if let Some(x) = j.get("x") {
                Res::Error(x.clone())
            } else if let Some(b) = j.str("b") {
                Res::Blob {
                    name: b.to_owned(),
                    labels: Labels::from_bits(
                        j.int("l").and_then(|x| u32::try_from(x).ok()).unwrap_or(0),
                    ),
                }
            } else {
                return Err("an entry without a response".to_owned());
            };
            if l.entries
                .insert(key.to_owned(), Entry { req, res })
                .is_some()
            {
                return Err(format!("two entries for `{key}`"));
            }
        } else if j.get("seq").is_some() {
            let seq = j.uint("seq").ok_or("a malformed sequence number")?;
            let ev = Event::from_j(&j)?;
            if seq != l.events.len() as u64 + 1 {
                // Numbered in order, from 1, without gaps: checked again by `verify`.
                l.events.push(ev);
                l.reply = Some(format!("seq:{seq}"));
                return Ok(l);
            }
            l.events.push(ev);
        } else if let Some(key) = j.str("out") {
            l.outputs
                .insert(key.to_owned(), j.str("d").unwrap_or_default().to_owned());
        } else if let Some(end) = j.str("end") {
            l.reply = Some(end.to_owned());
        } else {
            return Err(format!("line {}: not a tape line", n + 2));
        }
    }
    for e in l.entries.values() {
        if let Res::Blob { name, .. } = &e.res
            && !l.blobs.contains_key(name)
        {
            return Err(format!("the blob `{name}` is missing"));
        }
    }
    index(&mut l);
    Ok(l)
}

/// Builds the lookup tables replay asks.
fn index(l: &mut Loaded) {
    for ev in &l.events {
        match ev {
            Event::Frame {
                path, caps, width, ..
            } => {
                l.frames.insert(path.clone(), (*caps, *width));
            }
            Event::Admit { key, max, .. } => {
                l.admissions.insert(key.clone(), Decision::Admit(*max));
            }
            Event::Refuse { key, which, .. } => {
                if let Some(k) = BudgetKind::ALL.into_iter().find(|k| k.name() == which) {
                    l.admissions.insert(key.clone(), Decision::Refuse(k));
                }
            }
            Event::Steps { branch, steps } => {
                l.steps.insert(branch.clone(), *steps);
            }
            Event::CancelPoint {
                branch,
                effects,
                steps,
            } => {
                l.cancels.insert(branch.clone(), (*effects, *steps));
            }
            Event::Ended {
                key,
                outcome,
                digest,
            } => {
                l.ended
                    .insert(key.clone(), (outcome.clone(), digest.clone()));
            }
            Event::Reparent { key, .. } => {
                l.reparented.insert(key.clone());
            }
            Event::RaceWon { construct, index } => {
                l.winners.insert(construct.clone(), *index);
            }
            Event::ParFailed { construct, index } => {
                l.failures.insert(construct.clone(), *index);
            }
            Event::TimeoutFired {
                branch,
                scope,
                effects,
                steps,
            } => {
                l.timeouts
                    .insert((branch.clone(), *scope), (*effects, *steps));
            }
            Event::Skipped { construct, index } => {
                l.skipped.insert((construct.clone(), *index));
            }
            Event::Wait { .. } | Event::Complete { .. } => {}
        }
    }
}

/// Verifies the tape against itself: the turn sequence is numbered without gaps; replaying
/// its ledger events in sequence order into ledgers built from its frame events reproduces
/// every admission (its reservation and effective `max_tokens`), refusal and completion; the
/// branches' step totals stay within the turn's limit; and every cancellation outcome agrees
/// with the entries (a drained call has its discarded response and its digest; a call that
/// never ran has neither).
fn verify(l: &Loaded) -> Result<(), String> {
    if l.reply.as_deref().is_some_and(|r| r.starts_with("seq:")) {
        return Err("the turn sequence is not numbered in order".to_owned());
    }
    let (ledgers, root) = Ledgers::new(l.root_caps, l.root_width.max(1));
    let mut nodes: HashMap<&str, NodeId> = HashMap::new();
    nodes.insert(&l.root, root);
    let mut held: HashMap<&str, crate::frame::Reservation> = HashMap::new();
    let mut steps_sum: u64 = 0;
    let limit = l.root_caps.steps;
    for (i, ev) in l.events.iter().enumerate() {
        let at = || format!("event {}", i + 1);
        match ev {
            Event::Frame {
                path,
                parent,
                caps,
                width,
            } => {
                let p = *nodes
                    .get(parent.as_str())
                    .ok_or_else(|| format!("{}: a frame below an unknown frame", at()))?;
                let n = ledgers
                    .open(p, *caps, *width)
                    .ok_or_else(|| format!("{}: a frame below a closed frame", at()))?;
                nodes.insert(path, n);
            }
            Event::Admit {
                key,
                frame,
                held: h,
                max,
                model,
            } => {
                let n = *nodes
                    .get(frame.as_str())
                    .ok_or_else(|| format!("{}: an admission in an unknown frame", at()))?;
                let r = match model {
                    None => ledgers
                        .admit_tool(n)
                        .map_err(|_| format!("{}: `{key}` was not coverable", at()))?,
                    Some(call) => match ledgers.admit_model(n, call) {
                        crate::frame::Admission::Admitted(r) => r,
                        _ => return Err(format!("{}: `{key}` was not coverable", at())),
                    },
                };
                if r.held != h.0 || r.max_tokens != *max {
                    return Err(format!(
                        "{}: `{key}`'s reservation does not follow from its recorded inputs",
                        at()
                    ));
                }
                if held.insert(key, r).is_some() {
                    return Err(format!("{}: `{key}` admitted twice", at()));
                }
            }
            Event::Refuse {
                key,
                frame,
                which,
                model,
            } => {
                // A refusal must have been one: the same admission is refused now, by the
                // same budget.
                let n = *nodes
                    .get(frame.as_str())
                    .ok_or_else(|| format!("{}: a refusal in an unknown frame", at()))?;
                let refused = match model {
                    None => match ledgers.admit_tool(n) {
                        Ok(r) => {
                            let _ = ledgers.cancel(&r);
                            None
                        }
                        Err(r) => Some(r.which),
                    },
                    Some(call) => match ledgers.admit_model(n, call) {
                        crate::frame::Admission::Admitted(r) => {
                            let _ = ledgers.cancel(&r);
                            None
                        }
                        crate::frame::Admission::Refused(r) => Some(r.which),
                        crate::frame::Admission::Wait { .. } => None,
                    },
                };
                if refused.map(BudgetKind::name) != Some(which.as_str()) {
                    return Err(format!(
                        "{}: `{key}` is recorded refused by `{which}`, which its inputs do not give",
                        at()
                    ));
                }
            }
            Event::Complete { key, actual } => {
                let r = held
                    .remove(key.as_str())
                    .ok_or_else(|| format!("{}: `{key}` completed without an admission", at()))?;
                ledgers
                    .complete(&r, actual.0)
                    .map_err(|_| format!("{}: `{key}` completed twice", at()))?;
            }
            Event::Steps { steps, .. } => {
                steps_sum = steps_sum.saturating_add(*steps);
                if limit.is_some_and(|lim| steps_sum > lim) {
                    return Err(format!(
                        "{}: the branches' step totals exceed the turn's limit",
                        at()
                    ));
                }
            }
            Event::Ended {
                key,
                outcome,
                digest,
            } => {
                let entry = l.entries.get(key);
                match (outcome.as_str(), digest, entry) {
                    ("drained", Some(d), Some(e)) => {
                        let v = match &e.res {
                            Res::Value(j) => codec::decode(j).ok(),
                            Res::Blob { name, .. } => l
                                .blobs
                                .get(name)
                                .and_then(Option::as_ref)
                                .and_then(|t| json::parse(t).ok())
                                .and_then(|j| codec::decode(&j).ok()),
                            Res::Error(_) => None,
                        };
                        // A keyed digest (a labelled answer of a headless tape) can only be
                        // checked with the session's key; its body is redacted anyway.
                        let keyed = d.starts_with("k:")
                            && matches!(&e.res, Res::Blob { name, .. } if name.starts_with("r:"));
                        if !keyed && v.is_none_or(|v| value_digest(&v).hex() != *d) {
                            return Err(format!(
                                "{}: the drained call `{key}` does not match its response",
                                at()
                            ));
                        }
                    }
                    // Drained with a failure: the failure is its entry.
                    ("drained", None, Some(e)) if matches!(e.res, Res::Error(_)) => {}
                    // Never ran, or (`failed`, `unacknowledged`) was ended by the scheduler at
                    // its bound or grace.
                    (
                        "aborted" | "abandoned" | "withdrawn" | "failed" | "unacknowledged",
                        None,
                        None,
                    ) => {}
                    _ => {
                        return Err(format!(
                            "{}: the outcome of `{key}` disagrees with the tape's entries",
                            at()
                        ));
                    }
                }
            }
            Event::Wait { .. }
            | Event::CancelPoint { .. }
            | Event::Reparent { .. }
            | Event::RaceWon { .. }
            | Event::ParFailed { .. }
            | Event::TimeoutFired { .. }
            | Event::Skipped { .. } => {}
        }
    }
    // Every admitted call was completed, and has its answer or (cancelled) its outcome; every
    // re-parented call has its outcome: deleting an `ended` or `complete` line is caught.
    if let Some(key) = held.keys().next() {
        return Err(format!("`{key}` was admitted but never completed"));
    }
    for ev in &l.events {
        // The further tool calls of one effect (`key~i`, `dispatch_all`) share its answer
        // or outcome.
        let base = |k: &str| k.split_once('~').map_or(k, |(b, _)| b).to_owned();
        if let Event::Admit { key, .. } = ev
            && !l.entries.contains_key(&base(key))
            && !l.ended.contains_key(&base(key))
        {
            return Err(format!("`{key}` has neither an answer nor an outcome"));
        }
    }
    for key in &l.reparented {
        if !l.ended.contains_key(key) {
            return Err(format!("the re-parented call `{key}` has no outcome"));
        }
    }
    // Every outcome needs its call: a withdrawn call never ran, so it has no entry.
    for (key, (outcome, _)) in &l.ended {
        if outcome != "drained" && l.entries.contains_key(key) {
            return Err(format!("`{key}` is recorded {outcome} but has a response"));
        }
    }
    Ok(())
}
