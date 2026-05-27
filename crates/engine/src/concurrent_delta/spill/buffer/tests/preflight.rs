//! Pre-flight writability probe tests.

use std::fs;

use super::super::super::SpillableReorderBuffer;

#[test]
fn probe_writability_succeeds_on_valid_dir() {
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir).expect("setup spill directory");

    buf.probe_writability()
        .expect("probe must succeed on a writable directory");

    // The probe file must not linger after a successful probe.
    assert!(
        !spill_dir.join(".oc-rsync-probe").exists(),
        "probe file must be cleaned up"
    );
}

#[cfg(unix)]
#[test]
fn probe_writability_fails_on_unwritable_dir() {
    use std::os::unix::fs::PermissionsExt;

    // Root bypasses file permission checks, so this test is meaningless
    // when running as uid 0. Skip gracefully.
    if rustix::process::getuid().is_root() {
        return;
    }

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir).expect("setup spill directory");

    // Revoke all permissions on the spill directory.
    fs::set_permissions(&spill_dir, fs::Permissions::from_mode(0o000))
        .expect("chmod 000 spill dir");

    let result = buf.probe_writability();

    // Restore permissions before assertions so cleanup succeeds.
    let _ = fs::set_permissions(&spill_dir, fs::Permissions::from_mode(0o755));

    let err = result.expect_err("probe must fail on an unwritable directory");
    match err {
        super::super::super::SpillError::Io(ref e) => {
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "expected PermissionDenied, got {:?}",
                e.kind()
            );
        }
        other => panic!("expected SpillError::Io(PermissionDenied), got {other:?}"),
    }
}

#[test]
fn probe_writability_skips_when_in_memory_only() {
    // Create a buffer with an explicit spill dir but in-memory-only mode.
    // The probe must return Ok without touching the filesystem.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir)
            .expect("setup spill directory")
            .with_in_memory_only(true);

    // Remove the directory entirely - if the probe tried to touch disk it
    // would fail, proving the skip path is taken.
    fs::remove_dir_all(&spill_dir).expect("remove spill dir");

    buf.probe_writability()
        .expect("probe must succeed (skip) in in-memory-only mode");
}

#[test]
fn probe_writability_skips_when_no_spill_dir() {
    // Default buffer with no explicit spill dir - uses SpooledTempFile.
    let buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(16, 8);

    buf.probe_writability()
        .expect("probe must succeed (skip) when no spill dir is configured");
}
