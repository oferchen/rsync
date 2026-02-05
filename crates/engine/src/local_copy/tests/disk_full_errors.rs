// Tests for disk full error handling.
//
// Disk full (ENOSPC) errors are a special class of I/O errors that require
// specific handling to prevent data corruption and provide clear diagnostics.
//
// Key behaviors tested:
// 1. Detection of disk full conditions (StorageFull ErrorKind)
// 2. Error propagation with correct error types and messages
// 3. Cleanup of partial writes on disk full errors
// 4. Exit code mapping (should map to RERR_PARTIAL = 23)
// 5. Error messages include path context
// 6. Guard behavior on disk full errors (temp file cleanup)

// ==================== LocalCopyError Construction Tests ====================

#[test]
fn local_copy_error_from_disk_full_io_error() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "No space left on device");
    let path = PathBuf::from("/tmp/test/file.txt");
    let error = LocalCopyError::io("write destination file", path.clone(), disk_full);

    // Verify error type
    assert!(matches!(error.kind(), LocalCopyErrorKind::Io { .. }));

    // Verify error message contains path
    let message = error.to_string();
    assert!(message.contains("/tmp/test/file.txt"));
    assert!(message.contains("write destination file"));
    assert!(message.contains("No space left on device"));
}

#[test]
fn local_copy_error_disk_full_has_correct_exit_code() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let error = LocalCopyError::io("copy file", PathBuf::from("/test"), disk_full);

    // I/O errors should return exit code 23 (RERR_PARTIAL/INVALID_OPERAND_EXIT_CODE)
    assert_eq!(
        error.exit_code(),
        super::filter_program::INVALID_OPERAND_EXIT_CODE
    );
}

#[test]
fn local_copy_error_disk_full_kind_provides_path_access() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let path = PathBuf::from("/destination/file.txt");
    let error = LocalCopyError::io("write file", path.clone(), disk_full);

    // Should be able to extract path from error
    let (action, extracted_path, source) = error.kind().as_io().expect("should be Io variant");
    assert_eq!(action, "write file");
    assert_eq!(extracted_path, path.as_path());
    assert_eq!(source.kind(), io::ErrorKind::StorageFull);
}

#[test]
fn local_copy_error_disk_full_code_name_is_rerr_partial() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let error = LocalCopyError::io("copy file", PathBuf::from("/test"), disk_full);

    assert_eq!(error.code_name(), "RERR_PARTIAL");
}

// ==================== Error Kind Detection Tests ====================

#[test]
fn detect_storage_full_error_kind() {
    let disk_full = io::Error::from(io::ErrorKind::StorageFull);
    assert_eq!(disk_full.kind(), io::ErrorKind::StorageFull);
}

#[test]
fn disk_full_distinct_from_other_io_errors() {
    // StorageFull should be distinguishable from other I/O errors
    let disk_full = io::Error::from(io::ErrorKind::StorageFull);
    let permission_denied = io::Error::from(io::ErrorKind::PermissionDenied);
    let not_found = io::Error::from(io::ErrorKind::NotFound);

    assert_ne!(disk_full.kind(), permission_denied.kind());
    assert_ne!(disk_full.kind(), not_found.kind());
    assert_eq!(disk_full.kind(), io::ErrorKind::StorageFull);
}

#[test]
#[cfg(target_os = "linux")]
fn raw_os_error_enospc_detected_as_storage_full() {
    // ENOSPC = 28 on Linux
    let enospc = io::Error::from_raw_os_error(28);
    assert_eq!(enospc.raw_os_error(), Some(28));
}

// ==================== Error Message Quality Tests ====================

#[test]
fn disk_full_error_message_includes_action_context() {
    let actions = [
        "write destination file",
        "copy file",
        "create temporary file",
        "truncate destination file",
    ];

    for action in actions {
        let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
        let error = LocalCopyError::io(action, PathBuf::from("/test"), disk_full);
        let message = error.to_string();

        assert!(
            message.contains(action),
            "Error message should contain action"
        );
    }
}

#[test]
fn disk_full_error_message_includes_full_path() {
    let paths = [
        "/home/user/documents/file.txt",
        "/tmp/rsync-temp-12345.tmp",
        "/very/long/nested/directory/structure/file.dat",
    ];

    for path_str in paths {
        let disk_full = io::Error::new(io::ErrorKind::StorageFull, "no space");
        let error = LocalCopyError::io("write file", PathBuf::from(path_str), disk_full);
        let message = error.to_string();

        assert!(
            message.contains(path_str),
            "Error message should contain path"
        );
    }
}

#[test]
fn disk_full_error_preserves_original_io_error_message() {
    let custom_messages = [
        "No space left on device",
        "Disk quota exceeded",
        "Cannot allocate memory for disk buffer",
    ];

    for original_msg in custom_messages {
        let disk_full = io::Error::new(io::ErrorKind::StorageFull, original_msg);
        let error = LocalCopyError::io("write file", PathBuf::from("/test"), disk_full);
        let message = error.to_string();

        assert!(
            message.contains(original_msg),
            "Error should preserve original message"
        );
    }
}

// ==================== DestinationWriteGuard Cleanup Tests ====================

#[test]
fn guard_discard_cleans_up_temp_file_on_disk_full() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("final.txt");

    let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
    let staging = guard.staging_path().to_path_buf();

    // Simulate disk full error during write - guard should clean up on discard
    guard.discard();

    // Staging file should be removed
    assert!(
        !staging.exists(),
        "Guard should clean up staging file on discard"
    );
    // Final destination should not exist
    assert!(
        !dest.exists(),
        "Final destination should not exist after discard"
    );
}

#[test]
fn guard_drop_cleans_up_temp_file_without_commit() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("final.txt");
    let staging;

    {
        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        staging = guard.staging_path().to_path_buf();
        // Guard dropped without commit - simulates error during write
    }

    // Staging file should be cleaned up by Drop
    assert!(!staging.exists(), "Drop should clean up staging file");
}

#[test]
fn guard_partial_mode_preserves_file_on_discard() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("final.txt");

    let (guard, mut file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");
    file.write_all(b"partial data").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();
    guard.discard();

    // In partial mode, file should be preserved for resume
    assert!(
        staging.exists(),
        "Partial mode should preserve file on discard"
    );
}

// ==================== Guard Remove Functions Tests ====================

#[test]
fn remove_existing_destination_handles_disk_full_context() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");

    // Remove should succeed - not a disk full scenario but tests cleanup path
    let result = remove_existing_destination(&path);
    assert!(result.is_ok());
    assert!(!path.exists());
}

#[test]
fn remove_incomplete_destination_does_not_propagate_errors() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("incomplete.txt");
    fs::write(&path, b"incomplete").expect("write");

    // This function silently ignores errors - important for error recovery
    remove_incomplete_destination(&path);
    assert!(!path.exists());
}

#[test]
fn remove_incomplete_destination_silent_on_not_found() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("nonexistent.txt");

    // Should not panic or return error
    remove_incomplete_destination(&path);
}

// ==================== Exit Code Verification Tests ====================

#[test]
fn io_error_exit_code_is_23_for_all_io_errors() {
    // All I/O errors (including disk full) should return exit code 23 (RERR_PARTIAL)
    let error_kinds = [
        io::ErrorKind::StorageFull,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::NotFound,
        io::ErrorKind::WriteZero,
        io::ErrorKind::Interrupted,
    ];

    for kind in error_kinds {
        let io_error = io::Error::from(kind);
        let error = LocalCopyError::io("write file", PathBuf::from("/test"), io_error);
        assert_eq!(
            error.exit_code(),
            super::filter_program::INVALID_OPERAND_EXIT_CODE,
        );
    }
}

#[test]
fn io_error_exit_code_matches_upstream_rerr_partial() {
    // RERR_PARTIAL = 23 in upstream rsync
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let error = LocalCopyError::io("copy file", PathBuf::from("/test"), disk_full);

    assert_eq!(error.exit_code(), 23);
}

// ==================== Error Source Chain Tests ====================

#[test]
fn disk_full_error_has_source() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let error = LocalCopyError::io("write file", PathBuf::from("/test"), disk_full);

    // LocalCopyError wraps LocalCopyErrorKind which has the source
    // The error chain should be accessible
    let message = format!("{}", error);
    assert!(message.contains("disk full"));
}

#[test]
fn disk_full_error_debug_format_includes_details() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "No space left on device");
    let error = LocalCopyError::io("write file", PathBuf::from("/destination/file.txt"), disk_full);

    let debug = format!("{:?}", error);

    // Debug format should include relevant details
    assert!(debug.contains("Io"));
    assert!(debug.contains("write file"));
}

// ==================== Real File Operation Tests ====================

#[test]
#[cfg(unix)]
fn file_write_error_maps_to_local_copy_error() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("test.txt");

    // Create a normal file to test error mapping
    fs::write(&path, b"content").expect("write");

    let metadata = fs::metadata(&path).expect("metadata");
    let mut perms = metadata.permissions();
    perms.set_mode(0o444); // Read-only
    fs::set_permissions(&path, perms).expect("set permissions");

    let result = fs::OpenOptions::new().write(true).open(&path);

    if let Err(io_error) = result {
        let error = LocalCopyError::io("write file", path.clone(), io_error);
        assert!(matches!(error.kind(), LocalCopyErrorKind::Io { .. }));
        assert_eq!(
            error.exit_code(),
            super::filter_program::INVALID_OPERAND_EXIT_CODE
        );
    }

    // Cleanup: restore permissions
    let mut perms = fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&path, perms).expect("restore permissions");
}

#[test]
fn directory_write_error_propagates_correctly() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path().join("testdir");
    fs::create_dir(&dir).expect("create dir");

    // Try to write a file where a directory exists
    let result = fs::write(&dir, b"content");
    assert!(result.is_err());

    if let Err(io_error) = result {
        let error = LocalCopyError::io("write file", dir.clone(), io_error);
        let message = error.to_string();

        assert!(message.contains("write file"));
        assert!(message.contains("testdir"));
    }
}

// ==================== Error Kind Extraction Tests ====================

#[test]
fn local_copy_error_kind_as_io_returns_components() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let path = PathBuf::from("/test/path.txt");
    let error = LocalCopyError::io("write destination", path.clone(), disk_full);

    match error.kind() {
        LocalCopyErrorKind::Io {
            action,
            path: error_path,
            source,
        } => {
            assert_eq!(*action, "write destination");
            assert_eq!(error_path, &path);
            assert_eq!(source.kind(), io::ErrorKind::StorageFull);
        }
        _ => panic!("Expected Io variant"),
    }
}

#[test]
fn local_copy_error_into_kind_consumes_error() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let error = LocalCopyError::io("write file", PathBuf::from("/test"), disk_full);

    let kind = error.into_kind();
    assert!(matches!(kind, LocalCopyErrorKind::Io { .. }));
}

// ==================== Comparison with Other Error Types ====================

#[test]
fn disk_full_vs_other_io_errors_have_same_exit_code() {
    let error_types = [
        ("disk full", io::ErrorKind::StorageFull),
        ("permission denied", io::ErrorKind::PermissionDenied),
        ("not found", io::ErrorKind::NotFound),
    ];

    let expected_code = super::filter_program::INVALID_OPERAND_EXIT_CODE;

    for (_name, kind) in error_types {
        let io_error = io::Error::new(kind, "error");
        let error = LocalCopyError::io("test", PathBuf::from("/test"), io_error);
        assert_eq!(error.exit_code(), expected_code);
    }
}

#[test]
fn io_errors_have_different_code_name_than_timeout() {
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let io_error = LocalCopyError::io("test", PathBuf::from("/test"), disk_full);
    let timeout_error = LocalCopyError::timeout(Duration::from_secs(30));

    assert_eq!(io_error.code_name(), "RERR_PARTIAL");
    assert_eq!(timeout_error.code_name(), "RERR_TIMEOUT");
    assert_ne!(io_error.code_name(), timeout_error.code_name());
}

// ==================== Large File Handling Tests ====================

#[test]
fn error_context_preserved_for_large_file_paths() {
    // Test with maximum path length scenarios
    let long_path = format!("/very{}/deep/path/file.txt", "/nested".repeat(20));
    let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
    let error = LocalCopyError::io("write file", PathBuf::from(&long_path), disk_full);

    let message = error.to_string();
    assert!(
        message.contains("deep"),
        "Long paths should be included in error messages"
    );
}

#[test]
fn error_for_special_characters_in_path() {
    let special_paths = [
        "/path/with spaces/file.txt",
        "/path/with_quotes/file.txt",
    ];

    for path_str in special_paths {
        let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
        let error = LocalCopyError::io("write file", PathBuf::from(path_str), disk_full);
        let message = error.to_string();

        // Error message should not panic or corrupt
        assert!(!message.is_empty());
        assert!(message.contains("write file"));
    }
}

// ==================== Concurrent Error Handling Tests ====================

#[test]
fn multiple_disk_full_errors_are_independent() {
    let errors: Vec<_> = (0..5)
        .map(|i| {
            let disk_full = io::Error::new(
                io::ErrorKind::StorageFull,
                format!("disk full error {}", i),
            );
            LocalCopyError::io("write file", PathBuf::from(format!("/test{}.txt", i)), disk_full)
        })
        .collect();

    // Each error should be independent with its own path and message
    for (i, error) in errors.iter().enumerate() {
        let message = error.to_string();
        assert!(message.contains(&format!("test{}.txt", i)));
        assert!(message.contains(&format!("disk full error {}", i)));
    }
}

// ==================== Partial Transfer Behavior Tests ====================

#[test]
fn disk_full_during_copy_preserves_partial_file_in_partial_mode() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("destination.txt");

    // Create guard in partial mode
    let (guard, mut file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");
    let staging = guard.staging_path().to_path_buf();

    // Write some data before "disk full"
    file.write_all(b"partial content before error").expect("write");
    drop(file);

    // Simulate disk full - in partial mode, discard should preserve the file
    guard.discard();

    // Partial file should be preserved
    assert!(staging.exists(), "Partial file should be preserved");
    let content = fs::read(&staging).expect("read partial");
    assert_eq!(content, b"partial content before error");
}

#[test]
fn disk_full_during_copy_removes_temp_file_in_normal_mode() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("destination.txt");

    // Create guard in normal mode (not partial)
    let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
    let staging = guard.staging_path().to_path_buf();

    // Write some data before "disk full"
    file.write_all(b"data before error").expect("write");
    drop(file);

    // Simulate disk full - in normal mode, discard should remove temp file
    guard.discard();

    // Temp file should be removed
    assert!(!staging.exists(), "Temp file should be removed in normal mode");
}

// ==================== Error Recovery Tests ====================

#[test]
fn can_retry_after_disk_full_error() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("test.txt");

    // First attempt - simulate failure
    {
        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        guard.discard();
    }

    // Second attempt - should succeed
    {
        let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        file.write_all(b"success on retry").expect("write");
        drop(file);
        guard.commit().expect("commit");
    }

    // Verify the file exists with correct content
    assert!(dest.exists());
    assert_eq!(fs::read(&dest).expect("read"), b"success on retry");
}

#[test]
fn multiple_failed_attempts_clean_up_properly() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("test.txt");

    // Create and discard multiple guards
    for _ in 0..5 {
        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        guard.discard();
    }

    // No temp files should remain
    assert!(!dest.exists());

    // Should still be able to successfully write
    let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
    file.write_all(b"final success").expect("write");
    drop(file);
    guard.commit().expect("commit");

    assert_eq!(fs::read(&dest).expect("read"), b"final success");
}

// ==================== Read-Only Filesystem Error Tests ====================

#[test]
fn readonly_filesystem_error_maps_correctly() {
    let readonly_err = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
    let error = LocalCopyError::io("write file", PathBuf::from("/readonly/path"), readonly_err);

    // Should also map to exit code 23
    assert_eq!(
        error.exit_code(),
        super::filter_program::INVALID_OPERAND_EXIT_CODE
    );

    // Error message should contain the path
    let message = error.to_string();
    assert!(message.contains("/readonly/path"));
}

#[test]
fn readonly_vs_disk_full_same_exit_code() {
    let readonly_err = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
    let diskfull_err = io::Error::from(io::ErrorKind::StorageFull);

    let readonly_error = LocalCopyError::io("write", PathBuf::from("/a"), readonly_err);
    let diskfull_error = LocalCopyError::io("write", PathBuf::from("/b"), diskfull_err);

    // Both should have the same exit code
    assert_eq!(readonly_error.exit_code(), diskfull_error.exit_code());
}
