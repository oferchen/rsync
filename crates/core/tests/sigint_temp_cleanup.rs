//! SIGINT during a real network transfer: exit code, cleanup, retention.
//!
//! This file used to *simulate* an interrupt - it set the shutdown atomics by
//! hand and called `CleanupManager::cleanup()` directly, so it stayed green
//! with the entire signal path deleted. It now delivers SIGINT to a live
//! `oc-rsync` client that is parked in a blocking read on a daemon socket, and
//! asserts only what an outside observer can see: the process exits
//! `RERR_SIGNAL` (20) within [`common::EXIT_WAIT`], and the destination is left
//! exactly as upstream leaves it.
//!
//! The three failure modes this pins, all of which the simulated version
//! missed:
//! - the handler is installed but nothing on the network path polls the flag,
//!   so the client runs to `--timeout` (caught by the exit deadline);
//! - the flag is polled but the transfer is parked in `read()` and never
//!   reaches a poll point (same deadline);
//! - shutdown happens but the receiver's temp file is not registered, so a
//!   `.name.XXXXXX` orphan survives (caught by the residue assertions).
//!
//! `partial_mid_transfer_kill.rs` and `no_partial_temp_cleanup.rs` cover the
//! same contract under SIGTERM; unit coverage of `CleanupManager` itself lives
//! in `cleanup_manager.rs` and `signal_integration.rs`.
//!
//! Upstream reference:
//! - `rsync.c:684 sig_int()` - records the signal only
//! - `io.c:750 got_kill_signal` - the I/O loop acts on it
//! - `cleanup.c:159-197` - `cleanup_got_literal && keep_partial` retention,
//!   otherwise unlink

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{FORWARD_CAP, StalledTransfer, TEST_FILE_SIZE, find_temp_files};

#[cfg(unix)]
use std::fs;

/// Default flags: SIGINT must leave neither the destination file nor the
/// `.name.XXXXXX` temp the receiver was writing into.
///
/// upstream: `cleanup.c:194-197` unlinks `cleanup_fname` when `keep_partial`
/// is unset.
#[cfg(unix)]
#[test]
fn sigint_removes_temp_orphans_from_destination() {
    let fixture = StalledTransfer::start();
    fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(&test_support::oc_rsync_bin(), &[&url], libc::SIGINT);

    assert!(
        !fixture.dest_path().join("large.bin").exists(),
        "without --partial SIGINT must not leave a destination file; daemon log: {}",
        fixture.daemon_log()
    );
    let leftovers: Vec<_> = fs::read_dir(fixture.dest_path())
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "SIGINT must leave the destination empty; found: {leftovers:?}"
    );
}

/// A nested pull: SIGINT keeps the directories the generator created but must
/// remove every temp beneath them, at any depth.
#[cfg(unix)]
#[test]
fn sigint_leaves_no_temp_orphans_in_nested_tree() {
    let fixture = StalledTransfer::start();
    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        fixture.add_file(name, TEST_FILE_SIZE);
    }
    let url = format!("{}/", fixture.module_url());
    fixture.interrupt(&test_support::oc_rsync_bin(), &["-r", &url], libc::SIGINT);

    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "SIGINT left temp file orphans in the tree: {orphans:?}"
    );
    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        let dest_file = fixture.dest_path().join(name);
        if dest_file.exists() {
            let actual = fs::metadata(&dest_file).expect("stat").len() as usize;
            assert_eq!(
                actual, TEST_FILE_SIZE,
                "{name} survived SIGINT at {actual} of {TEST_FILE_SIZE} bytes; without --partial \
                 an incomplete file must be removed, not left looking finished"
            );
        }
    }
}

/// `--partial` flips the same path from unlink to retain: the bytes that made
/// it must be at the final destination and be a genuine prefix of the source.
///
/// upstream: `cleanup.c:167-182` renames the temp onto `cleanup_new_fname`
/// when `keep_partial` is set and literal data arrived.
#[cfg(unix)]
#[test]
fn sigint_retains_partial_prefix_with_partial_flag() {
    let fixture = StalledTransfer::start();
    let source = fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &["--partial", &url],
        libc::SIGINT,
    );

    let dest_file = fixture.dest_path().join("large.bin");
    assert!(
        dest_file.exists(),
        "--partial must leave the partial at the destination after SIGINT; daemon log: {}",
        fixture.daemon_log()
    );

    let partial = fs::read(&dest_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "the proxy caps the transfer at {FORWARD_CAP} bytes, so a complete \
         {TEST_FILE_SIZE}-byte file means SIGINT was not what stopped it"
    );
    assert_eq!(
        &partial[..],
        &source[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}
