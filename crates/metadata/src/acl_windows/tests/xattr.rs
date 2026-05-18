//! Tests for the reserved SDDL xattr slot helpers.

use std::fs::File;
use tempfile::tempdir;

use protocol::xattr::{XattrEntry, XattrList};

use crate::acl_windows::xattr::{
    WINDOWS_SDDL_XATTR_NAME, apply_sddl_from_xattrs, find_sddl_in_xattrs,
};

#[test]
fn find_sddl_in_xattrs_returns_payload() {
    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.other".to_vec(), b"value".to_vec()));
    list.push(XattrEntry::new(
        WINDOWS_SDDL_XATTR_NAME.to_vec(),
        b"O:BAG:SYD:P(A;;FA;;;BA)".to_vec(),
    ));
    let sddl = find_sddl_in_xattrs(&list).expect("sddl present");
    assert!(sddl.starts_with("O:BAG:SY"));
}

#[test]
fn find_sddl_in_xattrs_returns_none_when_missing() {
    let list = XattrList::new();
    assert!(find_sddl_in_xattrs(&list).is_none());
}

#[test]
fn find_sddl_in_xattrs_skips_abbreviated_entries() {
    let mut list = XattrList::new();
    list.push(XattrEntry::abbreviated(
        WINDOWS_SDDL_XATTR_NAME.to_vec(),
        b"checksum".to_vec(),
        1024,
    ));
    assert!(find_sddl_in_xattrs(&list).is_none());
}

#[test]
fn apply_sddl_from_xattrs_no_payload_is_noop() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let list = XattrList::new();
    let applied = apply_sddl_from_xattrs(&file, &list).expect("noop ok");
    assert!(!applied);
}

#[cfg(windows)]
#[test]
fn sddl_xattr_entry_round_trips_on_ntfs() {
    use crate::acl_windows::sddl::{read_dacl_sddl, write_dacl_sddl};
    use crate::acl_windows::xattr::sddl_xattr_entry;

    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    File::create(&src).expect("src");
    // Pin a known descriptor so the round-trip assertion is stable
    // regardless of NTFS inheritance on the runner.
    let canonical = "O:BAG:SYD:P(A;;FA;;;BA)(A;;FA;;;WD)";
    write_dacl_sddl(&src, canonical).expect("seed sddl");

    let entry = sddl_xattr_entry(&src)
        .expect("read sddl xattr")
        .expect("entry present");
    assert_eq!(entry.name(), WINDOWS_SDDL_XATTR_NAME);
    let payload = std::str::from_utf8(entry.datum()).expect("utf8");
    assert!(payload.contains("D:"));

    let mut list = XattrList::new();
    list.push(entry);

    let dst = dir.path().join("dst");
    File::create(&dst).expect("dst");
    let applied = apply_sddl_from_xattrs(&dst, &list).expect("apply sddl");
    assert!(applied, "expected SDDL xattr to be consumed");

    let read_back = read_dacl_sddl(&dst).expect("read back");
    assert!(read_back.contains("(A;;FA;;;BA)"), "got {read_back:?}");
    assert!(read_back.contains("(A;;FA;;;WD)"), "got {read_back:?}");
}
