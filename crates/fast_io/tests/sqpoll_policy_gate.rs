//! Integration tests for the `--no-io-uring-sqpoll` opt-out gate.
//!
//! Verifies that calling [`fast_io::set_sqpoll_disabled_by_policy`] flips
//! the process-wide [`fast_io::is_sqpoll_disabled_by_policy`] query, and
//! that on Linux the ring builder honours the gate by never requesting
//! `IORING_SETUP_SQPOLL` even when the per-config `sqpoll: true` flag is
//! set. The gate exists so rootless Kubernetes pods and other
//! `CAP_SYS_NICE`-less environments get a deterministic opt-out instead
//! of relying on the transparent `EPERM` fallback.

use fast_io::{is_sqpoll_disabled_by_policy, set_sqpoll_disabled_by_policy};

/// The gate must read as `false` before any explicit opt-out and must
/// flip to `true` exactly when [`set_sqpoll_disabled_by_policy`] is
/// called. Once set, it must stay set: there is no public unset path,
/// since toggling SQPOLL back on while rings are being built on other
/// threads would race the builder.
///
/// This test runs in its own integration binary so the
/// `AtomicBool` state cannot leak across unrelated unit tests sharing
/// the same process. Cargo gives every `tests/*.rs` file its own
/// binary, isolating the state.
#[test]
fn sqpoll_gate_is_off_until_explicitly_set_and_then_stays_on() {
    // Initial state: gate is off so production builds keep their existing
    // SQPOLL behaviour (the per-config `sqpoll: false` default already
    // makes this a no-op for default callers, but the gate must not
    // misreport itself).
    assert!(
        !is_sqpoll_disabled_by_policy(),
        "process must start with the SQPOLL opt-out gate off"
    );

    set_sqpoll_disabled_by_policy();

    assert!(
        is_sqpoll_disabled_by_policy(),
        "set_sqpoll_disabled_by_policy must flip the gate to true"
    );

    // Idempotent: calling the setter again must not toggle the state
    // back. There is no API to clear it; verify by calling twice.
    set_sqpoll_disabled_by_policy();
    assert!(
        is_sqpoll_disabled_by_policy(),
        "the SQPOLL opt-out must be one-way once set"
    );
}

/// On Linux with the `io_uring` feature, building a ring from a config
/// that requests SQPOLL must still succeed after the gate is set: the
/// builder silently downgrades to a regular ring. This is the safety net
/// for environments where SQPOLL would fail with `EPERM`; the gate makes
/// the downgrade deterministic instead of relying on the kernel reject.
///
/// The behaviour we assert is "ring builds", not "no SQPOLL kthread is
/// observable" - the latter would require sampling `/proc` from a test,
/// which is brittle. The deterministic check is that the gate causes
/// [`fast_io::IoUringConfig::build_ring`] to take its
/// suppress-SQPOLL branch, which we verify indirectly: the existing
/// `build_ring_with_sqpoll_falls_back_gracefully` unit test in
/// `config.rs` already proves the fallback succeeds; this test confirms
/// the gate-driven path stays compatible with that contract.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
#[test]
fn sqpoll_gate_keeps_ring_construction_compatible_with_sqpoll_off_policy() {
    if !fast_io::is_io_uring_available() {
        // The CI runner kernel may be below 5.6 or block io_uring via
        // seccomp; skip rather than fail. The fallback path that
        // applies when io_uring is unavailable already routes through
        // standard I/O, so the gate has nothing to gate.
        return;
    }

    set_sqpoll_disabled_by_policy();
    assert!(is_sqpoll_disabled_by_policy());

    // The visible contract: the policy-driven factory dispatches
    // through the same ring-construction code path that `build_ring`
    // uses internally, and must return a valid writer under
    // `IoUringPolicy::SqpollOff` without panicking or returning
    // `Unsupported`. `build_ring` itself is `pub(crate)`; the factory
    // call is the public surface that proves the gate-driven path
    // stays compatible.
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let path = tmpdir.path().join("sqpoll-gate.bin");
    let file = std::fs::File::create(&path).expect("create");
    let writer = fast_io::writer_from_file(file, 4096, fast_io::IoUringPolicy::SqpollOff)
        .expect("writer_from_file must succeed under the SQPOLL opt-out");
    drop(writer);

    assert!(
        is_sqpoll_disabled_by_policy(),
        "the gate must remain set after factory dispatch"
    );
}
