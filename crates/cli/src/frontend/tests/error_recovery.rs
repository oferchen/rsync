/// Error recovery tests verifying graceful handling of write failures.
///
/// These tests simulate destination write failures using read-only directories
/// and verify that the transfer reports appropriate errors without crashing.
/// Gated to unix because they rely on filesystem permission enforcement.
use super::common::*;
use super::*;

/// When a destination file cannot be written (read-only parent directory),
/// the transfer exits with a non-zero code indicating partial transfer or
/// file I/O error, and stderr contains a diagnostic message.
#[cfg(unix)]
#[test]
fn write_failure_readonly_dest_reports_error_and_exits_nonzero() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create a source file to transfer.
    std::fs::write(source_dir.join("file.txt"), b"payload").expect("write source");

    // Create the destination subdirectory that mirrors the source name,
    // then make it read-only so file creation inside it fails.
    let dest_subdir = dest_dir.join("src");
    std::fs::create_dir_all(&dest_subdir).expect("create dest subdir");
    std::fs::set_permissions(&dest_subdir, PermissionsExt::from_mode(0o555))
        .expect("set readonly perms");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    // Restore writable permissions so tempdir cleanup succeeds.
    std::fs::set_permissions(&dest_subdir, PermissionsExt::from_mode(0o755))
        .expect("restore perms");

    // Upstream rsync returns 23 (partial transfer) when some files fail.
    assert_ne!(code, 0, "exit code must be non-zero on write failure");
    assert!(
        code == 23 || code == 11,
        "expected exit code 23 (partial transfer) or 11 (file I/O error), got {code}"
    );

    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        !rendered.is_empty(),
        "stderr should contain a diagnostic message about the write failure"
    );
}

/// When only some files in a recursive transfer are blocked by permissions,
/// the writable files still transfer successfully and the exit code reflects
/// a partial transfer.
#[cfg(unix)]
#[test]
fn partial_write_failure_transfers_writable_files() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");

    // Source tree: src/ok/good.txt and src/blocked/secret.txt
    let ok_src = source_dir.join("ok");
    let blocked_src = source_dir.join("blocked");
    std::fs::create_dir_all(&ok_src).expect("create ok source");
    std::fs::create_dir_all(&blocked_src).expect("create blocked source");
    std::fs::write(ok_src.join("good.txt"), b"good data").expect("write good");
    std::fs::write(blocked_src.join("secret.txt"), b"secret data").expect("write secret");

    // Pre-create destination tree with the "blocked" subdirectory read-only.
    let dest_root = dest_dir.join("src");
    let ok_dst = dest_root.join("ok");
    let blocked_dst = dest_root.join("blocked");
    std::fs::create_dir_all(&ok_dst).expect("create ok dest");
    std::fs::create_dir_all(&blocked_dst).expect("create blocked dest");
    std::fs::set_permissions(&blocked_dst, PermissionsExt::from_mode(0o555)).expect("set readonly");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    // Restore permissions for cleanup.
    std::fs::set_permissions(&blocked_dst, PermissionsExt::from_mode(0o755))
        .expect("restore perms");

    // The writable file should have been transferred despite the other failure.
    let good_dest = ok_dst.join("good.txt");
    assert!(
        good_dest.exists(),
        "writable file should still be transferred"
    );
    assert_eq!(
        std::fs::read(&good_dest).expect("read good dest"),
        b"good data"
    );

    // The blocked file should not exist.
    let secret_dest = blocked_dst.join("secret.txt");
    assert!(
        !secret_dest.exists(),
        "file in read-only directory should not be created"
    );

    // Exit code indicates partial transfer.
    assert_ne!(code, 0, "exit code must be non-zero for partial failure");
    assert!(
        code == 23 || code == 11,
        "expected exit code 23 (partial transfer) or 11 (file I/O error), got {code}"
    );

    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        !rendered.is_empty(),
        "stderr should contain an error diagnostic"
    );
}

/// A transfer to a read-only destination directory does not crash or panic -
/// it exits cleanly with a non-zero code.
#[cfg(unix)]
#[test]
fn write_failure_does_not_crash() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create multiple source files to exercise the transfer pipeline.
    for i in 0..5 {
        std::fs::write(
            source_dir.join(format!("file_{i}.txt")),
            format!("content {i}").as_bytes(),
        )
        .expect("write source file");
    }

    // Pre-create destination and make it entirely read-only.
    let dest_subdir = dest_dir.join("src");
    std::fs::create_dir_all(&dest_subdir).expect("create dest subdir");
    std::fs::set_permissions(&dest_subdir, PermissionsExt::from_mode(0o555)).expect("set readonly");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    // Restore permissions for cleanup.
    std::fs::set_permissions(&dest_subdir, PermissionsExt::from_mode(0o755))
        .expect("restore perms");

    // The process must not crash (which would manifest as exit code > 127
    // from signal termination). Any valid rsync exit code is acceptable.
    assert!(
        code <= 127,
        "process should not crash; exit code {code} suggests signal termination"
    );
    assert_ne!(code, 0, "exit code must be non-zero on write failure");

    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        !rendered.is_empty(),
        "stderr should report the write failure"
    );
}
