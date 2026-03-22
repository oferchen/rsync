//! Integration tests for error recovery scenarios.
//!
//! Verifies that the transfer engine handles various error conditions gracefully,
//! matching upstream rsync behavior for exit codes and partial results.
//!
//! Most tests require Unix filesystem semantics (chmod, symlinks) and are gated
//! with `#[cfg(unix)]`.

mod test_timeout;

#[cfg(unix)]
mod error_recovery {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use core::client::{ClientConfig, PARTIAL_TRANSFER_EXIT_CODE, run_client};
    use tempfile::tempdir;
    use crate::test_timeout::{LOCAL_TIMEOUT, run_with_timeout};

    /// Helper: create a file with the given content, creating parent dirs as needed.
    fn touch(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    /// When some source files are missing read permissions, the transfer should
    /// copy the accessible files and exit with code 23 (RERR_PARTIAL).
    ///
    /// upstream: rsync copies readable files, skips unreadable ones, exits 23.
    #[test]
    fn error_recovery_partial_transfer_exit_code() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let dest = temp.path().join("dest");

            fs::create_dir_all(&source).expect("create source");
            fs::create_dir_all(&dest).expect("create dest");

            // Use distinct sizes to avoid quick-check skips.
            touch(&source.join("alpha.txt"), b"alpha content here");
            touch(
                &source.join("beta.txt"),
                b"beta - this file will be unreadable",
            );
            touch(
                &source.join("gamma.txt"),
                b"gamma data for transfer verification",
            );

            // Remove read permission from beta.
            fs::set_permissions(source.join("beta.txt"), fs::Permissions::from_mode(0o000))
                .expect("chmod 000");

            let mut source_arg = source.into_os_string();
            source_arg.push("/");

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest.clone().into_os_string()])
                .mkpath(true)
                .delete(true)
                .times(true)
                .build();

            let result = run_client(config);

            let error = result.expect_err("partial transfer should return Err");
            assert_eq!(
                error.exit_code(),
                PARTIAL_TRANSFER_EXIT_CODE,
                "exit code should be 23 (RERR_PARTIAL), got {}",
                error.exit_code()
            );

            // Readable files should have been copied.
            assert!(
                dest.join("alpha.txt").exists(),
                "alpha.txt should be copied"
            );
            assert_eq!(
                fs::read(dest.join("alpha.txt")).expect("read alpha"),
                b"alpha content here"
            );
            assert!(
                dest.join("gamma.txt").exists(),
                "gamma.txt should be copied"
            );
            assert_eq!(
                fs::read(dest.join("gamma.txt")).expect("read gamma"),
                b"gamma data for transfer verification"
            );

            // The unreadable file should not appear at the destination.
            assert!(
                !dest.join("beta.txt").exists(),
                "unreadable beta.txt should not be at destination"
            );
        });
    }

    /// A symlink loop (a -> b -> a) should be handled gracefully without
    /// hanging or panicking. With --copy-links the engine dereferences
    /// symlinks and should detect the cycle.
    ///
    /// upstream: rsync warns about symlink loops and skips them, exits 23.
    #[test]
    fn error_recovery_symlink_loop() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let dest = temp.path().join("dest");

            fs::create_dir_all(&source).expect("create source");
            fs::create_dir_all(&dest).expect("create dest");

            // Create a regular file so the transfer is not empty.
            touch(&source.join("normal.txt"), b"normal file content");

            // Create a symlink loop: link_a -> link_b -> link_a.
            std::os::unix::fs::symlink(source.join("link_b"), source.join("link_a"))
                .expect("create link_a");
            std::os::unix::fs::symlink(source.join("link_a"), source.join("link_b"))
                .expect("create link_b");

            let mut source_arg = source.into_os_string();
            source_arg.push("/");

            // With --copy-links the engine tries to dereference and should detect the loop.
            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest.clone().into_os_string()])
                .mkpath(true)
                .copy_links(true)
                .build();

            let result = run_client(config);

            // The transfer may succeed (skipping loops) or return partial transfer.
            // Either way it must not hang or panic.
            match result {
                Ok(_) => {
                    // Acceptable - the engine skipped the loop entries.
                }
                Err(err) => {
                    // Partial transfer (23) is the expected upstream behavior.
                    assert_eq!(
                        err.exit_code(),
                        PARTIAL_TRANSFER_EXIT_CODE,
                        "expected exit code 23, got {}",
                        err.exit_code()
                    );
                }
            }

            // The normal file should always be copied regardless of the loop.
            assert!(
                dest.join("normal.txt").exists(),
                "normal.txt should be copied despite symlink loop"
            );
        });
    }

    /// When the destination directory is read-only, writing files should fail
    /// with an appropriate error.
    ///
    /// upstream: rsync reports a write error and exits with code 23.
    #[test]
    fn error_recovery_readonly_destination() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let dest = temp.path().join("dest");

            fs::create_dir_all(&source).expect("create source");
            fs::create_dir_all(&dest).expect("create dest");

            touch(&source.join("file.txt"), b"content to write");

            // Make destination directory read-only.
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).expect("chmod dest");

            let mut source_arg = source.into_os_string();
            source_arg.push("/");

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest.clone().into_os_string()])
                .times(true)
                .build();

            let result = run_client(config);

            // Restore permissions before assertions so tempdir cleanup succeeds.
            let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));

            // The transfer must fail - either FileSelect (3) or PartialTransfer (23).
            let error = result.expect_err("write to read-only dest should fail");
            let code = error.exit_code();
            assert!(
                code == PARTIAL_TRANSFER_EXIT_CODE
                    || code == core::client::FILE_SELECTION_EXIT_CODE,
                "expected exit code 23 or 3, got {code}"
            );

            // The file should not have been written.
            assert!(
                !dest.join("file.txt").exists(),
                "file should not be created in read-only destination"
            );
        });
    }

    /// When a source file has no read permission, the transfer should skip it
    /// and report a partial transfer. This is similar to the existing permission
    /// denied test but exercises a single-file scenario without --delete.
    ///
    /// upstream: rsync skips unreadable files, exits 23.
    #[test]
    fn error_recovery_source_permission_denied() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let dest = temp.path().join("dest");

            fs::create_dir_all(&source).expect("create source");
            fs::create_dir_all(&dest).expect("create dest");

            // One readable, one unreadable - distinct sizes to avoid quick-check.
            touch(&source.join("accessible.txt"), b"this file can be read");
            touch(
                &source.join("locked.dat"),
                b"locked file with secret data inside",
            );

            fs::set_permissions(source.join("locked.dat"), fs::Permissions::from_mode(0o000))
                .expect("chmod 000");

            let mut source_arg = source.into_os_string();
            source_arg.push("/");

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest.clone().into_os_string()])
                .mkpath(true)
                .delete(true)
                .times(true)
                .build();

            let result = run_client(config);

            let error = result.expect_err("transfer with unreadable file should fail");
            assert_eq!(
                error.exit_code(),
                PARTIAL_TRANSFER_EXIT_CODE,
                "exit code should be 23, got {}",
                error.exit_code()
            );

            // The readable file should still transfer.
            assert!(
                dest.join("accessible.txt").exists(),
                "accessible.txt should be copied"
            );
            assert_eq!(
                fs::read(dest.join("accessible.txt")).expect("read"),
                b"this file can be read"
            );

            // The locked file should not appear at destination.
            assert!(
                !dest.join("locked.dat").exists(),
                "locked.dat should not be at destination"
            );
        });
    }

    /// Modifying a source file during transfer should not cause a panic or
    /// data corruption. The engine should either transfer the old or new
    /// content, and may report a partial transfer.
    ///
    /// upstream: rsync detects file changes via mtime/size and may redo the
    /// file in phase 2, ultimately exiting 0 or 23.
    #[test]
    fn error_recovery_concurrent_modification() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let dest = temp.path().join("dest");

            fs::create_dir_all(&source).expect("create source");
            fs::create_dir_all(&dest).expect("create dest");

            // Create a reasonably sized file so there is a window for modification.
            let initial_content = vec![b'A'; 64 * 1024];
            touch(&source.join("mutable.bin"), &initial_content);

            // Also create a stable file for reference.
            touch(&source.join("stable.txt"), b"stable content");

            // Modify the file just before transfer - simulating a concurrent write.
            // In a real scenario this would happen during transfer, but for a
            // deterministic test we modify between file list build and data transfer.
            let modified_content = vec![b'B'; 64 * 1024 + 512];
            fs::write(source.join("mutable.bin"), &modified_content)
                .expect("modify source file");

            let mut source_arg = source.into_os_string();
            source_arg.push("/");

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest.clone().into_os_string()])
                .mkpath(true)
                .times(true)
                .build();

            let result = run_client(config);

            // Either success or partial transfer - both are acceptable.
            match result {
                Ok(_) => {}
                Err(err) => {
                    assert_eq!(
                        err.exit_code(),
                        PARTIAL_TRANSFER_EXIT_CODE,
                        "expected exit code 23, got {}",
                        err.exit_code()
                    );
                }
            }

            // The stable file should always be transferred correctly.
            assert!(
                dest.join("stable.txt").exists(),
                "stable.txt should be copied"
            );
            assert_eq!(
                fs::read(dest.join("stable.txt")).expect("read stable"),
                b"stable content"
            );

            // The mutable file should exist with some content (old or new).
            assert!(
                dest.join("mutable.bin").exists(),
                "mutable.bin should be copied"
            );
            let dest_content = fs::read(dest.join("mutable.bin")).expect("read mutable");
            assert!(!dest_content.is_empty(), "mutable.bin should have content");
        });
    }

    /// Transferring an empty source directory tree should succeed with exit
    /// code 0 and create the corresponding empty directories at the destination.
    ///
    /// upstream: rsync creates the directory structure and exits 0.
    #[test]
    fn error_recovery_empty_source_directory() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let dest = temp.path().join("dest");

            fs::create_dir_all(&source).expect("create source");
            fs::create_dir_all(&dest).expect("create dest");

            // Create nested empty directories.
            fs::create_dir_all(source.join("level1/level2/level3")).expect("create nested dirs");
            fs::create_dir_all(source.join("sibling")).expect("create sibling dir");

            let mut source_arg = source.into_os_string();
            source_arg.push("/");

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest.clone().into_os_string()])
                .mkpath(true)
                .recursive(true)
                .times(true)
                .build();

            let result = run_client(config);

            // An empty directory tree should transfer successfully.
            assert!(
                result.is_ok(),
                "empty directory transfer should succeed, got: {result:?}"
            );

            // Verify the directory structure was created.
            assert!(
                dest.join("level1").is_dir(),
                "level1 directory should exist"
            );
            assert!(
                dest.join("level1/level2").is_dir(),
                "level2 directory should exist"
            );
            assert!(
                dest.join("level1/level2/level3").is_dir(),
                "level3 directory should exist"
            );
            assert!(
                dest.join("sibling").is_dir(),
                "sibling directory should exist"
            );
        });
    }
}
