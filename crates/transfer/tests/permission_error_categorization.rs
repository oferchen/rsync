//! Comprehensive tests for permission denied error categorization.
//!
//! This module tests the error categorization logic in the transfer crate,
//! verifying that permission denied errors are correctly classified as
//! recoverable errors that allow the transfer to continue with other files.

use std::io;
use std::path::Path;

use transfer::error::{
    categorize_io_error, DeltaFatalError, DeltaRecoverableError, DeltaTransferError,
};

// =============================================================================
// Permission Denied Error Categorization
// =============================================================================

#[test]
fn categorize_permission_denied_as_recoverable() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/test/file.txt");

    let categorized = categorize_io_error(err, path, "read");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            path: p,
            operation: op,
        }) => {
            assert_eq!(p, path);
            assert_eq!(op, "read");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn categorize_permission_denied_for_open_operation() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/protected/secret.dat");

    let categorized = categorize_io_error(err, path, "open");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            operation,
            ..
        }) => {
            assert_eq!(operation, "open");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn categorize_permission_denied_for_write_operation() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/readonly/data.bin");

    let categorized = categorize_io_error(err, path, "write");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            operation,
            ..
        }) => {
            assert_eq!(operation, "write");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn categorize_permission_denied_for_stat_operation() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/restricted/metadata.txt");

    let categorized = categorize_io_error(err, path, "stat");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            operation,
            ..
        }) => {
            assert_eq!(operation, "stat");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn categorize_permission_denied_for_readdir_operation() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/private/directory");

    let categorized = categorize_io_error(err, path, "readdir");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            operation,
            ..
        }) => {
            assert_eq!(operation, "readdir");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

// =============================================================================
// Comparison with Fatal Errors
// =============================================================================

#[test]
fn verify_permission_denied_is_not_fatal() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/test.txt");

    let categorized = categorize_io_error(err, path, "read");

    assert!(
        matches!(categorized, DeltaTransferError::Recoverable(_)),
        "Permission denied should be recoverable, not fatal"
    );
}

#[test]
fn verify_disk_full_is_fatal_not_recoverable() {
    let err = io::Error::from(io::ErrorKind::StorageFull);
    let path = Path::new("/full/disk/file.txt");

    let categorized = categorize_io_error(err, path, "write");

    match categorized {
        DeltaTransferError::Fatal(DeltaFatalError::DiskFull { .. }) => {
            // Correct - disk full should be fatal
        }
        _ => panic!("Disk full should be a fatal error"),
    }
}

#[test]
fn verify_readonly_filesystem_is_fatal() {
    let err = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
    let path = Path::new("/readonly/mount/file.txt");

    let categorized = categorize_io_error(err, path, "write");

    match categorized {
        DeltaTransferError::Fatal(DeltaFatalError::ReadOnlyFilesystem { .. }) => {
            // Correct - read-only filesystem should be fatal
        }
        _ => panic!("Read-only filesystem should be a fatal error"),
    }
}

#[test]
fn verify_not_found_is_recoverable() {
    let err = io::Error::from(io::ErrorKind::NotFound);
    let path = Path::new("/missing/file.txt");

    let categorized = categorize_io_error(err, path, "open");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound { .. }) => {
            // Correct - file not found should be recoverable
        }
        _ => panic!("File not found should be recoverable"),
    }
}

// =============================================================================
// Error Display Tests
// =============================================================================

#[test]
fn permission_denied_error_displays_operation() {
    let err = DeltaRecoverableError::PermissionDenied {
        path: "/test/secret.txt".into(),
        operation: "open".to_owned(),
    };

    let message = err.to_string();
    assert!(message.contains("Permission denied"), "Should mention permission denied");
    assert!(message.contains("open"), "Should mention the operation");
    assert!(message.contains("/test/secret.txt"), "Should mention the path");
}

#[test]
fn permission_denied_error_displays_path() {
    let err = DeltaRecoverableError::PermissionDenied {
        path: "/deeply/nested/protected/file.dat".into(),
        operation: "read".to_owned(),
    };

    let message = err.to_string();
    assert!(
        message.contains("/deeply/nested/protected/file.dat"),
        "Should include full path"
    );
}

// =============================================================================
// Error Variant Verification
// =============================================================================

#[test]
fn recoverable_error_from_permission_denied() {
    let inner = DeltaRecoverableError::PermissionDenied {
        path: "/test.txt".into(),
        operation: "write".to_owned(),
    };
    let outer: DeltaTransferError = inner.into();

    assert!(matches!(
        outer,
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied { .. })
    ));
}

#[test]
fn fatal_error_is_distinct_from_recoverable() {
    let fatal = DeltaFatalError::DiskFull {
        path: "/full.txt".into(),
        bytes_needed: Some(1024),
    };
    let fatal_outer: DeltaTransferError = fatal.into();

    let recoverable = DeltaRecoverableError::PermissionDenied {
        path: "/denied.txt".into(),
        operation: "read".to_owned(),
    };
    let recoverable_outer: DeltaTransferError = recoverable.into();

    // They should be different variants
    assert!(matches!(fatal_outer, DeltaTransferError::Fatal(_)));
    assert!(matches!(recoverable_outer, DeltaTransferError::Recoverable(_)));
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn categorize_permission_denied_with_empty_path() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("");

    let categorized = categorize_io_error(err, path, "read");

    // Should still categorize correctly even with empty path
    assert!(matches!(
        categorized,
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied { .. })
    ));
}

#[test]
fn categorize_permission_denied_with_special_characters_in_path() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/path/with spaces/and\ttabs/file.txt");

    let categorized = categorize_io_error(err, path, "read");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            path: p,
            ..
        }) => {
            assert_eq!(p, path.to_path_buf());
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn categorize_permission_denied_with_unicode_path() {
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/unicode/\u{1F600}/\u{4E2D}\u{6587}/file.txt");

    let categorized = categorize_io_error(err, path, "read");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            path: p,
            ..
        }) => {
            assert_eq!(p, path.to_path_buf());
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

// =============================================================================
// Multiple Error Type Comparison
// =============================================================================

#[test]
fn compare_error_categorization_across_types() {
    let path = Path::new("/test/file.txt");

    // Create various error types and verify categorization
    let test_cases = [
        (io::ErrorKind::PermissionDenied, true, "permission_denied"),
        (io::ErrorKind::NotFound, true, "not_found"),
        (io::ErrorKind::StorageFull, false, "storage_full"),
        (io::ErrorKind::ReadOnlyFilesystem, false, "readonly_fs"),
        (io::ErrorKind::Interrupted, true, "interrupted"),
        (io::ErrorKind::WouldBlock, true, "would_block"),
    ];

    for (error_kind, should_be_recoverable, description) in test_cases {
        let err = io::Error::from(error_kind);
        let categorized = categorize_io_error(err, path, "test");

        let is_recoverable = matches!(categorized, DeltaTransferError::Recoverable(_));

        assert_eq!(
            is_recoverable, should_be_recoverable,
            "Error type {} should be {}, got {}",
            description,
            if should_be_recoverable {
                "recoverable"
            } else {
                "fatal"
            },
            if is_recoverable { "recoverable" } else { "fatal" }
        );
    }
}

// =============================================================================
// Error Source Chain Tests
// =============================================================================

#[test]
fn recoverable_io_error_has_source() {
    use std::error::Error;

    let inner = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
    let err = DeltaRecoverableError::Io {
        path: "/test.txt".into(),
        error: inner,
    };

    // The error should have a source
    assert!(err.source().is_some());
}

#[test]
fn fatal_io_error_has_source() {
    use std::error::Error;

    let inner = io::Error::new(io::ErrorKind::Other, "unknown error");
    let err = DeltaFatalError::Io(inner);

    // The error should have a source
    assert!(err.source().is_some());
}

// =============================================================================
// Real-world Scenario Tests
// =============================================================================

#[test]
fn scenario_protected_system_file() {
    // Simulating trying to read a protected system file
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/etc/shadow");

    let categorized = categorize_io_error(err, path, "open");

    // Should be recoverable - skip this file, continue with others
    assert!(
        matches!(categorized, DeltaTransferError::Recoverable(_)),
        "Protected system file should result in recoverable error"
    );
}

#[test]
fn scenario_restricted_directory() {
    // Simulating trying to list a restricted directory
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/root/.ssh");

    let categorized = categorize_io_error(err, path, "readdir");

    // Should be recoverable - skip this directory, continue with others
    assert!(
        matches!(categorized, DeltaTransferError::Recoverable(_)),
        "Restricted directory should result in recoverable error"
    );
}

#[test]
fn scenario_write_to_readonly_mount() {
    // Simulating trying to write to a read-only mounted filesystem
    let err = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
    let path = Path::new("/mnt/cdrom/file.txt");

    let categorized = categorize_io_error(err, path, "write");

    // Should be fatal - can't write anywhere on this mount
    assert!(
        matches!(categorized, DeltaTransferError::Fatal(_)),
        "Read-only filesystem should result in fatal error"
    );
}

#[test]
fn scenario_temp_file_creation_permission_denied() {
    // Simulating trying to create a temp file in a protected directory
    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/var/protected/.file.XXXXXX.tmp");

    let categorized = categorize_io_error(err, path, "create");

    // Should be recoverable for individual file operations
    assert!(
        matches!(categorized, DeltaTransferError::Recoverable(_)),
        "Temp file creation permission denied should be recoverable"
    );
}
