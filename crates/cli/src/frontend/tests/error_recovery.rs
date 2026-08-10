//! Tests for error recovery when source files vanish during transfer.
//!
//! Upstream rsync returns exit code 24 (RERR_VANISHED) when files disappear
//! between file list generation and the actual transfer.
//!
//! References:
//! - upstream: errcode.h - RERR_VANISHED = 24
//! - upstream: flist.c:1286-1294 - vanished file handling during file list build
//! - upstream: main.c:1338-1345 - io_error to exit code mapping

use super::common::*;
use super::*;

/// When one of several explicit source paths is missing, the transfer should
/// still copy the remaining files and exit 23 (RERR_PARTIAL) via the failed
/// link_stat. Verified against upstream rsync 3.4.3.
#[cfg(unix)]
#[test]
fn missing_source_operand_yields_exit_23_and_remaining_files_transfer() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");

    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let file_a = src_dir.join("alpha.txt");
    let file_b = src_dir.join("bravo.txt");
    let file_c = src_dir.join("charlie.txt");
    std::fs::write(&file_a, b"alpha content").expect("write alpha");
    std::fs::write(&file_b, b"bravo content").expect("write bravo");
    std::fs::write(&file_c, b"charlie content").expect("write charlie");

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

    std::fs::remove_file(&file_b).expect("remove bravo.txt");

    // upstream: flist.c send_file_list() - a missing explicit source operand
    // fails its link_stat, printing `link_stat "%s" failed` and setting
    // IOERR_GENERAL (exit 23, RERR_PARTIAL); the remaining operands still
    // transfer. Distinct from exit 24, which is reserved for a file that
    // disappears mid-transfer after it was already in the file list.
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        file_a.clone().into_os_string(),
        file_b.into_os_string(),
        file_c.clone().into_os_string(),
        dst_dir.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);

    assert_eq!(
        code, 23,
        "expected exit code 23 (partial/link_stat), got {code}: {stderr_text}"
    );

    assert!(
        stderr_text.contains("link_stat"),
        "stderr should mention the failed link_stat: {stderr_text}"
    );

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

/// When a single explicit source path does not exist, the transfer exits 23
/// (RERR_PARTIAL) via the failed link_stat (not 24 or 3). Verified against
/// upstream rsync 3.4.3.
#[cfg(unix)]
#[test]
fn single_missing_source_yields_exit_23() {
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

    // upstream: flist.c send_file_list() - a missing source operand fails its
    // link_stat and exits 23 (RERR_PARTIAL). Verified against upstream 3.4.3.
    assert!(
        code == 23,
        "expected exit code 23 (partial/link_stat), got {code}: {stderr_text}"
    );
}
