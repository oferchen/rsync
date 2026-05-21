//! Receiver strategy dispatch coverage (Path B).
//!
//! Verifies that `ReceiverContext::dispatch_receiver_strategy` walks the
//! file list, computes the total source byte count, and returns the
//! [`ReceiverStrategy`] dictated by the file_count / total_size heuristic
//! documented in `docs/design/parallel-receive-delta-default-on.md`.

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::super::stats::ReceiverStrategy;
use super::super::support::{test_config, test_handshake};

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
    let entries = (0..200)
        .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o644))
        .collect();
    let mut ctx = make_ctx_with_files(entries);
    let strategy = ctx.dispatch_receiver_strategy(200);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}

#[test]
fn dispatch_empty_file_list_picks_sequential() {
    let mut ctx = make_ctx_with_files(Vec::new());
    let strategy = ctx.dispatch_receiver_strategy(0);
    assert_eq!(strategy, ReceiverStrategy::Sequential);
}
