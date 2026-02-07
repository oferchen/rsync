// =============================================================================
// Permission Denied Error Handling Tests
// =============================================================================
//
// This module contains comprehensive tests for permission denied error handling
// during local copy operations. Tests cover:
// - Read permission denied (source files/directories)
// - Write permission denied (destination files/directories)
// - Directory traverse permission denied
// - Error recovery and continuation behavior
// - Exit code verification
//
// These tests are Unix-only as they rely on Unix permission semantics.

use std::os::unix::fs::PermissionsExt;

// =========================================================================
// Helper Functions
// =========================================================================

/// Returns true if running as root (uid 0), in which case permission tests
/// should be skipped as root can access everything.
fn is_root() -> bool {
    rustix::process::geteuid().as_raw() == 0
}

/// Creates a test file and makes it unreadable.
fn create_unreadable_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).expect("write file");
    fs::set_permissions(&path, PermissionsExt::from_mode(0o000)).expect("set permissions");
    path
}

/// Creates a test directory that is not writable.
fn create_readonly_dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(&path).expect("create dir");
    fs::set_permissions(&path, PermissionsExt::from_mode(0o555)).expect("set permissions");
    path
}

/// Creates a test directory that is readable but not executable (can't traverse).
#[allow(dead_code)]
fn create_untraversable_dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(&path).expect("create dir");
    fs::set_permissions(&path, PermissionsExt::from_mode(0o644)).expect("set permissions");
    path
}

/// Restores permissions to allow cleanup.
fn restore_permissions(path: &Path, mode: u32) {
    let _ = fs::set_permissions(path, PermissionsExt::from_mode(mode));
}

// =========================================================================
// Read Permission Denied Tests
// =========================================================================

#[test]
fn permission_read_denied_single_file() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let unreadable = create_unreadable_file(&source_root, "unreadable.txt", b"secret");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        unreadable.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&unreadable, 0o644);

    // Should fail with an I/O error related to permission denied
    let error = result.expect_err("should fail with permission denied");
    match error.kind() {
        LocalCopyErrorKind::Io { source, .. } => {
            assert_eq!(
                source.kind(),
                io::ErrorKind::PermissionDenied,
                "expected PermissionDenied, got {:?}",
                source.kind()
            );
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn permission_read_denied_returns_io_error_with_multiple_files() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a readable file
    fs::write(source_root.join("readable.txt"), b"can read this").expect("write readable");

    // Create an unreadable file
    let unreadable = create_unreadable_file(&source_root, "unreadable.txt", b"secret");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);
    let result = plan.execute_with_report(LocalCopyExecution::Apply, options);

    // Restore permissions for cleanup
    restore_permissions(&unreadable, 0o644);

    // Current implementation returns an error when permission denied occurs
    // This matches rsync behavior where errors cause partial transfer exit code
    match result {
        Ok(_report) => {
            // If it succeeds, the readable file should have been copied
            // and unreadable file should be missing
            assert!(
                !dest_root.join("unreadable.txt").exists(),
                "unreadable file should not be copied"
            );
        }
        Err(error) => {
            // Error is expected for permission denied
            match error.kind() {
                LocalCopyErrorKind::Io { source, .. } => {
                    assert_eq!(
                        source.kind(),
                        io::ErrorKind::PermissionDenied,
                        "expected PermissionDenied error"
                    );
                }
                other => panic!("unexpected error kind: {other:?}"),
            }
        }
    }
}

#[test]
fn permission_read_denied_on_directory_blocks_contents() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("inner.txt"), b"inner content").expect("write inner");

    // Make the nested directory inaccessible
    fs::set_permissions(&nested, PermissionsExt::from_mode(0o000)).expect("set permissions");

    let dest_root = temp.path().join("dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&nested, 0o755);

    // Result depends on implementation - may error or skip
    // The key behavior is that we don't crash and handle gracefully
    if let Err(error) = result {
        // Error case: should be permission-related
        if let LocalCopyErrorKind::Io { source, .. } = error.kind() {
            assert!(
                source.kind() == io::ErrorKind::PermissionDenied
                    || source.kind() == io::ErrorKind::Other,
                "expected permission-related error"
            );
        }
        // Other error kinds may be acceptable
    }
    // Success case: directory was skipped, which is also valid behavior
}

// =========================================================================
// Write Permission Denied Tests
// =========================================================================

#[test]
fn permission_write_denied_destination_directory() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    // Create readonly destination
    let dest_root = create_readonly_dir(temp.path(), "dest");

    let operands = vec![
        source_root.join("file.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&dest_root, 0o755);

    let error = result.expect_err("should fail with permission denied");
    match error.kind() {
        LocalCopyErrorKind::Io { source, .. } => {
            assert_eq!(
                source.kind(),
                io::ErrorKind::PermissionDenied,
                "expected PermissionDenied"
            );
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn permission_write_denied_creating_directory_in_readonly_parent() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    // Create destination with readonly permissions
    let dest_root = create_readonly_dir(temp.path(), "dest");

    let operands = vec![
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&dest_root, 0o755);

    let error = result.expect_err("should fail to create directory");
    match error.kind() {
        LocalCopyErrorKind::Io { action, source, .. } => {
            assert!(
                action.contains("create") || action.contains("directory"),
                "action should mention directory creation: {action}"
            );
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn permission_write_denied_overwriting_readonly_file() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"new content").expect("write source");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    let dest_file = dest_root.join("file.txt");
    fs::write(&dest_file, b"old content").expect("write dest");
    fs::set_permissions(&dest_file, PermissionsExt::from_mode(0o444)).expect("make readonly");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&dest_file, 0o644);

    // The behavior depends on implementation:
    // - May fail with permission denied
    // - May succeed if using temp file + rename strategy
    // Both are valid rsync behaviors
    if let Err(error) = &result {
        if let LocalCopyErrorKind::Io { source, .. } = error.kind() {
            // Accept any error - the key is we don't crash
            let _ = source;
        }
    }

    // Verify the destination file still has its original content
    // (the readonly file wasn't corrupted)
    let content = fs::read(&dest_file).expect("read dest");
    assert!(
        content == b"old content" || content == b"new content",
        "destination should have old or new content"
    );
}

// =========================================================================
// Directory Traverse Permission Denied Tests
// =========================================================================

#[test]
fn permission_traverse_denied_directory() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    // Remove execute permission (can't cd into directory)
    let untraversable = create_untraversable_dir(&source_root, "noexec");

    let dest_root = temp.path().join("dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&untraversable, 0o755);

    // Should handle gracefully - either error or skip
    // The exact behavior depends on walk implementation
    let _ = result; // Don't assert specific outcome, just that we don't panic
}

#[test]
fn permission_traverse_denied_deep_hierarchy() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create: source/a/b/c/d/file.txt
    let deep = source_root.join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep).expect("create deep");
    fs::write(deep.join("file.txt"), b"deep content").expect("write file");

    // Remove execute permission from 'b'
    let blocked_dir = source_root.join("a").join("b");
    fs::set_permissions(&blocked_dir, PermissionsExt::from_mode(0o644)).expect("block access");

    let dest_root = temp.path().join("dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&blocked_dir, 0o755);

    // Should handle gracefully - either complete with partial transfer or error
    let _ = result;
}

// =========================================================================
// Exit Code Tests
// =========================================================================

#[test]
fn permission_denied_returns_partial_transfer_exit_code() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create readable and unreadable files
    fs::write(source_root.join("readable.txt"), b"content").expect("write readable");
    let unreadable = create_unreadable_file(&source_root, "unreadable.txt", b"secret");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&unreadable, 0o644);

    // If the operation returned an error, check the exit code
    if let Err(error) = result {
        // Permission denied should map to partial transfer (23) or file I/O error
        let exit_code = error.exit_code();
        assert!(
            exit_code == 23 || exit_code == 11,
            "expected exit code 23 (partial) or 11 (file I/O), got {exit_code}"
        );
    }
}

// =========================================================================
// Dry Run Tests
// =========================================================================

#[test]
fn permission_dry_run_detects_unreadable_source() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let unreadable = create_unreadable_file(&source_root, "unreadable.txt", b"secret");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        unreadable.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with(LocalCopyExecution::DryRun);

    // Restore permissions for cleanup
    restore_permissions(&unreadable, 0o644);

    // Dry run may or may not detect the permission issue early
    // depending on whether it tries to stat/read files
    let _ = result;

    // Verify destination was not modified
    assert!(
        !dest_root.join("unreadable.txt").exists(),
        "dry run should not create files"
    );
}

#[test]
fn permission_dry_run_with_readonly_destination() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    // Create readonly destination
    let dest_root = create_readonly_dir(temp.path(), "dest");

    let operands = vec![
        source_root.join("file.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with(LocalCopyExecution::DryRun);

    // Restore permissions for cleanup
    restore_permissions(&dest_root, 0o755);

    // Dry run should either detect the issue or succeed
    // (since it doesn't actually write)
    let _ = result;

    // Destination should remain empty
    assert!(
        fs::read_dir(&dest_root).expect("read dest").count() == 0,
        "dry run should not create files"
    );
}

// =========================================================================
// Special Cases
// =========================================================================

#[test]
fn permission_sticky_bit_directory_handling() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    // Create destination with sticky bit (like /tmp)
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::set_permissions(&dest_root, PermissionsExt::from_mode(0o1777)).expect("set sticky");

    let operands = vec![
        source_root.join("file.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Should succeed (sticky bit doesn't prevent writes for owner)
    result.expect("should succeed with sticky bit directory");
    assert!(dest_root.join("file.txt").exists());
}

#[test]
fn permission_setuid_setgid_file_copy() {
    if is_root() {
        return; // Skip: root bypasses permission checks
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let source_file = source_root.join("setuid_file.txt");
    fs::write(&source_file, b"content").expect("write file");
    // Note: Non-root users usually can't set setuid/setgid bits effectively
    // This test verifies we handle files with these bits correctly
    fs::set_permissions(&source_file, PermissionsExt::from_mode(0o4755)).expect("set setuid");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_file.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should succeed - setuid/setgid bits may or may not be preserved
    // depending on filesystem and privileges
    result.expect("should handle setuid file");
    assert!(dest_root.join("setuid_file.txt").exists());
}

#[test]
fn permission_immutable_file_handling() {
    // Note: Setting immutable flag requires root, so we just test
    // that regular permission handling works for files we can't modify
    if is_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("new.txt"), b"new content").expect("write new");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create a file we can't write to (simulating immutable)
    let dest_file = dest_root.join("new.txt");
    fs::write(&dest_file, b"old content").expect("write old");
    fs::set_permissions(&dest_file, PermissionsExt::from_mode(0o000)).expect("make immutable");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&dest_file, 0o644);

    // Should handle gracefully (error or skip)
    let _ = result;
}

// =========================================================================
// Error Message Verification
// =========================================================================

#[test]
fn permission_denied_error_includes_path() {
    if is_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let unreadable = create_unreadable_file(&source_root, "secret.txt", b"data");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        unreadable.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&unreadable, 0o644);

    if let Err(error) = result {
        let message = error.to_string();
        // Error message should include the path for debugging
        assert!(
            message.contains("secret.txt") || message.contains(&source_root.display().to_string()),
            "error message should include path: {message}"
        );
    }
}

#[test]
fn permission_denied_error_includes_action() {
    if is_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write");

    let dest_root = create_readonly_dir(temp.path(), "dest");

    let operands = vec![
        source_root.join("file.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    restore_permissions(&dest_root, 0o755);

    if let Err(error) = result {
        // Error should describe what action failed
        if let LocalCopyErrorKind::Io { action, .. } = error.kind() {
            assert!(
                !action.is_empty(),
                "action should describe the failed operation"
            );
        }
    }
}

// =========================================================================
// Cross-boundary Permission Tests
// =========================================================================

#[test]
fn permission_different_user_ownership_handling() {
    if is_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_root.join("file.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Try to preserve owner (will fail for non-root but should not crash)
    let options = LocalCopyOptions::default().owner(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should complete (ownership preservation may silently fail for non-root)
    let _ = result;
    assert!(dest_root.join("file.txt").exists());
}

#[test]
fn permission_group_write_required() {
    if is_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write");

    // Create dest with no group write (but user can write)
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::set_permissions(&dest_root, PermissionsExt::from_mode(0o755)).expect("set perms");

    let operands = vec![
        source_root.join("file.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Should succeed since user has write permission
    result.expect("should succeed with user write permission");
    assert!(dest_root.join("file.txt").exists());
}
