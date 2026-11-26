//! The host contract suite against `ScriptedHost` (unguarded), and the guard's own refusals in
//! front of a host that checks nothing.

#![cfg(feature = "host-tests")]

use std::sync::Arc;

use ratio_eval::CancelToken;
use ratio_eval::frame::{FrameKind, Ledgers, Scope, Settings};
use ratio_eval::host::{DeniedBy, EffectCtx, EffectKey, Guard, Host, HostError, NoHost};
use ratio_eval::host_tests::{self, Fixture, ScriptedFixture};

#[test]
fn scripted_host_passes_the_contract_suite() {
    host_tests::run_all(&ScriptedFixture::default());
}

fn secrecy<T>(r: &Result<T, HostError>) -> bool {
    matches!(
        r,
        Err(HostError::Denied {
            by: DeniedBy::Secrecy,
            ..
        })
    )
}

fn cx() -> EffectCtx {
    EffectCtx {
        key: EffectKey {
            frame: Arc::from("turn"),
            branch: Arc::from(""),
            site: ([0; 32], 1),
            occurrence: 0,
        },
        cancel: CancelToken::new(),
        deadline: None,
    }
}

/// Every sink of the contract suite, at the guard, in front of a host that checks nothing
/// (`NoHost` would answer `Unsupported`): each is refused by the guard, from the labels, with
/// the flags claiming nothing is secret.
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
    for mut call in [
        calls.network,
        calls.command,
        calls.write,
        calls.mcp,
        calls.any_tool,
    ] {
        call.args = call
            .args
            .into_iter()
            .map(|(n, v)| (n, v.with_labels(ratio_eval::Labels::source(1))))
            .collect();
        call.secret = false;
        let tool = call.tool.clone();
        let r = ratio_eval::block_on(Arc::clone(&guard).call_tool(frame.clone(), cx(), call));
        assert!(secrecy(&r), "the guard must refuse a secret to `{tool}`");
    }
    for mut op in host_tests::sink_operations() {
        op.secret = false;
        let what = Arc::clone(&op.builtin);
        let r = ratio_eval::block_on(Arc::clone(&guard).operate(frame.clone(), cx(), op));
        assert!(secrecy(&r), "the guard must refuse a secret to `{what}`");
    }
    let mut ev = host_tests::sink_event();
    ev.secret = false;
    assert!(secrecy(&guard.emit(&frame, ev)));
}
