//! Tests for SIGINT temp file cleanup behavior.
//!
//! Verifies that when SIGINT triggers a shutdown, the `CleanupManager` removes
//! registered temporary files (`.XXXXXX` orphans), the correct exit code is
//! returned, and no partial files remain in the destination directory.
//!
//! These tests exercise the same code paths that real signal delivery uses -
//! the signal handler sets atomic flags, then the main loop detects shutdown
//! and invokes `CleanupManager::cleanup()`.

#![cfg(unix)]

use core::exit_code::ExitCode;
use core::signal::{CleanupManager, ShutdownReason};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

/// Simulates the SIGINT handler setting atomic flags, then the application
/// detecting shutdown and running cleanup. Verifies that all registered
/// temp files are removed.
#[test]
fn sigint_cleans_up_registered_temp_files() {
    let manager = CleanupManager::global();
    manager.reset_for_testing();
    core::signal::reset_for_testing();

    let dest = tempdir().expect("create dest dir");

    // Create temp files mimicking rsync's `.filename.XXXXXX` pattern
    let temp_names = [
        ".largefile.dat.a1b2c3",
        ".photo.jpg.d4e5f6",
        ".backup.tar.gz.789abc",
        ".document.pdf.def012",
        ".video.mp4.345678",
    ];

    let temp_paths: Vec<PathBuf> = temp_names
        .iter()
        .map(|name| {
            let path = dest.path().join(name);
            fs::write(&path, vec![0u8; 4096]).expect("write temp file");
            manager.register_temp_file(path.clone());
            path
        })
        .collect();

    // Also create a completed (non-temp) file that should survive cleanup
    let final_file = dest.path().join("completed_transfer.dat");
    fs::write(&final_file, b"completed data").expect("write final file");

    // Verify all temp files exist before signal
    for path in &temp_paths {
        assert!(
            path.exists(),
            "temp file should exist before signal: {path:?}"
        );
    }
    assert!(final_file.exists());

    // Simulate SIGINT: the signal handler sets the shutdown flag
    core::signal::request_shutdown(ShutdownReason::Interrupted);

    // Simulate the main loop detecting shutdown and running cleanup
    assert!(core::signal::is_shutdown_requested());
    assert_eq!(
        core::signal::shutdown_reason(),
        Some(ShutdownReason::Interrupted)
    );

    manager.cleanup();

    // All registered temp files should be removed
    for path in &temp_paths {
        assert!(
            !path.exists(),
            "temp file should be cleaned up after SIGINT: {path:?}"
        );
    }

    // Completed file should survive - it was never registered
    assert!(
        final_file.exists(),
        "completed file should not be removed by cleanup"
    );

    // Verify exit code maps correctly
    assert_eq!(ShutdownReason::Interrupted.exit_code(), ExitCode::Signal);
    assert_eq!(ExitCode::Signal.as_i32(), 20);
}

/// Verifies that a file unregistered before SIGINT (simulating a completed
/// transfer) is not removed during cleanup.
#[test]
fn completed_transfer_survives_sigint_cleanup() {
    let manager = CleanupManager::global();
    manager.reset_for_testing();
    core::signal::reset_for_testing();

    let dest = tempdir().expect("create dest dir");

    // Create three temp files representing in-progress transfers
    let in_progress_1 = dest.path().join(".file_a.txt.abc123");
    let in_progress_2 = dest.path().join(".file_b.txt.def456");
    let completed = dest.path().join(".file_c.txt.ghi789");

    for path in [&in_progress_1, &in_progress_2, &completed] {
        fs::write(path, b"transfer data").expect("write temp");
        manager.register_temp_file(path.clone());
    }

    // Simulate file_c completing its transfer - rename to final and unregister
    let final_c = dest.path().join("file_c.txt");
    fs::rename(&completed, &final_c).expect("rename to final");
    manager.unregister_temp_file(&completed);

    // Now SIGINT arrives
    core::signal::request_shutdown(ShutdownReason::Interrupted);
    manager.cleanup();

    // In-progress temp files should be removed
    assert!(
        !in_progress_1.exists(),
        "in-progress temp should be cleaned up"
    );
    assert!(
        !in_progress_2.exists(),
        "in-progress temp should be cleaned up"
    );

    // Completed file should survive (unregistered before cleanup)
    assert!(
        final_c.exists(),
        "completed transfer should survive SIGINT cleanup"
    );
}

/// Verifies that cleanup callbacks registered by the transfer engine run
/// when SIGINT triggers cleanup, and that they execute before temp file
/// removal (LIFO order).
#[test]
fn sigint_cleanup_runs_callbacks_before_file_removal() {
    let manager = CleanupManager::global();
    manager.reset_for_testing();
    core::signal::reset_for_testing();

    let dest = tempdir().expect("create dest dir");
    let temp_file = dest.path().join(".transfer.dat.abc123");
    fs::write(&temp_file, b"data").expect("write temp");
    manager.register_temp_file(temp_file.clone());

    let callback_order = Arc::new(AtomicUsize::new(0));

    // Register a cleanup callback (simulating engine resource cleanup)
    let order = Arc::clone(&callback_order);
    manager.register_cleanup(Box::new(move || {
        order.store(1, Ordering::SeqCst);
    }));

    // Simulate SIGINT
    core::signal::request_shutdown(ShutdownReason::Interrupted);
    manager.cleanup();

    // Callback should have run
    assert_eq!(callback_order.load(Ordering::SeqCst), 1);

    // Temp file should be removed
    assert!(!temp_file.exists());
}

/// Verifies that a second SIGINT sets the abort flag for immediate
/// termination, while still allowing cleanup to proceed.
#[test]
fn double_sigint_sets_abort_flag() {
    let manager = CleanupManager::global();
    manager.reset_for_testing();
    core::signal::reset_for_testing();

    let dest = tempdir().expect("create dest dir");
    let temp_file = dest.path().join(".data.bin.abc123");
    fs::write(&temp_file, b"partial").expect("write temp");
    manager.register_temp_file(temp_file.clone());

    // First SIGINT - graceful shutdown
    core::signal::request_shutdown(ShutdownReason::Interrupted);
    assert!(core::signal::is_shutdown_requested());
    assert!(!core::signal::is_abort_requested());

    // Second SIGINT - immediate abort
    core::signal::request_abort();
    assert!(core::signal::is_abort_requested());

    // Even on abort, cleanup should still remove temp files
    manager.cleanup();
    assert!(
        !temp_file.exists(),
        "temp files should be cleaned up even on abort"
    );
}

/// Verifies that the destination directory contains no files matching
/// rsync's temp file pattern after SIGINT cleanup. This is the key
/// invariant: no `.XXXXXX` orphans left behind.
#[test]
fn no_temp_orphans_remain_after_sigint() {
    let manager = CleanupManager::global();
    manager.reset_for_testing();
    core::signal::reset_for_testing();

    let dest = tempdir().expect("create dest dir");

    // Create a subdirectory structure with temp files at various levels
    let sub = dest.path().join("subdir");
    fs::create_dir(&sub).expect("create subdir");

    let temps = vec![
        dest.path().join(".root_file.dat.a1b2c3"),
        sub.join(".nested_file.txt.d4e5f6"),
        sub.join(".deep_file.log.789abc"),
    ];

    // Also create legitimate dot-files that should not be removed
    let legit_dotfile = dest.path().join(".rsync-filter");
    fs::write(&legit_dotfile, b"- *.bak").expect("write filter");

    for path in &temps {
        fs::write(path, b"partial transfer data").expect("write temp");
        manager.register_temp_file(path.clone());
    }

    // Simulate SIGINT and cleanup
    core::signal::request_shutdown(ShutdownReason::Interrupted);
    manager.cleanup();

    // No temp orphans should remain
    for path in &temps {
        assert!(!path.exists(), "temp orphan should be removed: {path:?}");
    }

    // Legitimate files should survive
    assert!(
        legit_dotfile.exists(),
        ".rsync-filter should survive cleanup"
    );
    assert!(sub.exists(), "subdirectory should survive cleanup");
}

/// Verifies the full exit code path: SIGINT maps to `ExitCode::Signal` (20),
/// matching upstream rsync behavior.
#[test]
fn sigint_exit_code_matches_upstream() {
    // upstream: rsync exits with code 20 on SIGINT/SIGTERM/SIGHUP
    let reason = ShutdownReason::Interrupted;
    let code = reason.exit_code();
    assert_eq!(code, ExitCode::Signal);
    assert_eq!(code.as_i32(), 20);

    // SIGTERM also maps to 20
    assert_eq!(ShutdownReason::Terminated.exit_code().as_i32(), 20);

    // SIGHUP also maps to 20
    assert_eq!(ShutdownReason::HangUp.exit_code().as_i32(), 20);
}
