//! Receiver strategy dispatch coverage (Path B).
//!
//! Verifies that `ReceiverContext::dispatch_receiver_strategy` walks the
//! file list, computes the total source byte count, and returns the
//! [`ReceiverStrategy`] dictated by the file_count / total_size heuristic
//! documented in `docs/design/parallel-receive-delta-default-on.md`.

use std::ffi::OsStr;
use std::sync::Mutex;

use platform::env::EnvGuard;
use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::super::stats::ReceiverStrategy;
use super::super::super::{FORCE_PARALLEL_RECEIVE_ENV, PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD};
use super::super::support::{test_config, test_handshake};

/// Serialises every test in this module that touches `OC_RSYNC_FORCE_PARALLEL`.
/// `EnvGuard` requires callers to ensure no concurrent env mutations, and even
/// tests that only *read* the variable must coordinate so a sibling test cannot
/// flip it under them.
static FORCE_PARALLEL_ENV_LOCK: Mutex<()> = Mutex::new(());

fn make_ctx_with_files(entries: Vec<FileEntry>) -> ReceiverContext {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = entries;
    ctx
}

#[test]
fn total_source_bytes_sums_file_list() {
    let entries = vec![
        FileEntry::new_file("a".into(), 100, 0o644),
        FileEntry::new_file("b".into(), 200, 0o644),
        FileEntry::new_file("c".into(), 300, 0o644),
    ];
    let ctx = make_ctx_with_files(entries);
    assert_eq!(ctx.total_source_bytes(), 600);
}

#[test]
fn dispatch_small_transfer_picks_sequential() {
    // 5 small files at 1 KiB each = 5 KiB total. Both inputs well below
    // the Path B cutoffs.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove(FORCE_PARALLEL_RECEIVE_ENV);
    let entries = (0..5)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1024, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(5);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}

#[test]
#[cfg(feature = "parallel-receive-delta")]
fn dispatch_many_small_files_picks_parallel() {
    // 200 tiny files trips the file_count cutoff even though total_size
    // is negligible.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove(FORCE_PARALLEL_RECEIVE_ENV);
    let entries = (0..200)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(200);
    assert_eq!(strategy, ReceiverStrategy::Parallel);
}

#[test]
#[cfg(feature = "parallel-receive-delta")]
fn dispatch_large_bytes_picks_parallel() {
    // Single 128 MiB file - trips the 64 MiB total_size cutoff even with
    // a small file_count.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove(FORCE_PARALLEL_RECEIVE_ENV);
    let entries = vec![FileEntry::new_file("big".into(), 128 * 1024 * 1024, 0o644)];
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(1);
    assert_eq!(strategy, ReceiverStrategy::Parallel);
}

#[test]
#[cfg(not(feature = "parallel-receive-delta"))]
fn dispatch_falls_back_to_sequential_without_feature() {
    // When the feature is compiled out, even a "wants-parallel" workload
    // must report sequential so telemetry never lies about the path taken.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove(FORCE_PARALLEL_RECEIVE_ENV);
    let entries = (0..200)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(200);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}

#[test]
fn dispatch_empty_file_list_picks_sequential() {
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove(FORCE_PARALLEL_RECEIVE_ENV);
    let mut ctx = make_ctx_with_files(Vec::new());
    let strategy = ctx.dispatch_receiver_strategy(0);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}

#[test]
fn dispatch_without_force_env_uses_threshold() {
    // With the env override absent, a sub-threshold workload still picks the
    // sequential path - the heuristic is unaffected.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove(FORCE_PARALLEL_RECEIVE_ENV);

    let small_count = PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD / 2;
    let entries = (0..small_count)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(small_count);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}

#[test]
#[cfg(feature = "parallel-receive-delta")]
fn dispatch_force_env_overrides_threshold_to_parallel() {
    // Sub-threshold workload with the env override set: parallel wins even
    // though both file_count and total_size are well under the cutoffs.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::set(FORCE_PARALLEL_RECEIVE_ENV, OsStr::new("1"));

    let entries = (0..5)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(5);
    assert_eq!(strategy, ReceiverStrategy::Parallel);
}

#[test]
#[cfg(not(feature = "parallel-receive-delta"))]
fn dispatch_force_env_falls_back_to_sequential_without_feature() {
    // Env override is set, but the feature is compiled out. The dispatcher
    // must log `parallel_unavailable` and return sequential so telemetry
    // never claims a path the binary cannot take.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::set(FORCE_PARALLEL_RECEIVE_ENV, OsStr::new("1"));

    let entries = (0..5)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(5);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}

#[test]
fn dispatch_force_env_empty_value_does_not_trigger() {
    // An empty value must not trip the override - matches `is_some_and(|v| !v.is_empty())`.
    let _lock = FORCE_PARALLEL_ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::set(FORCE_PARALLEL_RECEIVE_ENV, OsStr::new(""));

    let entries = (0..5)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(5);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}
