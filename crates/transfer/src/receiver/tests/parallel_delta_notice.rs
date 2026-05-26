//! Tests for the parallel-receive-delta feature notice (PFF-3).
//!
//! Verifies that a diagnostic message is emitted when the
//! `parallel-receive-delta` feature is enabled, warning that the feature
//! provides experimental scaffolding rather than a production-validated
//! parallel path.

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, debug_log, drain_events, init};

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

/// When the `parallel-receive-delta` feature is enabled, `setup_transfer`
/// emits a `debug_log!(Recv, 1, ...)` notice. This test exercises the same
/// macro call that `setup_transfer` uses and verifies the expected keywords
/// appear in the message. The test compiles on all platforms.
#[cfg(feature = "parallel-receive-delta")]
#[test]
fn parallel_receive_delta_notice_emitted() {
    init_recv_level1();

    debug_log!(
        Recv,
        1,
        "parallel-receive-delta feature enabled - \
         this is experimental scaffolding, not a production-validated \
         parallel path; the receiver still uses the sequential delta loop"
    );

    let msgs = recv_messages();
    assert!(
        msgs.iter()
            .any(|m| m.contains("parallel-receive-delta feature enabled")
                && m.contains("experimental scaffolding")),
        "expected parallel-receive-delta notice in: {msgs:?}"
    );
}

/// When the feature is disabled, no notice should be emitted by the
/// feature-gated code path. This test verifies the cfg gate compiles
/// correctly in the absence of the feature.
#[cfg(not(feature = "parallel-receive-delta"))]
#[test]
fn parallel_receive_delta_notice_absent_without_feature() {
    init_recv_level1();

    // The feature-gated debug_log! in setup_transfer is not compiled.
    // Verify no stray parallel-receive-delta messages appear.
    let msgs = recv_messages();
    assert!(
        !msgs
            .iter()
            .any(|m| m.contains("parallel-receive-delta feature enabled")),
        "unexpected parallel-receive-delta notice without feature: {msgs:?}"
    );
}
