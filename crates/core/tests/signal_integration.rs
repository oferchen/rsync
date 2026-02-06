//! Integration tests for signal handling.
//!
//! These tests verify that signal handling works correctly in realistic scenarios.

use core::signal::{install_signal_handlers, CleanupManager, ShutdownReason};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn signal_handler_installation_succeeds() {
    core::signal::reset_for_testing();

    // Signal handlers should install without error
    let result = install_signal_handlers();
    assert!(result.is_ok());

    let handler = result.unwrap();
    assert!(!handler.is_shutdown_requested());
    assert!(!handler.is_abort_requested());
}

#[test]
fn cleanup_manager_tracks_temp_files() {
    let manager = CleanupManager::global();
    let dir = tempdir().expect("tempdir");

    // Create multiple temp files
    let paths: Vec<_> = (0..5)
        .map(|i| {
            let path = dir.path().join(format!("temp_{i}.tmp"));
            fs::write(&path, format!("data {i}")).expect("write file");
            path
        })
        .collect();

    // Register all temp files
    let initial_count = manager.temp_file_count();
    for path in &paths {
        manager.register_temp_file(path.clone());
    }

    // Verify count increased
    assert_eq!(manager.temp_file_count(), initial_count + 5);

    // Unregister one file (simulating successful completion)
    manager.unregister_temp_file(&paths[0]);
    assert_eq!(manager.temp_file_count(), initial_count + 4);

    // Cleanup should remove only the registered files
    manager.cleanup_temp_files();

    // paths[0] should still exist (unregistered)
    assert!(paths[0].exists());

    // paths[1..] should be gone (were registered and cleaned up)
    for path in &paths[1..] {
        assert!(!path.exists(), "File {path:?} should be removed");
    }
}

#[test]
fn cleanup_callbacks_execute_on_cleanup() {
    let manager = CleanupManager::global();

    let flag1 = Arc::new(AtomicBool::new(false));
    let flag2 = Arc::new(AtomicBool::new(false));
    let flag3 = Arc::new(AtomicBool::new(false));

    let f1 = Arc::clone(&flag1);
    let f2 = Arc::clone(&flag2);
    let f3 = Arc::clone(&flag3);

    manager.register_cleanup(Box::new(move || {
        f1.store(true, Ordering::SeqCst);
    }));

    manager.register_cleanup(Box::new(move || {
        f2.store(true, Ordering::SeqCst);
    }));

    manager.register_cleanup(Box::new(move || {
        f3.store(true, Ordering::SeqCst);
    }));

    // Callbacks should not have run yet
    assert!(!flag1.load(Ordering::SeqCst));
    assert!(!flag2.load(Ordering::SeqCst));
    assert!(!flag3.load(Ordering::SeqCst));

    // Run cleanup
    manager.cleanup();

    // All callbacks should have run
    assert!(flag1.load(Ordering::SeqCst));
    assert!(flag2.load(Ordering::SeqCst));
    assert!(flag3.load(Ordering::SeqCst));
}

#[test]
fn shutdown_reason_provides_correct_exit_code() {
    // Verify exit code mapping
    assert_eq!(
        ShutdownReason::Interrupted.exit_code(),
        core::exit_code::ExitCode::Signal
    );
    assert_eq!(
        ShutdownReason::Terminated.exit_code(),
        core::exit_code::ExitCode::Signal
    );
    assert_eq!(
        ShutdownReason::HangUp.exit_code(),
        core::exit_code::ExitCode::Signal
    );
    assert_eq!(
        ShutdownReason::PipeBroken.exit_code(),
        core::exit_code::ExitCode::SocketIo
    );
    assert_eq!(
        ShutdownReason::UserRequested.exit_code(),
        core::exit_code::ExitCode::Ok
    );
}

#[test]
fn shutdown_reason_descriptions_are_clear() {
    // Verify all shutdown reasons have meaningful descriptions
    let reasons = [
        ShutdownReason::Interrupted,
        ShutdownReason::Terminated,
        ShutdownReason::HangUp,
        ShutdownReason::PipeBroken,
        ShutdownReason::UserRequested,
    ];

    for reason in reasons {
        let desc = reason.description();
        assert!(!desc.is_empty(), "Reason {reason:?} has empty description");
        assert!(
            desc.len() > 5,
            "Reason {reason:?} has too short description: {desc}"
        );
    }
}

#[test]
fn temp_file_guard_integrates_with_cleanup_manager() {
    let manager = CleanupManager::global();
    let dir = tempdir().expect("tempdir");
    let temp_path = dir.path().join("integration_test.tmp");

    // Create and register temp file
    fs::write(&temp_path, b"test data").expect("write file");
    let initial_count = manager.temp_file_count();
    manager.register_temp_file(temp_path.clone());

    assert!(temp_path.exists());
    assert_eq!(manager.temp_file_count(), initial_count + 1);

    // Simulate error path - cleanup removes file
    manager.cleanup_temp_files();
    assert!(!temp_path.exists());
}

#[test]
fn programmatic_shutdown_request() {
    // Reset state
    core::signal::reset_for_testing();

    // Initially no shutdown requested
    assert!(!core::signal::is_shutdown_requested());
    assert!(core::signal::shutdown_reason().is_none());

    // Request programmatic shutdown
    core::signal::request_shutdown(ShutdownReason::UserRequested);

    // Shutdown should be set
    assert!(core::signal::is_shutdown_requested());
    assert_eq!(
        core::signal::shutdown_reason(),
        Some(ShutdownReason::UserRequested)
    );

    // Exit code should be Ok (0) for user-requested shutdown
    assert_eq!(
        ShutdownReason::UserRequested.exit_code().as_i32(),
        0
    );
}

#[test]
fn abort_flag_is_independent() {
    core::signal::reset_for_testing();

    // Abort can be set independently
    core::signal::request_abort();
    assert!(core::signal::is_abort_requested());

    // Can still set shutdown reason after abort
    core::signal::request_shutdown(ShutdownReason::Interrupted);
    assert!(core::signal::is_shutdown_requested());
    assert!(core::signal::is_abort_requested());
}
