//! Tests for the SDDL parse/format helpers and (on Windows) the
//! round-trip wrappers around `read_dacl_sddl`/`write_dacl_sddl`.

use crate::acl_windows::common::{RSYNC_PERM_EXECUTE, RSYNC_PERM_READ, RSYNC_PERM_WRITE};
use crate::acl_windows::sddl::{
    parse_aces, perms_to_sddl_rights, sddl_rights_to_perms, split_sddl,
};
use windows::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};

#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use tempfile::tempdir;

#[test]
fn sddl_rights_decode_two_letter_tokens() {
    assert_eq!(
        sddl_rights_to_perms("FA"),
        RSYNC_PERM_READ | RSYNC_PERM_WRITE | RSYNC_PERM_EXECUTE
    );
    assert_eq!(sddl_rights_to_perms("FR"), RSYNC_PERM_READ);
    assert_eq!(sddl_rights_to_perms("FW"), RSYNC_PERM_WRITE);
    assert_eq!(sddl_rights_to_perms("FX"), RSYNC_PERM_EXECUTE);
    assert_eq!(
        sddl_rights_to_perms("FRFX"),
        RSYNC_PERM_READ | RSYNC_PERM_EXECUTE
    );
}

#[test]
fn sddl_rights_decode_hex_mask() {
    let mask = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;
    let token = format!("0x{mask:x}");
    assert_eq!(
        sddl_rights_to_perms(&token),
        RSYNC_PERM_READ | RSYNC_PERM_WRITE
    );
}

#[test]
fn sddl_rights_encode_canonical_order() {
    assert_eq!(perms_to_sddl_rights(0), "");
    assert_eq!(perms_to_sddl_rights(RSYNC_PERM_READ), "FR");
    assert_eq!(
        perms_to_sddl_rights(RSYNC_PERM_READ | RSYNC_PERM_EXECUTE),
        "FRFX"
    );
    assert_eq!(
        perms_to_sddl_rights(RSYNC_PERM_READ | RSYNC_PERM_WRITE | RSYNC_PERM_EXECUTE),
        "FRFWFX"
    );
}

#[test]
fn split_sddl_separates_owner_group_dacl() {
    let (o, g, d, s) = split_sddl("O:BAG:SYD:(A;;FA;;;BA)");
    assert_eq!(o, Some("BA"));
    assert_eq!(g, Some("SY"));
    assert_eq!(d, Some("(A;;FA;;;BA)"));
    assert_eq!(s, None);
}

#[test]
fn parse_aces_skips_malformed_entries() {
    let aces = parse_aces("(A;;FA;;;BA)(broken)(A;;FR;;;WD)");
    assert_eq!(aces.len(), 2);
    assert_eq!(aces[0].trustee, "BA");
    assert_eq!(aces[1].trustee, "WD");
}

#[cfg(windows)]
#[test]
fn read_dacl_sddl_returns_non_empty_for_temp_file() {
    use crate::acl_windows::sddl::read_dacl_sddl;

    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let sddl = read_dacl_sddl(&file).expect("read sddl");
    // Any NTFS DACL serialises to at least the "D:" prefix.
    assert!(sddl.contains("D:"), "expected DACL section, got {sddl:?}");
}

#[cfg(windows)]
#[test]
fn write_dacl_sddl_round_trips_known_descriptor() {
    use crate::acl_windows::sddl::{read_dacl_sddl, write_dacl_sddl};

    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");

    // Owner BA, Group SY, DACL grants full access to BA and Everyone.
    let canonical = "O:BAG:SYD:P(A;;FA;;;BA)(A;;FA;;;WD)";
    write_dacl_sddl(&file, canonical).expect("write sddl");

    let read_back = read_dacl_sddl(&file).expect("read sddl");
    assert!(
        read_back.contains("O:BA"),
        "owner BA missing in {read_back:?}"
    );
    assert!(
        read_back.contains("G:SY"),
        "group SY missing in {read_back:?}"
    );
    assert!(
        read_back.contains("(A;;FA;;;BA)"),
        "BA ACE missing in {read_back:?}"
    );
    assert!(
        read_back.contains("(A;;FA;;;WD)"),
        "Everyone ACE missing in {read_back:?}"
    );
}

#[cfg(windows)]
#[test]
fn write_dacl_sddl_preserves_owner_and_group() {
    use crate::acl_windows::sddl::{read_dacl_sddl, write_dacl_sddl};

    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");

    let descriptor = "O:BAG:BA D:P(A;;FA;;;BA)";
    write_dacl_sddl(&file, descriptor).expect("write sddl");
    let read_back = read_dacl_sddl(&file).expect("read sddl");
    assert!(read_back.starts_with("O:BAG:BA"), "got {read_back:?}");
}

#[cfg(windows)]
#[test]
fn write_dacl_sddl_rejects_invalid_input() {
    use crate::acl_windows::sddl::write_dacl_sddl;

    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let result = write_dacl_sddl(&file, "not-a-sddl-string");
    assert!(result.is_err(), "expected parse error, got {result:?}");
}
