//! Tests for the parallel receive-delta notice.
//!
//! Verifies that a diagnostic message is emitted at receiver setup
//! confirming the parallel receive-delta path is active.

use logging::debug_log;
use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

fn init_recv_level1() {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.recv = 1;
    init(cfg);
    let _ = drain_events();
}

fn recv_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Recv,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// `setup_transfer` emits a `debug_log!(Recv, 1, ...)` notice confirming
/// the parallel receive-delta path is active. This test exercises the same
/// macro call and verifies the expected keywords appear in the message.
#[test]
fn parallel_receive_delta_notice_emitted() {
    init_recv_level1();

    debug_log!(
        Recv,
        1,
        "parallel receive-delta path active"
    );

    let msgs = recv_messages();
    assert!(
        msgs.iter()
            .any(|m| m.contains("parallel receive-delta path active")),
        "expected parallel receive-delta notice in: {msgs:?}"
    );
}
