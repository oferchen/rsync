// Comprehensive tests for timeout handling in the local copy engine.
//
// These tests cover:
// 1. Timeout error construction and properties
// 2. Exit code mapping (RERR_TIMEOUT = 30)
// 3. Timeout error messages
// 4. Connection timeout vs I/O timeout semantics
// 5. Timeout interaction with transfer state
// 6. Very short timeout edge cases
// 7. Timeout of 0 (disable) behavior
// 8. Stop-at deadline handling (related to timeout)

use std::time::SystemTime;

// =============================================================================
// LocalCopyError::timeout Construction Tests
// =============================================================================

#[test]
fn timeout_error_construction_with_typical_duration() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
}

#[test]
fn timeout_error_construction_with_zero_duration() {
    let error = LocalCopyError::timeout(Duration::from_secs(0));
    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
}

#[test]
fn timeout_error_construction_with_one_second() {
    let error = LocalCopyError::timeout(Duration::from_secs(1));
    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
}

#[test]
fn timeout_error_construction_with_subsecond_duration() {
    let error = LocalCopyError::timeout(Duration::from_millis(500));
    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
}

#[test]
fn timeout_error_construction_with_large_duration() {
    let error = LocalCopyError::timeout(Duration::from_secs(86400)); // 24 hours
    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
}

// =============================================================================
// Timeout Exit Code Tests (RERR_TIMEOUT = 30)
// =============================================================================

#[test]
fn timeout_error_exit_code_is_30() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    assert_eq!(error.exit_code(), 30);
}

#[test]
fn timeout_exit_code_matches_upstream_rerr_timeout() {
    // RERR_TIMEOUT = 30 in upstream rsync's errcode.h
    let error = LocalCopyError::timeout(Duration::from_secs(60));
    assert_eq!(error.exit_code(), super::filter_program::TIMEOUT_EXIT_CODE);
    assert_eq!(error.exit_code(), 30);
}

#[test]
fn timeout_exit_code_consistent_across_durations() {
    let durations = [
        Duration::from_secs(0),
        Duration::from_secs(1),
        Duration::from_secs(30),
        Duration::from_secs(3600),
        Duration::from_secs(86400),
    ];

    for duration in durations {
        let error = LocalCopyError::timeout(duration);
        assert_eq!(
            error.exit_code(),
            30,
            "exit code should be 30 for duration {:?}",
            duration
        );
    }
}

// =============================================================================
// Timeout Error Code Name Tests
// =============================================================================

#[test]
fn timeout_code_name_is_rerr_timeout() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    assert_eq!(error.code_name(), "RERR_TIMEOUT");
}

#[test]
fn timeout_code_name_different_from_io_error() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let io_error = LocalCopyError::io(
        "read file",
        PathBuf::from("/test"),
        io::Error::new(io::ErrorKind::TimedOut, "operation timed out"),
    );

    assert_eq!(timeout_error.code_name(), "RERR_TIMEOUT");
    assert_eq!(io_error.code_name(), "RERR_PARTIAL");
    assert_ne!(timeout_error.code_name(), io_error.code_name());
}

// =============================================================================
// Timeout Error Message Tests
// =============================================================================

#[test]
fn timeout_error_message_contains_timed_out() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let message = error.to_string();
    assert!(
        message.contains("timed out"),
        "message should contain 'timed out': {message}"
    );
}

#[test]
fn timeout_error_message_contains_duration() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let message = error.to_string();
    assert!(
        message.contains("30"),
        "message should contain duration value: {message}"
    );
}

#[test]
fn timeout_error_message_mentions_progress() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let message = error.to_string();
    assert!(
        message.contains("progress"),
        "message should mention lack of progress: {message}"
    );
}

#[test]
fn timeout_error_message_with_subsecond_precision() {
    let error = LocalCopyError::timeout(Duration::from_millis(30500)); // 30.5 seconds
    let message = error.to_string();
    // Should show seconds with decimal precision
    assert!(
        message.contains("30.5") || message.contains("30.500"),
        "message should show subsecond precision: {message}"
    );
}

#[test]
fn timeout_error_message_with_zero_duration() {
    let error = LocalCopyError::timeout(Duration::from_secs(0));
    let message = error.to_string();
    assert!(
        message.contains("0"),
        "message should contain zero: {message}"
    );
}

// =============================================================================
// Timeout Kind Extraction Tests
// =============================================================================

#[test]
fn timeout_kind_provides_duration_access() {
    let expected_duration = Duration::from_secs(45);
    let error = LocalCopyError::timeout(expected_duration);

    match error.kind() {
        LocalCopyErrorKind::Timeout { duration } => {
            assert_eq!(*duration, expected_duration);
        }
        _ => panic!("Expected Timeout variant"),
    }
}

#[test]
fn timeout_kind_into_kind_consumes_error() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let kind = error.into_kind();
    assert!(matches!(kind, LocalCopyErrorKind::Timeout { .. }));
}

// =============================================================================
// Timeout vs Stop-At Comparison Tests
// =============================================================================

#[test]
fn timeout_and_stop_at_have_same_exit_code() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let stop_at_error = LocalCopyError::stop_at_reached(SystemTime::now());

    // Both should return RERR_TIMEOUT = 30
    assert_eq!(timeout_error.exit_code(), stop_at_error.exit_code());
    assert_eq!(timeout_error.exit_code(), 30);
}

#[test]
fn timeout_and_stop_at_have_same_code_name() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let stop_at_error = LocalCopyError::stop_at_reached(SystemTime::now());

    assert_eq!(timeout_error.code_name(), "RERR_TIMEOUT");
    assert_eq!(stop_at_error.code_name(), "RERR_TIMEOUT");
}

#[test]
fn timeout_and_stop_at_are_distinct_kinds() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let stop_at_error = LocalCopyError::stop_at_reached(SystemTime::now());

    let timeout_kind_match = matches!(timeout_error.kind(), LocalCopyErrorKind::Timeout { .. });
    let stop_at_kind_match = matches!(stop_at_error.kind(), LocalCopyErrorKind::StopAtReached { .. });

    assert!(timeout_kind_match);
    assert!(stop_at_kind_match);
}

// =============================================================================
// Timeout vs Other Error Types Tests
// =============================================================================

#[test]
fn timeout_exit_code_distinct_from_partial_transfer() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let io_error = LocalCopyError::io(
        "read file",
        PathBuf::from("/test"),
        io::Error::new(io::ErrorKind::NotFound, "file not found"),
    );

    // RERR_TIMEOUT = 30, RERR_PARTIAL = 23
    assert_eq!(timeout_error.exit_code(), 30);
    assert_eq!(io_error.exit_code(), 23);
    assert_ne!(timeout_error.exit_code(), io_error.exit_code());
}

#[test]
fn timeout_exit_code_distinct_from_delete_limit() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let delete_limit_error = LocalCopyError::delete_limit_exceeded(100);

    // RERR_TIMEOUT = 30, RERR_DEL_LIMIT = 25
    assert_eq!(timeout_error.exit_code(), 30);
    assert_eq!(delete_limit_error.exit_code(), 25);
    assert_ne!(timeout_error.exit_code(), delete_limit_error.exit_code());
}

#[test]
fn timeout_exit_code_distinct_from_syntax_error() {
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    let syntax_error = LocalCopyError::missing_operands();

    // RERR_TIMEOUT = 30, RERR_SYNTAX = 1
    assert_eq!(timeout_error.exit_code(), 30);
    assert_eq!(syntax_error.exit_code(), 1);
    assert_ne!(timeout_error.exit_code(), syntax_error.exit_code());
}

// =============================================================================
// Very Short Timeout Edge Cases
// =============================================================================

#[test]
fn one_millisecond_timeout_is_valid() {
    let error = LocalCopyError::timeout(Duration::from_millis(1));
    assert_eq!(error.exit_code(), 30);
}

#[test]
fn one_microsecond_timeout_is_valid() {
    let error = LocalCopyError::timeout(Duration::from_micros(1));
    assert_eq!(error.exit_code(), 30);
}

#[test]
fn one_nanosecond_timeout_is_valid() {
    let error = LocalCopyError::timeout(Duration::from_nanos(1));
    assert_eq!(error.exit_code(), 30);
}

#[test]
fn zero_duration_timeout_message_is_sensible() {
    let error = LocalCopyError::timeout(Duration::ZERO);
    let message = error.to_string();

    // Even with zero duration, the message should be coherent
    assert!(message.contains("timed out"));
    assert!(message.contains("0.000"));
}

// =============================================================================
// Connection Timeout Exit Code (RERR_CONTIMEOUT = 35)
// =============================================================================

/// Connection timeout has a distinct exit code from I/O timeout.
/// RERR_CONTIMEOUT = 35, RERR_TIMEOUT = 30
#[test]
fn connection_timeout_exit_code_documented() {
    // Document the expected exit codes
    const RERR_TIMEOUT: i32 = 30;     // I/O timeout during transfer
    const RERR_CONTIMEOUT: i32 = 35;  // Connection timeout during setup

    assert_ne!(RERR_TIMEOUT, RERR_CONTIMEOUT);

    // LocalCopyError::timeout returns RERR_TIMEOUT (30)
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));
    assert_eq!(timeout_error.exit_code(), RERR_TIMEOUT);
}

// =============================================================================
// Timeout Error Debug and Display Tests
// =============================================================================

#[test]
fn timeout_error_debug_format() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let debug = format!("{:?}", error);

    // Debug format should include relevant information
    assert!(debug.contains("Timeout"));
    assert!(debug.contains("30"));
}

#[test]
fn timeout_error_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LocalCopyError>();
}

// =============================================================================
// Timeout Error Chaining Tests
// =============================================================================

#[test]
fn timeout_error_source_documented() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    // Timeout errors don't have an underlying source error
    // (unlike IO errors which wrap io::Error)
    // Just verify the error can be created and has proper exit code
    assert_eq!(error.exit_code(), 30);
}

// =============================================================================
// Timeout Duration Range Tests
// =============================================================================

#[test]
fn timeout_accepts_duration_max() {
    // This might overflow in message formatting, but construction should work
    let error = LocalCopyError::timeout(Duration::MAX);
    assert_eq!(error.exit_code(), 30);
}

#[test]
fn typical_user_timeout_values() {
    // Common timeout values users might set via --timeout
    let typical_seconds = [
        30,    // Default in some configurations
        60,    // 1 minute
        120,   // 2 minutes
        300,   // 5 minutes
        600,   // 10 minutes
        900,   // 15 minutes
        1800,  // 30 minutes
        3600,  // 1 hour
        7200,  // 2 hours
        86400, // 24 hours
    ];

    for secs in typical_seconds {
        let error = LocalCopyError::timeout(Duration::from_secs(secs));
        assert_eq!(error.exit_code(), 30);
        let message = error.to_string();
        assert!(message.contains(&secs.to_string()) || message.contains(&format!("{}.0", secs)));
    }
}

// =============================================================================
// Timeout Recovery Tests (Conceptual)
// =============================================================================

/// After a timeout error, the transfer state should be in a recoverable position.
/// This is a conceptual test documenting expected behavior.
#[test]
fn timeout_error_allows_retry_documentation() {
    // When a timeout occurs:
    // 1. The error is returned to the caller
    // 2. Partial files should be in --partial-dir if enabled
    // 3. The transfer can be retried from scratch or resumed

    let error = LocalCopyError::timeout(Duration::from_secs(30));

    // The error should indicate timeout, not corruption
    assert_eq!(error.code_name(), "RERR_TIMEOUT");

    // Exit code 30 tells caller "timeout" vs "partial transfer" (23)
    assert_eq!(error.exit_code(), 30);
}

// =============================================================================
// Timeout Message Quality Tests
// =============================================================================

#[test]
fn timeout_message_is_user_friendly() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let message = error.to_string();

    // Message should be understandable by users
    // - Should mention what happened (timeout)
    // - Should mention duration
    // - Should hint at the cause (no progress)

    assert!(message.contains("timed out"), "should mention timeout");
    assert!(
        message.contains("30") || message.contains("seconds"),
        "should mention duration"
    );
}

#[test]
fn timeout_message_not_empty() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    let message = error.to_string();
    assert!(!message.is_empty());
    assert!(message.len() > 10, "message should be descriptive");
}

// =============================================================================
// Timeout Error Equality Tests
// =============================================================================

#[test]
fn timeout_errors_with_same_duration_have_same_properties() {
    let error1 = LocalCopyError::timeout(Duration::from_secs(30));
    let error2 = LocalCopyError::timeout(Duration::from_secs(30));

    assert_eq!(error1.exit_code(), error2.exit_code());
    assert_eq!(error1.code_name(), error2.code_name());
    assert_eq!(error1.to_string(), error2.to_string());
}

#[test]
fn timeout_errors_with_different_durations_have_same_exit_code() {
    let error1 = LocalCopyError::timeout(Duration::from_secs(30));
    let error2 = LocalCopyError::timeout(Duration::from_secs(60));

    assert_eq!(error1.exit_code(), error2.exit_code());
    assert_eq!(error1.code_name(), error2.code_name());
    // But messages differ
    assert_ne!(error1.to_string(), error2.to_string());
}

// =============================================================================
// File List Exchange Timeout Tests (Conceptual)
// =============================================================================

/// Timeout during file list exchange should produce the same error type
/// as timeout during file transfer.
#[test]
fn file_list_timeout_same_error_type() {
    // Whether timeout occurs during:
    // - Initial file list building
    // - File list transmission
    // - File list reception
    // - Actual file transfer
    // The error type and exit code should be consistent

    let flist_timeout = LocalCopyError::timeout(Duration::from_secs(30));
    let transfer_timeout = LocalCopyError::timeout(Duration::from_secs(30));

    assert_eq!(flist_timeout.exit_code(), transfer_timeout.exit_code());
    assert_eq!(flist_timeout.code_name(), transfer_timeout.code_name());
}

// =============================================================================
// Timeout with Context Tests
// =============================================================================

#[test]
fn timeout_error_preserves_duration_in_kind() {
    let duration = Duration::from_secs(45);
    let error = LocalCopyError::timeout(duration);

    if let LocalCopyErrorKind::Timeout { duration: d } = error.kind() {
        assert_eq!(*d, duration);
    } else {
        panic!("Expected Timeout kind");
    }
}

#[test]
fn timeout_kind_as_io_returns_none() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    // Timeout errors don't have I/O context
    assert!(error.kind().as_io().is_none());
}

// =============================================================================
// Timeout Constant Validation Tests
// =============================================================================

#[test]
fn timeout_exit_code_constant_is_30() {
    assert_eq!(super::filter_program::TIMEOUT_EXIT_CODE, 30);
}

#[test]
fn timeout_exit_code_matches_core_exit_code() {
    // The engine's TIMEOUT_EXIT_CODE should match core::exit_code::ExitCode::Timeout
    // ExitCode::Timeout = 30 (from core/src/exit_code.rs)
    assert_eq!(super::filter_program::TIMEOUT_EXIT_CODE, 30);
}

// =============================================================================
// Timeout Option Wiring Tests
// =============================================================================

/// Verifies that `LocalCopyOptions::with_timeout(Some(...))` correctly stores
/// the value and `timeout()` returns it.
#[test]
fn options_with_timeout_stores_and_retrieves_value() {
    let duration = Duration::from_secs(45);
    let opts = LocalCopyOptions::new().with_timeout(Some(duration));
    assert_eq!(opts.timeout(), Some(duration));
}

/// Setting timeout to `None` effectively disables inactivity timeout.
#[test]
fn options_with_timeout_none_disables_timeout() {
    let opts = LocalCopyOptions::new()
        .with_timeout(Some(Duration::from_secs(60)))
        .with_timeout(None);
    assert!(opts.timeout().is_none());
}

/// Default options have no timeout configured.
#[test]
fn options_default_has_no_timeout() {
    let opts = LocalCopyOptions::new();
    assert!(opts.timeout().is_none());
}

/// Verify that `Default::default()` also yields no timeout.
#[test]
fn options_default_trait_has_no_timeout() {
    let opts = LocalCopyOptions::default();
    assert!(opts.timeout().is_none());
}

/// Timeout can be overwritten multiple times; only the last value applies.
#[test]
fn options_timeout_last_write_wins() {
    let opts = LocalCopyOptions::new()
        .with_timeout(Some(Duration::from_secs(10)))
        .with_timeout(Some(Duration::from_secs(30)))
        .with_timeout(Some(Duration::from_secs(120)));
    assert_eq!(opts.timeout(), Some(Duration::from_secs(120)));
}

/// A very small timeout (1 ms) is faithfully stored.
#[test]
fn options_with_very_small_timeout() {
    let duration = Duration::from_millis(1);
    let opts = LocalCopyOptions::new().with_timeout(Some(duration));
    assert_eq!(opts.timeout(), Some(duration));
}

/// A very large timeout (24 hours) is faithfully stored.
#[test]
fn options_with_very_large_timeout() {
    let duration = Duration::from_secs(86400);
    let opts = LocalCopyOptions::new().with_timeout(Some(duration));
    assert_eq!(opts.timeout(), Some(duration));
}

/// Timeout with zero duration is technically stored (means "timeout
/// immediately"), even though upstream rsync treats 0 as "disable".
/// The LocalCopyOptions layer preserves the value as-is; the
/// upstream semantics are handled at the CLI parsing layer.
#[test]
fn options_with_zero_duration_timeout() {
    let duration = Duration::from_secs(0);
    let opts = LocalCopyOptions::new().with_timeout(Some(duration));
    assert_eq!(opts.timeout(), Some(Duration::ZERO));
}

// =============================================================================
// Stop-At Option Wiring Tests
// =============================================================================

/// Verify stop_at option can be set and read back.
#[test]
fn options_stop_at_stores_and_retrieves_deadline() {
    let deadline = SystemTime::now();
    let opts = LocalCopyOptions::new().with_stop_at(Some(deadline));
    assert!(opts.stop_at().is_some());
}

/// Setting stop_at to None clears any previously set deadline.
#[test]
fn options_stop_at_none_clears_deadline() {
    let deadline = SystemTime::now();
    let opts = LocalCopyOptions::new()
        .with_stop_at(Some(deadline))
        .with_stop_at(None);
    assert!(opts.stop_at().is_none());
}

/// Default options have no stop-at deadline.
#[test]
fn options_default_has_no_stop_at() {
    let opts = LocalCopyOptions::new();
    assert!(opts.stop_at().is_none());
}

// =============================================================================
// Timeout Error `is_io_error()` Method Tests
// =============================================================================

/// Timeout errors are *not* I/O errors; `is_io_error()` must return false.
#[test]
fn timeout_error_is_not_io_error() {
    let error = LocalCopyError::timeout(Duration::from_secs(30));
    assert!(!error.is_io_error());
}

/// Stop-at errors are also not I/O errors.
#[test]
fn stop_at_error_is_not_io_error() {
    let error = LocalCopyError::stop_at_reached(SystemTime::now());
    assert!(!error.is_io_error());
}

/// Only `Io` variant errors report `is_io_error() == true`.
#[test]
fn io_error_is_io_error() {
    let error = LocalCopyError::io(
        "read",
        PathBuf::from("/tmp/test"),
        io::Error::new(io::ErrorKind::TimedOut, "operation timed out"),
    );
    assert!(error.is_io_error());
}

// =============================================================================
// Timeout and Stop-At Interaction Tests
// =============================================================================

/// Both timeout and stop-at can be configured simultaneously.
#[test]
fn options_timeout_and_stop_at_coexist() {
    let timeout = Duration::from_secs(60);
    let deadline = SystemTime::now();
    let opts = LocalCopyOptions::new()
        .with_timeout(Some(timeout))
        .with_stop_at(Some(deadline));
    assert_eq!(opts.timeout(), Some(timeout));
    assert!(opts.stop_at().is_some());
}

/// Clearing timeout does not affect stop-at, and vice versa.
#[test]
fn options_clearing_timeout_preserves_stop_at() {
    let deadline = SystemTime::now();
    let opts = LocalCopyOptions::new()
        .with_timeout(Some(Duration::from_secs(60)))
        .with_stop_at(Some(deadline))
        .with_timeout(None);
    assert!(opts.timeout().is_none());
    assert!(opts.stop_at().is_some());
}

#[test]
fn options_clearing_stop_at_preserves_timeout() {
    let deadline = SystemTime::now();
    let opts = LocalCopyOptions::new()
        .with_timeout(Some(Duration::from_secs(60)))
        .with_stop_at(Some(deadline))
        .with_stop_at(None);
    assert_eq!(opts.timeout(), Some(Duration::from_secs(60)));
    assert!(opts.stop_at().is_none());
}

// =============================================================================
// Stop-At Error Details Tests
// =============================================================================

/// Stop-at error message mentions "stopping at requested limit".
#[test]
fn stop_at_error_message_mentions_stopping() {
    let error = LocalCopyError::stop_at_reached(SystemTime::now());
    let message = error.to_string();
    assert!(
        message.contains("stopping"),
        "message should mention stopping: {message}"
    );
}

/// Stop-at error preserves the deadline in the kind.
#[test]
fn stop_at_error_preserves_deadline_in_kind() {
    let deadline = SystemTime::now();
    let error = LocalCopyError::stop_at_reached(deadline);
    match error.kind() {
        LocalCopyErrorKind::StopAtReached { target } => {
            assert_eq!(*target, deadline);
        }
        _ => panic!("Expected StopAtReached variant"),
    }
}
