use crate::local_copy::{
    test_support::take_fsync_call_count, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
};

/// Tests that fsync-enabled transfers complete successfully.
///
/// With deferred sync always compiled in, fsync operations are batched via
/// DeferredSync and flushed at the end of the transfer. The immediate fsync
/// call count will be 0 because DeferredSync handles syncing, not the
/// per-file immediate path.
#[test]
fn execute_succeeds_with_fsync_enabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"payload").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Clear any previous instrumentation counts.
    take_fsync_call_count();

    let options = LocalCopyOptions::default().fsync(true);
    plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Immediate fsync count is 0 because DeferredSync handles batched syncing.
    assert_eq!(take_fsync_call_count(), 0);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"payload");
}

#[test]
fn execute_skips_fsync_when_not_requested() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"payload").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    take_fsync_call_count();

    plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(take_fsync_call_count(), 0);
    assert!(destination.exists());
}
