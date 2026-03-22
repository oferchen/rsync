//! Error recovery tests for permission-denied source files.
//!
//! Verifies that the transfer engine handles unreadable source files gracefully,
//! matching upstream rsync behavior: readable files are still copied, the
//! unreadable file is skipped with an I/O error, and the process exits with
//! code 23 (RERR_PARTIAL).
//!
//! These tests require Unix chmod semantics and are gated with `#[cfg(unix)]`.

mod test_timeout;

#[cfg(unix)]
mod permission_denied {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use crate::test_timeout::{LOCAL_TIMEOUT, run_with_timeout};
    use core::client::{ClientConfig, PARTIAL_TRANSFER_EXIT_CODE, run_client};
    use tempfile::tempdir;

    /// Helper: create a file with the given content, creating parent dirs as needed.
    fn touch(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    /// When a source file is unreadable (chmod 000), the transfer should fail
    /// with exit code 23 (RERR_PARTIAL) indicating a partial transfer.
    ///
    /// With `--delete` enabled the engine continues past individual I/O errors
    /// so that deletion accounting can complete. Readable files processed before
    /// and after the unreadable entry are still copied to the destination.
    ///
    /// upstream: rsync skips unreadable files, logs a warning, copies the rest,
    /// and exits 23.
    #[test]
    fn unreadable_source_file_yields_partial_transfer_with_delete() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source_root = temp.path().join("source");
            let dest_root = temp.path().join("dest");

            fs::create_dir_all(&source_root).expect("create source root");
            fs::create_dir_all(&dest_root).expect("create dest root");

            // Create three source files with distinct sizes to avoid quick-check skips.
            touch(&source_root.join("aaa_readable.txt"), b"hello world");
            touch(
                &source_root.join("bbb_forbidden.txt"),
                b"this content is inaccessible",
            );
            touch(
                &source_root.join("ccc_also_readable.txt"),
                b"still transferable content here",
            );

            // Make one file unreadable.
            fs::set_permissions(
                source_root.join("bbb_forbidden.txt"),
                fs::Permissions::from_mode(0o000),
            )
            .expect("chmod 000");

            let mut source_arg = source_root.into_os_string();
            source_arg.push(std::path::MAIN_SEPARATOR.to_string());

            // Use --delete so the engine continues past the I/O error instead of
            // aborting immediately. This matches the common real-world usage pattern
            // where users want best-effort transfers.
            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest_root.clone().into_os_string()])
                .mkpath(true)
                .delete(true)
                .times(true)
                .build();

            let result = run_client(config);

            // The transfer must return an error with exit code 23.
            let error =
                result.expect_err("transfer with unreadable source file should return Err, not Ok");
            assert_eq!(
                error.exit_code(),
                PARTIAL_TRANSFER_EXIT_CODE,
                "exit code should be 23 (RERR_PARTIAL), got {}",
                error.exit_code()
            );

            // Readable files should have been copied despite the error.
            assert!(
                dest_root.join("aaa_readable.txt").exists(),
                "readable file before the forbidden one should be copied"
            );
            assert_eq!(
                fs::read(dest_root.join("aaa_readable.txt")).expect("read aaa"),
                b"hello world"
            );

            assert!(
                dest_root.join("ccc_also_readable.txt").exists(),
                "readable file after the forbidden one should be copied"
            );
            assert_eq!(
                fs::read(dest_root.join("ccc_also_readable.txt")).expect("read ccc"),
                b"still transferable content here"
            );

            // The forbidden file should NOT have been created at the destination.
            assert!(
                !dest_root.join("bbb_forbidden.txt").exists(),
                "unreadable source file should not appear at destination"
            );
        });
    }

    /// Without --delete, a permission-denied I/O error aborts the transfer
    /// immediately. The exit code is still 23 (RERR_PARTIAL).
    #[test]
    fn unreadable_source_file_aborts_without_delete() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source_root = temp.path().join("source");
            let dest_root = temp.path().join("dest");

            fs::create_dir_all(&source_root).expect("create source root");
            fs::create_dir_all(&dest_root).expect("create dest root");

            // Single unreadable file - guarantees the error is hit.
            touch(&source_root.join("secret.txt"), b"classified");

            fs::set_permissions(
                source_root.join("secret.txt"),
                fs::Permissions::from_mode(0o000),
            )
            .expect("chmod 000");

            let mut source_arg = source_root.into_os_string();
            source_arg.push(std::path::MAIN_SEPARATOR.to_string());

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest_root.clone().into_os_string()])
                .mkpath(true)
                .build();

            let result = run_client(config);

            let error = result.expect_err("transfer with unreadable source file should return Err");
            assert_eq!(
                error.exit_code(),
                PARTIAL_TRANSFER_EXIT_CODE,
                "exit code should be 23 (RERR_PARTIAL), got {}",
                error.exit_code()
            );

            // The unreadable file must not appear at the destination.
            assert!(
                !dest_root.join("secret.txt").exists(),
                "unreadable file should not be created at destination"
            );
        });
    }

    /// A directory containing a mix of readable and unreadable files in a
    /// subdirectory still copies the accessible entries and reports exit 23.
    #[test]
    fn nested_unreadable_file_yields_partial_transfer() {
        run_with_timeout(LOCAL_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source_root = temp.path().join("source");
            let dest_root = temp.path().join("dest");

            fs::create_dir_all(&source_root).expect("create source root");
            fs::create_dir_all(&dest_root).expect("create dest root");

            // Top-level file (always readable).
            touch(&source_root.join("top.txt"), b"top-level content");
            // Nested files - one readable, one forbidden.
            touch(
                &source_root.join("subdir/readable.txt"),
                b"nested readable data",
            );
            touch(
                &source_root.join("subdir/forbidden.txt"),
                b"nested secret data",
            );

            fs::set_permissions(
                source_root.join("subdir/forbidden.txt"),
                fs::Permissions::from_mode(0o000),
            )
            .expect("chmod 000");

            let mut source_arg = source_root.into_os_string();
            source_arg.push(std::path::MAIN_SEPARATOR.to_string());

            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest_root.clone().into_os_string()])
                .mkpath(true)
                .delete(true)
                .times(true)
                .build();

            let result = run_client(config);

            let error = result.expect_err("transfer with nested unreadable file should return Err");
            assert_eq!(
                error.exit_code(),
                PARTIAL_TRANSFER_EXIT_CODE,
                "exit code should be 23 (RERR_PARTIAL), got {}",
                error.exit_code()
            );

            // Top-level readable file should be copied.
            assert!(
                dest_root.join("top.txt").exists(),
                "top-level readable file should be copied"
            );
            assert_eq!(
                fs::read(dest_root.join("top.txt")).expect("read top"),
                b"top-level content"
            );

            // Nested readable file should be copied.
            assert!(
                dest_root.join("subdir/readable.txt").exists(),
                "nested readable file should be copied"
            );

            // Nested forbidden file should NOT exist at destination.
            assert!(
                !dest_root.join("subdir/forbidden.txt").exists(),
                "nested unreadable file should not appear at destination"
            );
        });
    }
}
