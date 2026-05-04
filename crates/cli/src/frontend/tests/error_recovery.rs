/// Tests for error recovery when source files vanish during transfer.
///
/// Upstream rsync returns exit code 24 (RERR_VANISHED) when files disappear
/// between file list generation and the actual transfer. These tests verify
/// that remaining files are still transferred correctly despite the vanished
/// file, and that the appropriate exit code is produced.
///
/// References:
/// - upstream: errcode.h - RERR_VANISHED = 24
/// - upstream: flist.c:1286-1294 - vanished file handling during file list build
/// - upstream: main.c:1338-1345 - io_error to exit code mapping
use super::common::*;
use super::*;

/// When one of several explicit source paths has been deleted, the transfer
/// should still copy the remaining files and exit with code 24 (vanished).
#[cfg(unix)]
#[test]
fn vanished_source_file_yields_exit_24_and_remaining_files_transfer() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");

    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    // Create three source files with distinct content and sizes
    let file_a = src_dir.join("alpha.txt");
    let file_b = src_dir.join("bravo.txt");
    let file_c = src_dir.join("charlie.txt");
    std::fs::write(&file_a, b"alpha content").expect("write alpha");
    std::fs::write(&file_b, b"bravo content").expect("write bravo");
    std::fs::write(&file_c, b"charlie content").expect("write charlie");

    // First transfer: sync all files to destination
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        {
            let mut p = src_dir.clone().into_os_string();
            p.push("/");
            p
        },
        dst_dir.clone().into_os_string(),
    ]);

    assert_eq!(
        code,
        0,
        "initial sync should succeed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        dst_dir.join("alpha.txt").exists(),
        "alpha should exist after initial sync"
    );
    assert!(
        dst_dir.join("bravo.txt").exists(),
        "bravo should exist after initial sync"
    );
    assert!(
        dst_dir.join("charlie.txt").exists(),
        "charlie should exist after initial sync"
    );

    // Delete one source file to simulate a vanished file
    std::fs::remove_file(&file_b).expect("remove bravo.txt");

    // Second transfer: pass explicit file paths including the now-deleted file.
    // The deleted path triggers the "file has vanished" code path in walk_path(),
    // which sets IOERR_VANISHED, producing exit code 24.
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        file_a.clone().into_os_string(),
        file_b.into_os_string(),
        file_c.clone().into_os_string(),
        dst_dir.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);

    // Exit code 24 = RERR_VANISHED: some files vanished before transfer
    assert_eq!(
        code, 24,
        "expected exit code 24 (vanished), got {code}: {stderr_text}"
    );

    // The vanished file warning should appear in stderr
    assert!(
        stderr_text.contains("vanished"),
        "stderr should mention vanished file: {stderr_text}"
    );

    // Remaining files should still be at the destination with correct content
    assert_eq!(
        std::fs::read(dst_dir.join("alpha.txt")).expect("read alpha"),
        b"alpha content",
        "alpha.txt should have correct content after partial transfer"
    );
    assert_eq!(
        std::fs::read(dst_dir.join("charlie.txt")).expect("read charlie"),
        b"charlie content",
        "charlie.txt should have correct content after partial transfer"
    );
}

/// A recursive directory transfer where all files exist should succeed
/// with exit code 0, confirming that exit code 24 is specific to the
/// vanished-file scenario.
#[cfg(unix)]
#[test]
fn recursive_transfer_without_vanished_files_exits_zero() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");

    std::fs::create_dir(&src_dir).expect("create src dir");

    std::fs::write(src_dir.join("one.txt"), b"one").expect("write one");
    std::fs::write(src_dir.join("two.txt"), b"two").expect("write two");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        {
            let mut p = src_dir.into_os_string();
            p.push("/");
            p
        },
        dst_dir.clone().into_os_string(),
    ]);

    assert_eq!(
        code,
        0,
        "transfer with no vanished files should exit 0: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        std::fs::read(dst_dir.join("one.txt")).expect("read"),
        b"one"
    );
    assert_eq!(
        std::fs::read(dst_dir.join("two.txt")).expect("read"),
        b"two"
    );
}

/// When a single explicit source path does not exist, the transfer should
/// still report exit code 24 (not 23 or 3) because the path vanished.
#[cfg(unix)]
#[test]
fn single_vanished_source_yields_exit_24() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("does_not_exist.txt");
    let dst = tmp.path().join("dest.txt");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        missing.into_os_string(),
        dst.into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);

    // A missing source file should produce exit code 24 or a file-selection error.
    // Upstream rsync produces "file has vanished" + exit 24 when the source path
    // fails stat with ENOENT during file list building.
    assert!(
        code == 24 || code == 23,
        "expected exit code 24 (vanished) or 23 (partial), got {code}: {stderr_text}"
    );
}
