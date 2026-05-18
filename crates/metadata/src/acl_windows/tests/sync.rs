//! Tests for the cross-platform `sync_acls` integration.

use std::fs::File;
use tempfile::tempdir;

use crate::acl_windows::sync::sync_acls;

#[test]
fn sync_acls_skips_when_not_following_symlinks() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    File::create(&src).expect("src");
    File::create(&dst).expect("dst");
    let result = sync_acls(&src, &dst, false);
    assert!(result.is_ok());
}

#[test]
fn sync_acls_returns_not_found_for_missing_source() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("missing");
    let dst = dir.path().join("dst");
    File::create(&dst).expect("dst");
    let result = sync_acls(&src, &dst, true);
    assert!(result.is_err());
}

#[cfg(windows)]
#[test]
fn sync_acls_round_trips_on_ntfs() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    File::create(&src).expect("src");
    File::create(&dst).expect("dst");
    // No assertion on the contents - inheritance varies between
    // CI runners. We just assert the call does not error on a
    // straightforward NTFS temp file.
    let result = sync_acls(&src, &dst, true);
    assert!(result.is_ok(), "sync_acls failed: {:?}", result.err());
}

#[cfg(windows)]
#[test]
fn sync_acls_prefers_sddl_round_trip() {
    use crate::acl_windows::sddl::{read_dacl_sddl, write_dacl_sddl};

    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    File::create(&src).expect("src");
    File::create(&dst).expect("dst");
    // Seed a non-inherited descriptor so we have a concrete payload to
    // compare on either end of the round-trip.
    let canonical = "O:BAG:SYD:P(A;;FA;;;BA)(A;;FA;;;WD)";
    write_dacl_sddl(&src, canonical).expect("seed sddl");

    sync_acls(&src, &dst, true).expect("sync acls");

    let read_back = read_dacl_sddl(&dst).expect("read dst sddl");
    assert!(read_back.contains("O:BA"), "got {read_back:?}");
    assert!(read_back.contains("G:SY"), "got {read_back:?}");
    assert!(read_back.contains("(A;;FA;;;BA)"), "got {read_back:?}");
    assert!(read_back.contains("(A;;FA;;;WD)"), "got {read_back:?}");
}
