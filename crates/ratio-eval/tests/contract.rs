//! The host contract suite against `ScriptedHost` (unguarded), and the guard's own refusals in
//! front of a host that checks nothing.

#![cfg(feature = "host-tests")]

use std::sync::Arc;

use ratio_eval::frame::{FrameKind, Ledgers, Scope, Settings};
use ratio_eval::host::{DeniedBy, EffectCtx, EffectKey, Guard, Host, HostError, NoHost};
use ratio_eval::host_tests::{self, Fixture, ScriptedFixture};
use ratio_eval::{CancelToken, Value};

#[test]
fn scripted_host_passes_the_contract_suite() {
    host_tests::run_all(&ScriptedFixture::default());
}

#[test]
fn the_guard_refuses_every_sink_before_a_host_that_checks_nothing() {
    let guard = Arc::new(Guard::new(Arc::new(NoHost)));
    let calls = ScriptedFixture::default().calls();
    let scope = Scope::session();
    let (ledgers, node) = Ledgers::new(scope.budget, scope.width);
    let frame = ratio_eval::frame::FrameCtx {
        path: Arc::from("turn"),
        parent: None,
        kind: FrameKind::Entry,
        harness: Arc::from("turn"),
        scope: Arc::new(scope),
        ledgers,
        node,
        settings: Arc::new(Settings::default()),
    };
    for mut call in [calls.network, calls.command, calls.write] {
        // A labelled argument, and a flag claiming nothing is secret: the guard recomputes it.
        call.args = call
            .args
            .into_iter()
            .map(|(n, v)| (n, v.with_labels(ratio_eval::Labels::source(1))))
            .collect();
        call.secret = false;
        let cx = EffectCtx {
            key: EffectKey {
                frame: Arc::from("turn"),
                branch: Arc::from(""),
                site: ([0; 32], 1),
                occurrence: 0,
            },
            cancel: CancelToken::new(),
            deadline: None,
        };
        let tool = call.tool.clone();
        let r = ratio_eval::block_on(Arc::clone(&guard).call_tool(frame.clone(), cx, call));
        assert!(
            matches!(
                r,
                Err(HostError::Denied {
                    by: DeniedBy::Secrecy,
                    ..
                })
            ),
            "the guard must refuse a secret to `{tool}`"
        );
    }
    let _ = Value::unit();
}
