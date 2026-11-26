//! The budget ledgers (chapter 04, "Admission by bounded reservation"; stage 10): the
//! effective `max_tokens` formula, the reply floor, unpriced hosted models under a cost cap,
//! and concurrent admissions that never overspend a cap, in a frame or in a child frame with a
//! lower cap, under deterministic interleavings and under real threads.

use std::sync::Arc;
use std::thread;

use ratio_eval::frame::{
    Admission, Amounts, BudgetKind, Caps, Card, LedgerError, Ledgers, ModelCall, NodeId, Prices,
    Refused,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn tokens(cap: u64) -> Caps {
    Caps {
        tokens: Some(cap),
        ..Caps::NONE
    }
}

fn call(input: u64, max_tokens: Option<u64>) -> ModelCall {
    ModelCall {
        input,
        max_tokens,
        card: Card {
            max_output: Some(4_096),
            window: Some(32_000),
            prices: Some(Prices::default()),
            local: false,
        },
        min_reply: 256,
    }
}

fn admitted(a: Admission) -> ratio_eval::frame::Reservation {
    match a {
        Admission::Admitted(r) => r,
        other => panic!("expected an admission, got {other:?}"),
    }
}

#[test]
fn the_max_tokens_sent_follows_the_formula() {
    for seed in 1..=5_000u64 {
        let mut r = Rng(seed);
        let (ledgers, root) = Ledgers::new(tokens(5_000 + r.below(50_000)), 16);
        let child = ledgers.open(root, tokens(r.below(20_000)), 16).unwrap();
        // Some spend and some held reservations first.
        let mut held = Vec::new();
        for _ in 0..r.below(4) {
            if let Admission::Admitted(res) =
                ledgers.admit_model(child, &call(r.below(500), Some(1 + r.below(2_000))))
            {
                held.push(res);
            }
        }
        if let Some(first) = held.pop() {
            assert!(
                ledgers
                    .complete(
                        &first,
                        Amounts {
                            tokens: first.held.tokens / 2,
                            model_calls: 1,
                            ..Amounts::default()
                        }
                    )
                    .is_ok()
            );
        }
        let input = r.below(3_000);
        let requested = (r.below(3) > 0).then(|| 1 + r.below(8_000));
        let max_output = (r.below(4) > 0).then(|| 1 + r.below(8_000));
        let window = 1 + r.below(40_000);
        let c = ModelCall {
            input,
            max_tokens: requested,
            card: Card {
                max_output,
                window: Some(window),
                prices: Some(Prices::default()),
                local: false,
            },
            min_reply: r.below(600),
        };
        let left = ledgers.remaining(child).tokens.unwrap();
        let model_max = max_output.unwrap_or(window.saturating_sub(input));
        let after_input = left.checked_sub(input);
        let want = after_input.map(|room| requested.unwrap_or(u64::MAX).min(model_max).min(room));
        let floor = c
            .min_reply
            .min(requested.unwrap_or(u64::MAX))
            .min(model_max)
            .max(1);
        let got = ledgers.admit_model(child, &c);
        match (want, got) {
            (Some(e), Admission::Admitted(res)) => {
                assert!(e >= floor, "seed {seed}: admitted below the floor");
                assert_eq!(res.max_tokens, Some(e), "seed {seed}");
                assert_eq!(res.held.tokens, input + e, "seed {seed}");
            }
            (Some(e), Admission::Wait { .. }) => {
                assert!(e < floor && ledgers.held() > 0, "seed {seed}");
            }
            (None, Admission::Wait { .. }) => assert!(ledgers.held() > 0, "seed {seed}"),
            (Some(e), Admission::Refused(f)) => {
                assert!(e < floor && ledgers.held() == 0, "seed {seed}");
                assert_eq!(f.which, BudgetKind::Tokens);
            }
            (None, Admission::Refused(f)) => {
                assert_eq!(ledgers.held(), 0, "seed {seed}");
                assert_eq!(f.which, BudgetKind::Tokens);
            }
            (w, g) => panic!("seed {seed}: expected {w:?}, got {g:?}"),
        }
    }
}

#[test]
fn max_tokens_one_is_admitted_with_one_token_whatever_the_floor() {
    for min_reply in [0, 1, 256, 10_000] {
        let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
        let mut c = call(100, Some(1));
        c.min_reply = min_reply;
        let res = admitted(ledgers.admit_model(root, &c));
        assert_eq!(res.max_tokens, Some(1));
        assert_eq!(res.held.tokens, 101);
    }
    // Even when only 1 token is left after the input.
    let (ledgers, root) = Ledgers::new(tokens(101), 16);
    let res = admitted(ledgers.admit_model(root, &call(100, Some(1))));
    assert_eq!(res.max_tokens, Some(1));
}

#[test]
fn below_the_floor_an_admission_waits_for_a_held_reservation_and_is_refused_without_one() {
    let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
    // A sibling holds 100 + 800.
    let sibling = admitted(ledgers.admit_model(root, &call(100, Some(800))));
    // 1,000 − 900 − 50 = 50 tokens of room: below the floor of 256, and a reservation is held.
    let Admission::Wait { epoch, node } = ledgers.admit_model(root, &call(50, None)) else {
        panic!("expected a wait");
    };
    assert_eq!(node, root);
    assert_eq!(epoch, ledgers.releases());
    // The sibling completes having used 100 + 300.
    assert!(
        ledgers
            .complete(
                &sibling,
                Amounts {
                    tokens: 400,
                    model_calls: 1,
                    ..Amounts::default()
                }
            )
            .is_ok()
    );
    // The epoch moved: a waiter that read it before the release retries at once.
    assert_ne!(ledgers.wait_for_release(epoch), epoch);
    let res = admitted(ledgers.admit_model(root, &call(50, None)));
    assert_eq!(res.max_tokens, Some(550));
    assert!(
        ledgers
            .complete(
                &res,
                Amounts {
                    tokens: 600,
                    model_calls: 1,
                    ..Amounts::default()
                }
            )
            .is_ok()
    );
    // Nothing held now, and 0 tokens of room: refused, naming the ledger.
    assert_eq!(
        ledgers.admit_model(root, &call(0, None)),
        Admission::Refused(Refused {
            which: BudgetKind::Tokens,
            node: root
        })
    );
    // Input that cannot be covered with nothing held: refused too.
    let (ledgers, root) = Ledgers::new(tokens(100), 16);
    assert!(matches!(
        ledgers.admit_model(root, &call(500, Some(10))),
        Admission::Refused(Refused {
            which: BudgetKind::Tokens,
            ..
        })
    ));
}

#[test]
fn an_unpriced_hosted_model_is_refused_under_any_cost_cap() {
    let unpriced = |local: bool| ModelCall {
        card: Card {
            prices: None,
            local,
            ..call(10, Some(10)).card
        },
        ..call(10, Some(10))
    };
    let cost = Caps {
        cost: Some(1_000_000),
        ..Caps::NONE
    };
    // The cap on the session's ledger, on the frame's, or on an ancestor's.
    for (root_caps, frame_caps) in [(cost, Caps::NONE), (Caps::NONE, cost)] {
        let (ledgers, root) = Ledgers::new(root_caps, 16);
        let mid = ledgers.open(root, frame_caps, 16).unwrap();
        let leaf = ledgers.open(mid, Caps::NONE, 16).unwrap();
        match ledgers.admit_model(leaf, &unpriced(false)) {
            Admission::Refused(f) => assert_eq!(f.which, BudgetKind::Cost),
            other => panic!("expected a cost refusal, got {other:?}"),
        }
        // A local model prices at zero; a configured price of 0 counts as priced.
        let _ = admitted(ledgers.admit_model(leaf, &unpriced(true)));
        let _ = admitted(ledgers.admit_model(leaf, &call(10, Some(10))));
    }
    // Without any cost cap, an unpriced model is admitted.
    let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
    let _ = admitted(ledgers.admit_model(root, &unpriced(false)));
}

#[test]
fn cost_is_reserved_at_the_cards_prices() {
    // $2 / $8 per million tokens; a cap of $0.01 = 10,000 micro-units.
    let prices = Prices {
        input: 2_000_000,
        output: 8_000_000,
    };
    let (ledgers, root) = Ledgers::new(
        Caps {
            cost: Some(10_000),
            ..Caps::NONE
        },
        16,
    );
    let mut c = call(1_000, None);
    c.card.prices = Some(prices);
    let res = admitted(ledgers.admit_model(root, &c));
    // Input costs 2,000; 8,000 left buys 1,000 output tokens, under the card's 4,096.
    assert_eq!(res.max_tokens, Some(1_000));
    assert_eq!(res.held.cost, 10_000);
}

/// One branch of the interleaving harness: in a frame, making `left` model calls.
struct Branch {
    node: NodeId,
    left: u32,
    holding: Option<ratio_eval::frame::Reservation>,
    waits: u32,
    refused: u32,
}

/// Checks that no capped ledger has more spent and held than its cap.
fn assert_never_over(ledgers: &Ledgers, nodes: &[NodeId], ctx: &str) {
    for &n in nodes {
        let caps = ledgers.caps(n);
        let spent = ledgers.spent(n);
        let held = ledgers.reserved(n);
        if let Some(cap) = caps.tokens {
            assert!(
                spent.tokens + held.tokens <= cap,
                "{ctx}: tokens over the cap of {n:?}"
            );
        }
        if let Some(cap) = caps.cost {
            assert!(
                spent.cost + held.cost <= cap,
                "{ctx}: cost over the cap of {n:?}"
            );
        }
        if let Some(cap) = caps.model_calls {
            assert!(
                spent.model_calls + held.model_calls <= cap,
                "{ctx}: calls over {n:?}"
            );
        }
    }
}

#[test]
fn concurrent_model_calls_never_exceed_a_token_cap_under_any_interleaving() {
    let (mut waited, mut refused) = (0u32, 0u32);
    for seed in 1..=3_000u64 {
        let mut r = Rng(seed);
        let prices = Prices {
            input: r.below(3_000_000),
            output: r.below(9_000_000),
        };
        let (ledgers, root) = Ledgers::new(
            Caps {
                tokens: Some(2_000 + r.below(20_000)),
                cost: (r.below(2) == 0).then(|| 5_000 + r.below(100_000)),
                ..Caps::NONE
            },
            16,
        );
        // Child frames with lower caps (and a grandchild), as `with { budget }` and harness
        // calls make them.
        let low = ledgers.open(root, tokens(r.below(6_000)), 16).unwrap();
        let mid = ledgers.open(root, Caps::NONE, 16).unwrap();
        let lower = ledgers.open(mid, tokens(r.below(3_000)), 16).unwrap();
        let nodes = [root, low, mid, lower];
        let mut branches: Vec<Branch> = (0..2 + r.below(6))
            .map(|_| Branch {
                node: nodes[usize::try_from(r.below(4)).unwrap()],
                left: 1 + u32::try_from(r.below(5)).unwrap(),
                holding: None,
                waits: 0,
                refused: 0,
            })
            .collect();
        let mut steps = 0;
        while branches.iter().any(|b| b.left > 0 || b.holding.is_some()) {
            steps += 1;
            assert!(steps < 100_000, "seed {seed}: no progress");
            let i = usize::try_from(r.below(branches.len() as u64)).unwrap();
            let br = &mut branches[i];
            if let Some(res) = br.holding.take() {
                // The reply: never more than the effective max_tokens it was sent with.
                let out = res.max_tokens.map_or(r.below(5_000), |m| r.below(m + 1));
                let input = res.held.tokens - res.max_tokens.unwrap_or(0);
                let actual = Amounts {
                    tokens: input + out,
                    cost: res.held.cost.min(r.below(res.held.cost + 1)),
                    model_calls: 1,
                    ..Amounts::default()
                };
                assert!(ledgers.complete(&res, actual).is_ok());
                br.left -= 1;
            } else if br.left > 0 {
                let mut c = call(
                    r.below(1_500),
                    (r.below(2) == 0).then(|| 1 + r.below(3_000)),
                );
                c.card.prices = Some(prices);
                c.min_reply = r.below(400);
                match ledgers.admit_model(br.node, &c) {
                    Admission::Admitted(res) => br.holding = Some(res),
                    Admission::Wait { epoch, node } => {
                        br.waits += 1;
                        assert!(
                            ledgers.held_on(node) > 0,
                            "seed {seed}: waits on nothing held"
                        );
                        assert_eq!(epoch, ledgers.releases());
                    }
                    Admission::Refused(f) => {
                        assert_eq!(
                            ledgers.held_on(f.node),
                            0,
                            "seed {seed}: refused while the binding ledger held reservations"
                        );
                        br.refused += 1;
                        br.left -= 1;
                    }
                }
            }
            assert_never_over(&ledgers, &nodes, &format!("seed {seed}"));
        }
        assert_eq!(ledgers.held(), 0);
        for n in nodes {
            assert_eq!(ledgers.reserved(n), Amounts::default(), "seed {seed}");
        }
        waited += branches.iter().map(|b| b.waits).sum::<u32>();
        refused += branches.iter().map(|b| b.refused).sum::<u32>();
    }
    // The harness reaches the floor's waits and the refusals, not only easy admissions.
    assert!(
        waited > 100 && refused > 100,
        "waits {waited}, refusals {refused}"
    );
}

#[test]
fn concurrent_model_calls_on_threads_never_exceed_any_cap() {
    let (ledgers, root) = Ledgers::new(tokens(200_000), 16);
    let child = ledgers.open(root, tokens(30_000), 16).unwrap();
    let nodes = [root, child];
    let mut handles = Vec::new();
    for t in 0..8u64 {
        let ledgers = ledgers.clone();
        let node = if t % 2 == 0 { root } else { child };
        handles.push(thread::spawn(move || {
            let mut r = Rng(t + 1);
            let mut done = 0;
            let mut tries = 0;
            while done < 200 && tries < 100_000 {
                tries += 1;
                match ledgers.admit_model(node, &call(r.below(800), Some(1 + r.below(1_200)))) {
                    Admission::Admitted(res) => {
                        let out = r.below(res.max_tokens.unwrap_or(0) + 1);
                        let input = res.held.tokens - res.max_tokens.unwrap_or(0);
                        thread::yield_now();
                        assert!(
                            ledgers
                                .complete(
                                    &res,
                                    Amounts {
                                        tokens: input + out,
                                        model_calls: 1,
                                        ..Amounts::default()
                                    }
                                )
                                .is_ok()
                        );
                        done += 1;
                    }
                    // Blocks until a release; one between the admission and the wait is not
                    // lost, so this never hangs.
                    Admission::Wait { epoch, .. } => {
                        ledgers.wait_for_release(epoch);
                    }
                    Admission::Refused(_) => done += 1,
                }
            }
        }));
    }
    let watcher = {
        let ledgers = ledgers.clone();
        thread::spawn(move || {
            for _ in 0..2_000 {
                assert_never_over(&ledgers, &nodes, "threads");
                thread::yield_now();
            }
        })
    };
    for h in handles {
        h.join().unwrap();
    }
    watcher.join().unwrap();
    assert_never_over(&ledgers, &nodes, "threads, at the end");
    assert!(ledgers.spent(child).tokens <= 30_000);
    assert!(ledgers.spent(root).tokens >= ledgers.spent(child).tokens);
}

#[test]
fn tool_calls_steps_and_width_are_ledgers_too() {
    let (ledgers, root) = Ledgers::new(
        Caps {
            tool_calls: Some(2),
            steps: Some(100),
            ..Caps::NONE
        },
        2,
    );
    let child = ledgers
        .open(
            root,
            Caps {
                steps: Some(30),
                ..Caps::NONE
            },
            1,
        )
        .unwrap();
    let t1 = ledgers.admit_tool(child).unwrap();
    let _t2 = ledgers.admit_tool(root).unwrap();
    assert_eq!(
        ledgers.admit_tool(child),
        Err(Refused {
            which: BudgetKind::ToolCalls,
            node: root
        })
    );
    assert!(ledgers.cancel(&t1).is_ok());
    assert_eq!(
        ledgers.cancel(&t1),
        Err(LedgerError::Released),
        "a reservation is given back once"
    );
    let _ = ledgers.admit_tool(child).unwrap();
    // Steps: the child's own cap binds first.
    ledgers.debit_steps(child, 30).unwrap();
    assert_eq!(
        ledgers.debit_steps(child, 1),
        Err(Refused {
            which: BudgetKind::Steps,
            node: child
        })
    );
    ledgers.debit_steps(root, 70).unwrap();
    assert_eq!(ledgers.remaining(root).steps, Some(0));
    assert_eq!(ledgers.binding(child, BudgetKind::Steps), Some(root));
    // Width counts running branches: a parked branch gives its slot back.
    assert!(ledgers.acquire_slot(child));
    assert!(!ledgers.acquire_slot(child), "the child's width is 1");
    assert!(ledgers.acquire_slot(root));
    assert!(!ledgers.acquire_slot(root), "the tree's width is 2");
    ledgers.release_slot(child);
    assert!(ledgers.acquire_slot(child));
    assert_eq!(ledgers.running(root), 2);
}

#[test]
fn a_closed_frame_admits_nothing_and_its_held_reservations_stay_on_its_ancestors() {
    let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
    let child = ledgers.open(root, tokens(500), 16).unwrap();
    let res = admitted(ledgers.admit_model(child, &call(10, Some(100))));
    ledgers.close(child);
    assert!(matches!(
        ledgers.admit_model(child, &call(1, Some(1))),
        Admission::Refused(_)
    ));
    assert_eq!(ledgers.reserved(root).tokens, 110);
    assert!(
        ledgers
            .complete(
                &res,
                Amounts {
                    tokens: 60,
                    model_calls: 1,
                    ..Amounts::default()
                }
            )
            .is_ok()
    );
    assert_eq!(ledgers.spent(root).tokens, 60);
    assert_eq!(ledgers.reserved(root).tokens, 0);
}

#[test]
fn overflowing_amounts_only_ever_refuse() {
    let (ledgers, root) = Ledgers::new(tokens(u64::MAX), 16);
    let res = admitted(ledgers.admit_model(root, &call(u64::MAX - 1_000, None)));
    assert!(
        ledgers
            .complete(
                &res,
                Amounts {
                    tokens: u64::MAX,
                    model_calls: 1,
                    ..Amounts::default()
                }
            )
            .is_ok()
    );
    assert!(matches!(
        ledgers.admit_model(root, &call(1, Some(1))),
        Admission::Refused(_)
    ));
    // Prices that overflow u64 when multiplied are refused, not wrapped.
    let (ledgers, root) = Ledgers::new(
        Caps {
            cost: Some(1_000),
            ..Caps::NONE
        },
        16,
    );
    let mut c = call(u64::MAX / 2, Some(10));
    c.card.prices = Some(Prices {
        input: u64::MAX,
        output: u64::MAX,
    });
    assert!(matches!(
        ledgers.admit_model(root, &c),
        Admission::Refused(_)
    ));
}

#[test]
fn a_reservation_is_given_back_once_and_only_to_its_own_tree() {
    let (a, root_a) = Ledgers::new(tokens(1_000), 16);
    let (b, _) = Ledgers::new(tokens(1_000), 16);
    let res = admitted(a.admit_model(root_a, &call(10, Some(10))));
    assert_eq!(
        b.complete(&res, Amounts::default()),
        Err(LedgerError::WrongTree)
    );
    assert_eq!(b.cancel(&res), Err(LedgerError::WrongTree));
    assert_eq!(a.held(), 1, "the wrong tree's call changed nothing");
    assert!(a.complete(&res, Amounts::default()).is_ok());
    assert_eq!(
        a.complete(&res, Amounts::default()),
        Err(LedgerError::Released)
    );
    assert_eq!(a.cancel(&res), Err(LedgerError::Released));
}

#[test]
fn an_overshoot_is_charged_and_returned() {
    let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
    let res = admitted(ledgers.admit_model(root, &call(100, Some(300))));
    let over = ledgers
        .complete(
            &res,
            Amounts {
                tokens: 450,
                model_calls: 1,
                ..Amounts::default()
            },
        )
        .unwrap();
    assert_eq!(over.tokens, 50);
    assert_eq!(ledgers.spent(root).tokens, 450);
    assert_eq!(ledgers.remaining(root).tokens, Some(550));
}

#[test]
fn an_admission_waits_only_for_reservations_on_the_ledger_that_binds() {
    // Root uncapped; frame `a` has 300 tokens; a sibling frame `b` holds a reservation.
    let (ledgers, root) = Ledgers::new(Caps::NONE, 16);
    let a = ledgers.open(root, tokens(300), 16).unwrap();
    let b = ledgers.open(root, Caps::NONE, 16).unwrap();
    let held = admitted(ledgers.admit_model(b, &call(100, Some(100))));
    // `a` alone binds, and nothing is held there: a release in `b` cannot help, so refused.
    assert_eq!(
        ledgers.admit_model(a, &call(200, None)),
        Admission::Refused(Refused {
            which: BudgetKind::Tokens,
            node: a
        })
    );
    // The input alone uncovered in `a`: refused too, not a wait on `b`.
    assert!(matches!(
        ledgers.admit_model(a, &call(400, Some(1))),
        Admission::Refused(Refused {
            which: BudgetKind::Tokens,
            ..
        })
    ));
    // With a reservation held on `a` itself, the same admission waits on `a`.
    let in_a = admitted(ledgers.admit_model(a, &call(10, Some(1))));
    assert!(matches!(
        ledgers.admit_model(a, &call(200, None)),
        Admission::Wait { node, .. } if node == a
    ));
    assert!(ledgers.cancel(&in_a).is_ok());
    assert!(ledgers.cancel(&held).is_ok());
}

#[test]
fn a_waiter_registered_or_blocked_sees_every_release() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Wake, Waker};
    struct Flag(AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
    let first = admitted(ledgers.admit_model(root, &call(100, Some(800))));
    let Admission::Wait { epoch, .. } = ledgers.admit_model(root, &call(50, None)) else {
        panic!("expected a wait");
    };
    let flag = Arc::new(Flag(AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&flag));
    assert!(ledgers.register_waker(epoch, &waker));
    // The release happens on another thread before the blocked wait starts.
    let other = ledgers.clone();
    thread::spawn(move || assert!(other.cancel(&first).is_ok()))
        .join()
        .unwrap();
    assert!(
        flag.0.load(Ordering::SeqCst),
        "the registered waker was woken"
    );
    assert_ne!(ledgers.wait_for_release(epoch), epoch, "no lost wakeup");
    assert!(
        !ledgers.register_waker(epoch, &waker),
        "the epoch already moved"
    );
}

#[test]
fn closed_frames_admit_and_open_nothing_below_them() {
    let (ledgers, root) = Ledgers::new(tokens(1_000), 16);
    let mid = ledgers.open(root, Caps::NONE, 16).unwrap();
    let leaf = ledgers.open(mid, Caps::NONE, 16).unwrap();
    ledgers.close(mid);
    assert!(ledgers.open(mid, Caps::NONE, 16).is_none());
    assert!(
        ledgers.open(leaf, Caps::NONE, 16).is_none(),
        "an ancestor is closed"
    );
    assert!(ledgers.admit_tool(leaf).is_err());
    assert!(matches!(
        ledgers.admit_model(leaf, &call(1, Some(1))),
        Admission::Refused(_)
    ));
}

#[test]
fn closed_nodes_are_freed_and_stale_ids_name_nothing() {
    let (ledgers, root) = Ledgers::new(tokens(1_000_000), 16);
    let first = ledgers.open(root, tokens(5), 16).unwrap();
    ledgers.close(first);
    for _ in 0..10_000 {
        let n = ledgers.open(root, tokens(5), 16).unwrap();
        ledgers.debit_steps(n, 1).unwrap();
        ledgers.close(n);
    }
    assert!(ledgers.slots() <= 3, "{} slots", ledgers.slots());
    // The first id's slot was reused under another generation: the stale id is refused.
    assert_eq!(ledgers.caps(first), Caps::NONE);
    assert!(ledgers.admit_tool(first).is_err());
    assert_eq!(ledgers.spent(root).steps, 10_000);
    // A node with a held reservation is kept until it completes.
    let n = ledgers.open(root, Caps::NONE, 16).unwrap();
    let res = ledgers.admit_tool(n).unwrap();
    ledgers.close(n);
    assert_eq!(ledgers.held_on(n), 1);
    assert!(
        ledgers
            .complete(
                &res,
                Amounts {
                    tool_calls: 1,
                    ..Amounts::default()
                }
            )
            .is_ok()
    );
    assert_eq!(ledgers.held_on(n), 0);
    assert_eq!(ledgers.spent(root).tool_calls, 1);
}
