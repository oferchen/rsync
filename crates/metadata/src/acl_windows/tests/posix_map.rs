//! Round-trip tests for the POSIX mode <-> DACL mapping helpers.

use crate::acl_windows::posix_map::{dacl_to_posix_mode, posix_mode_to_dacl};

#[test]
fn posix_mode_to_dacl_uses_three_allow_aces_with_protected_flag() {
    let sddl = posix_mode_to_dacl(0o755, "S-1-5-21-100", "S-1-5-21-200");
    assert!(sddl.starts_with("O:S-1-5-21-100"));
    assert!(sddl.contains("G:S-1-5-21-200"));
    assert!(sddl.contains("D:P("));
    assert!(sddl.contains("(A;;FRFWFX;;;S-1-5-21-100)"));
    assert!(sddl.contains("(A;;FRFX;;;S-1-5-21-200)"));
    assert!(sddl.contains("(A;;FRFX;;;WD)"));
}

#[test]
fn round_trip_mode_755_preserves_rwx_triplet() {
    let owner = "S-1-5-21-1";
    let group = "S-1-5-21-2";
    let sddl = posix_mode_to_dacl(0o755, owner, group);
    let back = dacl_to_posix_mode(&sddl);
    assert_eq!(back, 0o755, "round-trip lost bits; sddl: {sddl}");
}

#[test]
fn round_trip_full_mode_matrix_preserves_rwx() {
    let owner = "S-1-5-21-1000";
    let group = "S-1-5-21-1001";
    for mode in 0o000u32..=0o777u32 {
        let sddl = posix_mode_to_dacl(mode, owner, group);
        let back = dacl_to_posix_mode(&sddl);
        assert_eq!(back, mode, "round-trip lost bits for mode {mode:03o}");
    }
}

#[test]
fn dacl_to_posix_mode_handles_everyone_as_other() {
    let sddl = "O:BAG:SYD:(A;;FA;;;BA)(A;;FRFX;;;SY)(A;;FR;;;WD)";
    assert_eq!(dacl_to_posix_mode(sddl), 0o754);
}

#[test]
fn dacl_to_posix_mode_falls_back_to_authenticated_users() {
    let sddl = "O:BAG:SYD:(A;;FA;;;BA)(A;;FRFX;;;SY)(A;;FRFX;;;AU)";
    assert_eq!(dacl_to_posix_mode(sddl), 0o755);
}

#[test]
fn dacl_to_posix_mode_drops_deny_aces() {
    let sddl = "O:BAG:SYD:(D;;FW;;;BA)(A;;FRFX;;;BA)(A;;FR;;;SY)(A;;FR;;;WD)";
    assert_eq!(dacl_to_posix_mode(sddl), 0o544);
}

#[test]
fn dacl_to_posix_mode_drops_inherited_aces() {
    let sddl = "O:BAG:SYD:(A;ID;FA;;;BA)(A;;FR;;;BA)(A;;FR;;;SY)(A;;FR;;;WD)";
    assert_eq!(dacl_to_posix_mode(sddl), 0o444);
}

#[test]
fn dacl_to_posix_mode_collapses_non_rwx_bits() {
    let sddl = "O:BAG:SYD:(A;;0x10000;;;BA)(A;;FR;;;SY)(A;;FR;;;WD)";
    let mode = dacl_to_posix_mode(sddl);
    assert_eq!(mode & 0o700, 0);
    assert_eq!(mode & 0o077, 0o044);
}

#[test]
fn dacl_to_posix_mode_returns_zero_for_missing_dacl() {
    assert_eq!(dacl_to_posix_mode("O:BAG:SY"), 0);
    assert_eq!(dacl_to_posix_mode(""), 0);
}
