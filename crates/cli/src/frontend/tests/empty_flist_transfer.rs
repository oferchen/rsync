//! End-to-end control for the empty file-list contract, exercised on every
//! platform (including Windows).
//!
//! A recursive transfer whose source tree is empty produces an empty transfer
//! file list and must complete with exit code 0 - the sender frames only the
//! implied root and no `io_error` bit, so nothing may be reported as an error.
//! This is the observable end-to-end counterpart to the wire-level empty-flist
//! coverage (`receiver_empty_file_list`, `send_empty_file_list`,
//! `protocol_flist_empty`). The behaviour is platform-agnostic: it needs no
//! POSIX-only surface, so it runs unconditionally rather than being gated to
//! Unix.

use super::common::*;
use super::*;

/// An empty source directory should transfer successfully with exit code 0.
///
/// Verifies that recursive transfer of an empty tree does not spuriously
/// report errors on any platform.
#[test]
fn empty_source_directory_transfers_successfully() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    let mut src_trailing = src.into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing,
        dst.into_os_string(),
    ]);

    assert_eq!(
        code,
        0,
        "empty directory sync should exit 0: {}",
        String::from_utf8_lossy(&stderr)
    );
}
