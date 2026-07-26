//! Unit coverage of [`CleanupManager`] itself.
//!
//! Deliberately named after what it exercises. These tests drive the manager
//! API directly, so they say nothing about whether any signal ever reaches it -
//! that contract belongs to `sigint_temp_cleanup.rs`, which delivers a real
//! SIGINT to a real transfer.
//!
//! What is left here is the behaviour `signal_integration.rs` does not already
//! cover: the selectivity of the sweep, and that an abort request does not
//! bypass it.
//!
//! upstream: `cleanup.c:_exit_cleanup()` removes only the file it recorded,
//! never a directory scan.

#![cfg(unix)]

use core::signal::{CleanupManager, ShutdownReason};
use std::fs;
use std::sync::Mutex;
use tempfile::tempdir;

/// `CleanupManager::global()` is process-wide and `reset_for_testing()` drains
/// it, so two of these running concurrently would each clear the other's
/// registrations and pass for the wrong reason.
static SERIAL: Mutex<()> = Mutex::new(());

/// The sweep must remove exactly the registered paths. rsync's temps sit
/// beside ordinary dotfiles such as `.rsync-filter`, and a cleanup that
/// pattern-matched the directory instead of consulting its registry would eat
/// user data.
#[test]
fn cleanup_removes_only_registered_paths() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let manager = CleanupManager::global();
    manager.reset_for_testing();

    let dest = tempdir().expect("create dest dir");
    let sub = dest.path().join("subdir");
    fs::create_dir(&sub).expect("create subdir");

    let registered = [
        dest.path().join(".root_file.dat.a1b2c3"),
        sub.join(".nested_file.txt.d4e5f6"),
        sub.join(".deep_file.log.789abc"),
    ];
    for path in &registered {
        fs::write(path, b"partial transfer data").expect("write temp");
        manager.register_temp_file(path.clone());
    }

    // Never registered: a user dotfile, and a completed transfer.
    let filter_file = dest.path().join(".rsync-filter");
    fs::write(&filter_file, b"- *.bak").expect("write filter");
    let completed = dest.path().join("completed_transfer.dat");
    fs::write(&completed, b"completed data").expect("write completed file");

    manager.cleanup();

    for path in &registered {
        assert!(!path.exists(), "registered temp must be removed: {path:?}");
    }
    assert!(filter_file.exists(), ".rsync-filter must survive cleanup");
    assert!(completed.exists(), "completed file must survive cleanup");
    assert!(sub.exists(), "subdirectory must survive cleanup");
}

/// A second interrupt sets the abort flag for immediate termination. That must
/// shorten the shutdown, not skip the sweep: upstream still runs
/// `_exit_cleanup()` on the forced path, and an aborted client that left its
/// temps behind is the orphan case this whole suite exists to prevent.
#[test]
fn cleanup_still_runs_after_an_abort_request() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let manager = CleanupManager::global();
    manager.reset_for_testing();
    core::signal::reset_for_testing();

    let dest = tempdir().expect("create dest dir");
    let temp_file = dest.path().join(".data.bin.abc123");
    fs::write(&temp_file, b"partial").expect("write temp");
    manager.register_temp_file(temp_file.clone());

    core::signal::request_shutdown(ShutdownReason::Interrupted);
    assert!(!core::signal::is_abort_requested());
    core::signal::request_abort();
    assert!(core::signal::is_abort_requested());

    manager.cleanup();
    assert!(
        !temp_file.exists(),
        "temp files must be cleaned up even on abort"
    );
}
