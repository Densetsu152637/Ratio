//! The tape (stage 11a): `conformance/replay/`, record then replay under permuted resolution
//! orders, the determinism property under random completion orders with budgets just below
//! demand, every divergence mutation, suppression of output-only effects and the reply digest,
//! labels kept and gained in replay, blobs in headless tapes, crafted tapes and the tape's
//! size.

#![allow(
    clippy::many_single_char_names,
    reason = "programs, hosts and tapes named briefly"
)]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ratio_eval::frame::{Card, Location, Prices, Scope};
use ratio_eval::host::{Host, ModelCard, ToolClass};
use ratio_eval::scripted::{Behaviour, ScriptedHost};
use ratio_eval::tape::{RecordMode, RecordOptions, ReplayOptions, Tape, TapeHost};
use ratio_eval::{CancelToken, Finished, Labels, Limits, Outcome, Prepared, Value};

type Setup = fn(&ScriptedHost);

fn fixture(name: &str) -> Setup {
    match name {
        "reads" => |h| {
            h.tool(
                "read_file",
                ToolClass::abort(false),
                Behaviour::Reply(Value::text("text")),
            );
        },
        "fail-first-b" => |h| {
            h.tool(
                "read_file",
                ToolClass::abort(false),
                Behaviour::PerArg(vec![
                    ("a".into(), vec![Ok(Value::text("A"))]),
                    (
                        "b".into(),
                        vec![Err("transient".into()), Ok(Value::text("B"))],
                    ),
                    ("c".into(), vec![Ok(Value::text("C"))]),
                ]),
            );
        },
        "slow-reads" => |h| {
            // The failing branch makes one read; its sibling makes three in a row.
            h.tool(
                "read_file",
                ToolClass::abort(false),
                Behaviour::After(Duration::from_millis(40), Value::text("s")),
            );
        },
        "everything" => everything,
        other => panic!("no fixture `{other}`"),
    }
}

fn everything(h: &ScriptedHost) {
    let big = Value::text(&"x".repeat(3000));
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::PerArg(vec![
            ("big".into(), vec![Ok(big)]),
            ("a".into(), vec![Ok(Value::text("A"))]),
            ("b".into(), vec![Ok(Value::text("B"))]),
            ("c".into(), vec![Ok(Value::text("C"))]),
        ]),
    )
    .tool(
        "write_file",
        ToolClass::drain(Duration::from_secs(60)),
        Behaviour::After(Duration::from_millis(60), Value::text("written")),
    )
    .tool(
        "needs_approval",
        ToolClass::drain(Duration::from_secs(60)),
        Behaviour::Approval(Value::unit()),
    )
    .card(ModelCard {
        name: Arc::from("m"),
        location: Location::Local,
        card: Card {
            max_output: Some(100),
            window: Some(8000),
            prices: Some(Prices {
                input: 500_000_000,
                output: 1_000_000_000,
            }),
            local: false,
        },
    })
    .reply("hello", Labels::NONE);
}

fn host(setup: Setup, jitter: u64) -> Arc<ScriptedHost> {
    let h = Arc::new(ScriptedHost::new("turn", Scope::session()));
    setup(&h);
    h.jitter(jitter);
    h
}

fn start(
    p: &Arc<Prepared>,
    h: Arc<dyn Host>,
    session: &ratio_eval::frame::Session,
) -> ratio_eval::Evaluation {
    let entry = p.find("main.ratio", "main").expect("a main");
    ratio_eval::evaluate_with(
        Arc::clone(p),
        entry,
        Vec::new(),
        Limits::default(),
        CancelToken::new(),
        session,
        h,
    )
    .expect("starts")
}

/// Records `main` on a host set up by `setup`; returns the run, the tape and the host.
fn record(
    p: &Arc<Prepared>,
    setup: Setup,
    jitter: u64,
    opts: RecordOptions,
) -> (Finished, String, Arc<TapeHost>, Arc<ScriptedHost>) {
    let inner = host(setup, jitter);
    let session = inner.session();
    let tape = Arc::new(TapeHost::record(Arc::clone(&inner) as Arc<dyn Host>, opts));
    let eval = start(p, Arc::clone(&tape) as Arc<dyn Host>, &session)
        .with_tape(Arc::clone(&tape) as Arc<dyn Tape>);
    let f = ratio_eval::block_on(eval);
    let text = tape.tape();
    (f, text, tape, inner)
}

/// Replays `text`; `Err` when the tape does not load.
fn replay(
    p: &Arc<Prepared>,
    setup: Setup,
    text: &str,
    permute: Option<u64>,
) -> Result<(Finished, Arc<TapeHost>, Arc<ScriptedHost>), String> {
    let inner = host(setup, 0);
    let session = inner.session();
    let tape = Arc::new(
        TapeHost::replay(
            Arc::clone(&inner) as Arc<dyn Host>,
            text,
            ReplayOptions {
                lenient: false,
                permute,
            },
        )
        .map_err(|e| e.to_string())?,
    );
    let eval = start(p, Arc::clone(&tape) as Arc<dyn Host>, &session)
        .with_tape(Arc::clone(&tape) as Arc<dyn Tape>);
    Ok((ratio_eval::block_on(eval), tape, inner))
}

fn shown(p: &Prepared, f: &Finished) -> String {
    match &f.outcome {
        Outcome::Returned(v) => ratio_eval::to_json_text(p, v),
        Outcome::Raised(e) => format!("raise {} {}", e.kind(p), e.message().unwrap_or_default()),
    }
}

fn kind(p: &Prepared, f: &Finished) -> String {
    match &f.outcome {
        Outcome::Returned(_) => "returned".into(),
        Outcome::Raised(e) => e.kind(p),
    }
}

fn is_replay(
    p: &Prepared,
    r: &Result<(Finished, Arc<TapeHost>, Arc<ScriptedHost>), String>,
) -> bool {
    match r {
        Err(_) => true,
        Ok((f, _, _)) => kind(p, f) == "Replay",
    }
}

#[test]
fn conformance_replay_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/replay");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the replay corpus")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "ratio"))
        .collect();
    files.sort();
    assert!(files.len() >= 4);
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let header = |d: &str| {
            source
                .lines()
                .find_map(|l| l.strip_prefix(&format!("// {d}: ")))
                .map(str::to_owned)
        };
        let expect = header("expect").expect("an expectation");
        let setup = fixture(&header("fixture").expect("a fixture"));
        let p = support::prepared(&source);
        for seed in 1..=4u64 {
            let (f, text, rec, _) = record(&p, setup, seed * 7919, RecordOptions::default());
            let got = shown(&p, &f);
            if let Some(v) = expect.strip_prefix("value ") {
                assert_eq!(got, v.replace(", ", ","), "{name} (record, seed {seed})");
            } else if let Some(k) = expect.strip_prefix("runtime-error ") {
                assert_eq!(kind(&p, &f), k, "{name} (record, seed {seed}): {got}");
            }
            for permute in [seed, seed + 100, seed + 1000] {
                let (r, tape, _) = replay(&p, setup, &text, Some(permute))
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(shown(&p, &r), got, "{name} (replay {permute})\n{text}");
                assert_eq!(tape.events(), rec.events(), "{name}: the event stream");
            }
            if name == "crafted-tape-steps.ratio" {
                crafted_steps(&p, setup, &text);
            }
        }
    }
}

/// A tape with the steps exhaustion removed, or step totals summing above the limit.
fn crafted_steps(p: &Arc<Prepared>, setup: Setup, text: &str) {
    let steps: Vec<(usize, u64)> = text
        .match_indices("\"steps\":")
        .filter_map(|(i, m)| {
            let rest = &text[i + m.len()..];
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let from = text[..i].rfind('\n').map_or(0, |x| x + 1);
            let to = text[i..].find('\n').map_or(text.len(), |x| i + x);
            let line = &text[from..to];
            (line.starts_with("{\"seq\"") && line.contains("\"ev\":\"steps\"") && !n.is_empty())
                .then(|| (i + m.len(), n.parse().unwrap()))
        })
        .collect();
    assert_eq!(steps.len(), 2, "two branch totals");
    // The exhausted branch is the one with fewer steps: claim it ran to the other's total.
    let (lo, hi) = if steps[0].1 < steps[1].1 {
        (steps[0], steps[1])
    } else {
        (steps[1], steps[0])
    };
    let removed = format!(
        "{}{}{}",
        &text[..lo.0],
        hi.1,
        &text[lo.0 + lo.1.to_string().len()..]
    );
    assert!(
        is_replay(p, &replay(p, setup, &removed, None)),
        "an exhaustion removed"
    );
    let over = format!(
        "{}{}{}",
        &text[..hi.0],
        40_000,
        &text[hi.0 + hi.1.to_string().len()..]
    );
    assert!(
        is_replay(p, &replay(p, setup, &over, None)),
        "totals above the limit"
    );
}

const EVERYTHING: &str = r#"
fn reads() -> Str {
  try {
    with { budget: { tool_calls: 2 } } {
      par { tools.read_file("a"); tools.read_file("b"); tools.read_file("c") }
    }
    "all"
  } catch e { "refused" }
}

fn modelled() -> Untrusted<Str> {
  with { budget: { tokens: 5000, cost: $10 } } {
    model.generate(prompt { task "hi" }, opts: { model: "m" })
  }
}

fn drained() -> Str { race { { tools.write_file("/w/a", "slow"); "w" }; "fast" } }

fn withdrawn() -> Str {
  race { { tools.call_tool("needs_approval", json.parse("1")); "a" }; "fast" }
}

fn big() -> Str { tools.read_file("big"); "big" }

fn spin(n: Int) -> Int {
  var i = 0
  while i < n { i = i + 1 }
  i
}

fn main() -> (Str, Untrusted<Str>, Str, Str, Str, (Int, Int)) {
  let w = withdrawn()
  (reads(), modelled(), drained(), w, big(), par { spin(1500); spin(2500) })
}
"#;

fn mutate(text: &str, find: &str, with: &str) -> String {
    assert!(text.contains(find), "the tape has `{find}`:\n{text}");
    text.replacen(find, with, 1)
}

/// The value after `"key":` on the first line holding `on`.
fn field(text: &str, on: &str, key: &str) -> String {
    let line = text
        .lines()
        .find(|l| l.contains(on))
        .unwrap_or_else(|| panic!("no `{on}`"));
    let i = line
        .find(&format!("\"{key}\":"))
        .unwrap_or_else(|| panic!("no `{key}` in {line}"));
    line[i + key.len() + 3..]
        .chars()
        .take_while(|c| *c != ',' && *c != '}')
        .collect()
}

#[test]
fn every_divergence_mutation_raises_replay() {
    let p = support::prepared(EVERYTHING);
    let (f, text, _, _) = record(&p, everything, 5, RecordOptions::default());
    let out = shown(&p, &f);
    assert!(out.contains("refused") && out.contains("fast"), "{out}");
    // A faithful replay passes.
    let ok = replay(&p, everything, &text, Some(9)).unwrap();
    assert_eq!(shown(&p, &ok.0), out);

    let line_of = |on: &str| text.lines().find(|l| l.contains(on)).unwrap().to_owned();
    let mut cases: Vec<(&str, String)> = Vec::new();
    // A sequence number.
    cases.push((
        "sequence number",
        mutate(&text, "{\"seq\":3,", "{\"seq\":4,"),
    ));
    // A refusal turned into another budget's.
    let refuse = line_of("\"ev\":\"refuse\"");
    cases.push((
        "refusal",
        text.replacen(&refuse, &refuse.replace("tool_calls", "tokens"), 1),
    ));
    // The model admission's recorded input count, card limit and price.
    let admit = line_of("\"model\":{");
    let input = field(&text, "\"model\":{", "input");
    cases.push((
        "input count",
        text.replacen(
            &admit,
            &admit.replacen(
                &format!("\"input\":{input}"),
                &format!("\"input\":{}", input.parse::<u64>().unwrap() + 40),
                1,
            ),
            1,
        ),
    ));
    cases.push((
        "card limit",
        text.replacen(
            &admit,
            &admit.replacen("\"max_output\":100", "\"max_output\":60", 1),
            1,
        ),
    ));
    cases.push((
        "price",
        text.replacen(
            &admit,
            &admit.replacen("\"price_in\":500000000", "\"price_in\":900000000", 1),
            1,
        ),
    ));
    // A step total.
    let steps = line_of("\"ev\":\"steps\"");
    let n = field(&text, "\"ev\":\"steps\"", "steps");
    cases.push((
        "step total",
        text.replacen(
            &steps,
            &steps.replacen(
                &format!("\"steps\":{n}"),
                &format!("\"steps\":{}", n.parse::<u64>().unwrap() + 1),
                1,
            ),
            1,
        ),
    ));
    // The drained and the withdrawn outcomes.
    let drained = line_of("\"outcome\":\"drained\"");
    cases.push((
        "drained entry",
        text.replacen(&drained, &drained.replace("drained", "withdrawn"), 1),
    ));
    let withdrawn = line_of("\"outcome\":\"withdrawn\"");
    cases.push((
        "withdrawn entry",
        text.replacen(&withdrawn, &withdrawn.replace("withdrawn", "drained"), 1),
    ));
    // A blob's body.
    cases.push(("blob", mutate(&text, "xxxxxxxxxx", "xxxxxyxxxx")));
    for (what, t) in cases {
        assert_ne!(t, text, "{what}: the mutation changed nothing");
        let r = replay(&p, everything, &t, Some(3));
        assert!(
            is_replay(&p, &r),
            "{what}: {:?}",
            r.as_ref().map(|r| shown(&p, &r.0))
        );
    }
}

const DETERMINISM: &str = r#"
fn work(i: Int) -> Str {
  tools.read_file("a{i}")
  tools.read_file("b{i}")
  "done {i}"
}

fn main() -> [Result<Str>] {
  with { budget: { tool_calls: 10 } } {
    par_map_settled([1, 2, 3, 4, 5, 6], |i: Int| work(i), limit: 6)
  }
}
"#;

#[test]
fn replay_is_deterministic_under_random_completion_orders_with_budgets_below_demand() {
    let p = support::prepared(DETERMINISM);
    let setup: Setup = |h| {
        h.tool(
            "read_file",
            ToolClass::abort(false),
            Behaviour::Reply(Value::text("t")),
        );
    };
    for seed in 1..=16u64 {
        let (f, text, _, _) = record(&p, setup, seed * 104_729, RecordOptions::default());
        let out = shown(&p, &f);
        // Twelve calls under a cap of ten: two branches are refused, whichever they are.
        assert_eq!(out.matches("\"tool_calls\"").count(), 2, "{out}");
        for permute in [seed + 1, seed * 31 + 7] {
            let (r, _, _) = replay(&p, setup, &text, Some(permute)).unwrap();
            assert_eq!(shown(&p, &r), out, "seed {seed}, permutation {permute}");
        }
    }
}

const OUTPUT: &str = r#"
fn main() -> Str {
  session.emit("progress", json.parse("1"))
  "reply"
}
"#;

#[test]
fn output_only_effects_are_suppressed_in_replay_and_checked_by_the_reply_digest() {
    let p = support::prepared(OUTPUT);
    let setup: Setup = |_| {};
    let (f, text, _, inner) = record(&p, setup, 0, RecordOptions::default());
    assert_eq!(shown(&p, &f), "\"reply\"");
    assert_eq!(inner.events().len(), 1);
    assert!(
        !text.contains("\"out\""),
        "no entry for an output-only effect by default"
    );
    let (r, tape, live) = replay(&p, setup, &text, None).unwrap();
    assert_eq!(shown(&p, &r), "\"reply\"");
    assert!(live.events().is_empty(), "replay makes no live emit");
    assert_eq!(tape.suppressed().len(), 1);

    // Altered in the output effect and the reply: the reply digest catches it.
    let altered = support::prepared(
        &OUTPUT
            .replace("\"1\"", "\"2\"")
            .replace("\"reply\"", "\"other\""),
    );
    let r = replay(&altered, setup, &text, None);
    assert!(is_replay(&altered, &r));

    // In full mode, a change only to the output effect is caught where it happens.
    let (_, full, _, _) = record(
        &p,
        setup,
        0,
        RecordOptions {
            mode: RecordMode::Full,
            ..RecordOptions::default()
        },
    );
    assert!(full.contains("\"out\""));
    let only_output = support::prepared(&OUTPUT.replace("\"1\"", "\"2\""));
    assert!(is_replay(
        &only_output,
        &replay(&only_output, setup, &full, None)
    ));
    assert!(!is_replay(&p, &replay(&p, setup, &full, None)));
}

const LEAK: &str = r#"
fn main() -> Str {
  let t = tools.read_file("/w/.env")
  tools.web_fetch(text.show(t))
  "sent"
}
"#;

fn leak_setup(h: &ScriptedHost) {
    h.tool(
        "read_file",
        ToolClass::abort(false),
        Behaviour::Reply(Value::text("KEY=1")),
    )
    .tool(
        "web_fetch",
        ToolClass::abort(true),
        Behaviour::Reply(Value::text("page")),
    );
}

fn secret_setup(h: &ScriptedHost) {
    leak_setup(h);
    h.secret_path("*.env");
}

#[test]
fn replayed_values_keep_their_labels_and_gain_the_replaying_rules() {
    let p = support::prepared(LEAK);
    // Recorded with the secret path configured: the read is labelled and the fetch refused;
    // replayed without it, the recorded label is kept and the fetch refused again.
    let (f, text, _, _) = record(&p, secret_setup, 0, RecordOptions::default());
    assert!(shown(&p, &f).contains("Denied"), "{}", shown(&p, &f));
    let (r, _, _) = replay(&p, leak_setup, &text, None).unwrap();
    assert_eq!(shown(&p, &r), shown(&p, &f));
    // Recorded without it: the fetch was sent. Replayed with it: the replayed read gains the
    // label and the fetch is refused (the run then diverges from its tape).
    let (f, text, _, _) = record(&p, leak_setup, 0, RecordOptions::default());
    assert_eq!(shown(&p, &f), "\"sent\"");
    let (r, _, _) = replay(&p, secret_setup, &text, None).unwrap();
    assert!(matches!(r.outcome, Outcome::Raised(_)), "{}", shown(&p, &r));
}

const BIG_SECRET: &str = r#"
fn main() -> Str {
  tools.read_file("/w/.env")
  tools.read_file("/w/small.env")
  "read"
}
"#;

#[test]
fn a_labelled_blob_in_a_headless_tape_is_named_by_the_redaction_digest_only() {
    let p = support::prepared(BIG_SECRET);
    let secret = "SECRET-".repeat(400);
    let setup: Setup = |h| {
        h.tool(
            "read_file",
            ToolClass::abort(false),
            Behaviour::PerArg(vec![
                (
                    "/w/.env".into(),
                    vec![Ok(Value::text(&"SECRET-".repeat(400)))],
                ),
                ("/w/small.env".into(), vec![Ok(Value::text("TOKEN=7"))]),
            ]),
        )
        .secret_path("*.env");
    };
    let opts = RecordOptions {
        headless: true,
        redaction_key: [9; 32],
        ..RecordOptions::default()
    };
    let (f, text, _, _) = record(&p, setup, 0, opts);
    assert_eq!(shown(&p, &f), "\"read\"");
    assert!(
        !text.contains("SECRET-") && !text.contains("TOKEN=7"),
        "{text}"
    );
    assert_eq!(text.matches("\"blob\":\"r:").count(), 2, "{text}");
    // No plain hash of the body anywhere.
    let body = format!("[\"L\",1,[\"s\",\"{secret}\"]]");
    let plain =
        ratio_eval::tape::hex(&ratio_eval::tape::redaction_digest(&[0; 32], body.as_bytes()).0);
    assert!(!text.contains(&plain));
    // Replaying it needs lenient mode.
    assert!(is_replay(&p, &replay(&p, setup, &text, None)));
}

#[test]
fn crafted_tapes_never_crash_or_hang_the_replayer() {
    let p = support::prepared(OUTPUT);
    let setup: Setup = |_| {};
    let header = "{\"tape\":1,\"ratio\":\"0.1.0\",\"ir\":1,\"mode\":\"default\",\"headless\":false,\"seed\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"root\":\"turn\",\"caps\":{},\"width\":16}";
    let deep = format!("{header}\n{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let long_number = format!("{header}\n{{\"seq\":{}}}", "9".repeat(10_000));
    let unterminated = format!("{header}\n{{\"e\":\"k\",\"q\":\"x");
    let huge_count = format!(
        "{header}\n{{\"seq\":1,\"ev\":\"steps\",\"branch\":\"b\",\"steps\":18446744073709551615}}"
    );
    let negative = format!(
        "{header}\n{{\"seq\":1,\"ev\":\"admit\",\"key\":\"k\",\"frame\":\"nowhere\",\"held\":{{\"tool_calls\":-1}},\"max\":null}}"
    );
    let start = std::time::Instant::now();
    for t in [
        "",
        "not json",
        "{}",
        &deep,
        &long_number,
        &unterminated,
        &huge_count,
        &negative,
    ] {
        let r = replay(&p, setup, t, None);
        assert!(is_replay(&p, &r));
    }
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn tape_size_regression() {
    let p = support::prepared(DETERMINISM);
    let setup: Setup = |h| {
        h.tool(
            "read_file",
            ToolClass::abort(false),
            Behaviour::Reply(Value::text("t")),
        );
    };
    let (_, text, _, _) = record(&p, setup, 0, RecordOptions::default());
    // Measured at 5,4xx bytes when written; a format change that grows tapes shows here.
    assert!(text.len() < 7_000, "the tape grew to {} bytes", text.len());
}
